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
        .env_remove("IDL_VERIFY_RPC_URL")
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
    let dir = std::env::temp_dir();
    let empty = dir.join("idl-verify-empty-samples.json");
    std::fs::write(&empty, "{}").unwrap();
    let no_data = dir.join("idl-verify-no-data-samples.json");
    std::fs::write(&no_data, r#"{"accounts":[{"address":"A","data":""}]}"#).unwrap();
    let saved = dir.join("idl-verify-not-saved.json");
    let samples = live("voltr_vault/samples.json");
    let cases: [(&[&str], &str); 7] = [
        (&["--samples", empty.to_str().unwrap()], "holds no samples"),
        (
            &["--samples", no_data.to_str().unwrap()],
            "only accounts with no data",
        ),
        (&["--rpc", " "], "the RPC URL is empty"),
        (
            &["--rpc", "http://127.0.0.1:9/k", "--program", "not-a-key"],
            "is not a base58 program address",
        ),
        // The env var can supply --rpc, but --samples still wins, so nothing would be saved.
        (
            &[
                "--samples",
                &samples,
                "--rpc",
                "http://127.0.0.1:9/k",
                "--save-samples",
                saved.to_str().unwrap(),
            ],
            "--save-samples saves only live samples",
        ),
        (
            &[
                "--samples",
                &samples,
                "--rpc",
                "http://127.0.0.1:9/k",
                "--max-tx",
                "5",
            ],
            "--max-tx limits only live sampling",
        ),
        (
            &[
                "--samples",
                &samples,
                "--rpc",
                "http://127.0.0.1:9/k",
                "--stall",
                "5",
            ],
            "--stall limits only live sampling",
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
    assert!(!saved.exists());
    // The default limits do not count as given, so --samples still runs.
    let out = idl_verify(&[
        "--idl",
        &idl,
        "--samples",
        &samples,
        "--rpc",
        "http://127.0.0.1:9/k",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn json_exit_2_prints_one_error_object() {
    let idl = live("voltr_vault/idl.json");
    let samples = live("voltr_vault/samples.json");
    let cases: [&[&str]; 4] = [
        &["--idl", "does-not-exist.json", "--samples", &idl],
        &["--idl", &idl, "--samples", &idl],
        &["--idl", &idl, "--rpc", "http://127.0.0.1:9/k"],
        &[
            "--idl",
            &idl,
            "--samples",
            &samples,
            "--rpc",
            "http://127.0.0.1:9/k",
            "--stall",
            "5",
        ],
    ];
    for args in cases {
        let mut all = args.to_vec();
        all.push("--json");
        let out = idl_verify(&all);
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        // The human message stays on stderr; stdout holds the same text as JSON.
        let message = stderr.trim().strip_prefix("error: ").unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"error": message, "exit_code": 2}),
            "{args:?}"
        );
    }
    // A usage error that clap reports exits 2 too.
    let out = idl_verify(&["--idl", &idl, "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["exit_code"], 2);
    assert!(
        json["error"].as_str().unwrap().contains("--samples"),
        "{json}"
    );
}

#[test]
fn fail_on_warning_exits_1_on_a_narrowed_field() {
    use base64::Engine;
    // The program writes `amount` as a u64; the IDL says u32. Borsh leaves 4 non-zero bytes.
    let dir = std::env::temp_dir().join("idl-verify-fail-on");
    std::fs::create_dir_all(&dir).unwrap();
    let idl = dir.join("idl.json");
    std::fs::write(
        &idl,
        r#"{"address":"11111111111111111111111111111111","metadata":{"name":"t","version":"0","spec":"0.1.0"},
            "instructions":[],"accounts":[{"name":"Pool","discriminator":[1,2,3,4,5,6,7,8]}],
            "types":[{"name":"Pool","type":{"kind":"struct","fields":[
                {"name":"name","type":"string"},{"name":"amount","type":"u32"}]}}]}"#,
    )
    .unwrap();
    let mut data = vec![1, 2, 3, 4, 5, 6, 7, 8, 2, 0, 0, 0, b'a', b'b'];
    data.extend((1u64 << 32 | 5).to_le_bytes());
    let data = base64::engine::general_purpose::STANDARD.encode(data);
    let samples = dir.join("samples.json");
    std::fs::write(
        &samples,
        format!(r#"{{"accounts":[{{"address":"A","data":"{data}"}}]}}"#),
    )
    .unwrap();

    let run = |extra: &[&str]| {
        let mut args = vec![
            "--idl",
            idl.to_str().unwrap(),
            "--samples",
            samples.to_str().unwrap(),
            "--json",
        ];
        args.extend_from_slice(extra);
        let out = idl_verify(&args);
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        (
            out.status.code(),
            report["exit_code"].clone(),
            report["counts"].clone(),
        )
    };
    let counts = serde_json::json!({"errors": 0, "warnings": 1, "notes": 0});
    assert_eq!(run(&[]), (Some(0), 0.into(), counts.clone()));
    assert_eq!(run(&["--fail-on", "warning"]), (Some(1), 1.into(), counts));
}

#[test]
fn upgrade_impact_adds_its_json_object() {
    // The new voltr IDL adds a u64 at the end of the zero-copy `Vault`.
    let mut idl: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(live("voltr_vault/idl.json")).unwrap())
            .unwrap();
    let vault = idl["types"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|t| t["name"] == "Vault")
        .unwrap();
    vault["type"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"name": "added", "type": "u64"}));
    let dir = std::env::temp_dir().join("idl-verify-upgrade-impact");
    std::fs::create_dir_all(&dir).unwrap();
    let new = dir.join("idl.json");
    std::fs::write(&new, idl.to_string()).unwrap();

    let out = idl_verify(&[
        "--idl",
        new.to_str().unwrap(),
        "--samples",
        &live("voltr_vault/samples.json"),
        "--upgrade-impact",
        "--json",
    ]);
    // A realloc is a warning, so the default --fail-on error passes.
    assert_eq!(out.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let impact = &report["upgrade_impact"];
    assert_eq!(impact["rent_source"], "formula");
    let vault = impact["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Vault")
        .unwrap();
    // 928 bytes grow to 936: 8 * 5080 lamports each.
    assert_eq!(
        vault["realloc"],
        serde_json::json!({"count": 5, "bytes": {"min": 8, "max": 8},
            "rent_each": {"min": 40640, "max": 40640}, "rent_total": 203_200})
    );
    assert_eq!(
        impact["totals"],
        serde_json::json!({"sampled": 16, "ok": 11, "shorter": 0, "realloc": 5,
            "broken": 0, "rent_total": 203_200})
    );
    assert_eq!(report["instructions"], serde_json::json!([]));
}
