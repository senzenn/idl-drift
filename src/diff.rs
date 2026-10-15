//! Semantic diff of two IDLs, with every change classified by its effect on a parser
//! generated from the old one.
//!
//! The rules follow how Shipstern's generated parsers read bytes: accounts by position (a
//! missing one fails with `Account does not exist at index N`), dispatch by discriminator,
//! and enum variants by tag index.

use crate::layout::{self, LayoutError, Reading, TypeIndex};
use crate::model::{
    AccountItem, ArrayLen, Count, EnumFields, Field, Format, GenericArg, Idl, OptionTag, Type,
    TypeDef, TypeDefTy,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub severity: Severity,
    pub kind: ChangeKind,
    /// What changed: an instruction, type or event name, with `#pos` for a positional item.
    pub subject: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub changes: Vec<Change>,
}

impl Report {
    fn push(
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

/// Diff two IDLs and classify every change.
///
/// When the IDLs come from different formats (Anchor, legacy Anchor, Codama), names are
/// compared in camelCase, so `deposit_vault` and `depositVault` are the same instruction.
#[must_use]
pub fn diff(old: &Idl, new: &Idl) -> Report {
    let mut r = if old.format == new.format {
        diff_names_as_given(old, new)
    } else {
        let mut old = with_tuple_structs(old).with_camel_case_names();
        let mut new = with_tuple_structs(new).with_camel_case_names();
        inline_struct_args(&mut old);
        inline_struct_args(&mut new);
        // Anchor IDLs can't describe an event envelope, so there is nothing to compare.
        if old.format != Format::Codama || new.format != Format::Codama {
            clear_envelopes(&mut old);
            clear_envelopes(&mut new);
        }
        diff_names_as_given(&old, &new)
    };
    // On the names as written: the camelCase step above can split names that collide.
    report_generated_name_collisions(&new.instructions, &mut r);
    diff_program(old, new, &mut r);
    r
}

/// A generated parser keeps only updates whose program is its `PROGRAM_ID`, and names its
/// module `to_snake_case(program name)`.
fn diff_program(old: &Idl, new: &Idl, r: &mut Report) {
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

/// A copy with each tuple alias written as a tuple struct, fields `_0`, `_1`, ..., as Anchor
/// writes it (see `normalize_type_defs`). Codama writes a tuple struct as a tuple alias, and
/// Borsh writes both the same way. A struct keeps the zero-copy layout computable.
fn with_tuple_structs(idl: &Idl) -> Idl {
    let mut idl = idl.clone();
    for td in &mut idl.types {
        if let TypeDefTy::Type {
            alias: Type::Tuple(items),
        } = &td.ty
        {
            if !items.is_empty() {
                let fields = items
                    .iter()
                    .enumerate()
                    .map(|(i, ty)| Field {
                        name: format!("_{i}"),
                        ty: ty.clone(),
                    })
                    .collect();
                td.ty = TypeDefTy::Struct { fields };
            }
        }
    }
    idl
}

/// Replace each arg that links a non-generic struct with the struct's fields, and each arg
/// that links a non-generic alias with the alias target, until no such arg is left. Borsh
/// writes a struct as its fields. Converters inline to different depths: Codama inlines only
/// a type with one use.
fn inline_struct_args(idl: &mut Idl) {
    let bodies: BTreeMap<String, TypeDefTy> = idl
        .types
        .iter()
        .filter(|td| td.generics.is_empty())
        .filter(|td| !matches!(td.ty, TypeDefTy::Enum { .. }))
        .map(|td| (td.name.clone(), td.ty.clone()))
        .collect();
    let body = |ty: &Type| match ty {
        Type::Defined { name, generics } if generics.is_empty() => bodies.get(name),
        _ => None,
    };
    for ix in &mut idl.instructions {
        // An IDL can make a type hold itself, or nest wide structs, so cap the depth and size.
        for _ in 0..MAX_INLINE_DEPTH {
            if !ix.args.iter().any(|a| body(&a.ty).is_some()) {
                break;
            }
            let next_len: usize = ix
                .args
                .iter()
                .map(|a| match body(&a.ty) {
                    Some(TypeDefTy::Struct { fields }) if !is_tuple(fields) => fields.len(),
                    _ => 1,
                })
                .sum();
            if next_len > MAX_INLINED_ARGS {
                break;
            }
            ix.args = std::mem::take(&mut ix.args)
                .into_iter()
                .flat_map(|arg| match body(&arg.ty) {
                    // Codama inlines a tuple struct as a tuple and keeps the arg name.
                    Some(TypeDefTy::Struct { fields }) if is_tuple(fields) => vec![Field {
                        name: arg.name.clone(),
                        ty: Type::Tuple(fields.iter().map(|f| f.ty.clone()).collect()),
                    }],
                    Some(TypeDefTy::Struct { fields }) => fields.clone(),
                    Some(TypeDefTy::Type { alias }) => vec![Field {
                        name: arg.name.clone(),
                        ty: alias.clone(),
                    }],
                    _ => vec![arg],
                })
                .collect();
        }
    }
}

const MAX_INLINE_DEPTH: usize = 16;
/// Far above any real instruction, far below what fills memory.
const MAX_INLINED_ARGS: usize = 1024;

/// Tuple fields after the camelCase step: `_0` becomes `0`.
fn is_tuple(fields: &[Field]) -> bool {
    !fields.is_empty()
        && fields
            .iter()
            .enumerate()
            .all(|(i, f)| f.name == i.to_string())
}

fn clear_envelopes(idl: &mut Idl) {
    for event in &mut idl.events {
        event.envelope_discriminator.clear();
        event.envelope_payload_offset = None;
    }
}

fn diff_names_as_given(old: &Idl, new: &Idl) -> Report {
    let mut r = Report::default();

    // Layout changes only matter on types a parser actually reads.
    let reached = reachable_types(new);
    report_duplicate_names(&new.instructions, &mut r);

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
            Some(oi) => diff_instruction(oi, ni, &mut r),
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

    diff_discriminator_collisions(old, new, &mut r);
    // Account layouts live in `types`; here only the discriminators.
    diff_account_types(&old.accounts, &new.accounts, &mut r);
    diff_events(&old.events, &new.events, &mut r);
    diff_types(old, new, &reached, &mut r);

    r
}

fn diff_account_types(
    old: &[crate::model::NamedDisc],
    new: &[crate::model::NamedDisc],
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
            Some(oa) if oa.discriminator != na.discriminator => {
                let msg = if oa.discriminator.len() == na.discriminator.len() {
                    format!("account discriminator changed ({name})")
                } else {
                    format!(
                        "account discriminator LENGTH changed ({name}): {}->{} bytes (scheme swap, e.g. Anchor<->Quasar)",
                        oa.discriminator.len(), na.discriminator.len()
                    )
                };
                r.push(
                    Severity::Breaking,
                    ChangeKind::AccountDiscriminatorChanged,
                    *name,
                    msg,
                );
            }
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

/// Events: the content discriminator, plus the self-CPI envelope. An envelope length
/// change (Anchor's 8-byte event tag to Pinocchio's 1-byte one) moves where the payload starts.
fn diff_events(old: &[crate::model::Event], new: &[crate::model::Event], r: &mut Report) {
    let om: BTreeMap<_, _> = old.iter().map(|e| (&e.name, e)).collect();
    let nm: BTreeMap<_, _> = new.iter().map(|e| (&e.name, e)).collect();
    for (name, ne) in &nm {
        match om.get(name) {
            None => r.push(
                Severity::Additive,
                ChangeKind::EventAdded,
                *name,
                format!("new event type: {name}"),
            ),
            Some(oe) => {
                if oe.discriminator != ne.discriminator {
                    let msg = if oe.discriminator.len() == ne.discriminator.len() {
                        format!("event discriminator changed ({name})")
                    } else {
                        format!(
                            "event discriminator LENGTH changed ({name}): {}->{} bytes (scheme swap, e.g. Anchor<->Quasar)",
                            oe.discriminator.len(),
                            ne.discriminator.len()
                        )
                    };
                    r.push(
                        Severity::Breaking,
                        ChangeKind::EventDiscriminatorChanged,
                        *name,
                        msg,
                    );
                } else if oe.discriminator_offset != ne.discriminator_offset
                    || oe.extra_discriminators != ne.extra_discriminators
                {
                    r.push(
                        Severity::Breaking,
                        ChangeKind::EventDiscriminatorChanged,
                        *name,
                        format!("event discriminator offset or chain changed ({name})"),
                    );
                }
                diff_event_envelope(name, oe, ne, r);
            }
        }
    }
    for name in om.keys() {
        if !nm.contains_key(name) {
            r.push(
                Severity::Breaking,
                ChangeKind::EventRemoved,
                *name,
                format!("event type removed: {name}"),
            );
        }
    }
}

fn diff_event_envelope(
    name: &str,
    oe: &crate::model::Event,
    ne: &crate::model::Event,
    r: &mut Report,
) {
    let old_env = &oe.envelope_discriminator;
    let new_env = &ne.envelope_discriminator;
    // Only compare when at least one side models an envelope.
    if old_env.is_empty() && new_env.is_empty() {
        // Still compare explicit payload offsets if both set.
        match (oe.envelope_payload_offset, ne.envelope_payload_offset) {
            (Some(o), Some(n)) if o != n => r.push(
                Severity::Breaking,
                ChangeKind::EventPayloadOffsetChanged,
                name,
                format!(
                    "event envelope payload offset changed ({name}): {o}->{n} (self-CPI parse starts elsewhere)"
                ),
            ),
            _ => {}
        }
        return;
    }
    if old_env != new_env {
        let msg = if old_env.len() == new_env.len() {
            format!("event envelope discriminator changed ({name})")
        } else {
            format!(
                "event envelope discriminator LENGTH changed ({name}): {}->{} bytes (scheme swap, e.g. Anchor 8-byte <-> Pinocchio 1-byte)",
                old_env.len(),
                new_env.len()
            )
        };
        r.push(
            Severity::Breaking,
            ChangeKind::EventEnvelopeChanged,
            name,
            msg,
        );
    }
    match (oe.effective_payload_offset(), ne.effective_payload_offset()) {
        (Some(o), Some(n)) if o != n => r.push(
            Severity::Breaking,
            ChangeKind::EventPayloadOffsetChanged,
            name,
            format!(
                "event envelope payload offset changed ({name}): {o}->{n} (self-CPI parse starts elsewhere)"
            ),
        ),
        _ => {}
    }
}

fn size_text(size: Option<usize>) -> String {
    size.map_or_else(|| "none".into(), |n| format!("{n} bytes"))
}

fn diff_instruction(
    oi: &crate::model::Instruction,
    ni: &crate::model::Instruction,
    r: &mut Report,
) {
    let ix = oi.name.as_str();
    if oi.discriminator != ni.discriminator {
        let msg = if oi.discriminator.len() == ni.discriminator.len() {
            format!("discriminator changed ({ix})")
        } else {
            format!(
                "discriminator LENGTH changed ({ix}): {}->{} bytes (scheme swap)",
                oi.discriminator.len(),
                ni.discriminator.len()
            )
        };
        r.push(
            Severity::Breaking,
            ChangeKind::DiscriminatorChanged,
            ix,
            msg,
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
    let max_a = oi.args.len().max(ni.args.len());
    for pos in 0..max_a {
        let subject = format!("{ix}#{pos}");
        match (oi.args.get(pos), ni.args.get(pos)) {
            (Some(o), Some(n)) if o != n => {
                let edit = field_edit(&oi.args, &ni.args, o, n);
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
    fn severity(self) -> Severity {
        match self {
            Self::Changed | Self::Moved => Severity::Breaking,
            Self::Renamed => Severity::Dangerous,
        }
    }
}

fn field_edit(old_list: &[Field], new_list: &[Field], o: &Field, n: &Field) -> FieldEdit {
    if o.ty != n.ty {
        FieldEdit::Changed
    } else if old_list.iter().any(|f| f.name == n.name) || new_list.iter().any(|f| f.name == o.name)
    {
        FieldEdit::Moved
    } else {
        FieldEdit::Renamed
    }
}

fn diff_accounts(ix: &str, ofa: &[Leaf], nfa: &[Leaf], r: &mut Report) {
    // Dropping only optional accounts off the end keeps every required position.
    let removed_all_optional = ofa.iter().skip(nfa.len()).all(|a| a.optional);
    let max = ofa.len().max(nfa.len());
    for pos in 0..max {
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
            (Some(_), None) => {
                if removed_all_optional {
                    r.push(
                        Severity::Dangerous,
                        ChangeKind::OptionalAccountRemoved,
                        subject,
                        format!("optional account removed from tail ({ix}#{pos}): required positions unchanged"),
                    );
                } else {
                    r.push(
                        Severity::Breaking,
                        ChangeKind::AccountRemoved,
                        subject,
                        format!("account removed from instruction ({ix}#{pos}): positions shift"),
                    );
                }
            }
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

impl std::fmt::Display for Leaf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/// Shipstern's `InstructionParser` tells instructions that share a discriminator apart by
/// account count, most first. The same discriminator and count needs a hand-written resolver.
fn diff_discriminator_collisions(old: &Idl, new: &Idl, r: &mut Report) {
    let old_classes = collision_classes(&old.instructions);
    let new_classes = collision_classes(&new.instructions);

    // Same discriminator and account count: unresolvable.
    for ((disc, count), names) in &new_classes {
        if names.len() < 2 {
            continue;
        }
        let hex = disc_hex(disc);
        let name_list = names.iter().cloned().collect::<Vec<_>>().join(", ");
        let preexisting = old_classes
            .get(&(disc.clone(), *count))
            .is_some_and(|n| n.len() >= 2);
        if preexisting {
            r.push(
                Severity::Dangerous,
                ChangeKind::CollisionPreexisting,
                &hex,
                format!(
                    "pre-existing collision on discriminator {hex} with {count} accounts: [{name_list}]"
                ),
            );
        } else {
            r.push(
                Severity::Breaking,
                ChangeKind::Collision,
                &hex,
                format!(
                    "unresolvable collision on discriminator {hex} with {count} accounts: [{name_list}]"
                ),
            );
        }
    }

    // Resolvable, but the set of account counts changed, so dispatch order may differ.
    let old_counts = account_counts_by_disc(&old.instructions);
    let new_counts = account_counts_by_disc(&new.instructions);
    for (disc, new_set) in &new_counts {
        if new_set.len() < 2 {
            continue;
        }
        let Some(old_set) = old_counts.get(disc) else {
            continue;
        };
        if old_set.len() < 2 {
            continue;
        }
        if old_set != new_set {
            let hex = disc_hex(disc);
            let old_list = counts_list(old_set);
            let new_list = counts_list(new_set);
            r.push(
                Severity::Dangerous,
                ChangeKind::DispatchOrderChanged,
                &hex,
                format!(
                    "disambiguation order changed for discriminator {hex}: account counts {{{old_list}}} -> {{{new_list}}}"
                ),
            );
        }
    }
}

/// `(discriminator, flattened account count)` to instruction names.
fn collision_classes(
    instructions: &[crate::model::Instruction],
) -> BTreeMap<(Vec<u8>, usize), BTreeSet<String>> {
    let mut map: BTreeMap<(Vec<u8>, usize), BTreeSet<String>> = BTreeMap::new();
    for ix in instructions {
        if ix.discriminator.is_empty() {
            continue;
        }
        let count = flatten_accounts(&ix.accounts).len();
        map.entry((ix.discriminator.clone(), count))
            .or_default()
            .insert(ix.name.clone());
    }
    map
}

/// Discriminator to its flattened account counts, skipping empty discriminators.
fn account_counts_by_disc(
    instructions: &[crate::model::Instruction],
) -> BTreeMap<Vec<u8>, BTreeSet<usize>> {
    let mut map: BTreeMap<Vec<u8>, BTreeSet<usize>> = BTreeMap::new();
    for ix in instructions {
        if ix.discriminator.is_empty() {
            continue;
        }
        let count = flatten_accounts(&ix.accounts).len();
        map.entry(ix.discriminator.clone())
            .or_default()
            .insert(count);
    }
    map
}

fn disc_hex(disc: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(disc.len() * 2);
    for b in disc {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn counts_list(counts: &BTreeSet<usize>) -> String {
    counts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn diff_types(old: &Idl, new: &Idl, reached: &BTreeSet<String>, r: &mut Report) {
    let om = old.type_map();
    let nm = new.type_map();
    let layout_attrs_changed = |(name, nt): (&String, &&TypeDef)| {
        om.get(name)
            .is_some_and(|ot| ot.serialization != nt.serialization || ot.repr != nt.repr)
    };
    let roots = if nm.iter().any(layout_attrs_changed) {
        zero_copy_roots(old, &nm)
    } else {
        Vec::new()
    };
    for (name, nt) in &nm {
        let Some(ot) = om.get(name) else { continue }; // added types reported via refs
        let is_reached = reached.contains(name);

        if ot.serialization != nt.serialization || ot.repr != nt.repr {
            diff_layout_attrs(name, ot, nt, reached, &roots, r);
        }

        match (&ot.ty, &nt.ty) {
            (TypeDefTy::Struct { fields: of }, TypeDefTy::Struct { fields: nf }) => {
                diff_fields(name, of, nf, is_reached, r);
            }
            (
                TypeDefTy::Enum {
                    variants: ov,
                    tag: otag,
                },
                TypeDefTy::Enum {
                    variants: nv,
                    tag: ntag,
                },
            ) => {
                if otag != ntag {
                    let show =
                        |t: &Option<Type>| t.as_ref().map_or_else(|| "u8".to_string(), tystr);
                    if is_reached {
                        r.push(
                            Severity::Breaking,
                            ChangeKind::EnumTagChanged,
                            name.as_str(),
                            format!(
                                "enum tag width changed ({name}): {} -> {} (every variant tag is read differently)",
                                show(otag),
                                show(ntag)
                            ),
                        );
                    } else {
                        r.push(
                            Severity::Cosmetic,
                            ChangeKind::EnumTagChanged,
                            name.as_str(),
                            format!("enum tag changed in UNREACHED type ({name})"),
                        );
                    }
                }
                diff_variants(name, ov, nv, is_reached, r);
            }
            // `Amount = u64` to `u32` changes the width everywhere `Amount` is used.
            (TypeDefTy::Type { alias: oa }, TypeDefTy::Type { alias: na }) if oa != na => {
                if is_reached {
                    r.push(
                        Severity::Breaking,
                        ChangeKind::AliasChanged,
                        name.as_str(),
                        format!(
                            "type alias target changed ({name}): {} -> {} (Borsh layout)",
                            tystr(oa),
                            tystr(na)
                        ),
                    );
                } else {
                    r.push(
                        Severity::Cosmetic,
                        ChangeKind::AliasChanged,
                        name.as_str(),
                        format!("alias changed in UNREACHED type ({name})"),
                    );
                }
            }
            (a, b) if std::mem::discriminant(a) != std::mem::discriminant(b) => {
                r.push(
                    Severity::Breaking,
                    ChangeKind::TypeKindChanged,
                    name.as_str(),
                    format!("type kind changed ({name})"),
                );
            }
            _ => {}
        }
    }
    // A type dropped from `types` while a parser still reaches it leaves a dangling
    // reference: no parser can be generated. A reference dangling in both IDLs is not a change.
    for name in reached {
        if om.contains_key(name) && !nm.contains_key(name) {
            r.push(
                Severity::Breaking,
                ChangeKind::TypeRemoved,
                name,
                format!("type removed while still referenced ({name}): dangling reference"),
            );
        }
    }
}

/// A zero-copy account, read once with every old `serialization` and `repr`, and once with
/// every new one. One reading per side, because changes on nested types can cancel out.
struct Root {
    name: String,
    before: Reading,
    after: Reading,
    /// The account type and every type it holds, at any depth.
    holds: BTreeSet<String>,
    effect: (LayoutEffect, String),
}

/// Only accounts are cast from memory, as in Shipstern's converter: instruction args and
/// events are always Borsh. The new attributes go on the old types, so field changes,
/// which are reported on their own, do not count twice.
fn zero_copy_roots(old: &Idl, new_types: &BTreeMap<String, &TypeDef>) -> Vec<Root> {
    let mut hybrid = old.clone();
    for td in &mut hybrid.types {
        if let Some(nt) = new_types.get(&td.name) {
            td.serialization.clone_from(&nt.serialization);
            td.repr = nt.repr;
        }
    }
    let (oi, hi) = (TypeIndex::new(old), TypeIndex::new(&hybrid));
    old.accounts
        .iter()
        .filter_map(|account| {
            let (before, after) = (oi.get(&account.name)?, hi.get(&account.name)?);
            (before.is_zero_copy() || after.is_zero_copy()).then(|| Root {
                name: account.name.clone(),
                before: reading(before),
                after: reading(after),
                holds: held_types(&oi, &account.name),
                effect: layout_effect(&oi, before, &hi, after),
            })
        })
        .collect()
}

fn reading(td: &TypeDef) -> Reading {
    if td.is_zero_copy() {
        Reading::ZeroCopy
    } else {
        Reading::Borsh
    }
}

/// `serialization` and `repr` only matter where a type is cast as zero-copy memory. Judge
/// the change by the zero-copy accounts that hold this type and whose reading it is part of.
fn diff_layout_attrs(
    name: &str,
    ot: &TypeDef,
    nt: &TypeDef,
    reached: &BTreeSet<String>,
    roots: &[Root],
    r: &mut Report,
) {
    let repr = |td: &TypeDef| {
        td.repr
            .map_or_else(|| "none".to_string(), |r| r.to_string())
    };
    let what = match (ot.serialization != nt.serialization, ot.repr != nt.repr) {
        (true, false) => format!(
            "serialization changed ({name}): {} -> {}",
            ot.serialization, nt.serialization
        ),
        (false, true) => format!("repr changed ({name}): {} -> {}", repr(ot), repr(nt)),
        _ => format!(
            "serialization and repr changed ({name}): {} {} -> {} {}",
            ot.serialization,
            repr(ot),
            nt.serialization,
            repr(nt)
        ),
    };
    if !reached.contains(name) {
        r.push(
            Severity::Cosmetic,
            ChangeKind::LayoutChanged,
            name,
            format!("{what} in an UNREACHED type"),
        );
        return;
    }
    let holders: Vec<&Root> = roots
        .iter()
        .filter(|root| root.holds.contains(name))
        .collect();
    // A zero-copy reading uses the account's own serialization and the repr of every type
    // inside it. A nested type's serialization has no effect on it.
    let repr_changed = layout::layout_repr(ot) != layout::layout_repr(nt);
    let worst = holders
        .iter()
        .filter(|root| {
            (root.name == name && root.before != root.after)
                || (root.after == Reading::ZeroCopy && repr_changed)
        })
        .map(|root| &root.effect)
        .max_by_key(|effect| effect.0);
    let (severity, detail) = match worst {
        None if holders.is_empty() => (
            Severity::Cosmetic,
            "only read as Borsh, which ignores both".to_string(),
        ),
        None => (
            Severity::Cosmetic,
            "no zero-copy reading depends on this change, so the bytes are the same".to_string(),
        ),
        Some((LayoutEffect::None, _)) => (
            Severity::Cosmetic,
            "no field of a zero-copy type moves, so the bytes are the same".to_string(),
        ),
        Some((LayoutEffect::Size, detail)) => (Severity::Dangerous, detail.clone()),
        Some((LayoutEffect::Moves, detail)) => (Severity::Breaking, detail.clone()),
    };
    r.push(
        severity,
        ChangeKind::LayoutChanged,
        name,
        format!("{what}: {detail}"),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LayoutEffect {
    None,
    Size,
    Moves,
}

/// Compare how a parser reads `before` and `after`; the string explains a non-`None` effect.
/// The two must have the same fields and differ only in `serialization` and `repr`.
fn layout_effect(
    bi: &TypeIndex<'_>,
    before: &TypeDef,
    ai: &TypeIndex<'_>,
    after: &TypeDef,
) -> (LayoutEffect, String) {
    let ty = Type::Defined {
        name: before.name.clone(),
        generics: Vec::new(),
    };
    let read =
        |index: &TypeIndex<'_>, td: &TypeDef| -> Result<(Vec<layout::Leaf>, usize), LayoutError> {
            Ok((
                index.leaves(td, reading(td))?,
                index.reading_size(&ty, reading(td))?,
            ))
        };
    let type_name = &before.name;
    match (read(bi, before), read(ai, after)) {
        (Ok((bl, bs)), Ok((al, as_))) => {
            // Same fields, so the leaves pair up by position. Names can repeat, such as
            // `padding_0` and `_padding_0`, which are both `padding0` in camelCase.
            let moved = bl.iter().zip(&al).find(|(b, a)| a.offset != b.offset);
            if let Some((b, a)) = moved {
                return (
                    LayoutEffect::Moves,
                    format!(
                        "moves `{type_name}.{}` from offset {} to {} (zero-copy layout)",
                        b.path, b.offset, a.offset
                    ),
                );
            }
            if bs == as_ {
                (LayoutEffect::None, String::new())
            } else {
                (
                    LayoutEffect::Size,
                    format!("no field moves, but {type_name} is {as_} bytes, was {bs}"),
                )
            }
        }
        // Without a layout, a field can move unseen, even when both sides fail the same way.
        (Err(e), _) | (_, Err(e)) => (
            LayoutEffect::Moves,
            format!(
                "the zero-copy layout of {type_name} can't be computed, so a field may move: {e}"
            ),
        ),
    }
}

/// `root` and every type it holds through fields, variant payloads and aliases.
fn held_types(index: &TypeIndex<'_>, root: &str) -> BTreeSet<String> {
    let mut held = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(name) = stack.pop() {
        if held.contains(&name) {
            continue;
        }
        if let Some(td) = index.get(&name) {
            for ty in body_types(&td.ty) {
                collect_defined_refs(ty, &mut stack);
            }
        }
        held.insert(name);
    }
    held
}

fn diff_fields(ty: &str, of: &[Field], nf: &[Field], reached: bool, r: &mut Report) {
    let max = of.len().max(nf.len());
    for pos in 0..max {
        let (severity, kind, change) = match (of.get(pos), nf.get(pos)) {
            (Some(o), Some(n)) if o != n => {
                let edit = field_edit(of, nf, o, n);
                let kind = match edit {
                    FieldEdit::Changed => ChangeKind::FieldChanged,
                    FieldEdit::Moved => ChangeKind::FieldMoved,
                    FieldEdit::Renamed => ChangeKind::FieldRenamed,
                };
                (edit.severity(), kind, field_change(o, n))
            }
            (Some(o), None) => (
                Severity::Breaking,
                ChangeKind::FieldRemoved,
                format!("{} removed (Borsh layout)", field_str(o)),
            ),
            (None, Some(n)) => (
                Severity::Breaking,
                ChangeKind::FieldAdded,
                format!("{} added (Borsh layout)", field_str(n)),
            ),
            _ => continue,
        };
        let subject = format!("{ty}#{pos}");
        if reached {
            r.push(
                severity,
                kind,
                subject,
                format!("FIELD changed ({ty}#{pos}) via reachable type: {change}"),
            );
        } else {
            r.push(
                Severity::Cosmetic,
                kind,
                subject,
                format!("field changed in UNREACHED type ({ty}#{pos}): dead code"),
            );
        }
    }
}

fn diff_variants(
    ty: &str,
    ov: &[crate::model::EnumVariant],
    nv: &[crate::model::EnumVariant],
    reached: bool,
    r: &mut Report,
) {
    let old_max = ov.len().saturating_sub(1);
    let max = ov.len().max(nv.len());
    for pos in 0..max {
        let (severity, kind, message) = match (ov.get(pos), nv.get(pos)) {
            (Some(o), Some(n)) if o.name == n.name && o.tag(pos) != n.tag(pos) => (
                Severity::Breaking,
                ChangeKind::VariantTagChanged,
                format!(
                    "enum variant tag value changed ({ty}#{pos} {}): {} -> {}",
                    o.name,
                    o.tag(pos),
                    n.tag(pos)
                ),
            ),
            (Some(o), Some(n)) if o.name == n.name && !same_payload(&o.fields, &n.fields) => (
                Severity::Breaking,
                ChangeKind::VariantPayloadChanged,
                format!(
                    "enum variant payload changed ({ty}#{pos} {}): {} -> {} (Borsh layout)",
                    o.name,
                    payload_str(&o.fields),
                    payload_str(&n.fields)
                ),
            ),
            // Either name at another index on the other side means the tag now selects a
            // different variant. A fresh name with the same payload is a rename.
            (Some(o), Some(n)) if o.name != n.name => {
                let moved = ov.iter().any(|v| v.name == n.name)
                    || nv.iter().any(|v| v.name == o.name)
                    || !same_payload(&o.fields, &n.fields);
                if moved {
                    (
                        Severity::Breaking,
                        ChangeKind::VariantMoved,
                        format!(
                            "enum variant changed at index ({ty}#{pos}): {} -> {} (tag reassigned)",
                            o.name, n.name
                        ),
                    )
                } else {
                    (
                        Severity::Dangerous,
                        ChangeKind::VariantRenamed,
                        format!(
                            "enum variant renamed ({ty}#{pos}): {} -> {} (same tag and payload)",
                            o.name, n.name
                        ),
                    )
                }
            }
            (None, Some(_)) if pos > old_max => (
                Severity::Dangerous,
                ChangeKind::VariantAppended,
                format!("enum variant APPENDED ({ty}#{pos}): existing tags unchanged, parser must handle new variant"),
            ),
            (None, Some(_)) => (
                Severity::Breaking,
                ChangeKind::VariantInserted,
                format!("enum variant INSERTED at non-terminal index ({ty}#{pos}): shifts later tags"),
            ),
            (Some(_), None) => (
                Severity::Breaking,
                ChangeKind::VariantRemoved,
                format!("enum variant removed ({ty}#{pos}): shifts later tags"),
            ),
            _ => continue,
        };
        let subject = format!("{ty}#{pos}");
        if reached {
            r.push(severity, kind, subject, message);
        } else {
            r.push(
                Severity::Cosmetic,
                kind,
                subject,
                format!("variant changed in UNREACHED enum ({ty}#{pos})"),
            );
        }
    }
}

/// A unit variant may be written as an empty tuple or an empty field list.
fn same_payload(a: &EnumFields, b: &EnumFields) -> bool {
    let empty = |f: &EnumFields| match f {
        EnumFields::Named(v) => v.is_empty(),
        EnumFields::Tuple(v) => v.is_empty(),
    };
    a == b || (empty(a) && empty(b))
}

fn payload_str(fields: &EnumFields) -> String {
    match fields {
        EnumFields::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(tystr).collect();
            format!("({})", parts.join(", "))
        }
        EnumFields::Named(fields) => {
            let parts: Vec<String> = fields.iter().map(field_str).collect();
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

/// Names of the types a parser reads: account and event types, types named in
/// instruction args, and everything their fields reference.
#[must_use]
pub fn reachable_types(idl: &Idl) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    for a in &idl.accounts {
        reached.insert(a.name.clone());
    }
    for e in &idl.events {
        reached.insert(e.name.clone());
    }
    for ix in &idl.instructions {
        for arg in &ix.args {
            let mut refs = Vec::new();
            collect_defined_refs(&arg.ty, &mut refs);
            for name in refs {
                reached.insert(name);
            }
        }
    }
    let tm = idl.type_map();
    let mut changed = true;
    while changed {
        changed = false;
        let snapshot: Vec<String> = reached.iter().cloned().collect();
        for tname in snapshot {
            let Some(td) = tm.get(&tname) else { continue };
            let mut refs = Vec::new();
            for ty in body_types(&td.ty) {
                collect_defined_refs(ty, &mut refs);
            }
            for r in refs {
                if reached.insert(r) {
                    changed = true;
                }
            }
        }
    }
    reached
}

/// Every type a definition's body mentions: struct fields, variant payloads, alias target.
fn body_types(ty: &TypeDefTy) -> Vec<&Type> {
    match ty {
        TypeDefTy::Struct { fields } => fields.iter().map(|f| &f.ty).collect(),
        TypeDefTy::Enum { variants, .. } => variants
            .iter()
            .flat_map(|v| match &v.fields {
                EnumFields::Named(fields) => fields.iter().map(|f| &f.ty).collect::<Vec<_>>(),
                EnumFields::Tuple(items) => items.iter().collect(),
            })
            .collect(),
        TypeDefTy::Type { alias } => vec![alias],
    }
}

/// Instructions are matched by name, so a duplicate name would hide drift on all but one.
fn report_duplicate_names(instructions: &[crate::model::Instruction], r: &mut Report) {
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

/// Shipstern names each instruction helper `parse_{to_snake_case(name)}`, which lowercases
/// ASCII only, so `ABC` and `Abc` give one helper and the generated parser fails to compile.
fn report_generated_name_collisions(instructions: &[crate::model::Instruction], r: &mut Report) {
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
        _ => shipstern_snake_case(&crate::model::camel(&idl.metadata.name)),
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

fn collect_defined_refs(t: &Type, out: &mut Vec<String>) {
    // A generic instantiation like `Wrapper<Node>` references Node too.
    if let Type::Defined { name, .. } = t {
        out.push(name.clone());
    }
    for child in t.children() {
        collect_defined_refs(child, out);
    }
}

fn field_str(f: &Field) -> String {
    format!("{}: {}", f.name, tystr(&f.ty))
}

/// Name both sides, so a rename doesn't read as `u64 -> u64`.
fn field_change(old: &Field, new: &Field) -> String {
    let (ot, nt) = (tystr(&old.ty), tystr(&new.ty));
    if old.name == new.name {
        format!("{}: {ot} -> {nt} (Borsh layout)", old.name)
    } else if old.ty == new.ty {
        format!("{} renamed to {} ({ot})", old.name, new.name)
    } else {
        format!("{}: {ot} -> {}: {nt} (Borsh layout)", old.name, new.name)
    }
}

/// A type in Rust-like syntax, such as `Option<u64>` or `[u8; 32]`.
fn tystr(t: &Type) -> String {
    let list = |items: &[Type]| items.iter().map(tystr).collect::<Vec<_>>().join(", ");
    match t {
        Type::Bool => "bool".into(),
        Type::U8 => "u8".into(),
        Type::I8 => "i8".into(),
        Type::U16 => "u16".into(),
        Type::I16 => "i16".into(),
        Type::U32 => "u32".into(),
        Type::I32 => "i32".into(),
        Type::U64 => "u64".into(),
        Type::I64 => "i64".into(),
        Type::U128 => "u128".into(),
        Type::I128 => "i128".into(),
        Type::U256 => "u256".into(),
        Type::I256 => "i256".into(),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Bytes => "bytes".into(),
        Type::String => "string".into(),
        Type::Pubkey => "pubkey".into(),
        Type::Option(i) => format!("Option<{}>", tystr(i)),
        Type::COption(i) => format!("COption<{}>", tystr(i)),
        Type::Vec(i) => format!("Vec<{}>", tystr(i)),
        Type::Array(i, ArrayLen::Value(n)) => format!("[{}; {n}]", tystr(i)),
        Type::Array(i, ArrayLen::Generic(n)) => format!("[{}; {n}]", tystr(i)),
        Type::Map(k, v) => format!("Map<{}, {}>", tystr(k), tystr(v)),
        Type::Set(i) => format!("Set<{}>", tystr(i)),
        Type::Tuple(items) => format!("({})", list(items)),
        Type::Defined { name, generics } if generics.is_empty() => name.clone(),
        Type::Defined { name, generics } => {
            let args: Vec<String> = generics
                .iter()
                .map(|g| match g {
                    GenericArg::Type { ty } => tystr(ty),
                    GenericArg::Const { value } => value.clone(),
                })
                .collect();
            format!("{name}<{}>", args.join(", "))
        }
        Type::Generic(name) => name.clone(),
        Type::Struct(fields) => {
            let fields: Vec<String> = fields.iter().map(field_str).collect();
            format!("{{ {} }}", fields.join(", "))
        }
        Type::ShortU16 => "shortU16".into(),
        Type::BigEndian(i) => format!("{} big-endian", tystr(i)),
        Type::Counted {
            ty,
            count: Count::Prefix(p),
        } => format!("{} with {} length", tystr(ty), tystr(p)),
        Type::Counted {
            ty,
            count: Count::Remainder,
        } => format!("{} to end of data", tystr(ty)),
        Type::CodamaOption { item, tag } => {
            let tag = match tag {
                OptionTag::Prefix { ty, fixed: false } => format!("{} tag", tystr(ty)),
                OptionTag::Prefix { ty, fixed: true } => format!("{} tag, fixed size", tystr(ty)),
                OptionTag::Zeroable => "zero means none".to_string(),
                OptionTag::Remainder => "none at end of data".to_string(),
            };
            format!("Option<{}> ({tag})", tystr(item))
        }
    }
}
