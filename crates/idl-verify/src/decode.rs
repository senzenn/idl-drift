//! Runtime decoder over the IDL model: no generated code, just the IDL and the bytes.
//!
//! Borsh types are read in order. Zero-copy types (`bytemuck`, `bytemuckunsafe`) are read at
//! their `repr(C)` field offsets from [`crate::layout`], the way the program casts them.

use std::collections::BTreeMap;
use std::fmt;

use idl_drift::model::{ArrayLen, EnumFields, Field, GenericArg, Type, TypeDef, TypeDefTy};

use crate::layout::{LayoutError, TypeIndex, MAX_DEPTH};

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

/// Why bytes did not decode. `offset` is relative to the slice passed to the decoder.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeError {
    pub offset: usize,
    /// Field chain, innermost first.
    path: Vec<String>,
    pub kind: DecodeErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecodeErrorKind {
    UnexpectedEnd { needed: usize, remaining: usize },
    TooLong { len: usize, remaining: usize },
    InvalidBool(u8),
    InvalidOptionTag(u8),
    InvalidCOptionTag(u32),
    InvalidEnumTag { tag: u64, variants: usize },
    InvalidUtf8,
    UndefinedType(String),
    UnresolvedGeneric(String),
    Unsupported(String),
    TooDeep,
    Layout(LayoutError),
}

impl DecodeError {
    fn new(offset: usize, kind: DecodeErrorKind) -> Self {
        Self {
            offset,
            path: Vec::new(),
            kind,
        }
    }

    fn within(mut self, segment: impl Into<String>) -> Self {
        self.path.push(segment.into());
        self
    }

    /// Shift the offset, for errors in data that follows a discriminator.
    #[must_use]
    pub fn offset_by(mut self, base: usize) -> Self {
        self.offset = self.offset.saturating_add(base);
        self
    }

    /// The field chain, such as `asset.total_value` or `items[3]`.
    #[must_use]
    pub fn path(&self) -> String {
        let mut out = String::new();
        for segment in self.path.iter().rev() {
            if !out.is_empty() && !segment.starts_with('[') {
                out.push('.');
            }
            out.push_str(segment);
        }
        out
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path();
        if path.is_empty() {
            write!(f, "at offset {}: {}", self.offset, self.kind)
        } else {
            write!(f, "`{path}` at offset {}: {}", self.offset, self.kind)
        }
    }
}

impl std::error::Error for DecodeError {}

impl fmt::Display for DecodeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd { needed, remaining } => {
                write!(f, "needs {needed} bytes, {remaining} left")
            }
            Self::TooLong { len, remaining } => {
                write!(f, "length {len} does not fit in the {remaining} bytes left")
            }
            Self::InvalidBool(b) => write!(f, "bool byte is {b}, not 0 or 1"),
            Self::InvalidOptionTag(t) => write!(f, "option tag is {t}, not 0 or 1"),
            Self::InvalidCOptionTag(t) => write!(f, "coption tag is {t}, not 0 or 1"),
            Self::InvalidEnumTag { tag, variants } => {
                write!(f, "enum tag {tag} is out of range ({variants} variants)")
            }
            Self::InvalidUtf8 => f.write_str("string is not valid UTF-8"),
            Self::UndefinedType(name) => write!(f, "type `{name}` is not defined in the IDL"),
            Self::UnresolvedGeneric(name) => write!(
                f,
                "`{name}` is an unbound generic or an encoding idl-verify cannot decode"
            ),
            Self::Unsupported(what) => f.write_str(what),
            Self::TooDeep => write!(f, "types nest deeper than {MAX_DEPTH} levels"),
            Self::Layout(e) => write!(f, "{e}"),
        }
    }
}

/// Generic parameter name to its argument, already resolved in the caller's scope.
type Scope = BTreeMap<String, GenericArg>;

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
    /// When the bytes don't fit the type: too short, an invalid tag, bool or UTF-8 string,
    /// or a type the IDL does not define well enough to decode.
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
        len: usize,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(
                self.value(r, item, scope, depth + 1)
                    .map_err(|e| e.within(format!("[{i}]")))?,
            );
        }
        Ok(Value::List(out))
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
            Type::F32 => Value::F32(f32::from_le_bytes(r.array()?)),
            Type::F64 => Value::F64(f64::from_le_bytes(r.array()?)),
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
                    t => return Err(DecodeError::new(at, DecodeErrorKind::InvalidOptionTag(t))),
                }
            }
            Type::COption(inner) => {
                let at = r.pos;
                match u32::from_le_bytes(r.array()?) {
                    // The value's bytes are present, zeroed, even when there is none.
                    0 => {
                        let size = self
                            .types
                            .borsh_size(&subst(inner, scope))
                            .map_err(|e| DecodeError::new(r.pos, DecodeErrorKind::Layout(e)))?
                            .ok_or_else(|| {
                                DecodeError::new(
                                    r.pos,
                                    DecodeErrorKind::Unsupported(
                                        "coption of a variable-size type".to_string(),
                                    ),
                                )
                            })?;
                        r.take(size)?;
                        Value::Option(None)
                    }
                    1 => Value::Option(Some(Box::new(self.value(r, inner, scope, depth + 1)?))),
                    t => return Err(DecodeError::new(at, DecodeErrorKind::InvalidCOptionTag(t))),
                }
            }
            Type::Vec(item) | Type::Set(item) => {
                let len = r.len_prefix(self.min_size(item, 0))?;
                self.list(r, item, len, scope, depth)?
            }
            Type::Array(item, len) => {
                let at = r.pos;
                let len = resolve_len(len, scope).map_err(|k| DecodeError::new(at, k))?;
                // The length comes from the IDL; check it against the data before allocating.
                let remaining = r.remaining();
                let fits = match self.min_size(item, 0) {
                    0 => len <= remaining,
                    min => len <= remaining / min,
                };
                if !fits {
                    return Err(DecodeError::new(
                        at,
                        DecodeErrorKind::TooLong { len, remaining },
                    ));
                }
                self.list(r, item, len, scope, depth)?
            }
            Type::Map(key, value) => {
                let min = self
                    .min_size(key, 0)
                    .saturating_add(self.min_size(value, 0));
                let len = r.len_prefix(min)?;
                let mut out = Vec::with_capacity(len);
                for i in 0..len {
                    let k = self
                        .value(r, key, scope, depth + 1)
                        .map_err(|e| e.within(format!("[{i}]")))?;
                    let v = self
                        .value(r, value, scope, depth + 1)
                        .map_err(|e| e.within(format!("[{i}]")))?;
                    out.push((k, v));
                }
                Value::Map(out)
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
        })
    }

    /// Smallest Borsh encoding of `ty`, used to reject length prefixes the data cannot hold.
    fn min_size(&self, ty: &Type, depth: usize) -> usize {
        if depth > MAX_DEPTH {
            return 0;
        }
        match ty {
            Type::Bool | Type::U8 | Type::I8 | Type::Option(_) => 1,
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
            Type::Defined { name, .. } => match self.types.get(name).map(|td| &td.ty) {
                Some(TypeDefTy::Struct { fields }) => {
                    self.min_sum(fields.iter().map(|f| &f.ty), depth)
                }
                Some(TypeDefTy::Type { alias }) => self.min_size(alias, depth + 1),
                Some(TypeDefTy::Enum { tag: None, .. }) => 1,
                Some(TypeDefTy::Enum { tag: Some(tag), .. }) => self.min_size(tag, depth + 1),
                None => 0,
            },
            Type::Array(_, ArrayLen::Generic(_)) | Type::Generic(_) => 0,
        }
    }

    fn min_sum<'t>(&self, types: impl Iterator<Item = &'t Type>, depth: usize) -> usize {
        types.fold(0, |acc, ty| {
            acc.saturating_add(self.min_size(ty, depth + 1))
        })
    }

    fn zero_copy(&self, td: &TypeDef, data: &[u8]) -> Result<Decoded, DecodeError> {
        let layout = self
            .types
            .struct_layout(td)
            .map_err(|e| DecodeError::new(0, DecodeErrorKind::Layout(e)))?;
        let value = self.zc_struct(td, data, 0, 0)?;
        if data.len() < layout.size {
            // Every field fit, so only trailing padding is missing.
            return Err(DecodeError::new(
                data.len(),
                DecodeErrorKind::UnexpectedEnd {
                    needed: layout.size - data.len(),
                    remaining: 0,
                },
            ));
        }
        Ok(Decoded {
            value,
            len: layout.size,
        })
    }

    fn zc_struct(
        &self,
        td: &TypeDef,
        data: &[u8],
        base: usize,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        let layout_err = |e| DecodeError::new(base, DecodeErrorKind::Layout(e));
        let layout = self.types.struct_layout(td).map_err(layout_err)?;
        let TypeDefTy::Struct { fields } = &td.ty else {
            return Err(layout_err(LayoutError::Unknown {
                path: Vec::new(),
                reason: "only structs have a zero-copy layout".to_string(),
            }));
        };
        let mut out = Vec::with_capacity(fields.len());
        for (field, at) in fields.iter().zip(&layout.fields) {
            let offset = base
                .checked_add(at.offset)
                .ok_or_else(|| layout_err(LayoutError::Overflow))?;
            let value = self
                .zc_value(&field.ty, data, offset, depth + 1)
                .map_err(|e| e.within(&field.name))?;
            out.push((field.name.clone(), value));
        }
        Ok(Value::Struct(out))
    }

    fn zc_value(
        &self,
        ty: &Type,
        data: &[u8],
        at: usize,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::new(at, DecodeErrorKind::TooDeep));
        }
        let layout_err = |e| DecodeError::new(at, DecodeErrorKind::Layout(e));
        match ty {
            Type::Defined { name, .. } => {
                let td = self.lookup(name, at)?;
                match &td.ty {
                    TypeDefTy::Type { alias } => self.zc_value(alias, data, at, depth + 1),
                    _ => self.zc_struct(td, data, at, depth + 1),
                }
            }
            Type::Array(item, ArrayLen::Value(len)) => {
                let (size, _) = self.types.size_align(item).map_err(layout_err)?;
                let remaining = data.len().saturating_sub(at);
                let fits = match size {
                    0 => *len <= remaining,
                    size => size
                        .checked_mul(*len)
                        .is_some_and(|total| total <= remaining),
                };
                if !fits {
                    return Err(DecodeError::new(
                        at,
                        DecodeErrorKind::TooLong {
                            len: *len,
                            remaining,
                        },
                    ));
                }
                let mut out = Vec::with_capacity(*len);
                for i in 0..*len {
                    let offset = size
                        .checked_mul(i)
                        .and_then(|o| o.checked_add(at))
                        .ok_or_else(|| layout_err(LayoutError::Overflow))?;
                    out.push(
                        self.zc_value(item, data, offset, depth + 1)
                            .map_err(|e| e.within(format!("[{i}]")))?,
                    );
                }
                Ok(Value::List(out))
            }
            // Scalars and pubkeys read the same bytes as Borsh, just at a fixed offset.
            _ => {
                let mut r = Reader { data, pos: at };
                self.value(&mut r, ty, &Scope::new(), depth)
            }
        }
    }
}

/// Bind `td`'s generic parameters to `args`, resolving the args in the caller's `outer` scope.
fn bind(td: &TypeDef, args: &[GenericArg], outer: &Scope) -> Result<Scope, DecodeErrorKind> {
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
fn subst(ty: &Type, scope: &Scope) -> Type {
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

fn resolve_len(len: &ArrayLen, scope: &Scope) -> Result<usize, DecodeErrorKind> {
    match len {
        ArrayLen::Value(n) => Ok(*n),
        ArrayLen::Generic(name) => match scope.get(name) {
            Some(GenericArg::Const { value }) => value
                .parse()
                .map_err(|_| DecodeErrorKind::UnresolvedGeneric(name.clone())),
            _ => Err(DecodeErrorKind::UnresolvedGeneric(name.clone())),
        },
    }
}

struct Reader<'d> {
    data: &'d [u8],
    pos: usize,
}

impl<'d> Reader<'d> {
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'d [u8], DecodeError> {
        let end = self.pos.checked_add(n);
        let Some(bytes) = end.and_then(|end| self.data.get(self.pos..end)) else {
            return Err(DecodeError::new(
                self.pos,
                DecodeErrorKind::UnexpectedEnd {
                    needed: n,
                    remaining: self.remaining(),
                },
            ));
        };
        self.pos += n;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let at = self.pos;
        let bytes = self.take(N)?;
        <[u8; N]>::try_from(bytes).map_err(|_| {
            DecodeError::new(
                at,
                DecodeErrorKind::UnexpectedEnd {
                    needed: N,
                    remaining: bytes.len(),
                },
            )
        })
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    /// Read a Borsh `u32` length and reject one the remaining bytes cannot hold.
    fn len_prefix(&mut self, min_item: usize) -> Result<usize, DecodeError> {
        let at = self.pos;
        let raw = u32::from_le_bytes(self.array()?);
        let remaining = self.remaining();
        let len = usize::try_from(raw).unwrap_or(usize::MAX);
        let fits = match min_item {
            0 => len <= remaining,
            n => len <= remaining / n,
        };
        if fits {
            Ok(len)
        } else {
            Err(DecodeError::new(
                at,
                DecodeErrorKind::TooLong { len, remaining },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use idl_drift::model::Idl;

    fn idl(types: &str) -> Idl {
        Idl::from_json(&format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},"types":{types}}}"#
        ))
        .unwrap()
    }

    fn decode(idl: &Idl, name: &str, data: &[u8]) -> Result<Decoded, DecodeError> {
        let index = TypeIndex::new(idl);
        let td = index.get(name).unwrap();
        Decoder::new(index).type_def(td, data)
    }

    #[test]
    fn borsh_struct_reports_bytes_used() {
        let idl = idl(r#"[{"name":"S","type":{"kind":"struct","fields":[
                {"name":"a","type":"u16"},{"name":"s","type":"string"},{"name":"o","type":{"option":"u8"}}]}}]"#);
        let d = decode(&idl, "S", &[1, 0, 2, 0, 0, 0, b'h', b'i', 1, 7, 99]).unwrap();
        assert_eq!(d.len, 10);
        assert_eq!(
            d.value,
            Value::Struct(vec![
                ("a".into(), Value::U16(1)),
                ("s".into(), Value::String("hi".into())),
                ("o".into(), Value::Option(Some(Box::new(Value::U8(7))))),
            ])
        );
    }

    #[test]
    fn zero_copy_reads_past_padding() {
        let idl = idl(
            r#"[{"name":"Z","serialization":"bytemuckunsafe","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
                {"name":"a","type":"u8"},{"name":"b","type":"u64"}]}}]"#,
        );
        let mut data = vec![5, 0xee, 0xee, 0xee, 0xee, 0xee, 0xee, 0xee];
        data.extend(9u64.to_le_bytes());
        let d = decode(&idl, "Z", &data).unwrap();
        assert_eq!(d.len, 16);
        assert_eq!(
            d.value,
            Value::Struct(vec![
                ("a".into(), Value::U8(5)),
                ("b".into(), Value::U64(9))
            ])
        );
    }

    #[test]
    fn short_data_names_the_field() {
        let idl = idl(r#"[{"name":"S","type":{"kind":"struct","fields":[
                {"name":"inner","type":{"defined":{"name":"I"}}}]}},
                {"name":"I","type":{"kind":"struct","fields":[{"name":"v","type":"u64"}]}}]"#);
        let err = decode(&idl, "S", &[1, 2, 3]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "`inner.v` at offset 0: needs 8 bytes, 3 left"
        );
    }

    #[test]
    fn huge_vec_length_is_rejected_without_allocating() {
        let idl = idl(
            r#"[{"name":"S","type":{"kind":"struct","fields":[{"name":"v","type":{"vec":"u64"}}]}}]"#,
        );
        let err = decode(&idl, "S", &[0xff, 0xff, 0xff, 0xff, 0, 0]).unwrap_err();
        assert_eq!(
            err.kind,
            DecodeErrorKind::TooLong {
                len: 0xffff_ffff,
                remaining: 2
            }
        );
    }

    #[test]
    fn enum_reads_declared_tag_width_and_values() {
        // Codama `size: u32` and an explicit variant discriminator, as converted.
        let idl = idl(
            r#"[{"name":"E","type":{"kind":"enum","tag":"u32","variants":[{"name":"A"},{"name":"B","discriminator":5}]}}]"#,
        );
        let d = decode(&idl, "E", &[5, 0, 0, 0]).unwrap();
        assert_eq!(d.len, 4);
        assert_eq!(
            d.value,
            Value::Enum {
                variant: "B".into(),
                value: Box::new(Value::Tuple(vec![]))
            }
        );
        let err = decode(&idl, "E", &[1, 0, 0, 0]).unwrap_err();
        assert_eq!(
            err.kind,
            DecodeErrorKind::InvalidEnumTag {
                tag: 1,
                variants: 2
            }
        );
    }

    #[test]
    fn enum_tag_out_of_range() {
        let idl = idl(
            r#"[{"name":"E","type":{"kind":"enum","variants":[{"name":"A"},{"name":"B","fields":["u8"]}]}}]"#,
        );
        assert_eq!(
            decode(&idl, "E", &[1, 4]).unwrap().value,
            Value::Enum {
                variant: "B".into(),
                value: Box::new(Value::Tuple(vec![Value::U8(4)]))
            }
        );
        let err = decode(&idl, "E", &[2]).unwrap_err();
        assert_eq!(
            err.kind,
            DecodeErrorKind::InvalidEnumTag {
                tag: 2,
                variants: 2
            }
        );
    }

    #[test]
    fn generic_type_and_const_arguments_resolve() {
        let idl = idl(r#"[{"name":"Outer","type":{"kind":"struct","fields":[
                  {"name":"w","type":{"defined":{"name":"Buf","generics":[
                    {"kind":"type","type":"u16"},{"kind":"const","value":"2"}]}}}]}},
                {"name":"Buf","generics":[{"kind":"type","name":"T"},{"kind":"const","name":"N","type":"usize"}],
                 "type":{"kind":"struct","fields":[{"name":"v","type":{"array":[{"generic":"T"},{"generic":"N"}]}}]}}]"#);
        let d = decode(&idl, "Outer", &[1, 0, 2, 0]).unwrap();
        assert_eq!(d.len, 4);
        let Value::Struct(outer) = d.value else {
            panic!()
        };
        assert_eq!(
            outer[0].1,
            Value::Struct(vec![(
                "v".into(),
                Value::List(vec![Value::U16(1), Value::U16(2)])
            )])
        );
    }

    #[test]
    fn u256_reads_32_bytes() {
        let idl = idl("[]");
        let decoder = Decoder::new(TypeIndex::new(&idl));
        let mut data = vec![7u8; 32];
        data.push(1);
        let d = decoder.borsh(&Type::I256, &data).unwrap();
        assert_eq!((d.value, d.len), (Value::I256([7; 32]), 32));
    }

    #[test]
    fn huge_array_length_is_an_error_not_a_panic() {
        let idl = idl(
            r#"[{"name":"S","type":{"kind":"struct","fields":[{"name":"v","type":{"array":["u8",4000000000000]}}]}},
                {"name":"Z","serialization":"bytemuck","type":{"kind":"struct","fields":[{"name":"v","type":{"array":["u8",4000000000000]}}]}}]"#,
        );
        for name in ["S", "Z"] {
            let err = decode(&idl, name, &[1, 2]).unwrap_err();
            assert!(
                matches!(err.kind, DecodeErrorKind::TooLong { .. }),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn coption_none_still_holds_the_value_bytes() {
        let idl = idl("[]");
        let decoder = Decoder::new(TypeIndex::new(&idl));
        let mut data = vec![0, 0, 0, 0];
        data.extend([0u8; 8]);
        let d = decoder
            .borsh(&Type::COption(Box::new(Type::U64)), &data)
            .unwrap();
        assert_eq!((d.value, d.len), (Value::Option(None), 12));
    }
}
