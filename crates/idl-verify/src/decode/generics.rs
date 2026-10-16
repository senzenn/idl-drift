//! Generic parameters: bind a defined type's generics to the arguments a use site passes.

use std::collections::BTreeMap;

use idl_drift::model::{const_len, ArrayLen, GenericArg, Type, TypeDef};

use super::DecodeErrorKind;

/// Generic parameter name to its argument, already resolved in the caller's scope.
pub(super) type Scope = BTreeMap<String, GenericArg>;

/// Bind `td`'s generic parameters to `args`, resolving the args in the caller's `outer` scope.
pub(super) fn bind(
    td: &TypeDef,
    args: &[GenericArg],
    outer: &Scope,
) -> Result<Scope, DecodeErrorKind> {
    if td.generics.is_empty() {
        return Ok(Scope::new());
    }
    if td.generics.len() != args.len() {
        return Err(DecodeErrorKind::Unsupported(format!(
            "`{}` takes {} generic arguments, the IDL passes {}",
            td.name,
            td.generics.len(),
            args.len()
        )));
    }
    Ok(td
        .generics
        .iter()
        .zip(args)
        .map(|(param, arg)| (param.name().to_string(), subst_arg(arg, outer)))
        .collect())
}

fn subst_arg(arg: &GenericArg, scope: &Scope) -> GenericArg {
    match arg {
        GenericArg::Type { ty } => GenericArg::Type {
            ty: Box::new(subst(ty, scope)),
        },
        GenericArg::Const { value } => match scope.get(value) {
            Some(bound @ GenericArg::Const { .. }) => bound.clone(),
            _ => arg.clone(),
        },
    }
}

/// Replace generic names in `ty` with their bindings in `scope`.
pub(super) fn subst(ty: &Type, scope: &Scope) -> Type {
    if scope.is_empty() {
        return ty.clone();
    }
    let boxed = |t: &Type| Box::new(subst(t, scope));
    match ty {
        Type::Generic(name) => match scope.get(name) {
            Some(GenericArg::Type { ty }) => (**ty).clone(),
            _ => ty.clone(),
        },
        Type::Option(t) => Type::Option(boxed(t)),
        Type::COption(t) => Type::COption(boxed(t)),
        Type::Vec(t) => Type::Vec(boxed(t)),
        Type::Set(t) => Type::Set(boxed(t)),
        Type::Array(t, len) => {
            let len = match resolve_len(len, scope) {
                Ok(n) => ArrayLen::Value(n),
                Err(_) => len.clone(),
            };
            Type::Array(boxed(t), len)
        }
        Type::Map(k, v) => Type::Map(boxed(k), boxed(v)),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|t| subst(t, scope)).collect()),
        Type::Defined { name, generics } => Type::Defined {
            name: name.clone(),
            generics: generics.iter().map(|g| subst_arg(g, scope)).collect(),
        },
        _ => ty.clone(),
    }
}

pub(super) fn resolve_len(len: &ArrayLen, scope: &Scope) -> Result<usize, DecodeErrorKind> {
    match len {
        ArrayLen::Value(n) => Ok(*n),
        ArrayLen::Generic(name) => match scope.get(name) {
            Some(GenericArg::Const { value }) => {
                const_len(value).ok_or_else(|| DecodeErrorKind::UnresolvedGeneric(name.clone()))
            }
            _ => Err(DecodeErrorKind::UnresolvedGeneric(name.clone())),
        },
    }
}
