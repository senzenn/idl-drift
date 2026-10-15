//! Sizes the decoder checks before it reads: the smallest encoding of a type, and types
//! that hold no data.

use idl_drift::model::{ArrayLen, Count, EnumFields, GenericArg, OptionTag, Type, TypeDefTy};

use super::generics::{bind, Scope};
use super::Decoder;
use crate::layout::MAX_DEPTH;

impl Decoder<'_> {
    /// `true` when Rust stores `ty` in zero bytes. Borsh refuses a Vec of such items. A fixed
    /// array is never zero-sized here: Shipstern stores it as a `Vec`.
    pub(super) fn is_zero_sized(&self, ty: &Type, scope: &Scope, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        match ty {
            Type::Tuple(items) => self.all_zero_sized(items.iter(), scope, depth),
            // A Codama struct type node, written inline instead of as a defined type.
            Type::Struct(fields) => self.all_zero_sized(fields.iter().map(|f| &f.ty), scope, depth),
            Type::Generic(name) => match scope.get(name) {
                Some(GenericArg::Type { ty }) => self.is_zero_sized(ty, &Scope::new(), depth + 1),
                _ => false,
            },
            Type::Defined { name, generics } => {
                let Some(td) = self.types.get(name) else {
                    return false;
                };
                let Ok(inner) = bind(td, generics, scope) else {
                    return false;
                };
                match &td.ty {
                    TypeDefTy::Struct { fields } => {
                        self.all_zero_sized(fields.iter().map(|f| &f.ty), &inner, depth)
                    }
                    TypeDefTy::Type { alias } => self.is_zero_sized(alias, &inner, depth + 1),
                    // A Rust enum with one variant stores no tag.
                    TypeDefTy::Enum {
                        variants,
                        tag: None,
                    } => match variants.as_slice() {
                        [only] => match &only.fields {
                            EnumFields::Named(fields) => {
                                self.all_zero_sized(fields.iter().map(|f| &f.ty), &inner, depth)
                            }
                            EnumFields::Tuple(items) => {
                                self.all_zero_sized(items.iter(), &inner, depth)
                            }
                        },
                        _ => false,
                    },
                    TypeDefTy::Enum { .. } => false,
                }
            }
            _ => false,
        }
    }

    fn all_zero_sized<'t>(
        &self,
        mut types: impl Iterator<Item = &'t Type>,
        scope: &Scope,
        depth: usize,
    ) -> bool {
        types.all(|ty| self.is_zero_sized(ty, scope, depth + 1))
    }

    /// Smallest Borsh encoding of `ty`, used to reject length prefixes the data cannot hold.
    pub(super) fn min_size(&self, ty: &Type, depth: usize) -> usize {
        if depth > MAX_DEPTH {
            return 0;
        }
        match ty {
            Type::Bool | Type::U8 | Type::I8 | Type::Option(_) | Type::ShortU16 => 1,
            Type::U16 | Type::I16 => 2,
            Type::U32
            | Type::I32
            | Type::F32
            | Type::Bytes
            | Type::String
            | Type::Vec(_)
            | Type::Map(..)
            | Type::Set(_) => 4,
            Type::U64 | Type::I64 | Type::F64 => 8,
            Type::U128 | Type::I128 => 16,
            Type::U256 | Type::I256 | Type::Pubkey => 32,
            Type::COption(inner) => self.min_size(inner, depth + 1).saturating_add(4),
            Type::Array(item, ArrayLen::Value(len)) => {
                self.min_size(item, depth + 1).saturating_mul(*len)
            }
            Type::Tuple(items) => self.min_sum(items.iter(), depth),
            Type::Struct(fields) => self.min_sum(fields.iter().map(|f| &f.ty), depth),
            Type::BigEndian(inner)
            | Type::Counted {
                count: Count::Prefix(inner),
                ..
            }
            | Type::CodamaOption {
                item: inner,
                tag: OptionTag::Zeroable,
            }
            | Type::CodamaOption {
                tag:
                    OptionTag::Prefix {
                        ty: inner,
                        fixed: false,
                    },
                ..
            } => self.min_size(inner, depth + 1),
            Type::CodamaOption {
                item,
                tag: OptionTag::Prefix { ty, fixed: true },
            } => self.min_sum([&**ty, &**item].into_iter(), depth),
            Type::Defined { name, .. } => match self.types.get(name).map(|td| &td.ty) {
                Some(TypeDefTy::Struct { fields }) => {
                    self.min_sum(fields.iter().map(|f| &f.ty), depth)
                }
                Some(TypeDefTy::Type { alias }) => self.min_size(alias, depth + 1),
                Some(TypeDefTy::Enum { tag: None, .. }) => 1,
                Some(TypeDefTy::Enum { tag: Some(tag), .. }) => self.min_size(tag, depth + 1),
                None => 0,
            },
            Type::Array(_, ArrayLen::Generic(_))
            | Type::Generic(_)
            | Type::Counted {
                count: Count::Remainder,
                ..
            }
            | Type::CodamaOption {
                tag: OptionTag::Remainder,
                ..
            } => 0,
        }
    }

    fn min_sum<'t>(&self, types: impl Iterator<Item = &'t Type>, depth: usize) -> usize {
        types.fold(0, |acc, ty| {
            acc.saturating_add(self.min_size(ty, depth + 1))
        })
    }
}
