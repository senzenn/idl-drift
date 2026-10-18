//! Classify samples by discriminator, decode them, and run every check.

mod accounts;
mod compare;
mod events;
mod instructions;
mod pda;
mod upgrade;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use idl_drift::model::{Format, Idl};

use crate::classify::Classifier;
use crate::decode::{DecodeError, Decoder, Value};
use crate::layout::TypeIndex;
use crate::reference::Reference;
use crate::report::{
    AccountSummary, Check, Coverage, EventSummary, Finding, InstructionSummary, ProgramInfo, Range,
    ReferenceSummary, Report, Severity, SizeCount,
};
use crate::samples::Samples;
use crate::text::{bytes, of_samples, plural, prefix_hex};

pub use upgrade::{offline_rent_minimum, verify_upgrade, OFFLINE_LAMPORTS_PER_BYTE};

/// Check `samples` against `idl`, optionally comparing values with a reference decoder.
#[must_use]
pub fn verify(idl: &Idl, samples: &Samples, reference: Option<&Reference>) -> Report {
    let mut v = Verifier::new(idl, samples, reference);
    v.check_program_id();
    let accounts = v.check_accounts();
    let instructions = v.check_instructions();
    let events = v.check_events();
    v.into_report(accounts, instructions, events)
}

/// The hint when an IDL has no instruction or event discriminators to match.
const LEGACY_DISCRIMINATORS: &str =
    "add the discriminators to the IDL; idl-verify computes them only for legacy (pre-0.30) Anchor IDLs";

struct Verifier<'a> {
    idl: &'a Idl,
    samples: &'a Samples,
    reference: Option<&'a Reference>,
    classifier: Classifier<'a>,
    decoder: Decoder<'a>,
    findings: Vec<Finding>,
    reference_summary: ReferenceSummary,
    /// The class of each account sample, only with `--upgrade-impact`.
    upgrade: Option<Vec<upgrade::TypeClasses>>,
}

/// Each sample's decode: the value and the bytes it used (discriminator included), or the error.
type Outcomes = Vec<(usize, Result<(Value, usize), DecodeError>)>;

/// Sample indices grouped by a text key, such as the head of the error message they share.
type Groups = BTreeMap<String, Vec<usize>>;

impl<'a> Verifier<'a> {
    fn new(idl: &'a Idl, samples: &'a Samples, reference: Option<&'a Reference>) -> Self {
        Self {
            idl,
            samples,
            reference,
            classifier: Classifier::new(idl),
            decoder: Decoder::new(TypeIndex::new(idl)),
            findings: Vec::new(),
            reference_summary: ReferenceSummary::default(),
            upgrade: None,
        }
    }

    fn into_report(
        self,
        (account_coverage, accounts): (Coverage, Vec<AccountSummary>),
        (instruction_coverage, instructions): (Coverage, Vec<InstructionSummary>),
        (event_coverage, events): (Coverage, Vec<EventSummary>),
    ) -> Report {
        let idl = self.idl;
        Report {
            program: ProgramInfo {
                name: idl.metadata.name.clone(),
                version: idl.metadata.version.clone(),
                address: idl.address.clone(),
            },
            samples_program_id: self.samples.program_id.clone(),
            account_coverage,
            accounts,
            instruction_coverage,
            instructions,
            event_coverage,
            events,
            reference: self.reference.map(|_| self.reference_summary),
            sampling: None,
            upgrade_impact: None,
            findings: self.findings,
            unused_allows: None,
        }
    }
}

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
            allowed: false,
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

    /// Errors for samples that match no IDL `kind`, one per group in `unknown`. When the IDL
    /// has nothing to match them by, one error says so instead, with `nothing_to_match` as hint.
    fn report_unclassified(
        &mut self,
        kind: &str,
        unknown: &Groups,
        nothing_to_match: Option<&str>,
        hint: &str,
        id: impl Fn(&Self, usize) -> String,
    ) {
        let subject = format!("{kind}s");
        let count = sample_count(unknown);
        if let Some(fix) = nothing_to_match.filter(|_| count > 0) {
            self.push(
                Severity::Error,
                Check::Discriminator,
                &subject,
                format!(
                    "the IDL gives no {kind} discriminators, so none of the {count} sampled {subject} can be classified"
                ),
                Some(fix.to_string()),
                None,
            );
            return;
        }
        for (head, idxs) in unknown {
            let example = id(self, idxs[0]);
            self.push(
                Severity::Error,
                Check::Discriminator,
                &subject,
                format!(
                    "{head}, in {} {}",
                    idxs.len(),
                    plural(idxs.len(), "sample", "samples")
                ),
                Some(hint.to_string()),
                Some(&example),
            );
        }
    }

    /// Whether `data` starts with `tag` and the IDL is an Anchor IDL. Only then is the program
    /// surely built by Anchor, which adds the IDL instruction and `IdlAccount`.
    fn is_anchor_builtin(&self, data: &[u8], tag: &[u8]) -> bool {
        matches!(self.idl.format, Format::Anchor | Format::LegacyAnchor) && data.starts_with(tag)
    }

    /// One note for `idxs`, the samples of Anchor's `name` entry, which are not checked.
    fn report_anchor_builtin(
        &mut self,
        kind: &str,
        name: &str,
        tag: &[u8],
        idxs: &[usize],
        id: impl Fn(&Self, usize) -> String,
    ) {
        let Some(&first) = idxs.first() else {
            return;
        };
        let example = id(self, first);
        let n = idxs.len();
        self.push(
            Severity::Note,
            Check::Discriminator,
            &format!("{kind}s"),
            format!(
                "{n} {kind}{} Anchor's {name} ({}); not checked",
                plural(n, " is", "s are"),
                prefix_hex(tag)
            ),
            Some("each Anchor program built without the `no-idl` feature has an instruction that writes its on-chain IDL and an IdlAccount that holds it; an Anchor IDL lists neither".into()),
            Some(&example),
        );
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
}

fn sample_count(groups: &Groups) -> usize {
    groups.values().map(Vec::len).sum()
}

/// The range of bytes the decoded samples used; `None` when none decoded.
fn bytes_used(outcomes: &Outcomes) -> Option<Range> {
    Range::of(
        outcomes
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok().map(|(_, used)| *used)),
    )
}

fn size_counts(sizes: &BTreeMap<usize, usize>) -> Vec<SizeCount> {
    sizes
        .iter()
        .map(|(&size, &count)| SizeCount { size, count })
        .collect()
}

/// A hint when every decoded sample ends in the same number of unread bytes, not all zero.
/// `data` gives a sample's bytes by index.
fn missing_field_hint<'d>(outcomes: &Outcomes, data: impl Fn(usize) -> &'d [u8]) -> Option<String> {
    let tails: Vec<&[u8]> = outcomes
        .iter()
        .filter_map(|(i, r)| data(*i).get(r.as_ref().ok()?.1..))
        .collect();
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
