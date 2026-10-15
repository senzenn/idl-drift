//! Classify samples by discriminator, decode them, and run every check.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use idl_drift::model::{Account, AccountItem, Event, Idl, Instruction, NamedDisc, Type, TypeDef};

use crate::decode::{DecodeError, Decoder, Value};
use crate::layout::{Reading, StructLayout, TypeIndex};
use crate::reference::{first_difference, normalize, to_json, Outcome, Reference};
use crate::report::{
    AccountSummary, Check, Coverage, EventSources, EventSummary, Finding, InstructionSummary,
    ProgramInfo, Range, ReferenceSummary, Report, Severity, SizeCount,
};
use crate::samples::{EventSample, EventSource, KeyFlags, Samples};

/// Anchor's `EVENT_IX_TAG` in little-endian: the envelope `emit_cpi!` puts before an event.
pub(crate) const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// Check `samples` against `idl`, optionally comparing values with a reference decoder.
#[must_use]
pub fn verify(idl: &Idl, samples: &Samples, reference: Option<&Reference>) -> Report {
    let types = TypeIndex::new(idl);
    let mut v = Verifier {
        idl,
        samples,
        reference,
        decoder: Decoder::new(types),
        findings: Vec::new(),
        reference_summary: ReferenceSummary::default(),
    };
    v.check_program_id();
    let (account_coverage, accounts) = v.check_accounts();
    let (instruction_coverage, instructions) = v.check_instructions();
    let (event_coverage, events) = v.check_events();
    let Verifier {
        findings,
        reference_summary,
        ..
    } = v;
    Report {
        program: ProgramInfo {
            name: idl.metadata.name.clone(),
            version: idl.metadata.version.clone(),
            address: idl.address.clone(),
        },
        samples_program_id: samples.program_id.clone(),
        account_coverage,
        accounts,
        instruction_coverage,
        instructions,
        event_coverage,
        events,
        reference: reference.map(|_| reference_summary),
        sampling: None,
        findings,
    }
}

struct Verifier<'a> {
    idl: &'a Idl,
    samples: &'a Samples,
    reference: Option<&'a Reference>,
    decoder: Decoder<'a>,
    findings: Vec<Finding>,
    reference_summary: ReferenceSummary,
}

enum AccountLayout {
    Borsh,
    /// Cast from the account bytes at `repr(C)` offsets.
    ZeroCopy(StructLayout),
}

/// One sample's decode: the value and bytes used (discriminator included), or the error.
type Outcomes = Vec<(usize, Result<(Value, usize), DecodeError>)>;

impl Verifier<'_> {
    fn push(
        &mut self,
        severity: Severity,
        check: Check,
        subject: &str,
        message: String,
        hint: Option<String>,
        example: Option<&str>,
    ) {
        self.findings.push(Finding {
            severity,
            check,
            subject: subject.to_string(),
            message,
            hint,
            example: example.filter(|e| !e.is_empty()).map(str::to_string),
        });
    }

    /// The program the samples came from; the IDL's address when the samples don't say.
    fn program_id(&self) -> &str {
        self.samples
            .program_id
            .as_deref()
            .unwrap_or(&self.idl.address)
    }

    fn check_program_id(&mut self) {
        let Some(sampled) = self.samples.program_id.as_deref() else {
            return;
        };
        let declared = self.idl.address.as_str();
        if !declared.is_empty() && declared != sampled {
            self.push(
                Severity::Warning,
                Check::ProgramId,
                "program",
                format!("the samples come from {sampled}, but the IDL is for {declared}"),
                Some("check that the IDL and the samples belong to the same program".into()),
                None,
            );
        }
    }

    // --- accounts ----------------------------------------------------------------------

    fn check_accounts(&mut self) -> (Coverage, Vec<AccountSummary>) {
        let idl = self.idl;
        let table = disc_table(
            idl.accounts
                .iter()
                .map(|a| (a.discriminator_offset, a.discriminator.as_slice())),
        );
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); idl.accounts.len()];
        let sized = sized_account_types(self.idl);
        let by_size = table.is_empty() && !sized.is_empty();
        // Key: the head of the error message, so each kind of miss groups on its own.
        let mut unknown: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut empty = Vec::new();
        for (i, sample) in self.samples.accounts.iter().enumerate() {
            if let Some(matches) = longest_match(&table, &sample.data) {
                groups[matches[0]].push(i);
                continue;
            }
            let len = sample.data.len();
            let fits: Vec<usize> = sized
                .iter()
                .filter(|(_, size)| *size == len)
                .map(|(t, _)| *t)
                .collect();
            let key = match fits.as_slice() {
                [t] => {
                    groups[*t].push(i);
                    continue;
                }
                [] if len == 0 => {
                    empty.push(i);
                    continue;
                }
                [] if by_size => format!("{len}-byte data matches no account type size"),
                [] => format!(
                    "{} matches no account discriminator in the IDL",
                    prefix_hex(&sample.data)
                ),
                many => {
                    let names: Vec<&str> = many
                        .iter()
                        .map(|t| idl.accounts[*t].name.as_str())
                        .collect();
                    format!("{len}-byte data fits {} alike", names.join(" and "))
                }
            };
            unknown.entry(key).or_default().push(i);
        }

        let unknown_count: usize = unknown.values().map(Vec::len).sum();
        if table.is_empty() && sized.is_empty() && !idl.accounts.is_empty() && unknown_count > 0 {
            self.push(
                Severity::Error,
                Check::Discriminator,
                "accounts",
                format!(
                    "the IDL gives no account discriminators, so none of the {unknown_count} sampled accounts can be classified"
                ),
                Some("add the discriminators or the account sizes to the IDL; idl-verify computes discriminators only for legacy (pre-0.30) Anchor IDLs".into()),
                None,
            );
        } else {
            let hint = unknown_account_hint(idl, &sized, by_size);
            for (head, idxs) in &unknown {
                let example = self.samples.accounts[idxs[0]].address.as_str();
                self.push(
                    Severity::Error,
                    Check::Discriminator,
                    "accounts",
                    format!(
                        "{head}, in {} {}",
                        idxs.len(),
                        plural(idxs.len(), "sample", "samples")
                    ),
                    Some(hint.clone()),
                    Some(example),
                );
            }
        }
        self.report_empty_accounts(&empty);

        let mut summaries = Vec::new();
        let mut not_sampled = Vec::new();
        for (nd, idxs) in idl.accounts.iter().zip(&groups) {
            if idxs.is_empty() {
                not_sampled.push(nd.name.clone());
            } else {
                summaries.push(self.check_account_type(nd, idxs));
            }
        }
        let coverage = Coverage {
            sampled: self.samples.accounts.len(),
            classified: self.samples.accounts.len() - unknown_count - empty.len(),
            not_sampled,
        };
        (coverage, summaries)
    }

    /// An account with no data is not an IDL fault, so it is a note.
    fn report_empty_accounts(&mut self, empty: &[usize]) {
        let Some(&first) = empty.first() else {
            return;
        };
        let example = self.samples.accounts[first].address.clone();
        self.push(
            Severity::Note,
            Check::Discriminator,
            "accounts",
            format!(
                "{} {} no data; not checked",
                empty.len(),
                plural(empty.len(), "account holds", "accounts hold")
            ),
            Some("an account with no data has no discriminator, so no IDL can describe it; a generated parser such as Shipstern's reports it as an unknown account".into()),
            Some(&example),
        );
    }

    fn check_account_type(&mut self, nd: &NamedDisc, idxs: &[usize]) -> AccountSummary {
        let subject = format!("account {}", nd.name);
        let disc = nd.payload_start();
        let accounts = &self.samples.accounts;
        let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
        for &i in idxs {
            *sizes.entry(accounts[i].data.len()).or_default() += 1;
        }
        let mut summary = AccountSummary {
            name: nd.name.clone(),
            samples: idxs.len(),
            layout: "unknown".to_string(),
            sizes: sizes
                .iter()
                .map(|(&size, &count)| SizeCount { size, count })
                .collect(),
            bytes_used: None,
            expected_size: None,
        };

        let types = self.decoder.types().clone();
        let Some(td) = types.get(&nd.name) else {
            self.push(
                Severity::Error,
                Check::MissingType,
                &subject,
                "has no entry in the IDL's `types`, so its data cannot be decoded".into(),
                None,
                None,
            );
            return summary;
        };
        summary.layout = if td.is_zero_copy() {
            let repr = td
                .repr
                .map_or_else(|| "repr(C)".to_string(), |r| r.to_string());
            format!("zero-copy {repr}")
        } else {
            "borsh".to_string()
        };
        let Some(layout) = self.account_layout(&subject, td) else {
            return summary;
        };
        let zero_copy = match &layout {
            AccountLayout::ZeroCopy(l) => Some(l),
            AccountLayout::Borsh => None,
        };

        let expected = match &zero_copy {
            Some(layout) => disc.checked_add(layout.size),
            None => types
                .borsh_size(&Type::Defined {
                    name: nd.name.clone(),
                    generics: Vec::new(),
                })
                .ok()
                .flatten()
                .and_then(|n| n.checked_add(disc)),
        };
        summary.expected_size = expected;

        let outcomes: Outcomes = idxs
            .iter()
            .map(|&i| {
                let body = accounts[i].data.get(disc..).unwrap_or_default();
                let result = self
                    .decoder
                    .type_def(td, body)
                    .map(|d| (d.value, disc + d.len))
                    .map_err(|e| e.offset_by(disc));
                (i, result)
            })
            .collect();
        summary.bytes_used = Range::of(
            outcomes
                .iter()
                .filter_map(|(_, r)| r.as_ref().ok().map(|(_, used)| *used)),
        );

        let hint = self.short_accounts_hint(&outcomes, expected);
        self.report_decode_failures(&subject, &outcomes, hint, |v, i| {
            v.samples.accounts[i].address.clone()
        });
        if expected.is_some() {
            self.report_fixed_leftover(&subject, nd, td, zero_copy, &outcomes);
            self.report_size_distribution(&subject, &sizes);
        } else {
            self.report_variable_leftover(&subject, &outcomes);
        }

        let padded = zero_copy.filter(|l| l.padding_len() > 0);
        self.compare_accounts(&subject, &outcomes, padded);
        summary
    }

    /// How `td` is laid out in account data; `None` (after a warning) when the zero-copy
    /// layout cannot be computed.
    fn account_layout(&mut self, subject: &str, td: &TypeDef) -> Option<AccountLayout> {
        if !td.is_zero_copy() {
            return Some(AccountLayout::Borsh);
        }
        let layout = match self.decoder.types().struct_layout(td) {
            Ok(layout) => layout,
            Err(e) => {
                self.push(
                    Severity::Warning,
                    Check::Layout,
                    subject,
                    format!("cannot verify: the zero-copy layout is unknown ({e})"),
                    Some("only zero-copy structs whose layout the IDL pins down can be checked: repr(C), no options, vecs or enums".into()),
                    None,
                );
                return None;
            }
        };
        // Pod refuses implicit padding at compile time, so a gap means the IDL is wrong.
        if td.serialization == "bytemuck" && layout.padding_len() > 0 {
            self.push(
                Severity::Error,
                Check::Layout,
                subject,
                format!(
                    "the IDL's repr(C) layout has {} of implicit padding ({}), but a `bytemuck` (Pod) type cannot have any",
                    bytes(layout.padding_len()),
                    padding_list(&layout)
                ),
                Some("the IDL's `repr` or field list is wrong; a missing `packed: true` is the usual cause".into()),
                None,
            );
        }
        Some(AccountLayout::ZeroCopy(layout))
    }

    /// A hint for decode failures when every failing account is shorter than the layout.
    fn short_accounts_hint(&self, outcomes: &Outcomes, expected: Option<usize>) -> Option<String> {
        let expected = expected?;
        let failing: Vec<usize> = outcomes
            .iter()
            .filter(|(_, r)| r.is_err())
            .map(|(i, _)| self.samples.accounts[*i].data.len())
            .collect();
        if failing.is_empty() || failing.iter().any(|len| *len >= expected) {
            return None;
        }
        let sizes = Range::of(failing).map_or_else(String::new, |r| r.to_string());
        Some(format!(
            "the IDL layout needs {expected} bytes and these accounts are {sizes} bytes: they may predate a program upgrade, or the IDL is out of date"
        ))
    }

    fn report_decode_failures(
        &mut self,
        subject: &str,
        outcomes: &Outcomes,
        hint: Option<String>,
        id: impl Fn(&Self, usize) -> String,
    ) {
        let failures: Vec<(usize, &DecodeError)> = outcomes
            .iter()
            .filter_map(|(i, r)| r.as_ref().err().map(|e| (*i, e)))
            .collect();
        let Some(&(first, err)) = failures.first() else {
            return;
        };
        let message = format!(
            "fails to decode in {}: {err}",
            of_samples(failures.len(), outcomes.len())
        );
        let example = id(self, first);
        self.push(
            Severity::Error,
            Check::Decode,
            subject,
            message,
            hint,
            Some(&example),
        );
    }

    fn report_size_distribution(&mut self, subject: &str, sizes: &BTreeMap<usize, usize>) {
        if sizes.len() < 2 {
            return;
        }
        let list: Vec<String> = sizes.iter().map(|(s, c)| format!("{s} ({c})")).collect();
        self.push(
            Severity::Note,
            Check::SizeDistribution,
            subject,
            format!(
                "this fixed-size type comes in {} sizes: {}",
                sizes.len(),
                list.join(", ")
            ),
            Some(
                "the odd sizes may be accounts created before a program upgrade, or reallocated"
                    .into(),
            ),
            None,
        );
    }

    /// Unused bytes on a fixed-size type: padding the IDL dropped, spare allocation, or a
    /// missing field.
    fn report_fixed_leftover(
        &mut self,
        subject: &str,
        nd: &NamedDisc,
        td: &TypeDef,
        zero_copy: Option<&StructLayout>,
        outcomes: &Outcomes,
    ) {
        let accounts = &self.samples.accounts;
        let disc = nd.payload_start();
        let mut classes: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (i, result) in outcomes {
            if let Ok((_, used)) = result {
                let size = accounts[*i].data.len();
                if size > *used {
                    classes.entry((size, *used)).or_default().push(*i);
                }
            }
        }
        let total = outcomes.len();
        let tails: Vec<&[u8]> = outcomes
            .iter()
            .filter_map(|(i, r)| accounts[*i].data.get(r.as_ref().ok()?.1..))
            .collect();
        let missing_field = missing_field_hint(&tails);
        for ((size, used), idxs) in classes {
            let unused = size - used;
            let count = of_samples(idxs.len(), total);
            let example = accounts[idxs[0]].address.clone();
            // A Borsh-labeled struct whose padded repr(C) size is exactly the account size.
            let padded = zero_copy
                .is_none()
                .then(|| self.decoder.types().struct_layout(td).ok())
                .flatten()
                .filter(|l| l.padding_len() > 0 && disc.checked_add(l.size) == Some(size));
            let (severity, message, hint) = if let Some(layout) = padded {
                let shifted = layout
                    .padding
                    .iter()
                    .find_map(|p| p.before.as_deref())
                    .map_or_else(String::new, |f| {
                        format!(" A decoder built from it reads `{f}` and every later field at the wrong offset.")
                    });
                (
                    Severity::Error,
                    format!("{unused} of {size} bytes unused in {count}"),
                    format!(
                        "{size} bytes is exactly the repr(C) layout ({disc} + {}), which pads {}. The IDL lays this type out as Borsh, without the padding.{shifted} Keep the Anchor IDL's `serialization` and `repr`, or add the padding as explicit fields.",
                        layout.size,
                        padding_list(&layout)
                    ),
                )
            } else if Some(size) == round_up_8(used) {
                (
                    Severity::Warning,
                    format!("{} after the data in {count}", bytes(unused)),
                    "the account size is the data size rounded up to 8 bytes, which suggests spare allocation; confirm against the program source".to_string(),
                )
            } else {
                let mut hint = missing_field.clone().unwrap_or_else(|| "the IDL may be missing fields or padding, or the program allocates more space than the type needs".to_string());
                if nd.size == Some(used) {
                    let _ = write!(
                        hint,
                        " (the IDL's declared account size, {used}, leaves them out too)"
                    );
                }
                (
                    Severity::Error,
                    format!("{unused} of {size} bytes unused in {count}"),
                    hint,
                )
            };
            self.push(
                severity,
                Check::BytesUsed,
                subject,
                message,
                Some(hint),
                Some(&example),
            );
        }
    }

    /// Unused bytes on a variable-size type are normal (accounts are allocated at their
    /// maximum size) as long as they are zero.
    fn report_variable_leftover(&mut self, subject: &str, outcomes: &Outcomes) {
        let accounts = &self.samples.accounts;
        let mut affected = Vec::new();
        let mut nonzero = Vec::new();
        let mut most = 0;
        for (i, result) in outcomes {
            let Ok((_, used)) = result else { continue };
            let data = &accounts[*i].data;
            let Some(rest) = data.get(*used..).filter(|r| !r.is_empty()) else {
                continue;
            };
            affected.push(*i);
            most = most.max(rest.len());
            if rest.iter().any(|b| *b != 0) {
                nonzero.push(*i);
            }
        }
        let total = outcomes.len();
        if let Some(&first) = nonzero.first() {
            let example = accounts[first].address.clone();
            let tails: Vec<&[u8]> = outcomes
                .iter()
                .filter_map(|(i, r)| accounts[*i].data.get(r.as_ref().ok()?.1..))
                .collect();
            let hint = missing_field_hint(&tails)
                .unwrap_or_else(|| "the IDL may be missing trailing fields".into());
            self.push(
                Severity::Warning,
                Check::BytesUsed,
                subject,
                format!(
                    "non-zero bytes follow the decoded data in {}",
                    of_samples(nonzero.len(), total)
                ),
                Some(hint),
                Some(&example),
            );
        } else if !affected.is_empty() {
            self.push(
                Severity::Note,
                Check::BytesUsed,
                subject,
                format!(
                    "up to {} of zeros follow the decoded data in {} (spare allocation)",
                    bytes(most),
                    of_samples(affected.len(), total)
                ),
                None,
                None,
            );
        }
    }

    fn compare_accounts(
        &mut self,
        subject: &str,
        outcomes: &Outcomes,
        padded: Option<&StructLayout>,
    ) {
        let Some(reference) = self.reference else {
            return;
        };
        let decoded: Vec<(usize, &Value)> = outcomes
            .iter()
            .filter_map(|(i, r)| r.as_ref().ok().map(|(v, _)| (*i, v)))
            .collect();
        if decoded.is_empty() {
            return;
        }
        if let Some(layout) = padded.filter(|_| !reference.honors_padding) {
            self.reference_summary.skipped += decoded.len();
            self.push(
                Severity::Note,
                Check::Reference,
                subject,
                format!(
                    "not compared with the reference decoder, which ignores the {} of repr(C) padding in zero-copy types",
                    bytes(layout.padding_len())
                ),
                None,
                None,
            );
            return;
        }
        let entries: Vec<(usize, Option<&Outcome>, &Value)> = decoded
            .iter()
            .map(|&(i, v)| (i, reference.accounts.get(i), v))
            .collect();
        let (compared, matched) = self.compare(subject, &entries, |i| {
            self.samples.accounts[i].address.clone()
        });
        self.reference_summary.accounts_compared += compared;
        self.reference_summary.accounts_matched += matched;
    }

    /// Compare decoded values with reference outcomes; returns (compared, matched).
    fn compare(
        &mut self,
        subject: &str,
        entries: &[(usize, Option<&Outcome>, &Value)],
        id: impl Fn(usize) -> String,
    ) -> (usize, usize) {
        let (label, failure_severity) =
            self.reference
                .map_or(("the reference decoder", Severity::Warning), |r| {
                    let severity = if r.failures_are_errors {
                        Severity::Error
                    } else {
                        Severity::Warning
                    };
                    (r.label, severity)
                });
        let theirs_name = if label == "the reference decoder" {
            "the reference"
        } else {
            label
        };
        let mut compared = 0;
        let mut mismatches = Vec::new();
        let mut failures = Vec::new();
        let mut skipped = Vec::new();
        for &(i, outcome, value) in entries {
            match outcome {
                Some(Outcome::Ok(theirs)) => {
                    compared += 1;
                    if let Some(d) = first_difference(&to_json(value), theirs) {
                        mismatches.push((i, d));
                    }
                }
                Some(Outcome::Failed(err)) => failures.push((i, err.clone())),
                Some(Outcome::Skipped(why)) => skipped.push(why.clone()),
                None => {}
            }
        }
        if let Some(why) = skipped.first() {
            self.reference_summary.skipped += skipped.len();
            self.push(
                Severity::Note,
                Check::Reference,
                subject,
                format!(
                    "not compared: {label} skipped {}: {why}",
                    of_samples(skipped.len(), entries.len())
                ),
                None,
                None,
            );
        }
        if let Some((i, d)) = mismatches.first() {
            let at = if d.path.is_empty() {
                String::new()
            } else {
                format!(", first at `{}`", d.path)
            };
            let example = id(*i);
            self.push(
                Severity::Error,
                Check::Reference,
                subject,
                format!(
                    "values differ from {label} in {}{at}: idl-verify read {}, {theirs_name} {}",
                    of_samples(mismatches.len(), entries.len()),
                    d.ours,
                    d.theirs
                ),
                Some("one of the two decoders misreads these bytes; check the field's type and offset".into()),
                Some(&example),
            );
        }
        if let Some((i, err)) = failures.first() {
            let example = id(*i);
            self.push(
                failure_severity,
                Check::Reference,
                subject,
                format!(
                    "{label} failed on {}: {err}",
                    of_samples(failures.len(), entries.len())
                ),
                (failure_severity == Severity::Error)
                    .then(|| format!("{label} rejects data that the IDL decodes")),
                Some(&example),
            );
        }
        (compared, compared - mismatches.len())
    }

    // --- instructions ------------------------------------------------------------------

    fn check_instructions(&mut self) -> (Coverage, Vec<InstructionSummary>) {
        let idl = self.idl;
        let table = disc_table(
            idl.instructions
                .iter()
                .map(|ix| (ix.discriminator_offset, ix.discriminator.as_slice())),
        );
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); idl.instructions.len()];
        let mut unknown: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, sample) in self.samples.instructions.iter().enumerate() {
            match longest_match(&table, &sample.data) {
                Some(matches) => {
                    let pick =
                        pick_by_account_count(&idl.instructions, &matches, sample.accounts.len());
                    groups[pick].push(i);
                }
                None => unknown.entry(prefix_hex(&sample.data)).or_default().push(i),
            }
        }

        let unknown_count: usize = unknown.values().map(Vec::len).sum();
        if table.is_empty() && !idl.instructions.is_empty() && unknown_count > 0 {
            self.push(
                Severity::Error,
                Check::Discriminator,
                "instructions",
                format!(
                    "the IDL gives no instruction discriminators, so none of the {unknown_count} sampled instructions can be classified"
                ),
                Some("add the discriminators to the IDL; idl-verify computes them only for legacy (pre-0.30) Anchor IDLs".into()),
                None,
            );
        } else {
            for (prefix, idxs) in &unknown {
                let example = self.samples.instructions[idxs[0]].signature.clone();
                self.push(
                    Severity::Error,
                    Check::Discriminator,
                    "instructions",
                    format!(
                        "{prefix} matches no instruction discriminator in the IDL, in {} {}",
                        idxs.len(),
                        plural(idxs.len(), "sample", "samples")
                    ),
                    Some("the program has an instruction the IDL does not declare".into()),
                    Some(&example),
                );
            }
        }

        let mut summaries = Vec::new();
        let mut not_sampled = Vec::new();
        for (ix, idxs) in idl.instructions.iter().zip(&groups) {
            if idxs.is_empty() {
                not_sampled.push(ix.name.clone());
            } else {
                summaries.push(self.check_instruction(ix, idxs));
            }
        }
        let coverage = Coverage {
            sampled: self.samples.instructions.len(),
            classified: self.samples.instructions.len() - unknown_count,
            not_sampled,
        };
        (coverage, summaries)
    }

    fn check_instruction(&mut self, ix: &Instruction, idxs: &[usize]) -> InstructionSummary {
        let subject = format!("instruction {}", ix.name);
        let disc = ix
            .discriminator_offset
            .saturating_add(ix.discriminator.len());
        let flat = flatten(&ix.accounts);
        let samples = &self.samples.instructions;

        let outcomes: Outcomes = idxs
            .iter()
            .map(|&i| {
                let body = samples[i].data.get(disc..).unwrap_or_default();
                let result = self
                    .decoder
                    .borsh_fields(&ix.args, body)
                    .map(|d| (d.value, disc + d.len))
                    .map_err(|e| e.offset_by(disc));
                (i, result)
            })
            .collect();

        self.report_decode_failures(&subject, &outcomes, None, |v, i| {
            v.samples.instructions[i].signature.clone()
        });
        self.report_trailing_args(&subject, &outcomes);
        self.report_account_counts(&subject, &flat, idxs);
        self.report_fixed_addresses(&subject, &flat, idxs);
        self.report_account_flags(&subject, &flat, idxs);
        self.compare_instructions(&subject, ix, &outcomes);

        let zero = Range { min: 0, max: 0 };
        InstructionSummary {
            name: ix.name.clone(),
            samples: idxs.len(),
            idl_accounts: flat.len(),
            sample_accounts: Range::of(idxs.iter().map(|&i| samples[i].accounts.len()))
                .unwrap_or(zero),
            data_sizes: Range::of(idxs.iter().map(|&i| samples[i].data.len())).unwrap_or(zero),
        }
    }

    /// Instruction data has no allocation slack, so any byte after the arguments matters.
    fn report_trailing_args(&mut self, subject: &str, outcomes: &Outcomes) {
        let samples = &self.samples.instructions;
        let trailing: Vec<(usize, usize)> = outcomes
            .iter()
            .filter_map(|(i, r)| {
                let used = r.as_ref().ok()?.1;
                let extra = samples[*i].data.len().checked_sub(used)?;
                (extra > 0).then_some((*i, extra))
            })
            .collect();
        let Some(&(first, _)) = trailing.first() else {
            return;
        };
        let extra = match Range::of(trailing.iter().map(|(_, n)| *n)) {
            Some(r) if r.min == r.max => bytes(r.min),
            Some(r) => format!("{r} bytes"),
            None => return,
        };
        let example = samples[first].signature.clone();
        let tails: Vec<&[u8]> = outcomes
            .iter()
            .filter_map(|(i, r)| samples[*i].data.get(r.as_ref().ok()?.1..))
            .collect();
        let hint = missing_field_hint(&tails).unwrap_or_else(|| {
            "the IDL may be missing arguments, or declares one narrower than the program reads"
                .into()
        });
        self.push(
            Severity::Error,
            Check::BytesUsed,
            subject,
            format!(
                "{extra} after the arguments in {}",
                of_samples(trailing.len(), outcomes.len())
            ),
            Some(hint),
            Some(&example),
        );
    }

    fn compare_instructions(&mut self, subject: &str, ix: &Instruction, outcomes: &Outcomes) {
        let Some(reference) = self.reference else {
            return;
        };
        let lone_arg = match ix.args.as_slice() {
            [arg] => Some(arg.name.as_str()),
            _ => None,
        };
        let (compared, matched) = self.compare_named(
            subject,
            &ix.name,
            lone_arg,
            outcomes,
            &reference.instructions,
            |i| self.samples.instructions[i].signature.clone(),
        );
        self.reference_summary.instructions_compared += compared;
        self.reference_summary.instructions_matched += matched;
    }

    /// Compare `{name, data}` replies: the name first, then the data. Returns
    /// (compared, matched).
    fn compare_named(
        &mut self,
        subject: &str,
        name: &str,
        lone_arg: Option<&str>,
        outcomes: &Outcomes,
        theirs: &[Outcome],
        id: impl Fn(usize) -> String,
    ) -> (usize, usize) {
        let mut entries = Vec::new();
        let mut wrong_name = Vec::new();
        for (i, result) in outcomes {
            let Ok((value, _)) = result else { continue };
            match theirs.get(*i) {
                Some(Outcome::Ok(reply)) => {
                    let their_name = reply
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default();
                    if normalize(their_name) == normalize(name) {
                        let mut data = reply.get("data").cloned().unwrap_or_default();
                        // Shipstern writes a lone struct argument as the struct itself.
                        if let Some(arg) = lone_arg {
                            let named = data
                                .as_object()
                                .is_some_and(|o| o.keys().any(|k| normalize(k) == normalize(arg)));
                            if !named {
                                data = serde_json::json!({ arg: data });
                            }
                        }
                        entries.push((*i, Some(Outcome::Ok(data)), value));
                    } else {
                        wrong_name.push((*i, their_name.to_string()));
                    }
                }
                other => entries.push((*i, other.cloned(), value)),
            }
        }
        if let Some((i, their_name)) = wrong_name.first() {
            let example = id(*i);
            self.push(
                Severity::Error,
                Check::Reference,
                subject,
                format!(
                    "{} names it `{their_name}` in {}",
                    self.reference.map_or("the reference decoder", |r| r.label),
                    of_samples(wrong_name.len(), outcomes.len())
                ),
                None,
                Some(&example),
            );
        }
        let borrowed: Vec<(usize, Option<&Outcome>, &Value)> = entries
            .iter()
            .map(|(i, o, v)| (*i, o.as_ref(), *v))
            .collect();
        let (compared, matched) = self.compare(subject, &borrowed, &id);
        (compared + wrong_name.len(), matched)
    }

    fn report_account_counts(&mut self, subject: &str, flat: &[&Account], idxs: &[usize]) {
        let samples = &self.samples.instructions;
        let total = idxs.len();
        let mut short: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut extra = Vec::new();
        for &i in idxs {
            let n = samples[i].accounts.len();
            if n < flat.len() {
                short.entry(n).or_default().push(i);
            } else if n > flat.len() {
                extra.push(n - flat.len());
            }
        }
        for (n, group) in short {
            let missing = &flat[n..];
            let required = missing.iter().any(|a| !a.optional);
            let names: Vec<String> = missing
                .iter()
                .map(|a| match &a.address {
                    Some(addr) => format!("{} (fixed: {addr})", a.name),
                    None => a.name.clone(),
                })
                .collect();
            let fixed = missing.iter().all(|a| a.address.is_some());
            // Shipstern reads an absent optional tail account as `None`, so nothing breaks.
            let (severity, hint) = if !required {
                (Severity::Note, "every missing account is optional at the tail; a generated parser reads them as absent")
            } else if fixed {
                (Severity::Error, "every missing account has a fixed address, so the program likely stopped requiring them; a parser that checks the IDL's account count rejects these instructions")
            } else {
                (
                    Severity::Error,
                    "a parser that checks the IDL's account count rejects these instructions",
                )
            };
            let example = samples[group[0]].signature.clone();
            self.push(
                severity,
                Check::AccountCount,
                subject,
                format!(
                    "{n} accounts passed where the IDL lists {}, in {}; missing: {}",
                    flat.len(),
                    of_samples(group.len(), total),
                    names.join(", ")
                ),
                Some(hint.to_string()),
                Some(&example),
            );
        }
        if let Some(most) = extra.iter().max() {
            self.push(
                Severity::Note,
                Check::AccountCount,
                subject,
                format!(
                    "up to {most} accounts passed beyond the {} the IDL lists (remaining accounts), in {}",
                    flat.len(),
                    of_samples(extra.len(), total)
                ),
                None,
                None,
            );
        }
    }

    fn report_fixed_addresses(&mut self, subject: &str, flat: &[&Account], idxs: &[usize]) {
        let samples = &self.samples.instructions;
        let program_id = self.program_id().to_string();
        for (pos, account) in flat.iter().enumerate() {
            let Some(address) = &account.address else {
                continue;
            };
            let wrong: Vec<(usize, &str)> = idxs
                .iter()
                .filter_map(|&i| {
                    let key = samples[i].accounts.get(pos)?;
                    // Anchor passes the program id in place of an absent optional account.
                    let absent = account.optional && *key == program_id;
                    (key != address && !absent).then_some((i, key.as_str()))
                })
                .collect();
            if let Some(&(first, key)) = wrong.first() {
                let example = samples[first].signature.clone();
                self.push(
                    Severity::Error,
                    Check::FixedAddress,
                    subject,
                    format!(
                        "account #{pos} `{}` must be {address}, but another key is passed in {} (such as {key})",
                        account.name,
                        of_samples(wrong.len(), idxs.len())
                    ),
                    Some("the IDL's account order does not match the program, or an earlier account is missing".into()),
                    Some(&example),
                );
            }
        }
    }

    /// A key's message flags are the union over all instructions, so only a missing privilege
    /// is evidence. A CPI can sign for a PDA, so only top-level samples count for `signer`.
    fn report_account_flags(&mut self, subject: &str, flat: &[&Account], idxs: &[usize]) {
        let samples = &self.samples.instructions;
        let program_id = self.program_id().to_string();
        let flagged: Vec<(usize, &KeyFlags)> = idxs
            .iter()
            .filter_map(|&i| Some((i, samples[i].flags.as_ref()?)))
            .collect();
        let top_level = flagged.iter().filter(|(_, f)| !f.cpi).count();
        let mut found = Vec::new();
        for (pos, account) in flat.iter().enumerate() {
            let mut read_only = Vec::new();
            let mut unsigned = Vec::new();
            for &(i, flags) in &flagged {
                let Some(key) = samples[i].accounts.get(pos) else {
                    continue;
                };
                // Anchor passes the program id in place of an absent optional account.
                if account.optional && *key == program_id {
                    continue;
                }
                if account.writable && flags.writable.get(pos) == Some(&false) {
                    read_only.push((i, key.as_str()));
                }
                let must_sign = account.signer && !account.may_sign && !flags.cpi;
                if must_sign && flags.signer.get(pos) == Some(&false) {
                    unsigned.push((i, key.as_str()));
                }
            }
            if let Some(&(first, key)) = read_only.first() {
                found.push((
                    first,
                    format!(
                        "account #{pos} `{}` is writable in the IDL, but the key passed there is read-only in {} (such as {key})",
                        account.name,
                        of_samples(read_only.len(), flagged.len())
                    ),
                ));
            }
            if let Some(&(first, key)) = unsigned.first() {
                found.push((
                    first,
                    format!(
                        "account #{pos} `{}` is a signer in the IDL, but the key passed there does not sign the transaction in {} (top-level calls only; such as {key})",
                        account.name,
                        of_samples(unsigned.len(), top_level)
                    ),
                ));
            }
        }
        for (first, message) in found {
            let example = samples[first].signature.clone();
            self.push(
                Severity::Error,
                Check::AccountFlags,
                subject,
                message,
                Some("the IDL's account order likely does not match the program, or an earlier account is missing".into()),
                Some(&example),
            );
        }
    }

    // --- events ------------------------------------------------------------------------

    fn check_events(&mut self) -> (Coverage, Vec<EventSummary>) {
        let idl = self.idl;
        let samples = &self.samples.events;
        if idl.events.is_empty() {
            if let Some(first) = samples.first() {
                self.push(
                    Severity::Error,
                    Check::Discriminator,
                    "events",
                    format!(
                        "the IDL declares no events, but the samples hold {} {}",
                        samples.len(),
                        plural(samples.len(), "event", "events")
                    ),
                    Some("the program emits events the IDL does not describe; add them to `events`, with their payload types in `types`".into()),
                    Some(&first.signature),
                );
            }
            let coverage = Coverage {
                sampled: samples.len(),
                classified: 0,
                not_sampled: Vec::new(),
            };
            return (coverage, Vec::new());
        }

        let envelopes = envelopes(idl);
        let bodies: Vec<Option<&[u8]>> =
            samples.iter().map(|s| event_body(s, &envelopes)).collect();

        let table = disc_table(idl.events.iter().map(|e| (0, e.discriminator.as_slice())));
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); idl.events.len()];
        let mut no_envelope: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut unknown: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, body) in bodies.iter().enumerate() {
            let Some(body) = body else {
                no_envelope
                    .entry(prefix_hex(&samples[i].data))
                    .or_default()
                    .push(i);
                continue;
            };
            match longest_match(&table, body) {
                Some(matches) => groups[matches[0]].push(i),
                None => unknown.entry(prefix_hex(body)).or_default().push(i),
            }
        }
        self.report_envelope_misses(&envelopes, &no_envelope);

        let unknown_count: usize = unknown.values().map(Vec::len).sum();
        if table.is_empty() && unknown_count > 0 {
            self.push(
                Severity::Error,
                Check::Discriminator,
                "events",
                format!(
                    "the IDL gives no event discriminators, so none of the {unknown_count} sampled events can be classified"
                ),
                Some("add the discriminators to the IDL; idl-verify computes them only for legacy (pre-0.30) Anchor IDLs".into()),
                None,
            );
        } else {
            for (prefix, idxs) in &unknown {
                let example = samples[idxs[0]].signature.clone();
                self.push(
                    Severity::Error,
                    Check::Discriminator,
                    "events",
                    format!(
                        "{prefix} matches no event discriminator in the IDL, in {} {}",
                        idxs.len(),
                        plural(idxs.len(), "sample", "samples")
                    ),
                    Some("the program emits an event the IDL does not declare".into()),
                    Some(&example),
                );
            }
        }

        let mut summaries = Vec::new();
        let mut not_sampled = Vec::new();
        for (ev, idxs) in idl.events.iter().zip(&groups) {
            if idxs.is_empty() {
                not_sampled.push(ev.name.clone());
            } else {
                summaries.push(self.check_event_type(ev, idxs, &bodies));
            }
        }
        let missed = unknown_count + no_envelope.values().map(Vec::len).sum::<usize>();
        let coverage = Coverage {
            sampled: samples.len(),
            classified: samples.len().saturating_sub(missed),
            not_sampled,
        };
        (coverage, summaries)
    }

    fn report_envelope_misses(
        &mut self,
        envelopes: &[Envelope<'_>],
        misses: &BTreeMap<String, Vec<usize>>,
    ) {
        let (expected, hint) = if envelopes == [ANCHOR_ENVELOPE] {
            (
                format!("Anchor's event tag {}", prefix_hex(&EVENT_IX_TAG)),
                "the IDL gives no envelope, so idl-verify expects the tag Anchor's `emit_cpi!` writes; a program that wraps its events another way needs the envelope in the IDL (in Codama, the tag as a constant discriminator at offset 0, before the event's)",
            )
        } else {
            let list: Vec<String> = envelopes.iter().map(ToString::to_string).collect();
            (
                format!("the IDL's envelope {}", list.join(" or ")),
                "the IDL's envelope does not match how the program wraps its self-CPI events",
            )
        };
        for (prefix, idxs) in misses {
            let example = self.samples.events[idxs[0]].signature.clone();
            self.push(
                Severity::Error,
                Check::Envelope,
                "events",
                format!(
                    "self-CPI data {prefix} lacks {expected}, in {} {}",
                    idxs.len(),
                    plural(idxs.len(), "sample", "samples")
                ),
                Some(hint.into()),
                Some(&example),
            );
        }
    }

    fn check_event_type(
        &mut self,
        ev: &Event,
        idxs: &[usize],
        bodies: &[Option<&[u8]>],
    ) -> EventSummary {
        let subject = format!("event {}", ev.name);
        let samples = &self.samples.events;
        let body = |i: usize| bodies[i].unwrap_or_default();
        let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
        let mut sources = EventSources::default();
        for &i in idxs {
            *sizes.entry(body(i).len()).or_default() += 1;
            match samples[i].source {
                EventSource::Log => sources.log += 1,
                EventSource::Cpi => sources.cpi += 1,
            }
        }
        let mut summary = EventSummary {
            name: ev.name.clone(),
            samples: idxs.len(),
            sources,
            sizes: sizes
                .iter()
                .map(|(&size, &count)| SizeCount { size, count })
                .collect(),
            bytes_used: None,
        };

        let Some(td) = self.decoder.types().get(&ev.name) else {
            self.push(
                Severity::Error,
                Check::MissingType,
                &subject,
                "has no entry in the IDL's `types`, so its payload cannot be decoded".into(),
                None,
                None,
            );
            return summary;
        };
        let disc = ev.discriminator.len();
        let outcomes: Outcomes = idxs
            .iter()
            .map(|&i| {
                let payload = body(i).get(disc..).unwrap_or_default();
                let result = self
                    .decoder
                    .type_def(td, payload)
                    .map(|d| (d.value, disc.saturating_add(d.len)))
                    .map_err(|e| e.offset_by(disc));
                (i, result)
            })
            .collect();
        summary.bytes_used = Range::of(
            outcomes
                .iter()
                .filter_map(|(_, r)| r.as_ref().ok().map(|(_, used)| *used)),
        );

        self.report_decode_failures(&subject, &outcomes, None, |v, i| {
            v.samples.events[i].signature.clone()
        });
        self.report_event_leftover(&subject, &outcomes, bodies);
        if let Some(reference) = self.reference {
            let (compared, matched) = self.compare_named(
                &subject,
                &ev.name,
                None,
                &outcomes,
                &reference.events,
                |i| self.samples.events[i].signature.clone(),
            );
            self.reference_summary.events_compared += compared;
            self.reference_summary.events_matched += matched;
        }
        summary
    }

    /// Event data has no allocation slack, so bytes after the payload are data the IDL
    /// does not describe. Zeros alone may be a missing field that happens to be zero.
    fn report_event_leftover(
        &mut self,
        subject: &str,
        outcomes: &Outcomes,
        bodies: &[Option<&[u8]>],
    ) {
        let mut affected = Vec::new();
        let mut first_nonzero = None;
        for (i, result) in outcomes {
            let Ok((_, used)) = result else { continue };
            let Some(rest) = bodies[*i]
                .and_then(|b| b.get(*used..))
                .filter(|r| !r.is_empty())
            else {
                continue;
            };
            affected.push((*i, rest.len()));
            if first_nonzero.is_none() && rest.iter().any(|b| *b != 0) {
                first_nonzero = Some(*i);
            }
        }
        let (Some(&(first, _)), Some(extra)) = (
            affected.first(),
            Range::of(affected.iter().map(|(_, n)| *n)),
        ) else {
            return;
        };
        let extra = if extra.min == extra.max {
            bytes(extra.min)
        } else {
            format!("{extra} bytes")
        };
        let (severity, what, hint, example) = match first_nonzero {
            Some(i) => (
                Severity::Error,
                "",
                "the IDL may be missing trailing fields, or declares one narrower than the program writes",
                i,
            ),
            None => (
                Severity::Note,
                " of zeros",
                "a trailing field the IDL lacks, or declares too narrow, may hold zero in these samples",
                first,
            ),
        };
        let example = self.samples.events[example].signature.clone();
        self.push(
            severity,
            Check::BytesUsed,
            subject,
            format!(
                "{extra}{what} after the payload in {}",
                of_samples(affected.len(), outcomes.len())
            ),
            Some(hint.into()),
            Some(&example),
        );
    }
}

fn unknown_account_hint(idl: &Idl, sized: &[(usize, usize)], by_size: bool) -> String {
    if by_size {
        let known: Vec<String> = sized
            .iter()
            .map(|(t, size)| format!("{} {size}", idl.accounts[*t].name))
            .collect();
        format!(
            "the IDL tells these accounts apart by size ({}); a longer account has room the IDL does not declare, or is a type it lacks",
            known.join(", ")
        )
    } else {
        "the program uses an account type the IDL does not declare, or these accounts belong to another program".to_string()
    }
}

/// The self-CPI envelope before an event: the bytes it starts with, and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Envelope<'a> {
    prefix: &'a [u8],
    len: usize,
}

const ANCHOR_ENVELOPE: Envelope<'static> = Envelope {
    prefix: &EVENT_IX_TAG,
    len: EVENT_IX_TAG.len(),
};

impl std::fmt::Display for Envelope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.prefix.is_empty() {
            write!(f, "of {}", bytes(self.len))
        } else {
            f.write_str(&prefix_hex(self.prefix))
        }
    }
}

/// The IDL's envelope for `event`, else Anchor's: an Anchor IDL cannot describe one.
fn envelope(event: &Event) -> Envelope<'_> {
    match event.effective_payload_offset() {
        Some(len) => Envelope {
            prefix: &event.envelope_discriminator,
            len,
        },
        None => ANCHOR_ENVELOPE,
    }
}

/// The IDL's envelopes, longest prefix first, so an envelope wins over a shorter one it extends.
pub(crate) fn envelopes(idl: &Idl) -> Vec<Envelope<'_>> {
    let mut envelopes: Vec<Envelope<'_>> = idl.events.iter().map(envelope).collect();
    envelopes.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()).then(a.cmp(b)));
    envelopes.dedup();
    envelopes
}

/// The event bytes, `[discriminator][payload]`, with a self-CPI sample's envelope removed;
/// `None` for a cpi sample that does not start with any envelope the IDL expects.
pub(crate) fn event_body<'s>(
    sample: &'s EventSample,
    envelopes: &[Envelope<'_>],
) -> Option<&'s [u8]> {
    match sample.source {
        EventSource::Log => Some(&sample.data),
        EventSource::Cpi => envelopes
            .iter()
            .filter(|e| sample.data.starts_with(e.prefix))
            .find_map(|e| sample.data.get(e.len..)),
    }
}

/// The envelopes that mark a self-CPI event instruction. Shipstern uses Anchor's tag when
/// the IDL declares none.
pub(crate) fn cpi_envelopes(idl: &Idl) -> Vec<Envelope<'_>> {
    let mut cpi: Vec<Envelope<'_>> = envelopes(idl)
        .into_iter()
        .filter(|e| !e.prefix.is_empty())
        .collect();
    if cpi.is_empty() {
        cpi.push(ANCHOR_ENVELOPE);
    }
    cpi
}

impl Envelope<'_> {
    pub(crate) fn wraps(&self, data: &[u8]) -> bool {
        data.starts_with(self.prefix)
    }
}

/// A discriminator: where it starts in the data, and its bytes.
pub(crate) type Disc<'a> = (usize, &'a [u8]);

/// `(discriminator, index)` pairs, longest discriminator first, empty ones dropped.
pub(crate) fn disc_table<'a>(discs: impl Iterator<Item = Disc<'a>>) -> Vec<(Disc<'a>, usize)> {
    let mut table: Vec<(Disc<'a>, usize)> = discs
        .enumerate()
        .filter(|(_, (_, d))| !d.is_empty())
        .map(|(i, d)| (d, i))
        .collect();
    table.sort_by_key(|((_, d), i)| (std::cmp::Reverse(d.len()), *i));
    table
}

/// Every entry whose discriminator is the longest one found in `data` at its offset.
pub(crate) fn longest_match(table: &[(Disc<'_>, usize)], data: &[u8]) -> Option<Vec<usize>> {
    let at = |(offset, disc): &Disc<'_>| data.get(*offset..).is_some_and(|d| d.starts_with(disc));
    let (found, _) = table.iter().find(|(d, _)| at(d))?;
    Some(
        table
            .iter()
            .filter(|(d, _)| d == found)
            .map(|(_, i)| *i)
            .collect(),
    )
}

/// Among instructions sharing a discriminator, prefer an exact account count, then the
/// largest count the sample satisfies (Shipstern's dispatch order).
/// Account types with no discriminator and a known size: the IDL's declared `size`, else
/// the type's fixed size. Codama's size discriminator tells such accounts apart by length.
pub(crate) fn sized_account_types(idl: &Idl) -> Vec<(usize, usize)> {
    let index = TypeIndex::new(idl);
    idl.accounts
        .iter()
        .enumerate()
        .filter(|(_, nd)| nd.discriminator.is_empty())
        .filter_map(|(i, nd)| {
            let computed = || {
                let td = index.get(&nd.name)?;
                let reading = if td.is_zero_copy() {
                    Reading::ZeroCopy
                } else {
                    Reading::Borsh
                };
                let ty = Type::Defined {
                    name: nd.name.clone(),
                    generics: Vec::new(),
                };
                index.reading_size(&ty, reading).ok()
            };
            nd.size.or_else(computed).map(|size| (i, size))
        })
        .collect()
}

pub(crate) fn pick_by_account_count(
    instructions: &[Instruction],
    candidates: &[usize],
    passed: usize,
) -> usize {
    let count = |i: &usize| flatten(&instructions[*i].accounts).len();
    candidates
        .iter()
        .find(|i| count(i) == passed)
        .or_else(|| {
            candidates
                .iter()
                .filter(|i| count(i) <= passed)
                .max_by_key(|i| count(i))
        })
        .or_else(|| candidates.first())
        .copied()
        .unwrap_or_default()
}

fn flatten(items: &[AccountItem]) -> Vec<&Account> {
    let mut out = Vec::new();
    for item in items {
        match item {
            AccountItem::Single(a) => out.push(a),
            AccountItem::Composite { accounts, .. } => out.extend(flatten(accounts)),
        }
    }
    out
}

pub(crate) fn prefix_hex(data: &[u8]) -> String {
    if data.is_empty() {
        return "empty data".to_string();
    }
    let mut out = String::from("0x");
    for b in data.iter().take(8) {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn padding_list(layout: &StructLayout) -> String {
    let parts: Vec<String> = layout.padding.iter().map(ToString::to_string).collect();
    parts.join(", ")
}

/// A hint when every sample ends in the same number of unread bytes, not all zero.
fn missing_field_hint(tails: &[&[u8]]) -> Option<String> {
    let n = tails.first()?.len();
    if n == 0
        || tails.iter().any(|t| t.len() != n)
        || tails.iter().all(|t| t.iter().all(|b| *b == 0))
    {
        return None;
    }
    let article = if n == 11 || n == 18 || n.to_string().starts_with('8') {
        "an"
    } else {
        "a"
    };
    let mut hint = format!(
        "the same {} {} in every sample: the IDL is likely missing {article} {n}-byte field at the end",
        bytes(n),
        plural(n, "follows", "follow")
    );
    // A Borsh option is a 0 or 1 tag, then the value.
    let option = match n {
        2 => Some("u8"),
        3 => Some("u16"),
        5 => Some("u32"),
        9 => Some("u64"),
        17 => Some("u128"),
        _ => None,
    };
    if let Some(ty) = option.filter(|_| tails.iter().all(|t| t.first().is_some_and(|b| *b <= 1))) {
        let _ = write!(hint, "; it fits an Option of a {ty}");
    }
    Some(hint)
}

fn round_up_8(n: usize) -> Option<usize> {
    n.div_ceil(8).checked_mul(8)
}

fn bytes(n: usize) -> String {
    format!("{n} {}", plural(n, "byte", "bytes"))
}

fn plural<'s>(n: usize, one: &'s str, many: &'s str) -> &'s str {
    if n == 1 {
        one
    } else {
        many
    }
}

fn of_samples(affected: usize, total: usize) -> String {
    format!(
        "{affected} of {total} {}",
        plural(total, "sample", "samples")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::{AccountSample, InstructionSample};

    const DISC: [u8; 2] = [7, 7];

    fn idl(accounts: &str, instructions: &str, types: &str) -> Idl {
        Idl::from_json(&format!(
            r#"{{"address":"P","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
                "accounts":{accounts},"instructions":{instructions},"types":{types}}}"#
        ))
        .unwrap()
    }

    /// One account type `A` with discriminator [7, 7] and the given type definition.
    fn account_idl(type_def: &str) -> Idl {
        idl(
            r#"[{"name":"A","discriminator":[7,7]}]"#,
            "[]",
            &format!("[{type_def}]"),
        )
    }

    fn accounts(bodies: &[&[u8]]) -> Samples {
        Samples {
            program_id: None,
            accounts: bodies
                .iter()
                .enumerate()
                .map(|(i, body)| AccountSample {
                    address: format!("addr{i}"),
                    data: [&DISC[..], body].concat(),
                })
                .collect(),
            instructions: Vec::new(),
            events: Vec::new(),
        }
    }

    fn instructions(list: &[(&[u8], &[&str])]) -> Samples {
        Samples {
            program_id: None,
            accounts: Vec::new(),
            instructions: list
                .iter()
                .map(|(data, keys)| InstructionSample {
                    signature: "sig".into(),
                    data: data.to_vec(),
                    accounts: keys.iter().map(ToString::to_string).collect(),
                    flags: None,
                })
                .collect(),
            events: Vec::new(),
        }
    }

    fn only(report: &Report) -> &Finding {
        assert_eq!(report.findings.len(), 1, "{:#?}", report.findings);
        &report.findings[0]
    }

    const STRING_TYPE: &str =
        r#"{"name":"A","type":{"kind":"struct","fields":[{"name":"s","type":"string"}]}}"#;
    const U32_TYPE: &str =
        r#"{"name":"A","type":{"kind":"struct","fields":[{"name":"v","type":"u32"}]}}"#;

    #[test]
    fn zeros_after_a_variable_size_type_are_spare_allocation() {
        let report = verify(
            &account_idl(STRING_TYPE),
            &accounts(&[&[1, 0, 0, 0, b'x', 0, 0, 0]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Note, Check::BytesUsed));
    }

    #[test]
    fn non_zero_bytes_after_a_variable_size_type_are_a_warning() {
        let report = verify(
            &account_idl(STRING_TYPE),
            &accounts(&[&[1, 0, 0, 0, b'x', 9]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Warning, Check::BytesUsed));
    }

    #[test]
    fn size_rounded_up_to_8_is_a_warning() {
        // 2 + 4 bytes used, 8 allocated.
        let report = verify(
            &account_idl(U32_TYPE),
            &accounts(&[&[1, 0, 0, 0, 0, 0]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Warning, Check::BytesUsed));
        assert_eq!(f.message, "2 bytes after the data in 1 of 1 sample");
    }

    #[test]
    fn unexplained_unused_bytes_are_an_error() {
        let report = verify(
            &account_idl(U32_TYPE),
            &accounts(&[&[1, 0, 0, 0, 5, 5, 5, 5, 5, 5, 5, 5]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Error, Check::BytesUsed));
        assert_eq!(f.message, "8 of 14 bytes unused in 1 of 1 sample");
        assert_eq!(
            f.hint.as_deref(),
            Some("the same 8 bytes follow in every sample: the IDL is likely missing an 8-byte field at the end")
        );
    }

    #[test]
    fn constant_trailing_bytes_suggest_a_missing_field() {
        let idl = idl("[]", r#"[{"name":"go","discriminator":[1]}]"#, "[]");
        let some_u64: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        let report = verify(
            &idl,
            &instructions(&[(some_u64, &[]), (some_u64, &[])]),
            None,
        );
        assert_eq!(
            only(&report).hint.as_deref(),
            Some("the same 9 bytes follow in every sample: the IDL is likely missing a 9-byte field at the end; it fits an Option of a u64")
        );
        // Lengths that vary keep the general hint.
        let report = verify(
            &idl,
            &instructions(&[(&[1, 5], &[]), (&[1, 5, 5], &[])]),
            None,
        );
        assert_eq!(
            only(&report).hint.as_deref(),
            Some(
                "the IDL may be missing arguments, or declares one narrower than the program reads"
            )
        );
    }

    #[test]
    fn sizes_of_a_fixed_type_are_counted() {
        let report = verify(
            &account_idl(U32_TYPE),
            &accounts(&[&[1, 0, 0, 0], &[2, 0, 0, 0, 0, 0]]),
            None,
        );
        let note = report
            .findings
            .iter()
            .find(|f| f.check == Check::SizeDistribution)
            .unwrap();
        assert_eq!(
            note.message,
            "this fixed-size type comes in 2 sizes: 6 (1), 8 (1)"
        );
    }

    #[test]
    fn zero_copy_type_without_a_known_layout_is_a_warning() {
        let report = verify(
            &account_idl(
                r#"{"name":"A","serialization":"bytemuck","type":{"kind":"struct","fields":[{"name":"o","type":{"option":"u8"}}]}}"#,
            ),
            &accounts(&[&[0]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Warning, Check::Layout));
    }

    #[test]
    fn account_without_a_type_definition_is_an_error() {
        let report = verify(
            &account_idl(r#"{"name":"B","type":{"kind":"struct"}}"#),
            &accounts(&[&[]]),
            None,
        );
        assert_eq!(only(&report).check, Check::MissingType);
    }

    #[test]
    fn accounts_with_no_data_are_a_note_not_an_error() {
        // perpetuals owns an empty account on mainnet; no discriminator can match it.
        let mut samples = accounts(&[&[5, 0, 0, 0]]);
        samples.accounts.push(AccountSample {
            address: "empty".into(),
            data: Vec::new(),
        });
        let report = verify(&account_idl(U32_TYPE), &samples, None);
        let f = only(&report);
        assert_eq!(
            (
                f.severity,
                f.check,
                f.message.as_str(),
                f.example.as_deref()
            ),
            (
                Severity::Note,
                Check::Discriminator,
                "1 account holds no data; not checked",
                Some("empty")
            )
        );
        assert_eq!(report.exit_code(), 0);
        assert_eq!(
            (
                report.account_coverage.sampled,
                report.account_coverage.classified
            ),
            (2, 1)
        );
    }

    #[test]
    fn idl_without_discriminators_says_so() {
        // A string field leaves no fixed size to tell accounts apart by.
        let idl = idl(
            r#"[{"name":"A"}]"#,
            "[]",
            r#"[{"name":"A","type":{"kind":"struct","fields":[{"name":"s","type":"string"}]}}]"#,
        );
        let report = verify(&idl, &accounts(&[&[1, 0, 0, 0]]), None);
        let f = only(&report);
        assert_eq!(f.check, Check::Discriminator);
        assert!(f
            .message
            .starts_with("the IDL gives no account discriminators"));
    }

    #[test]
    fn accounts_without_discriminators_are_told_apart_by_size() {
        let idl = idl(
            r#"[{"name":"Small","size":4},{"name":"Big"}]"#,
            "[]",
            r#"[{"name":"Small","type":{"kind":"struct","fields":[{"name":"a","type":"u32"}]}},
                {"name":"Big","type":{"kind":"struct","fields":[{"name":"a","type":"u64"},{"name":"b","type":"u64"}]}}]"#,
        );
        let raw = |bodies: &[&[u8]]| Samples {
            program_id: None,
            accounts: bodies
                .iter()
                .enumerate()
                .map(|(i, b)| AccountSample {
                    address: format!("addr{i}"),
                    data: b.to_vec(),
                })
                .collect(),
            instructions: Vec::new(),
            events: Vec::new(),
        };
        // 4 bytes by the declared size, 16 by the computed Borsh size.
        let report = verify(&idl, &raw(&[&[1, 0, 0, 0], &[0; 16]]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.accounts.len(), 2);
        // A length that no type has is an error that names the length.
        let report = verify(&idl, &raw(&[&[0; 7]]), None);
        let f = only(&report);
        assert_eq!(f.check, Check::Discriminator);
        assert!(
            f.message
                .starts_with("7-byte data matches no account type size"),
            "{}",
            f.message
        );
    }

    #[test]
    fn shared_discriminator_dispatches_by_account_count() {
        let idl = idl(
            "[]",
            r#"[{"name":"one","discriminator":[1],"accounts":[{"name":"a"}]},
                {"name":"two","discriminator":[1],"accounts":[{"name":"a"},{"name":"b"}],
                 "args":[{"name":"x","type":"u8"}]}]"#,
            "[]",
        );
        let report = verify(&idl, &instructions(&[(&[1, 9], &["k1", "k2"])]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.instructions[0].name, "two");
    }

    #[test]
    fn optional_tail_accounts_left_out_are_a_note_not_a_warning() {
        // SPL Governance castVote passes 11 of 13 accounts live; Shipstern parses it.
        let lenient = idl(
            "[]",
            r#"[{"name":"vote","discriminator":[1],"accounts":[
                {"name":"realm"},{"name":"voterWeightRecord","optional":true}]}]"#,
            "[]",
        );
        let report = verify(&lenient, &instructions(&[(&[1], &["R"])]), None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Note, Check::AccountCount));
        // A required account left out is still an error.
        let strict = idl(
            "[]",
            r#"[{"name":"vote","discriminator":[1],"accounts":[{"name":"realm"},{"name":"proposal"}]}]"#,
            "[]",
        );
        let report = verify(&strict, &instructions(&[(&[1], &["R"])]), None);
        assert_eq!(only(&report).severity, Severity::Error);
    }

    #[test]
    fn absent_optional_account_may_be_the_program_id() {
        let idl = idl(
            "[]",
            r#"[{"name":"go","discriminator":[1],"accounts":[
                {"name":"rent","optional":true,"address":"SysvarRent111111111111111111111111111111111"}]}]"#,
            "[]",
        );
        let report = verify(&idl, &instructions(&[(&[1], &["P"])]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    fn go(accounts: &str) -> Idl {
        idl(
            "[]",
            &format!(r#"[{{"name":"go","discriminator":[1],"accounts":{accounts}}}]"#),
            "[]",
        )
    }

    /// One `go` sample: its keys, `(signer, writable)` per key, and whether it is a CPI.
    fn flagged(keys: &[&str], flags: &[(bool, bool)], cpi: bool) -> Samples {
        let mut samples = instructions(&[(&[1], keys)]);
        samples.instructions[0].flags = Some(KeyFlags {
            signer: flags.iter().map(|f| f.0).collect(),
            writable: flags.iter().map(|f| f.1).collect(),
            cpi,
        });
        samples
    }

    fn flag_findings(report: &Report) -> Vec<String> {
        report
            .findings
            .iter()
            .filter(|f| f.check == Check::AccountFlags)
            .map(|f| f.message.clone())
            .collect()
    }

    /// The program takes `vault` (writable), then `authority` (signer).
    const SWAPPED: &str =
        r#"[{"name":"authority","signer":true},{"name":"vault","writable":true}]"#;
    const VAULT_THEN_AUTH: [(bool, bool); 2] = [(false, true), (true, false)];

    #[test]
    fn swapped_accounts_lack_the_privileges_the_idl_gives_them() {
        let samples = flagged(&["Vault", "Auth"], &VAULT_THEN_AUTH, false);
        let found = flag_findings(&verify(&go(SWAPPED), &samples, None));
        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(
            found[0].contains("#0 `authority` is a signer"),
            "{found:#?}"
        );
        assert!(found[1].contains("#1 `vault` is writable"), "{found:#?}");
        // A CPI can sign for a PDA, so only the writable check holds there.
        let samples = flagged(&["Vault", "Auth"], &VAULT_THEN_AUTH, true);
        let found = flag_findings(&verify(&go(SWAPPED), &samples, None));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].contains("`vault` is writable"), "{found:#?}");
    }

    #[test]
    fn a_dropped_account_shifts_a_read_only_key_into_a_writable_slot() {
        // The program takes `authority`, `mint`, `vault`; the IDL lost `mint`.
        let idl = go(r#"[{"name":"authority","signer":true},{"name":"vault","writable":true}]"#);
        let samples = flagged(
            &["Auth", "Mint", "Vault"],
            &[(true, false), (false, false), (false, true)],
            false,
        );
        let found = flag_findings(&verify(&idl, &samples, None));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].contains("#1 `vault` is writable in the IDL, but the key passed there is read-only in 1 of 1 sample (such as Mint)"),
            "{found:#?}"
        );
    }

    #[test]
    fn extra_privileges_absent_optionals_and_either_signers_are_not_errors() {
        let mut idl = go(
            r#"[{"name":"authority","signer":true},{"name":"vault","writable":true},
            {"name":"mint"},{"name":"payer","signer":true},{"name":"rent","optional":true,"writable":true}]"#,
        );
        // Codama `isSigner: "either"`.
        if let AccountItem::Single(payer) = &mut idl.instructions[0].accounts[3] {
            payer.may_sign = true;
        }
        let samples = flagged(
            &["Auth", "Vault", "Mint", "Payer", "P"],
            &[
                (true, true),
                (false, true),
                (false, true),
                (false, false),
                (false, false),
            ],
            false,
        );
        let report = verify(&idl, &samples, None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    #[test]
    fn samples_without_flags_load_and_skip_the_check() {
        let samples = Samples::from_json(
            r#"{"instructions":[{"signature":"S","data":"AQ==","accounts":["Vault","Auth"]}]}"#,
        )
        .unwrap();
        assert!(samples.instructions[0].flags.is_none());
        let report = verify(&go(SWAPPED), &samples, None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    const VOLTR_IDL: &str = include_str!("../../../fixtures/live/voltr_vault/idl.json");
    /// `InitProtocolEvent` in the voltr IDL.
    const INIT_PROTOCOL: [u8; 8] = [13, 81, 183, 132, 88, 43, 202, 213];

    fn voltr(edit: impl FnOnce(&mut serde_json::Value)) -> Idl {
        let mut json: serde_json::Value = serde_json::from_str(VOLTR_IDL).unwrap();
        edit(&mut json);
        Idl::from_json(&json.to_string()).unwrap()
    }

    /// `InitProtocolEvent { admin, treasury, operational_state: u16, initialized_ts: u64 }`.
    fn init_protocol() -> Vec<u8> {
        let mut out = INIT_PROTOCOL.to_vec();
        out.extend([1; 32]);
        out.extend([2; 32]);
        out.extend(1u16.to_le_bytes());
        out.extend(1_760_000_000u64.to_le_bytes());
        out
    }

    fn events(list: &[(EventSource, Vec<u8>)]) -> Samples {
        Samples {
            program_id: None,
            accounts: Vec::new(),
            instructions: Vec::new(),
            events: list
                .iter()
                .enumerate()
                .map(|(i, (source, data))| EventSample {
                    signature: format!("sig{i}"),
                    source: *source,
                    data: data.clone(),
                })
                .collect(),
        }
    }

    fn log(data: Vec<u8>) -> (EventSource, Vec<u8>) {
        (EventSource::Log, data)
    }

    fn cpi(envelope: &[u8], body: &[u8]) -> (EventSource, Vec<u8>) {
        (EventSource::Cpi, [envelope, body].concat())
    }

    /// The type of `field` in the IDL type `name`.
    fn field_type<'j>(
        json: &'j mut serde_json::Value,
        name: &str,
        field: &str,
    ) -> &'j mut serde_json::Value {
        let types = json["types"].as_array_mut().unwrap();
        let td = types.iter_mut().find(|t| t["name"] == name).unwrap();
        let fields = td["type"]["fields"].as_array_mut().unwrap();
        let f = fields.iter_mut().find(|f| f["name"] == field).unwrap();
        &mut f["type"]
    }

    #[test]
    fn events_from_logs_and_self_cpi_are_decoded_and_summarized() {
        let body = init_protocol();
        let samples = events(&[log(body.clone()), cpi(&EVENT_IX_TAG, &body)]);
        let report = verify(&voltr(|_| {}), &samples, None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.event_coverage.classified, 2);
        assert_eq!(report.event_coverage.not_sampled.len(), 25);
        let e = &report.events[0];
        assert_eq!(
            (e.name.as_str(), e.sources.log, e.sources.cpi),
            ("InitProtocolEvent", 1, 1)
        );
        assert_eq!(e.bytes_used, Some(Range { min: 82, max: 82 }));

        let mut human = Vec::new();
        report.write_human(&mut human, false).unwrap();
        let human = String::from_utf8(human).unwrap();
        assert!(
            human.contains("\nevents: 2 sampled, 2 classified\n"),
            "{human}"
        );
        assert!(
            human.contains("InitProtocolEvent        2    1    1    82  82"),
            "{human}"
        );
        let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        assert_eq!(json["event_coverage"]["classified"], 2);
        assert_eq!(json["events"][0]["sources"]["cpi"], 1);
    }

    #[test]
    fn event_values_are_compared_with_the_decoder_under_test() {
        let samples = events(&[
            log(init_protocol()),
            log(init_protocol()),
            log(init_protocol()),
        ]);
        let reply = |ts: u64| {
            Outcome::Ok(serde_json::json!({ "name": "InitProtocolEvent", "data": {
                "admin": bs58::encode([1u8; 32]).into_string(),
                "treasury": bs58::encode([2u8; 32]).into_string(),
                "operational_state": 1, "initialized_ts": ts }}))
        };
        let reference = Reference::from_decoder(
            Vec::new(),
            Vec::new(),
            vec![
                reply(1_760_000_000),
                reply(7),
                Outcome::Skipped("built without program-events".into()),
            ],
        );
        let report = verify(&voltr(|_| {}), &samples, Some(&reference));
        let found: Vec<(Severity, &str, Option<&str>)> = report
            .findings
            .iter()
            .map(|f| (f.severity, f.message.as_str(), f.example.as_deref()))
            .collect();
        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(
            found.iter().any(|(s, m, e)| *s == Severity::Error
                && m.contains("values differ")
                && m.contains("`initialized_ts`")
                && *e == Some("sig1")),
            "{found:#?}"
        );
        assert!(found.contains(&(
            Severity::Note,
            "not compared: the decoder under test skipped 1 of 3 samples: built without program-events",
            None
        )));
        let r = report.reference.unwrap();
        assert_eq!((r.events_compared, r.events_matched, r.skipped), (2, 1, 1));
    }

    #[test]
    fn non_zero_bytes_after_an_event_payload_are_an_error() {
        // The program writes a trailing u64 the IDL does not declare; it is zero in sig0.
        let with = |v: u64| [init_protocol(), v.to_le_bytes().to_vec()].concat();
        let samples = events(&[log(with(0)), log(with(5000))]);
        let report = verify(&voltr(|_| {}), &samples, None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Error, Check::BytesUsed));
        assert_eq!(f.message, "8 bytes after the payload in 2 of 2 samples");
        assert_eq!(f.example.as_deref(), Some("sig1"));
    }

    #[test]
    fn narrowed_event_field_with_zero_high_bytes_is_a_note() {
        // A u32 timestamp leaves the u64's zero high bytes after the payload.
        let idl = voltr(|j| {
            *field_type(j, "InitProtocolEvent", "initialized_ts") = "u32".into();
        });
        let report = verify(&idl, &events(&[log(init_protocol())]), None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Note, Check::BytesUsed));
        assert_eq!(
            f.message,
            "4 bytes of zeros after the payload in 1 of 1 sample"
        );
    }

    #[test]
    fn unknown_event_discriminator_is_an_error() {
        let mut body = init_protocol();
        body[..8].copy_from_slice(&[9; 8]);
        let report = verify(&voltr(|_| {}), &events(&[log(body)]), None);
        let f = only(&report);
        assert_eq!(
            (f.severity, f.check),
            (Severity::Error, Check::Discriminator)
        );
        assert_eq!(
            f.message,
            "0x0909090909090909 matches no event discriminator in the IDL, in 1 sample"
        );
        assert_eq!(report.event_coverage.classified, 0);
    }

    #[test]
    fn event_decode_failure_names_the_field_and_offset() {
        let mut body = init_protocol();
        body.truncate(78);
        let report = verify(&voltr(|_| {}), &events(&[log(body)]), None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Error, Check::Decode));
        assert_eq!(
            f.message,
            "fails to decode in 1 of 1 sample: `initialized_ts` at offset 74: needs 8 bytes, 4 left"
        );
    }

    #[test]
    fn event_without_a_payload_type_is_an_error() {
        let idl = voltr(|j| {
            let types = j["types"].as_array_mut().unwrap();
            types.retain(|t| t["name"] != "InitProtocolEvent");
        });
        let report = verify(&idl, &events(&[log(init_protocol())]), None);
        let f = only(&report);
        assert_eq!(
            (f.subject.as_str(), f.check),
            ("event InitProtocolEvent", Check::MissingType)
        );
    }

    #[test]
    fn cpi_event_must_carry_the_expected_envelope() {
        // Anchor IDLs cannot describe an envelope, so Anchor's tag is expected.
        let wrong_tag = cpi(&[1, 2, 3, 4, 5, 6, 7, 8], &init_protocol());
        let report = verify(&voltr(|_| {}), &events(&[wrong_tag]), None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Error, Check::Envelope));
        assert_eq!(
            f.message,
            "self-CPI data 0x0102030405060708 lacks Anchor's event tag 0xe445a52e51cb9a1d, in 1 sample"
        );
        // An envelope in the IDL replaces Anchor's tag.
        let idl = voltr(|j| {
            for ev in j["events"].as_array_mut().unwrap() {
                ev["envelope_discriminator"] = serde_json::json!([9]);
            }
        });
        let report = verify(&idl, &events(&[cpi(&[9], &init_protocol())]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    #[test]
    fn events_the_idl_cannot_classify_are_one_error() {
        let three = events(&[log(vec![1]), log(vec![2]), log(vec![3])]);
        let report = verify(&idl("[]", "[]", "[]"), &three, None);
        assert_eq!(
            only(&report).message,
            "the IDL declares no events, but the samples hold 3 events"
        );
        // Events without discriminators cannot be told apart either.
        let idl = voltr(|j| {
            for ev in j["events"].as_array_mut().unwrap() {
                ev["discriminator"] = serde_json::json!([]);
            }
        });
        let report = verify(&idl, &three, None);
        assert!(only(&report)
            .message
            .starts_with("the IDL gives no event discriminators"));
    }
}
