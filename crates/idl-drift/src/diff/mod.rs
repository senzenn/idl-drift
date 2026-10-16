//! Semantic diff of two IDLs, with every change classified by its effect on a parser
//! generated from the old one.
//!
//! The rules follow how Shipstern's generated parsers read bytes: accounts by position (a
//! missing one fails with `Account does not exist at index N`), dispatch by discriminator,
//! and enum variants by tag index.

mod accounts;
mod collisions;
mod errors;
mod events;
mod instructions;
mod normalize;
mod program;
mod report;
mod text;
mod types;
mod zero_copy;

use crate::model::{Field, Idl};

pub use crate::reach::reachable_types;
pub use report::{Change, ChangeKind, Report, Severity};

/// Diff two IDLs and classify every change.
///
/// When the IDLs come from different formats (Anchor, legacy Anchor, Codama), names are
/// compared in camelCase, so `deposit_vault` and `depositVault` are the same instruction.
#[must_use]
pub fn diff(old: &Idl, new: &Idl) -> Report {
    let mut r = if old.format == new.format {
        diff_names_as_given(old, new)
    } else {
        let (old, new) = normalize::common_form(old, new);
        diff_names_as_given(&old, &new)
    };
    // On the names as written: the camelCase step above can split names that collide.
    program::report_generated_name_collisions(&new.instructions, &mut r);
    program::diff_program(old, new, &mut r);
    r
}

fn diff_names_as_given(old: &Idl, new: &Idl) -> Report {
    let mut r = Report::default();
    // Layout changes only matter on types a parser actually reads.
    let reached = reachable_types(new);
    instructions::report_duplicate_names(&new.instructions, &mut r);
    instructions::diff_instructions(old, new, &mut r);
    collisions::diff_discriminator_collisions(old, new, &mut r);
    // Account layouts live in `types`; here only the discriminators.
    accounts::diff_account_types(&old.accounts, &new.accounts, &new.types, &mut r);
    events::diff_events(&old.events, &new.events, &mut r);
    errors::diff_errors(&old.errors, &new.errors, &mut r);
    types::diff_types(old, new, &reached, &mut r);
    r
}

/// How a field at a fixed position changed.
#[derive(Debug, Clone, Copy)]
enum FieldEdit {
    /// The type changed, so the bytes at this position mean something else.
    Changed,
    /// Same type, but one of the names holds another position on the other side: the
    /// parser now reads the wrong bytes under that name.
    Moved,
    /// Same type and a fresh name: the bytes are the same, only the generated name differs.
    Renamed,
}

impl FieldEdit {
    fn of(old_list: &[Field], new_list: &[Field], o: &Field, n: &Field) -> Self {
        if o.ty != n.ty {
            Self::Changed
        } else if old_list.iter().any(|f| f.name == n.name)
            || new_list.iter().any(|f| f.name == o.name)
        {
            Self::Moved
        } else {
            Self::Renamed
        }
    }

    fn severity(self) -> Severity {
        match self {
            Self::Changed | Self::Moved => Severity::Breaking,
            Self::Renamed => Severity::Dangerous,
        }
    }
}
