//! The flat IDL model that both tools work on.
//!
//! [`Idl::from_json`] reads Anchor JSON (0.30 spec and legacy) and Codama `rootNode` JSON
//! into the same structs. Every optional field defaults, so an IDL that omits
//! `writable: false` and one that spells it out compare equal.

use serde::Deserialize;
use std::collections::BTreeMap;

pub type Discriminator = Vec<u8>;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Idl {
    pub address: String,
    pub metadata: Metadata,
    #[serde(default)]
    pub instructions: Vec<Instruction>,
    #[serde(default)]
    pub accounts: Vec<NamedDisc>,
    /// Events, each with a content discriminator and an optional self-CPI envelope.
    #[serde(default)]
    pub events: Vec<Event>,
    #[serde(default)]
    pub errors: Vec<ErrorCode>,
    #[serde(default)]
    pub types: Vec<TypeDef>,
    /// The file format this came from, which sets the naming convention.
    #[serde(skip)]
    pub format: Format,
}

/// Where an IDL came from. Each names things its own way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// Anchor 0.30 spec: `snake_case` instructions and fields, `PascalCase` types.
    #[default]
    Anchor,
    /// Pre-0.30 Anchor: `camelCase` instructions and fields, `PascalCase` types.
    LegacyAnchor,
    /// Codama: `camelCase` everything.
    Codama,
    /// Shank: the legacy Anchor shape, a number tag on each instruction, and no account
    /// discriminators.
    Shank,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Metadata {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub spec: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Instruction {
    pub name: String,
    /// Anchor uses 8 bytes, Quasar 1. Empty for native programs that dispatch by tag or
    /// data length.
    #[serde(default)]
    pub discriminator: Discriminator,
    /// Where the discriminator starts in the data (Codama only; Anchor's is at 0).
    #[serde(default)]
    pub discriminator_offset: usize,
    /// Later Codama discriminators, by offset. Codama requires every one to match, but
    /// Shipstern dispatches on `discriminator` alone.
    #[serde(default)]
    pub extra_discriminators: Vec<(usize, Discriminator)>,
    /// Codama `sizeDiscriminatorNode` at any position: the data length must match too.
    #[serde(default)]
    pub size_discriminator: Option<usize>,
    #[serde(default)]
    pub accounts: Vec<AccountItem>,
    #[serde(default)]
    pub args: Vec<Field>,
    /// Explicit start of the arguments; `None` means right after the discriminators.
    #[serde(default)]
    pub payload_offset: Option<usize>,
    /// How a caller passes an absent optional account. Anchor IDLs do not say.
    #[serde(skip)]
    pub optional_accounts: OptionalAccounts,
}

/// Codama `optionalAccountStrategy`: what goes in the place of an absent optional account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OptionalAccounts {
    /// The program id, so each later account keeps its position. Anchor always does this.
    #[default]
    ProgramId,
    /// Nothing, so each later account moves one position earlier.
    Omitted,
}

/// A single account, or a composite group (told apart by its `accounts` array).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AccountItem {
    Composite {
        name: String,
        accounts: Vec<AccountItem>,
    },
    Single(Account),
}

// The bools are the IDL's own account flags, one each.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Account {
    pub name: String,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub signer: bool,
    /// Codama `isSigner: "either"`: `signer` is set, but the account need not sign.
    #[serde(skip)]
    pub may_sign: bool,
    #[serde(default)]
    pub optional: bool,
    /// Fixed address (a program or sysvar). Not part of the positional diff.
    #[serde(default)]
    pub address: Option<String>,
    /// Codama public-key default: `address` fills the slot when the caller gives no key, but
    /// the program can accept another key.
    #[serde(skip)]
    pub address_is_default: bool,
    /// The seeds the address derives from. Not part of the positional diff.
    #[serde(default, deserialize_with = "deserialize_pda")]
    pub pda: Option<Pda>,
}

/// How an account address derives from seeds: Anchor `pda`, or a Codama `pdaValueNode`
/// default.
#[derive(Debug, Clone, PartialEq)]
pub struct Pda {
    pub seeds: Vec<Seed>,
    /// The program that derives the address; `None` means the instruction's own program.
    pub program: Option<Seed>,
}

/// One PDA seed.
#[derive(Debug, Clone, PartialEq)]
pub enum Seed {
    /// Fixed bytes.
    Const(Vec<u8>),
    /// The key of another account in the same instruction, by name.
    Account(String),
    /// An instruction argument, by name, or a field inside a struct argument by a dotted path
    /// such as `params.id`.
    Arg(String),
    /// A seed the model cannot describe, such as a field of another account's data.
    Other,
}

/// Read an Anchor `pda`. A seed of an unknown shape becomes [`Seed::Other`], so the field
/// never stops an IDL from loading.
fn deserialize_pda<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Pda>, D::Error> {
    let Some(value) = Option::<serde_json::Value>::deserialize(d)? else {
        return Ok(None);
    };
    let Some(seeds) = value.get("seeds").and_then(serde_json::Value::as_array) else {
        return Ok(None);
    };
    Ok(Some(Pda {
        seeds: seeds.iter().map(anchor_seed).collect(),
        // Legacy Anchor names the program `programId`.
        program: value
            .get("program")
            .or_else(|| value.get("programId"))
            .map(anchor_seed),
    }))
}

fn anchor_seed(seed: &serde_json::Value) -> Seed {
    let text = |key| seed.get(key).and_then(serde_json::Value::as_str);
    let path = text("path").map(str::to_string);
    match (text("kind"), path) {
        (Some("const"), _) => match seed.get("value") {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect::<Option<Vec<u8>>>()
                .map_or(Seed::Other, Seed::Const),
            // Legacy Anchor writes a string seed as text.
            Some(serde_json::Value::String(s)) if text("type") == Some("string") => {
                Seed::Const(s.as_bytes().to_vec())
            }
            _ => Seed::Other,
        },
        // A dotted account path reads a field of that account's data, which the model cannot
        // describe.
        (Some("account"), Some(path)) if !path.contains('.') => Seed::Account(path),
        (Some("arg"), Some(path)) => Seed::Arg(path),
        _ => Seed::Other,
    }
}

/// An account type. Native programs may give no discriminator and tell types apart by size.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct NamedDisc {
    pub name: String,
    #[serde(default)]
    pub discriminator: Discriminator,
    /// Where the discriminator starts in the data (Codama only; Anchor's is at 0).
    #[serde(default)]
    pub discriminator_offset: usize,
    /// Later Codama discriminators, by offset. Codama requires every one to match, but
    /// Shipstern's account parser checks only `discriminator`.
    #[serde(default)]
    pub extra_discriminators: Vec<(usize, Discriminator)>,
    /// Declared account size in bytes, discriminator included (Codama `size`).
    #[serde(default)]
    pub size: Option<usize>,
    /// Codama `sizeDiscriminatorNode`: the parser selects the account by its data length.
    #[serde(default)]
    pub size_discriminator: Option<usize>,
    /// Explicit body start; `None` means right after the discriminator.
    #[serde(default)]
    pub payload_offset: Option<usize>,
}

impl Instruction {
    /// Where the arguments start: `payload_offset` when the IDL sets it, else after the
    /// discriminator bytes.
    #[must_use]
    pub fn payload_start(&self) -> usize {
        self.payload_offset.unwrap_or_else(|| {
            discriminator_end(
                self.discriminator_offset,
                &self.discriminator,
                &self.extra_discriminators,
            )
        })
    }
}

impl NamedDisc {
    /// Where the account body starts: after the discriminator bytes, unless the IDL says the
    /// discriminator byte is also the body's first field (SPL Governance).
    #[must_use]
    pub fn payload_start(&self) -> usize {
        self.payload_offset.unwrap_or_else(|| {
            discriminator_end(
                self.discriminator_offset,
                &self.discriminator,
                &self.extra_discriminators,
            )
        })
    }
}

/// The end of the discriminator bytes: the end of the first discriminator, moved on by each
/// later one that starts where the bytes so far end. A gap stops the run, because a field can
/// fill the gap.
#[must_use]
pub fn discriminator_end(offset: usize, first: &[u8], extra: &[(usize, Discriminator)]) -> usize {
    let mut end = offset.saturating_add(first.len());
    // Each step must move forward: an IDL offset near usize::MAX must not loop forever.
    for _ in 0..extra.len() {
        let next = extra
            .iter()
            .find(|(at, d)| *at == end && !d.is_empty())
            .and_then(|(at, d)| at.checked_add(d.len()));
        match next {
            Some(next) if next > end => end = next,
            _ => break,
        }
    }
    end
}

/// A program event. The envelope is the self-CPI wrap around the payload: Anchor's 8-byte
/// event tag or Pinocchio's 1-byte one. Empty envelope fields mean the IDL doesn't say.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Event {
    pub name: String,
    /// Content discriminator, such as Anchor's event sighash.
    #[serde(default)]
    pub discriminator: Discriminator,
    /// Where the content discriminator starts, after any envelope (Codama only).
    #[serde(default)]
    pub discriminator_offset: usize,
    /// More constants of a Codama chain, by offset after any envelope. Shipstern checks them
    /// all and reads the payload after the last one.
    #[serde(default)]
    pub extra_discriminators: Vec<(usize, Discriminator)>,
    /// Shipstern's `cpi_event_discriminator`.
    #[serde(default)]
    pub envelope_discriminator: Discriminator,
    /// Shipstern's `cpi_event_payload_offset`: where payload decoding starts.
    #[serde(default)]
    pub envelope_payload_offset: Option<usize>,
}

impl Event {
    /// Where the payload starts in the event bytes, after the envelope is removed: after the
    /// last discriminator, as Shipstern reads a chain.
    #[must_use]
    pub fn payload_start(&self) -> usize {
        self.extra_discriminators
            .iter()
            .map(|(at, disc)| at.saturating_add(disc.len()))
            .fold(
                self.discriminator_offset
                    .saturating_add(self.discriminator.len()),
                usize::max,
            )
    }

    /// The explicit payload offset, else the envelope length when there is an envelope.
    #[must_use]
    pub fn effective_payload_offset(&self) -> Option<usize> {
        self.envelope_payload_offset
            .or((!self.envelope_discriminator.is_empty())
                .then_some(self.envelope_discriminator.len()))
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ErrorCode {
    pub code: u32,
    pub name: String,
    #[serde(default)]
    pub msg: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: Type,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TypeDef {
    pub name: String,
    /// `borsh`, `bytemuck`, `bytemuckunsafe`, or `custom:<name>` for Anchor's
    /// `{"custom": "<name>"}`, whose layout the IDL does not describe.
    #[serde(
        default = "default_serialization",
        deserialize_with = "deserialize_serialization"
    )]
    pub serialization: String,
    /// Memory representation, which sets the layout of zero-copy types.
    #[serde(default)]
    pub repr: Option<Repr>,
    #[serde(default)]
    pub generics: Vec<GenericParam>,
    #[serde(rename = "type")]
    pub ty: TypeDefTy,
}

pub(crate) fn default_serialization() -> String {
    "borsh".to_string()
}

/// The spec writes a custom serializer as `{"custom": "<name>"}`.
fn deserialize_serialization<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Serialization {
        Name(String),
        Custom { custom: String },
    }
    Ok(match Serialization::deserialize(d)? {
        Serialization::Name(name) => name,
        Serialization::Custom { custom } => format!("custom:{custom}"),
    })
}

impl TypeDef {
    /// `true` for Anchor `zero_copy` types, which are cast from account bytes instead of
    /// Borsh-decoded.
    #[must_use]
    pub fn is_zero_copy(&self) -> bool {
        matches!(self.serialization.as_str(), "bytemuck" | "bytemuckunsafe")
    }
}

/// `#[repr]` of a type, as the Solana IDL spec records it.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct Repr {
    pub kind: ReprKind,
    #[serde(default)]
    pub packed: bool,
    #[serde(default)]
    pub align: Option<usize>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReprKind {
    Rust,
    C,
    Transparent,
}

impl std::fmt::Display for Repr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            ReprKind::Rust => "Rust",
            ReprKind::C => "C",
            ReprKind::Transparent => "transparent",
        };
        write!(f, "repr({kind}")?;
        if self.packed {
            f.write_str(", packed")?;
        }
        if let Some(n) = self.align {
            write!(f, ", align({n})")?;
        }
        f.write_str(")")
    }
}

/// A generic parameter declared on a type definition.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GenericParam {
    Type {
        name: String,
    },
    Const {
        name: String,
        #[serde(rename = "type")]
        ty: String,
    },
}

impl GenericParam {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Type { name } | Self::Const { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TypeDefTy {
    Struct {
        #[serde(default)]
        fields: Vec<Field>,
    },
    Enum {
        #[serde(default)]
        variants: Vec<EnumVariant>,
        /// Integer type of the variant tag (Codama `size`); Borsh's `u8` when absent.
        #[serde(default)]
        tag: Option<Type>,
    },
    /// A type alias, e.g. `{ "kind": "type", "alias": "u64" }`.
    Type { alias: Type },
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    /// Variant payload. Anchor emits either named fields
    /// (`[{"name":"x","type":"u64"}]`) or tuple fields (`["u64"]`).
    #[serde(default)]
    pub fields: EnumFields,
    /// Explicit tag value (Codama); the variant's index when absent.
    #[serde(default)]
    pub discriminator: Option<u32>,
}

impl EnumVariant {
    /// The tag value that selects this variant, at position `index` in the list.
    #[must_use]
    pub fn tag(&self, index: usize) -> u64 {
        self.discriminator
            .map_or_else(|| u64::try_from(index).unwrap_or(u64::MAX), u64::from)
    }
}

/// Named or tuple variant fields. `Tuple` comes first because serde's untagged fallback
/// after a half-parsed `Vec<Field>` is unreliable.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EnumFields {
    Tuple(Vec<Type>),
    Named(Vec<Field>),
}

impl Default for EnumFields {
    fn default() -> Self {
        Self::Tuple(Vec::new())
    }
}

/// An IDL type. `Defined` names an entry in `types`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Type {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    U128,
    I128,
    U256,
    I256,
    F32,
    F64,
    Bytes,
    String,
    Pubkey,
    Option(Box<Type>),
    /// Solana `COption`: a `u32` tag, then the value, which is present even when the tag is 0.
    #[serde(rename = "coption")]
    COption(Box<Type>),
    Vec(Box<Type>),
    Array(Box<Type>, ArrayLen),
    /// Borsh map: a `u32` length, then key and value pairs.
    Map(Box<Type>, Box<Type>),
    /// Borsh set: a `u32` length, then the items.
    Set(Box<Type>),
    /// Elements in order, with no length prefix.
    Tuple(Vec<Type>),
    /// A type from `types`. The generic arguments are part of the identity:
    /// `Wrapper<u64>` and `Wrapper<u32>` have different layouts.
    Defined {
        name: String,
        #[serde(default)]
        generics: Vec<GenericArg>,
    },
    Generic(String),
    /// Codama inline struct: the fields in order, as Borsh writes a struct.
    #[serde(skip)]
    Struct(Vec<Field>),
    /// Codama `shortU16`: 1 to 3 bytes, 7 bits in each.
    #[serde(skip)]
    ShortU16,
    /// A Codama big-endian number. The inner type is the number.
    #[serde(skip)]
    BigEndian(Box<Type>),
    /// A Codama `Bytes`, `String`, `Vec`, `Map` or `Set` whose length is not Borsh's `u32`.
    #[serde(skip)]
    Counted {
        ty: Box<Type>,
        count: Count,
    },
    /// A Codama option that is not Borsh's `Option` or Solana's `COption`.
    #[serde(skip)]
    CodamaOption {
        item: Box<Type>,
        tag: OptionTag,
    },
}

/// Where a Codama collection gets its length.
#[derive(Debug, Clone, PartialEq)]
pub enum Count {
    /// A number of this type before the items. For `Bytes` and `String` it counts bytes.
    Prefix(Box<Type>),
    /// The items fill the rest of the data.
    Remainder,
}

/// How a Codama option tells that the item is there.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionTag {
    /// A number before the item: 0 for absent, 1 for present. With `fixed`, an absent
    /// item still takes its bytes.
    Prefix { ty: Box<Type>, fixed: bool },
    /// No tag: an item of only zero bytes is absent.
    Zeroable,
    /// No tag: the item is absent when no bytes are left.
    Remainder,
}

impl Type {
    /// The name in a `Generic` without the `#<hash>` that the Codama reader adds to a node it
    /// cannot read. The hash only tells two contents of the node apart.
    #[must_use]
    pub fn generic_kind(name: &str) -> &str {
        name.split_once('#').map_or(name, |(kind, _)| kind)
    }

    /// The types this one holds, so a walk over a type tree needs no list of variants.
    /// Length and tag prefixes are numbers, not data, so they are not included.
    #[must_use]
    pub fn children(&self) -> Vec<&Type> {
        match self {
            Self::Option(inner)
            | Self::COption(inner)
            | Self::Vec(inner)
            | Self::Array(inner, _)
            | Self::Set(inner)
            | Self::BigEndian(inner)
            | Self::Counted { ty: inner, .. }
            | Self::CodamaOption { item: inner, .. } => vec![inner],
            Self::Map(key, value) => vec![key, value],
            Self::Tuple(items) => items.iter().collect(),
            Self::Struct(fields) => fields.iter().map(|f| &f.ty).collect(),
            Self::Defined { generics, .. } => generics
                .iter()
                .filter_map(|g| match g {
                    GenericArg::Type { ty } => Some(&**ty),
                    GenericArg::Const { .. } => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// A generic argument to a `Defined` type.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GenericArg {
    /// A type argument, e.g. the `u64` in `Wrapper<u64>`.
    Type {
        #[serde(rename = "type")]
        ty: Box<Type>,
    },
    /// A const argument, e.g. the `8` in `Buffer<8>`.
    Const { value: String },
}

/// The length in a const generic argument, read the way Codama's Anchor converter and Shipstern
/// read it (JS `parseInt`): leading spaces, an optional `+`, hex after `0x`, digits up to the
/// first other character.
#[must_use]
pub fn const_len(value: &str) -> Option<usize> {
    let s = value.trim_start();
    let s = s.strip_prefix('+').unwrap_or(s);
    let (digits, radix) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => (hex, 16),
        None => (s, 10),
    };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    usize::from_str_radix(digits.get(..end)?, radix).ok()
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(from = "ArrayLenJson")]
pub enum ArrayLen {
    Value(usize),
    /// Name of a const generic parameter.
    Generic(String),
}

/// The spec writes a generic length as `{"generic": "N"}`; accept a bare name too.
#[derive(Deserialize)]
#[serde(untagged)]
enum ArrayLenJson {
    Value(usize),
    Generic { generic: String },
    Name(String),
}

impl From<ArrayLenJson> for ArrayLen {
    fn from(len: ArrayLenJson) -> Self {
        match len {
            ArrayLenJson::Value(n) => Self::Value(n),
            ArrayLenJson::Generic { generic: name } | ArrayLenJson::Name(name) => {
                Self::Generic(name)
            }
        }
    }
}

impl Idl {
    /// Parse Codama `rootNode` JSON, or Anchor JSON in the 0.30 spec or a legacy shape.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let mut value: serde_json::Value = serde_json::from_str(s)?;
        if value.get("kind").and_then(|k| k.as_str()) == Some("rootNode") {
            // Spec 2 renames fields, so a parse error would name a field and not the cause.
            if let Some(version) = value.get("version").and_then(|v| v.as_str()) {
                let major = version
                    .split('.')
                    .next()
                    .and_then(|m| m.parse::<u64>().ok());
                if major.is_some_and(|m| m >= 2) {
                    return Err(serde::de::Error::custom(format!(
                        "Codama spec {version} is not supported; only spec 1.x can be read"
                    )));
                }
            }
            crate::codama_convert::fix_string_error_codes(&mut value);
            let root: codama_nodes::RootNode = serde_json::from_value(value)?;
            Ok(crate::codama_convert::from_codama(root))
        } else {
            // Anchor puts spec in metadata. A top-level spec is another format, for example Quasar.
            if let Some(spec) = value.get("spec").and_then(|s| s.as_str()) {
                return Err(serde::de::Error::custom(format!(
                    "IDL spec {spec} is not supported; only Anchor, Shank and Codama IDLs can be read"
                )));
            }
            // Anchor reads only spec 0.1.0. Another spec can rename fields, which serde then
            // reads as empty, so a diff would not see them.
            if let Some(spec) = value.pointer("/metadata/spec").and_then(|s| s.as_str()) {
                if spec != "0.1.0" {
                    return Err(serde::de::Error::custom(format!(
                        "Anchor IDL spec {spec} is not supported; only spec 0.1.0 can be read"
                    )));
                }
            }
            let shank = value.pointer("/metadata/origin").and_then(|o| o.as_str()) == Some("shank");
            if shank {
                shank_discriminators(&mut value)?;
            }
            let legacy = normalize_legacy_anchor(&mut value, shank);
            let mut idl: Self = serde_json::from_value(value)?;
            if shank {
                idl.format = Format::Shank;
            } else if legacy {
                idl.format = Format::LegacyAnchor;
            }
            Ok(idl)
        }
    }

    /// A copy with every name in `camelCase`, as Codama writes them, so IDLs from different
    /// formats can be compared name for name.
    #[must_use]
    pub fn with_camel_case_names(&self) -> Self {
        let mut idl = self.clone();
        for ix in &mut idl.instructions {
            ix.name = camel(&ix.name);
            camel_accounts(&mut ix.accounts);
            ix.args.iter_mut().for_each(camel_field);
        }
        for account in &mut idl.accounts {
            account.name = camel(&account.name);
        }
        for event in &mut idl.events {
            event.name = camel(&event.name);
        }
        for error in &mut idl.errors {
            error.name = camel(&error.name);
        }
        for td in &mut idl.types {
            td.name = camel(&td.name);
            match &mut td.ty {
                TypeDefTy::Struct { fields } => fields.iter_mut().for_each(camel_field),
                TypeDefTy::Enum { variants, .. } => {
                    for variant in variants {
                        variant.name = camel(&variant.name);
                        match &mut variant.fields {
                            EnumFields::Named(fields) => fields.iter_mut().for_each(camel_field),
                            EnumFields::Tuple(items) => items.iter_mut().for_each(camel_type),
                        }
                    }
                }
                TypeDefTy::Type { alias } => camel_type(alias),
            }
        }
        idl
    }

    /// `types`, indexed by name.
    #[must_use]
    pub fn type_map(&self) -> BTreeMap<String, &TypeDef> {
        self.types.iter().map(|t| (t.name.clone(), t)).collect()
    }
}

/// Codama's `camelCase`, which differs from heck on acronyms (`AMMConfig` is `aMMConfig`).
/// Anchor 0.30 names foreign types by path (`whirlpool::state::Position`); legacy IDLs
/// flattened the same path to `WhirlpoolStatePosition`, so `::` splits a word too.
pub(crate) fn camel(name: &str) -> String {
    let mut spaced = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(c);
    }
    let mut out = String::with_capacity(name.len());
    for word in spaced
        .split(|c: char| matches!(c, '-' | '_' | '+' | '.' | ':') || c.is_whitespace())
        .filter(|w| !w.is_empty())
    {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    let mut chars = out.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

fn camel_field(field: &mut Field) {
    field.name = camel(&field.name);
    camel_type(&mut field.ty);
}

fn camel_type(ty: &mut Type) {
    match ty {
        Type::Defined { name, generics } => {
            *name = camel(name);
            for arg in generics {
                if let GenericArg::Type { ty } = arg {
                    camel_type(ty);
                }
            }
        }
        Type::Option(inner)
        | Type::COption(inner)
        | Type::Vec(inner)
        | Type::Array(inner, _)
        | Type::Set(inner)
        | Type::BigEndian(inner)
        | Type::Counted { ty: inner, .. }
        | Type::CodamaOption { item: inner, .. } => camel_type(inner),
        Type::Map(key, value) => {
            camel_type(key);
            camel_type(value);
        }
        Type::Tuple(items) => items.iter_mut().for_each(camel_type),
        Type::Struct(fields) => fields.iter_mut().for_each(camel_field),
        _ => {}
    }
}

fn camel_accounts(items: &mut [AccountItem]) {
    for item in items {
        match item {
            AccountItem::Single(account) => account.name = camel(&account.name),
            AccountItem::Composite { name, accounts } => {
                *name = camel(name);
                camel_accounts(accounts);
            }
        }
    }
}

/// Rewrite pre-0.30 Anchor JSON into the 0.30 spec shape: metadata, key names and inline
/// types. Returns whether the IDL was in the legacy shape.
fn normalize_legacy_anchor(value: &mut serde_json::Value, shank: bool) -> bool {
    let Some(obj) = value.as_object_mut() else {
        return false;
    };

    // 0.30 IDLs keep `name` in `metadata`; legacy ones put it at the top, and may have a
    // `metadata` holding only the address.
    let has_top_level_name = obj.contains_key("name") || obj.contains_key("version");
    let metadata_has_name = obj.get("metadata").and_then(|m| m.get("name")).is_some();
    let legacy = has_top_level_name && !metadata_has_name;
    if legacy {
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let version = obj
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("0.0.0")
            .to_string();
        let address = obj
            .get("metadata")
            .and_then(|m| m.get("address"))
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String(String::new()));
        obj.insert(
            "metadata".into(),
            serde_json::json!({
                "name": name,
                "version": version,
                "spec": if shank { "shank" } else { "legacy-anchor" },
            }),
        );
        obj.entry("address".to_string()).or_insert(address);
        add_legacy_discriminators(obj, shank);
    }
    move_inline_types(obj);
    // A legacy alias moves to `alias` first, so the key walk sees it as a place for a type.
    normalize_type_defs(value);
    rewrite_legacy_keys(value, false);
    legacy
}

/// Shank tags each instruction with `discriminant`, a number of a given width. Hashing the
/// name the Anchor way would give a tag the program does not read.
fn shank_discriminators(value: &mut serde_json::Value) -> Result<(), serde_json::Error> {
    let Some(ixs) = value.get_mut("instructions").and_then(|v| v.as_array_mut()) else {
        return Ok(());
    };
    for ix in ixs {
        let Some(map) = ix.as_object_mut() else {
            continue;
        };
        let name = map.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let tag = map.get("discriminant");
        let width = match tag.and_then(|t| t.get("type")).and_then(|t| t.as_str()) {
            Some("u8") => 1,
            Some("u16") => 2,
            Some("u32") => 4,
            Some("u64") => 8,
            _ => 0,
        };
        let value = tag
            .and_then(|t| t.get("value"))
            .and_then(serde_json::Value::as_u64);
        let Some(value) = value.filter(|v| width > 0 && (width == 8 || v >> (8 * width) == 0))
        else {
            return Err(serde::de::Error::custom(format!(
                "Shank instruction {name}: `discriminant` must be a u8, u16, u32 or u64 that fits its type"
            )));
        };
        let bytes = value.to_le_bytes();
        let disc = serde_json::json!(bytes.get(..width).unwrap_or_default());
        map.insert("discriminator".to_string(), disc);
    }
    Ok(())
}

/// Legacy accounts carry their layout in `type` and legacy events in `fields`; the spec keeps
/// both in `types`.
fn move_inline_types(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let mut extra_types: Vec<serde_json::Value> = Vec::new();

    if let Some(accounts) = obj.get_mut("accounts").and_then(|a| a.as_array_mut()) {
        for acct in accounts.iter_mut() {
            let Some(aobj) = acct.as_object_mut() else {
                continue;
            };
            if let Some(ty) = aobj.remove("type") {
                let name = aobj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("UnknownAccount")
                    .to_string();
                extra_types.push(serde_json::json!({
                    "name": name,
                    "type": ty,
                }));
            }
            aobj.entry("discriminator".to_string())
                .or_insert_with(|| serde_json::json!([]));
        }
    }

    if let Some(events) = obj.get_mut("events").and_then(|a| a.as_array_mut()) {
        for ev in events.iter_mut() {
            let Some(eobj) = ev.as_object_mut() else {
                continue;
            };
            if let Some(fields) = eobj.remove("fields") {
                let name = eobj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("UnknownEvent")
                    .to_string();
                // `index` marks indexed event fields; it is not layout.
                let clean_fields: Vec<serde_json::Value> = fields
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| {
                                let fo = f.as_object()?;
                                Some(serde_json::json!({
                                    "name": fo.get("name")?,
                                    "type": fo.get("type")?,
                                }))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                extra_types.push(serde_json::json!({
                    "name": name,
                    "type": { "kind": "struct", "fields": clean_fields },
                }));
            }
            eobj.entry("discriminator".to_string())
                .or_insert_with(|| serde_json::json!([]));
        }
    }

    if !extra_types.is_empty() {
        let types = obj
            .entry("types".to_string())
            .or_insert_with(|| serde_json::json!([]));
        if let Some(arr) = types.as_array_mut() {
            arr.extend(extra_types);
        }
    }
}

/// Fill in the discriminators legacy IDLs leave out, the way Anchor derives them: the first
/// 8 bytes of `sha256("global:<snake_case name>")`, `"account:<Type>"` or `"event:<Type>"`.
/// Shank accounts and events have no discriminator, so they get none.
fn add_legacy_discriminators(obj: &mut serde_json::Map<String, serde_json::Value>, shank: bool) {
    use heck::ToSnakeCase;
    use sha2::{Digest, Sha256};

    for (list, namespace) in [
        ("instructions", "global"),
        ("accounts", "account"),
        ("events", "event"),
    ] {
        if shank && namespace != "global" {
            continue;
        }
        let Some(items) = obj.get_mut(list).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for item in items {
            let Some(map) = item.as_object_mut() else {
                continue;
            };
            let present = map
                .get("discriminator")
                .and_then(|d| d.as_array())
                .is_some_and(|d| !d.is_empty());
            let Some(name) = map.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            if present {
                continue;
            }
            // Legacy IDLs camelCase instruction names; the hash uses the Rust fn name.
            let name = if namespace == "global" {
                name.to_snake_case()
            } else {
                rust_type_name(name)
            };
            let hash = Sha256::digest(format!("{namespace}:{name}").as_bytes());
            map.insert(
                "discriminator".to_string(),
                serde_json::json!(hash.get(..8).unwrap_or_default()),
            );
        }
    }
}

/// Anchor hashes the Rust type name. Hand-made IDLs such as Pyth's lowercase its first letter
/// (`priceUpdateV2`), or write it in snake case. Shipstern reads an ASCII name the same way.
fn rust_type_name(name: &str) -> String {
    use heck::ToUpperCamelCase;

    if name.contains('_') && !name.bytes().any(|b| b.is_ascii_uppercase()) {
        return name.to_upper_camel_case();
    }
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

/// Two shapes the spec structs don't take: tuple struct fields, which Anchor writes as bare
/// types (`["bool"]` or `[{"array":["u8",4]}]`) and get named `_0`, `_1`, ...; and the
/// pre-0.30 alias `{"kind":"alias","value":T}` for `{"kind":"type","alias":T}`.
fn normalize_type_defs(value: &mut serde_json::Value) {
    let Some(types) = value.get_mut("types").and_then(|t| t.as_array_mut()) else {
        return;
    };
    for td in types {
        let Some(ty) = td.get_mut("type").and_then(|t| t.as_object_mut()) else {
            continue;
        };
        match ty.get("kind").and_then(|k| k.as_str()) {
            Some("alias") => {
                if let Some(target) = ty.remove("value") {
                    ty.insert("kind".into(), "type".into());
                    ty.insert("alias".into(), target);
                }
            }
            Some("struct") => {
                let Some(fields) = ty.get_mut("fields").and_then(|f| f.as_array_mut()) else {
                    continue;
                };
                // A bare type is anything without a `name`; a type object never has one.
                for (i, f) in fields.iter_mut().enumerate() {
                    if f.get("name").is_none() {
                        let bare = std::mem::take(f);
                        *f = serde_json::json!({ "name": format!("_{i}"), "type": bare });
                    }
                }
            }
            _ => {}
        }
    }
}

/// `is_type` is true where a type goes. Only there is `"publicKey"` the legacy pubkey type: a
/// name, a seed or a doc can hold the same text.
fn rewrite_legacy_keys(value: &mut serde_json::Value, is_type: bool) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.remove("isMut") {
                map.entry("writable".to_string()).or_insert(v);
            }
            if let Some(v) = map.remove("isSigner") {
                map.entry("signer".to_string()).or_insert(v);
            }
            if let Some(v) = map.remove("isOptional") {
                map.entry("optional".to_string()).or_insert(v);
            }
            // Shank names the Rust collection; Borsh writes each map and set the same way.
            for (from, to) in [
                ("hashMap", "map"),
                ("bTreeMap", "map"),
                ("hashSet", "set"),
                ("bTreeSet", "set"),
            ] {
                if let Some(v) = map.remove(from) {
                    map.entry(to.to_string()).or_insert(v);
                }
            }
            // Old shorthand: `{"defined": "Foo"}` for `{"defined": {"name": "Foo"}}`.
            if let Some(serde_json::Value::String(name)) = map.get("defined").cloned() {
                map.insert(
                    "defined".into(),
                    serde_json::json!({ "name": name, "generics": [] }),
                );
            }
            for (key, v) in map.iter_mut() {
                let holds_type = matches!(
                    key.as_str(),
                    "type"
                        | "alias"
                        | "fields"
                        | "option"
                        | "coption"
                        | "vec"
                        | "array"
                        | "map"
                        | "set"
                        | "tuple"
                );
                rewrite_legacy_keys(v, holds_type);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                rewrite_legacy_keys(v, is_type);
            }
        }
        serde_json::Value::String(s) if is_type && s == "publicKey" => {
            *s = "pubkey".into();
        }
        _ => {}
    }
}

#[cfg(test)]
mod discriminator_end_tests {
    use super::discriminator_end;

    #[test]
    fn a_chain_ends_and_an_offset_at_the_top_stops() {
        assert_eq!(
            discriminator_end(0, &[1], &[(1, vec![2]), (2, vec![3, 4])]),
            4
        );
        // A gap stops the run: a field can sit in it.
        assert_eq!(discriminator_end(0, &[1], &[(2, vec![2])]), 1);
        // A hostile IDL: the end saturates at usize::MAX and a discriminator sits there.
        let top = usize::MAX;
        assert_eq!(discriminator_end(top - 1, &[1], &[(top, vec![2])]), top);
    }
}
