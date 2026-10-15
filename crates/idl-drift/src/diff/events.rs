//! Events: the content discriminator, plus the self-CPI envelope. An envelope length change
//! (Anchor's 8-byte event tag to Pinocchio's 1-byte one) moves where the payload starts.

use std::collections::BTreeMap;

use super::text::disc_changed;
use super::{ChangeKind, Report, Severity};
use crate::model::Event;

pub(super) fn diff_events(old: &[Event], new: &[Event], r: &mut Report) {
    let om: BTreeMap<_, _> = old.iter().map(|e| (&e.name, e)).collect();
    let nm: BTreeMap<_, _> = new.iter().map(|e| (&e.name, e)).collect();
    for (name, ne) in &nm {
        let Some(oe) = om.get(name) else {
            r.push(
                Severity::Additive,
                ChangeKind::EventAdded,
                *name,
                format!("new event type: {name}"),
            );
            continue;
        };
        if oe.discriminator != ne.discriminator {
            r.push(
                Severity::Breaking,
                ChangeKind::EventDiscriminatorChanged,
                *name,
                disc_changed(
                    "event discriminator",
                    name,
                    &oe.discriminator,
                    &ne.discriminator,
                    "scheme swap, e.g. Anchor<->Quasar",
                ),
            );
        } else if oe.discriminator_offset != ne.discriminator_offset
            || oe.extra_discriminators != ne.extra_discriminators
        {
            r.push(
                Severity::Breaking,
                ChangeKind::EventDiscriminatorChanged,
                *name,
                format!("event discriminator offset or chain changed ({name})"),
            );
        }
        diff_envelope(name, oe, ne, r);
    }
    for name in om.keys() {
        if !nm.contains_key(name) {
            r.push(
                Severity::Breaking,
                ChangeKind::EventRemoved,
                *name,
                format!("event type removed: {name}"),
            );
        }
    }
}

fn diff_envelope(name: &str, oe: &Event, ne: &Event, r: &mut Report) {
    if oe.envelope_discriminator != ne.envelope_discriminator {
        r.push(
            Severity::Breaking,
            ChangeKind::EventEnvelopeChanged,
            name,
            disc_changed(
                "event envelope discriminator",
                name,
                &oe.envelope_discriminator,
                &ne.envelope_discriminator,
                "scheme swap, e.g. Anchor 8-byte <-> Pinocchio 1-byte",
            ),
        );
    }
    // A side with no envelope and no explicit offset does not say where the payload starts.
    if let (Some(o), Some(n)) = (oe.effective_payload_offset(), ne.effective_payload_offset()) {
        if o != n {
            r.push(
                Severity::Breaking,
                ChangeKind::EventPayloadOffsetChanged,
                name,
                format!(
                    "event envelope payload offset changed ({name}): {o}->{n} (self-CPI parse starts elsewhere)"
                ),
            );
        }
    }
}
