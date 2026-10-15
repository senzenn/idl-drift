//! Short text forms for messages, such as a hex prefix or a byte count.

use std::fmt::Write as _;

use crate::report::Range;

/// The first 8 bytes of `data` in hex, such as `0xe445a52e51cb9a1d`.
pub(crate) fn prefix_hex(data: &[u8]) -> String {
    if data.is_empty() {
        return "empty data".to_string();
    }
    let mut out = String::from("0x");
    for b in data.iter().take(8) {
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub(crate) fn bytes(n: usize) -> String {
    format!("{n} {}", plural(n, "byte", "bytes"))
}

/// `1 byte` for one value, `2-5 bytes` for a range.
pub(crate) fn byte_range(r: Range) -> String {
    if r.min == r.max {
        bytes(r.min)
    } else {
        format!("{r} bytes")
    }
}

pub(crate) fn plural<'s>(n: usize, one: &'s str, many: &'s str) -> &'s str {
    if n == 1 {
        one
    } else {
        many
    }
}

pub(crate) fn of_samples(affected: usize, total: usize) -> String {
    format!(
        "{affected} of {total} {}",
        plural(total, "sample", "samples")
    )
}
