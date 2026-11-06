//! Generic parameters: bind a defined type's generics to the arguments a use site passes.

use std::collections::BTreeMap;

use idl_drift::model::{const_len, ArrayLen, GenericArg, Type, TypeDef};

use super::{Call, DecodeError, DecodeErrorKind};

/// Generic parameter name to its argument, already resolved in the caller's scope.
pub(super) type Scope = BTreeMap<String, GenericArg>;

impl Call<'_, '_> {
    /// Bind `td`'s generic parameters to `args`, resolving the args in the caller's `outer`
    /// scope.
    pub(super) fn bind(
        &self,
        td: &TypeDef,
        args: &[GenericArg],
        outer: &Scope,
        at: usize,
    ) -> Result<Scope, DecodeError> {
        if td.generics.is_empty() {
            return Ok(Scope::new());
        }
        if td.generics.len() != args.len() {
            return Err(DecodeError::new(
                at,
                DecodeErrorKind::Unsupported(format!(
                    "`{}` takes {} generic arguments, the IDL passes {}",
                    td.name,
                    td.generics.len(),
                    args.len()
                )),
            ));
        }
        td.generics
            .iter()
            .zip(args)
            .map(|(param, arg)| Ok((param.name().to_string(), self.subst_arg(arg, outer, at)?)))
            .collect()
    }

    fn subst_arg(
        &self,
        arg: &GenericArg,
        scope: &Scope,
        at: usize,
    ) -> Result<GenericArg, DecodeError> {
        Ok(match arg {
            GenericArg::Type { ty } => GenericArg::Type {
                ty: Box::new(self.subst(ty, scope, at)?),
            },
            GenericArg::Const { value } => {
                self.spend(at)?;
                match scope.get(value) {
                    Some(bound @ GenericArg::Const { .. }) => bound.clone(),
                    _ => arg.clone(),
                }
            }
        })
    }

    /// Replace generic names in `ty` with their bindings in `scope`. Each use of a generic
    /// type copies its arguments, so each type node of the copy spends a node of the work.
    pub(super) fn subst(&self, ty: &Type, scope: &Scope, at: usize) -> Result<Type, DecodeError> {
        self.spend(at)?;
        let boxed = |t: &Type| self.subst(t, scope, at).map(Box::new);
        Ok(match ty {
            Type::Generic(name) => match scope.get(name) {
                Some(GenericArg::Type { ty }) => self.subst(ty, &Scope::new(), at)?,
                _ => ty.clone(),
            },
            Type::Option(t) => Type::Option(boxed(t)?),
            Type::COption(t) => Type::COption(boxed(t)?),
            Type::Vec(t) => Type::Vec(boxed(t)?),
            Type::Set(t) => Type::Set(boxed(t)?),
            Type::Array(t, len) => {
                let len = match resolve_len(len, scope) {
                    Ok(n) => ArrayLen::Value(n),
                    Err(_) => len.clone(),
                };
                Type::Array(boxed(t)?, len)
            }
            Type::Map(k, v) => Type::Map(boxed(k)?, boxed(v)?),
            Type::Tuple(items) => Type::Tuple(
                items
                    .iter()
                    .map(|t| self.subst(t, scope, at))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Defined { name, generics } => Type::Defined {
                name: name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.subst_arg(g, scope, at))
                    .collect::<Result<_, _>>()?,
            },
            _ => {
                self.spend_copy(ty.children(), at)?;
                ty.clone()
            }
        })
    }

    /// Spend a node of the work for each type node under `types`, which a clone copies.
    fn spend_copy(&self, types: Vec<&Type>, at: usize) -> Result<(), DecodeError> {
        types.into_iter().try_for_each(|t| {
            self.spend(at)?;
            self.spend_copy(t.children(), at)
        })
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
