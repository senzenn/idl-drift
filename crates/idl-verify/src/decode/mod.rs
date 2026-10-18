//! Runtime decoder over the IDL model: no generated code, just the IDL and the bytes.
//!
//! Borsh types are read in order. Zero-copy types (`bytemuck`, `bytemuckunsafe`) are read at
//! their `repr(C)` field offsets from [`crate::layout`], the way the program casts them.

mod error;
mod generics;
mod reader;
mod size;
mod zero_copy;

use idl_drift::model::{Count, EnumFields, Field, GenericArg, OptionTag, Type, TypeDef, TypeDefTy};

use error::type_label;
pub use error::{DecodeError, DecodeErrorKind};
use generics::{bind, resolve_len, subst, Scope};
use reader::{repeat, Reader};

use crate::layout::{TypeIndex, MAX_DEPTH};

/// A decoded value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    /// Little-endian bytes; Rust has no native 256-bit integer.
    U256([u8; 32]),
    I256([u8; 32]),
    F32(f32),
    F64(f64),
    Pubkey([u8; 32]),
    Bytes(Vec<u8>),
    String(String),
    Option(Option<Box<Value>>),
    /// Vec, array or set elements.
    List(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Tuple(Vec<Value>),
    Struct(Vec<(String, Value)>),
    /// The variant name and its payload: a `Struct`, or a `Tuple` (empty for a unit variant).
    Enum {
        variant: String,
        value: Box<Value>,
    },
}

/// A decoded value and the number of bytes it occupies.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub value: Value,
    pub len: usize,
}

/// Decodes bytes against one IDL.
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
    types: TypeIndex<'a>,
}

impl<'a> Decoder<'a> {
    #[must_use]
    pub fn new(types: TypeIndex<'a>) -> Self {
        Self { types }
    }

    #[must_use]
    pub fn types(&self) -> &TypeIndex<'a> {
        &self.types
    }

    /// Decode `td` from the start of `data`, zero-copy or Borsh per its `serialization`.
    ///
    /// # Errors
    ///
    /// When the bytes don't fit the type: too short, an invalid tag, bool or UTF-8 string, a
    /// NaN float in Borsh, or a type the IDL does not define well enough to decode.
    pub fn type_def(&self, td: &TypeDef, data: &[u8]) -> Result<Decoded, DecodeError> {
        if td.is_zero_copy() {
            self.zero_copy(td, data)
        } else {
            let mut r = Reader { data, pos: 0 };
            let value = self.def_body(&mut r, td, &Scope::new(), 0)?;
            Ok(Decoded { value, len: r.pos })
        }
    }

    /// Decode `fields` in order as Borsh, the way instruction arguments are encoded.
    ///
    /// # Errors
    ///
    /// As [`Self::type_def`].
    pub fn borsh_fields(&self, fields: &[Field], data: &[u8]) -> Result<Decoded, DecodeError> {
        let mut r = Reader { data, pos: 0 };
        let value = self.fields(&mut r, fields, &Scope::new(), 0)?;
        Ok(Decoded { value, len: r.pos })
    }

    /// Decode one Borsh value of type `ty` from the start of `data`.
    ///
    /// # Errors
    ///
    /// As [`Self::type_def`].
    pub fn borsh(&self, ty: &Type, data: &[u8]) -> Result<Decoded, DecodeError> {
        let mut r = Reader { data, pos: 0 };
        let value = self.value(&mut r, ty, &Scope::new(), 0)?;
        Ok(Decoded { value, len: r.pos })
    }

    fn lookup(&self, name: &str, at: usize) -> Result<&'a TypeDef, DecodeError> {
        self.types
            .get(name)
            .ok_or_else(|| DecodeError::new(at, DecodeErrorKind::UndefinedType(name.to_string())))
    }

    fn fields(
        &self,
        r: &mut Reader<'_>,
        fields: &[Field],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let mut out = Vec::with_capacity(fields.len());
        for field in fields {
            let value = self
                .value(r, &field.ty, scope, depth + 1)
                .map_err(|e| e.within(&field.name))?;
            out.push((field.name.clone(), value));
        }
        Ok(Value::Struct(out))
    }

    fn def_body(
        &self,
        r: &mut Reader<'_>,
        td: &TypeDef,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        match &td.ty {
            TypeDefTy::Struct { fields } => self.fields(r, fields, scope, depth),
            TypeDefTy::Type { alias } => self.value(r, alias, scope, depth + 1),
            TypeDefTy::Enum { variants, tag } => {
                let at = r.pos;
                let tag = match tag {
                    None => u64::from(r.u8()?),
                    Some(ty) => match self.value(r, ty, scope, depth + 1)? {
                        Value::U8(v) => u64::from(v),
                        Value::U16(v) => u64::from(v),
                        Value::U32(v) => u64::from(v),
                        Value::U64(v) => v,
                        _ => {
                            return Err(DecodeError::new(
                                at,
                                DecodeErrorKind::Unsupported("enum tag type".to_string()),
                            ))
                        }
                    },
                };
                let found = variants.iter().enumerate().find(|(i, v)| v.tag(*i) == tag);
                let variant = found.map(|(_, v)| v).ok_or_else(|| {
                    DecodeError::new(
                        at,
                        DecodeErrorKind::InvalidEnumTag {
                            tag,
                            variants: variants.len(),
                        },
                    )
                })?;
                let value = match &variant.fields {
                    EnumFields::Named(fields) => self.fields(r, fields, scope, depth),
                    EnumFields::Tuple(items) => self.tuple(r, items, scope, depth),
                }
                .map_err(|e| e.within(&variant.name))?;
                Ok(Value::Enum {
                    variant: variant.name.clone(),
                    value: Box::new(value),
                })
            }
        }
    }

    fn tuple(
        &self,
        r: &mut Reader<'_>,
        items: &[Type],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let mut out = Vec::with_capacity(items.len());
        for (i, ty) in items.iter().enumerate() {
            out.push(
                self.value(r, ty, scope, depth + 1)
                    .map_err(|e| e.within(i.to_string()))?,
            );
        }
        Ok(Value::Tuple(out))
    }

    fn list(
        &self,
        r: &mut Reader<'_>,
        item: &Type,
        len: Option<usize>,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        repeat(r, len, |r| self.value(r, item, scope, depth + 1)).map(Value::List)
    }

    fn map(
        &self,
        r: &mut Reader<'_>,
        (key, value): (&Type, &Type),
        len: Option<usize>,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        repeat(r, len, |r| {
            let k = self.value(r, key, scope, depth + 1)?;
            let v = self.value(r, value, scope, depth + 1)?;
            Ok((k, v))
        })
        .map(Value::Map)
    }

    /// A Codama collection: the length comes from `count`, not from a Borsh `u32`.
    fn counted(
        &self,
        r: &mut Reader<'_>,
        ty: &Type,
        count: &Count,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let at = r.pos;
        let len = match count {
            Count::Prefix(prefix) => Some(self.prefix(r, prefix, depth)?),
            Count::Remainder => None,
        };
        let fits = |r: &Reader<'_>, min| len.map(|n| r.fits(at, n, min)).transpose();
        Ok(match ty {
            Type::Bytes | Type::String => {
                let len = fits(r, 1)?.unwrap_or_else(|| r.remaining());
                let start = r.pos;
                let bytes = r.take(len)?;
                if matches!(ty, Type::Bytes) {
                    Value::Bytes(bytes.to_vec())
                } else {
                    let s = std::str::from_utf8(bytes)
                        .map_err(|_| DecodeError::new(start, DecodeErrorKind::InvalidUtf8))?;
                    Value::String(s.to_string())
                }
            }
            Type::Vec(item) | Type::Set(item) => {
                let len = fits(r, self.min_size(item, 0))?;
                self.list(r, item, len, scope, depth)?
            }
            Type::Map(key, value) => {
                let min = self
                    .min_size(key, 0)
                    .saturating_add(self.min_size(value, 0));
                let len = fits(r, min)?;
                self.map(r, (key, value), len, scope, depth)?
            }
            _ => {
                return Err(DecodeError::new(
                    at,
                    DecodeErrorKind::Unsupported(format!("a length on `{ty:?}`")),
                ))
            }
        })
    }

    /// A Codama option without Borsh's `u8` tag.
    fn codama_option(
        &self,
        r: &mut Reader<'_>,
        item: &Type,
        tag: &OptionTag,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let present = match tag {
            OptionTag::Prefix { ty, fixed } => {
                let at = r.pos;
                match self.prefix(r, ty, depth)? {
                    0 => {
                        if *fixed {
                            r.take(self.fixed_size(item, scope, r.pos, "fixed option")?)?;
                        }
                        false
                    }
                    1 => true,
                    t => return Err(DecodeError::new(at, DecodeErrorKind::InvalidOptionTag(t))),
                }
            }
            OptionTag::Zeroable => {
                let size = self.fixed_size(item, scope, r.pos, "zeroable option")?;
                let at = r.pos;
                let zero = r.take(size)?.iter().all(|b| *b == 0);
                if !zero {
                    r.pos = at;
                }
                !zero
            }
            OptionTag::Remainder => r.remaining() > 0,
        };
        Ok(Value::Option(if present {
            Some(Box::new(self.value(r, item, scope, depth + 1)?))
        } else {
            None
        }))
    }

    /// Read a length or tag prefix, which can be any integer type.
    fn prefix(&self, r: &mut Reader<'_>, ty: &Type, depth: usize) -> Result<u128, DecodeError> {
        let at = r.pos;
        let n = match self.value(r, ty, &Scope::new(), depth + 1)? {
            Value::U8(v) => Some(u128::from(v)),
            Value::U16(v) => Some(u128::from(v)),
            Value::U32(v) => Some(u128::from(v)),
            Value::U64(v) => Some(u128::from(v)),
            Value::U128(v) => Some(v),
            Value::I8(v) => u128::try_from(v).ok(),
            Value::I16(v) => u128::try_from(v).ok(),
            Value::I32(v) => u128::try_from(v).ok(),
            Value::I64(v) => u128::try_from(v).ok(),
            Value::I128(v) => u128::try_from(v).ok(),
            _ => None,
        };
        n.ok_or_else(|| {
            DecodeError::new(
                at,
                DecodeErrorKind::Unsupported("a prefix that is not a positive integer".to_string()),
            )
        })
    }

    /// The Borsh size of `ty`, for an option that keeps the item's bytes when it is absent.
    fn fixed_size(
        &self,
        ty: &Type,
        scope: &Scope,
        at: usize,
        what: &str,
    ) -> Result<usize, DecodeError> {
        self.types
            .borsh_size(&subst(ty, scope))
            .map_err(|e| DecodeError::new(at, DecodeErrorKind::Layout(e)))?
            .ok_or_else(|| {
                DecodeError::new(
                    at,
                    DecodeErrorKind::Unsupported(format!("{what} of a variable-size type")),
                )
            })
    }

    // One arm per IDL type; splitting it would only scatter the dispatch.
    #[allow(clippy::too_many_lines)]
    fn value(
        &self,
        r: &mut Reader<'_>,
        ty: &Type,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::new(r.pos, DecodeErrorKind::TooDeep));
        }
        Ok(match ty {
            Type::Bool => {
                let at = r.pos;
                match r.u8()? {
                    0 => Value::Bool(false),
                    1 => Value::Bool(true),
                    b => return Err(DecodeError::new(at, DecodeErrorKind::InvalidBool(b))),
                }
            }
            Type::U8 => Value::U8(r.u8()?),
            Type::I8 => Value::I8(i8::from_le_bytes(r.array()?)),
            Type::U16 => Value::U16(u16::from_le_bytes(r.array()?)),
            Type::I16 => Value::I16(i16::from_le_bytes(r.array()?)),
            Type::U32 => Value::U32(u32::from_le_bytes(r.array()?)),
            Type::I32 => Value::I32(i32::from_le_bytes(r.array()?)),
            Type::U64 => Value::U64(u64::from_le_bytes(r.array()?)),
            Type::I64 => Value::I64(i64::from_le_bytes(r.array()?)),
            Type::U128 => Value::U128(u128::from_le_bytes(r.array()?)),
            Type::I128 => Value::I128(i128::from_le_bytes(r.array()?)),
            Type::U256 => Value::U256(r.array()?),
            Type::I256 => Value::I256(r.array()?),
            // Borsh refuses NaN. Zero-copy reads it.
            Type::F32 => {
                let at = r.pos;
                let v = f32::from_le_bytes(r.array()?);
                if v.is_nan() {
                    return Err(DecodeError::new(at, DecodeErrorKind::NaN));
                }
                Value::F32(v)
            }
            Type::F64 => {
                let at = r.pos;
                let v = f64::from_le_bytes(r.array()?);
                if v.is_nan() {
                    return Err(DecodeError::new(at, DecodeErrorKind::NaN));
                }
                Value::F64(v)
            }
            Type::Pubkey => Value::Pubkey(r.array()?),
            Type::Bytes => {
                let len = r.len_prefix(1)?;
                Value::Bytes(r.take(len)?.to_vec())
            }
            Type::String => {
                let len = r.len_prefix(1)?;
                let at = r.pos;
                let bytes = r.take(len)?;
                let s = std::str::from_utf8(bytes)
                    .map_err(|_| DecodeError::new(at, DecodeErrorKind::InvalidUtf8))?;
                Value::String(s.to_string())
            }
            Type::Option(inner) => {
                let at = r.pos;
                match r.u8()? {
                    0 => Value::Option(None),
                    1 => Value::Option(Some(Box::new(self.value(r, inner, scope, depth + 1)?))),
                    t => {
                        return Err(DecodeError::new(
                            at,
                            DecodeErrorKind::InvalidOptionTag(u128::from(t)),
                        ))
                    }
                }
            }
            Type::COption(inner) => {
                let at = r.pos;
                match u32::from_le_bytes(r.array()?) {
                    // The value's bytes are present, zeroed, even when there is none.
                    0 => {
                        r.take(self.fixed_size(inner, scope, r.pos, "coption")?)?;
                        Value::Option(None)
                    }
                    1 => Value::Option(Some(Box::new(self.value(r, inner, scope, depth + 1)?))),
                    t => return Err(DecodeError::new(at, DecodeErrorKind::InvalidCOptionTag(t))),
                }
            }
            Type::Vec(item) | Type::Set(item) => {
                // Borsh refuses before it reads the length, so an empty collection fails too.
                if self.is_zero_sized(item, scope, depth) {
                    return Err(DecodeError::new(
                        r.pos,
                        DecodeErrorKind::ZeroSizedItems(type_label(item)),
                    ));
                }
                let len = r.len_prefix(self.min_size(item, 0))?;
                self.list(r, item, Some(len), scope, depth)?
            }
            Type::Array(item, len) => {
                let at = r.pos;
                let len = resolve_len(len, scope).map_err(|k| DecodeError::new(at, k))?;
                // The length comes from the IDL; check it against the data before allocating.
                // A short array is short data, as for a scalar: TooLong is for read lengths.
                let remaining = r.remaining();
                let min = self.min_size(item, 0);
                let fits = match min {
                    0 => len <= remaining,
                    min => len <= remaining / min,
                };
                if !fits {
                    let kind = match len.checked_mul(min) {
                        Some(needed) if min > 0 => {
                            DecodeErrorKind::UnexpectedEnd { needed, remaining }
                        }
                        _ => DecodeErrorKind::TooLong { len, remaining },
                    };
                    return Err(DecodeError::new(at, kind));
                }
                self.list(r, item, Some(len), scope, depth)?
            }
            Type::Map(key, value) => {
                // Shipstern reads a map as a Vec of key-value structs, so only empty pairs fail.
                if self.is_zero_sized(key, scope, depth) && self.is_zero_sized(value, scope, depth)
                {
                    return Err(DecodeError::new(
                        r.pos,
                        DecodeErrorKind::ZeroSizedItems(format!(
                            "({}, {})",
                            type_label(key),
                            type_label(value)
                        )),
                    ));
                }
                let min = self
                    .min_size(key, 0)
                    .saturating_add(self.min_size(value, 0));
                let len = r.len_prefix(min)?;
                self.map(r, (key, value), Some(len), scope, depth)?
            }
            Type::Tuple(items) => self.tuple(r, items, scope, depth)?,
            Type::Defined { name, generics } => {
                let td = self.lookup(name, r.pos)?;
                let inner = bind(td, generics, scope).map_err(|k| DecodeError::new(r.pos, k))?;
                self.def_body(r, td, &inner, depth + 1)?
            }
            Type::Generic(name) => match scope.get(name) {
                Some(GenericArg::Type { ty }) => self.value(r, ty, &Scope::new(), depth + 1)?,
                _ => {
                    return Err(DecodeError::new(
                        r.pos,
                        DecodeErrorKind::UnresolvedGeneric(name.clone()),
                    ))
                }
            },
            Type::Struct(fields) => self.fields(r, fields, scope, depth)?,
            Type::ShortU16 => Value::U16(r.short_u16()?),
            Type::BigEndian(inner) => r.big_endian(inner)?,
            Type::Counted { ty, count } => self.counted(r, ty, count, scope, depth)?,
            Type::CodamaOption { item, tag } => self.codama_option(r, item, tag, scope, depth)?,
        })
    }
}

#[cfg(test)]
mod tests;
