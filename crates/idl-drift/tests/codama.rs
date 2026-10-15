//! Reading Codama `rootNode` IDLs into the flat model.

mod common;

use common::{fixture, fixture_text, max_sev, parse};
use idl_drift::codama_convert::decode_encoded_bytes;
use idl_drift::model::{ArrayLen, Idl, Type, TypeDefTy};
use idl_drift::{coverage, diff, ChangeKind, Severity};

#[test]
fn codama_order_engine_self_diff_is_empty() {
    let idl = parse(include_str!("fixtures/codama_order_engine.json"));
    assert_eq!(idl.metadata.name, "orderEngine");
    assert_eq!(idl.address, "61DFfeTKM7trxYcPQCM78bJ794ddZprZpAwAnLiwTpYH");
    assert_eq!(idl.instructions.len(), 1);
    assert_eq!(idl.instructions[0].name, "fill");
    // A field discriminator takes its bytes from the omitted arg's default.
    assert_eq!(
        idl.instructions[0].discriminator,
        vec![0xa8, 0x60, 0xb7, 0xa3, 0x5c, 0x0a, 0x28, 0xa0]
    );
    assert_eq!(idl.instructions[0].args.len(), 3);
    let r = diff(&idl, &idl);
    assert!(r.changes.is_empty(), "{:?}", r.changes);
}

#[test]
fn codama_0_13_idl_loads() {
    // `npx codama convert` output; codama-nodes 0.9.1 rejected it (AmountTypeNode `seeds`).
    let idl = fixture("live/voltr_vault/idl.js-codama.json");
    let receipt = idl
        .accounts
        .iter()
        .find(|a| a.name == "adaptorAddReceipt")
        .expect("adaptorAddReceipt account");
    assert_eq!(receipt.discriminator, [105, 99, 219, 155, 77, 241, 7, 119]);
    assert!(coverage(&idl).fully_mapped());
}

#[test]
fn codama_spec_2_fails_with_its_version() {
    // Spec 2 renames `name` to `identifier`, so serde alone reports a missing `name`.
    let err = Idl::from_json(
        r#"{"kind":"rootNode","standard":"codama","version":"2.0.0","program":{
        "kind":"programNode","identifier":"t","publicKey":"P","version":"0"}}"#,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Codama spec 2.0.0 is not supported; only spec 1.x can be read"
    );
}

#[test]
fn same_discriminator_two_encodings_is_no_change() {
    // The bytes [1,2,3,4] under every Codama encoding.
    let expected = vec![1u8, 2, 3, 4];
    assert_eq!(decode_encoded_bytes("base16", "01020304"), expected);
    assert_eq!(decode_encoded_bytes("base58", "2VfUX"), expected);
    assert_eq!(decode_encoded_bytes("base64", "AQIDBA=="), expected);

    let vault = |data: &str, encoding: &str| {
        parse(&format!(
            r#"{{
      "kind":"rootNode","standard":"codama","version":"1.5.1",
      "program":{{
        "kind":"programNode","name":"enc","publicKey":"11111111111111111111111111111111","version":"0.1.0",
        "accounts":[{{
          "kind":"accountNode","name":"vault",
          "data":{{"kind":"structTypeNode","fields":[
            {{"kind":"structFieldTypeNode","name":"discriminator","defaultValueStrategy":"omitted",
             "type":{{"kind":"fixedSizeTypeNode","size":4,"type":{{"kind":"bytesTypeNode"}}}},
             "defaultValue":{{"kind":"bytesValueNode","data":"{data}","encoding":"{encoding}"}}}},
            {{"kind":"structFieldTypeNode","name":"bal",
             "type":{{"kind":"numberTypeNode","format":"u64","endian":"le"}}}}
          ]}},
          "discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}}]
        }}],
        "instructions":[],"definedTypes":[],"pdas":[],"events":[],"errors":[]
      }},
      "additionalPrograms":[]
    }}"#
        ))
    };
    let a = vault("01020304", "base16");
    let b = vault("2VfUX", "base58");
    assert_eq!(a.accounts[0].discriminator, expected);
    assert_eq!(b.accounts[0].discriminator, expected);
    let r = diff(&a, &b);
    assert!(r.changes.is_empty(), "{:?}", r.changes);
}

#[test]
fn non_ascii_hex_discriminator_does_not_panic() {
    // Slicing the hex string by byte index used to split `é` and panic.
    assert_eq!(decode_encoded_bytes("base16", "éa"), "éa".as_bytes());
    assert_eq!(decode_encoded_bytes("base16", "0a1"), [0x00, 0xa1]);
}

/// A one-instruction Codama program whose discriminator is a numeric arg.
fn codama_transfer(format: &str, value: u64, offset: usize) -> String {
    format!(
        r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
  "program":{{"kind":"programNode","name":"system","publicKey":"11111111111111111111111111111111","version":"0",
    "accounts":[],"definedTypes":[],"pdas":[],"errors":[],
    "instructions":[{{"kind":"instructionNode","name":"transfer","accounts":[],
      "arguments":[
        {{"kind":"instructionArgumentNode","name":"discriminator","defaultValueStrategy":"omitted",
          "type":{{"kind":"numberTypeNode","format":"{format}","endian":"le"}},
          "defaultValue":{{"kind":"numberValueNode","number":{value}}}}},
        {{"kind":"instructionArgumentNode","name":"lamports","type":{{"kind":"numberTypeNode","format":"u64","endian":"le"}}}}],
      "discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":{offset}}}]}}]}}}}"#
    )
}

#[test]
fn codama_numeric_discriminator_keeps_its_width_and_offset() {
    let ix = |f, v, o| parse(&codama_transfer(f, v, o)).instructions[0].clone();
    assert_eq!(ix("u32", 2, 0).discriminator, [2, 0, 0, 0]);
    assert_eq!(ix("u32", 512, 0).discriminator, [0, 2, 0, 0]);
    assert_eq!(ix("u8", 2, 4).discriminator_offset, 4);
    // A value that doesn't fit is not silently truncated.
    assert!(ix("u8", 256, 0).discriminator.is_empty());
    for (old, new) in [
        (("u8", 2, 0), ("u32", 2, 0)),
        (("u32", 256, 0), ("u32", 512, 0)),
        (("u8", 2, 0), ("u8", 2, 4)),
    ] {
        let report = diff(
            &parse(&codama_transfer(old.0, old.1, old.2)),
            &parse(&codama_transfer(new.0, new.1, new.2)),
        );
        assert!(
            report.has_breaking(),
            "{old:?} -> {new:?}: {:?}",
            report.changes
        );
    }
}

/// A Codama program whose one instruction takes an enum with the given tag size.
fn codama_enum_arg(size: &str, b_discriminator: Option<u32>) -> String {
    let disc = b_discriminator.map_or_else(String::new, |d| format!(r#","discriminator":{d}"#));
    format!(
        r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
  "program":{{"kind":"programNode","name":"t","publicKey":"11111111111111111111111111111111","version":"0",
    "accounts":[],"pdas":[],"errors":[],
    "definedTypes":[{{"kind":"definedTypeNode","name":"state","type":{{"kind":"enumTypeNode",
      "size":{{"kind":"numberTypeNode","format":"{size}","endian":"le"}},
      "variants":[{{"kind":"enumEmptyVariantTypeNode","name":"a"}},{{"kind":"enumEmptyVariantTypeNode","name":"b"{disc}}}]}}}}],
    "instructions":[{{"kind":"instructionNode","name":"set","accounts":[],
      "arguments":[{{"kind":"instructionArgumentNode","name":"state","type":{{"kind":"definedTypeLinkNode","name":"state"}}}}],
      "discriminators":[]}}]}}}}"#
    )
}

#[test]
fn codama_enum_tag_width_and_variant_values_are_breaking() {
    let base = codama_enum_arg("u8", None);
    assert_eq!(max_sev(&base, &base), None);
    for new in [codama_enum_arg("u32", None), codama_enum_arg("u8", Some(5))] {
        assert_eq!(max_sev(&base, &new), Some(Severity::Breaking), "{new}");
    }
}

/// A Codama program whose one instruction takes an arg of type `arg`.
fn codama_arg(arg: &str, defined_types: &str) -> String {
    format!(
        r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
  "program":{{"kind":"programNode","name":"t","publicKey":"11111111111111111111111111111111","version":"0",
    "accounts":[],"pdas":[],"errors":[],"definedTypes":[{defined_types}],
    "instructions":[{{"kind":"instructionNode","name":"set","accounts":[],"discriminators":[],
      "arguments":[{{"kind":"instructionArgumentNode","name":"x","type":{arg}}}]}}]}}}}"#
    )
}

/// A Codama program with one defined type `ext` passed as an instruction arg.
fn codama_defined_arg(ext: &str) -> String {
    codama_arg(
        r#"{"kind":"definedTypeLinkNode","name":"ext"}"#,
        &format!(r#"{{"kind":"definedTypeNode","name":"ext","type":{ext}}}"#),
    )
}

#[test]
fn codama_wrapper_on_defined_type_is_kept() {
    let st = r#"{"kind":"structTypeNode","fields":[{"kind":"structFieldTypeNode","name":"x","type":{"kind":"numberTypeNode","format":"u64","endian":"le"}}]}"#;
    let prefixed = format!(
        r#"{{"kind":"sizePrefixTypeNode","type":{st},"prefix":{{"kind":"numberTypeNode","format":"u16","endian":"le"}}}}"#
    );
    assert_eq!(
        max_sev(&codama_defined_arg(st), &codama_defined_arg(&prefixed)),
        Some(Severity::Breaking)
    );
}

#[test]
fn codama_fixed_size_bytes_type_resolves_to_a_byte_array() {
    let idl = parse(&codama_defined_arg(
        r#"{"kind":"fixedSizeTypeNode","size":32,"type":{"kind":"bytesTypeNode"}}"#,
    ));
    assert_eq!(
        idl.types[0].ty,
        TypeDefTy::Type {
            alias: Type::Array(Box::new(Type::U8), ArrayLen::Value(32))
        }
    );
}

#[test]
fn change_inside_a_codama_only_encoding_is_breaking() {
    let num = |format: &str, endian: &str| {
        format!(r#"{{"kind":"numberTypeNode","format":"{format}","endian":"{endian}"}}"#)
    };
    let inline = |ty: &str| {
        format!(
            r#"{{"kind":"structTypeNode","fields":[{{"kind":"structFieldTypeNode","name":"a","type":{ty}}}]}}"#
        )
    };
    let sized = |prefix: &str| {
        format!(
            r#"{{"kind":"sizePrefixTypeNode","type":{{"kind":"stringTypeNode","encoding":"utf8"}},"prefix":{prefix}}}"#
        )
    };
    let option = |prefix: &str, fixed: bool| {
        format!(
            r#"{{"kind":"optionTypeNode","fixed":{fixed},"item":{{"kind":"publicKeyTypeNode"}},"prefix":{prefix}}}"#
        )
    };
    let zeroable = r#"{"kind":"zeroableOptionTypeNode","item":{"kind":"publicKeyTypeNode"}}"#;
    let (u8le, u16le, u32le, u64le) = (
        num("u8", "le"),
        num("u16", "le"),
        num("u32", "le"),
        num("u64", "le"),
    );
    // Each pair reads different bytes. An older converter made both sides the same `Generic`.
    let mut cases = vec![
        ("inline struct field", inline(&u64le), inline(&u32le)),
        ("size prefix width", sized(&u8le), sized(&u16le)),
        ("byte order", u64le.clone(), num("u64", "be")),
        ("fixed option", option(&u16le, false), option(&u16le, true)),
        (
            "zeroable option",
            zeroable.to_string(),
            option(&u8le, false),
        ),
    ]
    .into_iter()
    .map(|(label, old, new)| (label, codama_arg(&old, ""), codama_arg(&new, "")))
    .collect::<Vec<_>>();
    // `inner` is linked only inside an inline struct, so its change must still count.
    let link = inline(r#"{"kind":"definedTypeLinkNode","name":"inner"}"#);
    let inner = |ty: &str| {
        let ty = inline(ty);
        format!(r#"{{"kind":"definedTypeNode","name":"inner","type":{ty}}}"#)
    };
    cases.push((
        "type behind an inline struct",
        codama_arg(&link, &inner(&u64le)),
        codama_arg(&link, &inner(&u32le)),
    ));
    for (label, old, new) in &cases {
        assert_eq!(max_sev(old, new), Some(Severity::Breaking), "{label}");
        assert_eq!(max_sev(new, new), None, "{label}: self-diff");
    }
}

#[test]
fn change_inside_a_codama_node_idl_drift_cannot_read_is_breaking() {
    let hidden = |ty: &str, docs: &str| {
        codama_arg(
            &format!(
                r#"{{"kind":"hiddenPrefixTypeNode","prefix":[{{"kind":"constantValueNode",
                "type":{{"kind":"numberTypeNode","format":"u8","endian":"le"}},"value":{{"kind":"numberValueNode","number":1}}}}],
                "type":{{"kind":"structTypeNode","fields":[{{"kind":"structFieldTypeNode","name":"a","docs":["{docs}"],
                  "type":{{"kind":"numberTypeNode","format":"{ty}","endian":"le"}}}}]}}}}"#
            ),
            "",
        )
    };
    let base = hidden("u8", "old");
    assert_eq!(max_sev(&base, &base), None);
    // A CI gate trusts the fingerprint, so it is a full SHA-256, not a short fast hash.
    let arg = &parse(&base).instructions[0].args[0].ty;
    let Type::Generic(name) = arg else {
        panic!("expected an unresolved node, got {arg:?}");
    };
    let digest = name.split_once('#').map_or("", |(_, d)| d);
    assert!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "{name}"
    );
    // Docs are not bytes.
    assert_eq!(max_sev(&base, &hidden("u8", "new")), None);
    let changes = diff(&parse(&base), &parse(&hidden("u64", "old"))).changes;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].severity, Severity::Breaking);
    assert_eq!(
        changes[0].message,
        "arg changed (set#0): x: hiddenPrefixTypeNode changed inside a Codama encoding that idl-drift cannot read, so check the new bytes by hand"
    );
}

#[test]
fn size_discriminators_and_event_chains_are_compared() {
    let root = |ix_size: u64, account_size: u64, event_chain: &str| {
        parse(&format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],"program":{{
            "kind":"programNode","name":"p","publicKey":"P","version":"1","pdas":[],"errors":[],"definedTypes":[],
            "instructions":[{{"kind":"instructionNode","name":"sized","accounts":[],"arguments":[],
              "discriminators":[{{"kind":"sizeDiscriminatorNode","size":{ix_size}}}]}}],
            "accounts":[{{"kind":"accountNode","name":"vault","data":{{"kind":"structTypeNode","fields":[]}},
              "discriminators":[{{"kind":"sizeDiscriminatorNode","size":{account_size}}}]}}],
            "events":[{{"kind":"eventNode","name":"trade","data":{{"kind":"structTypeNode","fields":[]}},
              "discriminators":[{event_chain}]}}]}}}}"#
        ))
    };
    let constant = |hex: &str, offset: u8| {
        format!(
            r#"{{"kind":"constantDiscriminatorNode","offset":{offset},"constant":{{"kind":"constantValueNode",
            "type":{{"kind":"bytesTypeNode"}},"value":{{"kind":"bytesValueNode","data":"{hex}","encoding":"base16"}}}}}}"#
        )
    };
    let chain = |last: &str| {
        format!(
            "{},{},{}",
            constant("fe", 0),
            constant("a1a2", 4),
            constant(last, 8)
        )
    };
    let kinds = |old: &Idl, new: &Idl| -> Vec<ChangeKind> {
        diff(old, new).changes.iter().map(|c| c.kind).collect()
    };
    let base = root(4, 100, &chain("b7"));
    assert_eq!(kinds(&base, &base), []);
    assert_eq!(
        kinds(&base, &root(8, 100, &chain("b7"))),
        [ChangeKind::DiscriminatorChanged]
    );
    assert_eq!(
        kinds(&base, &root(4, 120, &chain("b7"))),
        [ChangeKind::AccountDiscriminatorChanged]
    );
    // The third constant of the chain: Shipstern checks it too.
    assert_eq!(
        kinds(&base, &root(4, 100, &chain("b0"))),
        [ChangeKind::EventDiscriminatorChanged]
    );
    // A lone event discriminator that moves from offset 0 to 8.
    let lone = |offset: u8| root(4, 100, &constant("e1e2", offset));
    assert_eq!(
        kinds(&lone(0), &lone(8)),
        [ChangeKind::EventDiscriminatorChanged]
    );
}

#[test]
fn limit_order_v1_to_v2_is_breaking() {
    let report = diff(
        &fixture("shipstern/limit_order_v1.json"),
        &fixture("shipstern/limit_order_v2.json"),
    );
    let has = |severity: Severity, text: &str| {
        report
            .changes
            .iter()
            .any(|c| c.severity == severity && c.message.contains(text))
    };
    // Fee account: four u64 fees become two u16 bps fields.
    assert!(has(
        Severity::Breaking,
        "(fee#0) via reachable type: makerFee: u64 -> normalFeeBps: u16"
    ));
    assert!(has(Severity::Breaking, "instruction removed: initFee"));
    assert!(has(
        Severity::Breaking,
        "instruction removed: cancelExpiredOrder"
    ));
    assert!(has(Severity::Additive, "cancelDustOrder"));
    // v2's Codama events hide only their own discriminator, so no envelope change: Codama
    // has no node for a self-CPI tag, and Shipstern infers one only from a discriminator chain.
    assert!(
        !report
            .changes
            .iter()
            .any(|c| c.message.to_lowercase().contains("envelope")),
        "{:?}",
        report.changes
    );
}

#[test]
fn limit_order_idls_have_no_unexpected_generics() {
    for file in ["limit_order_v1", "limit_order_v2"] {
        let report = coverage(&fixture(&format!("shipstern/{file}.json")));
        assert!(report.fully_mapped(), "{file}: {:?}", report.unmapped);
    }
    // A u32-prefixed bytes node maps to `Type::Bytes`, not `Generic`.
    let v2 = fixture("shipstern/limit_order_v2.json");
    let fill = v2
        .instructions
        .iter()
        .find(|i| i.name == "fillOrder")
        .expect("fillOrder");
    let swap = fill
        .args
        .iter()
        .find(|a| a.name == "swapData")
        .expect("swapData arg");
    assert_eq!(swap.ty, Type::Bytes);
}

#[test]
fn token_2022_sub_discriminator_change_is_breaking() {
    // The second omitted arg tells the transfer-fee instructions apart, and Codama checks it.
    let text = fixture_text("codama/token_2022_transfer_fee.json");
    let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let ix = json["program"]["instructions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|ix| ix["name"] == "transferCheckedWithFee")
        .unwrap();
    assert_eq!(ix["arguments"][1]["name"], "transferFeeDiscriminator");
    ix["arguments"][1]["defaultValue"]["number"] = 9.into();
    let report = diff(&parse(&text), &parse(&json.to_string()));
    let breaking: Vec<(ChangeKind, &str)> = report
        .changes
        .iter()
        .filter(|c| c.severity == Severity::Breaking)
        .map(|c| (c.kind, c.subject.as_str()))
        .collect();
    assert_eq!(
        breaking,
        [(ChangeKind::DiscriminatorChanged, "transferCheckedWithFee")]
    );
}

#[test]
fn omitted_field_outside_the_discriminators_stays_in_the_layout() {
    // Program Metadata accounts start with an omitted enum byte that no discriminator holds.
    let idl = fixture("codama/pmp_accounts.json");
    let metadata = idl.types.iter().find(|t| t.name == "metadata").unwrap();
    let TypeDefTy::Struct { fields } = &metadata.ty else {
        panic!("{:?}", metadata.ty);
    };
    let names: Vec<&str> = fields.iter().take(2).map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["discriminator", "program"]);
}
