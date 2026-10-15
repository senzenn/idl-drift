//! The CI-facing contract: the `--fail-on` threshold, the JSON shape and exit codes.

mod common;

use std::process::Command;

use common::{fixture, parse, AMM_NEW, AMM_OLD};
use idl_drift::{diff, ChangeKind, Severity};

fn drift() -> Command {
    Command::new(env!("CARGO_BIN_EXE_idl-drift"))
}

#[test]
fn fails_at_applies_the_threshold() {
    // Enum append is Dangerous: passes the default gate, fails a stricter one.
    let report = diff(
        &fixture("manual/03_enum_append_old.json"),
        &fixture("manual/03_enum_append_new.json"),
    );
    assert!(!report.fails_at(Severity::Breaking));
    assert!(report.fails_at(Severity::Dangerous));
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
fn json_flag_prints_one_error_object_on_exit_2() {
    let out = drift()
        .args([
            "diff",
            "no/such/old.json",
            "tests/fixtures/amm_new.json",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["error", "exit_code"]);
    assert_eq!(json["exit_code"], 2);
    assert!(json["error"]
        .as_str()
        .unwrap()
        .starts_with("read no/such/old.json: "));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: read no/such/old.json: "));
}

#[test]
fn closed_stdout_keeps_the_exit_code() {
    // A reader that has already closed the pipe, such as `head -0`.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let status = drift()
        .args([
            "diff",
            "tests/fixtures/amm_old.json",
            "tests/fixtures/amm_new.json",
        ])
        .stdout(writer)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}
