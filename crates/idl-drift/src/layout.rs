//! Sizes and memory layouts computed from the IDL alone.
//!
//! Zero-copy (`bytemuck`, `bytemuckunsafe`) accounts are cast straight from account bytes,
//! so their fields sit at `repr(C)` offsets, with padding that Borsh never writes. Alignment
//! follows SBF, not `x86_64`: `u128` and `i128` align to 8, a pubkey (`[u8; 32]`) to 1, and
//! `packed` aligns every field to 1.

use std::collections::BTreeMap;
use std::fmt;

use crate::model::{
    ArrayLen, EnumFields, Idl, OptionTag, Repr, ReprKind, Type, TypeDef, TypeDefTy,
};

/// Deeper nesting than this is treated as a recursive type.
pub const MAX_DEPTH: usize = 64;

/// The IDL's `types[]`, indexed by name.
#[derive(Debug, Clone)]
pub struct TypeIndex<'a> {
    types: BTreeMap<&'a str, &'a TypeDef>,
}

impl<'a> TypeIndex<'a> {
    #[must_use]
    pub fn new(idl: &'a Idl) -> Self {
        Self {
            types: idl.types.iter().map(|t| (t.name.as_str(), t)).collect(),
        }
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'a TypeDef> {
        self.types.get(name).copied()
    }

    /// `repr(C)` layout of a struct type definition, whatever its `serialization` says.
    ///
    /// # Errors
    ///
    /// When the layout is not pinned down by the IDL: enums, options, vecs, generics,
    /// `repr(Rust)` (except packed with no bool), undefined types, overflowing sizes or
    /// recursive types.
    pub fn struct_layout(&self, td: &TypeDef) -> Result<StructLayout, LayoutError> {
        self.struct_layout_at(td, 0)
    }

    /// Size and alignment of `ty` inside zero-copy memory.
    ///
    /// # Errors
    ///
    /// As [`Self::struct_layout`].
    pub fn size_align(&self, ty: &Type) -> Result<(usize, usize), LayoutError> {
        self.size_align_at(ty, 0)
    }

    /// Every primitive and array in `td`, flattened with its offset under `reading`. Arrays
    /// list their first element too, so a stride change shows up as a moved leaf.
    ///
    /// # Errors
    ///
    /// As [`Self::struct_layout`] for zero-copy, or when a Borsh size varies.
    pub fn leaves(&self, td: &TypeDef, reading: Reading) -> Result<Vec<Leaf>, LayoutError> {
        let mut out = Vec::new();
        self.def_leaves(td, reading, "", 0, 0, &mut out)?;
        Ok(out)
    }

    /// Size of `ty` under `reading`.
    ///
    /// # Errors
    ///
    /// As [`Self::leaves`].
    pub fn reading_size(&self, ty: &Type, reading: Reading) -> Result<usize, LayoutError> {
        self.reading_size_at(ty, reading, 0)
    }

    fn reading_size_at(
        &self,
        ty: &Type,
        reading: Reading,
        depth: usize,
    ) -> Result<usize, LayoutError> {
        match reading {
            Reading::ZeroCopy => Ok(self.size_align_at(ty, depth)?.0),
            Reading::Borsh => self
                .borsh_size_at(ty, depth)?
                .ok_or_else(|| LayoutError::unknown("the Borsh size varies")),
        }
    }

    fn def_leaves(
        &self,
        td: &TypeDef,
        reading: Reading,
        path: &str,
        base: usize,
        depth: usize,
        out: &mut Vec<Leaf>,
    ) -> Result<(), LayoutError> {
        if depth > MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        match &td.ty {
            TypeDefTy::Struct { fields } => {
                let offsets = match reading {
                    Reading::ZeroCopy => self
                        .struct_layout_at(td, depth)?
                        .fields
                        .iter()
                        .map(|f| f.offset)
                        .collect(),
                    Reading::Borsh => {
                        let mut offsets = Vec::with_capacity(fields.len());
                        let mut at = 0usize;
                        for field in fields {
                            offsets.push(at);
                            let size = self
                                .reading_size_at(&field.ty, reading, depth + 1)
                                .map_err(|e| e.within(&field.name))?;
                            at = at.checked_add(size).ok_or(LayoutError::Overflow)?;
                        }
                        offsets
                    }
                };
                for (field, offset) in fields.iter().zip(offsets) {
                    let at = base.checked_add(offset).ok_or(LayoutError::Overflow)?;
                    let child = if path.is_empty() {
                        field.name.clone()
                    } else {
                        format!("{path}.{}", field.name)
                    };
                    self.type_leaves(&field.ty, reading, &child, at, depth + 1, out)
                        .map_err(|e| e.within(&field.name))?;
                }
                Ok(())
            }
            TypeDefTy::Type { alias } => {
                self.type_leaves(alias, reading, path, base, depth + 1, out)
            }
            TypeDefTy::Enum { .. } => {
                let ty = Type::Defined {
                    name: td.name.clone(),
                    generics: Vec::new(),
                };
                let size = self.reading_size_at(&ty, reading, depth)?;
                out.push(Leaf {
                    path: path.to_string(),
                    offset: base,
                    size,
                });
                Ok(())
            }
        }
    }

    fn type_leaves(
        &self,
        ty: &Type,
        reading: Reading,
        path: &str,
        at: usize,
        depth: usize,
        out: &mut Vec<Leaf>,
    ) -> Result<(), LayoutError> {
        if let Type::Defined { name, .. } = ty {
            let td = self.lookup(name)?;
            return self.def_leaves(td, reading, path, at, depth + 1, out);
        }
        let size = self.reading_size_at(ty, reading, depth)?;
        out.push(Leaf {
            path: path.to_string(),
            offset: at,
            size,
        });
        match ty {
            Type::Array(item, _) => {
                self.type_leaves(item, reading, &format!("{path}[0]"), at, depth + 1, out)
            }
            _ => Ok(()),
        }
    }

    /// Borsh size of `ty` when every value has the same size, `None` when it varies.
    ///
    /// # Errors
    ///
    /// On undefined types, overflowing sizes or recursive types.
    pub fn borsh_size(&self, ty: &Type) -> Result<Option<usize>, LayoutError> {
        self.borsh_size_at(ty, 0)
    }

    fn lookup(&self, name: &str) -> Result<&'a TypeDef, LayoutError> {
        self.get(name)
            .ok_or_else(|| LayoutError::UndefinedType(name.to_string()))
    }

    fn struct_layout_at(&self, td: &TypeDef, depth: usize) -> Result<StructLayout, LayoutError> {
        if depth > MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        let TypeDefTy::Struct { fields } = &td.ty else {
            return Err(LayoutError::unknown("only structs have a zero-copy layout"));
        };
        if !td.generics.is_empty() {
            return Err(LayoutError::unknown("generic types have no fixed layout"));
        }
        let repr = layout_repr(td);
        match repr.kind {
            ReprKind::Rust if !repr.packed => {
                return Err(LayoutError::unknown("repr(Rust) may reorder fields"));
            }
            // rustc 1.85 to 1.97 move a `repr(Rust, packed)` field that has a niche. Enums and
            // options have no layout here, so a bool is the only niche left.
            ReprKind::Rust => {
                for field in fields {
                    if self.holds_bool(&field.ty, depth + 1)? {
                        return Err(LayoutError::unknown(
                            "repr(Rust, packed) may reorder a field that holds a bool",
                        )
                        .within(&field.name));
                    }
                }
            }
            ReprKind::Transparent if fields.len() != 1 => {
                return Err(LayoutError::unknown(
                    "repr(transparent) needs exactly one field",
                ));
            }
            ReprKind::Transparent | ReprKind::C => {}
        }
        if repr.packed && repr.align.is_some() {
            return Err(LayoutError::unknown("packed and align conflict"));
        }

        let mut layout = StructLayout::default();
        let mut offset = 0usize;
        let mut align = 1usize;
        for field in fields {
            let (size, natural) = self
                .size_align_at(&field.ty, depth + 1)
                .map_err(|e| e.within(&field.name))?;
            let field_align = if repr.packed { 1 } else { natural };
            let start = round_up(offset, field_align)?;
            if start > offset {
                layout.padding.push(Padding {
                    offset,
                    len: start - offset,
                    before: Some(field.name.clone()),
                });
            }
            layout.fields.push(FieldLayout {
                name: field.name.clone(),
                offset: start,
                size,
                align: field_align,
            });
            offset = start.checked_add(size).ok_or(LayoutError::Overflow)?;
            align = align.max(field_align);
        }
        if let Some(min) = repr.align {
            if !min.is_power_of_two() {
                return Err(LayoutError::unknown("align is not a power of two"));
            }
            align = align.max(min);
        }
        let size = round_up(offset, align)?;
        if size > offset {
            layout.padding.push(Padding {
                offset,
                len: size - offset,
                before: None,
            });
        }
        layout.size = size;
        layout.align = align;
        Ok(layout)
    }

    /// `true` when `ty` holds a bool at any depth. Types without a zero-copy layout give
    /// `false`, so the layout error names them.
    fn holds_bool(&self, ty: &Type, depth: usize) -> Result<bool, LayoutError> {
        if depth > MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        Ok(match ty {
            Type::Bool => true,
            Type::Array(item, _) => self.holds_bool(item, depth + 1)?,
            Type::Defined { name, .. } => match &self.lookup(name)?.ty {
                TypeDefTy::Struct { fields } => {
                    for field in fields {
                        if self.holds_bool(&field.ty, depth + 1)? {
                            return Ok(true);
                        }
                    }
                    false
                }
                TypeDefTy::Type { alias } => self.holds_bool(alias, depth + 1)?,
                TypeDefTy::Enum { .. } => false,
            },
            _ => false,
        })
    }

    fn size_align_at(&self, ty: &Type, depth: usize) -> Result<(usize, usize), LayoutError> {
        if depth > MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        Ok(match ty {
            Type::Bool | Type::U8 | Type::I8 => (1, 1),
            Type::U16 | Type::I16 => (2, 2),
            Type::U32 | Type::I32 | Type::F32 => (4, 4),
            Type::U64 | Type::I64 | Type::F64 => (8, 8),
            // SBF aligns 128-bit integers to 8 (x86_64 uses 16 since Rust 1.77).
            Type::U128 | Type::I128 => (16, 8),
            Type::Pubkey => (32, 1),
            // Rust has no native 256-bit integer; the alignment depends on the crate used.
            Type::U256 | Type::I256 => {
                return Err(LayoutError::unknown(
                    "a 256-bit integer has no fixed zero-copy alignment",
                ));
            }
            Type::Array(item, ArrayLen::Value(len)) => {
                let (size, align) = self.size_align_at(item, depth + 1)?;
                (size.checked_mul(*len).ok_or(LayoutError::Overflow)?, align)
            }
            Type::Array(_, ArrayLen::Generic(name)) => {
                return Err(LayoutError::unknown(format!(
                    "array length is the generic `{name}`"
                )));
            }
            Type::Defined { name, generics } => {
                if !generics.is_empty() {
                    return Err(LayoutError::unknown(format!(
                        "`{name}` is generic and has no fixed layout"
                    )));
                }
                let td = self.lookup(name)?;
                match &td.ty {
                    TypeDefTy::Struct { .. } => {
                        let layout = self.struct_layout_at(td, depth + 1)?;
                        (layout.size, layout.align)
                    }
                    TypeDefTy::Type { alias } => self.size_align_at(alias, depth + 1)?,
                    TypeDefTy::Enum { .. } => {
                        return Err(LayoutError::unknown(format!(
                            "`{name}` is an enum, which has no defined zero-copy layout"
                        )));
                    }
                }
            }
            Type::Option(_) | Type::COption(_) => {
                return Err(LayoutError::unknown("an option has no zero-copy layout"));
            }
            Type::Vec(_) | Type::Bytes | Type::String | Type::Map(..) | Type::Set(_) => {
                return Err(LayoutError::unknown(
                    "a variable-length value has no zero-copy layout",
                ));
            }
            Type::Tuple(_) => {
                return Err(LayoutError::unknown("a tuple has no defined layout"));
            }
            Type::Generic(name) => {
                return Err(LayoutError::unknown(format!(
                    "`{name}` is generic or unsupported"
                )));
            }
            // Only Codama IDLs have these, and Codama types are always Borsh.
            Type::Struct(_)
            | Type::ShortU16
            | Type::BigEndian(_)
            | Type::Counted { .. }
            | Type::CodamaOption { .. } => {
                return Err(LayoutError::unknown(
                    "a Codama encoding has no zero-copy layout",
                ));
            }
        })
    }

    fn borsh_size_at(&self, ty: &Type, depth: usize) -> Result<Option<usize>, LayoutError> {
        if depth > MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        Ok(match ty {
            Type::Bool | Type::U8 | Type::I8 => Some(1),
            Type::U16 | Type::I16 => Some(2),
            Type::U32 | Type::I32 | Type::F32 => Some(4),
            Type::U64 | Type::I64 | Type::F64 => Some(8),
            Type::U128 | Type::I128 => Some(16),
            Type::U256 | Type::I256 | Type::Pubkey => Some(32),
            Type::COption(inner) => self
                .borsh_size_at(inner, depth + 1)?
                .map(|n| n.checked_add(4).ok_or(LayoutError::Overflow))
                .transpose()?,
            Type::Array(item, ArrayLen::Value(len)) => self
                .borsh_size_at(item, depth + 1)?
                .map(|n| n.checked_mul(*len).ok_or(LayoutError::Overflow))
                .transpose()?,
            Type::Tuple(items) => self.sum_sizes(items.iter(), depth)?,
            Type::Struct(fields) => self.sum_sizes(fields.iter().map(|f| &f.ty), depth)?,
            Type::BigEndian(inner)
            | Type::CodamaOption {
                item: inner,
                tag: OptionTag::Zeroable,
            } => self.borsh_size_at(inner, depth + 1)?,
            Type::CodamaOption {
                item,
                tag: OptionTag::Prefix { ty, fixed: true },
            } => self.sum_sizes([&**ty, &**item].into_iter(), depth)?,
            Type::Defined { name, generics } if generics.is_empty() => {
                let td = self.lookup(name)?;
                if !td.generics.is_empty() {
                    return Ok(None);
                }
                match &td.ty {
                    TypeDefTy::Struct { fields } => {
                        self.sum_sizes(fields.iter().map(|f| &f.ty), depth)?
                    }
                    TypeDefTy::Type { alias } => self.borsh_size_at(alias, depth + 1)?,
                    TypeDefTy::Enum { variants, tag } => {
                        let tag_size = match tag {
                            None => Some(1),
                            Some(t) => self.borsh_size_at(t, depth + 1)?,
                        };
                        let Some(tag_size) = tag_size else {
                            return Ok(None);
                        };
                        let mut size = None;
                        for variant in variants {
                            let payload = match &variant.fields {
                                EnumFields::Named(fields) => {
                                    self.sum_sizes(fields.iter().map(|f| &f.ty), depth)?
                                }
                                EnumFields::Tuple(items) => self.sum_sizes(items.iter(), depth)?,
                            };
                            match (payload, size) {
                                (None, _) => return Ok(None),
                                (Some(p), Some(s)) if p != s => return Ok(None),
                                (Some(p), _) => size = Some(p),
                            }
                        }
                        Some(
                            size.unwrap_or(0)
                                .checked_add(tag_size)
                                .ok_or(LayoutError::Overflow)?,
                        )
                    }
                }
            }
            Type::Option(_)
            | Type::Vec(_)
            | Type::Bytes
            | Type::String
            | Type::Map(..)
            | Type::Set(_)
            | Type::Array(_, ArrayLen::Generic(_))
            | Type::Defined { .. }
            | Type::Generic(_)
            | Type::ShortU16
            | Type::Counted { .. }
            | Type::CodamaOption { .. } => None,
        })
    }

    fn sum_sizes<'t>(
        &self,
        types: impl Iterator<Item = &'t Type>,
        depth: usize,
    ) -> Result<Option<usize>, LayoutError> {
        let mut total = 0usize;
        for ty in types {
            let Some(size) = self.borsh_size_at(ty, depth + 1)? else {
                return Ok(None);
            };
            total = total.checked_add(size).ok_or(LayoutError::Overflow)?;
        }
        Ok(Some(total))
    }
}

/// How a parser reads a type's bytes: packed Borsh, or cast as `repr(C)` memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Borsh,
    ZeroCopy,
}

/// A primitive or array at a fixed offset inside a flattened type. `path` is dotted, with
/// `[0]` for an array's first element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub path: String,
    pub offset: usize,
    pub size: usize,
}

/// A struct's `repr(C)` layout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructLayout {
    pub size: usize,
    pub align: usize,
    pub fields: Vec<FieldLayout>,
    /// Implicit padding, in offset order.
    pub padding: Vec<Padding>,
}

impl StructLayout {
    /// Total implicit padding in bytes.
    #[must_use]
    pub fn padding_len(&self) -> usize {
        self.padding.iter().map(|p| p.len).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub align: usize,
}

/// Bytes the compiler inserts to align the next field (or the struct size, when `before` is
/// `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Padding {
    pub offset: usize,
    pub len: usize,
    pub before: Option<String>,
}

impl fmt::Display for Padding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = if self.len == 1 { "byte" } else { "bytes" };
        match &self.before {
            Some(field) => write!(f, "{} {unit} before `{field}`", self.len),
            None => write!(f, "{} {unit} at the end", self.len),
        }
    }
}

/// Why a layout or size could not be computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// The layout depends on something the IDL does not pin down. `path` is the field chain,
    /// innermost first.
    Unknown {
        path: Vec<String>,
        reason: String,
    },
    UndefinedType(String),
    Overflow,
    TooDeep,
}

impl LayoutError {
    fn unknown(reason: impl Into<String>) -> Self {
        Self::Unknown {
            path: Vec::new(),
            reason: reason.into(),
        }
    }

    fn within(mut self, field: &str) -> Self {
        if let Self::Unknown { path, .. } = &mut self {
            path.push(field.to_string());
        }
        self
    }
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown { path, reason } if path.is_empty() => f.write_str(reason),
            Self::Unknown { path, reason } => {
                let path: Vec<&str> = path.iter().rev().map(String::as_str).collect();
                write!(f, "`{}`: {reason}", path.join("."))
            }
            Self::UndefinedType(name) => write!(f, "type `{name}` is not defined in the IDL"),
            Self::Overflow => f.write_str("size overflows"),
            Self::TooDeep => write!(f, "types nest deeper than {MAX_DEPTH} levels"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// The `repr` that sets a struct's zero-copy layout. A type with no `repr` is laid out as
/// `repr(C)`, the default of Anchor's `zero_copy`. The default of `zero_copy(unsafe)` is
/// `repr(Rust, packed)`, but Anchor writes that `repr` into the IDL.
pub(crate) fn layout_repr(td: &TypeDef) -> Repr {
    td.repr.unwrap_or(Repr {
        kind: ReprKind::C,
        packed: false,
        align: None,
    })
}

fn round_up(offset: usize, align: usize) -> Result<usize, LayoutError> {
    offset
        .div_ceil(align)
        .checked_mul(align)
        .ok_or(LayoutError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idl(types: &str) -> Idl {
        Idl::from_json(&format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},"types":{types}}}"#
        ))
        .unwrap()
    }

    fn layout(types: &str, name: &str) -> Result<StructLayout, LayoutError> {
        let idl = idl(types);
        let index = TypeIndex::new(&idl);
        index.struct_layout(index.get(name).unwrap())
    }

    const RECEIPT: &str = r#"[{"name":"R","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
        {"name":"vault","type":"pubkey"},{"name":"bump","type":"u8"},
        {"name":"epoch","type":"u64"},{"name":"flag","type":"u8"}]}}]"#;

    #[test]
    fn repr_c_pads_before_aligned_fields_and_at_the_end() {
        let l = layout(RECEIPT, "R").unwrap();
        assert_eq!((l.size, l.align), (56, 8));
        assert_eq!(l.fields[2].offset, 40);
        let gaps: Vec<String> = l.padding.iter().map(ToString::to_string).collect();
        assert_eq!(gaps, ["7 bytes before `epoch`", "7 bytes at the end"]);
    }

    #[test]
    fn packed_has_no_padding() {
        let l = layout(
            &RECEIPT.replace(r#""kind":"c""#, r#""kind":"c","packed":true"#),
            "R",
        )
        .unwrap();
        assert_eq!((l.size, l.align, l.padding_len()), (42, 1, 0));
    }

    #[test]
    fn u128_and_i128_align_to_8_on_sbf() {
        for ty in ["u128", "i128"] {
            let l = layout(
                &format!(
                    r#"[{{"name":"S","type":{{"kind":"struct","fields":[
                        {{"name":"a","type":"u8"}},{{"name":"b","type":"{ty}"}}]}}}}]"#
                ),
                "S",
            )
            .unwrap();
            assert_eq!((l.fields[1].offset, l.size, l.align), (8, 24, 8), "{ty}");
        }
    }

    #[test]
    fn pubkey_aligns_to_1() {
        let l = layout(
            r#"[{"name":"S","type":{"kind":"struct","fields":[
                {"name":"a","type":"u8"},{"name":"b","type":"pubkey"}]}}]"#,
            "S",
        )
        .unwrap();
        assert_eq!((l.fields[1].offset, l.size, l.align), (1, 33, 1));
    }

    #[test]
    fn align_modifier_rounds_the_size() {
        let l = layout(
            r#"[{"name":"S","repr":{"kind":"c","align":16},"type":{"kind":"struct","fields":[
                {"name":"a","type":"u64"}]}}]"#,
            "S",
        )
        .unwrap();
        assert_eq!((l.size, l.align), (16, 16));
    }

    #[test]
    fn nested_struct_keeps_its_own_padding() {
        let l = layout(
            r#"[{"name":"Outer","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[
                  {"name":"tag","type":"u8"},{"name":"inner","type":{"defined":{"name":"Inner"}}}]}},
                {"name":"Inner","type":{"kind":"struct","fields":[
                  {"name":"a","type":"u8"},{"name":"b","type":"u32"}]}}]"#,
            "Outer",
        )
        .unwrap();
        assert_eq!((l.fields[1].offset, l.size), (1, 9));
    }

    #[test]
    fn unknown_layout_names_the_field() {
        let err = layout(
            r#"[{"name":"S","type":{"kind":"struct","fields":[{"name":"memo","type":{"option":"u64"}}]}}]"#,
            "S",
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "`memo`: an option has no zero-copy layout");
    }

    #[test]
    fn repr_rust_is_unknown() {
        let err = layout(&RECEIPT.replace(r#""kind":"c""#, r#""kind":"rust""#), "R").unwrap_err();
        assert_eq!(err.to_string(), "repr(Rust) may reorder fields");
    }

    #[test]
    fn packed_repr_rust_keeps_order_without_a_niche() {
        let packed = RECEIPT.replace(r#""kind":"c""#, r#""kind":"rust","packed":true"#);
        let l = layout(&packed, "R").unwrap();
        assert_eq!((l.size, l.align, l.padding_len()), (42, 1, 0));
        assert_eq!(l.fields[2].offset, 33);
        let flag = |ty: &str| {
            let fields = packed.replace(r#""type":"u8"}]"#, &format!(r#""type":{ty}}}]"#));
            let e = r#"{"name":"E","type":{"kind":"enum","variants":[{"name":"A"}]}}"#;
            let types = format!("{},{e}]", fields.strip_suffix(']').unwrap());
            layout(&types, "R").unwrap_err().to_string()
        };
        assert_eq!(
            flag(r#""bool""#),
            "`flag`: repr(Rust, packed) may reorder a field that holds a bool"
        );
        assert_eq!(
            flag(r#"{"defined":{"name":"E"}}"#),
            "`flag`: `E` is an enum, which has no defined zero-copy layout"
        );
    }

    #[test]
    fn huge_array_overflows_instead_of_panicking() {
        let err = layout(
            r#"[{"name":"S","type":{"kind":"struct","fields":[
                {"name":"a","type":{"array":["u64",18446744073709551615]}}]}}]"#,
            "S",
        )
        .unwrap_err();
        assert_eq!(err, LayoutError::Overflow);
    }

    #[test]
    fn recursive_type_is_too_deep() {
        let err = layout(
            r#"[{"name":"S","type":{"kind":"struct","fields":[
                {"name":"s","type":{"defined":{"name":"S"}}}]}}]"#,
            "S",
        )
        .unwrap_err();
        assert_eq!(err, LayoutError::TooDeep);
    }

    #[test]
    fn leaves_flatten_nested_structs_and_arrays() {
        let idl = idl(r#"[{"name":"O","type":{"kind":"struct","fields":[
            {"name":"a","type":"u8"},{"name":"i","type":{"array":[{"defined":{"name":"I"}},2]}}]}},
            {"name":"I","type":{"kind":"struct","fields":[{"name":"v","type":"u64"}]}}]"#);
        let index = TypeIndex::new(&idl);
        let o = index.get("O").unwrap();
        let at = |reading| -> Vec<(String, usize)> {
            let leaves = index.leaves(o, reading).unwrap();
            leaves.into_iter().map(|l| (l.path, l.offset)).collect()
        };
        let expect = |i| vec![("a".into(), 0), ("i".into(), i), ("i[0].v".into(), i)];
        assert_eq!(at(Reading::Borsh), expect(1));
        assert_eq!(at(Reading::ZeroCopy), expect(8));
    }

    #[test]
    fn borsh_size_is_none_for_variable_types() {
        let idl = idl(
            r#"[{"name":"E","type":{"kind":"enum","variants":[{"name":"A","fields":["u32"]},{"name":"B","fields":["i32"]}]}},
                {"name":"V","type":{"kind":"enum","variants":[{"name":"A"},{"name":"B","fields":["u8"]}]}}]"#,
        );
        let index = TypeIndex::new(&idl);
        let defined = |name: &str| Type::Defined {
            name: name.into(),
            generics: vec![],
        };
        assert_eq!(index.borsh_size(&defined("E")), Ok(Some(5)));
        assert_eq!(index.borsh_size(&defined("V")), Ok(None));
        assert_eq!(
            index.borsh_size(&Type::COption(Box::new(Type::Pubkey))),
            Ok(Some(36))
        );
        assert_eq!(
            index.borsh_size(&Type::Option(Box::new(Type::U8))),
            Ok(None)
        );
    }
}
