//! One test per diff rule: build two IDLs, diff them, check the highest severity and,
//! where it matters, the message.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use idl_drift::codama_convert::decode_encoded_bytes;
use idl_drift::model::{
    Account, AccountItem, ArrayLen, Event, Field, Format, Instruction, Metadata, NamedDisc, Type,
    TypeDef, TypeDefTy,
};
use idl_drift::{diff, model::Idl, ChangeKind, Severity};

fn parse(s: &str) -> Idl {
    Idl::from_json(s).expect("fixture should deserialize")
}

fn max_sev(old: &str, new: &str) -> Option<Severity> {
    diff(&parse(old), &parse(new))
        .changes
        .iter()
        .map(|c| c.severity)
        .max()
}

const AMM_OLD: &str = r#"{
  "address":"A","metadata":{"name":"amm","version":"0.1.0","spec":"0.1.0"},
  "instructions":[{"name":"swap","discriminator":[1,2,3,4,5,6,7,8],
    "accounts":[{"name":"pool","writable":true},{"name":"user","signer":true}],
    "args":[{"name":"amountIn","type":"u64"}]}],
  "accounts":[{"name":"Pool","discriminator":[9,9,9,9,9,9,9,9]}],
  "types":[{"name":"Pool","type":{"kind":"struct","fields":[
    {"name":"authority","type":"pubkey"},{"name":"feeBps","type":"u16"}]}}]
}"#;

const AMM_NEW: &str = r#"{
  "address":"A","metadata":{"name":"amm","version":"0.2.0","spec":"0.1.0"},
  "instructions":[{"name":"swap","discriminator":[1,2,3,4,5,6,7,8],
    "accounts":[{"name":"pool","writable":true},{"name":"user","signer":true},{"name":"feeVault","writable":true}],
    "args":[{"name":"amountIn","type":"u64"}]},
    {"name":"collectFees","discriminator":[8,7,6,5,4,3,2,1],"accounts":[{"name":"pool","writable":true}],"args":[]}],
  "accounts":[{"name":"Pool","discriminator":[9,9,9,9,9,9,9,9]}],
  "types":[{"name":"Pool","type":{"kind":"struct","fields":[
    {"name":"authority","type":"pubkey"},{"name":"feeBps","type":"u32"}]}}]
}"#;

#[test]
fn amm_pool_field_width_is_breaking() {
    // Pool.feeBps u16 to u32 on a reachable account type.
    let r = diff(&parse(AMM_OLD), &parse(AMM_NEW));
    assert!(r.has_breaking(), "expected breaking; got {:?}", r.changes);
    assert_eq!(r.exit_code(), 1);
}

const DEAD_OLD: &str = r#"{
  "address":"A","metadata":{"name":"d","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"noop","discriminator":[1],"accounts":[],"args":[]}],
  "types":[{"name":"DeadType","type":{"kind":"struct","fields":[{"name":"x","type":"u16"}]}}]
}"#;
const DEAD_NEW: &str = r#"{
  "address":"A","metadata":{"name":"d","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"noop","discriminator":[1],"accounts":[],"args":[]}],
  "types":[{"name":"DeadType","type":{"kind":"struct","fields":[{"name":"x","type":"u64"}]}}]
}"#;

#[test]
fn unreachable_type_change_is_not_breaking() {
    let r = diff(&parse(DEAD_OLD), &parse(DEAD_NEW));
    assert!(
        !r.has_breaking(),
        "dead type must not be breaking; got {:?}",
        r.changes
    );
    assert_eq!(r.exit_code(), 0);
}

const ENUM_OLD: &str = r#"{
  "address":"A","metadata":{"name":"e","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"upd","discriminator":[1],"accounts":[],"args":[{"name":"m","type":{"defined":{"name":"Mode"}}}]}],
  "types":[{"name":"Mode","type":{"kind":"enum","variants":[{"name":"Fast"},{"name":"Slow"}]}}]
}"#;
const ENUM_APPEND: &str = r#"{
  "address":"A","metadata":{"name":"e","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"upd","discriminator":[1],"accounts":[],"args":[{"name":"m","type":{"defined":{"name":"Mode"}}}]}],
  "types":[{"name":"Mode","type":{"kind":"enum","variants":[{"name":"Fast"},{"name":"Slow"},{"name":"Paused"}]}}]
}"#;
const ENUM_INSERT: &str = r#"{
  "address":"A","metadata":{"name":"e","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"upd","discriminator":[1],"accounts":[],"args":[{"name":"m","type":{"defined":{"name":"Mode"}}}]}],
  "types":[{"name":"Mode","type":{"kind":"enum","variants":[{"name":"Fast"},{"name":"Paused"},{"name":"Slow"}]}}]
}"#;

#[test]
fn enum_append_is_dangerous_not_breaking() {
    let r = diff(&parse(ENUM_OLD), &parse(ENUM_APPEND));
    assert!(
        !r.has_breaking(),
        "append must not be breaking; got {:?}",
        r.changes
    );
    assert_eq!(max_sev(ENUM_OLD, ENUM_APPEND), Some(Severity::Dangerous));
}

#[test]
fn enum_middle_insert_is_breaking() {
    // Inserting before Slow moves its tag from 1 to 2.
    let r = diff(&parse(ENUM_OLD), &parse(ENUM_INSERT));
    assert!(
        r.has_breaking(),
        "insert must be breaking; got {:?}",
        r.changes
    );
}

const SER_OLD: &str = r#"{
  "address":"A","metadata":{"name":"s","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"noop","discriminator":[1],"accounts":[],"args":[]}],
  "accounts":[{"name":"Vault","discriminator":[1,2,3,4,5,6,7,8]}],
  "types":[{"name":"Vault","serialization":"borsh","type":{"kind":"struct","fields":[{"name":"bal","type":"u64"}]}}]
}"#;
const SER_NEW: &str = r#"{
  "address":"A","metadata":{"name":"s","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"noop","discriminator":[1],"accounts":[],"args":[]}],
  "accounts":[{"name":"Vault","discriminator":[1,2,3,4,5,6,7,8]}],
  "types":[{"name":"Vault","serialization":"bytemuck","type":{"kind":"struct","fields":[{"name":"bal","type":"u64"}]}}]
}"#;

// A single u64 has no padding, so Borsh and repr(C) write the same bytes.
#[test]
fn serialization_change_without_padding_is_cosmetic() {
    assert_eq!(max_sev(SER_OLD, SER_NEW), Some(Severity::Cosmetic));
}

#[test]
fn serialization_change_with_padding_is_breaking() {
    // u8 then u64: repr(C) puts 7 bytes of padding before `bal`.
    let with_flag = |s: &str| {
        s.replace(
            r#"[{"name":"bal""#,
            r#"[{"name":"flag","type":"u8"},{"name":"bal""#,
        )
    };
    let report = diff(&parse(&with_flag(SER_OLD)), &parse(&with_flag(SER_NEW)));
    let messages: Vec<&str> = report.changes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        messages,
        ["serialization changed (Vault): borsh -> bytemuck: moves `Vault.bal` from offset 1 to 8 (zero-copy layout)"]
    );
}

#[test]
fn serialization_change_with_only_trailing_padding_is_dangerous() {
    // u64 then u8: 7 bytes of padding at the end, every field keeps its offset.
    let tail = |s: &str| {
        s.replace(
            r#""type":"u64"}]"#,
            r#""type":"u64"},{"name":"flag","type":"u8"}]"#,
        )
    };
    assert_eq!(
        max_sev(&tail(SER_OLD), &tail(SER_NEW)),
        Some(Severity::Dangerous)
    );
}

// Wrapper<u64> to Wrapper<u32> changes the layout even though the type name is the same.
const GEN_OLD: &str = r#"{
  "address":"A","metadata":{"name":"g","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"f","discriminator":[1],"accounts":[],
    "args":[{"name":"w","type":{"defined":{"name":"Wrapper","generics":[{"kind":"type","type":"u64"}]}}}]}],
  "types":[{"name":"Wrapper","type":{"kind":"struct","fields":[{"name":"inner","type":{"generic":"T"}}]}}]
}"#;
const GEN_NEW: &str = r#"{
  "address":"A","metadata":{"name":"g","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"f","discriminator":[1],"accounts":[],
    "args":[{"name":"w","type":{"defined":{"name":"Wrapper","generics":[{"kind":"type","type":"u32"}]}}}]}],
  "types":[{"name":"Wrapper","type":{"kind":"struct","fields":[{"name":"inner","type":{"generic":"T"}}]}}]
}"#;

#[test]
fn generic_arg_change_is_breaking() {
    let r = diff(&parse(GEN_OLD), &parse(GEN_NEW));
    assert!(
        r.has_breaking(),
        "Wrapper<u64>->Wrapper<u32> must be breaking; got {:?}",
        r.changes
    );
}

// Native programs may give no discriminator; an unchanged one must diff clean.
const NODISC: &str = r#"{
  "address":"A","metadata":{"name":"n","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"transfer","accounts":[],"args":[{"name":"amount","type":"u64"}]}]
}"#;

#[test]
fn missing_discriminator_deserializes_and_is_stable() {
    let idl = parse(NODISC);
    let r = diff(&idl, &idl);
    assert!(!r.has_breaking());
    assert_eq!(r.exit_code(), 0);
}

// `Amount = u64` to `u32` on a reachable alias changes the width everywhere it is used.
const ALIAS_OLD: &str = r#"{
  "address":"A","metadata":{"name":"a","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"x","discriminator":[1],"accounts":[],
    "args":[{"name":"amt","type":{"defined":{"name":"Amount"}}}]}],
  "types":[{"name":"Amount","type":{"kind":"type","alias":"u64"}}]
}"#;
const ALIAS_NEW: &str = r#"{
  "address":"A","metadata":{"name":"a","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"x","discriminator":[1],"accounts":[],
    "args":[{"name":"amt","type":{"defined":{"name":"Amount"}}}]}],
  "types":[{"name":"Amount","type":{"kind":"type","alias":"u32"}}]
}"#;

#[test]
fn alias_target_change_is_breaking() {
    let r = diff(&parse(ALIAS_OLD), &parse(ALIAS_NEW));
    assert!(
        r.has_breaking(),
        "Amount=u64->u32 must be breaking; got {:?}",
        r.changes
    );
}

// Instructions match by name, so a duplicate name would hide drift on all but one.
const DUP: &str = r#"{
  "address":"A","metadata":{"name":"d","version":"1","spec":"0.1.0"},
  "instructions":[
    {"name":"swap","discriminator":[1],"accounts":[],"args":[]},
    {"name":"swap","discriminator":[2],"accounts":[],"args":[]}
  ]
}"#;

#[test]
fn duplicate_instruction_name_is_flagged() {
    let idl = parse(DUP);
    let r = diff(&idl, &idl);
    assert!(
        r.changes
            .iter()
            .any(|c| c.message.contains("duplicate instruction name")),
        "expected a duplicate-name finding; got {:?}",
        r.changes
    );
}

// Writing default fields out explicitly must change nothing; if this regresses, every
// diff floods with false positives.
const OMITTED: &str = r#"{
  "address":"A","metadata":{"name":"o","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"swap","discriminator":[1],
    "accounts":[{"name":"pool","writable":true}]}]
}"#;
const EXPLICIT: &str = r#"{
  "address":"A","metadata":{"name":"o","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"swap","discriminator":[1],
    "accounts":[{"name":"pool","writable":true,"signer":false,"optional":false}],
    "args":[]}],
  "accounts":[],"events":[],"errors":[],"types":[]
}"#;

#[test]
fn default_omission_produces_no_changes() {
    let r = diff(&parse(OMITTED), &parse(EXPLICIT));
    assert!(
        r.changes.is_empty(),
        "default-omission must normalize to zero changes; got {:?}",
        r.changes
    );
    assert_eq!(r.exit_code(), 0);
}

/// A real Codama `RootNode` must diff clean against itself.
#[test]
fn codama_order_engine_self_diff_is_empty() {
    let src = include_str!("fixtures/codama_order_engine.json");
    let idl = parse(src);
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
    assert!(
        r.changes.is_empty(),
        "Codama self-diff must be empty; got {:?}",
        r.changes
    );
    assert_eq!(r.exit_code(), 0);
}

/// Codama `program.events` land in `Idl.events` with their discriminator bytes.
#[test]
fn codama_events_are_top_level_with_discriminator() {
    let idl = parse(include_str!("fixtures/codama_tiny.json"));
    assert_eq!(idl.events.len(), 1);
    assert_eq!(idl.events[0].name, "pong");
    assert_eq!(
        idl.events[0].discriminator,
        vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11]
    );
    let r = diff(&idl, &idl);
    assert!(r.changes.is_empty(), "got {:?}", r.changes);
}

// Anchor wraps self-CPI events in an 8-byte tag, Pinocchio often in 1 byte; switching
// moves where the payload starts.
const EVENT_ENV_8: &str = r#"{
  "address":"A","metadata":{"name":"e","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"noop","discriminator":[1],"accounts":[],"args":[]}],
  "events":[{
    "name":"Trade",
    "discriminator":[1,2,3,4,5,6,7,8],
    "envelope_discriminator":[228,69,165,46,81,203,154,29],
    "envelope_payload_offset":8
  }]
}"#;
const EVENT_ENV_1: &str = r#"{
  "address":"A","metadata":{"name":"e","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"noop","discriminator":[1],"accounts":[],"args":[]}],
  "events":[{
    "name":"Trade",
    "discriminator":[1,2,3,4,5,6,7,8],
    "envelope_discriminator":[254],
    "envelope_payload_offset":1
  }]
}"#;

#[test]
fn event_envelope_length_8_to_1_is_breaking() {
    let r = diff(&parse(EVENT_ENV_8), &parse(EVENT_ENV_1));
    assert!(
        r.has_breaking(),
        "8-byte -> 1-byte event envelope must be breaking; got {:?}",
        r.changes
    );
    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Breaking
                && c.message.contains("envelope discriminator LENGTH")
                && c.message.contains("8->1")
        }),
        "expected envelope LENGTH finding; got {:?}",
        r.changes
    );
    assert_eq!(r.exit_code(), 1);
}

// --- Discriminator collisions (Shipstern dispatches shared discriminators by account count) ---

fn bare_idl(instructions: Vec<Instruction>) -> Idl {
    Idl {
        address: "A".into(),
        metadata: Metadata {
            name: "t".into(),
            version: "1".into(),
            spec: "0.1.0".into(),
        },
        instructions,
        accounts: vec![],
        events: vec![],
        errors: vec![],
        types: vec![],
        format: Format::Anchor,
    }
}

fn acct(name: &str) -> AccountItem {
    AccountItem::Single(Account {
        name: name.into(),
        writable: false,
        signer: false,
        optional: false,
        address: None,
    })
}

fn opt_acct(name: &str) -> AccountItem {
    AccountItem::Single(Account {
        name: name.into(),
        writable: false,
        signer: false,
        optional: true,
        address: None,
    })
}

fn ix(name: &str, disc: Vec<u8>, accounts: Vec<AccountItem>) -> Instruction {
    Instruction {
        name: name.into(),
        discriminator: disc,
        discriminator_offset: 0,
        accounts,
        args: vec![],
    }
}

#[test]
fn same_disc_same_account_count_is_breaking() {
    let disc = vec![0x01, 0x02, 0x03, 0x04];
    let old = bare_idl(vec![ix("swap_a", disc.clone(), vec![acct("a"), acct("b")])]);
    let new = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix("swap_b", disc, vec![acct("c"), acct("d")]),
    ]);
    let r = diff(&old, &new);
    assert!(
        r.has_breaking(),
        "same disc + same account count must be breaking; got {:?}",
        r.changes
    );
    assert!(
        r.changes
            .iter()
            .any(|c| c.message.contains("unresolvable collision")),
        "expected unresolvable collision; got {:?}",
        r.changes
    );
    assert_eq!(r.exit_code(), 1);
}

#[test]
fn same_disc_different_account_count_is_not_breaking() {
    let disc = vec![0x01, 0x02, 0x03, 0x04];
    let idl = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix("swap_b", disc, vec![acct("a"), acct("b"), acct("c")]),
    ]);
    let r = diff(&idl, &idl);
    assert!(
        !r.has_breaking(),
        "fan-out by account count is resolvable; got {:?}",
        r.changes
    );
}

#[test]
fn disambiguation_order_change_is_dangerous() {
    let disc = vec![0xaa, 0xbb];
    // Counts {2,4} become {2,3} by dropping optional tail accounts, which alone is only
    // Dangerous.
    let old = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix(
            "swap_b",
            disc.clone(),
            vec![acct("a"), acct("b"), acct("c"), opt_acct("d")],
        ),
    ]);
    let new = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix("swap_b", disc, vec![acct("a"), acct("b"), acct("c")]),
    ]);
    let r = diff(&old, &new);
    assert!(
        !r.has_breaking(),
        "order change alone must not be breaking; got {:?}",
        r.changes
    );
    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Dangerous && c.message.contains("disambiguation order changed")
        }),
        "expected disambiguation order Dangerous; got {:?}",
        r.changes
    );
}

// --- Discriminator encoding normalization (base16 / base58 / base64) ---

#[test]
fn same_discriminator_two_encodings_is_no_change() {
    // Same underlying bytes [1,2,3,4] under every Codama encoding.
    let expected = vec![1u8, 2, 3, 4];
    let from_hex = decode_encoded_bytes("base16", "01020304");
    let from_b58 = decode_encoded_bytes("base58", "2VfUX");
    let from_b64 = decode_encoded_bytes("base64", "AQIDBA==");
    assert_eq!(from_hex, expected, "base16");
    assert_eq!(from_b58, expected, "base58");
    assert_eq!(from_b64, expected, "base64");
    assert_eq!(from_hex, from_b58);
    assert_eq!(from_hex, from_b64);

    // The same account discriminator in base16 and base58 must not count as a change.
    let base16_idl = r#"{
      "kind":"rootNode","standard":"codama","version":"1.5.1",
      "program":{
        "kind":"programNode","name":"enc","publicKey":"11111111111111111111111111111111","version":"0.1.0",
        "accounts":[{
          "kind":"accountNode","name":"vault",
          "data":{"kind":"structTypeNode","fields":[
            {"kind":"structFieldTypeNode","name":"discriminator","defaultValueStrategy":"omitted",
             "type":{"kind":"fixedSizeTypeNode","size":4,"type":{"kind":"bytesTypeNode"}},
             "defaultValue":{"kind":"bytesValueNode","data":"01020304","encoding":"base16"}},
            {"kind":"structFieldTypeNode","name":"bal",
             "type":{"kind":"numberTypeNode","format":"u64","endian":"le"}}
          ]},
          "discriminators":[{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}]
        }],
        "instructions":[],"definedTypes":[],"pdas":[],"events":[],"errors":[]
      },
      "additionalPrograms":[]
    }"#;
    let base58_idl = r#"{
      "kind":"rootNode","standard":"codama","version":"1.5.1",
      "program":{
        "kind":"programNode","name":"enc","publicKey":"11111111111111111111111111111111","version":"0.1.0",
        "accounts":[{
          "kind":"accountNode","name":"vault",
          "data":{"kind":"structTypeNode","fields":[
            {"kind":"structFieldTypeNode","name":"discriminator","defaultValueStrategy":"omitted",
             "type":{"kind":"fixedSizeTypeNode","size":4,"type":{"kind":"bytesTypeNode"}},
             "defaultValue":{"kind":"bytesValueNode","data":"2VfUX","encoding":"base58"}},
            {"kind":"structFieldTypeNode","name":"bal",
             "type":{"kind":"numberTypeNode","format":"u64","endian":"le"}}
          ]},
          "discriminators":[{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}]
        }],
        "instructions":[],"definedTypes":[],"pdas":[],"events":[],"errors":[]
      },
      "additionalPrograms":[]
    }"#;

    let a = parse(base16_idl);
    let b = parse(base58_idl);
    assert_eq!(a.accounts[0].discriminator, expected);
    assert_eq!(b.accounts[0].discriminator, expected);

    let r = diff(&a, &b);
    assert!(
        !r.has_breaking(),
        "same disc under base16 vs base58 must not break; got {:?}",
        r.changes
    );
    assert!(
        !r.changes
            .iter()
            .any(|c| c.message.contains("discriminator")),
        "zero discriminator-change entries expected; got {:?}",
        r.changes
    );
}

// --- Event payload reachability ---

fn typedef_struct(name: &str, fields: Vec<Field>) -> TypeDef {
    TypeDef {
        name: name.into(),
        serialization: "borsh".into(),
        repr: None,
        generics: vec![],
        ty: TypeDefTy::Struct { fields },
    }
}

fn field(name: &str, ty: Type) -> Field {
    Field {
        name: name.into(),
        ty,
    }
}

fn defined(name: &str) -> Type {
    Type::Defined {
        name: name.into(),
        generics: vec![],
    }
}

fn event_named(name: &str) -> Event {
    Event {
        name: name.into(),
        discriminator: vec![1, 2, 3, 4, 5, 6, 7, 8],
        envelope_discriminator: vec![],
        envelope_payload_offset: None,
    }
}

fn idl_with(events: Vec<Event>, types: Vec<TypeDef>) -> Idl {
    Idl {
        address: "A".into(),
        metadata: Metadata {
            name: "t".into(),
            version: "1".into(),
            spec: "0.1.0".into(),
        },
        instructions: vec![],
        accounts: vec![],
        events,
        errors: vec![],
        types,
        format: Format::Anchor,
    }
}

#[test]
fn event_only_type_field_change_is_breaking() {
    // Only the Trade event's payload references TradeDetail; that is enough to reach it.
    let old = idl_with(
        vec![event_named("Trade")],
        vec![
            typedef_struct("Trade", vec![field("detail", defined("TradeDetail"))]),
            typedef_struct("TradeDetail", vec![field("px", Type::U16)]),
        ],
    );
    let new = idl_with(
        vec![event_named("Trade")],
        vec![
            typedef_struct("Trade", vec![field("detail", defined("TradeDetail"))]),
            typedef_struct("TradeDetail", vec![field("px", Type::U32)]),
        ],
    );
    let r = diff(&old, &new);
    assert!(
        r.has_breaking(),
        "event-only Defined type field change must be Breaking; got {:?}",
        r.changes
    );
    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Breaking
                && c.message.contains("TradeDetail")
                && c.message.contains("Borsh layout")
        }),
        "expected Breaking TradeDetail Borsh layout; got {:?}",
        r.changes
    );
    assert!(
        !r.changes.iter().any(|c| c.message.contains("UNREACHED")),
        "must not be Cosmetic/UNREACHED; got {:?}",
        r.changes
    );
    assert_eq!(r.exit_code(), 1);
}

#[test]
fn truly_dead_type_change_is_cosmetic() {
    // Nothing references TradeDetail, so widening its field stays Cosmetic.
    let old = idl_with(
        vec![event_named("Trade")],
        vec![
            typedef_struct("Trade", vec![field("qty", Type::U64)]),
            typedef_struct("TradeDetail", vec![field("px", Type::U16)]),
        ],
    );
    let new = idl_with(
        vec![event_named("Trade")],
        vec![
            typedef_struct("Trade", vec![field("qty", Type::U64)]),
            typedef_struct("TradeDetail", vec![field("px", Type::U32)]),
        ],
    );
    let r = diff(&old, &new);
    assert!(
        !r.has_breaking(),
        "truly dead type must not be Breaking; got {:?}",
        r.changes
    );
    assert!(
        r.changes
            .iter()
            .any(|c| { c.severity == Severity::Cosmetic && c.message.contains("UNREACHED") }),
        "expected Cosmetic UNREACHED; got {:?}",
        r.changes
    );
}

// --- Map, set and tuple element types are layout ---

fn idl_account(account: &str, types: Vec<TypeDef>) -> Idl {
    Idl {
        address: "A".into(),
        metadata: Metadata {
            name: "t".into(),
            version: "1".into(),
            spec: "0.1.0".into(),
        },
        instructions: vec![],
        accounts: vec![NamedDisc {
            name: account.into(),
            discriminator: vec![1, 2, 3, 4, 5, 6, 7, 8],
            discriminator_offset: 0,
            size: None,
            payload_offset: None,
        }],
        events: vec![],
        errors: vec![],
        types,
        format: Format::Anchor,
    }
}

#[test]
fn map_value_widen_on_reachable_field_is_breaking() {
    let old = idl_account(
        "Vault",
        vec![typedef_struct(
            "Vault",
            vec![field(
                "balances",
                Type::Map(Box::new(Type::Pubkey), Box::new(Type::U64)),
            )],
        )],
    );
    let new = idl_account(
        "Vault",
        vec![typedef_struct(
            "Vault",
            vec![field(
                "balances",
                Type::Map(Box::new(Type::Pubkey), Box::new(Type::U128)),
            )],
        )],
    );
    let r = diff(&old, &new);
    assert!(
        r.has_breaking(),
        "map value widen must Breaking; got {:?}",
        r.changes
    );
    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Breaking
                && (c.message.contains("balances")
                    || c.message.contains("Vault#0")
                    || c.message.contains("Map<"))
        }),
        "expected Breaking mentioning the balances field; got {:?}",
        r.changes
    );
}

#[test]
fn set_element_change_is_breaking() {
    let old = idl_account(
        "Book",
        vec![typedef_struct(
            "Book",
            vec![field("ids", Type::Set(Box::new(Type::U32)))],
        )],
    );
    let new = idl_account(
        "Book",
        vec![typedef_struct(
            "Book",
            vec![field("ids", Type::Set(Box::new(Type::U64)))],
        )],
    );
    let r = diff(&old, &new);
    assert!(
        r.has_breaking(),
        "set element change must Breaking; got {:?}",
        r.changes
    );
}

#[test]
fn tuple_element_reorder_is_breaking() {
    let old = idl_account(
        "Pair",
        vec![typedef_struct(
            "Pair",
            vec![field("xy", Type::Tuple(vec![Type::U8, Type::U64]))],
        )],
    );
    let new = idl_account(
        "Pair",
        vec![typedef_struct(
            "Pair",
            vec![field("xy", Type::Tuple(vec![Type::U64, Type::U8]))],
        )],
    );
    let r = diff(&old, &new);
    assert!(
        r.has_breaking(),
        "tuple element reorder must Breaking; got {:?}",
        r.changes
    );
}

#[test]
fn map_defined_value_stays_reachable() {
    // Slot is reachable only as a map value inside Vault.
    let old = idl_account(
        "Vault",
        vec![
            typedef_struct(
                "Vault",
                vec![field(
                    "bySlot",
                    Type::Map(Box::new(Type::U64), Box::new(defined("Slot"))),
                )],
            ),
            typedef_struct("Slot", vec![field("lamports", Type::U16)]),
        ],
    );
    let new = idl_account(
        "Vault",
        vec![
            typedef_struct(
                "Vault",
                vec![field(
                    "bySlot",
                    Type::Map(Box::new(Type::U64), Box::new(defined("Slot"))),
                )],
            ),
            typedef_struct("Slot", vec![field("lamports", Type::U32)]),
        ],
    );
    let r = diff(&old, &new);
    assert!(
        r.has_breaking(),
        "Defined-only-via-Map-value must stay reachable/Breaking; got {:?}",
        r.changes
    );
    assert!(
        !r.changes.iter().any(|c| c.message.contains("UNREACHED")),
        "must not be Cosmetic/UNREACHED; got {:?}",
        r.changes
    );
}

#[test]
fn unchanged_map_set_tuple_is_no_change() {
    let fields = vec![
        field(
            "balances",
            Type::Map(Box::new(Type::Pubkey), Box::new(Type::U64)),
        ),
        field("ids", Type::Set(Box::new(Type::U32))),
        field("xy", Type::Tuple(vec![Type::U8, Type::U64])),
    ];
    let old = idl_account("Vault", vec![typedef_struct("Vault", fields.clone())]);
    let new = idl_account("Vault", vec![typedef_struct("Vault", fields)]);
    let r = diff(&old, &new);
    assert!(
        r.changes.is_empty(),
        "unchanged Map/Set/Tuple must be zero changes; got {:?}",
        r.changes
    );
}

// --- Real Codama pair: limit_order v1 to v2 ---

fn limit_order_v1() -> Idl {
    parse(include_str!("idls/limit_order_v1.json"))
}

fn limit_order_v2() -> Idl {
    parse(include_str!("idls/limit_order_v2.json"))
}

#[test]
fn limit_order_v1_to_v2_is_breaking() {
    let old = limit_order_v1();
    let new = limit_order_v2();
    let r = diff(&old, &new);

    assert!(
        r.has_breaking(),
        "v1->v2 must be Breaking; got {:?}",
        r.changes
    );
    assert_eq!(r.exit_code(), 1);

    // Fee account: four u64 fees become two u16 bps fields.
    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Breaking
                && c.message.contains("fee#")
                && c.message.contains("makerFee: u64 -> normalFeeBps: u16")
        }),
        "expected fee account layout Breaking; got {:?}",
        r.changes
            .iter()
            .filter(|c| c.message.contains("fee"))
            .collect::<Vec<_>>()
    );

    // v1 events are bare structs; v2 wraps them in a self-CPI envelope.
    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Breaking && c.message.to_lowercase().contains("envelope")
        }),
        "expected event-envelope Breaking for bare->CPI-wrapped; got {:?}",
        r.changes
    );

    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Breaking && c.message.contains("instruction removed: initFee")
        }),
        "expected initFee removal; got {:?}",
        r.changes
    );
    assert!(
        r.changes.iter().any(|c| {
            c.severity == Severity::Breaking
                && c.message
                    .contains("instruction removed: cancelExpiredOrder")
        }),
        "expected cancelExpiredOrder removal; got {:?}",
        r.changes
    );

    assert!(
        r.changes
            .iter()
            .any(|c| { c.severity == Severity::Additive && c.message.contains("cancelDustOrder") }),
        "expected Additive cancelDustOrder; got {:?}",
        r.changes
    );
}

#[test]
fn limit_order_idls_have_no_unexpected_generics() {
    use idl_drift::coverage;
    use idl_drift::model::Type;

    for (label, idl) in [("v1", limit_order_v1()), ("v2", limit_order_v2())] {
        let report = coverage(&idl);
        assert!(
            report.fully_mapped(),
            "{label}: unexpected reachable Generic: {:?}",
            report.unmapped
        );
    }

    // A u32-prefixed bytes node maps to Type::Bytes, not Generic.
    let v2 = limit_order_v2();
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
    assert_eq!(
        swap.ty,
        Type::Bytes,
        "swapData must map to Bytes, not Generic; got {:?}",
        swap.ty
    );
}

#[test]
fn codama_0_13_idl_loads() {
    // `npx codama convert` output; codama-nodes 0.9.1 rejected it (AmountTypeNode `seeds`).
    let src = std::fs::read_to_string("fixtures/live/voltr_vault/idl.js-codama.json")
        .expect("read fixture");
    let idl = parse(&src);
    let receipt = idl
        .accounts
        .iter()
        .find(|a| a.name == "adaptorAddReceipt")
        .expect("adaptorAddReceipt account");
    assert_eq!(receipt.discriminator, [105, 99, 219, 155, 77, 241, 7, 119]);
    assert!(idl_drift::coverage(&idl).fully_mapped());
}

const ZC_PACKED: &str = r#"{
  "address":"A","metadata":{"name":"zc","version":"0.1.0","spec":"0.1.0"},
  "accounts":[{"name":"Pool","discriminator":[1,1,1,1,1,1,1,1]}],
  "types":[{"name":"Pool","serialization":"bytemuck","repr":{"kind":"c","packed":true},
    "type":{"kind":"struct","fields":[{"name":"a","type":"u8"},{"name":"b","type":"u64"}]}}]
}"#;

#[test]
fn repr_change_on_reachable_type_is_breaking() {
    let unpacked = ZC_PACKED.replace(r#","packed":true"#, "");
    let report = diff(&parse(ZC_PACKED), &parse(&unpacked));
    assert!(report.has_breaking());
    assert!(report.changes.iter().any(|c| c.message
        == "repr changed (Pool): repr(C, packed) -> repr(C): moves `Pool.b` from offset 1 to 8 (zero-copy layout)"));
}

#[test]
fn anchor_spec_layout_fields_parse() {
    use idl_drift::model::{GenericParam, Repr, ReprKind};
    let idl = parse(
        r#"{
  "address":"A","metadata":{"name":"t","version":"0.1.0","spec":"0.1.0"},
  "instructions":[{"name":"go","discriminator":[1],
    "accounts":[{"name":"system_program","address":"11111111111111111111111111111111"}],
    "args":[{"name":"x","type":"f64"},{"name":"y","type":{"coption":"f32"}}]}],
  "types":[{"name":"Buf","generics":[{"kind":"type","name":"T"},{"kind":"const","name":"N","type":"usize"}],
    "repr":{"kind":"c","align":8},
    "type":{"kind":"struct","fields":[{"name":"v","type":{"array":[{"generic":"T"},{"generic":"N"}]}}]}}]
}"#,
    );
    let ix = &idl.instructions[0];
    let AccountItem::Single(acct) = &ix.accounts[0] else {
        panic!("expected a single account");
    };
    assert_eq!(
        acct.address.as_deref(),
        Some("11111111111111111111111111111111")
    );
    assert_eq!(ix.args[0].ty, Type::F64);
    assert_eq!(ix.args[1].ty, Type::COption(Box::new(Type::F32)));
    let buf = &idl.types[0];
    assert_eq!(
        buf.repr,
        Some(Repr {
            kind: ReprKind::C,
            packed: false,
            align: Some(8)
        })
    );
    assert_eq!(
        buf.generics[1],
        GenericParam::Const {
            name: "N".into(),
            ty: "usize".into()
        }
    );
}

#[test]
fn rename_message_names_both_fields() {
    let renamed = AMM_OLD.replace(r#""name":"feeBps""#, r#""name":"tradeFeeBps""#);
    let report = diff(&parse(AMM_OLD), &parse(&renamed));
    let messages: Vec<&str> = report.changes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        messages,
        ["FIELD changed (Pool#1) via reachable type: feeBps renamed to tradeFeeBps (u16)"]
    );
}

/// A Pool account whose `mode` field is the enum `Mode`, with `extra` spliced into `types`.
fn pool_with_enum(variant_fields: &str, extra: &str) -> String {
    format!(
        r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
  "accounts":[{{"name":"Pool","discriminator":[1,1,1,1,1,1,1,1]}}],
  "types":[{{"name":"Pool","type":{{"kind":"struct","fields":[{{"name":"mode","type":{{"defined":{{"name":"Mode"}}}}}}]}}}},
    {{"name":"Mode","type":{{"kind":"enum","variants":[{{"name":"Fixed","fields":{variant_fields}}}]}}}}{extra}]}}"#
    )
}

#[test]
fn type_reached_only_through_an_enum_payload_is_checked() {
    let fee = |ty: &str| {
        format!(
            r#",{{"name":"Fee","type":{{"kind":"struct","fields":[{{"name":"bps","type":"{ty}"}}]}}}}"#
        )
    };
    let old = pool_with_enum(r#"[{"defined":{"name":"Fee"}}]"#, &fee("u16"));
    let new = pool_with_enum(r#"[{"defined":{"name":"Fee"}}]"#, &fee("u64"));
    assert_eq!(max_sev(&old, &new), Some(Severity::Breaking));
}

#[test]
fn type_reached_only_through_an_alias_is_checked() {
    let idl = |ty: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
  "accounts":[{{"name":"Pool","discriminator":[1,1,1,1,1,1,1,1]}}],
  "types":[{{"name":"Pool","type":{{"kind":"struct","fields":[{{"name":"fee","type":{{"defined":{{"name":"FeeAlias"}}}}}}]}}}},
    {{"name":"FeeAlias","type":{{"kind":"type","alias":{{"defined":{{"name":"Fee"}}}}}}}},
    {{"name":"Fee","type":{{"kind":"struct","fields":[{{"name":"bps","type":"{ty}"}}]}}}}]}}"#
        )
    };
    assert_eq!(max_sev(&idl("u16"), &idl("u64")), Some(Severity::Breaking));
}

#[test]
fn inline_variant_payload_change_is_breaking() {
    let report = diff(
        &parse(&pool_with_enum(r#"["u16"]"#, "")),
        &parse(&pool_with_enum(r#"["u64"]"#, "")),
    );
    let messages: Vec<&str> = report.changes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        messages,
        ["enum variant payload changed (Mode#0 Fixed): (u16) -> (u64) (Borsh layout)"]
    );
}

#[test]
fn u256_and_i256_parse_and_diff() {
    let idl = |ty: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
  "instructions":[{{"name":"go","discriminator":[1],"args":[{{"name":"x","type":"{ty}"}}]}}]}}"#
        )
    };
    let report = diff(&parse(&idl("u256")), &parse(&idl("i256")));
    let messages: Vec<&str> = report.changes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        messages,
        ["arg changed (go#0): x: u256 -> i256 (Borsh layout)"]
    );
    assert!(idl_drift::coverage(&parse(&idl("u256"))).fully_mapped());
}

#[test]
fn legacy_discriminators_match_the_real_ones() {
    // The same program as a legacy Anchor IDL (no discriminators) and as Codama (with them).
    let legacy =
        parse(&std::fs::read_to_string("fixtures/real/limit_order_v1_onchain.json").unwrap());
    let codama = limit_order_v1();
    let key = |name: &str| name.to_lowercase();
    let pairs = |a: Vec<(String, Vec<u8>)>, b: Vec<(String, Vec<u8>)>| {
        let b: std::collections::BTreeMap<_, _> =
            b.into_iter().map(|(n, d)| (key(&n), d)).collect();
        assert!(!a.is_empty());
        for (name, disc) in a {
            assert_eq!(disc.len(), 8, "{name}");
            assert_eq!(Some(&disc), b.get(&key(&name)), "{name}");
        }
    };
    let ixs = |idl: &Idl| {
        idl.instructions
            .iter()
            .map(|i| (i.name.clone(), i.discriminator.clone()))
            .collect()
    };
    let accts = |idl: &Idl| {
        idl.accounts
            .iter()
            .map(|a| (a.name.clone(), a.discriminator.clone()))
            .collect()
    };
    let events = |idl: &Idl| {
        idl.events
            .iter()
            .map(|e| (e.name.clone(), e.discriminator.clone()))
            .collect()
    };
    pairs(ixs(&legacy), ixs(&codama));
    pairs(accts(&legacy), accts(&codama));
    pairs(events(&legacy), events(&codama));
}

#[test]
fn legacy_idl_with_address_only_metadata_parses() {
    let idl = parse(
        r#"{"version":"0.1.0","name":"counter","metadata":{"address":"Count111"},
            "instructions":[{"name":"increment","accounts":[],"args":[]}]}"#,
    );
    assert_eq!(idl.address, "Count111");
    assert_eq!(idl.metadata.name, "counter");
    assert_eq!(idl.instructions[0].discriminator.len(), 8);
}

#[test]
fn same_program_in_two_formats_diffs_clean() {
    // Legacy Anchor (camelCase, no discriminators) against Codama (camelCase types too).
    let legacy =
        parse(&std::fs::read_to_string("fixtures/real/limit_order_v1_onchain.json").unwrap());
    let report = diff(&legacy, &limit_order_v1());
    assert!(report.changes.is_empty(), "{:#?}", report.changes);
}

#[test]
fn cross_format_diff_still_reports_real_changes() {
    let codama = limit_order_v1();
    let mut legacy: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("fixtures/real/limit_order_v1_onchain.json").unwrap(),
    )
    .unwrap();
    // Widen the first arg of initializeOrder (u64) in the legacy file.
    legacy["instructions"][0]["args"][0]["type"] = serde_json::json!("u128");
    let report = diff(&parse(&legacy.to_string()), &codama);
    let messages: Vec<&str> = report.changes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        messages,
        ["arg changed (initializeOrder#0): makingAmount: u128 -> u64 (Borsh layout)"]
    );
}

#[test]
fn codama_conversion_that_drops_zero_copy_layout_is_flagged() {
    // `npx codama convert` drops serialization and repr. Only types whose padding moves
    // fields misread; the rest are byte-identical.
    let read = |p: &str| parse(&std::fs::read_to_string(p).unwrap());
    let report = diff(
        &read("fixtures/live/voltr_vault/idl.json"),
        &read("fixtures/live/voltr_vault/idl.js-codama.json"),
    );
    let at = |sev: Severity| -> Vec<&str> {
        report
            .changes
            .iter()
            .filter(|c| c.severity == sev)
            .map(|c| c.message.split(':').next().unwrap_or_default())
            .collect()
    };
    assert_eq!(
        at(Severity::Breaking),
        ["serialization and repr changed (adaptorAddReceipt)"]
    );
    assert_eq!(
        at(Severity::Dangerous),
        ["serialization and repr changed (requestWithdrawVaultReceipt)"]
    );
    assert_eq!(at(Severity::Cosmetic).len(), 11);
    assert_eq!(report.changes.len(), 13);
}

#[test]
fn non_ascii_hex_discriminator_does_not_panic() {
    // Slicing the hex string by byte index used to split `é` and panic.
    let decoded = decode_encoded_bytes("base16", "éa");
    assert_eq!(decoded, "éa".as_bytes());
    assert_eq!(decode_encoded_bytes("base16", "0a1"), [0x00, 0xa1]);
}

#[test]
fn new_instruction_reusing_a_discriminator_is_breaking() {
    let old = r#"{"address":"A","metadata":{"name":"t","version":"0","spec":"0.1.0"},
      "instructions":[{"name":"a","discriminator":[1],"accounts":[{"name":"x"}],"args":[]}]}"#;
    let new = r#"{"address":"A","metadata":{"name":"t","version":"0","spec":"0.1.0"},
      "instructions":[{"name":"a","discriminator":[1],"accounts":[{"name":"x"}],"args":[]},
        {"name":"b","discriminator":[1],"accounts":[{"name":"x"},{"name":"y"}],"args":[{"name":"n","type":"u64"}]}]}"#;
    let report = diff(&parse(old), &parse(new));
    assert!(report
        .changes
        .iter()
        .any(|c| c.severity == Severity::Breaking
            && c.message
                .starts_with("new instruction b reuses discriminator 01 of [a]")));
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
    let base = parse(&codama_enum_arg("u8", None));
    assert!(diff(&base, &parse(&codama_enum_arg("u8", None)))
        .changes
        .is_empty());
    for new in [codama_enum_arg("u32", None), codama_enum_arg("u8", Some(5))] {
        let report = diff(&base, &parse(&new));
        assert!(report.has_breaking(), "{:?}", report.changes);
    }
}

/// A Codama program with one defined type `ext` passed as an instruction arg.
fn codama_defined_arg(ext: &str) -> String {
    format!(
        r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
  "program":{{"kind":"programNode","name":"t","publicKey":"11111111111111111111111111111111","version":"0",
    "accounts":[],"pdas":[],"errors":[],
    "definedTypes":[{{"kind":"definedTypeNode","name":"ext","type":{ext}}}],
    "instructions":[{{"kind":"instructionNode","name":"set","accounts":[],
      "arguments":[{{"kind":"instructionArgumentNode","name":"ext","type":{{"kind":"definedTypeLinkNode","name":"ext"}}}}],
      "discriminators":[]}}]}}}}"#
    )
}

#[test]
fn codama_wrapper_on_defined_type_is_kept() {
    let st = r#"{"kind":"structTypeNode","fields":[{"kind":"structFieldTypeNode","name":"x","type":{"kind":"numberTypeNode","format":"u64","endian":"le"}}]}"#;
    let prefixed = format!(
        r#"{{"kind":"sizePrefixTypeNode","type":{st},"prefix":{{"kind":"numberTypeNode","format":"u16","endian":"le"}}}}"#
    );
    let report = diff(
        &parse(&codama_defined_arg(st)),
        &parse(&codama_defined_arg(&prefixed)),
    );
    assert!(report.has_breaking(), "{:?}", report.changes);
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

// `Inner` has no padding of its own, but unpacking it moves `v` inside zero-copy `Outer`.
const NESTED_PACKED: &str = r#"{
  "address":"A","metadata":{"name":"n","version":"0","spec":"0.1.0"},
  "accounts":[{"name":"Outer","discriminator":[1,1,1,1,1,1,1,1]}],
  "types":[{"name":"Outer","serialization":"bytemuck","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
      {"name":"tag","type":"u8"},{"name":"inner","type":{"defined":{"name":"Inner"}}}]}},
    {"name":"Inner","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[{"name":"v","type":"u64"}]}}]
}"#;

#[test]
fn repr_change_on_nested_type_that_moves_outer_fields_is_breaking() {
    let unpacked = NESTED_PACKED.replace(r#","packed":true"#, "");
    let report = diff(&parse(NESTED_PACKED), &parse(&unpacked));
    let messages: Vec<&str> = report.changes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        messages,
        ["repr changed (Inner): repr(C, packed) -> repr(C): moves `Outer.inner.v` from offset 1 to 8 (zero-copy layout)"]
    );
}

#[test]
fn repr_change_on_borsh_only_type_is_cosmetic() {
    // Only an instruction arg holds `Inner`, and Borsh ignores repr.
    let old = r#"{
  "address":"A","metadata":{"name":"b","version":"0","spec":"0.1.0"},
  "instructions":[{"name":"f","discriminator":[1],"accounts":[],
    "args":[{"name":"i","type":{"defined":{"name":"Inner"}}}]}],
  "types":[{"name":"Inner","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[
    {"name":"a","type":"u8"},{"name":"v","type":"u64"}]}}]
}"#;
    let new = old.replace(r#","packed":true"#, "");
    assert_eq!(max_sev(old, &new), Some(Severity::Cosmetic));
}

#[test]
fn removing_several_optional_tail_accounts_is_dangerous() {
    let disc = vec![1];
    let old = bare_idl(vec![ix(
        "f",
        disc.clone(),
        vec![acct("a"), opt_acct("b"), opt_acct("c")],
    )]);
    let report = diff(
        &old,
        &bare_idl(vec![ix("f", disc.clone(), vec![acct("a")])]),
    );
    let max = report.changes.iter().map(|c| c.severity).max();
    assert_eq!(max, Some(Severity::Dangerous), "{:?}", report.changes);
    // A required account in the removed block still breaks.
    let old = bare_idl(vec![ix(
        "f",
        disc.clone(),
        vec![acct("a"), acct("b"), opt_acct("c")],
    )]);
    let report = diff(&old, &bare_idl(vec![ix("f", disc, vec![acct("a")])]));
    assert!(report.has_breaking());
}

#[test]
fn camel_case_matches_codama_on_acronyms() {
    let mut idl = bare_idl(vec![ix(
        "init_AMMConfig",
        vec![1],
        vec![acct("pool_state")],
    )]);
    idl = idl.with_camel_case_names();
    assert_eq!(idl.instructions[0].name, "initAMMConfig");
    let AccountItem::Single(a) = &idl.instructions[0].accounts[0] else {
        panic!("single account")
    };
    assert_eq!(a.name, "poolState");
}

#[test]
fn anchor_030_tuple_struct_with_object_typed_fields_parses() {
    // Anchor writes tuple struct fields as bare types; `{"array":[..]}` is a type, not a field.
    let idl = parse(
        r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.1.0"},
            "types":[{"name":"Seed","type":{"kind":"struct","fields":[{"array":["u8",24]},"bool"]}}]}"#,
    );
    let TypeDefTy::Struct { fields } = &idl.types[0].ty else {
        panic!("expected a struct");
    };
    assert_eq!(fields[0].name, "_0");
    assert_eq!(
        fields[0].ty,
        Type::Array(Box::new(Type::U8), ArrayLen::Value(24))
    );
    assert_eq!(fields[1].ty, Type::Bool);
}

#[test]
fn legacy_alias_kind_parses_as_a_type_alias() {
    let idl = parse(
        r#"{"version":"0.1.0","name":"legacy","metadata":null,
            "instructions":[{"name":"noop","accounts":[],"args":[{"name":"c","type":{"defined":"Config"}}]}],
            "types":[{"name":"Config","type":{"kind":"alias","value":{"defined":"Inner"}}},
                     {"name":"Inner","type":{"kind":"struct","fields":[{"name":"x","type":"u8"}]}}]}"#,
    );
    assert_eq!(
        idl.types[0].ty,
        TypeDefTy::Type {
            alias: Type::Defined {
                name: "Inner".into(),
                generics: vec![]
            }
        }
    );
}

#[test]
fn custom_serialization_parses_and_is_not_zero_copy() {
    let idl = parse(
        r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.1.0"},
            "types":[{"name":"Tick","serialization":{"custom":"market-tick-v1"},"repr":{"kind":"c"},
                      "type":{"kind":"struct","fields":[{"name":"slot","type":"u64"}]}}]}"#,
    );
    assert_eq!(idl.types[0].serialization, "custom:market-tick-v1");
    assert!(!idl.types[0].is_zero_copy());
}

const DANGLING_OLD: &str = r#"{
  "address":"A","metadata":{"name":"d","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"set","discriminator":[1],"accounts":[],"args":[{"name":"c","type":{"defined":{"name":"Config"}}}]}],
  "types":[{"name":"Config","type":{"kind":"struct","fields":[{"name":"x","type":"u8"}]}}]
}"#;
const DANGLING_NEW: &str = r#"{
  "address":"A","metadata":{"name":"d","version":"1","spec":"0.1.0"},
  "instructions":[{"name":"set","discriminator":[1],"accounts":[],"args":[{"name":"c","type":{"defined":{"name":"Config"}}}]}],
  "types":[]
}"#;

#[test]
fn removing_a_type_that_an_arg_still_references_is_breaking() {
    let report = diff(&parse(DANGLING_OLD), &parse(DANGLING_NEW));
    let msgs: Vec<&str> = report.changes.iter().map(|c| c.message.as_str()).collect();
    assert_eq!(
        msgs,
        ["type removed while still referenced (Config): dangling reference"]
    );
    assert!(report.has_breaking());
}

#[test]
fn a_reference_dangling_in_both_idls_is_not_a_change() {
    assert_eq!(max_sev(DANGLING_NEW, DANGLING_NEW), None);
}

#[test]
fn fails_at_applies_the_threshold() {
    // Enum append is Dangerous: passes the default gate, fails a stricter one.
    let report = diff(
        &parse(&std::fs::read_to_string("fixtures/manual/03_enum_append_old.json").unwrap()),
        &parse(&std::fs::read_to_string("fixtures/manual/03_enum_append_new.json").unwrap()),
    );
    assert!(!report.fails_at(Severity::Breaking));
    assert!(report.fails_at(Severity::Dangerous));
}

fn kinds(old: &str, new: &str) -> Vec<(Severity, ChangeKind)> {
    diff(&parse(old), &parse(new))
        .changes
        .iter()
        .map(|c| (c.severity, c.kind))
        .collect()
}

#[test]
fn field_rename_with_same_type_is_dangerous() {
    let renamed = AMM_OLD.replace(r#""name":"feeBps""#, r#""name":"tradeFeeBps""#);
    assert_eq!(
        kinds(AMM_OLD, &renamed),
        [(Severity::Dangerous, ChangeKind::FieldRenamed)]
    );
}

#[test]
fn swapping_two_same_typed_fields_is_breaking() {
    // Two renames whose names exist on the other side are a reorder: bytes change meaning.
    let old = r#"{"address":"A","metadata":{"name":"s","version":"1","spec":"0.1.0"},
      "accounts":[{"name":"P","discriminator":[1]}],
      "types":[{"name":"P","type":{"kind":"struct","fields":[{"name":"a","type":"u64"},{"name":"b","type":"u64"}]}}]}"#;
    let new = old.replace(
        r#"{"name":"a","type":"u64"},{"name":"b","type":"u64"}"#,
        r#"{"name":"b","type":"u64"},{"name":"a","type":"u64"}"#,
    );
    assert_eq!(
        kinds(old, &new),
        [
            (Severity::Breaking, ChangeKind::FieldMoved),
            (Severity::Breaking, ChangeKind::FieldMoved)
        ]
    );
}

#[test]
fn account_flag_flip_at_same_position_is_dangerous() {
    let new = AMM_OLD.replace(
        r#"{"name":"user","signer":true}"#,
        r#"{"name":"user","signer":true,"writable":true}"#,
    );
    assert_eq!(
        kinds(AMM_OLD, &new),
        [(Severity::Dangerous, ChangeKind::AccountFlagsChanged)]
    );
}

#[test]
fn account_rename_at_same_position_is_dangerous_but_a_swap_is_breaking() {
    let renamed = AMM_OLD.replace(
        r#"{"name":"user","signer":true}"#,
        r#"{"name":"payer","signer":true}"#,
    );
    assert_eq!(
        kinds(AMM_OLD, &renamed),
        [(Severity::Dangerous, ChangeKind::AccountRenamed)]
    );
    let swapped = AMM_OLD.replace(
        r#"[{"name":"pool","writable":true},{"name":"user","signer":true}]"#,
        r#"[{"name":"user","signer":true},{"name":"pool","writable":true}]"#,
    );
    assert!(kinds(AMM_OLD, &swapped)
        .iter()
        .all(|k| *k == (Severity::Breaking, ChangeKind::AccountMoved)));
}

#[test]
fn arg_rename_is_dangerous_and_arg_type_change_is_breaking() {
    let renamed = AMM_OLD.replace(r#""name":"amountIn""#, r#""name":"amount""#);
    assert_eq!(
        kinds(AMM_OLD, &renamed),
        [(Severity::Dangerous, ChangeKind::ArgRenamed)]
    );
    let widened = AMM_OLD.replace(
        r#"{"name":"amountIn","type":"u64"}"#,
        r#"{"name":"amountIn","type":"u128"}"#,
    );
    assert_eq!(
        kinds(AMM_OLD, &widened),
        [(Severity::Breaking, ChangeKind::ArgChanged)]
    );
}

#[test]
fn enum_variant_rename_is_dangerous() {
    let renamed = ENUM_OLD.replace(r#""name":"Slow""#, r#""name":"Lazy""#);
    assert_eq!(
        kinds(ENUM_OLD, &renamed),
        [(Severity::Dangerous, ChangeKind::VariantRenamed)]
    );
}

#[test]
fn change_serializes_with_kind_and_subject() {
    let report = diff(&parse(AMM_OLD), &parse(AMM_NEW));
    let field = report
        .changes
        .iter()
        .find(|c| c.kind == ChangeKind::FieldChanged)
        .expect("Pool.feeBps widened");
    let json = serde_json::to_value(field).unwrap();
    assert_eq!(json["severity"], "breaking");
    assert_eq!(json["kind"], "field_changed");
    assert_eq!(json["subject"], "Pool#1");
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
fn instruction_names_equal_up_to_ascii_case_are_breaking() {
    // Shipstern turns both into `parse_abc` and the generated parser fails to compile.
    let idl = |a: &str, b: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"c","version":"1","spec":"0.1.0"}},
              "instructions":[{{"name":"{a}","discriminator":[1],"accounts":[],"args":[]}},
                              {{"name":"{b}","discriminator":[2],"accounts":[],"args":[]}}]}}"#
        )
    };
    let colliding = idl("ABC", "Abc");
    assert_eq!(
        kinds(&colliding, &colliding),
        [(Severity::Breaking, ChangeKind::InstructionNameCollision)]
    );
    // Non-ASCII case stays apart in Shipstern's conversion, so this IDL compiles.
    let unicode = idl("ä", "Ä");
    assert!(kinds(&unicode, &unicode).is_empty());
}
