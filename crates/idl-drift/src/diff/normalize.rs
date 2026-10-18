//! Bring two IDLs from different formats into one form, so that only real changes differ.

use std::collections::BTreeMap;

use crate::model::{
    ArrayLen, EnumFields, Field, Format, GenericArg, GenericParam, Idl, Type, TypeDefTy,
};

/// An IDL can make a type hold itself, or nest wide structs, so inlining stops at this depth.
const MAX_INLINE_DEPTH: usize = 16;
/// Far above any real instruction, far below what fills memory.
const MAX_INLINED_ARGS: usize = 1024;
/// Far above any real IDL, far below what fills memory. Each expansion pays for every type node
/// it creates, so a wide body or an arg used many times cannot get past the cap.
const MAX_EXPANDED_NODES: usize = 1 << 16;
/// Node visits while the sizes are measured. A refused expansion still costs its visits, so a
/// hostile IDL cannot make the measurement itself run for a long time.
const MAX_MEASURE_WORK: usize = 1 << 20;

/// What generic expansion may still spend.
struct Allowance {
    /// New type nodes.
    nodes: usize,
    /// Node visits while measuring.
    work: usize,
}

/// Copies of `old` and `new` with camelCase names, tuple structs, generic types expanded at
/// each use and inlined struct args.
pub(super) fn common_form(old: &Idl, new: &Idl) -> (Idl, Idl) {
    let mut old_form = with_tuple_structs(old).with_camel_case_names();
    let mut new_form = with_tuple_structs(new).with_camel_case_names();
    expand_generic_uses(&mut old_form);
    expand_generic_uses(&mut new_form);
    inline_struct_args(&mut old_form);
    inline_struct_args(&mut new_form);
    // Anchor IDLs can't describe an event envelope, so there is nothing to compare.
    if old.format != Format::Codama || new.format != Format::Codama {
        clear_envelopes(&mut old_form);
        clear_envelopes(&mut new_form);
    }
    (old_form, new_form)
}

/// The types that a type body holds directly: field, variant payload and alias types.
pub(super) fn body_types(body: &mut TypeDefTy) -> Vec<&mut Type> {
    match body {
        TypeDefTy::Struct { fields } => fields.iter_mut().map(|f| &mut f.ty).collect(),
        TypeDefTy::Enum { variants, .. } => variants
            .iter_mut()
            .flat_map(|v| match &mut v.fields {
                EnumFields::Named(fields) => fields.iter_mut().map(|f| &mut f.ty).collect(),
                EnumFields::Tuple(items) => items.iter_mut().collect::<Vec<_>>(),
            })
            .collect(),
        TypeDefTy::Type { alias } => vec![alias],
    }
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

/// Replace each arg that holds a struct (linked or inline) with the struct's fields, and each
/// arg that links an alias with the alias target, until no such arg is left. Borsh writes a
/// struct as its fields. Converters inline to different depths: Codama inlines only a type
/// with one use, and Shipstern flattens only one level.
fn inline_struct_args(idl: &mut Idl) {
    let bodies: BTreeMap<String, TypeDefTy> = idl
        .types
        .iter()
        .filter(|td| td.generics.is_empty())
        .filter(|td| !matches!(td.ty, TypeDefTy::Enum { .. }))
        .map(|td| (td.name.clone(), td.ty.clone()))
        .collect();
    let parts = |arg: &Field| -> Option<Vec<Field>> {
        let body = match &arg.ty {
            Type::Struct(fields) => return Some(fields.clone()),
            Type::Defined { name, generics } if generics.is_empty() => bodies.get(name)?,
            _ => return None,
        };
        Some(match body {
            // Codama inlines a tuple struct as a tuple and keeps the arg name.
            TypeDefTy::Struct { fields } if is_tuple(fields) => vec![Field {
                name: arg.name.clone(),
                ty: Type::Tuple(fields.iter().map(|f| f.ty.clone()).collect()),
            }],
            TypeDefTy::Struct { fields } => fields.clone(),
            TypeDefTy::Type { alias } => vec![Field {
                name: arg.name.clone(),
                ty: alias.clone(),
            }],
            TypeDefTy::Enum { .. } => return None,
        })
    };
    for ix in &mut idl.instructions {
        for _ in 0..MAX_INLINE_DEPTH {
            let next: Vec<Option<Vec<Field>>> = ix.args.iter().map(parts).collect();
            if next.iter().all(Option::is_none) {
                break;
            }
            let next_len: usize = next.iter().map(|p| p.as_ref().map_or(1, Vec::len)).sum();
            if next_len > MAX_INLINED_ARGS {
                break;
            }
            ix.args = std::mem::take(&mut ix.args)
                .into_iter()
                .zip(next)
                .flat_map(|(arg, part)| part.unwrap_or_else(|| vec![arg]))
                .collect();
        }
    }
}

/// Generic structs and aliases by name: the params and the body.
type GenericDefs = BTreeMap<String, (Vec<GenericParam>, TypeDefTy)>;

/// Write each use of a generic struct or alias as its body with the args in place, as Codama
/// converters do (Shipstern's `defined_type`). Shipstern rejects a generic enum: it stays a link.
fn expand_generic_uses(idl: &mut Idl) {
    let defs: GenericDefs = idl
        .types
        .iter()
        .filter(|td| !td.generics.is_empty())
        .filter(|td| !matches!(td.ty, TypeDefTy::Enum { .. }))
        .map(|td| (td.name.clone(), (td.generics.clone(), td.ty.clone())))
        .collect();
    if defs.is_empty() {
        return;
    }
    let mut allowance = Allowance {
        nodes: MAX_EXPANDED_NODES,
        work: MAX_MEASURE_WORK,
    };
    let mut expand = |ty: &mut Type| expand_type(ty, &defs, &mut allowance, 0);
    for arg in idl.instructions.iter_mut().flat_map(|ix| &mut ix.args) {
        expand(&mut arg.ty);
    }
    // The body of a generic type keeps its params; no parser reads it after expansion.
    for td in idl.types.iter_mut().filter(|td| td.generics.is_empty()) {
        for ty in body_types(&mut td.ty) {
            expand(ty);
        }
    }
}

/// `depth` counts nested expansions, so a type that holds itself stops.
fn expand_type(ty: &mut Type, defs: &GenericDefs, allowance: &mut Allowance, depth: usize) {
    let body = if depth < MAX_INLINE_DEPTH {
        instantiate(ty, defs, allowance)
    } else {
        None
    };
    match body {
        Some(body) => {
            *ty = body;
            expand_type(ty, defs, allowance, depth + 1);
        }
        None => each_child_mut(ty, &mut |child| expand_type(child, defs, allowance, depth)),
    }
}

/// The body of the generic type that `ty` uses, with the args in place of the params. A
/// tuple struct becomes a tuple, as Codama writes it.
fn instantiate(ty: &Type, defs: &GenericDefs, allowance: &mut Allowance) -> Option<Type> {
    let Type::Defined { name, generics } = ty else {
        return None;
    };
    let (params, body) = defs.get(name).filter(|_| !generics.is_empty())?;
    let scope: BTreeMap<&str, &GenericArg> = params
        .iter()
        .map(GenericParam::name)
        .zip(generics)
        .collect();
    // Count the nodes before any clone, so a refused expansion allocates nothing.
    let cost = match body {
        TypeDefTy::Struct { fields } => {
            let mut cost = fields.len().max(1);
            for field in fields {
                let room = allowance.nodes.checked_sub(cost)?;
                cost += expanded_size(&field.ty, &scope, room, &mut allowance.work)?;
            }
            cost
        }
        TypeDefTy::Type { alias } => {
            expanded_size(alias, &scope, allowance.nodes, &mut allowance.work)?
        }
        TypeDefTy::Enum { .. } => return None,
    };
    allowance.nodes = allowance.nodes.checked_sub(cost)?;
    let subst = |ty: &Type| {
        let mut ty = ty.clone();
        substitute(&mut ty, &scope);
        ty
    };
    Some(match body {
        TypeDefTy::Struct { fields } if is_tuple(fields) => {
            Type::Tuple(fields.iter().map(|f| subst(&f.ty)).collect())
        }
        TypeDefTy::Struct { fields } => Type::Struct(
            fields
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: subst(&f.ty),
                })
                .collect(),
        ),
        TypeDefTy::Type { alias } => subst(alias),
        TypeDefTy::Enum { .. } => return None,
    })
}

/// Put the args in `scope` in place of the type and const params in `ty`.
/// Type nodes in `ty` once the args are in place, or `None` when that passes `limit` or the work
/// runs out. Each child gets only the room that is left, so the walk stops early.
fn expanded_size(
    ty: &Type,
    scope: &BTreeMap<&str, &GenericArg>,
    limit: usize,
    work: &mut usize,
) -> Option<usize> {
    *work = work.checked_sub(1)?;
    if let Type::Generic(name) = ty {
        if let Some(GenericArg::Type { ty: arg }) = scope.get(name.as_str()) {
            return expanded_size(arg, &BTreeMap::new(), limit, work);
        }
    }
    let mut size = 1_usize;
    for child in ty.children() {
        let room = limit.checked_sub(size)?;
        size += expanded_size(child, scope, room, work)?;
    }
    (size <= limit).then_some(size)
}

fn substitute(ty: &mut Type, scope: &BTreeMap<&str, &GenericArg>) {
    let konst = |name: &str| match scope.get(name) {
        Some(GenericArg::Const { value }) => Some(value.clone()),
        _ => None,
    };
    match ty {
        Type::Generic(name) => {
            if let Some(GenericArg::Type { ty: arg }) = scope.get(name.as_str()) {
                *ty = (**arg).clone();
            }
            return;
        }
        Type::Array(_, len) => {
            if let ArrayLen::Generic(name) = len {
                if let Some(n) = konst(name).and_then(|v| crate::model::const_len(&v)) {
                    *len = ArrayLen::Value(n);
                }
            }
        }
        Type::Defined { generics, .. } => {
            for arg in generics {
                if let GenericArg::Const { value } = arg {
                    if let Some(v) = konst(value) {
                        *value = v;
                    }
                }
            }
        }
        _ => {}
    }
    each_child_mut(ty, &mut |child| substitute(child, scope));
}

/// Rename generic params in `ty`: `names` maps a param name to its new name.
pub(super) fn rename_generics(ty: &mut Type, names: &BTreeMap<&str, &str>) {
    let rename = |name: &mut String| {
        if let Some(to) = names.get(name.as_str()) {
            *name = (*to).to_string();
        }
    };
    match ty {
        Type::Generic(name) | Type::Array(_, ArrayLen::Generic(name)) => rename(name),
        Type::Defined { generics, .. } => {
            for arg in generics {
                if let GenericArg::Const { value } = arg {
                    rename(value);
                }
            }
        }
        _ => {}
    }
    each_child_mut(ty, &mut |child| rename_generics(child, names));
}

/// Call `f` on each type that `ty` holds, the same list as `Type::children`.
fn each_child_mut(ty: &mut Type, f: &mut impl FnMut(&mut Type)) {
    match ty {
        Type::Option(inner)
        | Type::COption(inner)
        | Type::Vec(inner)
        | Type::Array(inner, _)
        | Type::Set(inner)
        | Type::BigEndian(inner)
        | Type::Counted { ty: inner, .. }
        | Type::CodamaOption { item: inner, .. } => f(inner),
        Type::Map(key, value) => {
            f(key);
            f(value);
        }
        Type::Tuple(items) => items.iter_mut().for_each(f),
        Type::Struct(fields) => fields.iter_mut().for_each(|field| f(&mut field.ty)),
        Type::Defined { generics, .. } => {
            for arg in generics {
                if let GenericArg::Type { ty } = arg {
                    f(ty);
                }
            }
        }
        _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expansion_pays_for_every_node_it_creates() {
        // One field, but it names the param 10 times, and the arg is a 20-item tuple.
        let body = TypeDefTy::Struct {
            fields: vec![Field {
                name: "f".into(),
                ty: Type::Tuple(vec![Type::Generic("T".into()); 10]),
            }],
        };
        let defs: GenericDefs = [(
            "Wide".to_string(),
            (vec![GenericParam::Type { name: "T".into() }], body),
        )]
        .into();
        let wide_use = Type::Defined {
            name: "Wide".into(),
            generics: vec![GenericArg::Type {
                ty: Box::new(Type::Tuple(vec![Type::U8; 20])),
            }],
        };
        let allowance = |nodes| Allowance {
            nodes,
            work: MAX_MEASURE_WORK,
        };
        // 1 field + 1 tuple + 10 copies of a 21-node arg.
        let mut tight = allowance(211);
        assert_eq!(instantiate(&wide_use, &defs, &mut tight), None);
        assert_eq!(tight.nodes, 211, "a refused expansion must cost nothing");
        let mut enough = allowance(1000);
        assert!(instantiate(&wide_use, &defs, &mut enough).is_some());
        assert_eq!(enough.nodes, 1000 - 212);
    }

    #[test]
    fn a_refused_measurement_stops_early() {
        // 1000 copies of a 1001-node arg: about a million nodes to count without an early stop.
        let arg = Type::Tuple(vec![Type::U8; 1000]);
        let body = Type::Tuple(vec![Type::Generic("T".into()); 1000]);
        let param = GenericArg::Type { ty: Box::new(arg) };
        let scope: BTreeMap<&str, &GenericArg> = [("T", &param)].into();
        let mut work = MAX_MEASURE_WORK;
        assert_eq!(expanded_size(&body, &scope, 100, &mut work), None);
        assert!(
            MAX_MEASURE_WORK - work <= 200,
            "visited {}",
            MAX_MEASURE_WORK - work
        );
    }
}
