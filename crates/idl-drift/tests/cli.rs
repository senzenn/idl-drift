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

#[test]
fn only_the_rpc_flag_needs_onchain() {
    let files = ["tests/fixtures/amm_old.json", "tests/fixtures/amm_new.json"];
    // A shell can export the URL for every run, so a file diff ignores the env var.
    let env = drift()
        .arg("diff")
        .args(files)
        .env("IDL_DRIFT_RPC_URL", "http://127.0.0.1:1/")
        .output()
        .unwrap();
    assert_eq!(env.status.code(), Some(1));
    let flag = drift()
        .args(["diff", "--rpc", "http://127.0.0.1:1/"])
        .args(files)
        .output()
        .unwrap();
    assert_eq!(flag.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&flag.stderr).contains("add --onchain"));
}

#[test]
fn onchain_errors_never_print_the_rpc_url() {
    // A refused connection, and a URL with no scheme, which ureq repeats in its error.
    for url in ["http://127.0.0.1:1/k3y", "k3y.example.com"] {
        let out = drift()
            .args([
                "diff",
                "--onchain",
                "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
                "tests/fixtures/amm_new.json",
                "--json",
            ])
            .env("IDL_DRIFT_RPC_URL", url)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let text = String::from_utf8_lossy(&[out.stdout, out.stderr].concat()).into_owned();
        assert!(text.contains("RPC request failed"), "{text}");
        assert!(!text.contains("k3y"), "{text}");
    }
}

const OLD_PATH: &str = "tests/fixtures/amm_old.json";
const NEW_PATH: &str = "tests/fixtures/amm_new.json";

/// An allow file in the temp dir, named for the test that uses it.
#[allow(clippy::unwrap_used)]
fn allow_file(name: &str, text: &str) -> String {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, text).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn allowed_change_does_not_fail_the_run_and_stays_visible() {
    // `Pool#1` holds a `#`; only a `#` after whitespace starts a comment.
    let allow = allow_file(
        "idl-drift-allow-used.txt",
        "# reviewed\nfield_changed Pool#1  # feeBps grew on purpose\narg_added swap#9\n",
    );
    let out = drift()
        .args(["diff", OLD_PATH, NEW_PATH, "--json", "--allow", &allow])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let field = json["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "field_changed")
        .unwrap();
    assert_eq!(field["allowed"], true);
    assert_eq!(json["counts"]["breaking"], 0);
    assert_eq!(
        json["unused_allows"],
        serde_json::json!([{"line": 3, "rule": "arg_added", "subject": "swap#9"}])
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("line 3: no change matches `arg_added swap#9`"),
        "{stderr}"
    );
}

#[test]
fn allow_file_typo_exits_2() {
    let cases = [
        (
            "field_chnged Pool#1\n",
            "line 1: unknown rule name `field_chnged`",
        ),
        (
            "\nfield_changed  # no subject\n",
            "line 2: give a rule name and a subject",
        ),
    ];
    for (i, (text, message)) in cases.into_iter().enumerate() {
        let allow = allow_file(&format!("idl-drift-allow-typo-{i}.txt"), text);
        let out = drift()
            .args(["diff", OLD_PATH, NEW_PATH, "--json", "--allow", &allow])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(json["exit_code"], 2);
        let error = json["error"].as_str().unwrap();
        assert!(error.ends_with(message), "{error}");
    }
}

#[test]
fn markdown_report_counts_changes_and_keeps_allowed_ones_apart() {
    let allow = allow_file("idl-drift-allow-markdown.txt", "field_changed Pool#1\n");
    let out = drift()
        .args(["diff", OLD_PATH, NEW_PATH, "--markdown", "--allow", &allow])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(md.starts_with("### idl-drift diff: "), "{md}");
    assert!(md.contains("| Breaking | 0 |\n| Dangerous | 1 |\n"), "{md}");
    assert!(md.contains("| Allowed | 1 |\n"), "{md}");
    let (main, details) = md.split_once("<details>").unwrap();
    assert!(
        main.contains(
            "| Additive | `instruction_added` | collectFees | new instruction\\: collectFees |\n"
        ),
        "{md}"
    );
    assert!(!main.contains("field_changed"), "{md}");
    assert!(
        details.contains("| Breaking | `field_changed` | Pool\\#1 |"),
        "{md}"
    );

    // A job summary shows why the run could not start.
    let out = drift()
        .args(["diff", "no/such/old.json", NEW_PATH, "--markdown"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(
        md.starts_with(
            "**idl-drift could not run (exit code 2):** read no\\/such\\/old\\.json\\: "
        ),
        "{md}"
    );
}
