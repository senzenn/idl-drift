//! Zero-copy types: fields read at their `repr(C)` offsets, the way the program casts them.

use idl_drift::model::{ArrayLen, Type, TypeDef, TypeDefTy};

use super::generics::Scope;
use super::reader::Reader;
use super::{DecodeError, DecodeErrorKind, Decoded, Decoder, Value};
use crate::layout::{LayoutError, MAX_DEPTH};

impl Decoder<'_> {
    pub(super) fn zero_copy(&self, td: &TypeDef, data: &[u8]) -> Result<Decoded, DecodeError> {
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
