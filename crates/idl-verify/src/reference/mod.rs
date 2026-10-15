//! Compare decoded values with an independent decoder's output.
//!
//! The reference file is what `tools/reference/reference.mjs` prints: one entry per sample,
//! in sample order, holding `@coral-xyz/anchor`'s decoded value in a plain JSON form
//! (`u64` and wider as `{"bn": "<decimal>"}`, pubkeys as base58, bytes as number arrays).

mod diff;
mod json;

use idl_drift::model::Idl;
use serde::Deserialize;
use serde_json::{json, Value as Json};

use crate::classify::{Classifier, Disc};
use crate::decode::Decoder;
use crate::error::Error;
use crate::layout::TypeIndex;
use crate::samples::{EventSample, Samples};
use crate::text::prefix_hex;

pub(crate) use diff::{first_difference, normalize};
pub(crate) use json::to_json;

/// Reference decodes, aligned with the samples by index.
#[derive(Debug, Clone)]
pub struct Reference {
    pub(crate) accounts: Vec<Outcome>,
    pub(crate) instructions: Vec<Outcome>,
    /// Empty when the other side does not decode events.
    pub(crate) events: Vec<Outcome>,
    /// `@coral-xyz/anchor` reads zero-copy types as Borsh, so padded types are skipped.
    /// A generated parser under test reads the padding, so it is compared.
    pub(crate) honors_padding: bool,
    /// Names the other side in findings: the reference decoder, the decoder under test or
    /// the old IDL.
    pub(crate) label: &'static str,
    /// A failure of the user's own parser or old IDL is an error, not a warning.
    pub(crate) failures_are_errors: bool,
}

/// One reference decode.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    Ok(Json),
    Failed(String),
    /// The decoder does not decode this kind of sample, such as a parser built without events.
    Skipped(String),
}

#[derive(Deserialize)]
struct ReferenceJson {
    #[serde(default)]
    accounts: Vec<EntryJson>,
    #[serde(default)]
    instructions: Vec<EntryJson>,
}

#[derive(Deserialize)]
struct EntryJson {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    signature: Option<String>,
    reference: OutcomeJson,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum OutcomeJson {
    Ok { value: Json },
    Error { error: String },
}

impl Reference {
    /// Parse a reference file and check it lines up with `samples`.
    ///
    /// # Errors
    ///
    /// When the file is not reference JSON, or its entries don't match the samples one to one.
    pub fn from_json(src: &str, samples: &Samples) -> Result<Self, Error> {
        let raw: ReferenceJson = serde_json::from_str(src).map_err(Error::ReferenceJson)?;
        check_aligned(
            "accounts",
            raw.accounts.iter().map(|e| e.address.as_deref()),
            samples.accounts.iter().map(|a| a.address.as_str()),
        )?;
        check_aligned(
            "instructions",
            raw.instructions.iter().map(|e| e.signature.as_deref()),
            samples.instructions.iter().map(|i| i.signature.as_str()),
        )?;
        let outcomes = |entries: Vec<EntryJson>| {
            entries
                .into_iter()
                .map(|e| match e.reference {
                    OutcomeJson::Ok { value } => Outcome::Ok(value),
                    OutcomeJson::Error { error } => Outcome::Failed(error),
                })
                .collect()
        };
        Ok(Self {
            accounts: outcomes(raw.accounts),
            instructions: outcomes(raw.instructions),
            events: Vec::new(),
            honors_padding: false,
            label: "the reference decoder",
            failures_are_errors: false,
        })
    }

    /// Outcomes from a decoder under test, already aligned with the samples.
    pub(crate) fn from_decoder(
        accounts: Vec<Outcome>,
        instructions: Vec<Outcome>,
        events: Vec<Outcome>,
    ) -> Self {
        Self {
            accounts,
            instructions,
            events,
            honors_padding: true,
            label: "the decoder under test",
            failures_are_errors: true,
        }
    }

    /// Outcomes from decoding `samples` with `old`, a second IDL of the program `new`
    /// describes. A sample that `old` classifies by another discriminator fails, naming both.
    #[must_use]
    pub fn from_idl(old: &Idl, new: &Idl, samples: &Samples) -> Self {
        let (ours, theirs) = (Classifier::new(new), Classifier::new(old));
        let decoder = Decoder::new(TypeIndex::new(old));
        let accounts = samples
            .accounts
            .iter()
            .map(|sample| {
                let Some(t) = theirs.account(&sample.data) else {
                    return Outcome::Failed(
                        "no account discriminator in the old IDL matches".into(),
                    );
                };
                let nd = &old.accounts[t];
                let disc = (nd.discriminator_offset, nd.discriminator.as_slice());
                if let Some(o) = ours.account(&sample.data) {
                    let n = &new.accounts[o];
                    let new_disc = (n.discriminator_offset, n.discriminator.as_slice());
                    if new_disc != disc {
                        return Outcome::Failed(other_disc("account", &nd.name, disc, new_disc));
                    }
                }
                let Some(td) = decoder.types().get(&nd.name) else {
                    return Outcome::Failed(format!("the old IDL has no type `{}`", nd.name));
                };
                let start = nd.payload_start();
                let body = sample.data.get(start..).unwrap_or_default();
                match decoder.type_def(td, body) {
                    Ok(d) => Outcome::Ok(to_json(&d.value)),
                    Err(e) => Outcome::Failed(e.offset_by(start).to_string()),
                }
            })
            .collect();
        let instructions = samples
            .instructions
            .iter()
            .map(|sample| {
                let Some(t) = theirs.instruction(sample) else {
                    return Outcome::Failed(
                        "no instruction discriminator in the old IDL matches".into(),
                    );
                };
                let ix = &old.instructions[t];
                let disc = (ix.discriminator_offset, ix.discriminator.as_slice());
                if let Some(o) = ours.instruction(sample) {
                    let n = &new.instructions[o];
                    let new_disc = (n.discriminator_offset, n.discriminator.as_slice());
                    if new_disc != disc {
                        return Outcome::Failed(other_disc(
                            "instruction",
                            &ix.name,
                            disc,
                            new_disc,
                        ));
                    }
                }
                let start = disc.0.saturating_add(disc.1.len());
                let body = sample.data.get(start..).unwrap_or_default();
                match decoder.borsh_fields(&ix.args, body) {
                    Ok(d) => Outcome::Ok(json!({ "name": ix.name, "data": to_json(&d.value) })),
                    Err(e) => Outcome::Failed(e.offset_by(start).to_string()),
                }
            })
            .collect();
        let events = samples
            .events
            .iter()
            .map(|sample| old_event(sample, &ours, &theirs, &decoder))
            .collect();
        Self {
            label: "the old IDL",
            ..Self::from_decoder(accounts, instructions, events)
        }
    }
}

/// `sample` decoded with the old IDL, which `theirs` classifies by; `ours` holds the new IDL.
fn old_event(
    sample: &EventSample,
    ours: &Classifier<'_>,
    theirs: &Classifier<'_>,
    decoder: &Decoder<'_>,
) -> Outcome {
    let (old, new) = (theirs.idl, ours.idl);
    if old.events.is_empty() {
        return Outcome::Failed("the old IDL declares no events".into());
    }
    // Each IDL removes its own envelope: the two can wrap self-CPI events apart.
    let Some(body) = theirs.event_body(sample) else {
        let list: Vec<String> = theirs.envelopes.iter().map(ToString::to_string).collect();
        return Outcome::Failed(format!(
            "self-CPI data {} lacks the old IDL's envelope {}",
            prefix_hex(&sample.data),
            list.join(" or ")
        ));
    };
    let Some(t) = theirs.event(body) else {
        return Outcome::Failed("no event discriminator in the old IDL matches".into());
    };
    let ev = &old.events[t];
    let disc = (0, ev.discriminator.as_slice());
    if let Some(o) = ours.event_body(sample).and_then(|b| ours.event(b)) {
        let new_disc = (0, new.events[o].discriminator.as_slice());
        if new_disc != disc {
            return Outcome::Failed(other_disc("event", &ev.name, disc, new_disc));
        }
    }
    let Some(td) = decoder.types().get(&ev.name) else {
        return Outcome::Failed(format!("the old IDL has no type `{}`", ev.name));
    };
    let start = ev.discriminator.len();
    let payload = body.get(start..).unwrap_or_default();
    match decoder.type_def(td, payload) {
        Ok(d) => Outcome::Ok(json!({ "name": ev.name, "data": to_json(&d.value) })),
        Err(e) => Outcome::Failed(e.offset_by(start).to_string()),
    }
}

fn other_disc(kind: &str, name: &str, old: Disc<'_>, new: Disc<'_>) -> String {
    let by = |(offset, disc): Disc<'_>| match (offset, disc.is_empty()) {
        (_, true) => "by size".to_string(),
        (0, false) => format!("by discriminator {}", prefix_hex(disc)),
        (at, false) => format!("by discriminator {} at offset {at}", prefix_hex(disc)),
    };
    format!(
        "the old IDL classifies it as {kind} `{name}` {}, the new IDL {}",
        by(old),
        by(new)
    )
}

/// Entries must match the samples one to one; an id, when given, must be the sample's.
fn check_aligned<'a>(
    list: &'static str,
    reference: impl ExactSizeIterator<Item = Option<&'a str>>,
    samples: impl ExactSizeIterator<Item = &'a str>,
) -> Result<(), Error> {
    let (r, s) = (reference.len(), samples.len());
    if r != s {
        return Err(Error::ReferenceMismatch(format!(
            "it has {r} {list}, the samples have {s}"
        )));
    }
    for (index, (id, sample)) in reference.zip(samples).enumerate() {
        if let Some(id) = id.filter(|id| *id != sample) {
            return Err(Error::ReferenceMismatch(format!(
                "{list}[{index}] is {id}, the sample is {sample}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::EVENT_IX_TAG;
    use crate::report::Severity;
    use crate::samples::{EventSource, InstructionSample};
    use crate::testing::idl;

    fn voltr(file: &str) -> String {
        let path = format!(
            "{}/../../fixtures/live/voltr_vault/{file}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn idl_that_drops_padding_reads_other_values() {
        let anchor = Idl::from_json(&voltr("idl.json")).unwrap();
        let codama = Idl::from_json(&voltr("idl.js-codama.json")).unwrap();
        let mut samples = Samples::from_json(&voltr("samples.json")).unwrap();
        let errors = |samples: &Samples| {
            let reference = Reference::from_idl(&codama, &anchor, samples);
            let report = crate::verify(&anchor, samples, Some(&reference));
            let messages: Vec<String> = report
                .findings
                .into_iter()
                .filter(|f| f.severity == Severity::Error)
                .map(|f| format!("{}: {}", f.subject, f.message))
                .collect();
            messages
        };
        // The fixtures hold zeros after the gap, so both IDLs read the same values.
        assert_eq!(errors(&samples), Vec::<String>::new());
        let receipt = [105, 99, 219, 155, 77, 241, 7, 119];
        let sample = samples
            .accounts
            .iter_mut()
            .find(|a| a.data.starts_with(&receipt))
            .unwrap();
        // Write `last_updated_epoch` at its repr(C) offset; the unpadded read starts 7 bytes early.
        sample.data[88..96].copy_from_slice(&987_654_321u64.to_le_bytes());
        assert_eq!(
            errors(&samples),
            ["account AdaptorAddReceipt: values differ from the old IDL in 1 of 5 samples, first at `_reserved[0]`: idl-verify read 0, the old IDL 104"]
        );
    }

    #[test]
    fn old_idl_with_another_discriminator_is_a_finding() {
        let new = idl(r#""instructions":[{"name":"go","discriminator":[1]}]"#);
        let old = idl(
            r#""instructions":[{"name":"go","discriminator":[2]},{"name":"stop","discriminator":[1,0]}]"#,
        );
        let samples = Samples {
            instructions: vec![InstructionSample {
                signature: "sig".into(),
                data: vec![1, 0],
                accounts: Vec::new(),
                flags: None,
            }],
            ..Samples::default()
        };
        let report = crate::verify(
            &new,
            &samples,
            Some(&Reference::from_idl(&old, &new, &samples)),
        );
        // A different discriminator under the old IDL is an error: the two IDLs route it apart.
        let found: Vec<(Severity, &str)> = report
            .findings
            .iter()
            .map(|f| (f.severity, f.message.as_str()))
            .collect();
        assert!(
            found.contains(&(Severity::Error, "the old IDL failed on 1 of 1 sample: the old IDL classifies it as instruction `stop` by discriminator 0x0100, the new IDL by discriminator 0x01")),
            "{found:#?}"
        );
    }

    #[test]
    fn old_idl_events_are_decoded_and_compared() {
        let idl = |disc: u8, fields: &str| {
            idl(&format!(
                r#""instructions":[],"events":[{{"name":"Ev","discriminator":[{disc}]}}],
                    "types":[{{"name":"Ev","type":{{"kind":"struct","fields":{fields}}}}}]"#
            ))
        };
        let a_b = r#"[{"name":"a","type":"u8"},{"name":"b","type":"u8"}]"#;
        let b_a = r#"[{"name":"b","type":"u8"},{"name":"a","type":"u8"}]"#;
        let event = |source, data: Vec<u8>| EventSample {
            signature: "sig".into(),
            source,
            data,
        };
        let samples = Samples {
            events: vec![
                event(EventSource::Log, vec![7, 1, 2]),
                event(
                    EventSource::Cpi,
                    [EVENT_IX_TAG.as_slice(), &[7, 1, 2]].concat(),
                ),
            ],
            ..Samples::default()
        };
        let new = idl(7, a_b);
        let errors = |old: &Idl| {
            let report = crate::verify(
                &new,
                &samples,
                Some(&Reference::from_idl(old, &new, &samples)),
            );
            let reference = report.reference.clone().unwrap();
            let messages: Vec<String> = report
                .findings
                .into_iter()
                .filter(|f| f.severity == Severity::Error)
                .map(|f| f.message)
                .collect();
            (reference.events_compared, messages)
        };
        // The old IDL swaps the two fields, so both samples read other values.
        assert_eq!(
            errors(&idl(7, b_a)),
            (2, vec!["values differ from the old IDL in 2 of 2 samples, first at `a`: idl-verify read 1, the old IDL 2".to_string()])
        );
        // An old IDL that cannot classify the event is an error, as for instructions.
        assert_eq!(
            errors(&idl(8, a_b)),
            (0, vec!["the old IDL failed on 2 of 2 samples: no event discriminator in the old IDL matches".to_string()])
        );
    }
}
