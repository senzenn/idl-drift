//! Instructions, account types or event types that share a discriminator.
//!
//! Shipstern's `InstructionParser` tells instructions apart by account count, most first. The
//! same discriminator and count needs a hand-written resolver. Its account parser tries the
//! account types in IDL order, so of two types with one discriminator only the first
//! decodes. Its event parser does not build when two events share a discriminator.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::instructions::account_count;
use super::text::disc_hex;
use super::{ChangeKind, Report, Severity};
use crate::model::{Idl, Instruction, NamedDisc};

/// What a generated parser selects an instruction by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DispatchKey<'a> {
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
pub(crate) type Classes<'a> = BTreeMap<(DispatchKey<'a>, usize), BTreeSet<String>>;

pub(super) fn diff_discriminator_collisions(old: &Idl, new: &Idl, r: &mut Report) {
    let old_classes = collision_classes(&old.instructions);
    let new_classes = collision_classes(&new.instructions);

    // Same discriminator and account count: unresolvable.
    for ((key, count), names) in &new_classes {
        if names.len() < 2 {
            continue;
        }
        let name_list = names_list(names);
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

    // Resolvable, but the account counts changed, so dispatch order may differ.
    let old_counts = counts_by_key(&old_classes);
    for (key, new_set) in &counts_by_key(&new_classes) {
        let Some(old_set) = old_counts.get(key) else {
            continue;
        };
        if new_set.len() < 2 || old_set.len() < 2 {
            continue;
        }
        let span = (*key, 0)..=(*key, usize::MAX);
        // An old parser takes the highest old count at or below the number of accounts that
        // only one instruction has. An instruction in a shared old count was already misrouted.
        let misrouted = new_classes
            .range(span.clone())
            .find_map(|((_, count), names)| {
                let routed = old_classes
                    .range((*key, 0)..=(*key, *count))
                    .rev()
                    .find(|(_, n)| n.len() == 1);
                let name = old_classes
                    .range(span.clone())
                    .filter(|(c, n)| n.len() == 1 && routed.map(|(rc, _)| rc) != Some(*c))
                    .find_map(|(_, old_names)| old_names.intersection(names).next())?;
                Some(match routed {
                    Some((_, to)) => {
                        format!("route {name} with {count} accounts to [{}]", names_list(to))
                    }
                    None => format!("cannot route {name} with {count} accounts"),
                })
            });
        let (severity, effect) = match misrouted {
            Some(misrouted) => (
                Severity::Breaking,
                format!(": parsers built from the old IDL {misrouted}"),
            ),
            None if old_set != new_set => (Severity::Dangerous, String::new()),
            None => continue,
        };
        r.push(
            severity,
            ChangeKind::DispatchOrderChanged,
            key.subject(),
            format!(
                "disambiguation order changed for {key}: account counts {{{}}} -> {{{}}}{effect}",
                counts_list(old_set),
                counts_list(new_set)
            ),
        );
    }

    diff_account_size_collisions(&old.accounts, &new.accounts, r);
    diff_type_collisions(old, new, r);
}

/// Shipstern's account parser decodes every account of one discriminator as the first type.
/// Its event parser does not build when two events share a discriminator.
fn diff_type_collisions<'a>(old: &'a Idl, new: &'a Idl, r: &mut Report) {
    let accounts = |idl: &'a Idl| {
        names_by_disc(idl.accounts.iter().map(|a| {
            (
                a.discriminator_offset,
                a.discriminator.as_slice(),
                a.name.as_str(),
            )
        }))
    };
    let events = |idl: &'a Idl| {
        names_by_disc(idl.events.iter().map(|e| {
            (
                e.discriminator_offset,
                e.discriminator.as_slice(),
                e.name.as_str(),
            )
        }))
    };
    diff_disc_collisions(
        "account",
        &accounts(old),
        &accounts(new),
        |first| format!(" all decode as {first}"),
        r,
    );
    diff_disc_collisions(
        "event",
        &events(old),
        &events(new),
        |_| ": no parser can be generated".to_owned(),
        r,
    );
}

pub(crate) fn collision_classes(instructions: &[Instruction]) -> Classes<'_> {
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

fn names_list(names: &BTreeSet<String>) -> String {
    names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
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

/// `(offset, first discriminator)` to account or event type names, in IDL order.
type NamesByDisc<'a> = BTreeMap<(usize, &'a [u8]), Vec<&'a str>>;

/// Shipstern dispatches on the first discriminator only. An empty one has no bytes to match.
fn names_by_disc<'a>(items: impl Iterator<Item = (usize, &'a [u8], &'a str)>) -> NamesByDisc<'a> {
    let mut map = NamesByDisc::new();
    for (offset, disc, name) in items {
        if !disc.is_empty() {
            map.entry((offset, disc)).or_default().push(name);
        }
    }
    map
}

/// The subject names the item kind, so an allow entry for an instruction collision on the
/// same bytes does not match.
fn diff_disc_collisions(
    what: &str,
    old: &NamesByDisc<'_>,
    new: &NamesByDisc<'_>,
    effect: impl Fn(&str) -> String,
    r: &mut Report,
) {
    for (key, names) in new {
        let [first, _, ..] = names.as_slice() else {
            continue;
        };
        let subject = format!("{what}:{}", disc_hex(key.1));
        let detail = format!(
            "{what} {}: [{}]{}",
            DispatchKey::Bytes(key.1),
            names.join(", "),
            effect(first)
        );
        if old.get(key).is_some_and(|n| n.len() >= 2) {
            r.push(
                Severity::Dangerous,
                ChangeKind::CollisionPreexisting,
                subject,
                format!("pre-existing collision on {detail}"),
            );
        } else {
            r.push(
                Severity::Breaking,
                ChangeKind::Collision,
                subject,
                format!("unresolvable collision on {detail}"),
            );
        }
    }
}
