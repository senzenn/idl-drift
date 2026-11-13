//! Predict where Shipstern's macro (0.13.0, 679fb715) panics on the tag of a defined enum in a
//! Codama IDL (proc-macro `build_defined_types.rs`, `rust_types_from_ir.rs` `enum_tag_type`).
//! The Anchor converter writes each tag as a `u8` with no explicit values, so these checks
//! apply to Codama JSON only.

use serde_json::Value;

use super::Rejection;

/// Not a converter error: the macro panics, and the compiler shows the panic message.
const RULE: &str = "EnumTag";

/// The codama-nodes `NestedTypeNode` wrappers. Another kind falls back to a number node.
const WRAPPERS: [&str; 7] = [
    "fixedSizeTypeNode",
    "hiddenPrefixTypeNode",
    "hiddenSuffixTypeNode",
    "postOffsetTypeNode",
    "preOffsetTypeNode",
    "sentinelTypeNode",
    "sizePrefixTypeNode",
];

/// The enum tags in `program`, a Codama program node, that the macro cannot read.
pub(super) fn rejections(program: &Value) -> Vec<Rejection> {
    let mut out = Vec::new();
    let types = program.get("definedTypes").and_then(Value::as_array);
    for defined in types.into_iter().flatten() {
        let node = &defined["type"];
        if node.get("kind").and_then(Value::as_str) != Some("enumTypeNode") {
            continue;
        }
        let name = defined.get("name").and_then(Value::as_str).unwrap_or("");
        let mut reject = |reason: String| {
            out.push(Rejection {
                item: format!("type {name}"),
                rule: RULE,
                reason,
            });
        };
        let variants = node.get("variants").and_then(Value::as_array);
        let count = variants.map_or(0, Vec::len);
        for (i, variant) in variants.into_iter().flatten().enumerate() {
            let explicit = variant.get("discriminator").and_then(Value::as_u64);
            if let Some(value) = explicit.filter(|&value| value != i as u64) {
                reject(format!(
                    "variant {i} has the explicit discriminator {value}, but Shipstern reads the variant position as the tag"
                ));
            }
        }
        // Codama requires `size`, so the macro cannot parse an IDL without it.
        let Some(size) = node.get("size").and_then(Value::as_object) else {
            continue;
        };
        if let Some(kind) = size
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| WRAPPERS.contains(kind))
        {
            reject(format!(
                "the tag size is a {kind}, and Shipstern reads only a numberTypeNode"
            ));
            continue;
        }
        match size.get("format").and_then(Value::as_str) {
            Some("shortU16") if count > 128 => reject(format!(
                "a shortU16 tag with {count} variants; Shipstern reads a shortU16 tag only up to 128 variants"
            )),
            Some(format @ ("f32" | "f64")) => reject(format!(
                "a {format} tag; Shipstern reads only an integer tag"
            )),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::Idl;

    /// The reasons that the public check gives for a Codama IDL with one enum `mode`.
    fn reasons(variants: &Value, size: &Value) -> Vec<String> {
        let text = json!({"kind": "rootNode", "standard": "codama", "version": "1.6.0",
            "program": {"kind": "programNode", "name": "t", "publicKey": "11111111111111111111111111111111",
                "version": "0.1.0", "accounts": [], "instructions": [], "definedTypes": [{
                    "kind": "definedTypeNode", "name": "mode",
                    "type": {"kind": "enumTypeNode", "variants": variants, "size": size}}],
                "pdas": [], "errors": []},
            "additionalPrograms": []})
        .to_string();
        let found = crate::shipstern::rejections(&text, &Idl::from_json(&text).unwrap()).unwrap();
        assert!(found
            .iter()
            .all(|r| r.rule == RULE && r.item == "type mode"));
        found.into_iter().map(|r| r.reason).collect()
    }

    fn number(format: &str) -> Value {
        json!({"kind": "numberTypeNode", "format": format, "endian": "le"})
    }

    fn variants(discriminators: &[Option<u64>]) -> Value {
        discriminators
            .iter()
            .enumerate()
            .map(|(i, d)| json!({"kind": "enumEmptyVariantTypeNode", "name": format!("v{i}"), "discriminator": d}))
            .collect()
    }

    // Each case matches a build of Shipstern 0.13.0 (679fb715) with include_shipstern_parser!.
    #[test]
    fn matches_the_macro() {
        let two = variants(&[None, None]);
        assert!(reasons(&two, &number("u16")).is_empty());
        assert!(reasons(&variants(&[Some(0), Some(1)]), &number("u8")).is_empty());
        assert_eq!(
            reasons(&variants(&[None, Some(5)]), &number("u8")),
            ["variant 1 has the explicit discriminator 5, but Shipstern reads the variant position as the tag"]
        );
        let wrapped = json!({"kind": "preOffsetTypeNode", "offset": 0, "strategy": "relative", "type": number("u8")});
        assert_eq!(
            reasons(&two, &wrapped),
            ["the tag size is a preOffsetTypeNode, and Shipstern reads only a numberTypeNode"]
        );
        assert!(reasons(&variants(&[None; 128]), &number("shortU16")).is_empty());
        assert_eq!(
            reasons(&variants(&[None; 129]), &number("shortU16")),
            ["a shortU16 tag with 129 variants; Shipstern reads a shortU16 tag only up to 128 variants"]
        );
        assert_eq!(
            reasons(&two, &number("f32")),
            ["a f32 tag; Shipstern reads only an integer tag"]
        );
    }
}
