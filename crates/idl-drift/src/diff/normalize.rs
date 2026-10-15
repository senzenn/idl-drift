//! Bring two IDLs from different formats into one form, so that only real changes differ.

use std::collections::BTreeMap;

use crate::model::{Field, Format, Idl, Type, TypeDefTy};

/// An IDL can make a type hold itself, or nest wide structs, so inlining stops at this depth.
const MAX_INLINE_DEPTH: usize = 16;
/// Far above any real instruction, far below what fills memory.
const MAX_INLINED_ARGS: usize = 1024;

/// Copies of `old` and `new` with camelCase names, tuple structs and inlined struct args.
pub(super) fn common_form(old: &Idl, new: &Idl) -> (Idl, Idl) {
    let mut old_form = with_tuple_structs(old).with_camel_case_names();
    let mut new_form = with_tuple_structs(new).with_camel_case_names();
    inline_struct_args(&mut old_form);
    inline_struct_args(&mut new_form);
    // Anchor IDLs can't describe an event envelope, so there is nothing to compare.
    if old.format != Format::Codama || new.format != Format::Codama {
        clear_envelopes(&mut old_form);
        clear_envelopes(&mut new_form);
    }
    (old_form, new_form)
}

/// A copy with each tuple alias written as a tuple struct, fields `_0`, `_1`, ..., as Anchor
/// writes it (see `normalize_type_defs`). Codama writes a tuple struct as a tuple alias, and
/// Borsh writes both the same way. A struct keeps the zero-copy layout computable.
fn with_tuple_structs(idl: &Idl) -> Idl {
    let mut idl = idl.clone();
    for td in &mut idl.types {
        if let TypeDefTy::Type {
            alias: Type::Tuple(items),
        } = &td.ty
        {
            if !items.is_empty() {
                let fields = items
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| Field {
                        name: format!("_{i}"),
                        ty: ty.clone(),
                    })
                    .collect();
                td.ty = TypeDefTy::Struct { fields };
            }
        }
    }
    idl
}

/// Replace each arg that links a non-generic struct with the struct's fields, and each arg
/// that links a non-generic alias with the alias target, until no such arg is left. Borsh
/// writes a struct as its fields. Converters inline to different depths: Codama inlines only
/// a type with one use.
fn inline_struct_args(idl: &mut Idl) {
    let bodies: BTreeMap<String, TypeDefTy> = idl
        .types
        .iter()
        .filter(|td| td.generics.is_empty())
        .filter(|td| !matches!(td.ty, TypeDefTy::Enum { .. }))
        .map(|td| (td.name.clone(), td.ty.clone()))
        .collect();
    let body = |ty: &Type| match ty {
        Type::Defined { name, generics } if generics.is_empty() => bodies.get(name),
        _ => None,
    };
    for ix in &mut idl.instructions {
        for _ in 0..MAX_INLINE_DEPTH {
            if !ix.args.iter().any(|a| body(&a.ty).is_some()) {
                break;
            }
            let next_len: usize = ix
                .args
                .iter()
                .map(|a| match body(&a.ty) {
                    Some(TypeDefTy::Struct { fields }) if !is_tuple(fields) => fields.len(),
                    _ => 1,
                })
                .sum();
            if next_len > MAX_INLINED_ARGS {
                break;
            }
            ix.args = std::mem::take(&mut ix.args)
                .into_iter()
                .flat_map(|arg| match body(&arg.ty) {
                    // Codama inlines a tuple struct as a tuple and keeps the arg name.
                    Some(TypeDefTy::Struct { fields }) if is_tuple(fields) => vec![Field {
                        name: arg.name.clone(),
                        ty: Type::Tuple(fields.iter().map(|f| f.ty.clone()).collect()),
                    }],
                    Some(TypeDefTy::Struct { fields }) => fields.clone(),
                    Some(TypeDefTy::Type { alias }) => vec![Field {
                        name: arg.name.clone(),
                        ty: alias.clone(),
                    }],
                    _ => vec![arg],
                })
                .collect();
        }
    }
}

/// Tuple fields after the camelCase step: `_0` becomes `0`.
fn is_tuple(fields: &[Field]) -> bool {
    !fields.is_empty()
        && fields
            .iter()
            .enumerate()
            .all(|(i, f)| f.name == i.to_string())
}

fn clear_envelopes(idl: &mut Idl) {
    for event in &mut idl.events {
        event.envelope_discriminator.clear();
        event.envelope_payload_offset = None;
    }
}
