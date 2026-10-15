//! Diffs across formats: the same program as legacy Anchor, Anchor 0.30 and Codama must
//! compare equal, and a real change must still show.

mod common;

use common::{fixture, fixture_text, messages, parse};
use idl_drift::diff;
use idl_drift::model::{AccountItem, Format};

#[test]
fn same_program_in_two_formats_diffs_clean() {
    // Legacy Anchor (camelCase, no discriminators) against Codama (camelCase types too).
    let report = diff(
        &fixture("real/limit_order_v1_onchain.json"),
        &fixture("shipstern/limit_order_v1.json"),
    );
    assert!(report.changes.is_empty(), "{:#?}", report.changes);
}

#[test]
fn cross_format_diff_still_reports_real_changes() {
    let mut legacy: serde_json::Value =
        serde_json::from_str(&fixture_text("real/limit_order_v1_onchain.json")).unwrap();
    // Widen the first arg of initializeOrder (u64) in the legacy file.
    legacy["instructions"][0]["args"][0]["type"] = serde_json::json!("u128");
    assert_eq!(
        messages(
            &parse(&legacy.to_string()),
            &fixture("shipstern/limit_order_v1.json")
        ),
        ["arg changed (initializeOrder#0): makingAmount: u128 -> u64 (Borsh layout)"]
    );
}

#[test]
fn camel_case_matches_codama_on_acronyms() {
    let idl = parse(
        r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.1.0"},
          "instructions":[{"name":"init_AMMConfig","discriminator":[1],"accounts":[{"name":"pool_state"}],"args":[]}]}"#,
    )
    .with_camel_case_names();
    assert_eq!(idl.instructions[0].name, "initAMMConfig");
    let AccountItem::Single(a) = &idl.instructions[0].accounts[0] else {
        panic!("single account")
    };
    assert_eq!(a.name, "poolState");
}

#[test]
fn camel_case_splits_anchor_type_paths() {
    // Anchor 0.30 names a foreign type by path; legacy IDLs flattened it. Both compare equal.
    let path = r#"{"address":"A","metadata":{"name":"p","version":"1","spec":"0.1.0"},
      "accounts":[{"name":"Position","discriminator":[170,188,143,228,122,64,247,208]}],
      "types":[{"name":"Position","type":{"kind":"struct","fields":[{"name":"r","type":{"defined":{"name":"whirlpool::state::position::PositionRewardInfo"}}}]}},
               {"name":"whirlpool::state::position::PositionRewardInfo","type":{"kind":"struct","fields":[{"name":"x","type":"u8"}]}}]}"#;
    let flat = r#"{"version":"1","name":"p","metadata":{"address":"A"},"instructions":[],
      "accounts":[{"name":"Position","type":{"kind":"struct","fields":[{"name":"r","type":{"defined":"WhirlpoolStatePositionPositionRewardInfo"}}]}}],
      "types":[{"name":"WhirlpoolStatePositionPositionRewardInfo","type":{"kind":"struct","fields":[{"name":"x","type":"u8"}]}}]}"#;
    let report = diff(&parse(flat), &parse(path));
    assert!(report.changes.is_empty(), "{:#?}", report.changes);
}

#[test]
fn anchor_and_its_codama_conversion_read_the_same_args() {
    // `Params` has one use, so Codama flattens it; `Inner` has two, so Codama keeps the link.
    let anchor = r#"{"address":"P","metadata":{"name":"p","version":"1","spec":"0.1.0"},
      "instructions":[
        {"name":"go","discriminator":[1],"accounts":[],"args":[{"name":"params","type":{"defined":{"name":"Params"}}}]},
        {"name":"stop","discriminator":[2],"accounts":[],"args":[
          {"name":"inner","type":{"defined":{"name":"Inner"}}},{"name":"flag","type":{"defined":{"name":"OptionBool"}}}]}],
      "types":[
        {"name":"Params","type":{"kind":"struct","fields":[{"name":"a","type":"u8"},{"name":"inner","type":{"defined":{"name":"Inner"}}}]}},
        {"name":"Inner","type":{"kind":"struct","fields":[{"name":"x","type":"u64"}]}},
        {"name":"OptionBool","type":{"kind":"struct","fields":["bool"]}}]}"#;
    let codama = |option_bool: &str| {
        let arg = |name: &str, ty: &str| {
            format!(r#"{{"kind":"instructionArgumentNode","name":"{name}","type":{ty}}}"#)
        };
        let link = |name: &str| format!(r#"{{"kind":"definedTypeLinkNode","name":"{name}"}}"#);
        let disc = |b: u8| {
            format!(
                r#"{{"kind":"instructionArgumentNode","name":"discriminator","defaultValueStrategy":"omitted","type":{{"kind":"fixedSizeTypeNode","size":1,"type":{{"kind":"bytesTypeNode"}}}},"defaultValue":{{"kind":"bytesValueNode","data":"0{b}","encoding":"base16"}}}}"#
            )
        };
        let ix = |name: &str, b: u8, args: &[String]| {
            format!(
                r#"{{"kind":"instructionNode","name":"{name}","accounts":[],"arguments":[{},{}],"discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}}]}}"#,
                disc(b),
                args.join(",")
            )
        };
        let defined = |name: &str, ty: &str| {
            format!(r#"{{"kind":"definedTypeNode","name":"{name}","type":{ty}}}"#)
        };
        format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","program":{{"kind":"programNode","name":"p","publicKey":"P","version":"1","accounts":[],"pdas":[],"errors":[],"events":[],
            "instructions":[{},{}],"definedTypes":[{},{}]}},"additionalPrograms":[]}}"#,
            ix(
                "go",
                1,
                &[
                    arg(
                        "a",
                        r#"{"kind":"numberTypeNode","format":"u8","endian":"le"}"#
                    ),
                    arg("inner", &link("inner"))
                ]
            ),
            ix(
                "stop",
                2,
                &[
                    arg("inner", &link("inner")),
                    arg("flag", &link("optionBool"))
                ]
            ),
            defined(
                "inner",
                r#"{"kind":"structTypeNode","fields":[{"kind":"structFieldTypeNode","name":"x","type":{"kind":"numberTypeNode","format":"u64","endian":"le"}}]}"#
            ),
            defined("optionBool", option_bool),
        )
    };
    let tuple = r#"{"kind":"tupleTypeNode","items":[{"kind":"booleanTypeNode","size":{"kind":"numberTypeNode","format":"u8","endian":"le"}}]}"#;
    let report = diff(&parse(anchor), &parse(&codama(tuple)));
    assert!(!report.has_breaking(), "{:?}", report.changes);
    // A Codama file whose `optionBool` lost its field reads 0 bytes where the program writes 1.
    let empty = r#"{"kind":"structTypeNode","fields":[]}"#;
    assert!(diff(&parse(anchor), &parse(&codama(empty))).has_breaking());
}

#[test]
fn a_struct_arg_that_holds_itself_does_not_grow_without_bound() {
    // 64 fields of its own type: 16 rounds of inlining would make 64^16 args.
    let fields: Vec<String> = (0..64)
        .map(|i| format!(r#"{{"name":"f{i}","type":{{"defined":{{"name":"Wide"}}}}}}"#))
        .collect();
    let old = parse(&format!(
        r#"{{"address":"P","metadata":{{"name":"p","version":"1","spec":"0.1.0"}},
        "instructions":[{{"name":"go","discriminator":[1],"accounts":[],"args":[{{"name":"w","type":{{"defined":{{"name":"Wide"}}}}}}]}}],
        "types":[{{"name":"Wide","type":{{"kind":"struct","fields":[{}]}}}}]}}"#,
        fields.join(",")
    ));
    let mut new = old.clone();
    new.format = Format::Codama;
    assert!(!diff(&old, &new).has_breaking());
}

/// A Codama instruction `go` with discriminator 1 and the given args.
fn codama_go(args: &[String], defined_types: &str) -> String {
    format!(
        r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
        "program":{{"kind":"programNode","name":"p","publicKey":"P","version":"1","accounts":[],"pdas":[],"errors":[],"events":[],
        "definedTypes":[{defined_types}],
        "instructions":[{{"kind":"instructionNode","name":"go","accounts":[],
          "discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}}],
          "arguments":[{{"kind":"instructionArgumentNode","name":"discriminator","defaultValueStrategy":"omitted",
            "type":{{"kind":"fixedSizeTypeNode","size":1,"type":{{"kind":"bytesTypeNode"}}}},
            "defaultValue":{{"kind":"bytesValueNode","data":"01","encoding":"base16"}}}},{}]}}]}}}}"#,
        args.join(",")
    )
}

fn codama_field(kind: &str, name: &str, ty: &str) -> String {
    format!(r#"{{"kind":"{kind}","name":"{name}","type":{ty}}}"#)
}

fn codama_struct(fields: &[(&str, &str)]) -> String {
    let fields: Vec<String> = fields
        .iter()
        .map(|(name, ty)| codama_field("structFieldTypeNode", name, ty))
        .collect();
    format!(
        r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
        fields.join(",")
    )
}

fn codama_num(format: &str) -> String {
    format!(r#"{{"kind":"numberTypeNode","format":"{format}","endian":"le"}}"#)
}

#[test]
fn codama_inline_struct_arg_reads_like_a_linked_struct() {
    // Anchor links `Params`, which links `Inner`; the Codama file writes `inner` inline.
    let anchor = parse(
        r#"{"address":"P","metadata":{"name":"p","version":"1","spec":"0.1.0"},
      "instructions":[{"name":"go","discriminator":[1],"accounts":[],"args":[
        {"name":"params","type":{"defined":{"name":"Params"}}},{"name":"amt","type":"u64"}]}],
      "types":[
        {"name":"Params","type":{"kind":"struct","fields":[{"name":"a","type":"u64"},{"name":"inner","type":{"defined":{"name":"Inner"}}}]}},
        {"name":"Inner","type":{"kind":"struct","fields":[{"name":"b","type":"u8"},{"name":"c","type":"u16"}]}}]}"#,
    );
    let codama = |c: &str| {
        let arg = |name: &str, ty: &str| codama_field("instructionArgumentNode", name, ty);
        let inner = codama_struct(&[("b", &codama_num("u8")), ("c", &codama_num(c))]);
        parse(&codama_go(
            &[
                arg("a", &codama_num("u64")),
                arg("inner", &inner),
                arg("amt", &codama_num("u64")),
            ],
            "",
        ))
    };
    let report = diff(&anchor, &codama("u16"));
    assert!(report.changes.is_empty(), "{:#?}", report.changes);
    assert_eq!(
        messages(&anchor, &codama("u32")),
        ["arg changed (go#2): c: u16 -> u32 (Borsh layout)"]
    );
}

#[test]
fn generic_type_matches_the_codama_body_written_at_each_use() {
    // Codama has no generics, so its converters write the body of `Price<u128>` and
    // `Signer<pubkey>` in place of each use.
    let anchor = parse(
        r#"{"address":"P","metadata":{"name":"p","version":"1","spec":"0.1.0"},
      "instructions":[{"name":"go","discriminator":[1],"accounts":[],"args":[
        {"name":"prices","type":{"defined":{"name":"Prices","generics":[{"kind":"type","type":"u128"}]}}},
        {"name":"cfg","type":{"defined":{"name":"Config"}}}]}],
      "types":[
        {"name":"Config","type":{"kind":"struct","fields":[
          {"name":"signers","type":{"array":[{"defined":{"name":"Signer","generics":[{"kind":"type","type":"pubkey"}]}},2]}}]}},
        {"name":"Signer","generics":[{"kind":"type","name":"T"}],"type":{"kind":"struct","fields":[
          {"name":"key","type":{"generic":"T"}},{"name":"expires_at","type":"i64"}]}},
        {"name":"Price","generics":[{"kind":"type","name":"T"}],"type":{"kind":"struct","fields":[
          {"name":"min","type":{"generic":"T"}},{"name":"max","type":{"generic":"T"}}]}},
        {"name":"Prices","generics":[{"kind":"type","name":"T"}],"type":{"kind":"struct","fields":[
          {"name":"index","type":{"defined":{"name":"Price","generics":[{"kind":"type","type":{"generic":"T"}}]}}},
          {"name":"long","type":{"defined":{"name":"Price","generics":[{"kind":"type","type":{"generic":"T"}}]}}}]}}]}"#,
    );
    let codama = |expires_at: &str| {
        let arg = |name: &str, ty: &str| codama_field("instructionArgumentNode", name, ty);
        let price = codama_struct(&[("min", &codama_num("u128")), ("max", &codama_num("u128"))]);
        let signer = codama_struct(&[
            ("key", r#"{"kind":"publicKeyTypeNode"}"#),
            ("expiresAt", &codama_num(expires_at)),
        ]);
        let signers = format!(
            r#"{{"kind":"arrayTypeNode","item":{signer},"count":{{"kind":"fixedCountNode","value":2}}}}"#
        );
        let config = codama_field(
            "definedTypeNode",
            "config",
            &codama_struct(&[("signers", &signers)]),
        );
        parse(&codama_go(
            &[
                arg("index", &price),
                arg("long", &price),
                arg("cfg", r#"{"kind":"definedTypeLinkNode","name":"config"}"#),
            ],
            &config,
        ))
    };
    let report = diff(&anchor, &codama("i64"));
    assert!(report.changes.is_empty(), "{:#?}", report.changes);
    // `cfg` is inlined into the args, so no parser reads `config` itself.
    assert_eq!(
        messages(&anchor, &codama("i32")),
        [
            "arg changed (go#4): signers: [{ key: pubkey, expiresAt: i64 }; 2] -> [{ key: pubkey, expiresAt: i32 }; 2] (Borsh layout)",
            "field changed in UNREACHED type (config#0): dead code",
        ]
    );
}
