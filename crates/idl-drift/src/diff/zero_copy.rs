//! `serialization` and `repr` changes, judged by the zero-copy accounts they affect.

use std::collections::{BTreeMap, BTreeSet};

use super::{ChangeKind, Report, Severity};
use crate::layout::{self, LayoutError, Reading, TypeIndex};
use crate::model::{Idl, Type, TypeDef};
use crate::reach::reachable;

/// A zero-copy account, read once with every old `serialization` and `repr`, and once with
/// every new one. One reading per side, because changes on nested types can cancel out.
pub(super) struct Root {
    name: String,
    before: Reading,
    after: Reading,
    /// The account type and every type it holds, at any depth.
    holds: BTreeSet<String>,
    effect: (LayoutEffect, String),
}

/// Only accounts are cast from memory, as in Shipstern's converter: instruction args and
/// events are always Borsh. The new attributes go on the old types, so field changes,
/// which are reported on their own, do not count twice.
pub(super) fn roots(old: &Idl, new_types: &BTreeMap<String, &TypeDef>) -> Vec<Root> {
    let mut hybrid = old.clone();
    for td in &mut hybrid.types {
        if let Some(nt) = new_types.get(&td.name) {
            td.serialization.clone_from(&nt.serialization);
            td.repr = nt.repr;
        }
    }
    let (oi, hi) = (TypeIndex::new(old), TypeIndex::new(&hybrid));
    old.accounts
        .iter()
        .filter_map(|account| {
            let (before, after) = (oi.get(&account.name)?, hi.get(&account.name)?);
            (before.is_zero_copy() || after.is_zero_copy()).then(|| Root {
                name: account.name.clone(),
                before: reading(before),
                after: reading(after),
                holds: reachable(&oi, vec![account.name.as_str()])
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                effect: layout_effect(&oi, before, &hi, after),
            })
        })
        .collect()
}

fn reading(td: &TypeDef) -> Reading {
    if td.is_zero_copy() {
        Reading::ZeroCopy
    } else {
        Reading::Borsh
    }
}

/// `serialization` and `repr` only matter where a type is cast as zero-copy memory. Judge
/// the change by the zero-copy accounts that hold this type and whose reading it is part of.
pub(super) fn diff_layout_attrs(
    name: &str,
    ot: &TypeDef,
    nt: &TypeDef,
    reached: &BTreeSet<String>,
    roots: &[Root],
    r: &mut Report,
) {
    let repr = |td: &TypeDef| {
        td.repr
            .map_or_else(|| "none".to_string(), |r| r.to_string())
    };
    let what = match (ot.serialization != nt.serialization, ot.repr != nt.repr) {
        (true, false) => format!(
            "serialization changed ({name}): {} -> {}",
            ot.serialization, nt.serialization
        ),
        (false, true) => format!("repr changed ({name}): {} -> {}", repr(ot), repr(nt)),
        _ => format!(
            "serialization and repr changed ({name}): {} {} -> {} {}",
            ot.serialization,
            repr(ot),
            nt.serialization,
            repr(nt)
        ),
    };
    if !reached.contains(name) {
        r.push(
            Severity::Cosmetic,
            ChangeKind::LayoutChanged,
            name,
            format!("{what} in an UNREACHED type"),
        );
        return;
    }
    let holders: Vec<&Root> = roots
        .iter()
        .filter(|root| root.holds.contains(name))
        .collect();
    // A zero-copy reading uses the account's own serialization and the repr of every type
    // inside it. A nested type's serialization has no effect on it.
    let repr_changed = layout::layout_repr(ot) != layout::layout_repr(nt);
    let worst = holders
        .iter()
        .filter(|root| {
            (root.name == name && root.before != root.after)
                || (root.after == Reading::ZeroCopy && repr_changed)
        })
        .map(|root| &root.effect)
        .max_by_key(|effect| effect.0);
    let (severity, detail) = match worst {
        None if holders.is_empty() => (
            Severity::Cosmetic,
            "only read as Borsh, which ignores both".to_string(),
        ),
        None => (
            Severity::Cosmetic,
            "no zero-copy reading depends on this change, so the bytes are the same".to_string(),
        ),
        Some((LayoutEffect::None, _)) => (
            Severity::Cosmetic,
            "no field of a zero-copy type moves, so the bytes are the same".to_string(),
        ),
        Some((LayoutEffect::Size, detail)) => (Severity::Dangerous, detail.clone()),
        Some((LayoutEffect::Moves, detail)) => (Severity::Breaking, detail.clone()),
    };
    r.push(
        severity,
        ChangeKind::LayoutChanged,
        name,
        format!("{what}: {detail}"),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LayoutEffect {
    None,
    Size,
    Moves,
}

/// Compare how a parser reads `before` and `after`; the string explains a non-`None` effect.
/// The two must have the same fields and differ only in `serialization` and `repr`.
fn layout_effect(
    bi: &TypeIndex<'_>,
    before: &TypeDef,
    ai: &TypeIndex<'_>,
    after: &TypeDef,
) -> (LayoutEffect, String) {
    let ty = Type::Defined {
        name: before.name.clone(),
        generics: Vec::new(),
    };
    let read =
        |index: &TypeIndex<'_>, td: &TypeDef| -> Result<(Vec<layout::Leaf>, usize), LayoutError> {
            Ok((
                index.leaves(td, reading(td))?,
                index.reading_size(&ty, reading(td))?,
            ))
        };
    let type_name = &before.name;
    match (read(bi, before), read(ai, after)) {
        (Ok((bl, bs)), Ok((al, as_))) => {
            // Same fields, so the leaves pair up by position. Names can repeat, such as
            // `padding_0` and `_padding_0`, which are both `padding0` in camelCase.
            let moved = bl.iter().zip(&al).find(|(b, a)| a.offset != b.offset);
            if let Some((b, a)) = moved {
                return (
                    LayoutEffect::Moves,
                    format!(
                        "moves `{type_name}.{}` from offset {} to {} (zero-copy layout)",
                        b.path, b.offset, a.offset
                    ),
                );
            }
            if bs == as_ {
                (LayoutEffect::None, String::new())
            } else {
                (
                    LayoutEffect::Size,
                    format!("no field moves, but {type_name} is {as_} bytes, was {bs}"),
                )
            }
        }
        // Without a layout, a field can move unseen, even when both sides fail the same way.
        (Err(e), _) | (_, Err(e)) => (
            LayoutEffect::Moves,
            format!(
                "the zero-copy layout of {type_name} can't be computed, so a field may move: {e}"
            ),
        ),
    }
}
