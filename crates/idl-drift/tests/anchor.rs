//! Reading Anchor IDLs: the 0.30 spec and the legacy shape.

mod common;

use common::{fixture, idl_kinds, kinds, messages, parse};
use idl_drift::model::{AccountItem, ArrayLen, GenericParam, Idl, Repr, ReprKind, Type, TypeDefTy};
use idl_drift::{coverage, ChangeKind, Severity};

#[test]
fn default_omission_produces_no_changes() {
    // If writing default fields out changed anything, every diff would flood with false
    // positives.
    let omitted = r#"{"address":"A","metadata":{"name":"o","version":"1","spec":"0.1.0"},
      "instructions":[{"name":"swap","discriminator":[1],"accounts":[{"name":"pool","writable":true}]}]}"#;
    let explicit = r#"{"address":"A","metadata":{"name":"o","version":"1","spec":"0.1.0"},
      "instructions":[{"name":"swap","discriminator":[1],
        "accounts":[{"name":"pool","writable":true,"signer":false,"optional":false}],"args":[]}],
      "accounts":[],"events":[],"errors":[],"types":[]}"#;
    assert_eq!(kinds(omitted, explicit), []);
}

#[test]
fn anchor_spec_layout_fields_parse() {
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
fn custom_serialization_parses_and_is_not_zero_copy() {
    let idl = parse(
        r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.1.0"},
            "types":[{"name":"Tick","serialization":{"custom":"market-tick-v1"},"repr":{"kind":"c"},
                      "type":{"kind":"struct","fields":[{"name":"slot","type":"u64"}]}}]}"#,
    );
    assert_eq!(idl.types[0].serialization, "custom:market-tick-v1");
    assert!(!idl.types[0].is_zero_copy());
}

#[test]
fn u256_and_i256_parse_and_diff() {
    let idl = |ty: &str| {
        parse(&format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
  "instructions":[{{"name":"go","discriminator":[1],"args":[{{"name":"x","type":"{ty}"}}]}}]}}"#
        ))
    };
    assert_eq!(
        messages(&idl("u256"), &idl("i256")),
        ["arg changed (go#0): x: u256 -> i256 (Borsh layout)"]
    );
    assert!(coverage(&idl("u256")).fully_mapped());
}

#[test]
fn legacy_discriminators_match_the_real_ones() {
    // The same program as a legacy Anchor IDL (no discriminators) and as Codama (with them).
    let legacy = fixture("real/limit_order_v1_onchain.json");
    let codama = fixture("shipstern/limit_order_v1.json");
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
fn anchor_spec_other_than_0_1_0_fails_with_its_spec() {
    let err =
        Idl::from_json(r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.2.0"}}"#)
            .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Anchor IDL spec 0.2.0 is not supported; only spec 0.1.0 can be read"
    );
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
fn account_dropped_from_accounts_with_its_type_kept_is_dangerous() {
    let idl = |accounts: &str, types: &str| {
        parse(&format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"1","spec":"0.1.0"}},
            "accounts":{accounts},"types":{types}}}"#
        ))
    };
    let t = |name: &str| format!(r#"{{"name":"{name}","type":{{"kind":"struct","fields":[]}}}}"#);
    let old = idl(
        r#"[{"name":"TokenAccount","discriminator":[1]},{"name":"Vault","discriminator":[2]}]"#,
        &format!("[{},{}]", t("TokenAccount"), t("Vault")),
    );
    // Anchor 1.x drops the external `TokenAccount` and keeps its type; `Vault` is gone.
    let new = idl("[]", &format!("[{}]", t("TokenAccount")));
    assert_eq!(
        idl_kinds(&old, &new),
        [
            (Severity::Dangerous, ChangeKind::AccountTypeRemoved),
            (Severity::Breaking, ChangeKind::AccountTypeRemoved),
        ]
    );
}

#[test]
fn const_generic_values_read_like_js_parse_int() {
    use idl_drift::model::const_len;
    // Shipstern's `shapes` fixture writes `[u8; N]` with N = " +0X20zz" and expects 32.
    let cases = [
        ("32", Some(32)),
        (" +0X20zz", Some(32)),
        ("12abc", Some(12)),
        ("0x", None),
        ("abc", None),
        ("-5", None),
    ];
    for (value, want) in cases {
        assert_eq!(const_len(value), want, "{value:?}");
    }
}
