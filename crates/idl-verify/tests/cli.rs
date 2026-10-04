//! The binary's exit-code contract: 0 clean, 1 errors found, 2 unreadable input.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Command, Output};

fn live(path: &str) -> String {
    format!("{}/../../fixtures/live/{path}", env!("CARGO_MANIFEST_DIR"))
}

fn idl_verify(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_idl-verify"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[test]
fn clean_idl_exits_0() {
    let out = idl_verify(&[
        "--idl",
        &live("jupiter_vaults/idl.json"),
        "--samples",
        &live("jupiter_vaults/samples.json"),
        "--reference",
        &live("jupiter_vaults/reference.json"),
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.ends_with("summary: 0 errors, 0 warnings, 1 note\n"),
        "{stdout}"
    );
}

#[test]
fn findings_exit_1_and_json_carries_them() {
    let out = idl_verify(&[
        "--idl",
        &live("voltr_vault/idl.js-codama.json"),
        "--samples",
        &live("voltr_vault/samples.json"),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["exit_code"], 1);
    assert_eq!(report["counts"]["errors"], 1);
    let error = &report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["severity"] == "error")
        .unwrap();
    assert_eq!(error["check"], "bytes_used");
    assert_eq!(error["subject"], "account adaptorAddReceipt");
}

#[test]
fn missing_file_exits_2() {
    let out = idl_verify(&[
        "--idl",
        "does-not-exist.json",
        "--samples",
        &live("voltr_vault/samples.json"),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.starts_with("error: cannot read does-not-exist.json"),
        "{stderr}"
    );
}

#[test]
fn reference_for_other_samples_exits_2() {
    let out = idl_verify(&[
        "--idl",
        &live("voltr_vault/idl.json"),
        "--samples",
        &live("voltr_vault/samples.json"),
        "--reference",
        &live("jupiter_vaults/reference.json"),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.starts_with("error: reference file does not match the samples"),
        "{stderr}"
    );
}
