//! Compare decoded values with an independent decoder's output.
//!
//! The reference file is what `tools/reference/reference.mjs` prints: one entry per sample,
//! in sample order, holding `@coral-xyz/anchor`'s decoded value in a plain JSON form
//! (`u64` and wider as `{"bn": "<decimal>"}`, pubkeys as base58, bytes as number arrays).

use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::{json, Map, Value as Json};

use crate::decode::Value;
use crate::error::Error;
use crate::samples::Samples;

/// Reference decodes, aligned with the samples by index.
#[derive(Debug, Clone)]
pub struct Reference {
    pub(crate) accounts: Vec<Outcome>,
    pub(crate) instructions: Vec<Outcome>,
}

/// One reference decode.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    Ok(Json),
    Failed(String),
}

#[derive(Deserialize)]
struct ReferenceJson {
    #[serde(default)]
    accounts: Vec<EntryJson>,
    #[serde(default)]
    instructions: Vec<EntryJson>,
}

#[derive(Deserialize)]
struct EntryJson {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    signature: Option<String>,
    reference: OutcomeJson,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum OutcomeJson {
    Ok { value: Json },
    Error { error: String },
}

impl Reference {
    /// Parse a reference file and check it lines up with `samples`.
    ///
    /// # Errors
    ///
    /// When the file is not reference JSON, or its entries don't match the samples one to one.
    pub fn from_json(src: &str, samples: &Samples) -> Result<Self, Error> {
        let raw: ReferenceJson = serde_json::from_str(src).map_err(Error::ReferenceJson)?;
        check_aligned(
            "accounts",
            raw.accounts.iter().map(|e| e.address.as_deref()),
            samples.accounts.iter().map(|a| a.address.as_str()),
        )?;
        check_aligned(
            "instructions",
            raw.instructions.iter().map(|e| e.signature.as_deref()),
            samples.instructions.iter().map(|i| i.signature.as_str()),
        )?;
        let outcomes = |entries: Vec<EntryJson>| {
            entries
                .into_iter()
                .map(|e| match e.reference {
                    OutcomeJson::Ok { value } => Outcome::Ok(value),
                    OutcomeJson::Error { error } => Outcome::Failed(error),
                })
                .collect()
        };
        Ok(Self {
            accounts: outcomes(raw.accounts),
            instructions: outcomes(raw.instructions),
        })
    }
}

/// Entries must match the samples one to one; an id, when given, must be the sample's.
fn check_aligned<'a>(
    list: &'static str,
    reference: impl ExactSizeIterator<Item = Option<&'a str>>,
    samples: impl ExactSizeIterator<Item = &'a str>,
) -> Result<(), Error> {
    let (r, s) = (reference.len(), samples.len());
    if r != s {
        return Err(Error::ReferenceMismatch(format!(
            "it has {r} {list}, the samples have {s}"
        )));
    }
    for (index, (id, sample)) in reference.zip(samples).enumerate() {
        if let Some(id) = id.filter(|id| *id != sample) {
            return Err(Error::ReferenceMismatch(format!(
                "{list}[{index}] is {id}, the sample is {sample}"
            )));
        }
    }
    Ok(())
}

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

/// First place where `ours` and `theirs` differ: the path and both values.
///
/// Object keys compare case-insensitively and without underscores, since JavaScript decoders
/// may camelCase field names.
pub(crate) fn first_difference(ours: &Json, theirs: &Json) -> Option<Difference> {
    diff_at(ours, theirs, &mut Vec::new())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Difference {
    pub path: String,
    pub ours: String,
    pub theirs: String,
}

fn diff_at(ours: &Json, theirs: &Json, path: &mut Vec<String>) -> Option<Difference> {
    let differ = |path: &[String]| Difference {
        path: join_path(path),
        ours: short(ours),
        theirs: short(theirs),
    };
    match (ours, theirs) {
        // Big integers are a single value, however they are wrapped.
        (Json::Object(a), _) | (_, Json::Object(a)) if is_big_int(a) => {
            (ours != theirs).then(|| differ(path))
        }
        (Json::Object(a), Json::Object(b)) => {
            if a.len() != b.len() {
                return Some(differ(path));
            }
            for (key, av) in a {
                let Some(bv) = b
                    .iter()
                    .find_map(|(k, v)| (normalize(k) == normalize(key)).then_some(v))
                else {
                    return Some(differ(path));
                };
                path.push(key.clone());
                let found = diff_at(av, bv, path);
                path.pop();
                if found.is_some() {
                    return found;
                }
            }
            None
        }
        (Json::Array(a), Json::Array(b)) => {
            if a.len() != b.len() {
                return Some(differ(path));
            }
            for (i, (av, bv)) in a.iter().zip(b).enumerate() {
                path.push(format!("[{i}]"));
                let found = diff_at(av, bv, path);
                path.pop();
                if found.is_some() {
                    return found;
                }
            }
            None
        }
        (Json::Number(a), Json::Number(b)) if a.as_f64() == b.as_f64() => None,
        (a, b) if a == b => None,
        _ => Some(differ(path)),
    }
}

fn join_path(segments: &[String]) -> String {
    let mut out = String::new();
    for segment in segments {
        if !out.is_empty() && !segment.starts_with('[') {
            out.push('.');
        }
        out.push_str(segment);
    }
    out
}

pub(crate) fn normalize(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_big_int(obj: &Map<String, Json>) -> bool {
    obj.len() == 1 && obj.get("bn").is_some_and(Json::is_string)
}

fn short(value: &Json) -> String {
    const MAX: usize = 60;
    let s = match value {
        Json::Object(obj) if is_big_int(obj) => obj["bn"].as_str().unwrap_or_default().to_string(),
        other => other.to_string(),
    };
    if s.chars().count() <= MAX {
        s
    } else {
        let cut: String = s.chars().take(MAX).collect();
        format!("{cut}...")
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

    #[test]
    fn difference_ignores_key_case_and_finds_the_path() {
        let ours = json!({"total_value": {"bn": "5"}, "items": [1, 2]});
        assert_eq!(
            first_difference(&ours, &json!({"totalValue": {"bn": "5"}, "items": [1, 2]})),
            None
        );
        assert_eq!(
            first_difference(&ours, &json!({"totalValue": {"bn": "5"}, "items": [1, 3]})),
            Some(Difference {
                path: "items[1]".into(),
                ours: "2".into(),
                theirs: "3".into()
            })
        );
    }
}
