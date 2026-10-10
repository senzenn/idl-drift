//! Classify samples by discriminator, decode them, and run every check.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use idl_drift::model::{Account, AccountItem, Idl, Instruction, NamedDisc, Type, TypeDef};

use crate::decode::{DecodeError, Decoder, Value};
use crate::layout::{StructLayout, TypeIndex};
use crate::reference::{first_difference, normalize, to_json, Outcome, Reference};
use crate::report::{
    AccountSummary, Check, Coverage, Finding, InstructionSummary, ProgramInfo, Range,
    ReferenceSummary, Report, Severity, SizeCount,
};
use crate::samples::Samples;

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
        reference: reference.map(|_| reference_summary),
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
        let mut unknown: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, sample) in self.samples.accounts.iter().enumerate() {
            match longest_match(&table, &sample.data) {
                Some(matches) => groups[matches[0]].push(i),
                None => unknown.entry(prefix_hex(&sample.data)).or_default().push(i),
            }
        }

        let unknown_count: usize = unknown.values().map(Vec::len).sum();
        if table.is_empty() && !idl.accounts.is_empty() && unknown_count > 0 {
            self.push(
                Severity::Error,
                Check::Discriminator,
                "accounts",
                format!(
                    "the IDL gives no account discriminators, so none of the {unknown_count} sampled accounts can be classified"
                ),
                Some("add the discriminators to the IDL; idl-verify computes them only for legacy (pre-0.30) Anchor IDLs".into()),
                None,
            );
        } else {
            for (prefix, idxs) in &unknown {
                let example = self.samples.accounts[idxs[0]].address.as_str();
                self.push(
                    Severity::Error,
                    Check::Discriminator,
                    "accounts",
                    format!(
                        "{prefix} matches no account discriminator in the IDL, in {} {}",
                        idxs.len(),
                        plural(idxs.len(), "sample", "samples")
                    ),
                    Some("the program uses an account type the IDL does not declare, or these accounts belong to another program".into()),
                    Some(example),
                );
            }
        }

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
            classified: self.samples.accounts.len() - unknown_count,
            not_sampled,
        };
        (coverage, summaries)
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
                let mut hint = "the IDL may be missing fields or padding, or the program allocates more space than the type needs".to_string();
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
            self.push(
                Severity::Warning,
                Check::BytesUsed,
                subject,
                format!(
                    "non-zero bytes follow the decoded data in {}",
                    of_samples(nonzero.len(), total)
                ),
                Some("the IDL may be missing trailing fields".into()),
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
        if let Some(layout) = padded {
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
        let mut compared = 0;
        let mut mismatches = Vec::new();
        let mut failures = Vec::new();
        for &(i, outcome, value) in entries {
            match outcome {
                Some(Outcome::Ok(theirs)) => {
                    compared += 1;
                    if let Some(d) = first_difference(&to_json(value), theirs) {
                        mismatches.push((i, d));
                    }
                }
                Some(Outcome::Failed(err)) => failures.push((i, err.clone())),
                None => {}
            }
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
                    "values differ from the reference decoder in {}{at}: idl-verify read {}, the reference {}",
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
                Severity::Warning,
                Check::Reference,
                subject,
                format!(
                    "the reference decoder failed on {}: {err}",
                    of_samples(failures.len(), entries.len())
                ),
                None,
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
        self.push(
            Severity::Error,
            Check::BytesUsed,
            subject,
            format!(
                "{extra} after the arguments in {}",
                of_samples(trailing.len(), outcomes.len())
            ),
            Some(
                "the IDL may be missing arguments, or declares one narrower than the program reads"
                    .into(),
            ),
            Some(&example),
        );
    }

    fn compare_instructions(&mut self, subject: &str, ix: &Instruction, outcomes: &Outcomes) {
        let Some(reference) = self.reference else {
            return;
        };
        let mut entries = Vec::new();
        let mut wrong_name = Vec::new();
        for (i, result) in outcomes {
            let Ok((value, _)) = result else { continue };
            match reference.instructions.get(*i) {
                // The reference reports `{name, data}`; compare the name, then the data.
                Some(Outcome::Ok(theirs)) => {
                    let name = theirs
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default();
                    if normalize(name) == normalize(&ix.name) {
                        let data = theirs.get("data").cloned().unwrap_or_default();
                        entries.push((*i, Some(Outcome::Ok(data)), value));
                    } else {
                        wrong_name.push((*i, name.to_string()));
                    }
                }
                other => entries.push((*i, other.cloned(), value)),
            }
        }
        if let Some((i, name)) = wrong_name.first() {
            let example = self.samples.instructions[*i].signature.clone();
            self.push(
                Severity::Error,
                Check::Reference,
                subject,
                format!(
                    "the reference decoder names it `{name}` in {}",
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
        let (compared, matched) = self.compare(subject, &borrowed, |i| {
            self.samples.instructions[i].signature.clone()
        });
        self.reference_summary.instructions_compared += compared + wrong_name.len();
        self.reference_summary.instructions_matched += matched;
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
fn pick_by_account_count(
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
        return "no data".to_string();
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
                })
                .collect(),
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
    fn idl_without_discriminators_says_so() {
        let idl = idl(r#"[{"name":"A"}]"#, "[]", &format!("[{U32_TYPE}]"));
        let report = verify(&idl, &accounts(&[&[1, 0, 0, 0]]), None);
        let f = only(&report);
        assert_eq!(f.check, Check::Discriminator);
        assert!(f
            .message
            .starts_with("the IDL gives no account discriminators"));
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
}
