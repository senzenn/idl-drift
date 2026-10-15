//! Error codes. They do not change how a parser reads bytes, but an indexer names the
//! `Custom(code)` of a failed transaction from the IDL's errors.

use std::collections::BTreeMap;

use super::{ChangeKind, Report, Severity};
use crate::model::ErrorCode;

pub(super) fn diff_errors(old: &[ErrorCode], new: &[ErrorCode], r: &mut Report) {
    let old_codes: BTreeMap<&str, u32> = old.iter().map(|e| (e.name.as_str(), e.code)).collect();
    let new_codes: BTreeMap<&str, u32> = new.iter().map(|e| (e.name.as_str(), e.code)).collect();
    let old_names: BTreeMap<u32, &str> = old.iter().map(|e| (e.code, e.name.as_str())).collect();
    let new_names: BTreeMap<u32, &str> = new.iter().map(|e| (e.code, e.name.as_str())).collect();
    for (&name, &code) in &old_codes {
        match new_codes.get(name) {
            Some(&new_code) if new_code != code => r.push(
                Severity::Dangerous,
                ChangeKind::ErrorRenumbered,
                name,
                format!("error renumbered ({name}): {code} -> {new_code}: decoders built from the old IDL give code {new_code} the wrong name or none"),
            ),
            Some(_) => {}
            // A new name on the same code is reported once, as a reused code.
            None if new_names
                .get(&code)
                .is_some_and(|n| !old_codes.contains_key(n)) => {}
            None => r.push(
                Severity::Dangerous,
                ChangeKind::ErrorRemoved,
                name,
                format!("error removed: {name} ({code}): decoders built from the new IDL cannot name code {code} in older transactions"),
            ),
        }
    }
    for (&name, &code) in &new_codes {
        if old_codes.contains_key(name) {
            continue;
        }
        if let Some(old_name) = old_names.get(&code) {
            r.push(
                Severity::Dangerous,
                ChangeKind::ErrorCodeReused,
                name,
                format!("error code {code} reused: {old_name} -> {name}: decoders built from the old IDL name it {old_name}"),
            );
        }
    }
}
