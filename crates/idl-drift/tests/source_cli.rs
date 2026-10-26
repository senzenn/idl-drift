//! `idl-drift source`: the program id in `declare_id!`, Anchor.toml, target/idl and on chain.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;

use serde_json::{json, Value};

const CONSISTENT: &str = "../../fixtures/source/consistent";
const DRIFT: &str = "../../fixtures/source/drift";

/// `idl-drift source`, without the RPC URL that the shell can export.
fn source() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_idl-drift"));
    cmd.arg("source").env_remove("IDL_DRIFT_RPC_URL");
    cmd
}

/// The exit code and the JSON report of `source --json` with `args`.
#[allow(clippy::unwrap_used)]
fn run_json(args: &[&str]) -> (Option<i32>, Value) {
    let out = source().args(args).arg("--json").output().unwrap();
    (
        out.status.code(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

/// `(kind, subject, severity)` of each finding, sorted.
#[allow(clippy::unwrap_used)]
fn findings(json: &Value) -> Vec<(&str, &str, &str)> {
    let mut found: Vec<_> = json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let field = |key: &str| f[key].as_str().unwrap();
            (field("kind"), field("subject"), field("severity"))
        })
        .collect();
    found.sort_unstable();
    found
}

/// The keys of a pretty-printed JSON object, in order. `Value` sorts its keys.
fn top_keys(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("  \""))
        .filter_map(|line| line.split_once('"'))
        .map(|(key, _)| key)
        .collect()
}

#[test]
fn drift_json_has_the_report_keys_counts_and_exit_code() {
    let out = source().args([DRIFT, "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        top_keys(&text),
        ["root", "ids", "findings", "counts", "exit_code"]
    );
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json["counts"],
        json!({"breaking": 2, "dangerous": 0, "additive": 0, "cosmetic": 1})
    );
    assert_eq!(json["exit_code"], 1);

    // A lower threshold cannot pass a Breaking finding.
    let (code, _) = run_json(&[DRIFT, "--fail-on", "cosmetic"]);
    assert_eq!(code, Some(1));
}

#[test]
fn markdown_report_lists_ids_and_findings() {
    let out = source().args([DRIFT, "--markdown"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(md.starts_with("### idl-drift source: "), "{md}");
    assert!(md.contains("| Breaking | 2 |\n| Dangerous | 0 |\n"), "{md}");
    assert!(md.contains("\n| Program | Place | Program id |\n"), "{md}");
    assert!(
        md.contains("| Breaking | `anchor_toml_mismatch` | amm\\@mainnet |"),
        "{md}"
    );
}

#[test]
fn missing_root_exits_2_with_the_error_object() {
    let out = source()
        .args(["no/such/workspace", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["error", "exit_code"]);
    assert_eq!(json["exit_code"], 2);
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("no/such/workspace"),
        "{json}"
    );
}

#[test]
fn workspace_with_an_id_in_one_place_exits_2() {
    // A valid id with no other copy and no finding is not a pass.
    let root = std::env::temp_dir().join(format!("idl-drift-one-id-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("Anchor.toml"),
        "[programs.mainnet]\namm = \"29dv8e1WcjL4w6a7HDaHbUfXrF12yiJiVcKQ1qgeT3rF\"\n",
    )
    .unwrap();
    let out = source().arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nothing to compare"),
        "{out:?}"
    );
}

#[test]
fn idl_flag_adds_an_idl_outside_target() {
    // A published copy that still has the `address` of the drift fixture's IDL.
    let path = std::env::temp_dir().join(format!("idl-drift-source-{}.json", std::process::id()));
    std::fs::write(
        &path,
        r#"{"address":"5bVzRzm2mwUd3iKzXTD9S82SJLjJKxKXzRJrzDyCZ3JJ",
        "metadata":{"name":"amm","version":"0.1.0","spec":"0.1.0"}}"#,
    )
    .unwrap();
    let path_text = path.to_string_lossy().into_owned();
    let (code, json) = run_json(&[CONSISTENT, "--idl", &path_text]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(code, Some(1));
    assert_eq!(
        findings(&json),
        [("idl_address_mismatch", "amm", "breaking")]
    );
    assert!(
        json["ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id["path"] == path_text.as_str()),
        "{json}"
    );
}

#[test]
fn only_the_rpc_flag_needs_onchain() {
    // A shell can export the URL for every run, so a run off chain ignores the env var.
    let env = source()
        .arg(CONSISTENT)
        .env("IDL_DRIFT_RPC_URL", "http://127.0.0.1:1/")
        .output()
        .unwrap();
    assert_eq!(env.status.code(), Some(0));
    let flag = source()
        .args([CONSISTENT, "--rpc", "http://127.0.0.1:1/"])
        .output()
        .unwrap();
    assert_eq!(flag.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&flag.stderr).contains("add --onchain"));
}

#[test]
fn onchain_without_a_working_rpc_exits_2() {
    let out = source().args([CONSISTENT, "--onchain"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs an RPC URL"));

    // A refused connection is not a finding, and the error does not print the URL.
    let out = source()
        .args([CONSISTENT, "--onchain", "--json"])
        .env("IDL_DRIFT_RPC_URL", "http://127.0.0.1:1/k3y")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&[out.stdout, out.stderr].concat()).into_owned();
    assert!(text.contains("RPC request failed"), "{text}");
    assert!(!text.contains("k3y"), "{text}");
}

/// A local JSON-RPC server that answers each request with the next of `results`, and its URL.
#[allow(clippy::unwrap_used)]
fn serve(results: Vec<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for result in results {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let body = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{result}}}"#);
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    url
}

#[test]
fn onchain_sends_the_requests_to_the_rpc_url() {
    // The chain rules have library tests. This test checks only the RPC wiring of the CLI.
    let no_account = r#"{"context":{"slot":1},"value":null}"#;
    let out = source()
        .args([CONSISTENT, "--onchain", "--json"])
        .env("IDL_DRIFT_RPC_URL", serve(vec![no_account]))
        .output()
        .unwrap();
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{json}");
    assert_eq!(
        findings(&json),
        [("onchain_missing", "amm@chain", "breaking")],
        "{json}"
    );
}
