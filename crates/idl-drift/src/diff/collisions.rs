//! Instructions or account types that share a discriminator.
//!
//! Shipstern's `InstructionParser` tells instructions apart by account count, most first. The
//! same discriminator and count needs a hand-written resolver. Its account parser tries the
//! account types in IDL order, so of two types with one size discriminator only the first
//! decodes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::instructions::account_count;
use super::text::disc_hex;
use super::{ChangeKind, Report, Severity};
use crate::model::{Idl, Instruction, NamedDisc};

/// What a generated parser selects an instruction by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DispatchKey<'a> {
    /// The discriminator bytes.
    Bytes(&'a [u8]),
    /// The data length (Codama `sizeDiscriminatorNode`).
    Size(usize),
}

impl<'a> DispatchKey<'a> {
    /// `None` when the instruction has neither, so it dispatches some other way. Shipstern
    /// checks only the first discriminator, so a later one does not split a collision.
    pub(super) fn of(ix: &'a Instruction) -> Option<Self> {
        if ix.discriminator.is_empty() {
            ix.size_discriminator.map(Self::Size)
        } else {
            Some(Self::Bytes(&ix.discriminator))
        }
    }

    fn subject(self) -> String {
        match self {
            Self::Bytes(disc) => disc_hex(disc),
            Self::Size(size) => format!("size:{size}"),
        }
    }
}

impl fmt::Display for DispatchKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(disc) => write!(f, "discriminator {}", disc_hex(disc)),
            Self::Size(size) => write!(f, "size discriminator {size} bytes"),
        }
    }
}

/// `(dispatch key, account count)` to instruction names.
type Classes<'a> = BTreeMap<(DispatchKey<'a>, usize), BTreeSet<String>>;

pub(super) fn diff_discriminator_collisions(old: &Idl, new: &Idl, r: &mut Report) {
    let old_classes = collision_classes(&old.instructions);
    let new_classes = collision_classes(&new.instructions);

    // Same discriminator and account count: unresolvable.
    for ((key, count), names) in &new_classes {
        if names.len() < 2 {
            continue;
        }
        let name_list = names.iter().cloned().collect::<Vec<_>>().join(", ");
        let preexisting = old_classes
            .get(&(*key, *count))
            .is_some_and(|n| n.len() >= 2);
        if preexisting {
            r.push(
                Severity::Dangerous,
                ChangeKind::CollisionPreexisting,
                key.subject(),
                format!("pre-existing collision on {key} with {count} accounts: [{name_list}]"),
            );
        } else {
            r.push(
                Severity::Breaking,
                ChangeKind::Collision,
                key.subject(),
                format!("unresolvable collision on {key} with {count} accounts: [{name_list}]"),
            );
        }
    }

    // Resolvable, but the set of account counts changed, so dispatch order may differ.
    let old_counts = counts_by_key(&old_classes);
    for (key, new_set) in &counts_by_key(&new_classes) {
        let Some(old_set) = old_counts.get(key) else {
            continue;
        };
        if new_set.len() < 2 || old_set.len() < 2 || old_set == new_set {
            continue;
        }
        r.push(
            Severity::Dangerous,
            ChangeKind::DispatchOrderChanged,
            key.subject(),
            format!(
                "disambiguation order changed for {key}: account counts {{{}}} -> {{{}}}",
                counts_list(old_set),
                counts_list(new_set)
            ),
        );
    }

    diff_account_size_collisions(&old.accounts, &new.accounts, r);
}

fn collision_classes(instructions: &[Instruction]) -> Classes<'_> {
    let mut map = Classes::new();
    for ix in instructions {
        let Some(key) = DispatchKey::of(ix) else {
            continue;
        };
        map.entry((key, account_count(&ix.accounts)))
            .or_default()
            .insert(ix.name.clone());
    }
    map
}

fn counts_by_key<'a>(classes: &Classes<'a>) -> BTreeMap<DispatchKey<'a>, BTreeSet<usize>> {
    let mut map: BTreeMap<DispatchKey<'a>, BTreeSet<usize>> = BTreeMap::new();
    for (key, count) in classes.keys() {
        map.entry(*key).or_default().insert(*count);
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

/// No account count breaks the tie here: every account of that size decodes as the first type.
fn diff_account_size_collisions(old: &[NamedDisc], new: &[NamedDisc], r: &mut Report) {
    let old_sizes = names_by_size(old);
    for (size, names) in &names_by_size(new) {
        let [first, _, ..] = names.as_slice() else {
            continue;
        };
        let key = DispatchKey::Size(*size);
        let name_list = names.join(", ");
        let preexisting = old_sizes.get(size).is_some_and(|n| n.len() >= 2);
        if preexisting {
            r.push(
                Severity::Dangerous,
                ChangeKind::CollisionPreexisting,
                key.subject(),
                format!(
                    "pre-existing collision on account {key}: [{name_list}] all decode as {first}"
                ),
            );
        } else {
            r.push(
                Severity::Breaking,
                ChangeKind::Collision,
                key.subject(),
                format!(
                    "unresolvable collision on account {key}: [{name_list}] all decode as {first}"
                ),
            );
        }
    }
}

/// Account type names in IDL order, by size discriminator.
fn names_by_size(accounts: &[NamedDisc]) -> BTreeMap<usize, Vec<&str>> {
    let mut map: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for account in accounts {
        if let Some(size) = account.size_discriminator {
            map.entry(size).or_default().push(&account.name);
        }
    }
    map
}
