//! With `--upgrade-impact --compare-idl`: the bytes that new fields take from a reserved array
//! must be zero, because the new program reads them as values.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use idl_drift::diff::{reserved_uses, ReservedUse};
use idl_drift::model::{EnumFields, Field, GenericArg, Idl, Type, TypeDef, TypeDefTy};
use serde_json::Value as Json;

use crate::layout::{TypeIndex, MAX_DEPTH};
use crate::reference::normalize;

/// The reserved uses between two IDLs, read on the old IDL's decodes.
pub(super) struct ReservedBytes<'a> {
    types: TypeIndex<'a>,
    /// By old struct name; a struct has at most one.
    uses: BTreeMap<&'a str, ReservedUse>,
    /// Old types that hold a struct with a use, through fields, variants, aliases and the
    /// generic arguments that they pass.
    holders: BTreeSet<&'a str>,
}

/// Why a sample breaks: a new field reads a non-zero byte, or the walk cannot reach a
/// reserved array, so the bytes are not checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fault {
    pub reason: String,
    pub detail: String,
}

fn unchecked(what: impl std::fmt::Display) -> Fault {
    Fault {
        reason: "the reserved bytes that new fields take are not checked".into(),
        detail: format!("{what}, so the bytes that new fields take are not checked"),
    }
}

impl<'a> ReservedBytes<'a> {
    /// `None` when no new field of `new` takes reserved bytes of `old`.
    pub(super) fn new(old: &'a Idl, new: &Idl) -> Option<Self> {
        let by_name: BTreeMap<String, ReservedUse> = reserved_uses(old, new)
            .into_iter()
            .map(|u| (normalize(&u.ty), with_new_names(u, new)))
            .collect();
        // The uses carry the names that the diff compares, so match them to the old names.
        let uses: BTreeMap<&str, ReservedUse> = old
            .types
            .iter()
            .filter(|t| matches!(t.ty, TypeDefTy::Struct { .. }))
            .filter_map(|t| Some((t.name.as_str(), by_name.get(&normalize(&t.name))?.clone())))
            .collect();
        if uses.is_empty() {
            return None;
        }
        let holders = holders(&old.types, uses.keys().copied());
        Some(Self {
            types: TypeIndex::new(old),
            uses,
            holders,
        })
    }

    /// Check `value`, the old IDL's decode of its account type `account`. `Ok(None)` when the
    /// type holds no use. Else `value` without the old reserved arrays, so that a compare does
    /// not pair them with the new, shorter ones.
    pub(super) fn check(&self, account: &str, value: &Json) -> Result<Option<Json>, Fault> {
        let root = Type::Defined {
            name: account.to_string(),
            generics: Vec::new(),
        };
        let scope = Scope::default();
        if !self.may_hold(&root, &scope, 0) {
            return Ok(None);
        }
        let mut value = value.clone();
        self.value(&root, &mut value, &scope, 0)?;
        Ok(Some(value))
    }

    /// `true` when a value of `ty` can hold a struct with a use. Too deep counts as `true`, so
    /// that the walk reports it.
    fn may_hold(&self, ty: &Type, scope: &Scope<'_>, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return true;
        }
        match ty {
            Type::Defined { name, .. } if self.holders.contains(name.as_str()) => true,
            Type::Generic(name) => scope
                .get(name)
                .is_none_or(|(ty, outer)| self.may_hold(ty, outer, depth + 1)),
            _ => ty
                .children()
                .into_iter()
                .any(|child| self.may_hold(child, scope, depth + 1)),
        }
    }

    /// Walk `json`, the decode of `ty`, along the old types. Each step goes one level into the
    /// JSON or binds generics, and the depth limit stops a loop of aliases.
    fn value(
        &self,
        ty: &Type,
        json: &mut Json,
        scope: &Scope<'_>,
        depth: usize,
    ) -> Result<(), Fault> {
        if !self.may_hold(ty, scope, 0) {
            return Ok(());
        }
        if depth > MAX_DEPTH {
            return Err(unchecked(format!(
                "the old types nest deeper than {MAX_DEPTH} levels"
            )));
        }
        let shape = |what: &str| unchecked(format!("the old decode does not hold {what}"));
        match ty {
            Type::Defined { name, generics } => {
                let Some(td) = self.types.get(name) else {
                    return Err(unchecked(format!("the old IDL does not define `{name}`")));
                };
                let inner = bind(td, generics, scope)?;
                match &td.ty {
                    TypeDefTy::Struct { fields } => {
                        if let Some(u) = self.uses.get(name.as_str()) {
                            check_struct(name, u, json)?;
                        }
                        self.fields(fields, json, &inner, depth)
                    }
                    TypeDefTy::Type { alias } => self.value(alias, json, &inner, depth + 1),
                    TypeDefTy::Enum { variants, .. } => {
                        let variant = json.as_object_mut().and_then(|map| {
                            let (key, payload) = map.iter_mut().next()?;
                            Some((variants.iter().find(|v| v.name == *key)?, payload))
                        });
                        let Some((v, payload)) = variant else {
                            return Err(shape(&format!("a variant of `{name}`")));
                        };
                        match &v.fields {
                            EnumFields::Named(fields) => {
                                self.fields(fields, payload, &inner, depth)
                            }
                            EnumFields::Tuple(items) => self.tuple(items, payload, &inner, depth),
                        }
                    }
                }
            }
            Type::Generic(name) => match scope.get(name) {
                Some((ty, outer)) => self.value(ty, json, outer, depth + 1),
                None => Err(unchecked(format!("the generic `{name}` has no argument"))),
            },
            Type::Struct(fields) => self.fields(fields, json, scope, depth),
            Type::Tuple(items) => self.tuple(items, json, scope, depth),
            Type::Array(item, _) | Type::Vec(item) | Type::Set(item) => {
                let Json::Array(items) = json else {
                    return Err(shape("a list"));
                };
                items
                    .iter_mut()
                    .try_for_each(|v| self.value(item, v, scope, depth + 1))
            }
            // `to_json` writes a map as a list of `[key, value]` pairs.
            Type::Map(key, value) => {
                let Json::Array(pairs) = json else {
                    return Err(shape("a map"));
                };
                pairs.iter_mut().try_for_each(|pair| {
                    let Some([k, v]) = pair.as_array_mut().map(Vec::as_mut_slice) else {
                        return Err(shape("a map entry"));
                    };
                    self.value(key, k, scope, depth + 1)?;
                    self.value(value, v, scope, depth + 1)
                })
            }
            Type::Option(item)
            | Type::COption(item)
            | Type::CodamaOption { item, .. }
            | Type::Counted { ty: item, .. }
            | Type::BigEndian(item) => {
                if json.is_null() {
                    return Ok(());
                }
                self.value(item, json, scope, depth + 1)
            }
            _ => Ok(()),
        }
    }

    fn fields(
        &self,
        fields: &[Field],
        json: &mut Json,
        scope: &Scope<'_>,
        depth: usize,
    ) -> Result<(), Fault> {
        let Json::Object(map) = json else {
            return Err(unchecked("the old decode does not hold a struct"));
        };
        for f in fields {
            if !self.may_hold(&f.ty, scope, 0) {
                continue;
            }
            let Some(v) = map.get_mut(&f.name) else {
                return Err(unchecked(format!(
                    "the old decode has no field `{}`",
                    f.name
                )));
            };
            self.value(&f.ty, v, scope, depth + 1)?;
        }
        Ok(())
    }

    /// `to_json` writes a tuple as an object keyed by the index.
    fn tuple(
        &self,
        items: &[Type],
        json: &mut Json,
        scope: &Scope<'_>,
        depth: usize,
    ) -> Result<(), Fault> {
        let Json::Object(map) = json else {
            return Err(unchecked("the old decode does not hold a tuple"));
        };
        for (i, ty) in items.iter().enumerate() {
            if !self.may_hold(ty, scope, 0) {
                continue;
            }
            let Some(v) = map.get_mut(&i.to_string()) else {
                return Err(unchecked(format!("the old decode has no tuple item {i}")));
            };
            self.value(ty, v, scope, depth + 1)?;
        }
        Ok(())
    }
}

/// `u` with each new field named as `new` writes it. Across formats the diff compares
/// camelCase names, which the user does not see.
fn with_new_names(mut u: ReservedUse, new: &Idl) -> ReservedUse {
    let fields = new
        .types
        .iter()
        .find(|t| normalize(&t.name) == normalize(&u.ty))
        .and_then(|t| match &t.ty {
            TypeDefTy::Struct { fields } => Some(fields),
            _ => None,
        });
    for t in &mut u.taken {
        let wanted = normalize(&t.field.name);
        if let Some(f) = fields
            .into_iter()
            .flatten()
            .find(|f| normalize(&f.name) == wanted)
        {
            t.field.name.clone_from(&f.name);
        }
    }
    u
}

/// Generic parameter names bound to argument types, each with the scope that the argument
/// is written in, as the decoder binds them.
#[derive(Clone, Default)]
struct Scope<'t>(Option<Rc<BTreeMap<&'t str, (&'t Type, Scope<'t>)>>>);

impl<'t> Scope<'t> {
    fn get(&self, name: &str) -> Option<&(&'t Type, Scope<'t>)> {
        self.0.as_ref()?.get(name)
    }
}

fn bind<'t>(
    td: &'t TypeDef,
    args: &'t [GenericArg],
    outer: &Scope<'t>,
) -> Result<Scope<'t>, Fault> {
    if td.generics.is_empty() {
        return Ok(Scope::default());
    }
    if td.generics.len() != args.len() {
        return Err(unchecked(format!(
            "`{}` takes {} generic arguments, the old IDL passes {}",
            td.name,
            td.generics.len(),
            args.len()
        )));
    }
    let bound = td
        .generics
        .iter()
        .zip(args)
        .filter_map(|(param, arg)| match arg {
            GenericArg::Type { ty } => Some((param.name(), (&**ty, outer.clone()))),
            GenericArg::Const { .. } => None,
        })
        .collect();
    Ok(Scope(Some(Rc::new(bound))))
}

/// Each type that holds one of `roots` through its body, at any depth: the reverse of the
/// links from each type to the types that its body names.
fn holders<'a>(types: &'a [TypeDef], roots: impl Iterator<Item = &'a str>) -> BTreeSet<&'a str> {
    let mut named_by: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for td in types {
        let mut names = Vec::new();
        for ty in body(td) {
            defined(ty, &mut names);
        }
        for name in names {
            named_by.entry(name).or_default().push(&td.name);
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = roots.collect();
    while let Some(name) = stack.pop() {
        if seen.insert(name) {
            stack.extend(named_by.get(name).into_iter().flatten().copied());
        }
    }
    seen
}

fn body(td: &TypeDef) -> Vec<&Type> {
    match &td.ty {
        TypeDefTy::Struct { fields } => fields.iter().map(|f| &f.ty).collect(),
        TypeDefTy::Type { alias } => vec![alias],
        TypeDefTy::Enum { variants, .. } => variants
            .iter()
            .flat_map(|v| match &v.fields {
                EnumFields::Named(fields) => fields.iter().map(|f| &f.ty).collect::<Vec<_>>(),
                EnumFields::Tuple(items) => items.iter().collect(),
            })
            .collect(),
    }
}

/// Each defined name in `ty`, generic arguments included, without recursion.
fn defined<'a>(ty: &'a Type, out: &mut Vec<&'a str>) {
    let mut stack = vec![ty];
    while let Some(ty) = stack.pop() {
        if let Type::Defined { name, .. } = ty {
            out.push(name);
        }
        stack.extend(ty.children());
    }
}

/// The bytes that each new field of `u` takes must be zero. Then drop the old array from
/// `json`: the new array holds other bytes under the same name.
fn check_struct(ty: &str, u: &ReservedUse, json: &mut Json) -> Result<(), Fault> {
    let wanted = normalize(&u.reserved.name);
    let found = json.as_object_mut().and_then(|map| {
        let key = map.keys().find(|k| normalize(k) == wanted)?.clone();
        let bytes = map.remove(&key)?;
        Some((key, bytes))
    });
    let missing = || {
        unchecked(format!(
            "the old decode of `{ty}` has no `{}` array",
            u.reserved.name
        ))
    };
    let Some((key, Json::Array(bytes))) = found else {
        return Err(missing());
    };
    for t in &u.taken {
        let taken = bytes.get(t.bytes.clone()).ok_or_else(missing)?;
        let non_zero = taken
            .iter()
            .zip(t.bytes.clone())
            .find(|(b, _)| b.as_u64() != Some(0));
        if let Some((b, i)) = non_zero {
            let field = format!("`{ty}.{}`", t.field.name);
            return Err(Fault {
                reason: format!("{field} takes reserved bytes that are not zero"),
                detail: format!(
                    "{field} takes bytes {}..{} of the old `{key}`, and `{key}[{i}]` is {b}, not 0",
                    t.bytes.start, t.bytes.end
                ),
            });
        }
    }
    Ok(())
}
