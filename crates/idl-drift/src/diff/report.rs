use serde::Serialize;

/// How a change affects a parser generated from the old IDL, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Cosmetic,
    Additive,
    Dangerous,
    Breaking,
}

/// The rule a change came from. Stable names, so CI can allow-list one change by `kind`
/// and `subject` instead of matching message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    LayoutChanged,
    FieldChanged,
    FieldMoved,
    FieldRenamed,
    FieldAdded,
    FieldRemoved,
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
        });
    }

    #[must_use]
    pub fn has_breaking(&self) -> bool {
        self.changes
            .iter()
            .any(|c| c.severity == Severity::Breaking)
    }

    /// 1 when any change is Breaking, else 0.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        i32::from(self.has_breaking())
    }

    /// `true` when any change is at `threshold` or above, for a stricter CI gate.
    #[must_use]
    pub fn fails_at(&self, threshold: Severity) -> bool {
        self.changes.iter().any(|c| c.severity >= threshold)
    }

    /// The number of changes per tier: `(breaking, dangerous, additive, cosmetic)`.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut b = 0;
        let mut d = 0;
        let mut a = 0;
        let mut c = 0;
        for ch in &self.changes {
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
