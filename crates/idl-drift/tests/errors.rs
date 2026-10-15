//! Error codes: no parser reads them, but an indexer names a failed transaction's code with them.

mod common;

use common::kinds;
use idl_drift::{ChangeKind, Severity};

fn idl(errors: &str) -> String {
    format!(
        r#"{{"address":"A","metadata":{{"name":"t","version":"1","spec":"0.1.0"}},"errors":[{errors}]}}"#
    )
}

const OLD: &str = r#"{"code":6000,"name":"Stale"},{"code":6001,"name":"Slippage"}"#;

#[test]
fn removed_error_is_dangerous() {
    assert_eq!(
        kinds(&idl(OLD), &idl(r#"{"code":6000,"name":"Stale"}"#)),
        [(Severity::Dangerous, ChangeKind::ErrorRemoved)]
    );
}

#[test]
fn renumbered_error_is_dangerous() {
    assert_eq!(
        kinds(
            &idl(OLD),
            &idl(r#"{"code":6000,"name":"Stale"},{"code":6002,"name":"Slippage"}"#)
        ),
        [(Severity::Dangerous, ChangeKind::ErrorRenumbered)]
    );
}

#[test]
fn new_name_on_an_old_code_is_one_reused_code() {
    assert_eq!(
        kinds(
            &idl(OLD),
            &idl(r#"{"code":6000,"name":"Stale"},{"code":6001,"name":"Expired"}"#)
        ),
        [(Severity::Dangerous, ChangeKind::ErrorCodeReused)]
    );
}
