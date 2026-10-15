//! Compare decoded values with the reference decoder, the decoder under test or the old IDL.

use super::{Outcomes, Verifier};
use crate::decode::Value;
use crate::reference::{first_difference, normalize, to_json, Outcome};
use crate::report::{Check, Severity};
use crate::text::of_samples;

impl Verifier<'_> {
    /// Compare decoded values with reference outcomes; returns (compared, matched).
    pub(super) fn compare(
        &mut self,
        subject: &str,
        entries: &[(usize, Option<&Outcome>, &Value)],
        id: impl Fn(usize) -> String,
    ) -> (usize, usize) {
        let Some(reference) = self.reference else {
            return (0, 0);
        };
        let label = reference.label;
        let failure_severity = if reference.failures_are_errors {
            Severity::Error
        } else {
            Severity::Warning
        };
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

    /// Compare `{name, data}` replies: the name first, then the data. Returns
    /// (compared, matched).
    pub(super) fn compare_named(
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
}
