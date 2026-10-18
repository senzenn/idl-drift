//! Why bytes did not decode, with the field path and offset.

use std::fmt;

use idl_drift::model::Type;

use crate::layout::{LayoutError, MAX_DEPTH};

/// Why bytes did not decode. `offset` is relative to the slice passed to the decoder.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeError {
    pub offset: usize,
    /// Field chain, innermost first.
    path: Vec<String>,
    pub kind: DecodeErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DecodeErrorKind {
    UnexpectedEnd {
        needed: usize,
        remaining: usize,
    },
    /// A length read from the data holds more items than the bytes left can hold.
    TooLong {
        len: usize,
        remaining: usize,
    },
    InvalidBool(u8),
    InvalidOptionTag(u128),
    InvalidCOptionTag(u32),
    InvalidEnumTag {
        tag: u64,
        variants: usize,
    },
    InvalidUtf8,
    NaN,
    InvalidShortU16,
    UndefinedType(String),
    UnresolvedGeneric(String),
    Unsupported(String),
    ZeroSizedItems(String),
    TooDeep,
    Layout(LayoutError),
}

impl DecodeError {
    pub(super) fn new(offset: usize, kind: DecodeErrorKind) -> Self {
        Self {
            offset,
            path: Vec::new(),
            kind,
        }
    }

    pub(super) fn within(mut self, segment: impl Into<String>) -> Self {
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
            Self::NaN => f.write_str("float is NaN, which borsh refuses"),
            Self::InvalidShortU16 => f.write_str("shortU16 is not in its shortest form"),
            Self::UndefinedType(name) => write!(f, "type `{name}` is not defined in the IDL"),
            Self::UnresolvedGeneric(name) => write!(
                f,
                "`{}` is an unbound generic or an encoding idl-verify cannot decode",
                Type::generic_kind(name)
            ),
            Self::Unsupported(what) => f.write_str(what),
            Self::ZeroSizedItems(item) => write!(
                f,
                "borsh refuses a collection of `{item}` because that type holds no data; the IDL may lack its fields"
            ),
            Self::TooDeep => write!(f, "types nest deeper than {MAX_DEPTH} levels"),
            Self::Layout(e) => write!(f, "{e}"),
        }
    }
}

/// A short name for `ty` in an error message.
pub(super) fn type_label(ty: &Type) -> String {
    match ty {
        Type::Defined { name, .. } | Type::Generic(name) => name.clone(),
        Type::Tuple(items) if items.is_empty() => "()".to_string(),
        Type::Tuple(_) => "tuple".to_string(),
        Type::Struct(_) => "struct".to_string(),
        _ => "item".to_string(),
    }
}
