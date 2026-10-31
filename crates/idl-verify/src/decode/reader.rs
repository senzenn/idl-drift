//! A cursor over the data, with the bounds checks every read needs.

use idl_drift::model::Type;

use super::{DecodeError, DecodeErrorKind, Value};

pub(super) struct Reader<'d> {
    pub(super) data: &'d [u8],
    pub(super) pos: usize,
}

impl<'d> Reader<'d> {
    pub(super) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub(super) fn take(&mut self, n: usize) -> Result<&'d [u8], DecodeError> {
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

    pub(super) fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
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

    pub(super) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    /// Read a Borsh `u32` length and reject one the remaining bytes cannot hold.
    pub(super) fn len_prefix(
        &mut self,
        min_item: impl FnOnce() -> usize,
    ) -> Result<usize, DecodeError> {
        let at = self.pos;
        let raw = u32::from_le_bytes(self.array()?);
        self.fits(at, u128::from(raw), min_item)
    }

    /// `len` items of at least `min_item()` bytes each, if the remaining bytes can hold them.
    /// `at` is where the length was read, for the error.
    pub(super) fn fits(
        &self,
        at: usize,
        len: u128,
        min_item: impl FnOnce() -> usize,
    ) -> Result<usize, DecodeError> {
        // Zero items fit in any data, and no item pays for the size walk.
        if len == 0 {
            return Ok(0);
        }
        let remaining = self.remaining();
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        let fits = match min_item() {
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

    /// Solana's compact `u16`, read as Shipstern reads it: 1 to 3 bytes, shortest form only.
    pub(super) fn short_u16(&mut self) -> Result<u16, DecodeError> {
        let at = self.pos;
        let mut value = 0u16;
        for index in 0..3 {
            let byte = self.u8()?;
            if (index == 2 && byte > 3) || (index > 0 && byte == 0) {
                return Err(DecodeError::new(at, DecodeErrorKind::InvalidShortU16));
            }
            value |= u16::from(byte & 0x7f) << (index * 7);
            if byte & 0x80 == 0 {
                break;
            }
        }
        Ok(value)
    }

    pub(super) fn big_endian(&mut self, ty: &Type) -> Result<Value, DecodeError> {
        Ok(match ty {
            Type::U8 => Value::U8(self.u8()?),
            Type::I8 => Value::I8(i8::from_be_bytes(self.array()?)),
            Type::U16 => Value::U16(u16::from_be_bytes(self.array()?)),
            Type::I16 => Value::I16(i16::from_be_bytes(self.array()?)),
            Type::U32 => Value::U32(u32::from_be_bytes(self.array()?)),
            Type::I32 => Value::I32(i32::from_be_bytes(self.array()?)),
            Type::U64 => Value::U64(u64::from_be_bytes(self.array()?)),
            Type::I64 => Value::I64(i64::from_be_bytes(self.array()?)),
            Type::U128 => Value::U128(u128::from_be_bytes(self.array()?)),
            Type::I128 => Value::I128(i128::from_be_bytes(self.array()?)),
            Type::F32 => Value::F32(f32::from_be_bytes(self.array()?)),
            Type::F64 => Value::F64(f64::from_be_bytes(self.array()?)),
            _ => {
                return Err(DecodeError::new(
                    self.pos,
                    DecodeErrorKind::Unsupported(format!("big-endian `{ty:?}`")),
                ))
            }
        })
    }
}

/// Read `len` items, or items up to the end of the data when `len` is `None`.
pub(super) fn repeat<T>(
    r: &mut Reader<'_>,
    len: Option<usize>,
    mut read: impl FnMut(&mut Reader<'_>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let mut out = Vec::with_capacity(len.unwrap_or(0));
    while len.map_or(r.remaining() > 0, |n| out.len() < n) {
        let at = r.pos;
        let item = read(r).map_err(|e| e.within(format!("[{}]", out.len())))?;
        // An item of zero bytes never reaches the end of the data.
        if len.is_none() && r.pos == at {
            return Err(DecodeError::new(
                at,
                DecodeErrorKind::Unsupported(
                    "items of zero bytes cannot fill the rest of the data".to_string(),
                ),
            ));
        }
        out.push(item);
    }
    Ok(out)
}
