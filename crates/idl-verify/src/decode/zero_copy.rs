//! Zero-copy types: fields read at their `repr(C)` offsets, the way the program casts them.

use std::rc::Rc;

use idl_drift::model::{ArrayLen, Type, TypeDef, TypeDefTy};

use super::generics::Scope;
use super::reader::Reader;
use super::{Call, DecodeError, DecodeErrorKind, Decoded, Value};
use crate::layout::{LayoutError, StructLayout};

impl Call<'_, '_> {
    pub(super) fn zero_copy(&self, td: &TypeDef, data: &[u8]) -> Result<Decoded, DecodeError> {
        let value = self.zc_struct(td, data, 0, 0)?;
        let layout = self.layout(td, 0)?;
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
        let layout_err = |e| DecodeError::layout(base, e);
        let layout = self.layout(td, base)?;
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

    /// The layout of `td`, walked only for its first value in this call.
    fn layout(&self, td: &TypeDef, at: usize) -> Result<Rc<StructLayout>, DecodeError> {
        let key = std::ptr::from_ref(td);
        if let Some(layout) = self.layouts.borrow().get(&key) {
            return Ok(Rc::clone(layout));
        }
        let layout = self
            .types
            .struct_layout_within(td, &self.work)
            .map_err(|e| DecodeError::layout(at, e))?;
        let layout = Rc::new(layout);
        self.layouts.borrow_mut().insert(key, Rc::clone(&layout));
        Ok(layout)
    }

    fn zc_value(
        &self,
        ty: &Type,
        data: &[u8],
        at: usize,
        depth: usize,
    ) -> Result<Value, DecodeError> {
        self.step(at, depth)?;
        let layout_err = |e| DecodeError::layout(at, e);
        match ty {
            Type::Defined { name, .. } => {
                let td = self.lookup(name, at)?;
                match &td.ty {
                    TypeDefTy::Type { alias } => self.zc_value(alias, data, at, depth + 1),
                    _ => self.zc_struct(td, data, at, depth + 1),
                }
            }
            Type::Array(item, ArrayLen::Value(len)) => {
                // A struct item takes its size from the cache, so each array does not walk it again.
                let cached = match &**item {
                    Type::Defined { name, generics } if generics.is_empty() => self
                        .types
                        .get(name)
                        .filter(|td| matches!(td.ty, TypeDefTy::Struct { .. })),
                    _ => None,
                };
                let size = match cached {
                    Some(td) => self.layout(td, at)?.size,
                    None => {
                        self.types
                            .size_align_within(item, &self.work)
                            .map_err(layout_err)?
                            .0
                    }
                };
                let remaining = data.len().saturating_sub(at);
                let total = size.checked_mul(*len);
                let fits = match size {
                    0 => *len <= remaining,
                    _ => total.is_some_and(|total| total <= remaining),
                };
                if !fits {
                    // As in Borsh: a short array is short data, not a length read from it.
                    let kind = match total {
                        Some(needed) if size > 0 => {
                            DecodeErrorKind::UnexpectedEnd { needed, remaining }
                        }
                        _ => DecodeErrorKind::TooLong {
                            len: *len,
                            remaining,
                        },
                    };
                    return Err(DecodeError::new(at, kind));
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
            // bytemuck casts any bit pattern, so a NaN that Borsh refuses is valid here.
            Type::F32 => Reader { data, pos: at }
                .array()
                .map(|b| Value::F32(f32::from_le_bytes(b))),
            Type::F64 => Reader { data, pos: at }
                .array()
                .map(|b| Value::F64(f64::from_le_bytes(b))),
            // Scalars and pubkeys read the same bytes as Borsh, just at a fixed offset.
            _ => {
                let mut r = Reader { data, pos: at };
                self.value(&mut r, ty, &Scope::new(), depth)
            }
        }
    }
}
