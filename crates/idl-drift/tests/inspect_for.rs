//! `inspect --for`: the framework lines and their exit code, and no change without the flag.

use std::path::PathBuf;
use std::process::{Command, Output};

/// An Anchor IDL that both frameworks read wrong: a prefix discriminator and a `COption`.
const IDL: &str = r#"{"address":"6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
    "metadata":{"name":"t","version":"1","spec":"0.1.0"},
    "instructions":[{"name":"go","discriminator":[1],"accounts":[],"args":[]},
      {"name":"stop","discriminator":[1,2],"accounts":[],"args":[{"name":"v","type":{"coption":"u64"}}]}]}"#;

#[allow(clippy::unwrap_used)]
fn inspect(name: &str, args: &[&str]) -> (Output, String) {
    let path: PathBuf =
        std::env::temp_dir().join(format!("idl-drift-for-{name}-{}.json", std::process::id()));
    std::fs::write(&path, IDL).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_idl-drift"))
        .arg("inspect")
        .args(args)
        .arg(&path)
        .output()
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    (out, text)
}

#[test]
fn without_for_the_output_and_exit_code_stay_as_before() {
    let (out, text) = inspect("none", &[]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text,
        "instructions: 2  accounts: 0  events: 0  types: 0
unmapped (Generic) fields: 0
undefined types: 0
padded zero-copy types also read as Borsh: 0
zero-copy accounts with an unknown layout: 0
instructions that Shipstern cannot tell apart: 0
account types that Shipstern skips: 0
"
    );
}

#[test]
fn each_framework_prints_once_and_fails_the_run() {
    let (out, text) = inspect(
        "both",
        &["--for", "carbon", "--for", "shipstern", "--for", "carbon"],
    );
    assert_eq!(out.status.code(), Some(1));
    let tail = text
        .split_once("account types that Shipstern skips: 0\n")
        .unwrap()
        .1;
    assert_eq!(
        tail,
        "parts that carbon-cli misreads, drops or cannot generate: 2
CARBON: ix:stop.v: a COption (u32 tag, fixed size); carbon-cli reads a u8 tag, and no bytes when the value is absent
CARBON: instruction stop: carbon-cli tries `go` first with the same check, so it can decode this instruction as `go`
carbon-cli changes that keep the decoded values: 0
parts that include_shipstern_parser! rejects: 1
SHIPSTERN: instruction go: discriminator [1] is a prefix of `stop`'s (AmbiguousDiscriminator)
"
    );
}
