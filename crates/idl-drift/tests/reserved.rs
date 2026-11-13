//! New fields that take the bytes of a reserved array: one Dangerous change when no other
//! field moves, else the usual Breaking field changes.

mod common;

use std::process::Command;

use common::{kinds, messages, parse};
use idl_drift::{diff, ChangeKind, Severity};

/// A `Pool` account with these fields. `attrs` goes into the type, such as a serialization.
fn pool(attrs: &str, fields: &str) -> String {
    format!(
        r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},"instructions":[],
  "accounts":[{{"name":"Pool","discriminator":[1,2,3,4,5,6,7,8]}}],
  "types":[{{"name":"Pool"{attrs},"type":{{"kind":"struct","fields":[{fields}]}}}}]}}"#
    )
}

fn borsh(fields: &str) -> String {
    pool("", fields)
}

fn zero_copy(fields: &str) -> String {
    pool(r#","serialization":"bytemuck","repr":{"kind":"c"}"#, fields)
}

const USED: [(Severity, ChangeKind); 1] = [(Severity::Dangerous, ChangeKind::ReservedBytesUsed)];
const GROWS: [(Severity, ChangeKind); 2] = [
    (Severity::Breaking, ChangeKind::FieldChanged),
    (Severity::Breaking, ChangeKind::FieldAdded),
];

#[test]
fn a_field_from_the_end_of_a_reserved_array_is_one_dangerous_change() {
    let old = borsh(r#"{"name":"a","type":"u64"},{"name":"reserved","type":{"array":["u8",16]}}"#);
    let new = borsh(
        r#"{"name":"a","type":"u64"},{"name":"b","type":"u64"},{"name":"reserved","type":{"array":["u8",8]}}"#,
    );
    let report = diff(&parse(&old), &parse(&new));
    assert_eq!(report.exit_code(), 0);
    assert_eq!(
        messages(&parse(&old), &parse(&new)),
        ["reserved bytes used (Pool#1) via reachable type: b: u64 takes bytes 0..8 of reserved: [u8; 16]: safe only if these bytes are zero in existing data"]
    );
    assert_eq!(report.changes[0].subject, "Pool#1");
}

#[test]
fn two_fields_from_a_reserved_array_in_the_middle_keep_the_later_field() {
    let old = borsh(
        r#"{"name":"a","type":"u64"},{"name":"_reserved","type":{"array":["u8",16]}},{"name":"c","type":"u32"}"#,
    );
    let new = borsh(
        r#"{"name":"a","type":"u64"},{"name":"b","type":"u32"},{"name":"flag","type":"u8"},{"name":"_reserved","type":{"array":["u8",11]}},{"name":"c","type":"u32"}"#,
    );
    assert_eq!(
        messages(&parse(&old), &parse(&new)),
        ["reserved bytes used (Pool#1) via reachable type: b: u32 takes bytes 0..4, flag: u8 takes bytes 4..5 of _reserved: [u8; 16]: safe only if these bytes are zero in existing data"]
    );
}

#[test]
fn fields_that_take_all_reserved_bytes_or_sit_before_a_vec_are_dangerous() {
    let cases = [
        (
            r#"{"name":"a","type":"u8"},{"name":"padding_0","type":{"array":["u8",32]}}"#,
            r#"{"name":"a","type":"u8"},{"name":"admin","type":"pubkey"}"#,
        ),
        // pump's QuoteControl: `reserves_admin` takes 32 of the 64 reserved bytes before a Vec.
        (
            r#"{"name":"admin","type":"pubkey"},{"name":"_reserved","type":{"array":["u8",64]}},{"name":"mints","type":{"vec":"pubkey"}}"#,
            r#"{"name":"admin","type":"pubkey"},{"name":"reserves_admin","type":"pubkey"},{"name":"_reserved","type":{"array":["u8",32]}},{"name":"mints","type":{"vec":"pubkey"}}"#,
        ),
    ];
    for (old, new) in cases {
        assert_eq!(kinds(&borsh(old), &borsh(new)), USED, "{new}");
    }
}

#[test]
fn new_fields_that_do_not_fit_stay_breaking() {
    let old = borsh(r#"{"name":"a","type":"u64"},{"name":"reserved","type":{"array":["u8",8]}}"#);
    let new = borsh(
        r#"{"name":"a","type":"u64"},{"name":"b","type":"u64"},{"name":"reserved","type":{"array":["u8",8]}}"#,
    );
    assert_eq!(kinds(&old, &new), GROWS);
}

#[test]
fn a_new_field_of_variable_size_stays_breaking() {
    let old = borsh(r#"{"name":"a","type":"u64"},{"name":"reserved","type":{"array":["u8",9]}}"#);
    let new = borsh(r#"{"name":"a","type":"u64"},{"name":"b","type":{"option":"u64"}}"#);
    assert_eq!(
        kinds(&old, &new),
        [(Severity::Breaking, ChangeKind::FieldChanged)]
    );
}

#[test]
fn new_fields_that_move_a_later_field_stay_breaking() {
    let old = borsh(
        r#"{"name":"a","type":"u64"},{"name":"reserved","type":{"array":["u8",8]}},{"name":"c","type":"u64"}"#,
    );
    let new = borsh(
        r#"{"name":"a","type":"u64"},{"name":"b","type":"u64"},{"name":"reserved","type":{"array":["u8",4]}},{"name":"c","type":"u64"}"#,
    );
    assert!(diff(&parse(&old), &parse(&new)).has_breaking());
    assert!(!kinds(&old, &new).contains(&USED[0]));
}

#[test]
fn an_array_without_a_reserved_name_stays_breaking() {
    for name in ["data", "reserves", "reserved_fee", "padding_before_c"] {
        let old = borsh(&format!(
            r#"{{"name":"a","type":"u64"}},{{"name":"{name}","type":{{"array":["u8",16]}}}}"#
        ));
        let new = borsh(&format!(
            r#"{{"name":"a","type":"u64"}},{{"name":"b","type":"u64"}},{{"name":"{name}","type":{{"array":["u8",8]}}}}"#
        ));
        assert_eq!(kinds(&old, &new), GROWS, "{name}");
    }
}

#[test]
fn another_change_in_the_struct_keeps_the_field_rules() {
    // `a` is renamed too, so the struct is compared field by field as before.
    let old = borsh(r#"{"name":"a","type":"u64"},{"name":"reserved","type":{"array":["u8",16]}}"#);
    let new = borsh(
        r#"{"name":"a2","type":"u64"},{"name":"b","type":"u64"},{"name":"reserved","type":{"array":["u8",8]}}"#,
    );
    let got = kinds(&old, &new);
    assert_eq!(got[0], (Severity::Dangerous, ChangeKind::FieldRenamed));
    assert_eq!(&got[1..], GROWS);
}

#[test]
fn zero_copy_reserved_bytes_are_dangerous_when_no_field_moves() {
    let old = zero_copy(
        r#"{"name":"a","type":"u64"},{"name":"reserved","type":{"array":["u8",16]}},{"name":"c","type":"u64"}"#,
    );
    let new = zero_copy(
        r#"{"name":"a","type":"u64"},{"name":"b","type":"u64"},{"name":"reserved","type":{"array":["u8",8]}},{"name":"c","type":"u64"}"#,
    );
    assert_eq!(kinds(&old, &new), USED);
}

#[test]
fn zero_copy_alignment_that_moves_a_field_stays_breaking() {
    // Borsh fits 8 + 7 bytes in the 15, but repr(C) aligns `b` to 8, so `c` moves from 16 to 24.
    let old = zero_copy(
        r#"{"name":"a","type":"u8"},{"name":"reserved","type":{"array":["u8",15]}},{"name":"c","type":"u64"}"#,
    );
    let new = zero_copy(
        r#"{"name":"a","type":"u8"},{"name":"b","type":"u64"},{"name":"reserved","type":{"array":["u8",7]}},{"name":"c","type":"u64"}"#,
    );
    assert!(diff(&parse(&old), &parse(&new)).has_breaking());
}

#[test]
fn zero_copy_alignment_change_stays_breaking() {
    // Same offsets and size, but the struct now aligns to 8, which moves it inside a parent.
    let old =
        zero_copy(r#"{"name":"a","type":"u8"},{"name":"reserved","type":{"array":["u8",15]}}"#);
    let new = zero_copy(
        r#"{"name":"a","type":"u8"},{"name":"reserved","type":{"array":["u8",7]}},{"name":"b","type":"u64"}"#,
    );
    assert!(diff(&parse(&old), &parse(&new)).has_breaking());
}

#[test]
fn reserved_bytes_in_an_unreached_type_are_cosmetic() {
    let unreached = |fields: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},"instructions":[],
  "types":[{{"name":"Spare","type":{{"kind":"struct","fields":[{fields}]}}}}]}}"#
        )
    };
    let old = unreached(r#"{"name":"reserved","type":{"array":["u8",8]}}"#);
    let new = unreached(r#"{"name":"b","type":"u64"}"#);
    assert_eq!(
        kinds(&old, &new),
        [(Severity::Cosmetic, ChangeKind::ReservedBytesUsed)]
    );
}

#[test]
fn reserved_uses_give_the_bytes_of_each_new_field() {
    let old = borsh(
        r#"{"name":"a","type":"u64"},{"name":"_padding1","type":{"array":["u8",16]}},{"name":"c","type":"u32"}"#,
    );
    let new = borsh(
        r#"{"name":"a","type":"u64"},{"name":"b","type":"u32"},{"name":"_padding1","type":{"array":["u8",8]}},{"name":"d","type":"u32"},{"name":"c","type":"u32"}"#,
    );
    let uses = idl_drift::diff::reserved_uses(&parse(&old), &parse(&new));
    let [u] = uses.as_slice() else {
        panic!("{uses:?}")
    };
    assert_eq!(
        (u.ty.as_str(), u.index, u.reserved.name.as_str()),
        ("Pool", 1, "_padding1")
    );
    let taken: Vec<_> = u
        .taken
        .iter()
        .map(|t| (t.field.name.as_str(), t.bytes.clone()))
        .collect();
    assert_eq!(taken, [("b", 0..4), ("d", 12..16)]);
}

#[test]
fn the_cli_allows_the_change_by_kind_and_subject() {
    let dir = std::env::temp_dir();
    let write = |name: &str, text: &str| {
        let path = dir.join(format!("{}-{name}", std::process::id()));
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    };
    let old = write(
        "idl-drift-reserved-old.json",
        &borsh(r#"{"name":"a","type":"u64"},{"name":"reserved","type":{"array":["u8",16]}}"#),
    );
    let new = write(
        "idl-drift-reserved-new.json",
        &borsh(
            r#"{"name":"a","type":"u64"},{"name":"b","type":"u64"},{"name":"reserved","type":{"array":["u8",8]}}"#,
        ),
    );
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_idl-drift"))
            .args(["diff", &old, &new, "--json", "--fail-on", "dangerous"])
            .args(extra)
            .output()
            .unwrap()
    };
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let change = &json["changes"][0];
    assert_eq!(
        (&change["severity"], &change["kind"], &change["subject"]),
        (
            &serde_json::json!("dangerous"),
            &serde_json::json!("reserved_bytes_used"),
            &serde_json::json!("Pool#1")
        )
    );
    let allow = write(
        "idl-drift-reserved.allow",
        "reserved_bytes_used Pool#1  # zero on chain\n",
    );
    let out = run(&["--allow", &allow]);
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["changes"][0]["allowed"], true);
}
