//! Helpers shared by the integration tests.

// Each test file uses only some of these helpers.
#![allow(dead_code, clippy::expect_used)]

use idl_drift::model::Idl;
use idl_drift::{diff, ChangeKind, Severity};

pub fn parse(s: &str) -> Idl {
    Idl::from_json(s).expect("fixture should deserialize")
}

/// An Anchor IDL with no items, to fill with struct update syntax.
pub fn empty_idl() -> Idl {
    parse(r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.1.0"}}"#)
}

/// Parse a file under the repo-root `fixtures/`.
pub fn fixture(path: &str) -> Idl {
    parse(&fixture_text(path))
}

pub fn fixture_text(path: &str) -> String {
    let path = format!("{}/../../fixtures/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).expect(&path)
}

pub fn max_sev(old: &str, new: &str) -> Option<Severity> {
    diff(&parse(old), &parse(new))
        .changes
        .iter()
        .map(|c| c.severity)
        .max()
}

pub fn kinds(old: &str, new: &str) -> Vec<(Severity, ChangeKind)> {
    idl_kinds(&parse(old), &parse(new))
}

pub fn idl_kinds(old: &Idl, new: &Idl) -> Vec<(Severity, ChangeKind)> {
    diff(old, new)
        .changes
        .iter()
        .map(|c| (c.severity, c.kind))
        .collect()
}

pub fn messages(old: &Idl, new: &Idl) -> Vec<String> {
    diff(old, new)
        .changes
        .into_iter()
        .map(|c| c.message)
        .collect()
}

/// An AMM with one `swap` instruction and a `Pool` account.
pub const AMM_OLD: &str = r#"{
  "address":"A","metadata":{"name":"amm","version":"0.1.0","spec":"0.1.0"},
  "instructions":[{"name":"swap","discriminator":[1,2,3,4,5,6,7,8],
    "accounts":[{"name":"pool","writable":true},{"name":"user","signer":true}],
    "args":[{"name":"amountIn","type":"u64"}]}],
  "accounts":[{"name":"Pool","discriminator":[9,9,9,9,9,9,9,9]}],
  "types":[{"name":"Pool","type":{"kind":"struct","fields":[
    {"name":"authority","type":"pubkey"},{"name":"feeBps","type":"u16"}]}}]
}"#;

/// `AMM_OLD` with a new account on `swap`, a new instruction, and `Pool.feeBps` widened.
pub const AMM_NEW: &str = r#"{
  "address":"A","metadata":{"name":"amm","version":"0.2.0","spec":"0.1.0"},
  "instructions":[{"name":"swap","discriminator":[1,2,3,4,5,6,7,8],
    "accounts":[{"name":"pool","writable":true},{"name":"user","signer":true},{"name":"feeVault","writable":true}],
    "args":[{"name":"amountIn","type":"u64"}]},
    {"name":"collectFees","discriminator":[8,7,6,5,4,3,2,1],"accounts":[{"name":"pool","writable":true}],"args":[]}],
  "accounts":[{"name":"Pool","discriminator":[9,9,9,9,9,9,9,9]}],
  "types":[{"name":"Pool","type":{"kind":"struct","fields":[
    {"name":"authority","type":"pubkey"},{"name":"feeBps","type":"u32"}]}}]
}"#;
