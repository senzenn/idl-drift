//! Type rules: struct fields, enum variants and aliases, and which types a parser reaches.

mod common;

use common::{empty_idl, idl_kinds, kinds, max_sev, messages, parse, AMM_NEW, AMM_OLD};
use idl_drift::model::{Event, Field, Idl, NamedDisc, Type, TypeDef, TypeDefTy};
use idl_drift::{diff, ChangeKind, Severity};

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

/// An IDL whose only root is the account `name`.
fn account_idl(name: &str, types: Vec<TypeDef>) -> Idl {
    Idl {
        accounts: vec![NamedDisc {
            name: name.into(),
            discriminator: vec![1, 2, 3, 4, 5, 6, 7, 8],
            discriminator_offset: 0,
            extra_discriminators: vec![],
            size: None,
            size_discriminator: None,
            payload_offset: None,
        }],
        types,
        ..empty_idl()
    }
}

/// An instruction that takes the enum `Mode` with these variants.
fn mode_enum(variants: &str) -> String {
    format!(
        r#"{{"address":"A","metadata":{{"name":"e","version":"1","spec":"0.1.0"}},
  "instructions":[{{"name":"upd","discriminator":[1],"accounts":[],"args":[{{"name":"m","type":{{"defined":{{"name":"Mode"}}}}}}]}}],
  "types":[{{"name":"Mode","type":{{"kind":"enum","variants":[{variants}]}}}}]}}"#
    )
}

const FAST_SLOW: &str = r#"{"name":"Fast"},{"name":"Slow"}"#;

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
fn amm_pool_field_width_is_breaking() {
    // Pool.feeBps u16 to u32 on a reachable account type.
    assert_eq!(diff(&parse(AMM_OLD), &parse(AMM_NEW)).exit_code(), 1);
}

#[test]
fn field_rename_is_dangerous_and_names_both_fields() {
    let renamed = AMM_OLD.replace(r#""name":"feeBps""#, r#""name":"tradeFeeBps""#);
    assert_eq!(
        kinds(AMM_OLD, &renamed),
        [(Severity::Dangerous, ChangeKind::FieldRenamed)]
    );
    assert_eq!(
        messages(&parse(AMM_OLD), &parse(&renamed)),
        ["FIELD changed (Pool#1) via reachable type: feeBps renamed to tradeFeeBps (u16)"]
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
fn map_set_and_tuple_element_changes_are_breaking() {
    let vault =
        |ty: Type| account_idl("Vault", vec![typedef_struct("Vault", vec![field("x", ty)])]);
    for (old, new) in [
        (
            Type::Map(Box::new(Type::Pubkey), Box::new(Type::U64)),
            Type::Map(Box::new(Type::Pubkey), Box::new(Type::U128)),
        ),
        (
            Type::Set(Box::new(Type::U32)),
            Type::Set(Box::new(Type::U64)),
        ),
        (
            Type::Tuple(vec![Type::U8, Type::U64]),
            Type::Tuple(vec![Type::U64, Type::U8]),
        ),
    ] {
        assert_eq!(
            idl_kinds(&vault(old.clone()), &vault(new)),
            [(Severity::Breaking, ChangeKind::FieldChanged)],
            "{old:?}"
        );
        assert_eq!(idl_kinds(&vault(old.clone()), &vault(old)), []);
    }
}

#[test]
fn generic_arg_change_is_breaking() {
    // `Wrapper<u64>` to `Wrapper<u32>` changes the layout, though the type name is the same.
    let idl = |arg: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"g","version":"1","spec":"0.1.0"}},
  "instructions":[{{"name":"f","discriminator":[1],"accounts":[],
    "args":[{{"name":"w","type":{{"defined":{{"name":"Wrapper","generics":[{{"kind":"type","type":"{arg}"}}]}}}}}}]}}],
  "types":[{{"name":"Wrapper","type":{{"kind":"struct","fields":[{{"name":"inner","type":{{"generic":"T"}}}}]}}}}]}}"#
        )
    };
    assert_eq!(
        kinds(&idl("u64"), &idl("u32")),
        [(Severity::Breaking, ChangeKind::ArgChanged)]
    );
}

#[test]
fn a_type_that_only_the_new_idl_reaches_is_not_breaking() {
    // No old parser reads `Leg`, so its change cannot break one. The new event is Additive.
    let idl = |qty: &str, event: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"1","spec":"0.1.0"}},
  "events":[{event}],
  "types":[{{"name":"Leg","type":{{"kind":"struct","fields":[{{"name":"qty","type":"{qty}"}}]}}}},
    {{"name":"Swapped","type":{{"kind":"struct","fields":[{{"name":"leg","type":{{"defined":{{"name":"Leg"}}}}}}]}}}}]}}"#
        )
    };
    let event = r#"{"name":"Swapped","discriminator":[9,9,9,9,9,9,9,9]}"#;
    assert_eq!(
        kinds(&idl("u32", ""), &idl("u64", event)),
        [
            (Severity::Additive, ChangeKind::EventAdded),
            (Severity::Cosmetic, ChangeKind::FieldChanged)
        ]
    );
}

#[test]
fn generic_params_bind_by_position() {
    let idl = |params: &str, a: &str, b: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"g","version":"1","spec":"0.1.0"}},
  "instructions":[{{"name":"f","discriminator":[1],"accounts":[],
    "args":[{{"name":"w","type":{{"defined":{{"name":"Pair","generics":[{{"kind":"type","type":"u8"}},{{"kind":"type","type":"u64"}}]}}}}}}]}}],
  "types":[{{"name":"Pair","generics":[{params}],"type":{{"kind":"struct","fields":[
    {{"name":"a","type":{{"generic":"{a}"}}}},{{"name":"b","type":{{"generic":"{b}"}}}}]}}}}]}}"#
        )
    };
    let old = idl(
        r#"{"kind":"type","name":"T"},{"kind":"type","name":"U"}"#,
        "T",
        "U",
    );
    let renamed = idl(
        r#"{"kind":"type","name":"X"},{"kind":"type","name":"Y"}"#,
        "X",
        "Y",
    );
    assert_eq!(kinds(&old, &renamed), []);
    // `a` now takes the second arg, a u64.
    let reordered = idl(
        r#"{"kind":"type","name":"U"},{"kind":"type","name":"T"}"#,
        "T",
        "U",
    );
    assert_eq!(
        kinds(&old, &reordered),
        [
            (Severity::Breaking, ChangeKind::FieldChanged),
            (Severity::Breaking, ChangeKind::FieldChanged)
        ]
    );
}

#[test]
fn alias_target_change_is_breaking() {
    // `Amount = u64` to `u32` changes the width everywhere `Amount` is used.
    let idl = |target: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"a","version":"1","spec":"0.1.0"}},
  "instructions":[{{"name":"x","discriminator":[1],"accounts":[],
    "args":[{{"name":"amt","type":{{"defined":{{"name":"Amount"}}}}}}]}}],
  "types":[{{"name":"Amount","type":{{"kind":"type","alias":"{target}"}}}}]}}"#
        )
    };
    assert_eq!(
        kinds(&idl("u64"), &idl("u32")),
        [(Severity::Breaking, ChangeKind::AliasChanged)]
    );
}

#[test]
fn enum_append_is_dangerous() {
    let appended = mode_enum(r#"{"name":"Fast"},{"name":"Slow"},{"name":"Paused"}"#);
    assert_eq!(
        kinds(&mode_enum(FAST_SLOW), &appended),
        [(Severity::Dangerous, ChangeKind::VariantAppended)]
    );
}

#[test]
fn enum_middle_insert_is_breaking() {
    // Inserting before Slow moves its tag from 1 to 2.
    let inserted = mode_enum(r#"{"name":"Fast"},{"name":"Paused"},{"name":"Slow"}"#);
    assert_eq!(
        max_sev(&mode_enum(FAST_SLOW), &inserted),
        Some(Severity::Breaking)
    );
}

#[test]
fn enum_variant_rename_is_dangerous() {
    let renamed = mode_enum(r#"{"name":"Fast"},{"name":"Lazy"}"#);
    assert_eq!(
        kinds(&mode_enum(FAST_SLOW), &renamed),
        [(Severity::Dangerous, ChangeKind::VariantRenamed)]
    );
    // Old bytes with tag 1 select no variant now.
    let retagged = mode_enum(r#"{"name":"Fast"},{"name":"Lazy","discriminator":2}"#);
    assert_eq!(
        kinds(&mode_enum(FAST_SLOW), &retagged),
        [(Severity::Breaking, ChangeKind::VariantMoved)]
    );
}

#[test]
fn inline_variant_payload_change_is_breaking() {
    assert_eq!(
        messages(
            &parse(&pool_with_enum(r#"["u16"]"#, "")),
            &parse(&pool_with_enum(r#"["u64"]"#, "")),
        ),
        ["enum variant payload changed (Mode#0 Fixed): (u16) -> (u64) (Borsh layout)"]
    );
}

#[test]
fn type_reached_only_through_an_event_is_checked() {
    let idl = |trade_fields: Vec<Field>, px: Type| Idl {
        events: vec![Event {
            name: "Trade".into(),
            discriminator: vec![1, 2, 3, 4, 5, 6, 7, 8],
            discriminator_offset: 0,
            extra_discriminators: vec![],
            envelope_discriminator: vec![],
            envelope_payload_offset: None,
        }],
        types: vec![
            typedef_struct("Trade", trade_fields),
            typedef_struct("TradeDetail", vec![field("px", px)]),
        ],
        ..empty_idl()
    };
    let detail = || vec![field("detail", defined("TradeDetail"))];
    assert_eq!(
        messages(&idl(detail(), Type::U16), &idl(detail(), Type::U32)),
        ["FIELD changed (TradeDetail#0) via reachable type: px: u16 -> u32 (Borsh layout)"]
    );
    // When nothing references `TradeDetail`, the same change is Cosmetic.
    let qty = || vec![field("qty", Type::U64)];
    assert_eq!(
        idl_kinds(&idl(qty(), Type::U16), &idl(qty(), Type::U32)),
        [(Severity::Cosmetic, ChangeKind::FieldChanged)]
    );
}

#[test]
fn type_reached_only_through_a_map_value_is_checked() {
    let idl = |lamports: Type| {
        let by_slot = Type::Map(Box::new(Type::U64), Box::new(defined("Slot")));
        account_idl(
            "Vault",
            vec![
                typedef_struct("Vault", vec![field("bySlot", by_slot)]),
                typedef_struct("Slot", vec![field("lamports", lamports)]),
            ],
        )
    };
    assert_eq!(
        idl_kinds(&idl(Type::U16), &idl(Type::U32)),
        [(Severity::Breaking, ChangeKind::FieldChanged)]
    );
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
    assert_eq!(
        messages(&parse(DANGLING_OLD), &parse(DANGLING_NEW)),
        ["type removed while still referenced (Config): dangling reference"]
    );
    assert_eq!(
        max_sev(DANGLING_OLD, DANGLING_NEW),
        Some(Severity::Breaking)
    );
}

#[test]
fn a_reference_dangling_in_both_idls_is_breaking() {
    assert_eq!(
        messages(&parse(DANGLING_NEW), &parse(DANGLING_NEW)),
        ["type used but not defined (Config): no parser can be generated"]
    );
}
