//! New fields that take the bytes of a reserved byte array, so that no other field moves and
//! the struct keeps its size. A parser then reads the old bytes as the new fields.

use std::collections::BTreeSet;
use std::ops::Range;

use super::text::field_str;
use super::types::{with_old_generic_names, Defs};
use super::{names, ChangeKind, Report, Severity};
use crate::layout::TypeIndex;
use crate::model::{ArrayLen, Field, Idl, Type, TypeDef, TypeDefTy};
use crate::reach::reachable;

/// Names of a reserved array after leading `_` and a trailing number go, in ASCII lowercase.
/// `padding_before_x` and `padding_end`, which idl-drift writes for repr(C) gaps, do not match.
const RESERVED_NAMES: [&str; 5] = ["reserved", "padding", "pad", "unused", "spare"];

/// New fields that take bytes of a reserved `[u8; N]` field, as [`reserved_uses`] finds them.
///
/// [`reserved_uses`]: super::reserved_uses
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ReservedUse {
    /// The struct type.
    pub ty: String,
    /// The position of the reserved field in the old struct, where its replacement starts in
    /// the new one. The first field there can be a new reserved array.
    pub index: usize,
    /// The reserved field of the old struct.
    pub reserved: Field,
    /// The new fields, in order. A new reserved array that keeps the other bytes is not one.
    pub taken: Vec<TakenBytes>,
}

/// A new field and the bytes of the old reserved array that it reads.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TakenBytes {
    pub field: Field,
    /// Offsets into the reserved array.
    pub bytes: Range<usize>,
}

/// The sizes and layouts that a reserved use must keep, for one pair of IDLs.
pub(super) struct Readings<'a> {
    old: TypeIndex<'a>,
    new: TypeIndex<'a>,
    /// Types that a zero-copy account holds in either IDL. Their repr(C) layout must also stay.
    zero_copy: BTreeSet<&'a str>,
}

impl<'a> Readings<'a> {
    pub(super) fn new(old: &'a Idl, new: &'a Idl) -> Self {
        let (old_index, new_index) = (TypeIndex::new(old), TypeIndex::new(new));
        let mut zero_copy = zero_copy_held(old, &old_index);
        zero_copy.extend(zero_copy_held(new, &new_index));
        Self {
            old: old_index,
            new: new_index,
            zero_copy,
        }
    }
}

fn zero_copy_held<'a>(idl: &'a Idl, index: &TypeIndex<'a>) -> BTreeSet<&'a str> {
    let roots = idl
        .accounts
        .iter()
        .filter(|a| index.get(&a.name).is_some_and(TypeDef::is_zero_copy))
        .map(|a| a.name.as_str())
        .collect();
    reachable(index, roots)
}

/// Each struct of both IDLs where new fields take reserved bytes.
pub(super) fn all(old: &Idl, new: &Idl) -> Vec<ReservedUse> {
    let (om, nm) = (old.type_map(), new.type_map());
    let defs = Defs::new(&om, &nm);
    let readings = Readings::new(old, new);
    let mut uses = Vec::new();
    for (name, nt) in &nm {
        let Some(ot) = om.get(name) else { continue };
        let renamed = with_old_generic_names(ot, nt);
        if let (TypeDefTy::Struct { fields: of }, TypeDefTy::Struct { fields: nf }) =
            (&ot.ty, renamed.as_ref().unwrap_or(&nt.ty))
        {
            uses.extend(find(name, of, nf, &defs, &readings));
        }
    }
    uses
}

/// The reserved use in struct `ty`, when it is the only change: every other field keeps its
/// name and bytes, the new fields fill the reserved bytes, and no zero-copy offset moves.
pub(super) fn find(
    ty: &str,
    of: &[Field],
    nf: &[Field],
    defs: &Defs,
    readings: &Readings,
) -> Option<ReservedUse> {
    let width = (nf.len() + 1).checked_sub(of.len())?;
    if !of.iter().any(|f| reserved_len(f).is_some()) {
        return None;
    }
    // The fields before and after the reserved one must not change. Count the unchanged
    // fields from each end once, so that a struct with many reserved arrays stays linear.
    let same = |(o, n): &(&Field, &Field)| defs.edited(o, n).is_none();
    let prefix = of.iter().zip(nf).take_while(same).count();
    if prefix == of.len() && of.len() == nf.len() {
        return None;
    }
    let suffix = of
        .iter()
        .rev()
        .zip(nf.iter().rev())
        .take_while(same)
        .count();
    let old_names = names(of);
    of.iter().enumerate().find_map(|(at, reserved)| {
        let len = reserved_len(reserved).filter(|n| *n > 0)?;
        if at > prefix || of.len() - at - 1 > suffix {
            return None;
        }
        let window = nf.get(at..at + width)?;
        // Borsh offsets of the window fields, from the start of the reserved array.
        let mut starts = Vec::with_capacity(window.len());
        let mut end = 0usize;
        let mut taken = Vec::new();
        for field in window {
            let size = readings.new.borsh_size(&field.ty).ok()??;
            let start = end;
            end = end.checked_add(size).filter(|end| *end <= len)?;
            starts.push(start);
            if reserved_len(field).is_none() {
                // A name of another old field means that field moved here.
                if old_names.contains(field.name.as_str()) {
                    return None;
                }
                taken.push(TakenBytes {
                    field: field.clone(),
                    bytes: start..end,
                });
            }
        }
        if end != len || taken.is_empty() {
            return None;
        }
        if readings.zero_copy.contains(ty) && !keeps_zero_copy_layout(ty, at, &starts, readings) {
            return None;
        }
        Some(ReservedUse {
            ty: ty.to_string(),
            index: at,
            reserved: reserved.clone(),
            taken,
        })
    })
}

/// `true` when the repr(C) layout keeps its size, its alignment and each other offset, and
/// each window field starts at its Borsh offset, so that no padding opens in the reserved bytes.
fn keeps_zero_copy_layout(ty: &str, at: usize, starts: &[usize], readings: &Readings) -> bool {
    let layouts = readings
        .old
        .get(ty)
        .zip(readings.new.get(ty))
        .map(|(ot, nt)| {
            (
                readings.old.struct_layout(ot),
                readings.new.struct_layout(nt),
            )
        });
    let Some((Ok(ol), Ok(nl))) = layouts else {
        return false;
    };
    let width = starts.len();
    let offset = |fields: &[crate::layout::FieldLayout], i: usize| fields.get(i).map(|f| f.offset);
    let Some(base) = offset(&ol.fields, at) else {
        return false;
    };
    let kept = (0..ol.fields.len()).filter(|i| *i != at).all(|i| {
        let new_at = if i < at { i } else { i + width - 1 };
        offset(&ol.fields, i) == offset(&nl.fields, new_at)
    });
    let packed = starts
        .iter()
        .enumerate()
        .all(|(k, start)| offset(&nl.fields, at + k) == base.checked_add(*start));
    (ol.size, ol.align) == (nl.size, nl.align) && kept && packed
}

/// N for a `[u8; N]` field with a reserved name.
fn reserved_len(field: &Field) -> Option<usize> {
    match &field.ty {
        Type::Array(item, ArrayLen::Value(n))
            if **item == Type::U8 && is_reserved_name(&field.name) =>
        {
            Some(*n)
        }
        _ => None,
    }
}

fn is_reserved_name(name: &str) -> bool {
    let base = name
        .trim_start_matches('_')
        .trim_end_matches(|c: char| c.is_ascii_digit());
    let base = base.strip_suffix('_').unwrap_or(base);
    RESERVED_NAMES.iter().any(|r| base.eq_ignore_ascii_case(r))
}

/// One change for the use, in place of the field changes that the same bytes would give.
pub(super) fn report(u: &ReservedUse, reached: bool, r: &mut Report) {
    let subject = format!("{}#{}", u.ty, u.index);
    if reached {
        let taken: Vec<String> = u
            .taken
            .iter()
            .map(|t| {
                format!(
                    "{} takes bytes {}..{}",
                    field_str(&t.field),
                    t.bytes.start,
                    t.bytes.end
                )
            })
            .collect();
        let message = format!(
            "reserved bytes used ({subject}) via reachable type: {} of {}: safe only if these bytes are zero in existing data",
            taken.join(", "),
            field_str(&u.reserved)
        );
        r.push(
            Severity::Dangerous,
            ChangeKind::ReservedBytesUsed,
            subject,
            message,
        );
    } else {
        let message = format!("reserved bytes used in UNREACHED type ({subject}): dead code");
        r.push(
            Severity::Cosmetic,
            ChangeKind::ReservedBytesUsed,
            subject,
            message,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::is_reserved_name;

    #[test]
    fn reserved_names_allow_leading_underscores_and_a_trailing_number() {
        for name in [
            "reserved",
            "_reserved",
            "padding_0",
            "_padding1",
            "padding0",
            "Pad",
            "unused2",
            "__spare",
        ] {
            assert!(is_reserved_name(name), "{name}");
        }
        for name in [
            "reserves",
            "reserve",
            "reserved_fee",
            "padding_before_c",
            "padding_end",
            "data",
            "_",
            "1",
        ] {
            assert!(!is_reserved_name(name), "{name}");
        }
    }
}
