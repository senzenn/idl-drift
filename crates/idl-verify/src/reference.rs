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
    /// `@coral-xyz/anchor` reads zero-copy types as Borsh, so padded types are skipped.
    /// A generated parser under test reads the padding, so it is compared.
    pub(crate) honors_padding: bool,
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
            honors_padding: false,
        })
    }

    /// Outcomes from a decoder under test, already aligned with the samples.
    pub(crate) fn from_decoder(accounts: Vec<Outcome>, instructions: Vec<Outcome>) -> Self {
        Self {
            accounts,
            instructions,
            honors_padding: true,
        }
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
    if let Shape::Compared(result) = shape_rule(ours, theirs, path) {
        return result;
    }
    match (ours, theirs) {
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

/// Whether a shape rule applies, and if so its result.
enum Shape {
    NoRule,
    Compared(Option<Difference>),
}

/// Rules for two writings of the same value: wrappers that one decoder adds and the other
/// does not.
fn shape_rule(ours: &Json, theirs: &Json, path: &mut Vec<String>) -> Shape {
    Shape::Compared(match (ours, theirs) {
        // Big integers are a single value, however they are wrapped: `{"bn":"5"}`, 5 or "5".
        (Json::Object(a), _) | (_, Json::Object(a)) if is_big_int(a) => {
            (big_int_text(peel(ours)) != big_int_text(peel(theirs))).then(|| Difference {
                path: join_path(path),
                ours: short(ours),
                theirs: short(theirs),
            })
        }
        // Shipstern wraps every enum in a struct with one `kind` field. Unwrap it when the
        // other side has no such wrapper.
        (Json::Object(a), other) | (other, Json::Object(a))
            if is_kind_wrapper(a) && !other.as_object().is_some_and(is_kind_wrapper) =>
        {
            let (wrapped, bare) = if matches!(ours, Json::Object(o) if is_kind_wrapper(o)) {
                (&a["kind"], theirs)
            } else {
                (ours, &a["kind"])
            };
            diff_at(wrapped, bare, path)
        }
        // A unit variant as `{"Fixed": {}}` on one side and its bare name on the other: the
        // names decide, so the wrapper rules below never peel a variant name away.
        (Json::Object(a), Json::String(name)) | (Json::String(name), Json::Object(a))
            if is_unit_shape(a) =>
        {
            (!is_unit_variant(a, name)).then(|| Difference {
                path: join_path(path),
                ours: short(ours),
                theirs: short(theirs),
            })
        }
        (Json::Object(a), Json::Array(items)) | (Json::Array(items), Json::Object(a))
            if is_indexed(a, items.len()) =>
        {
            for (i, item) in items.iter().enumerate() {
                let (av, bv) = if matches!(ours, Json::Object(_)) {
                    (&a[&i.to_string()], item)
                } else {
                    (item, &a[&i.to_string()])
                };
                path.push(format!("[{i}]"));
                let found = diff_at(av, bv, path);
                path.pop();
                if found.is_some() {
                    return Shape::Compared(found);
                }
            }
            None
        }
        // A one-field wrapper around a value; compare the value.
        (Json::Object(a), other) | (other, Json::Object(a))
            if single_wrapper(a).is_some() && !other.is_object() =>
        {
            let inner = single_wrapper(a).unwrap_or(&Json::Null);
            if matches!(ours, Json::Object(_)) {
                diff_at(inner, theirs, path)
            } else {
                diff_at(ours, inner, path)
            }
        }
        // A struct with no fields: `{}` on one side, its bare name `"OptionBool"` on the other.
        (Json::Object(a), Json::String(name)) | (Json::String(name), Json::Object(a))
            if a.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase()) =>
        {
            None
        }
        // `Debug` prints a unit payload as a tuple around a unit struct: `{"0": "SideLong"}`.
        (Json::Object(a), Json::Object(b))
            if (a.is_empty() && is_unit_struct_tuple(b))
                || (b.is_empty() && is_unit_struct_tuple(a)) =>
        {
            None
        }
        // A value read from Rust `Debug` text keeps its type name: `{"Pool": {..}}` against
        // a plain struct. An enum variant on the other side has that name as a key, so stays.
        (Json::Object(a), Json::Object(b))
            if type_wrapped(a, b).is_some() || type_wrapped(b, a).is_some() =>
        {
            match (type_wrapped(a, b), type_wrapped(b, a)) {
                (Some(inner), _) => diff_at(inner, theirs, path),
                (None, Some(inner)) => diff_at(ours, inner, path),
                (None, None) => None,
            }
        }
        _ => return Shape::NoRule,
    })
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
    // Shipstern names tuple fields `item_0`; idl-verify names them `0`.
    let key = key.strip_prefix("item_").unwrap_or(key);
    key.chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// The value inside one-key wrappers, stopping at a big integer.
fn peel(mut v: &Json) -> &Json {
    while let Json::Object(m) = v {
        if m.len() != 1 || is_big_int(m) {
            break;
        }
        match m.values().next() {
            Some(inner) => v = inner,
            None => break,
        }
    }
    v
}

fn is_unit_struct_tuple(obj: &Map<String, Json>) -> bool {
    obj.len() == 1
        && obj
            .get("0")
            .and_then(Json::as_str)
            .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))
}

fn type_wrapped<'a>(wrapped: &'a Map<String, Json>, other: &Map<String, Json>) -> Option<&'a Json> {
    let (name, inner) = wrapped.iter().next().filter(|_| wrapped.len() == 1)?;
    let is_type = (normalize(name) == "0" || name.starts_with(|c: char| c.is_ascii_uppercase()))
        && inner.is_object();
    // Only an enum, always one key, can carry the name as a key; a struct field cannot block.
    let enum_with_that_variant =
        other.len() == 1 && other.keys().any(|k| normalize(k) == normalize(name));
    (is_type && !enum_with_that_variant).then_some(inner)
}

/// A one-field object around a value: a 1-tuple `{"0": x}`, Shipstern's `{"items": [..]}`,
/// or a `Debug`-read wrapper struct. Against a non-object it holds the same bytes as `x`.
fn single_wrapper(obj: &Map<String, Json>) -> Option<&Json> {
    if obj.len() == 1 {
        obj.values().next()
    } else {
        None
    }
}

fn is_kind_wrapper(obj: &Map<String, Json>) -> bool {
    obj.len() == 1 && obj.contains_key("kind")
}

fn is_big_int(obj: &Map<String, Json>) -> bool {
    obj.len() == 1 && obj.get("bn").is_some_and(Json::is_string)
}

/// The decimal text of a big integer in any of its three spellings.
fn big_int_text(value: &Json) -> Option<String> {
    match value {
        Json::Object(obj) if is_big_int(obj) => obj["bn"].as_str().map(str::to_string),
        Json::Number(n) => Some(n.to_string()),
        Json::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// `{"Fixed": {}}` on one side and `"Fixed"` on the other.
/// One key around an empty payload: how idl-verify writes a unit variant.
fn is_unit_shape(obj: &Map<String, Json>) -> bool {
    obj.len() == 1
        && obj.values().all(|v| match v {
            Json::Object(inner) => inner.is_empty(),
            Json::Array(inner) => inner.is_empty(),
            Json::Null => true,
            _ => false,
        })
}

fn is_unit_variant(obj: &Map<String, Json>, name: &str) -> bool {
    obj.len() == 1
        && obj.iter().any(|(k, v)| {
            normalize(k) == normalize(name)
                && match v {
                    Json::Object(inner) => inner.is_empty(),
                    Json::Array(inner) => inner.is_empty(),
                    Json::Null => true,
                    _ => false,
                }
        })
}

/// A tuple written as `{"0": a, "1": b}` on one side and `[a, b]` on the other.
fn is_indexed(obj: &Map<String, Json>, len: usize) -> bool {
    obj.len() == len && (0..len).all(|i| obj.contains_key(&i.to_string()))
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
    fn serde_shapes_compare_equal_to_the_plain_form() {
        // A generated parser serialized with serde: numbers bare, unit variants as names,
        // tuples as arrays. None of these is a difference.
        let ours = json!({
            "amount": {"bn": "7"}, "kind": {"Fixed": {}}, "pair": {"0": 1, "1": {"bn": "2"}}
        });
        let theirs = json!({ "amount": 7, "kind": "Fixed", "pair": [1, 2] });
        assert_eq!(first_difference(&ours, &theirs), None);
        // Shipstern's enum wrapper and tuple field names.
        let shipstern = json!({
            "amount": 7, "kind": {"kind": "Fixed"}, "pair": {"item_0": 1, "item_1": 2}
        });
        assert_eq!(first_difference(&ours, &shipstern), None);
        // A newtype and a nested-vector wrapper hold the same bytes as their inner value.
        assert_eq!(
            first_difference(&json!({"0": [1, 2]}), &json!([1, 2])),
            None
        );
        assert_eq!(
            first_difference(&json!([[1]]), &json!([{"items": [1]}])),
            None
        );
        assert!(first_difference(&json!({"0": [1, 2]}), &json!([1, 3])).is_some());
        // Shipstern output read from `Debug`: type names stay as wrapper keys.
        let shipstern = json!({"amount": 7, "kind": "Fixed", "pair": {"Pair": {"0": 1, "1": 2}}});
        assert_eq!(first_difference(&ours, &shipstern), None);
        let step = json!({"swap": {"whirlpool": {"aToB": true}}});
        let debug_step = json!({"Step": {"swap": {"Whirlpool": {"a_to_b": true}}}});
        assert_eq!(first_difference(&step, &debug_step), None);
        // Shipstern 0.12 read from `Debug`.
        let side =
            json!({"side": {"long": {}}, "status": {"approved": {"timestamp": {"bn": "5"}}}});
        let debug_side = json!({"side": {"Long": {"0": "SideLong"}},
            "status": {"Approved": {"0": {"ProposalStatusApproved": {"timestamp": 5}}}}});
        assert_eq!(first_difference(&side, &debug_side), None);
        assert_eq!(
            first_difference(&json!({"p": [1, 2]}), &json!({"p": {"positions": [1, 2]}})),
            None
        );
        assert!(
            first_difference(&json!({"p": [1, 2]}), &json!({"p": {"positions": [1, 3]}})).is_some()
        );
        // Shipstern unit variants, unit structs, a type with a same-named field, item_0, wide ints.
        assert_eq!(
            first_difference(
                &json!({"swap": {"obsidian": {}}}),
                &json!({"swap": "Obsidian"})
            ),
            None
        );
        assert_eq!(
            first_difference(&json!({"t": {}}), &json!({"t": "OptionBool"})),
            None
        );
        let duration = json!({"d": {"duration": {"0": [1, 0]}, "durationType": 0}});
        let debug_duration = json!({"d": {"Duration": {"duration": [1, 0], "duration_type": 0}}});
        assert_eq!(first_difference(&duration, &debug_duration), None);
        let exact = json!({"p": {"exactIn": {"0": {"amountIn": {"bn": "5"}, "min": 0}}}});
        let debug_exact = json!({"p": {"ExactIn": {"0": {"item_0": {"ExactInParams": {"amount_in": 5, "min": 0}}}}}});
        assert_eq!(first_difference(&exact, &debug_exact), None);
        let frac = json!({"f": {"0": {"bn": "10"}}});
        assert_eq!(
            first_difference(&frac, &json!({"f": {"0": {"Frac": {"item_0": 10}}}})),
            None
        );
        assert!(first_difference(&frac, &json!({"f": {"0": {"Frac": {"item_0": 11}}}})).is_some());
        assert!(first_difference(
            &json!({"swap": {"obsidian": {}}}),
            &json!({"swap": "Raydium"})
        )
        .is_some());
        // A wrong variant is still a difference.
        let wrong = json!({"swap": {"Raydium": {"a_to_b": true}}});
        assert!(first_difference(&step, &wrong).is_some());
        let wrong = json!({ "amount": 8, "kind": "Fixed", "pair": [1, 2] });
        assert_eq!(
            first_difference(&ours, &wrong).map(|d| d.path),
            Some("amount".to_string())
        );
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
