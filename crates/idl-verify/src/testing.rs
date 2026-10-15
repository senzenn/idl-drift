//! Builders that unit tests in more than one module use.

use idl_drift::model::Idl;
use serde_json::{json, Value};

use crate::report::{Finding, Report};

/// An Anchor IDL of program `P`; `fields` is the rest of the object, such as `"accounts":[]`.
pub(crate) fn idl(fields: &str) -> Idl {
    Idl::from_json(&format!(
        r#"{{"address":"P","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},{fields}}}"#
    ))
    .unwrap()
}

/// The single finding in `report`, which must have no others.
pub(crate) fn only(report: &Report) -> &Finding {
    assert_eq!(report.findings.len(), 1, "{:#?}", report.findings);
    &report.findings[0]
}

/// A `getTransaction` result, `json` encoding, with signature `Sig1`.
pub(crate) fn tx(keys: &[&str], instructions: &Value) -> Value {
    json!({
        "transaction": {
            "signatures": ["Sig1"],
            "message": { "accountKeys": keys, "instructions": instructions }
        },
        "meta": {
            "err": null,
            "innerInstructions": [],
            "loadedAddresses": { "writable": [], "readonly": [] }
        }
    })
}
