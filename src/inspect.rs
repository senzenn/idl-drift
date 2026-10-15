//! Find fields whose layout the converter could not resolve (`Type::Generic`), lay out
//! every zero-copy type field by field, and find padded zero-copy types Borsh also reads.

use std::collections::BTreeSet;

use crate::diff::reachable_types;
use crate::layout::{LayoutError, Padding, Reading, StructLayout, TypeIndex};
use crate::model::{EnumFields, GenericArg, Idl, Repr, Type, TypeDef, TypeDefTy};

/// A field or instruction arg whose type is or contains `Type::Generic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmappedField {
    /// Such as `MyAccount.owner` or `ix:swap.amount`.
    pub path: String,
    /// The string inside `Type::Generic`.
    pub kind: String,
}

/// Coverage of one IDL.
#[derive(Debug, Clone)]
pub struct CoverageReport {
    pub instructions: usize,
    pub accounts: usize,
    pub events: usize,
    pub types: usize,
    pub unmapped: Vec<UnmappedField>,
}

impl CoverageReport {
    /// `true` when every reachable field has a concrete layout.
    #[must_use]
    pub fn fully_mapped(&self) -> bool {
        self.unmapped.is_empty()
    }
}

/// Scan instruction args and reachable types for `Type::Generic`.
#[must_use]
pub fn coverage(idl: &Idl) -> CoverageReport {
    let reached = reachable_types(idl);
    let mut unmapped = Vec::new();

    for ix in &idl.instructions {
        for arg in &ix.args {
            collect_generic(
                &format!("ix:{}.{}", ix.name, arg.name),
                &arg.ty,
                &mut unmapped,
            );
        }
    }

    for td in &idl.types {
        if !reached.contains(&td.name) {
            continue;
        }
        match &td.ty {
            TypeDefTy::Struct { fields } => {
                for f in fields {
                    collect_generic(&format!("{}.{}", td.name, f.name), &f.ty, &mut unmapped);
                }
            }
            TypeDefTy::Enum { variants, tag } => {
                if let Some(tag) = tag {
                    collect_generic(&format!("{}.<tag>", td.name), tag, &mut unmapped);
                }
                for v in variants {
                    match &v.fields {
                        EnumFields::Named(fields) => {
                            for f in fields {
                                collect_generic(
                                    &format!("{}.{}.{}", td.name, v.name, f.name),
                                    &f.ty,
                                    &mut unmapped,
                                );
                            }
                        }
                        EnumFields::Tuple(tys) => {
                            for (i, ty) in tys.iter().enumerate() {
                                collect_generic(
                                    &format!("{}.{}.{}", td.name, v.name, i),
                                    ty,
                                    &mut unmapped,
                                );
                            }
                        }
                    }
                }
            }
            TypeDefTy::Type { alias } => {
                collect_generic(&td.name, alias, &mut unmapped);
            }
        }
    }

    CoverageReport {
        instructions: idl.instructions.len(),
        accounts: idl.accounts.len(),
        events: idl.events.len(),
        types: idl.types.len(),
        unmapped,
    }
}

fn collect_generic(path: &str, ty: &Type, out: &mut Vec<UnmappedField>) {
    match ty {
        Type::Generic(kind) => {
            out.push(UnmappedField {
                path: path.to_string(),
                kind: kind.clone(),
            });
        }
        Type::Option(inner)
        | Type::COption(inner)
        | Type::Vec(inner)
        | Type::Array(inner, _)
        | Type::Set(inner) => {
            collect_generic(path, inner, out);
        }
        Type::Map(key, value) => {
            collect_generic(path, key, out);
            collect_generic(path, value, out);
        }
        Type::Tuple(items) => {
            for item in items {
                collect_generic(path, item, out);
            }
        }
        Type::Defined { generics, .. } => {
            for g in generics {
                if let crate::model::GenericArg::Type { ty } = g {
                    collect_generic(path, ty, out);
                }
            }
        }
        Type::Bool
        | Type::U8
        | Type::I8
        | Type::U16
        | Type::I16
        | Type::U32
        | Type::I32
        | Type::U64
        | Type::I64
        | Type::U128
        | Type::I128
        | Type::U256
        | Type::I256
        | Type::F32
        | Type::F64
        | Type::Bytes
        | Type::String
        | Type::Pubkey => {}
    }
}

/// The memory layout of one zero-copy type, next to the size Borsh would read.
#[derive(Debug, Clone)]
pub struct TypeLayout {
    pub name: String,
    pub serialization: String,
    pub repr: Option<Repr>,
    /// `repr(C)` offsets and gaps, or why they cannot be computed.
    pub layout: Result<StructLayout, LayoutError>,
    /// `None` when a field has no fixed Borsh size.
    pub borsh_size: Option<usize>,
}

/// Every `bytemuck` and `bytemuckunsafe` type, in IDL order. A converter that drops the
/// `serialization` field makes this list empty, which is itself the finding.
#[must_use]
pub fn layouts(idl: &Idl) -> Vec<TypeLayout> {
    let index = TypeIndex::new(idl);
    idl.types
        .iter()
        .filter(|td| td.is_zero_copy())
        .map(|td| {
            let ty = Type::Defined {
                name: td.name.clone(),
                generics: Vec::new(),
            };
            TypeLayout {
                name: td.name.clone(),
                serialization: td.serialization.clone(),
                repr: td.repr,
                layout: index.struct_layout(td),
                borsh_size: index.reading_size(&ty, Reading::Borsh).ok(),
            }
        })
        .collect()
}

/// A type in zero-copy memory, with implicit padding, that Borsh also reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedType {
    pub name: String,
    pub padding: Vec<Padding>,
}

/// Padded types inside zero-copy accounts that an instruction argument, an event or a Borsh
/// account also reaches. Borsh writes no padding, so one of the two reads is wrong. This is
/// the case Shipstern's converter rejects as `PaddedBorshType`.
#[must_use]
pub fn shared_types(idl: &Idl) -> Vec<SharedType> {
    let index = TypeIndex::new(idl);
    let accounts: Vec<&TypeDef> = idl
        .accounts
        .iter()
        .filter_map(|a| index.get(&a.name))
        .filter(|t| t.generics.is_empty())
        .collect();
    let names = |serialization: &str| -> Vec<&str> {
        accounts
            .iter()
            .filter(|t| t.serialization == serialization)
            .map(|t| t.name.as_str())
            .collect()
    };
    // A `bytemuck` derive checks only its own fields, so an unsafe struct inside it is a root.
    let in_bytemuck = reachable(&index, names("bytemuck"));
    let roots = names("bytemuckunsafe")
        .into_iter()
        .chain(in_bytemuck.into_iter().filter(|name| {
            index
                .get(name)
                .is_some_and(|t| t.serialization == "bytemuckunsafe" && t.generics.is_empty())
        }))
        .collect();
    let in_zero_copy = reachable(&index, roots);

    let mut borsh_roots = names("borsh");
    for arg in idl.instructions.iter().flat_map(|ix| &ix.args) {
        push_defined(&arg.ty, &mut borsh_roots);
    }
    borsh_roots.extend(idl.events.iter().map(|e| e.name.as_str()));
    let borsh = reachable(&index, borsh_roots);

    idl.types
        .iter()
        .filter(|t| in_zero_copy.contains(t.name.as_str()) && borsh.contains(t.name.as_str()))
        .filter_map(|t| {
            let layout = index.struct_layout(t).ok()?;
            (layout.padding_len() > 0).then(|| SharedType {
                name: t.name.clone(),
                padding: layout.padding,
            })
        })
        .collect()
}

/// The named types and every type they reach through fields, variants and aliases.
fn reachable<'a>(index: &TypeIndex<'a>, mut stack: Vec<&'a str>) -> BTreeSet<&'a str> {
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

fn push_defined<'a>(ty: &'a Type, out: &mut Vec<&'a str>) {
    match ty {
        Type::Defined { name, generics } => {
            out.push(name);
            for g in generics {
                if let GenericArg::Type { ty } = g {
                    push_defined(ty, out);
                }
            }
        }
        Type::Option(inner)
        | Type::COption(inner)
        | Type::Vec(inner)
        | Type::Array(inner, _)
        | Type::Set(inner) => push_defined(inner, out),
        Type::Map(key, value) => {
            push_defined(key, out);
            push_defined(value, out);
        }
        Type::Tuple(items) => {
            for item in items {
                push_defined(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voltr(file: &str) -> Idl {
        let path = format!(
            "{}/fixtures/live/voltr_vault/{file}",
            env!("CARGO_MANIFEST_DIR")
        );
        Idl::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn voltr_receipt_shows_its_gap_and_both_sizes() {
        let report = layouts(&voltr("idl.json"));
        let receipt = report
            .iter()
            .find(|t| t.name == "AdaptorAddReceipt")
            .expect("zero-copy type listed");
        let layout = receipt.layout.as_ref().unwrap();
        assert_eq!((layout.size, receipt.borsh_size), (144, Some(137)));
        let gaps: Vec<String> = layout.padding.iter().map(ToString::to_string).collect();
        assert_eq!(gaps, ["7 bytes before `last_updated_epoch`"]);
    }

    #[test]
    fn converter_that_drops_serialization_lists_no_zero_copy_types() {
        assert!(layouts(&voltr("idl.js-codama.json")).is_empty());
    }

    #[test]
    fn padded_zero_copy_type_read_as_borsh_is_flagged() {
        let idl = |args: &str| {
            Idl::from_json(&format!(
                r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
                "accounts":[{{"name":"Pool","discriminator":[1]}}],
                "instructions":[{{"name":"set","discriminator":[2],"args":{args}}}],
                "types":[
                  {{"name":"Pool","serialization":"bytemuckunsafe","repr":{{"kind":"c"}},"type":{{"kind":"struct","fields":[
                    {{"name":"cfg","type":{{"defined":{{"name":"Cfg"}}}}}}]}}}},
                  {{"name":"Cfg","serialization":"bytemuckunsafe","repr":{{"kind":"c"}},"type":{{"kind":"struct","fields":[
                    {{"name":"flag","type":"u8"}},{{"name":"epoch","type":"u64"}}]}}}}]}}"#
            ))
            .unwrap()
        };
        let shared = shared_types(&idl(
            r#"[{"name":"cfg","type":{"defined":{"name":"Cfg"}}}]"#,
        ));
        let found: Vec<(&str, String)> = shared
            .iter()
            .map(|s| (s.name.as_str(), s.padding[0].to_string()))
            .collect();
        assert_eq!(found, [("Cfg", "7 bytes before `epoch`".to_string())]);
        // Only the zero-copy account reads it.
        assert!(shared_types(&idl("[]")).is_empty());
    }
}
