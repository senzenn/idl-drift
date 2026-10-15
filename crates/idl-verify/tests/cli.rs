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

#[test]
fn rpc_failure_exits_2_without_printing_the_url() {
    // Port 9 on localhost is closed, so no network is needed. The path stands in for a token.
    for url in ["http://127.0.0.1:9/secret-token", "secret-token"] {
        let out = idl_verify(&["--idl", &live("voltr_vault/idl.json"), "--rpc", url]);
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("getSignaturesForAddress"), "{stderr}");
        assert!(!stderr.contains("secret-token"), "{stderr}");
    }
}

#[test]
fn reads_the_rpc_url_from_the_environment() {
    let out = Command::new(env!("CARGO_BIN_EXE_idl-verify"))
        .args(["--idl", &live("voltr_vault/idl.json")])
        .env("IDL_VERIFY_RPC_URL", "http://127.0.0.1:9/secret-token")
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("getSignaturesForAddress"), "{stderr}");
}

#[test]
fn nothing_to_check_and_bad_input_exit_2_before_any_request() {
    let empty = std::env::temp_dir().join("idl-verify-empty-samples.json");
    std::fs::write(&empty, "{}").unwrap();
    let cases: [(&[&str], &str); 3] = [
        (&["--samples", empty.to_str().unwrap()], "holds no samples"),
        (&["--rpc", " "], "the RPC URL is empty"),
        (
            &["--rpc", "http://127.0.0.1:9/k", "--program", "not-a-key"],
            "is not a base58 program address",
        ),
    ];
    let idl = live("voltr_vault/idl.json");
    for (args, message) in cases {
        let mut all = vec!["--idl", idl.as_str()];
        all.extend_from_slice(args);
        let out = idl_verify(&all);
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    }
}
