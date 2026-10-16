//! Account types: how a parser selects each one. Their layouts are in `types`.

use std::collections::BTreeMap;

use super::text::{disc_changed, extra_discs_text, size_text};
use super::{ChangeKind, Report, Severity};
use crate::model::{NamedDisc, TypeDef};

pub(super) fn diff_account_types(
    old: &[NamedDisc],
    new: &[NamedDisc],
    new_types: &[TypeDef],
    r: &mut Report,
) {
    let om: BTreeMap<_, _> = old.iter().map(|a| (&a.name, a)).collect();
    let nm: BTreeMap<_, _> = new.iter().map(|a| (&a.name, a)).collect();
    for (name, na) in &nm {
        match om.get(name) {
            None => r.push(
                Severity::Additive,
                ChangeKind::AccountTypeAdded,
                *name,
                format!("new account type: {name}"),
            ),
            Some(oa) if oa.discriminator != na.discriminator => r.push(
                Severity::Breaking,
                ChangeKind::AccountDiscriminatorChanged,
                *name,
                disc_changed(
                    "account discriminator",
                    name,
                    &oa.discriminator,
                    &na.discriminator,
                    "scheme swap, e.g. Anchor<->Quasar",
                ),
            ),
            Some(oa) if oa.discriminator_offset != na.discriminator_offset => r.push(
                Severity::Breaking,
                ChangeKind::AccountDiscriminatorOffsetChanged,
                *name,
                format!(
                    "account discriminator offset changed ({name}): {} -> {}",
                    oa.discriminator_offset, na.discriminator_offset
                ),
            ),
            Some(oa) if oa.extra_discriminators != na.extra_discriminators => r.push(
                Severity::Breaking,
                ChangeKind::AccountDiscriminatorChanged,
                *name,
                format!(
                    "account later discriminators changed ({name}): {} -> {}",
                    extra_discs_text(&oa.extra_discriminators),
                    extra_discs_text(&na.extra_discriminators)
                ),
            ),
            Some(oa) if oa.size_discriminator != na.size_discriminator => r.push(
                Severity::Breaking,
                ChangeKind::AccountDiscriminatorChanged,
                *name,
                format!(
                    "account size discriminator changed ({name}): {} -> {}: parsers built from the old IDL select it by data length",
                    size_text(oa.size_discriminator),
                    size_text(na.size_discriminator)
                ),
            ),
            _ => {}
        }
    }
    for name in om.keys() {
        if nm.contains_key(name) {
            continue;
        }
        // Anchor 1.x lists only the accounts that the program owns, but keeps their types.
        if new_types.iter().any(|t| &t.name == *name) {
            r.push(
                Severity::Dangerous,
                ChangeKind::AccountTypeRemoved,
                *name,
                format!(
                    "account type removed, its type kept: {name}: Anchor 1.x lists only the accounts that the program owns, so a parser built from the new IDL does not decode it"
                ),
            );
        } else {
            r.push(
                Severity::Breaking,
                ChangeKind::AccountTypeRemoved,
                *name,
                format!("account type removed: {name}"),
            );
        }
    }
}
