//! Instructions: which exist, and per instruction its discriminator, accounts and args.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::collisions::DispatchKey;
use super::text::{disc_changed, extra_discs_text, field_change, field_str, size_text};
use super::types::Defs;
use super::{names, ChangeKind, FieldEdit, Report, Severity};
use crate::model::{camel, AccountItem, Idl, Instruction};

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
    // Names from two formats are camelCase here, so group members must match Codama's names.
    let camel_paths = old.format != new.format;
    let old_ix: BTreeMap<_, _> = old.instructions.iter().map(|i| (&i.name, i)).collect();
    let new_ix: BTreeMap<_, _> = new.instructions.iter().map(|i| (&i.name, i)).collect();
    let (om, nm) = (old.type_map(), new.type_map());
    let defs = Defs::new(&om, &nm);

    for (name, ni) in &new_ix {
        match old_ix.get(name) {
            None => {
                // An old parser routes a reused discriminator or size to the instruction it knows.
                let key = DispatchKey::of(ni);
                let shared: Vec<&str> = old
                    .instructions
                    .iter()
                    .filter(|oi| key.is_some() && DispatchKey::of(oi) == key)
                    .map(|oi| oi.name.as_str())
                    .collect();
                match key {
                    Some(key) if !shared.is_empty() => r.push(
                        Severity::Breaking,
                        ChangeKind::DiscriminatorReused,
                        *name,
                        format!(
                            "new instruction {name} reuses {key} of [{}]: parsers built from the old IDL route it there and misread its args",
                            shared.join(", ")
                        ),
                    ),
                    _ => r.push(
                        Severity::Additive,
                        ChangeKind::InstructionAdded,
                        *name,
                        format!("new instruction: {name}"),
                    ),
                }
            }
            Some(oi) => diff_instruction(oi, ni, camel_paths, &defs, r),
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

fn diff_instruction(
    oi: &Instruction,
    ni: &Instruction,
    camel_paths: bool,
    defs: &Defs,
    r: &mut Report,
) {
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
    if oi.extra_discriminators != ni.extra_discriminators {
        r.push(
            Severity::Breaking,
            ChangeKind::DiscriminatorChanged,
            ix,
            format!(
                "later discriminators changed ({ix}): {} -> {}",
                extra_discs_text(&oi.extra_discriminators),
                extra_discs_text(&ni.extra_discriminators)
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
        &flatten_accounts(&oi.accounts, camel_paths),
        &flatten_accounts(&ni.accounts, camel_paths),
        r,
    );

    // Any arg change moves every later byte.
    let (old_names, new_names) = (names(&oi.args), names(&ni.args));
    for pos in 0..oi.args.len().max(ni.args.len()) {
        let subject = format!("{ix}#{pos}");
        match (oi.args.get(pos), ni.args.get(pos)) {
            (Some(o), Some(n)) => {
                let Some(n) = defs.edited(o, n) else { continue };
                let edit = FieldEdit::of(&old_names, &new_names, o, &n);
                let kind = match edit {
                    FieldEdit::Changed => ChangeKind::ArgChanged,
                    FieldEdit::Moved => ChangeKind::ArgMoved,
                    FieldEdit::Renamed => ChangeKind::ArgRenamed,
                };
                r.push(
                    edit.severity(),
                    kind,
                    subject,
                    format!("arg changed ({ix}#{pos}): {}", field_change(o, &n)),
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
                // Either account at another position on the other side means the account
                // moved, so the parser reads the wrong one. Fresh names are only a rename.
                let elsewhere = |list: &[Leaf], leaf: &Leaf| {
                    list.iter()
                        .enumerate()
                        .any(|(i, a)| i != pos && a.same_account(leaf))
                };
                let moved = elsewhere(ofa, n) || elsewhere(nfa, o);
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
                        subject.clone(),
                        format!("account renamed at fixed position ({ix}#{pos}): {o} -> {n} (same position, generated field name changes)"),
                    );
                    // The same account, so a flag change still matters, and allowing the
                    // rename must not hide it.
                    if o.flags() != n.flags() {
                        r.push(
                            Severity::Dangerous,
                            ChangeKind::AccountFlagsChanged,
                            subject,
                            format!("account flags changed ({ix}#{pos}): {o} -> {n} (position unchanged, parsers don't read flags)"),
                        );
                    }
                }
            }
            (Some(o), Some(n)) if o.flags() != n.flags() => r.push(
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
#[derive(Debug, Clone)]
struct Leaf {
    /// The name Shipstern generates for it.
    name: String,
    /// Its group path, which stays the same when Shipstern adds or drops the group prefix.
    path: String,
    writable: bool,
    signer: bool,
    optional: bool,
}

impl Leaf {
    fn same_account(&self, other: &Self) -> bool {
        self.name == other.name || self.path == other.path
    }

    fn flags(&self) -> (bool, bool, bool) {
        (self.writable, self.signer, self.optional)
    }
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

/// Shipstern's Anchor conversion adds the group path to each group member's name only when
/// some account name repeats, as in Codama's `sourceOwner` for `owner` in group `source`.
/// With `camel_paths` unset, the path keeps the IDL's own names: `source.owner`.
fn flatten_accounts(items: &[AccountItem], camel_paths: bool) -> Vec<Leaf> {
    let mut out = Vec::new();
    flatten_into(items, None, camel_paths, repeats_a_name(items), &mut out);
    out
}

fn flatten_into(
    items: &[AccountItem],
    group: Option<&str>,
    camel_paths: bool,
    prefix: bool,
    out: &mut Vec<Leaf>,
) {
    let path_of = |name: &str| match group {
        None => name.to_owned(),
        Some(g) if camel_paths => camel(&format!("{g}_{name}")),
        Some(g) => format!("{g}.{name}"),
    };
    for it in items {
        match it {
            AccountItem::Single(a) => {
                let path = path_of(&a.name);
                out.push(Leaf {
                    name: if prefix { path.clone() } else { a.name.clone() },
                    path,
                    writable: a.writable,
                    signer: a.signer,
                    optional: a.optional,
                });
            }
            AccountItem::Composite { name, accounts } => {
                flatten_into(accounts, Some(&path_of(name)), camel_paths, prefix, out);
            }
        }
    }
}

fn repeats_a_name(items: &[AccountItem]) -> bool {
    fn walk(items: &[AccountItem], seen: &mut BTreeSet<String>) -> bool {
        items.iter().any(|it| match it {
            AccountItem::Single(a) => !seen.insert(camel(&a.name)),
            AccountItem::Composite { accounts, .. } => walk(accounts, seen),
        })
    }
    walk(items, &mut BTreeSet::new())
}

/// The number of accounts a parser reads by position: composites count their members.
pub(super) fn account_count(items: &[AccountItem]) -> usize {
    flatten_accounts(items, false).len()
}
