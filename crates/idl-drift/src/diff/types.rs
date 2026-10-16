//! Type definitions: struct fields, enum variants and aliases. A field, variant, tag or alias
//! change on a type that no parser reads is Cosmetic.

use std::collections::BTreeSet;

use super::text::{
    field_change, field_str, has_unreadable, payload_str, type_change, unreadable_change,
};
use super::{zero_copy, ChangeKind, FieldEdit, Report, Severity};
use crate::model::{EnumFields, EnumVariant, Field, Idl, Type, TypeDef, TypeDefTy};

pub(super) fn diff_types(old: &Idl, new: &Idl, reached: &BTreeSet<String>, r: &mut Report) {
    let om = old.type_map();
    let nm = new.type_map();
    let attrs_changed =
        |ot: &TypeDef, nt: &TypeDef| ot.serialization != nt.serialization || ot.repr != nt.repr;
    let roots = if nm
        .iter()
        .any(|(name, nt)| om.get(name).is_some_and(|ot| attrs_changed(ot, nt)))
    {
        zero_copy::roots(old, &nm)
    } else {
        Vec::new()
    };
    for (name, nt) in &nm {
        // An added type shows up as a change to the field or arg that uses it.
        let Some(ot) = om.get(name) else { continue };
        let is_reached = reached.contains(name);

        if attrs_changed(ot, nt) {
            zero_copy::diff_layout_attrs(name, ot, nt, reached, &roots, r);
        }

        match (&ot.ty, &nt.ty) {
            (TypeDefTy::Struct { fields: of }, TypeDefTy::Struct { fields: nf }) => {
                diff_fields(name, of, nf, is_reached, r);
            }
            (
                TypeDefTy::Enum {
                    variants: ov,
                    tag: otag,
                },
                TypeDefTy::Enum {
                    variants: nv,
                    tag: ntag,
                },
            ) => {
                if otag != ntag {
                    let u8_tag = Type::U8;
                    let tags = type_change(
                        otag.as_ref().unwrap_or(&u8_tag),
                        ntag.as_ref().unwrap_or(&u8_tag),
                        "every variant tag is read differently",
                    );
                    if is_reached {
                        r.push(
                            Severity::Breaking,
                            ChangeKind::EnumTagChanged,
                            name.as_str(),
                            format!("enum tag width changed ({name}): {tags}"),
                        );
                    } else {
                        r.push(
                            Severity::Cosmetic,
                            ChangeKind::EnumTagChanged,
                            name.as_str(),
                            format!("enum tag changed in UNREACHED type ({name})"),
                        );
                    }
                }
                diff_variants(name, ov, nv, is_reached, r);
            }
            // `Amount = u64` to `u32` changes the width everywhere `Amount` is used.
            (TypeDefTy::Type { alias: oa }, TypeDefTy::Type { alias: na }) if oa != na => {
                if is_reached {
                    r.push(
                        Severity::Breaking,
                        ChangeKind::AliasChanged,
                        name.as_str(),
                        format!(
                            "type alias target changed ({name}): {}",
                            type_change(oa, na, "Borsh layout")
                        ),
                    );
                } else {
                    r.push(
                        Severity::Cosmetic,
                        ChangeKind::AliasChanged,
                        name.as_str(),
                        format!("alias changed in UNREACHED type ({name})"),
                    );
                }
            }
            (a, b) if std::mem::discriminant(a) != std::mem::discriminant(b) => {
                r.push(
                    Severity::Breaking,
                    ChangeKind::TypeKindChanged,
                    name.as_str(),
                    format!("type kind changed ({name})"),
                );
            }
            _ => {}
        }
    }
    diff_dangling(old, new, reached, r);
}

/// References with no definition: no parser can be generated from the new IDL.
fn diff_dangling(old: &Idl, new: &Idl, reached: &BTreeSet<String>, r: &mut Report) {
    let om = old.type_map();
    let nm = new.type_map();
    // A type dropped from `types` while a parser still reaches it.
    let removed: BTreeSet<&String> = reached
        .iter()
        .filter(|name| om.contains_key(*name) && !nm.contains_key(*name))
        .collect();
    for name in &removed {
        r.push(
            Severity::Breaking,
            ChangeKind::TypeRemoved,
            name.as_str(),
            format!("type removed while still referenced ({name}): dangling reference"),
        );
    }
    // A reference that dangles in both IDLs is not a change, but the new IDL still cannot
    // give a parser.
    for name in crate::reach::undefined_types(new) {
        if !removed.contains(&name) {
            r.push(
                Severity::Breaking,
                ChangeKind::TypeUndefined,
                name.as_str(),
                format!("type used but not defined ({name}): no parser can be generated"),
            );
        }
    }
}

fn diff_fields(ty: &str, of: &[Field], nf: &[Field], reached: bool, r: &mut Report) {
    for pos in 0..of.len().max(nf.len()) {
        let (severity, kind, change) = match (of.get(pos), nf.get(pos)) {
            (Some(o), Some(n)) if o != n => {
                let edit = FieldEdit::of(of, nf, o, n);
                let kind = match edit {
                    FieldEdit::Changed => ChangeKind::FieldChanged,
                    FieldEdit::Moved => ChangeKind::FieldMoved,
                    FieldEdit::Renamed => ChangeKind::FieldRenamed,
                };
                (edit.severity(), kind, field_change(o, n))
            }
            (Some(o), None) => (
                Severity::Breaking,
                ChangeKind::FieldRemoved,
                format!("{} removed (Borsh layout)", field_str(o)),
            ),
            (None, Some(n)) => (
                Severity::Breaking,
                ChangeKind::FieldAdded,
                format!("{} added (Borsh layout)", field_str(n)),
            ),
            _ => continue,
        };
        let subject = format!("{ty}#{pos}");
        if reached {
            r.push(
                severity,
                kind,
                subject,
                format!("FIELD changed ({ty}#{pos}) via reachable type: {change}"),
            );
        } else {
            r.push(
                Severity::Cosmetic,
                kind,
                subject,
                format!("field changed in UNREACHED type ({ty}#{pos}): dead code"),
            );
        }
    }
}

fn diff_variants(ty: &str, ov: &[EnumVariant], nv: &[EnumVariant], reached: bool, r: &mut Report) {
    let old_max = ov.len().saturating_sub(1);
    for pos in 0..ov.len().max(nv.len()) {
        let (severity, kind, message) = match (ov.get(pos), nv.get(pos)) {
            (Some(o), Some(n)) if o.name == n.name && o.tag(pos) != n.tag(pos) => (
                Severity::Breaking,
                ChangeKind::VariantTagChanged,
                format!(
                    "enum variant tag value changed ({ty}#{pos} {}): {} -> {}",
                    o.name,
                    o.tag(pos),
                    n.tag(pos)
                ),
            ),
            (Some(o), Some(n)) if o.name == n.name && !same_payload(&o.fields, &n.fields) => (
                Severity::Breaking,
                ChangeKind::VariantPayloadChanged,
                format!(
                    "enum variant payload changed ({ty}#{pos} {}): {}",
                    o.name,
                    payload_change(&o.fields, &n.fields)
                ),
            ),
            // Either name at another index on the other side means the tag now selects a
            // different variant. A fresh name with the same payload is a rename.
            (Some(o), Some(n)) if o.name != n.name => {
                let moved = ov.iter().any(|v| v.name == n.name)
                    || nv.iter().any(|v| v.name == o.name)
                    || !same_payload(&o.fields, &n.fields);
                if moved {
                    (
                        Severity::Breaking,
                        ChangeKind::VariantMoved,
                        format!(
                            "enum variant changed at index ({ty}#{pos}): {} -> {} (tag reassigned)",
                            o.name, n.name
                        ),
                    )
                } else {
                    (
                        Severity::Dangerous,
                        ChangeKind::VariantRenamed,
                        format!(
                            "enum variant renamed ({ty}#{pos}): {} -> {} (same tag and payload)",
                            o.name, n.name
                        ),
                    )
                }
            }
            (None, Some(_)) if pos > old_max => (
                Severity::Dangerous,
                ChangeKind::VariantAppended,
                format!("enum variant APPENDED ({ty}#{pos}): existing tags unchanged, parser must handle new variant"),
            ),
            (None, Some(_)) => (
                Severity::Breaking,
                ChangeKind::VariantInserted,
                format!("enum variant INSERTED at non-terminal index ({ty}#{pos}): shifts later tags"),
            ),
            (Some(_), None) => (
                Severity::Breaking,
                ChangeKind::VariantRemoved,
                format!("enum variant removed ({ty}#{pos}): shifts later tags"),
            ),
            _ => continue,
        };
        let subject = format!("{ty}#{pos}");
        if reached {
            r.push(severity, kind, subject, message);
        } else {
            r.push(
                Severity::Cosmetic,
                kind,
                subject,
                format!("variant changed in UNREACHED enum ({ty}#{pos})"),
            );
        }
    }
}

fn payload_change(old: &EnumFields, new: &EnumFields) -> String {
    let (os, ns) = (payload_str(old), payload_str(new));
    let unreadable = |f: &EnumFields| match f {
        EnumFields::Tuple(items) => items.iter().any(has_unreadable),
        EnumFields::Named(fields) => fields.iter().any(|f| has_unreadable(&f.ty)),
    };
    if os == ns && (unreadable(old) || unreadable(new)) {
        unreadable_change(&os)
    } else {
        format!("{os} -> {ns} (Borsh layout)")
    }
}

/// A unit variant may be written as an empty tuple or an empty field list.
fn same_payload(a: &EnumFields, b: &EnumFields) -> bool {
    let empty = |f: &EnumFields| match f {
        EnumFields::Named(v) => v.is_empty(),
        EnumFields::Tuple(v) => v.is_empty(),
    };
    a == b || (empty(a) && empty(b))
}
