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
    #[serde(default)]
    pub accounts: Vec<AccountItem>,
    #[serde(default)]
    pub args: Vec<Field>,
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Account {
    pub name: String,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub signer: bool,
    #[serde(default)]
    pub optional: bool,
    /// Fixed address (a program or sysvar). Not part of the positional diff.
    #[serde(default)]
    pub address: Option<String>,
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
    /// Declared account size in bytes, discriminator included (Codama `size`).
    #[serde(default)]
    pub size: Option<usize>,
    /// Explicit body start; `None` means right after the discriminator.
    #[serde(default)]
    pub payload_offset: Option<usize>,
}

impl NamedDisc {
    /// Where the account body starts: after the discriminator, unless the IDL says the
    /// discriminator byte is also the body's first field (SPL Governance).
    #[must_use]
    pub fn payload_start(&self) -> usize {
        self.payload_offset.unwrap_or_else(|| {
            self.discriminator_offset
                .saturating_add(self.discriminator.len())
        })
    }
}

/// A program event. The envelope is the self-CPI wrap around the payload: Anchor's 8-byte
/// event tag or Pinocchio's 1-byte one. Empty envelope fields mean the IDL doesn't say.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Event {
    pub name: String,
    /// Content discriminator, such as Anchor's event sighash.
    #[serde(default)]
    pub discriminator: Discriminator,
    /// Shipstern's `cpi_event_discriminator`.
    #[serde(default)]
    pub envelope_discriminator: Discriminator,
    /// Shipstern's `cpi_event_payload_offset`: where payload decoding starts.
    #[serde(default)]
    pub envelope_payload_offset: Option<usize>,
}

impl Event {
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

fn default_serialization() -> String {
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
            crate::codama_convert::fix_string_error_codes(&mut value);
            let root: codama_nodes::RootNode = serde_json::from_value(value)?;
            Ok(crate::codama_convert::from_codama(root))
        } else {
            let legacy = normalize_legacy_anchor(&mut value);
            let mut idl: Self = serde_json::from_value(value)?;
            if legacy {
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

/// Rewrite pre-0.30 Anchor JSON into the 0.30 spec shape: `name` and `version` into
/// `metadata`, old key names, and inline account and event types moved into `types`.
/// Codama's `camelCase`, which differs from heck on acronyms (`AMMConfig` is `aMMConfig`).
/// Anchor 0.30 names foreign types by path (`whirlpool::state::Position`); legacy IDLs
/// flattened the same path to `WhirlpoolStatePosition`, so `::` splits a word too.
fn camel(name: &str) -> String {
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
        | Type::Set(inner) => camel_type(inner),
        Type::Map(key, value) => {
            camel_type(key);
            camel_type(value);
        }
        Type::Tuple(items) => items.iter_mut().for_each(camel_type),
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

/// Returns whether the IDL was in the legacy shape.
fn normalize_legacy_anchor(value: &mut serde_json::Value) -> bool {
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
                "spec": "legacy-anchor",
            }),
        );
        obj.entry("address".to_string()).or_insert(address);
        add_legacy_discriminators(obj);
    }

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

    rewrite_legacy_keys(value);
    normalize_type_defs(value);
    legacy
}

/// Fill in the discriminators legacy IDLs leave out, the way Anchor derives them: the first
/// 8 bytes of `sha256("global:<snake_case name>")`, `"account:<Name>"` or `"event:<Name>"`.
fn add_legacy_discriminators(obj: &mut serde_json::Map<String, serde_json::Value>) {
    use heck::ToSnakeCase;
    use sha2::{Digest, Sha256};

    for (list, namespace) in [
        ("instructions", "global"),
        ("accounts", "account"),
        ("events", "event"),
    ] {
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
                name.to_string()
            };
            let hash = Sha256::digest(format!("{namespace}:{name}").as_bytes());
            map.insert(
                "discriminator".to_string(),
                serde_json::json!(hash.get(..8).unwrap_or_default()),
            );
        }
    }
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

fn rewrite_legacy_keys(value: &mut serde_json::Value) {
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
            // Old shorthand: `{"defined": "Foo"}` for `{"defined": {"name": "Foo"}}`.
            if let Some(serde_json::Value::String(name)) = map.get("defined").cloned() {
                map.insert(
                    "defined".into(),
                    serde_json::json!({ "name": name, "generics": [] }),
                );
            }
            for v in map.values_mut() {
                rewrite_legacy_keys(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                rewrite_legacy_keys(v);
            }
        }
        serde_json::Value::String(s) if s == "publicKey" => {
            *s = "pubkey".into();
        }
        _ => {}
    }
}
