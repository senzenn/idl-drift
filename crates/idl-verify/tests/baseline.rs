//! `--write-allow`: baseline mode.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Command, Output};

fn live(path: &str) -> String {
    format!("{}/../../fixtures/live/{path}", env!("CARGO_MANIFEST_DIR"))
}

fn idl_verify(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_idl-verify"))
        .args(args)
        .env("NO_COLOR", "1")
        .env_remove("IDL_VERIFY_RPC_URL")
        .output()
        .unwrap()
}

#[test]
fn write_allow_writes_a_baseline_that_the_next_run_passes_with() {
    let dir = std::env::temp_dir().join(format!("idl-verify-baseline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("verify.allow");
    let (idl, samples) = (
        live("voltr_vault/idl.js-codama.json"),
        live("voltr_vault/samples.json"),
    );
    let run = |flags: &[&str]| {
        let base = ["--idl", &idl, "--samples", &samples, "--json"];
        idl_verify(&[&base[..], flags].concat())
    };
    let path = file.to_str().unwrap();
    let out = run(&["--write-allow", path]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&file).unwrap();
    let entries: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    // The subject holds a space.
    assert_eq!(entries, ["bytes_used account adaptorAddReceipt"]);

    let out = run(&["--allow", path]);
    assert_eq!(out.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["unused_allows"], serde_json::json!([]));
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // An unused entry stays, with its comment, until --prune-allow removes it.
    let stale = format!("{text}bytes_used account vault  # v2\n");
    std::fs::write(&file, &stale).unwrap();
    let update = ["--allow", path, "--write-allow", path];
    let out = run(&update);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), stale);
    let out = run(&[&update[..], &["--prune-allow"]].concat());
    assert_eq!(out.status.code(), Some(0));
    let warning = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(warning.contains("--prune-allow removed it"), "{warning}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let line = stale.lines().count();
    assert_eq!(
        report["pruned_allows"],
        serde_json::json!([{"line": line, "rule": "bytes_used", "subject": "account vault"}])
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

    // An existing file that the run does not read: refuse, and keep it.
    let out = run(&["--write-allow", path]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    std::fs::remove_dir_all(&dir).unwrap();
}
