//! Instructions that share a discriminator.
//!
//! Shipstern's `InstructionParser` tells them apart by account count, most first. The same
//! discriminator and count needs a hand-written resolver.

use std::collections::{BTreeMap, BTreeSet};

use super::instructions::account_count;
use super::text::disc_hex;
use super::{ChangeKind, Report, Severity};
use crate::model::{Idl, Instruction};

/// `(discriminator, account count)` to instruction names.
type Classes = BTreeMap<(Vec<u8>, usize), BTreeSet<String>>;

pub(super) fn diff_discriminator_collisions(old: &Idl, new: &Idl, r: &mut Report) {
    let old_classes = collision_classes(&old.instructions);
    let new_classes = collision_classes(&new.instructions);

    // Same discriminator and account count: unresolvable.
    for ((disc, count), names) in &new_classes {
        if names.len() < 2 {
            continue;
        }
        let hex = disc_hex(disc);
        let name_list = names.iter().cloned().collect::<Vec<_>>().join(", ");
        let preexisting = old_classes
            .get(&(disc.clone(), *count))
            .is_some_and(|n| n.len() >= 2);
        if preexisting {
            r.push(
                Severity::Dangerous,
                ChangeKind::CollisionPreexisting,
                &hex,
                format!(
                    "pre-existing collision on discriminator {hex} with {count} accounts: [{name_list}]"
                ),
            );
        } else {
            r.push(
                Severity::Breaking,
                ChangeKind::Collision,
                &hex,
                format!(
                    "unresolvable collision on discriminator {hex} with {count} accounts: [{name_list}]"
                ),
            );
        }
    }

    // Resolvable, but the set of account counts changed, so dispatch order may differ.
    let old_counts = counts_by_disc(&old_classes);
    for (disc, new_set) in &counts_by_disc(&new_classes) {
        let Some(old_set) = old_counts.get(disc) else {
            continue;
        };
        if new_set.len() < 2 || old_set.len() < 2 || old_set == new_set {
            continue;
        }
        let hex = disc_hex(disc);
        r.push(
            Severity::Dangerous,
            ChangeKind::DispatchOrderChanged,
            &hex,
            format!(
                "disambiguation order changed for discriminator {hex}: account counts {{{}}} -> {{{}}}",
                counts_list(old_set),
                counts_list(new_set)
            ),
        );
    }
}

/// Instructions with an empty discriminator dispatch some other way, so they are skipped.
fn collision_classes(instructions: &[Instruction]) -> Classes {
    let mut map = Classes::new();
    for ix in instructions {
        if ix.discriminator.is_empty() {
            continue;
        }
        map.entry((ix.discriminator.clone(), account_count(&ix.accounts)))
            .or_default()
            .insert(ix.name.clone());
    }
    map
}

fn counts_by_disc(classes: &Classes) -> BTreeMap<&[u8], BTreeSet<usize>> {
    let mut map: BTreeMap<&[u8], BTreeSet<usize>> = BTreeMap::new();
    for (disc, count) in classes.keys() {
        map.entry(disc.as_slice()).or_default().insert(*count);
    }
    map
}

fn counts_list(counts: &BTreeSet<usize>) -> String {
    counts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}
