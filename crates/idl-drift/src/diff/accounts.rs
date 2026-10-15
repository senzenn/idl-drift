//! Account types: how a parser selects each one. Their layouts are in `types`.

use std::collections::BTreeMap;

use super::text::{disc_changed, size_text};
use super::{ChangeKind, Report, Severity};
use crate::model::NamedDisc;

pub(super) fn diff_account_types(old: &[NamedDisc], new: &[NamedDisc], r: &mut Report) {
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
        if !nm.contains_key(name) {
            r.push(
                Severity::Breaking,
                ChangeKind::AccountTypeRemoved,
                *name,
                format!("account type removed: {name}"),
            );
        }
    }
}
