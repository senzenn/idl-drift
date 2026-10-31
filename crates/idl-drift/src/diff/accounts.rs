//! Account types: how a parser selects each one. Their layouts are in `types`.

use std::collections::BTreeMap;

use super::text::{disc_changed, extra_discs_text, size_text};
use super::{ChangeKind, Report, Severity};
use crate::model::{Field, NamedDisc, TypeDef, TypeDefTy};

pub(super) fn diff_account_types(
    old: &[NamedDisc],
    new: &[NamedDisc],
    old_types: &[TypeDef],
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
            Some(oa) => diff_account(name, oa, na, old_types, new_types, r),
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

/// Each check runs on its own, so an allow entry for one change does not hide another.
fn diff_account(
    name: &str,
    oa: &NamedDisc,
    na: &NamedDisc,
    old_types: &[TypeDef],
    new_types: &[TypeDef],
    r: &mut Report,
) {
    let any_disc_changed = oa.discriminator != na.discriminator
        || oa.extra_discriminators != na.extra_discriminators
        || oa.size_discriminator != na.size_discriminator
        || oa.discriminator_offset != na.discriminator_offset;
    if oa.discriminator != na.discriminator {
        r.push(
            Severity::Breaking,
            ChangeKind::AccountDiscriminatorChanged,
            name,
            disc_changed(
                "account discriminator",
                name,
                &oa.discriminator,
                &na.discriminator,
                "scheme swap, e.g. Anchor<->Quasar",
            ),
        );
    }
    if oa.extra_discriminators != na.extra_discriminators {
        r.push(
            Severity::Breaking,
            ChangeKind::AccountDiscriminatorChanged,
            name,
            format!(
                "account later discriminators changed ({name}): {} -> {}",
                extra_discs_text(&oa.extra_discriminators),
                extra_discs_text(&na.extra_discriminators)
            ),
        );
    }
    if oa.size_discriminator != na.size_discriminator {
        r.push(
            Severity::Breaking,
            ChangeKind::AccountDiscriminatorChanged,
            name,
            format!(
                "account size discriminator changed ({name}): {} -> {}: parsers built from the old IDL select it by data length",
                size_text(oa.size_discriminator),
                size_text(na.size_discriminator)
            ),
        );
    }
    if oa.discriminator_offset != na.discriminator_offset {
        r.push(
            Severity::Breaking,
            ChangeKind::AccountDiscriminatorOffsetChanged,
            name,
            format!(
                "account discriminator offset changed ({name}): {} -> {}",
                oa.discriminator_offset, na.discriminator_offset
            ),
        );
    }
    // A number constant is also the first field. A bytes constant comes before the fields.
    // A discriminator change above already moves the body.
    if !any_disc_changed
        && oa.payload_start() != na.payload_start()
        && !first_field_dropped(
            fields(old_types, name),
            fields(new_types, name),
            oa.payload_start() < na.payload_start(),
        )
    {
        r.push(
            Severity::Breaking,
            ChangeKind::AccountDiscriminatorOffsetChanged,
            name,
            format!(
                "account body start changed ({name}): byte {} -> {}: each field moves",
                oa.payload_start(),
                na.payload_start()
            ),
        );
    }
}

/// The fields of the struct that holds an account's data.
fn fields<'a>(types: &'a [TypeDef], name: &str) -> Option<&'a [Field]> {
    match &types.iter().find(|t| t.name == name)?.ty {
        TypeDefTy::Struct { fields } => Some(fields),
        _ => None,
    }
}

/// `true` when the body that starts first only adds a first field. A number constant is that
/// field, so each later field stays at its byte.
fn first_field_dropped(old: Option<&[Field]>, new: Option<&[Field]>, old_first: bool) -> bool {
    let (Some(o), Some(n)) = (old, new) else {
        return false;
    };
    let (first, later) = if old_first { (o, n) } else { (n, o) };
    first.get(1..) == Some(later)
}
