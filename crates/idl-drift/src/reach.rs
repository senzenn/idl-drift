//! Which types a parser reads, found by following type references from a set of roots.

use std::collections::BTreeSet;

use crate::layout::TypeIndex;
use crate::model::{EnumFields, Idl, Type, TypeDefTy};

/// Names of the types a parser reads: account and event types, types named in
/// instruction args, and everything their fields reference.
#[must_use]
pub fn reachable_types(idl: &Idl) -> BTreeSet<String> {
    let mut roots: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
    roots.extend(idl.events.iter().map(|e| e.name.as_str()));
    for arg in idl.instructions.iter().flat_map(|ix| &ix.args) {
        push_defined(&arg.ty, &mut roots);
    }
    reachable(&TypeIndex::new(idl), roots)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Names that an account, an event or a `defined` type uses but `types` does not hold, from
/// every arg and type, reached or not. A parser generator cannot build such an IDL.
pub(crate) fn undefined_types(idl: &Idl) -> BTreeSet<String> {
    let index = TypeIndex::new(idl);
    let mut used: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
    used.extend(idl.events.iter().map(|e| e.name.as_str()));
    for arg in idl.instructions.iter().flat_map(|ix| &ix.args) {
        push_defined(&arg.ty, &mut used);
    }
    let all: Vec<&str> = idl.types.iter().map(|t| t.name.as_str()).collect();
    for name in reachable(&index, all) {
        used.push(name);
    }
    used.into_iter()
        .filter(|name| index.get(name).is_none())
        .map(str::to_string)
        .collect()
}

/// The roots and every type they reach through fields, variant payloads and aliases. A name
/// with no definition stays in the set, but has nothing to follow.
pub(crate) fn reachable<'a>(index: &TypeIndex<'a>, mut stack: Vec<&'a str>) -> BTreeSet<&'a str> {
    let mut seen = BTreeSet::new();
    while let Some(name) = stack.pop() {
        if !seen.insert(name) {
            continue;
        }
        let Some(td) = index.get(name) else { continue };
        match &td.ty {
            TypeDefTy::Struct { fields } => {
                for f in fields {
                    push_defined(&f.ty, &mut stack);
                }
            }
            TypeDefTy::Enum { variants, .. } => {
                for v in variants {
                    match &v.fields {
                        EnumFields::Named(fields) => {
                            for f in fields {
                                push_defined(&f.ty, &mut stack);
                            }
                        }
                        EnumFields::Tuple(items) => {
                            for ty in items {
                                push_defined(ty, &mut stack);
                            }
                        }
                    }
                }
            }
            TypeDefTy::Type { alias } => push_defined(alias, &mut stack),
        }
    }
    seen
}

/// Push the name of every type from `types` that `ty` uses, generic arguments included.
pub(crate) fn push_defined<'a>(ty: &'a Type, out: &mut Vec<&'a str>) {
    if let Type::Defined { name, .. } = ty {
        out.push(name);
    }
    for child in ty.children() {
        push_defined(child, out);
    }
}
