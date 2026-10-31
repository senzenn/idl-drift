//! Find where two JSON writings of a value differ, ignoring how each decoder wraps it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value as Json};

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
    if let Shape::Compared(result) = shape_rule(ours, theirs, path) {
        return result;
    }
    match (ours, theirs) {
        (Json::Object(a), Json::Object(b)) => {
            if a.len() != b.len() {
                return Some(difference(path, ours, theirs));
            }
            // Normalize each key once: a scan per key costs two allocations per pair.
            let mut by_key: BTreeMap<String, &Json> = BTreeMap::new();
            for (k, v) in b {
                by_key.entry(normalize(k)).or_insert(v);
            }
            for (key, av) in a {
                let Some(&bv) = by_key.get(&normalize(key)) else {
                    return Some(difference(path, ours, theirs));
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
                return Some(difference(path, ours, theirs));
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
        _ => Some(difference(path, ours, theirs)),
    }
}

/// A public key as base58 text on one side and as its 32 bytes on the other, as a serde derive
/// on `Pubkey` writes it (Carbon's decoders do). `None` when the pair is not of that shape.
fn key_as_bytes(ours: &Json, theirs: &Json) -> Option<bool> {
    let ((Json::String(text), Json::Array(items)) | (Json::Array(items), Json::String(text))) =
        (ours, theirs)
    else {
        return None;
    };
    if items.len() != 32 {
        return None;
    }
    let bytes: Vec<u8> = items
        .iter()
        .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
        .collect::<Option<_>>()?;
    let key = bs58::decode(text)
        .into_vec()
        .ok()
        .filter(|k| k.len() == 32)?;
    Some(key == bytes)
}

/// Whether a shape rule applies, and if so its result.
enum Shape {
    NoRule,
    Compared(Option<Difference>),
}

/// Rules for two writings of the same value: wrappers that one decoder adds and the other
/// does not.
fn shape_rule(ours: &Json, theirs: &Json, path: &mut Vec<String>) -> Shape {
    if let Some(same) = key_as_bytes(ours, theirs) {
        return Shape::Compared((!same).then(|| difference(path, ours, theirs)));
    }
    Shape::Compared(match (ours, theirs) {
        // Big integers are a single value, however they are wrapped: `{"bn":"5"}`, 5 or "5".
        (Json::Object(a), _) | (_, Json::Object(a)) if is_big_int(a) => (big_int_text(peel(ours))
            != big_int_text(peel(theirs)))
        .then(|| difference(path, ours, theirs)),
        // serde writes NaN as null.
        (Json::Object(a), Json::Null)
            if a.len() == 1 && a.get("nan") == Some(&Json::Bool(true)) =>
        {
            None
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
            (!is_unit_variant(a, name)).then(|| difference(path, ours, theirs))
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

fn difference(path: &[String], ours: &Json, theirs: &Json) -> Difference {
    Difference {
        path: join_path(path),
        ours: short(ours),
        theirs: short(theirs),
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
    let is_type = is_type_key(name) && inner.is_object();
    // Only an enum, always one key, can carry the name as a key; a struct field cannot block.
    let enum_with_that_variant =
        other.len() == 1 && other.keys().any(|k| normalize(k) == normalize(name));
    (is_type && !enum_with_that_variant && holds_fields_of(inner, other)).then_some(inner)
}

fn is_type_key(name: &str) -> bool {
    normalize(name) == "0" || name.starts_with(|c: char| c.is_ascii_uppercase())
}

/// A type wrapper holds the other side's fields, maybe under more wrappers. If it does not,
/// the key is an enum variant, and peeling it would hide a wrong variant.
fn holds_fields_of(mut inner: &Json, other: &Map<String, Json>) -> bool {
    let keys = |m: &Map<String, Json>| m.keys().map(|k| normalize(k)).collect::<BTreeSet<_>>();
    let want = keys(other);
    while let Json::Object(m) = inner {
        if keys(m) == want {
            return true;
        }
        match single_wrapper(m) {
            Some(next) if m.keys().all(|k| is_type_key(k)) => inner = next,
            _ => return false,
        }
    }
    false
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

/// One key around an empty payload: how idl-verify writes a unit variant.
fn is_unit_shape(obj: &Map<String, Json>) -> bool {
    obj.len() == 1 && obj.values().all(is_empty_payload)
}

fn is_unit_variant(obj: &Map<String, Json>, name: &str) -> bool {
    obj.len() == 1
        && obj
            .iter()
            .any(|(k, v)| normalize(k) == normalize(name) && is_empty_payload(v))
}

fn is_empty_payload(value: &Json) -> bool {
    match value {
        Json::Object(inner) => inner.is_empty(),
        Json::Array(inner) => inner.is_empty(),
        Json::Null => true,
        _ => false,
    }
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
    use serde_json::json;

    #[test]
    fn a_key_as_base58_matches_its_32_bytes() {
        let system = json!("11111111111111111111111111111111");
        assert_eq!(first_difference(&system, &json!(vec![0; 32])), None);
        let mut other = vec![0; 32];
        other[31] = 1;
        assert!(first_difference(&system, &json!(other)).is_some());
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
        // Variant names that look like type names on both sides, as serde and Anchor write them.
        for (ours, theirs) in [
            (
                json!({"s": {"Long": {"size": 5}}}),
                json!({"s": {"Short": {"size": 5}}}),
            ),
            (json!({"s": {"Active": {}}}), json!({"s": {"Paused": {}}})),
            (
                json!({"a": {"Exact": {"0": {"bn": "5"}}}}),
                json!({"a": {"Max": 5}}),
            ),
            (
                json!({"a": {"Exact": {"0": {"bn": "5"}}}}),
                json!({"a": {"max": {"0": {"bn": "5"}}}}),
            ),
        ] {
            assert!(
                first_difference(&ours, &theirs).is_some(),
                "{ours} {theirs}"
            );
        }
        let wrong = json!({ "amount": 8, "kind": "Fixed", "pair": [1, 2] });
        assert_eq!(
            first_difference(&ours, &wrong).map(|d| d.path),
            Some("amount".to_string())
        );
    }

    #[test]
    fn a_nan_matches_the_null_that_serde_writes() {
        assert_eq!(
            first_difference(&json!({"n": {"nan": true}}), &json!({"n": null})),
            None
        );
        assert!(first_difference(&json!({"n": 1.0}), &json!({"n": null})).is_some());
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
