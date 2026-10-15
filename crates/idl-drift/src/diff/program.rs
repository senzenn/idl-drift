//! The program address and the names Shipstern generates from the IDL.

use std::collections::{BTreeMap, BTreeSet};

use super::{ChangeKind, Report, Severity};
use crate::model::{camel, Format, Idl, Instruction};

/// A generated parser keeps only updates whose program is its `PROGRAM_ID`, and names its
/// module `to_snake_case(program name)`.
pub(super) fn diff_program(old: &Idl, new: &Idl, r: &mut Report) {
    // A legacy IDL can have no address, and then there is nothing to compare.
    if !old.address.is_empty() && !new.address.is_empty() && old.address != new.address {
        r.push(
            Severity::Breaking,
            ChangeKind::ProgramAddressChanged,
            "program",
            format!(
                "program address changed: {} -> {}: parsers built from the old IDL drop every update from the new address",
                old.address, new.address
            ),
        );
    }
    let old_mod = module_name(old);
    let new_mod = module_name(new);
    if old_mod != new_mod {
        r.push(
            Severity::Dangerous,
            ChangeKind::ProgramRenamed,
            "program",
            format!(
                "program renamed: {} -> {}: the generated module changes from `{old_mod}` to `{new_mod}`, so code that names it no longer compiles",
                old.metadata.name, new.metadata.name
            ),
        );
    }
}

/// Shipstern names each instruction helper `parse_{to_snake_case(name)}`, which lowercases
/// ASCII only, so `ABC` and `Abc` give one helper and the generated parser fails to compile.
pub(super) fn report_generated_name_collisions(instructions: &[Instruction], r: &mut Report) {
    let mut by_helper: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for ix in instructions {
        by_helper
            .entry(shipstern_snake_case(&ix.name))
            .or_default()
            .insert(ix.name.as_str());
    }
    for (helper, names) in by_helper {
        // One distinct name repeated is an exact duplicate, reported on its own.
        if names.len() > 1 {
            let list = names.into_iter().collect::<Vec<_>>().join(", ");
            r.push(
                Severity::Breaking,
                ChangeKind::InstructionNameCollision,
                &helper,
                format!("instructions [{list}] share the generated helper `parse_{helper}`: a Shipstern parser built from this IDL fails to compile"),
            );
        }
    }
}

/// Shipstern camel-cases an Anchor program name into Codama's `program.name` first.
fn module_name(idl: &Idl) -> String {
    match idl.format {
        Format::Codama => shipstern_snake_case(&idl.metadata.name),
        _ => shipstern_snake_case(&camel(&idl.metadata.name)),
    }
}

/// Shipstern's `to_snake_case` (`crates/proc-macro/src/utils.rs`).
fn shipstern_snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev_upper = chars[i - 1].is_uppercase();
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if !prev_upper || next_lower {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}
