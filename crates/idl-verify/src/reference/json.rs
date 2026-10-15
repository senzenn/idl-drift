//! Decoded values in the reference decoder's JSON form.

use std::fmt::Write as _;

use serde_json::{json, Map, Value as Json};

use crate::decode::Value;

/// `value` in the reference decoder's JSON form.
pub(crate) fn to_json(value: &Value) -> Json {
    match value {
        Value::Bool(b) => json!(b),
        Value::U8(n) => json!(n),
        Value::U16(n) => json!(n),
        Value::U32(n) => json!(n),
        Value::I8(n) => json!(n),
        Value::I16(n) => json!(n),
        Value::I32(n) => json!(n),
        Value::U64(n) => json!({ "bn": n.to_string() }),
        Value::I64(n) => json!({ "bn": n.to_string() }),
        Value::U128(n) => json!({ "bn": n.to_string() }),
        Value::I128(n) => json!({ "bn": n.to_string() }),
        Value::U256(le) => json!({ "bn": decimal_256(le, false) }),
        Value::I256(le) => json!({ "bn": decimal_256(le, true) }),
        Value::F32(n) => float(f64::from(*n)),
        Value::F64(n) => float(*n),
        Value::Pubkey(key) => json!(bs58::encode(key).into_string()),
        Value::Bytes(bytes) => json!(bytes),
        Value::String(s) => json!(s),
        Value::Option(None) => Json::Null,
        Value::Option(Some(inner)) => to_json(inner),
        Value::List(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Map(entries) => Json::Array(
            entries
                .iter()
                .map(|(k, v)| json!([to_json(k), to_json(v)]))
                .collect(),
        ),
        Value::Tuple(items) => Json::Object(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), to_json(v)))
                .collect(),
        ),
        Value::Struct(fields) => Json::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), to_json(v)))
                .collect(),
        ),
        Value::Enum { variant, value } => {
            let mut obj = Map::new();
            obj.insert(variant.clone(), to_json(value));
            Json::Object(obj)
        }
    }
}

/// Decimal form of a little-endian 256-bit integer, two's complement when `signed`.
fn decimal_256(le: &[u8; 32], signed: bool) -> String {
    const CHUNK: u128 = 10_000_000_000_000_000_000; // 10^19, the largest power of 10 in a u64
    let mut bytes = *le;
    let negative = signed && bytes[31] & 0x80 != 0;
    if negative {
        // Two's complement: invert, then add one.
        let mut carry = true;
        for b in &mut bytes {
            *b = !*b;
            if carry {
                (*b, carry) = b.overflowing_add(1);
            }
        }
    }
    let mut limbs = [0u64; 4];
    for (limb, chunk) in limbs.iter_mut().zip(bytes.chunks_exact(8)) {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        *limb = u64::from_le_bytes(word);
    }
    // Repeated division by 10^19, most significant limb first; each remainder is 19 digits.
    let mut groups = Vec::new();
    while limbs.iter().any(|l| *l != 0) {
        let mut rem: u128 = 0;
        for limb in limbs.iter_mut().rev() {
            let cur = (rem << 64) | u128::from(*limb);
            *limb = u64::try_from(cur / CHUNK).unwrap_or(u64::MAX);
            rem = cur % CHUNK;
        }
        groups.push(rem);
    }
    let mut out = String::from(if negative { "-" } else { "" });
    match groups.split_last() {
        None => out.push('0'),
        Some((first, rest)) => {
            out.push_str(&first.to_string());
            for group in rest.iter().rev() {
                let _ = write!(out, "{group:019}");
            }
        }
    }
    out
}

fn float(n: f64) -> Json {
    if n.is_nan() {
        json!({ "nan": true })
    } else {
        // JSON has no infinity; JavaScript's JSON.stringify writes null.
        serde_json::Number::from_f64(n).map_or(Json::Null, Json::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_form_matches_anchor_plain_output() {
        let value = Value::Struct(vec![
            ("amount".into(), Value::U64(1_208_595_200)),
            ("bump".into(), Value::U8(253)),
            ("mint".into(), Value::Pubkey([0; 32])),
            ("memo".into(), Value::Option(None)),
            (
                "kind".into(),
                Value::Enum {
                    variant: "Fixed".into(),
                    value: Box::new(Value::Tuple(vec![])),
                },
            ),
        ]);
        assert_eq!(
            to_json(&value),
            json!({
                "amount": {"bn": "1208595200"},
                "bump": 253,
                "mint": "11111111111111111111111111111111",
                "memo": null,
                "kind": {"Fixed": {}},
            })
        );
    }

    #[test]
    fn decimal_256_matches_known_values() {
        let from_u128 = |n: u128, fill: u8| {
            let mut le = [fill; 32];
            le[..16].copy_from_slice(&n.to_le_bytes());
            le
        };
        assert_eq!(decimal_256(&[0; 32], false), "0");
        assert_eq!(
            decimal_256(&from_u128(u128::MAX, 0), false),
            u128::MAX.to_string()
        );
        let mut two_128 = [0u8; 32];
        two_128[16] = 1;
        assert_eq!(
            decimal_256(&two_128, false),
            "340282366920938463463374607431768211456"
        );
        assert_eq!(
            decimal_256(&[0xff; 32], false),
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        );
        assert_eq!(decimal_256(&[0xff; 32], true), "-1");
        let mut min = [0u8; 32];
        min[31] = 0x80;
        assert_eq!(
            decimal_256(&min, true),
            "-57896044618658097711785492504343953926634992332820282019728792003956564819968"
        );
        assert_eq!(decimal_256(&from_u128(5, 0), true), "5");
    }
}
