//! Event checks: envelope, classification, decoding and bytes after the payload.

use std::collections::BTreeMap;

use idl_drift::model::Event;

use super::{
    bytes_used, sample_count, size_counts, Groups, Outcomes, Verifier, LEGACY_DISCRIMINATORS,
};
use crate::classify::{ANCHOR_ENVELOPE, EVENT_IX_TAG};
use crate::report::{Check, Coverage, EventSources, EventSummary, Range, Severity};
use crate::samples::EventSource;
use crate::text::{byte_range, of_samples, plural, prefix_hex};

impl Verifier<'_> {
    pub(super) fn check_events(&mut self) -> (Coverage, Vec<EventSummary>) {
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

        let bodies: Vec<Option<&[u8]>> = samples
            .iter()
            .map(|s| self.classifier.event_body(s))
            .collect();
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); idl.events.len()];
        let mut no_envelope = Groups::new();
        let mut unknown = Groups::new();
        for (i, body) in bodies.iter().enumerate() {
            let Some(body) = body else {
                no_envelope
                    .entry(prefix_hex(&samples[i].data))
                    .or_default()
                    .push(i);
                continue;
            };
            match self.classifier.event(body) {
                Some(t) => groups[t].push(i),
                None => unknown
                    .entry(format!(
                        "{} matches no event discriminator in the IDL",
                        prefix_hex(body)
                    ))
                    .or_default()
                    .push(i),
            }
        }
        self.report_envelope_misses(&no_envelope);
        let nothing_to_match = self.classifier.events.is_empty();
        self.report_unclassified(
            "event",
            &unknown,
            nothing_to_match.then_some(LEGACY_DISCRIMINATORS),
            "the program emits an event the IDL does not declare",
            |v, i| v.samples.events[i].signature.clone(),
        );

        let mut summaries = Vec::new();
        let mut not_sampled = Vec::new();
        for (ev, idxs) in idl.events.iter().zip(&groups) {
            if idxs.is_empty() {
                not_sampled.push(ev.name.clone());
            } else {
                summaries.push(self.check_event_type(ev, idxs, &bodies));
            }
        }
        let missed = sample_count(&unknown) + sample_count(&no_envelope);
        let coverage = Coverage {
            sampled: samples.len(),
            classified: samples.len().saturating_sub(missed),
            not_sampled,
        };
        (coverage, summaries)
    }

    fn report_envelope_misses(&mut self, misses: &Groups) {
        let envelopes = &self.classifier.envelopes;
        let (expected, hint) = if envelopes.as_slice() == [ANCHOR_ENVELOPE] {
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
            sizes: size_counts(&sizes),
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
        summary.bytes_used = bytes_used(&outcomes);

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
                "{}{what} after the payload in {}",
                byte_range(extra),
                of_samples(affected.len(), outcomes.len())
            ),
            Some(hint.into()),
            Some(&example),
        );
    }
}

#[cfg(test)]
mod tests {
    use idl_drift::model::Idl;

    use crate::classify::EVENT_IX_TAG;
    use crate::reference::{Outcome, Reference};
    use crate::report::{Check, Range, Severity};
    use crate::samples::{EventSample, EventSource, Samples};
    use crate::testing::{idl, only};
    use crate::verify;

    const VOLTR_IDL: &str = include_str!("../../../../fixtures/live/voltr_vault/idl.json");
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
            events: list
                .iter()
                .enumerate()
                .map(|(i, (source, data))| EventSample {
                    signature: format!("sig{i}"),
                    source: *source,
                    data: data.clone(),
                })
                .collect(),
            ..Samples::default()
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
        let report = verify(
            &idl(r#""accounts":[],"instructions":[],"types":[]"#),
            &three,
            None,
        );
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
