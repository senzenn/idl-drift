//! Type definitions: struct fields, enum variants and aliases. A field, variant, tag or alias
//! change on a type that no parser reads is Cosmetic.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use super::normalize::{body_types, each_child_mut, rename_generics};
use super::text::{
    field_change, field_str, has_unreadable, payload_str, type_change, unreadable_change,
};
use super::{names, zero_copy, ChangeKind, FieldEdit, Report, Severity};
use crate::model::{EnumFields, EnumVariant, Field, GenericParam, Idl, Type, TypeDef, TypeDefTy};

pub(super) fn diff_types(old: &Idl, new: &Idl, reached: &BTreeSet<String>, r: &mut Report) {
    let om = old.type_map();
    let nm = new.type_map();
    let defs = Defs {
        old: &om,
        new: &nm,
        work: Cell::new(0),
    };
    // An old parser reads a type only when the old IDL reaches it too.
    let old_reached = super::reachable_types(old);
    let read_by_both: BTreeSet<String> = reached.intersection(&old_reached).cloned().collect();
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
        let is_reached = read_by_both.contains(name);

        if attrs_changed(ot, nt) {
            zero_copy::diff_layout_attrs(name, ot, nt, &read_by_both, &roots, r);
        }

        let renamed = with_old_generic_names(ot, nt);
        match (&ot.ty, renamed.as_ref().unwrap_or(&nt.ty)) {
            (TypeDefTy::Struct { fields: of }, TypeDefTy::Struct { fields: nf }) => {
                diff_fields(name, of, nf, is_reached, &defs, r);
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
                diff_variants(name, ov, nv, is_reached, &defs, r);
            }
            // `Amount = u64` to `u32` changes the width everywhere `Amount` is used.
            (TypeDefTy::Type { alias: oa }, TypeDefTy::Type { alias: na })
                if !defs.same_bytes(oa, na) =>
            {
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

/// The body of `nt` with each generic param named as the old param at its position. Args
/// bind by position, so a renamed param is no change, and a reordered one changes the fields.
fn with_old_generic_names(ot: &TypeDef, nt: &TypeDef) -> Option<TypeDefTy> {
    if ot.generics.len() != nt.generics.len() {
        return None;
    }
    let names: BTreeMap<&str, &str> = nt
        .generics
        .iter()
        .map(GenericParam::name)
        .zip(ot.generics.iter().map(GenericParam::name))
        .filter(|(new, old)| new != old)
        .collect();
    if names.is_empty() {
        return None;
    }
    let mut ty = nt.ty.clone();
    for field_ty in body_types(&mut ty) {
        rename_generics(field_ty, &names);
    }
    Some(ty)
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

fn diff_fields(ty: &str, of: &[Field], nf: &[Field], reached: bool, defs: &Defs, r: &mut Report) {
    let (old_names, new_names) = (names(of), names(nf));
    for pos in 0..of.len().max(nf.len()) {
        let (severity, kind, change) = match (of.get(pos), nf.get(pos)) {
            (Some(o), Some(n)) => {
                let Some(n) = defs.edited(o, n) else { continue };
                let edit = FieldEdit::of(&old_names, &new_names, o, &n);
                let kind = match edit {
                    FieldEdit::Changed => ChangeKind::FieldChanged,
                    FieldEdit::Moved => ChangeKind::FieldMoved,
                    FieldEdit::Renamed => ChangeKind::FieldRenamed,
                };
                (edit.severity(), kind, field_change(o, &n))
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

/// A change to report: its tier, rule and message.
type VariantChange = (Severity, ChangeKind, String);

fn diff_variants(
    ty: &str,
    ov: &[EnumVariant],
    nv: &[EnumVariant],
    reached: bool,
    defs: &Defs,
    r: &mut Report,
) {
    let old_names: BTreeSet<&str> = ov.iter().map(|v| v.name.as_str()).collect();
    let new_names: BTreeSet<&str> = nv.iter().map(|v| v.name.as_str()).collect();
    let in_old = |name: &str| old_names.contains(name);
    let in_new = |name: &str| new_names.contains(name);
    // A new variant after the last old one is appended: no old tag moves for it.
    let last_old = nv.iter().rposition(|v| in_old(&v.name));
    // When the kept variants keep their order, each shift comes from an insert or a removal,
    // which is reported at its own index.
    let kept_order = ov
        .iter()
        .map(|v| v.name.as_str())
        .filter(|n| in_new(n))
        .eq(nv.iter().map(|v| v.name.as_str()).filter(|n| in_old(n)));
    for pos in 0..ov.len().max(nv.len()) {
        let change = match (ov.get(pos), nv.get(pos)) {
            (Some(o), Some(n)) if o.name == n.name => same_name_change(ty, pos, o, n, defs),
            // The old variant moves to a later index, so its tag and the ones after it shift.
            (Some(o), Some(n)) if !in_old(&n.name) && in_new(&o.name) => Some(inserted(ty, pos)),
            (Some(o), Some(n)) if !in_new(&o.name) && in_old(&n.name) => Some(removed(ty, pos)),
            (Some(o), Some(n)) if kept_order && in_old(&n.name) && in_new(&o.name) => None,
            // Either name at another index on the other side means the tag now selects a
            // different variant. A fresh name with the same tag and payload is a rename.
            (Some(o), Some(n)) => {
                let moved = in_old(&n.name)
                    || in_new(&o.name)
                    || o.tag(pos) != n.tag(pos)
                    || matches!(defs.payload_edit(&o.fields, &n.fields), PayloadEdit::Changed);
                Some(if moved {
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
                })
            }
            // An old variant at another index: the change is reported at the index it left.
            (None, Some(n)) if in_old(&n.name) => None,
            (Some(o), None) if in_new(&o.name) => None,
            (None, Some(_)) if last_old.is_none_or(|last| last < pos) => Some((
                Severity::Dangerous,
                ChangeKind::VariantAppended,
                format!("enum variant APPENDED ({ty}#{pos}): existing tags unchanged, parser must handle new variant"),
            )),
            (None, Some(_)) => Some(inserted(ty, pos)),
            (Some(_), None) => Some(removed(ty, pos)),
            (None, None) => None,
        };
        let Some((severity, kind, message)) = change else {
            continue;
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

/// A variant that keeps its name: its tag or its payload can change.
fn same_name_change(
    ty: &str,
    pos: usize,
    o: &EnumVariant,
    n: &EnumVariant,
    defs: &Defs,
) -> Option<VariantChange> {
    if o.tag(pos) != n.tag(pos) {
        return Some((
            Severity::Breaking,
            ChangeKind::VariantTagChanged,
            format!(
                "enum variant tag value changed ({ty}#{pos} {}): {} -> {}",
                o.name,
                o.tag(pos),
                n.tag(pos)
            ),
        ));
    }
    match defs.payload_edit(&o.fields, &n.fields) {
        PayloadEdit::Same => None,
        PayloadEdit::Renamed => Some((
            Severity::Dangerous,
            ChangeKind::FieldRenamed,
            format!(
                "enum variant payload fields renamed ({ty}#{pos} {}): {} -> {} (same bytes)",
                o.name,
                payload_str(&o.fields),
                payload_str(&n.fields)
            ),
        )),
        PayloadEdit::Changed => Some((
            Severity::Breaking,
            ChangeKind::VariantPayloadChanged,
            format!(
                "enum variant payload changed ({ty}#{pos} {}): {}",
                o.name,
                payload_change(&o.fields, &n.fields)
            ),
        )),
    }
}

fn inserted(ty: &str, pos: usize) -> VariantChange {
    (
        Severity::Breaking,
        ChangeKind::VariantInserted,
        format!("enum variant INSERTED at non-terminal index ({ty}#{pos}): shifts later tags"),
    )
}

fn removed(ty: &str, pos: usize) -> VariantChange {
    (
        Severity::Breaking,
        ChangeKind::VariantRemoved,
        format!("enum variant removed ({ty}#{pos}): shifts later tags"),
    )
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

/// How a variant payload changed, field by field.
enum PayloadEdit {
    Same,
    /// The same bytes under other field names.
    Renamed,
    Changed,
}

/// Payload fields, with each tuple item named by its index. A named and a tuple payload of
/// the same types then compare as a rename, and a unit variant as no fields.
fn payload_fields(fields: &EnumFields) -> Vec<Field> {
    match fields {
        EnumFields::Named(fields) => fields.clone(),
        EnumFields::Tuple(items) => items
            .iter()
            .enumerate()
            .map(|(i, ty)| Field {
                name: i.to_string(),
                ty: ty.clone(),
            })
            .collect(),
    }
}

/// The old and new type definitions, to see through a link to a struct.
struct Defs<'a, 'b> {
    old: &'b BTreeMap<String, &'a TypeDef>,
    new: &'b BTreeMap<String, &'a TypeDef>,
    /// Nodes that the current [`Defs::same_bytes`] call may still visit.
    work: Cell<usize>,
}

impl Defs<'_, '_> {
    /// `true` when `o` and `n` are the same bytes: equal, or a link to a non-generic struct
    /// that the other side writes inline. Codama inlines a type that has only one use. Two
    /// links with different names stay different.
    fn same_bytes(&self, o: &Type, n: &Type) -> bool {
        self.work.set(MAX_SAME_WORK);
        self.same_at(o, n, 0)
    }

    fn same_at(&self, o: &Type, n: &Type, depth: usize) -> bool {
        if o == n {
            return true;
        }
        // Links that each expand to many links grow without a limit. "Not the same" reports a
        // change, which is the safe answer.
        let Some(left) = self.work.get().checked_sub(1) else {
            return false;
        };
        self.work.set(left);
        if depth >= MAX_SAME_DEPTH {
            return false;
        }
        match (o, n) {
            (Type::Struct(of), Type::Struct(nf)) => return self.same_fields(of, nf, depth),
            (Type::Defined { .. }, Type::Struct(nf)) => {
                return inline_fields(o, self.old)
                    .is_some_and(|of| self.same_fields(of, nf, depth));
            }
            (Type::Struct(of), Type::Defined { .. }) => {
                return inline_fields(n, self.new)
                    .is_some_and(|nf| self.same_fields(of, nf, depth));
            }
            _ => {}
        }
        let (oc, nc) = (o.children(), n.children());
        oc.len() == nc.len()
            && shell(o) == shell(n)
            && oc
                .iter()
                .zip(&nc)
                .all(|(a, b)| self.same_at(a, b, depth + 1))
    }

    fn same_fields(&self, of: &[Field], nf: &[Field], depth: usize) -> bool {
        of.len() == nf.len()
            && of
                .iter()
                .zip(nf)
                .all(|(a, b)| a.name == b.name && self.same_at(&a.ty, &b.ty, depth + 1))
    }

    /// `n` as a change from `o`, with the old type when the bytes are the same, so that only
    /// the name can differ. `None` when nothing changed.
    fn edited(&self, o: &Field, n: &Field) -> Option<Field> {
        let same = self.same_bytes(&o.ty, &n.ty);
        if same && o.name == n.name {
            return None;
        }
        Some(Field {
            name: n.name.clone(),
            ty: if same { o.ty.clone() } else { n.ty.clone() },
        })
    }

    fn payload_edit(&self, o: &EnumFields, n: &EnumFields) -> PayloadEdit {
        let (of, nf) = (payload_fields(o), payload_fields(n));
        if of.len() != nf.len() {
            return PayloadEdit::Changed;
        }
        let (old_names, new_names) = (names(&of), names(&nf));
        let mut edit = PayloadEdit::Same;
        for (a, b) in of.iter().zip(&nf) {
            let Some(b) = self.edited(a, b) else { continue };
            match FieldEdit::of(&old_names, &new_names, a, &b) {
                FieldEdit::Renamed => edit = PayloadEdit::Renamed,
                FieldEdit::Changed | FieldEdit::Moved => return PayloadEdit::Changed,
            }
        }
        edit
    }
}

/// A struct inside a struct inside a link: real IDLs nest far less than this.
const MAX_SAME_DEPTH: usize = 16;
/// Type nodes that one compare may visit. A real link and its inline copy have far fewer.
const MAX_SAME_WORK: usize = 1 << 14;

/// `ty` with each child type set to `bool`, so two nodes compare without their children.
fn shell(ty: &Type) -> Type {
    let mut ty = ty.clone();
    each_child_mut(&mut ty, &mut |child| *child = Type::Bool);
    ty
}

/// The fields of a link to a non-generic struct.
fn inline_fields<'a>(ty: &Type, defs: &BTreeMap<String, &'a TypeDef>) -> Option<&'a [Field]> {
    let Type::Defined { name, generics } = ty else {
        return None;
    };
    let td = defs.get(name)?;
    match &td.ty {
        TypeDefTy::Struct { fields } if generics.is_empty() && td.generics.is_empty() => {
            Some(fields)
        }
        _ => None,
    }
}
