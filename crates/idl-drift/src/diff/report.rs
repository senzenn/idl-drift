use serde::{Deserialize, Serialize};

use crate::allow::{AllowEntry, AllowList};

/// How a change affects a parser generated from the old IDL, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Cosmetic,
    Additive,
    Dangerous,
    Breaking,
}

/// The version bump that a crate generated from the old IDL needs, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Bump {
    None,
    Patch,
    Major,
}

impl Bump {
    /// The part of the version that a 0.y.z crate bumps: Cargo reads y as its major part.
    #[must_use]
    pub fn zero_x_note(self) -> Option<&'static str> {
        match self {
            Self::Major => Some("a 0.y.z crate bumps y"),
            Self::Patch => Some("a 0.y.z crate bumps z"),
            Self::None => None,
        }
    }
}

/// The rule a change came from. Stable names, so CI can allow-list one change by `kind`
/// and `subject` instead of matching message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    ProgramAddressChanged,
    ProgramRenamed,
    InstructionAdded,
    InstructionRemoved,
    InstructionDuplicate,
    InstructionNameCollision,
    DiscriminatorReused,
    DiscriminatorChanged,
    DiscriminatorOffsetChanged,
    AccountMoved,
    AccountRenamed,
    AccountFlagsChanged,
    AccountRemoved,
    OptionalAccountRemoved,
    AccountAdded,
    ArgChanged,
    ArgMoved,
    ArgRenamed,
    ArgRemoved,
    ArgAdded,
    Collision,
    CollisionPreexisting,
    DispatchOrderChanged,
    AccountTypeAdded,
    AccountTypeRemoved,
    AccountDiscriminatorChanged,
    AccountDiscriminatorOffsetChanged,
    EventAdded,
    EventRemoved,
    EventDiscriminatorChanged,
    EventEnvelopeChanged,
    EventPayloadOffsetChanged,
    ErrorRemoved,
    ErrorRenumbered,
    ErrorCodeReused,
    EnumTagChanged,
    AliasChanged,
    TypeKindChanged,
    TypeRemoved,
    TypeUndefined,
    LayoutChanged,
    FieldChanged,
    FieldMoved,
    FieldRenamed,
    FieldAdded,
    FieldRemoved,
    ReservedBytesUsed,
    VariantTagChanged,
    VariantPayloadChanged,
    VariantMoved,
    VariantRenamed,
    VariantAppended,
    VariantInserted,
    VariantRemoved,
}

/// One classified change between the two IDLs.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub severity: Severity,
    pub kind: ChangeKind,
    /// What changed: an instruction, type or event name, with `#pos` for a positional item.
    pub subject: String,
    pub message: String,
    /// An allow file accepts this change, so it does not count toward the exit code.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub allowed: bool,
}

/// Every change, in the order the rules found them.
#[derive(Debug, Default)]
pub struct Report {
    pub changes: Vec<Change>,
}

impl Report {
    pub(super) fn push(
        &mut self,
        severity: Severity,
        kind: ChangeKind,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.changes.push(Change {
            severity,
            kind,
            subject: subject.into(),
            message: message.into(),
            allowed: false,
        });
    }

    /// Mark each change that `list` allows. Returns the entries that allow no change.
    pub fn allow<'a>(
        &mut self,
        list: &'a AllowList<ChangeKind>,
    ) -> Vec<&'a AllowEntry<ChangeKind>> {
        for c in &mut self.changes {
            c.allowed = list.allows(&c.kind, &c.subject);
        }
        list.entries()
            .iter()
            .filter(|e| {
                !self
                    .changes
                    .iter()
                    .any(|c| c.kind == e.rule && c.subject == e.subject)
            })
            .collect()
    }

    /// The changes that no allow entry accepts.
    fn counted(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| !c.allowed)
    }

    #[must_use]
    pub fn has_breaking(&self) -> bool {
        self.counted().any(|c| c.severity == Severity::Breaking)
    }

    /// 1 when a change that is not allowed is Breaking, else 0.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        i32::from(self.has_breaking())
    }

    /// `true` when any change that is not allowed is at `threshold` or above, for a
    /// stricter CI gate.
    #[must_use]
    pub fn fails_at(&self, threshold: Severity) -> bool {
        self.counted().any(|c| c.severity >= threshold)
    }

    /// The bump for the highest tier of all changes. An allow entry sets only the exit code:
    /// the generated crate changes all the same.
    #[must_use]
    pub fn suggested_bump(&self) -> Bump {
        let bump = |c: &Change| match c.severity {
            Severity::Cosmetic => Bump::Patch,
            // Each Additive rule adds an instruction, account type or event: a variant of an enum
            // that Shipstern and Carbon generate without `#[non_exhaustive]`.
            Severity::Additive | Severity::Dangerous | Severity::Breaking => Bump::Major,
        };
        self.changes.iter().map(bump).max().unwrap_or(Bump::None)
    }

    /// The number of changes per tier that are not allowed:
    /// `(breaking, dangerous, additive, cosmetic)`.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut b = 0;
        let mut d = 0;
        let mut a = 0;
        let mut c = 0;
        for ch in self.counted() {
            match ch.severity {
                Severity::Breaking => b += 1,
                Severity::Dangerous => d += 1,
                Severity::Additive => a += 1,
                Severity::Cosmetic => c += 1,
            }
        }
        (b, d, a, c)
    }
}

#[cfg(test)]
mod tests {
    use super::{Bump, ChangeKind, Report, Severity};

    /// The bump of a report with these `(severity, kind, allowed)` changes.
    fn bump(changes: &[(Severity, ChangeKind, bool)]) -> Bump {
        let mut report = Report::default();
        for &(severity, kind, allowed) in changes {
            report.push(severity, kind, "x", "m");
            if let Some(last) = report.changes.last_mut() {
                last.allowed = allowed;
            }
        }
        report.suggested_bump()
    }

    #[test]
    fn suggested_bump_takes_the_highest_tier_of_all_changes() {
        use ChangeKind as K;
        use Severity as S;
        assert_eq!(bump(&[]), Bump::None);
        assert_eq!(bump(&[(S::Cosmetic, K::FieldChanged, false)]), Bump::Patch);
        // Each new instruction, account type or event adds a variant to a generated enum.
        for kind in [K::InstructionAdded, K::AccountTypeAdded, K::EventAdded] {
            assert_eq!(bump(&[(S::Additive, kind, false)]), Bump::Major);
        }
        assert_eq!(
            bump(&[(S::Dangerous, K::VariantAppended, false)]),
            Bump::Major
        );
        assert_eq!(bump(&[(S::Breaking, K::FieldChanged, false)]), Bump::Major);
        // An allowed change still changes the crate.
        let allowed = [
            (S::Cosmetic, K::FieldChanged, false),
            (S::Breaking, K::ArgAdded, true),
        ];
        assert_eq!(bump(&allowed), Bump::Major);
    }

    #[test]
    fn each_bump_names_the_part_that_a_zero_x_crate_bumps() {
        assert_eq!(Bump::Major.zero_x_note(), Some("a 0.y.z crate bumps y"));
        assert_eq!(Bump::Patch.zero_x_note(), Some("a 0.y.z crate bumps z"));
        assert_eq!(Bump::None.zero_x_note(), None);
    }
}
