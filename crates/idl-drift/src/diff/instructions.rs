//! Instructions: which exist, and per instruction its discriminator, accounts and args.

use std::collections::BTreeMap;
use std::fmt;

use super::text::{disc_changed, disc_hex, field_change, field_str, size_text};
use super::{ChangeKind, FieldEdit, Report, Severity};
use crate::model::{AccountItem, Idl, Instruction};

/// Instructions are matched by name, so a duplicate name would hide drift on all but one.
pub(super) fn report_duplicate_names(instructions: &[Instruction], r: &mut Report) {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for ix in instructions {
        *seen.entry(ix.name.as_str()).or_insert(0) += 1;
    }
    for (name, count) in seen {
        if count > 1 {
            r.push(
                Severity::Breaking,
                ChangeKind::InstructionDuplicate,
                name,
                format!("duplicate instruction name '{name}' appears {count}x: the IDL is ambiguous and matching is unreliable"),
            );
        }
    }
}

pub(super) fn diff_instructions(old: &Idl, new: &Idl, r: &mut Report) {
    let old_ix: BTreeMap<_, _> = old.instructions.iter().map(|i| (&i.name, i)).collect();
    let new_ix: BTreeMap<_, _> = new.instructions.iter().map(|i| (&i.name, i)).collect();

    for (name, ni) in &new_ix {
        match old_ix.get(name) {
            None => {
                // An old parser routes a reused discriminator to the instruction it knows.
                let shared: Vec<&str> = old
                    .instructions
                    .iter()
                    .filter(|oi| {
                        !ni.discriminator.is_empty() && oi.discriminator == ni.discriminator
                    })
                    .map(|oi| oi.name.as_str())
                    .collect();
                if shared.is_empty() {
                    r.push(
                        Severity::Additive,
                        ChangeKind::InstructionAdded,
                        *name,
                        format!("new instruction: {name}"),
                    );
                } else {
                    r.push(
                        Severity::Breaking,
                        ChangeKind::DiscriminatorReused,
                        *name,
                        format!(
                            "new instruction {name} reuses discriminator {} of [{}]: parsers built from the old IDL route it there and misread its args",
                            disc_hex(&ni.discriminator),
                            shared.join(", ")
                        ),
                    );
                }
            }
            Some(oi) => diff_instruction(oi, ni, r),
        }
    }
    for name in old_ix.keys() {
        if !new_ix.contains_key(name) {
            r.push(
                Severity::Breaking,
                ChangeKind::InstructionRemoved,
                *name,
                format!("instruction removed: {name}"),
            );
        }
    }
}

fn diff_instruction(oi: &Instruction, ni: &Instruction, r: &mut Report) {
    let ix = oi.name.as_str();
    if oi.discriminator != ni.discriminator {
        r.push(
            Severity::Breaking,
            ChangeKind::DiscriminatorChanged,
            ix,
            disc_changed(
                "discriminator",
                ix,
                &oi.discriminator,
                &ni.discriminator,
                "scheme swap",
            ),
        );
    }
    if oi.discriminator_offset != ni.discriminator_offset {
        r.push(
            Severity::Breaking,
            ChangeKind::DiscriminatorOffsetChanged,
            ix,
            format!(
                "discriminator offset changed ({ix}): {} -> {}",
                oi.discriminator_offset, ni.discriminator_offset
            ),
        );
    }
    if oi.size_discriminator != ni.size_discriminator {
        r.push(
            Severity::Breaking,
            ChangeKind::DiscriminatorChanged,
            ix,
            format!(
                "size discriminator changed ({ix}): {} -> {}: parsers built from the old IDL select it by data length",
                size_text(oi.size_discriminator),
                size_text(ni.size_discriminator)
            ),
        );
    }

    // Parsers read accounts by position, so compare the flattened lists index by index.
    diff_accounts(
        ix,
        &flatten_accounts(&oi.accounts),
        &flatten_accounts(&ni.accounts),
        r,
    );

    // Any arg change moves every later byte.
    for pos in 0..oi.args.len().max(ni.args.len()) {
        let subject = format!("{ix}#{pos}");
        match (oi.args.get(pos), ni.args.get(pos)) {
            (Some(o), Some(n)) if o != n => {
                let edit = FieldEdit::of(&oi.args, &ni.args, o, n);
                let kind = match edit {
                    FieldEdit::Changed => ChangeKind::ArgChanged,
                    FieldEdit::Moved => ChangeKind::ArgMoved,
                    FieldEdit::Renamed => ChangeKind::ArgRenamed,
                };
                r.push(
                    edit.severity(),
                    kind,
                    subject,
                    format!("arg changed ({ix}#{pos}): {}", field_change(o, n)),
                );
            }
            (Some(o), None) => r.push(
                Severity::Breaking,
                ChangeKind::ArgRemoved,
                subject,
                format!("arg removed ({ix}#{pos}): {}", field_str(o)),
            ),
            (None, Some(n)) => r.push(
                Severity::Breaking,
                ChangeKind::ArgAdded,
                subject,
                format!(
                    "new arg ({ix}#{pos}): {} (Borsh layout grows)",
                    field_str(n)
                ),
            ),
            _ => {}
        }
    }
}

fn diff_accounts(ix: &str, ofa: &[Leaf], nfa: &[Leaf], r: &mut Report) {
    // Dropping only optional accounts off the end keeps every required position.
    let removed_all_optional = ofa.iter().skip(nfa.len()).all(|a| a.optional);
    for pos in 0..ofa.len().max(nfa.len()) {
        let subject = format!("{ix}#{pos}");
        match (ofa.get(pos), nfa.get(pos)) {
            (Some(o), Some(n)) if o.name != n.name => {
                // Either name at another position on the other side means the account
                // moved, so the parser reads the wrong one. Fresh names are only a rename.
                let moved = ofa.iter().any(|a| a.name == n.name)
                    || nfa.iter().any(|a| a.name == o.name);
                if moved {
                    r.push(
                        Severity::Breaking,
                        ChangeKind::AccountMoved,
                        subject,
                        format!("account changed at fixed position ({ix}#{pos}): {o} -> {n}"),
                    );
                } else {
                    r.push(
                        Severity::Dangerous,
                        ChangeKind::AccountRenamed,
                        subject,
                        format!("account renamed at fixed position ({ix}#{pos}): {o} -> {n} (same position, generated field name changes)"),
                    );
                }
            }
            (Some(o), Some(n)) if o != n => r.push(
                Severity::Dangerous,
                ChangeKind::AccountFlagsChanged,
                subject,
                format!("account flags changed ({ix}#{pos}): {o} -> {n} (position unchanged, parsers don't read flags)"),
            ),
            (Some(_), None) if removed_all_optional => r.push(
                Severity::Dangerous,
                ChangeKind::OptionalAccountRemoved,
                subject,
                format!("optional account removed from tail ({ix}#{pos}): required positions unchanged"),
            ),
            (Some(_), None) => r.push(
                Severity::Breaking,
                ChangeKind::AccountRemoved,
                subject,
                format!("account removed from instruction ({ix}#{pos}): positions shift"),
            ),
            (None, Some(_)) => r.push(
                Severity::Dangerous,
                ChangeKind::AccountAdded,
                subject,
                format!("new account on instruction ({ix}#{pos}): a parser built from the new IDL reads it by index, so it fails on a transaction without it"),
            ),
            _ => {}
        }
    }
}

/// One account in an instruction's flattened, positional account list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Leaf {
    name: String,
    writable: bool,
    signer: bool,
    optional: bool,
}

impl fmt::Display for Leaf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        let flags: Vec<&str> = [
            (self.writable, "writable"),
            (self.signer, "signer"),
            (self.optional, "optional"),
        ]
        .into_iter()
        .filter_map(|(on, flag)| on.then_some(flag))
        .collect();
        if !flags.is_empty() {
            write!(f, " ({})", flags.join(", "))?;
        }
        Ok(())
    }
}

fn flatten_accounts(items: &[AccountItem]) -> Vec<Leaf> {
    let mut out = Vec::new();
    for it in items {
        match it {
            AccountItem::Single(a) => out.push(Leaf {
                name: a.name.clone(),
                writable: a.writable,
                signer: a.signer,
                optional: a.optional,
            }),
            AccountItem::Composite { accounts, .. } => out.extend(flatten_accounts(accounts)),
        }
    }
    out
}

/// The number of accounts a parser reads by position: composites count their members.
pub(super) fn account_count(items: &[AccountItem]) -> usize {
    flatten_accounts(items).len()
}
