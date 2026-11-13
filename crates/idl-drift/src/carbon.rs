//! What carbon-cli changes or drops when it generates a Carbon decoder from an IDL.
//!
//! Each rule follows the TypeScript of carbon-cli 2.0.0 (Carbon commit a64db3c), in
//! `packages/cli` and `packages/renderer`, and of `@codama/nodes-from-anchor` 1.2.1, which its
//! Anchor path calls. No rule is checked against carbon-cli output.

use std::collections::{BTreeMap, BTreeSet};

use codama_nodes::{
    ConstantValueNode, CountNode, DefinedTypeNode, DiscriminatorNode, Endianness,
    EnumVariantTypeNode, EventNode, InstructionInputValueNode, NestedTypeNode, Number,
    NumberFormat, NumberTypeNode, OptionalAccountStrategy, RootNode, StructFieldTypeNode, TypeNode,
    ValueNode,
};
use serde_json::Value;

use crate::codama_convert::{decode_bytes, decode_encoded_bytes, fix_string_error_codes};
use crate::layout::{Reading, TypeIndex};
use crate::model::{rust_type_name, AccountItem, EnumFields, Format, Idl, Type, TypeDefTy};
use crate::shipstern::snake_case;

/// How a finding changes the generated decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// The decoder misreads bytes or drops data, or carbon-cli cannot generate it.
    Fail,
    /// The generated code is different, but it decodes the same values.
    Note,
}

/// One place where carbon-cli does not follow the IDL.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub level: Level,
    /// Such as `Pool.fee`, `ix:swap.amount`, `account Pool` or `program`.
    pub item: String,
    pub reason: String,
}

/// Each place where carbon-cli 2.0.0 does not follow this IDL. Codama JSON needs
/// `--standard codama`; any other IDL goes through carbon-cli's Anchor path.
///
/// # Errors
///
/// When `json` is not JSON, or a Codama root that does not parse.
pub fn findings(json: &str, idl: &Idl) -> Result<Vec<Finding>, serde_json::Error> {
    let raw: Value = serde_json::from_str(json)?;
    let mut out = Vec::new();
    program_id(&raw, &mut out);
    if idl.format == Format::Codama {
        let mut value = raw;
        fix_string_error_codes(&mut value);
        let root: RootNode = serde_json::from_value(value)?;
        Codama::new(&root, &mut out).run();
    } else {
        anchor(&raw, idl, &mut out);
    }
    let mut seen = BTreeSet::new();
    out.retain(|f| seen.insert(f.clone()));
    Ok(out)
}

fn push(out: &mut Vec<Finding>, level: Level, item: &str, reason: impl Into<String>) {
    out.push(Finding {
        level,
        item: item.to_string(),
        reason: reason.into(),
    });
}

/// cli/src/lib/decoder.ts:94-100 and :308-312: without a base58 `address`, generation stops.
fn program_id(raw: &Value, out: &mut Vec<Finding>) {
    let address = [raw.get("address"), raw.pointer("/metadata/address")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|a| !a.is_empty());
    let base58 = |a: &str| {
        (32..=44).contains(&a.len())
            && a.bytes()
                .all(|b| b.is_ascii_alphanumeric() && !matches!(b, b'0' | b'O' | b'I' | b'l'))
    };
    if !address.is_some_and(base58) {
        let reason = "no base58 program id in `address` or `metadata.address`, so carbon-cli stops unless --program-id is given";
        push(out, Level::Note, "program", reason);
    }
}

/// The rules for carbon-cli's Anchor path, read from the flat model.
fn anchor(raw: &Value, idl: &Idl, out: &mut Vec<Finding>) {
    // cli/src/lib/idl-transformer.ts:18 hashes `event:<name>` as written; Anchor hashes the Rust type.
    let events = raw.get("events").and_then(Value::as_array);
    let legacy = events.is_some_and(|e| {
        e.iter()
            .any(|e| e.get("fields").is_some_and(Value::is_array))
    });
    for name in events.into_iter().flatten().filter(|_| legacy) {
        let Some(name) = name.get("name").and_then(Value::as_str) else {
            continue;
        };
        let rust = rust_type_name(name);
        if rust != name {
            let reason = format!("carbon-cli hashes `event:{name}`, but Anchor hashes `event:{rust}`, so no event matches");
            push(out, Level::Fail, &format!("event {name}"), reason);
        }
    }
    unreadable_types(raw, out);
    // nodes-from-anchor v00/AccountNode.ts gives the accounts of a Shank IDL no discriminator.
    if raw.pointer("/metadata/origin").and_then(Value::as_str) == Some("shank") {
        let level = if idl.accounts.len() > 1 {
            Level::Fail
        } else {
            Level::Note
        };
        for account in &idl.accounts {
            let reason = "nodes-from-anchor gives a Shank account no discriminator, so the decoder accepts any data that Borsh can read";
            push(out, level, &format!("account {}", account.name), reason);
        }
    }
    for td in &idl.types {
        match &td.ty {
            TypeDefTy::Struct { fields } => {
                for f in fields {
                    coption(&format!("{}.{}", td.name, f.name.as_str()), &f.ty, out);
                }
            }
            TypeDefTy::Enum { variants, .. } => {
                for v in variants {
                    let path = format!("{}.{}", td.name, v.name);
                    match &v.fields {
                        EnumFields::Named(fields) => {
                            for f in fields {
                                coption(&format!("{path}.{}", f.name.as_str()), &f.ty, out);
                            }
                        }
                        EnumFields::Tuple(types) => {
                            for ty in types {
                                coption(&path, ty, out);
                            }
                        }
                    }
                }
            }
            TypeDefTy::Type { alias } => coption(&td.name, alias, out),
        }
    }
    for ix in &idl.instructions {
        for arg in &ix.args {
            coption(
                &format!("ix:{}.{}", ix.name, arg.name.as_str()),
                &arg.ty,
                out,
            );
        }
        let accounts = anchor_accounts(&ix.accounts);
        let names = accounts.iter().map(|(name, _)| name.clone()).collect();
        duplicate_accounts(&format!("ix:{}", ix.name.as_str()), names, out);
    }
    zero_copy_padding(idl, out);
    let key = |disc: &[u8]| Key::Constants(vec![(0, disc.to_vec())]);
    let accounts = idl
        .accounts
        .iter()
        .map(|a| (a.name.clone(), key(&a.discriminator), 0, 0));
    same_check("account", &accounts.collect::<Vec<_>>(), out);
    // nodes-from-anchor v01/InstructionNode.ts sets `optionalAccountStrategy: programId`, so
    // an instruction always sends each account slot.
    let instructions = idl.instructions.iter().map(|ix| {
        let accounts = anchor_accounts(&ix.accounts);
        let sends = accounts.len();
        let needs = needed_accounts(accounts);
        (ix.name.clone(), key(&ix.discriminator), needs, sends)
    });
    same_check("instruction", &by_name(instructions.collect()), out);
    let events = idl
        .events
        .iter()
        .map(|e| (e.name.clone(), key(&e.discriminator), 0, 0));
    same_check("event", &events.collect::<Vec<_>>(), out);
}

/// nodes-from-anchor v01/InstructionAccountNode.ts:40-45 flattens account groups with no
/// prefix. Each account comes with `optional`.
fn anchor_accounts(items: &[AccountItem]) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for item in items {
        match item {
            AccountItem::Single(account) => out.push((account.name.clone(), account.optional)),
            AccountItem::Composite { accounts, .. } => out.extend(anchor_accounts(accounts)),
        }
    }
    out
}

/// renderer/templates/instructionsPage.njk:64 takes each required account with `?`, so a
/// decode needs the accounts up to the last required one, after the `snake_case` dedup.
fn needed_accounts(accounts: Vec<(String, bool)>) -> usize {
    let mut seen = BTreeSet::new();
    let unique: Vec<bool> = accounts
        .into_iter()
        .filter(|(name, _)| seen.insert(snake_case(name)))
        .map(|(_, optional)| optional)
        .collect();
    unique
        .iter()
        .rposition(|optional| !optional)
        .map_or(0, |i| i + 1)
}

/// What nodes-from-anchor 1.2.1 cannot turn into a decoder that compiles.
#[derive(Default)]
struct Unreadable {
    /// `{"generic": ...}` as a type: v01/typeNodes/TypeNode.ts:47-113 has no generics, so it throws.
    generic: bool,
    /// An array length that is not a number: v01/typeNodes/ArrayTypeNode.ts:9 keeps it, and
    /// renderer/src/getTypeManifestVisitor.ts:56-63 writes `[T; <length>]`, which is not Rust.
    length: bool,
}

impl Unreadable {
    fn scan(&mut self, ty: &Value) {
        let Some(obj) = ty.as_object() else {
            return;
        };
        self.generic |= obj.contains_key("generic");
        for key in ["vec", "option", "coption", "value", "alias"] {
            obj.get(key).into_iter().for_each(|inner| self.scan(inner));
        }
        if let Some(pair) = obj.get("array").and_then(Value::as_array) {
            pair.first().into_iter().for_each(|item| self.scan(item));
            self.length |= pair.get(1).is_some_and(|len| !len.is_u64());
        }
        let fields = ["fields", "variants"]
            .into_iter()
            .filter_map(|k| obj.get(k));
        for field in fields.filter_map(Value::as_array).flatten() {
            self.scan(field.get("type").unwrap_or(field));
            let variant_fields = field.get("fields").and_then(Value::as_array);
            for f in variant_fields.into_iter().flatten() {
                self.scan(f.get("type").unwrap_or(f));
            }
        }
    }

    /// The reason for one item, or `None` when nodes-from-anchor reads it.
    fn reason(ty: &Value) -> Option<&'static str> {
        let mut found = Self::default();
        found.scan(ty);
        if found.generic {
            Some("a `{\"generic\": ...}` type, which nodes-from-anchor 1.2.1 cannot read, so carbon-cli stops")
        } else if found.length {
            Some("an array length that is not a number; carbon-cli writes it into the Rust array type, which does not compile")
        } else {
            None
        }
    }
}

/// The spec 0.1.0 types and arguments that carbon-cli stops on or cannot compile. An alias of
/// kind `type` throws too: v01/typeNodes/TypeNode.ts:86 reads only `{"kind": "alias"}`.
fn unreadable_types(raw: &Value, out: &mut Vec<Finding>) {
    if raw.pointer("/metadata/spec").and_then(Value::as_str) != Some("0.1.0") {
        return;
    }
    let types = raw
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for def in types {
        let name = def.get("name").and_then(Value::as_str).unwrap_or_default();
        let body = def.get("type").unwrap_or(&Value::Null);
        let reason = if body.get("kind").and_then(Value::as_str) == Some("type") {
            "an alias of kind `type`, which nodes-from-anchor 1.2.1 cannot read, so carbon-cli stops"
        } else if let Some(reason) = Unreadable::reason(body) {
            reason
        } else {
            continue;
        };
        push(out, Level::Fail, &format!("type {name}"), reason);
    }
    let ixs = raw
        .get("instructions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for ix in ixs {
        let name = ix.get("name").and_then(Value::as_str).unwrap_or_default();
        for arg in ix
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reason) = arg.get("type").and_then(Unreadable::reason) {
                let arg_name = arg.get("name").and_then(Value::as_str).unwrap_or_default();
                push(out, Level::Fail, &format!("ix:{name}.{arg_name}"), reason);
            }
        }
    }
}

/// renderer/src/getTypeManifestVisitor.ts:277: an option is `Option<T>`, a u8 tag and no bytes
/// when absent, whatever its prefix and `fixed`.
fn coption(path: &str, ty: &Type, out: &mut Vec<Finding>) {
    if matches!(ty, Type::COption(_)) {
        let reason = "a COption (u32 tag, fixed size); carbon-cli reads a u8 tag, and no bytes when the value is absent";
        push(out, Level::Fail, path, reason);
    }
    for child in ty.children() {
        coption(path, child, out);
    }
}

/// nodes-from-anchor v01/AccountNode.ts:18-41 keeps no `serialization` or `repr` (carbon-cli
/// calls it at decoder.ts:343), and renderer/templates/accountsPage.njk:94 reads with Borsh.
fn zero_copy_padding(idl: &Idl, out: &mut Vec<Finding>) {
    let index = TypeIndex::new(idl);
    for account in &idl.accounts {
        let Some(td) = index
            .get(account.name.as_str())
            .filter(|t| t.is_zero_copy())
        else {
            continue;
        };
        let (Ok(memory), Ok(borsh)) = (
            index.leaves(td, Reading::ZeroCopy),
            index.leaves(td, Reading::Borsh),
        ) else {
            continue;
        };
        let moved = memory
            .iter()
            .zip(&borsh)
            .find(|(m, b)| m.offset != b.offset)
            .map(|(m, _)| m.path.as_str());
        if let Some(field) = moved {
            let reason = format!("a zero-copy account with repr(C) padding before `{field}`; carbon-cli reads it as Borsh, so `{field}` and each field after it are misread");
            push(
                out,
                Level::Fail,
                &format!("account {}", account.name.as_str()),
                reason,
            );
        }
    }
}

/// renderer/src/getRenderMapVisitor.ts:583-589 keeps the first account of each `snake_case` name.
fn duplicate_accounts(item: &str, names: Vec<String>, out: &mut Vec<Finding>) {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for name in names {
        let snake = snake_case(&name);
        if let Some(first) = seen.get(&snake) {
            let reason = format!("accounts `{first}` and `{name}` have one snake_case name, `{snake}`; carbon-cli drops `{name}`, so the decoder does not show it");
            push(out, Level::Fail, item, reason);
        } else {
            seen.insert(snake, name);
        }
    }
}

/// What a generated decoder compares before Borsh reads the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    /// Bytes at offsets, from constant discriminators.
    Constants(Vec<(u64, Vec<u8>)>),
    /// The exact data length.
    Size(u64),
    /// No comparison: Borsh reads any data that is long enough.
    Nothing,
}

impl Key {
    /// `true` when data that passes `later` also passes `self`.
    fn accepts(&self, later: &Key) -> bool {
        match (self, later) {
            (Key::Constants(first), Key::Constants(second)) => first.iter().all(|(at, bytes)| {
                !bytes.is_empty()
                    && second
                        .iter()
                        .any(|(other_at, other)| other_at == at && other.starts_with(bytes))
            }),
            (Key::Size(a), Key::Size(b)) => a == b,
            _ => false,
        }
    }
}

/// An item's name, its check, the accounts that its decode needs, and the accounts that its
/// instructions send (both 0 for an account or event).
type Checked = (String, Key, usize, usize);

/// renderer/templates/instructionsMod.njk:71 tries instructions sorted by name, without case.
fn by_name(mut items: Vec<Checked>) -> Vec<Checked> {
    items.sort_by_key(|(name, _, _, _)| name.to_lowercase());
    items
}

/// renderer/templates/accountsMod.njk:83 and crates/macros/src/try_decode_ixs.rs:14-15 return
/// the first decode that passes, so a later item with the same check and no fewer accounts is
/// decoded as the earlier one.
fn same_check(kind: &str, items: &[Checked], out: &mut Vec<Finding>) {
    for (i, (name, key, _, sends)) in items.iter().enumerate() {
        if let Some((first, _, _, _)) = items
            .get(..i)
            .unwrap_or_default()
            .iter()
            .find(|(_, k, needs, _)| k.accepts(key) && needs <= sends)
        {
            let reason = format!("carbon-cli tries `{first}` first with the same check, so it can decode this {kind} as `{first}`");
            push(out, Level::Fail, &format!("{kind} {name}"), reason);
        }
    }
}

/// A discriminator as the renderer sees it, after it turns field discriminators with a
/// default value into constants (getRenderMapVisitor.ts:176-196 and :552-572).
enum Disc {
    Constant {
        offset: u64,
        bytes: Result<Vec<u8>, String>,
    },
    Field(String),
    Size(u64),
}

impl Disc {
    fn text(&self) -> String {
        match self {
            Disc::Constant { offset, .. } => format!("the constant at offset {offset}"),
            Disc::Field(name) => format!("the field `{name}`"),
            Disc::Size(size) => format!("the size {size}"),
        }
    }
}

/// What `getDiscriminatorManifest` (getTypeManifestVisitor.ts:442-567) makes of a list.
struct Manifest {
    key: Key,
    /// Bytes that the decoder skips before Borsh reads the fields.
    skip: u64,
    /// A field discriminator gets a comment and no comparison (:550-555).
    field_only: bool,
    ignored: Vec<String>,
    /// Why `getDiscriminatorBytes` throws, which stops generation.
    error: Option<String>,
}

fn manifest(discs: &[Disc]) -> Manifest {
    let constants: Vec<(u64, &Result<Vec<u8>, String>)> = discs
        .iter()
        .filter_map(|d| match d {
            Disc::Constant { offset, bytes } => Some((*offset, bytes)),
            _ => None,
        })
        .collect();
    let mut m = Manifest {
        key: Key::Nothing,
        skip: 0,
        field_only: false,
        ignored: Vec::new(),
        error: None,
    };
    // :450-528: with two or more constants, all are compared and the rest is not.
    if constants.len() > 1 {
        let mut sorted = constants;
        sorted.sort_by_key(|(offset, _)| *offset);
        let mut checks = Vec::new();
        for (offset, bytes) in &sorted {
            match bytes {
                Ok(bytes) => checks.push((*offset, bytes.clone())),
                Err(why) => m.error = Some(why.clone()),
            }
        }
        m.skip = checks.first().map_or(0, |(_, b)| b.len() as u64);
        m.key = Key::Constants(checks);
        m.ignored = discs
            .iter()
            .filter(|d| !matches!(d, Disc::Constant { .. }))
            .map(Disc::text)
            .collect();
        return m;
    }
    // :530-566: otherwise only the first one counts.
    let Some(first) = discs.first() else {
        return m;
    };
    match first {
        Disc::Constant { offset, bytes } => match bytes {
            Ok(bytes) => {
                m.skip = bytes.len() as u64;
                m.key = Key::Constants(vec![(*offset, bytes.clone())]);
            }
            Err(why) => m.error = Some(why.clone()),
        },
        Disc::Field(_) => m.field_only = true,
        Disc::Size(size) => m.key = Key::Size(*size),
    }
    m.ignored = discs.iter().skip(1).map(Disc::text).collect();
    m
}

/// renderer/src/utils/discriminatorHelpers.ts:4-17: bytes, a u8 to u64 number or a string.
fn constant_bytes(ty: &TypeNode, value: &ValueNode) -> Result<Vec<u8>, String> {
    match value {
        ValueNode::Bytes(bytes) => Ok(decode_bytes(bytes)),
        ValueNode::Number(n) => number_bytes(ty, &n.number),
        ValueNode::String(s) => match ty {
            TypeNode::String(t) => {
                let encoding = serde_json::to_value(t.encoding).unwrap_or_default();
                Ok(decode_encoded_bytes(
                    encoding.as_str().unwrap_or_default(),
                    &s.string,
                ))
            }
            _ => Err("a string value whose type is not a string".to_string()),
        },
        other => Err(format!("a {} value", node_kind(other))),
    }
}

/// The same for an instruction argument's default value.
fn input_bytes(ty: &TypeNode, value: &InstructionInputValueNode) -> Result<Vec<u8>, String> {
    match value {
        InstructionInputValueNode::BytesValue(b) => {
            constant_bytes(ty, &ValueNode::Bytes(b.clone()))
        }
        InstructionInputValueNode::NumberValue(n) => number_bytes(ty, &n.number),
        InstructionInputValueNode::StringValue(s) => {
            constant_bytes(ty, &ValueNode::String(s.clone()))
        }
        other => Err(format!("a {} value", node_kind(other))),
    }
}

/// discriminatorHelpers.ts:32-59: only u8, u16, u32 and u64, in the type's byte order.
fn number_bytes(ty: &TypeNode, number: &Number) -> Result<Vec<u8>, String> {
    let TypeNode::Number(n) = ty else {
        return Err("a number value whose type is not a number".to_string());
    };
    let width = match n.format {
        NumberFormat::U8 => 1,
        NumberFormat::U16 => 2,
        NumberFormat::U32 => 4,
        NumberFormat::U64 => 8,
        other => return Err(format!("a {} number", format_name(other))),
    };
    // JavaScript masks and divides the number, which gives the two's complement bytes.
    let le = match number {
        Number::UnsignedInteger(u) => u.to_le_bytes(),
        Number::SignedInteger(i) => i.to_le_bytes(),
        // :36-52 masks a float as a whole number and does not throw.
        Number::Float(f) => truncate(*f).to_le_bytes(),
    };
    let mut bytes = le.get(..width).unwrap_or_default().to_vec();
    if n.endian == Endianness::Be {
        bytes.reverse();
    }
    Ok(bytes)
}

#[allow(clippy::cast_possible_truncation)]
fn truncate(f: f64) -> i64 {
    f.trunc() as i64
}

fn node_kind(node: &impl serde::Serialize) -> String {
    let value = serde_json::to_value(node).unwrap_or_default();
    value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("other")
        .to_string()
}

fn format_name(format: NumberFormat) -> &'static str {
    match format {
        NumberFormat::U8 => "u8",
        NumberFormat::U16 => "u16",
        NumberFormat::U32 => "u32",
        NumberFormat::U64 => "u64",
        NumberFormat::U128 => "u128",
        NumberFormat::I8 => "i8",
        NumberFormat::I16 => "i16",
        NumberFormat::I32 => "i32",
        NumberFormat::I64 => "i64",
        NumberFormat::I128 => "i128",
        NumberFormat::F32 => "f32",
        NumberFormat::F64 => "f64",
        NumberFormat::ShortU16 => "shortU16",
    }
}

/// `prefix` as text, such as `u16` or `big-endian u32`.
fn number_text(prefix: &NestedTypeNode<NumberTypeNode>) -> String {
    match prefix {
        NestedTypeNode::Value(n) if n.endian == Endianness::Be && !is_byte(n) => {
            format!("big-endian {}", format_name(n.format))
        }
        NestedTypeNode::Value(n) => format_name(n.format).to_string(),
        _ => "wrapped number".to_string(),
    }
}

fn is_byte(n: &NumberTypeNode) -> bool {
    matches!(n.format, NumberFormat::U8 | NumberFormat::I8)
}

/// Borsh writes each length and count as a little-endian u32.
fn is_u32_le(prefix: &NestedTypeNode<NumberTypeNode>) -> bool {
    matches!(prefix, NestedTypeNode::Value(n) if n.format == NumberFormat::U32 && n.endian == Endianness::Le)
}

fn is_u8(prefix: &NestedTypeNode<NumberTypeNode>) -> bool {
    matches!(prefix, NestedTypeNode::Value(n) if n.format == NumberFormat::U8)
}

/// The rules for carbon-cli's Codama path (`--standard codama`), read from the Codama nodes.
struct Codama<'r, 'o> {
    root: &'r RootNode,
    defined: BTreeMap<&'r str, &'r DefinedTypeNode>,
    /// The fixed size of each defined type, computed once.
    sizes: BTreeMap<&'r str, Option<u64>>,
    /// Nodes that `fixed_size` may still visit, so a hostile IDL cannot take long.
    visits: usize,
    /// `fixed_size` stopped on its depth or visit limit, so `None` means "not known".
    gave_up: bool,
    out: &'o mut Vec<Finding>,
}

impl<'r, 'o> Codama<'r, 'o> {
    fn new(root: &'r RootNode, out: &'o mut Vec<Finding>) -> Self {
        let defined = root
            .program
            .defined_types
            .iter()
            .map(|t| (t.name.as_str(), t))
            .collect();
        Self {
            root,
            defined,
            sizes: BTreeMap::new(),
            visits: 1 << 16,
            gave_up: false,
            out,
        }
    }

    fn fail(&mut self, item: &str, reason: impl Into<String>) {
        push(self.out, Level::Fail, item, reason);
    }

    fn run(&mut self) {
        let program = &self.root.program;
        for dt in &program.defined_types {
            let name = dt.name.as_str();
            // getRenderMapVisitor.ts:359-368: a one-bool tuple reads a missing byte as false.
            if let TypeNode::Tuple(t) = dt.r#type.as_ref() {
                if t.items.len() == 1 && matches!(t.items.first(), Some(TypeNode::Boolean(_))) {
                    let reason = "a tuple of one bool; carbon-cli reads it as false when the byte is missing";
                    push(self.out, Level::Note, name, reason);
                }
            }
            match dt.r#type.as_ref() {
                TypeNode::Enum(e) => self.enum_rules(name, e),
                ty => self.walk(name, ty, 0),
            }
        }
        self.accounts();
        self.instructions();
        self.events();
    }

    fn accounts(&mut self) {
        let program = &self.root.program;
        let mut accounts = Vec::new();
        for account in &program.accounts {
            let fields: &[StructFieldTypeNode] = match &account.data {
                NestedTypeNode::Value(s) => &s.fields,
                _ => &[],
            };
            // getRenderMapVisitor.ts:162-174: a field named `discriminator` is not rendered.
            let removed: Vec<&StructFieldTypeNode> = fields
                .iter()
                .filter(|f| f.name.as_str() == "discriminator")
                .collect();
            for f in fields.iter().filter(|f| f.name.as_str() != "discriminator") {
                self.field(account.name.as_str(), f, false);
            }
            let discs = account
                .discriminators
                .iter()
                .map(|d| match d {
                    DiscriminatorNode::Field(f) if f.name.as_str() == "discriminator" => {
                        match removed
                            .first()
                            .and_then(|r| r.default_value.as_ref().as_ref().map(|v| (r, v)))
                        {
                            Some((r, v)) => Disc::Constant {
                                offset: f.offset,
                                bytes: constant_bytes(&r.r#type, v),
                            },
                            None => Disc::Field(f.name.to_string()),
                        }
                    }
                    other => disc(other),
                })
                .collect::<Vec<_>>();
            let leading = fields
                .first()
                .is_some_and(|f| f.name.as_str() == "discriminator")
                || removed.is_empty();
            let removed_types: Vec<&TypeNode> = removed.iter().map(|f| f.r#type.as_ref()).collect();
            let key = self.discriminators(
                &format!("account {}", account.name.as_str()),
                &discs,
                &removed_types,
                (leading, fields.len() > removed.len()),
                program.accounts.len(),
            );
            accounts.push((account.name.to_string(), key, 0, 0));
        }
        same_check("account", &accounts, self.out);
    }

    fn instructions(&mut self) {
        let program = &self.root.program;
        let mut instructions = Vec::new();
        for ix in &program.instructions {
            let field_names: BTreeSet<&str> = ix
                .discriminators
                .iter()
                .filter_map(|d| match d {
                    DiscriminatorNode::Field(f) => Some(f.name.as_str()),
                    _ => None,
                })
                .collect();
            // getRenderMapVisitor.ts:527-529: each argument that a field discriminator names is not rendered.
            let (removed, kept): (Vec<_>, Vec<_>) = ix
                .arguments
                .iter()
                .partition(|a| field_names.contains(a.name.as_str()));
            let path = format!("ix:{}", ix.name.as_str());
            for arg in &kept {
                let arg_path = format!("{path}.{}", arg.name.as_str());
                // renderer/templates/instructionsPage.njk:19 writes the type after the name.
                match arg.r#type.as_ref() {
                    TypeNode::Struct(_) => {
                        let reason = "an inline struct as an argument; carbon-cli writes Rust that does not compile";
                        self.fail(&arg_path, reason);
                        self.walk(&arg_path, &arg.r#type, 0);
                    }
                    // extractStructArrayItems.ts:25-38 names the item of an array argument.
                    TypeNode::Array(a) if matches!(a.item.as_ref(), TypeNode::Struct(_)) => {
                        if !matches!(a.count.as_ref(), CountNode::Fixed(_)) {
                            self.count(&arg_path, &a.count);
                        }
                        self.walk(&arg_path, &a.item, 1);
                    }
                    ty => self.walk(&arg_path, ty, 0),
                }
            }
            let discs = ix
                .discriminators
                .iter()
                .map(|d| match d {
                    DiscriminatorNode::Field(f) => {
                        let arg = removed.iter().find(|a| a.name == f.name);
                        match arg.and_then(|a| a.default_value.as_ref().as_ref().map(|v| (a, v))) {
                            Some((a, v)) => Disc::Constant {
                                offset: f.offset,
                                bytes: input_bytes(&a.r#type, v),
                            },
                            None => Disc::Field(f.name.to_string()),
                        }
                    }
                    other => disc(other),
                })
                .collect::<Vec<_>>();
            let first_kept = ix
                .arguments
                .iter()
                .position(|a| !field_names.contains(a.name.as_str()))
                .unwrap_or(ix.arguments.len());
            let leading = ix
                .arguments
                .iter()
                .enumerate()
                .all(|(i, a)| !field_names.contains(a.name.as_str()) || i < first_kept);
            let removed_types: Vec<&TypeNode> = removed.iter().map(|a| a.r#type.as_ref()).collect();
            let key = self.discriminators(
                &format!("instruction {}", ix.name.as_str()),
                &discs,
                &removed_types,
                (leading, !kept.is_empty()),
                program.instructions.len(),
            );
            let accounts: Vec<(String, bool)> = ix
                .accounts
                .iter()
                .map(|a| (a.name.to_string(), a.is_optional.unwrap_or(false)))
                .collect();
            let names = accounts.iter().map(|(name, _)| name.clone()).collect();
            // With `omitted`, an absent optional account sends no slot.
            let sends = if ix.optional_account_strategy == Some(OptionalAccountStrategy::Omitted) {
                accounts.iter().filter(|(_, optional)| !optional).count()
            } else {
                accounts.len()
            };
            let needs = needed_accounts(accounts);
            instructions.push((ix.name.to_string(), key, needs, sends));
            duplicate_accounts(&path, names, self.out);
        }
        same_check("instruction", &by_name(instructions), self.out);
    }

    /// The decoder skips `skip` bytes and then reads the rendered fields from there
    /// (accountsPage.njk:90-94, instructionsPage.njk:46-50). `(leading, rest)`: the removed
    /// fields come first, and other fields are left to read.
    fn discriminators(
        &mut self,
        item: &str,
        discs: &[Disc],
        removed: &[&'r TypeNode],
        (leading, rest): (bool, bool),
        siblings: usize,
    ) -> Key {
        let m = manifest(discs);
        if let Some(why) = &m.error {
            let reason = format!(
                "carbon-cli cannot turn a discriminator into bytes ({why}), so generation stops"
            );
            self.fail(item, reason);
        }
        let mut removed_size = Ok(Some(0u64));
        for ty in removed {
            let size = self.known_size(ty);
            removed_size = removed_size.and_then(|total| {
                size.map(|size| total.zip(size).map(|(a, b)| a.saturating_add(b)))
            });
        }
        // With no field left to read, the skip does not matter; an unknown size gives no line.
        let misread = rest
            && (!leading
                || match removed_size {
                    Ok(Some(size)) => size != m.skip,
                    Ok(None) => m.skip == 0,
                    Err(()) => false,
                });
        if misread {
            let reason = format!("carbon-cli skips {} bytes before the fields, but the fields it reads do not start there, so they are misread", m.skip);
            self.fail(item, reason);
        }
        if m.key == Key::Nothing && m.error.is_none() {
            let why = if m.field_only {
                "carbon-cli only writes a comment for a field discriminator"
            } else {
                "it has no discriminator"
            };
            // With one item of the kind, no other item can match the same data.
            let level = if siblings > 1 {
                Level::Fail
            } else {
                Level::Note
            };
            let reason = format!("{why}, so the decoder accepts any data that Borsh can read");
            push(self.out, level, item, reason);
        }
        if !m.ignored.is_empty() {
            let reason = format!("carbon-cli does not check {}", m.ignored.join(", "));
            push(self.out, Level::Note, item, reason);
        }
        m.key
    }

    /// renderer/src/eventNodes.ts:23-111: one event that the renderer cannot frame, or two that
    /// conflict, makes it skip every event.
    fn events(&mut self) {
        let events = &self.root.program.events;
        if events.is_empty() {
            return;
        }
        let mut cpi: Option<Vec<u8>> = None;
        for event in events {
            let item = format!("event {}", event.name.as_str());
            let Some((envelope, _)) = event_shape(event) else {
                // eventNodes.ts:37-41.
                self.fail(
                    &item,
                    "not a supported event-CPI shape, so carbon-cli skips every event",
                );
                return;
            };
            match (&cpi, envelope) {
                (Some(first), Some(this)) if *first != this => {
                    // eventNodes.ts:45-53.
                    self.fail(&item, "a different event-CPI discriminator than the other events, so carbon-cli skips every event");
                    return;
                }
                (None, Some(this)) => cpi = Some(this),
                _ => {}
            }
        }
        let mut names = BTreeSet::new();
        for event in events {
            if !names.insert(event.name.as_str()) {
                // eventNodes.ts:62-69.
                self.fail(
                    &format!("event {}", event.name.as_str()),
                    "a second event of this name, so carbon-cli skips every event",
                );
                return;
            }
        }
        let mut payloads = Vec::new();
        for event in events {
            let payload = match event.data.as_ref() {
                TypeNode::HiddenPrefix(h) => h.r#type.as_ref(),
                other => other,
            };
            match self.defined.get(event.name.as_str()) {
                // eventNodes.ts:75-84.
                Some(dt) if shape(&dt.r#type) != shape(payload) => {
                    self.fail(&format!("event {}", event.name.as_str()), "a defined type of this name has another shape, so carbon-cli skips every event");
                    return;
                }
                Some(_) => {}
                None => payloads.push((event.name.as_str(), payload)),
            }
        }
        for (name, payload) in payloads {
            self.walk(name, payload, 0);
        }
    }

    fn field(&mut self, owner: &str, f: &'r StructFieldTypeNode, in_variant: bool) {
        let path = format!("{owner}.{}", f.name.as_str());
        if matches!(f.r#type.as_ref(), TypeNode::Struct(_)) {
            if in_variant {
                // getTypeManifestVisitor.ts:154-157 keeps a variant field's type as it is.
                let reason = "an inline struct in an enum variant; carbon-cli writes Rust that does not compile";
                self.fail(&path, reason);
            } else {
                // getTypeManifestVisitor.ts:336-348 writes the struct's fields into the parent.
                let reason = "an inline struct; carbon-cli puts its fields in the parent struct, so the field names change";
                push(self.out, Level::Note, &path, reason);
            }
        }
        self.walk(&path, &f.r#type, 0);
    }

    /// renderer/src/getTypeManifestVisitor.ts:229-240 ignores the tag size and the variant tags.
    fn enum_rules(&mut self, path: &str, e: &'r codama_nodes::EnumTypeNode) {
        if !is_u8(&e.size) {
            let reason = format!(
                "a {} variant tag; carbon-cli reads a u8 tag",
                number_text(&e.size)
            );
            self.fail(path, reason);
        }
        for (i, variant) in e.variants.iter().enumerate() {
            let (name, tag) = match variant {
                EnumVariantTypeNode::Empty(v) => (v.name.as_str(), v.discriminator),
                EnumVariantTypeNode::Struct(v) => (v.name.as_str(), v.discriminator),
                EnumVariantTypeNode::Tuple(v) => (v.name.as_str(), v.discriminator),
            };
            if let Some(tag) = tag.filter(|t| usize::try_from(*t).ok() != Some(i)) {
                let reason = format!("variant `{name}` has tag {tag}; carbon-cli writes it with tag {i}, its position");
                self.fail(path, reason);
            }
            let variant_path = format!("{path}.{name}");
            match variant {
                EnumVariantTypeNode::Struct(v) => {
                    if let NestedTypeNode::Value(s) = &v.r#struct {
                        for f in &s.fields {
                            self.field(&variant_path, f, true);
                        }
                    }
                }
                EnumVariantTypeNode::Tuple(v) => {
                    if let NestedTypeNode::Value(t) = &v.tuple {
                        for item in &t.items {
                            self.nested(&variant_path, item, 0);
                        }
                    }
                }
                EnumVariantTypeNode::Empty(_) => {}
            }
        }
    }

    fn number(&mut self, path: &str, n: &NumberTypeNode) {
        if n.format == NumberFormat::ShortU16 {
            // getTypeManifestVisitor.ts:269-275 writes the format name as the Rust type.
            self.fail(path, "a shortU16; carbon-cli writes the Rust type `shortU16`, which does not exist, so the decoder does not compile");
        } else if n.endian == Endianness::Be && !is_byte(n) {
            // getTypeManifestVisitor.ts:269-275 ignores `endian`.
            let reason = format!(
                "a big-endian {}; carbon-cli reads it little-endian",
                format_name(n.format)
            );
            self.fail(path, reason);
        }
    }

    fn nested_number(&mut self, path: &str, n: &NestedTypeNode<NumberTypeNode>) {
        if let NestedTypeNode::Value(n) = n {
            self.number(path, n);
        }
    }

    /// One type, by the visitor in renderer/src/getTypeManifestVisitor.ts.
    fn walk(&mut self, path: &str, ty: &'r TypeNode, depth: usize) {
        if depth > 128 {
            return;
        }
        let depth = depth + 1;
        if let Some((reason, inner)) = wrapper(ty) {
            self.fail(path, reason);
            self.nested(path, inner, depth);
            return;
        }
        match ty {
            TypeNode::Number(n) => self.number(path, n),
            TypeNode::Amount(a) => self.nested_number(path, &a.number),
            TypeNode::SolAmount(a) => self.nested_number(path, &a.number),
            TypeNode::DateTime(d) => self.nested_number(path, &d.number),
            // :80-86 renders `bool`, one byte.
            TypeNode::Boolean(b) if !is_u8(&b.size) => {
                let size = number_text(&b.size);
                self.fail(
                    path,
                    format!("a bool stored as a {size}; carbon-cli reads 1 byte"),
                );
            }
            // :88-94 and :326-332 render `Vec<u8>` and `String`, with a u32 length.
            TypeNode::Bytes(_) | TypeNode::String(_) => {
                let reason = "runs to the end of the data; carbon-cli reads a u32 length first";
                self.fail(path, reason);
            }
            TypeNode::SizePrefix(sp) => match sp.r#type.as_ref() {
                // :322-324 drops the prefix and reads the inner type.
                TypeNode::Bytes(_) | TypeNode::String(_) if !is_u32_le(&sp.prefix) => {
                    let prefix = number_text(&sp.prefix);
                    self.fail(
                        path,
                        format!("a {prefix} length; carbon-cli reads a u32 length"),
                    );
                }
                TypeNode::Bytes(_) | TypeNode::String(_) => {}
                inner => {
                    self.fail(path, "a size prefix around a type that is not a string or bytes; carbon-cli does not read the prefix");
                    self.nested(path, inner, depth);
                }
            },
            TypeNode::FixedSize(fs) => self.fixed_wrapper(path, fs, depth),
            TypeNode::Array(a) => {
                // :56-77: a fixed count is `[T; N]`, any other count is `Vec<T>`.
                if !matches!(a.count.as_ref(), CountNode::Fixed(_)) {
                    self.count(path, &a.count);
                }
                self.nested(path, &a.item, depth);
            }
            TypeNode::Map(m) => {
                // :255-267 renders `HashMap`, with a u32 count.
                self.count(path, &m.count);
                self.nested(path, &m.key, depth);
                self.nested(path, &m.value, depth);
            }
            TypeNode::Set(s) => {
                // :311-320 renders `HashSet`, with a u32 count.
                self.count(path, &s.count);
                self.nested(path, &s.item, depth);
            }
            TypeNode::Option(o) => {
                // :277-285 renders `Option<T>`: a u8 tag, and no bytes when absent.
                if !is_u8(&o.prefix) || o.fixed == Some(true) {
                    let fixed = if o.fixed == Some(true) {
                        ", fixed size"
                    } else {
                        ""
                    };
                    let reason = format!("an option with a {} tag{fixed}; carbon-cli reads a u8 tag, and no bytes when the value is absent", number_text(&o.prefix));
                    self.fail(path, reason);
                }
                self.nested(path, &o.item, depth);
            }
            TypeNode::Enum(e) => {
                // :229-240 writes the variants as the field type, which is not Rust.
                let reason = "an enum written inline; carbon-cli writes Rust that does not compile";
                self.fail(path, reason);
                self.enum_rules(path, e);
            }
            TypeNode::Struct(s) => {
                for f in &s.fields {
                    self.field(path, f, false);
                }
            }
            TypeNode::Tuple(t) => {
                for item in &t.items {
                    self.nested(path, item, depth);
                }
            }
            _ => {}
        }
    }

    /// renderer/src/getTypeManifestVisitor.ts:242-253: a fixed size around anything but bytes is
    /// dropped.
    fn fixed_wrapper(
        &mut self,
        path: &str,
        fs: &'r codama_nodes::FixedSizeTypeNode<TypeNode>,
        depth: usize,
    ) {
        match fs.r#type.as_ref() {
            // :242-251 renders fixed-size bytes as `[u8; N]`.
            TypeNode::Bytes(_) => {}
            // :252 reads the inner type, here a string with a u32 length.
            TypeNode::String(_) => {
                let reason = format!(
                    "a string of fixed size {}; carbon-cli reads a u32 length and then the string",
                    fs.size
                );
                self.fail(path, reason);
            }
            inner => {
                // With the inner type of that size, dropping the wrapper reads the same bytes.
                match self.known_size(inner) {
                    Ok(size) if size == u64::try_from(fs.size).ok() => {
                        let reason = format!("a fixed size of {} bytes around a type of the same size; carbon-cli drops the wrapper, and the values stay the same", fs.size);
                        push(self.out, Level::Note, path, reason);
                    }
                    Ok(_) => {
                        let reason = format!("a fixed size of {} bytes around another type; carbon-cli drops the fixed size", fs.size);
                        self.fail(path, reason);
                    }
                    // The size is not known, so no line is better than a wrong one.
                    Err(()) => {}
                }
                self.nested(path, inner, depth);
            }
        }
    }

    /// A type inside an option, a collection, a tuple or a wrapper. The visitor writes an inline
    /// struct there as `{ ... }` (getTypeManifestVisitor.ts:390-400), which is not a Rust type.
    fn nested(&mut self, path: &str, inner: &'r TypeNode, depth: usize) {
        if matches!(inner, TypeNode::Struct(_)) {
            let reason = "an inline struct inside another type; carbon-cli writes Rust that does not compile";
            self.fail(path, reason);
        }
        self.walk(path, inner, depth);
    }

    fn count(&mut self, path: &str, count: &CountNode) {
        let reason = match count {
            CountNode::Prefixed(p) if is_u32_le(&p.prefix) => return,
            CountNode::Prefixed(p) => format!(
                "a {} item count; carbon-cli reads a u32 count",
                number_text(&p.prefix)
            ),
            CountNode::Fixed(f) => {
                format!("a fixed count of {}; carbon-cli reads a u32 count", f.value)
            }
            CountNode::Remainder(_) => {
                "items to the end of the data; carbon-cli reads a u32 count first".to_string()
            }
        };
        self.fail(path, reason);
    }

    /// The size of a field that the renderer drops, when every value has one size.
    fn fixed_size(&mut self, ty: &'r TypeNode, depth: usize) -> Option<u64> {
        match self.visits.checked_sub(1) {
            Some(left) if depth <= 128 => self.visits = left,
            _ => {
                self.gave_up = true;
                return None;
            }
        }
        let number = |n: &NestedTypeNode<NumberTypeNode>| match n {
            NestedTypeNode::Value(n) => width(n.format),
            _ => None,
        };
        match ty {
            TypeNode::Number(n) => width(n.format),
            TypeNode::Boolean(b) => number(&b.size),
            TypeNode::PublicKey(_) => Some(32),
            TypeNode::FixedSize(f) => u64::try_from(f.size).ok(),
            TypeNode::Array(a) => match a.count.as_ref() {
                CountNode::Fixed(c) => self.fixed_size(&a.item, depth + 1)?.checked_mul(c.value),
                _ => None,
            },
            TypeNode::Tuple(t) => self.sum_sizes(t.items.iter(), depth),
            TypeNode::Struct(s) => {
                self.sum_sizes(s.fields.iter().map(|f| f.r#type.as_ref()), depth)
            }
            TypeNode::Link(link) => {
                let (&name, &def) = self.defined.get_key_value(link.name.as_str())?;
                if let Some(known) = self.sizes.get(name) {
                    return *known;
                }
                // A type that holds itself reads this `None`.
                self.sizes.insert(name, None);
                let before = std::mem::replace(&mut self.gave_up, false);
                let size = self.fixed_size(&def.r#type, depth + 1);
                // A size that the limits cut short is not stored, so a later walk can find it.
                if self.gave_up {
                    self.sizes.remove(name);
                } else {
                    self.sizes.insert(name, size);
                }
                self.gave_up |= before;
                size
            }
            _ => None,
        }
    }

    /// The size, or `Err` when the walk stopped on its limits and the size is not known.
    fn known_size(&mut self, ty: &'r TypeNode) -> Result<Option<u64>, ()> {
        self.gave_up = false;
        let size = self.fixed_size(ty, 0);
        if self.gave_up {
            Err(())
        } else {
            Ok(size)
        }
    }

    fn sum_sizes(
        &mut self,
        types: impl Iterator<Item = &'r TypeNode>,
        depth: usize,
    ) -> Option<u64> {
        let mut total = 0u64;
        for ty in types {
            total = total.checked_add(self.fixed_size(ty, depth + 1)?)?;
        }
        Some(total)
    }
}

/// A node whose wrapper the visitor drops, with the reason, and the type inside it.
fn wrapper(ty: &TypeNode) -> Option<(&'static str, &TypeNode)> {
    Some(match ty {
        // getTypeManifestVisitor.ts:423-431 and :287-295 render `Option<T>`, with a u8 tag.
        TypeNode::ZeroableOption(z) => (
            "a zeroable option, which has no tag; carbon-cli reads a u8 tag",
            &z.item,
        ),
        TypeNode::RemainderOption(o) => (
            "a remainder option, which has no tag; carbon-cli reads a u8 tag",
            &o.item,
        ),
        // :297-300.
        TypeNode::HiddenPrefix(h) => (
            "a hidden prefix; carbon-cli reads only the inner type",
            &h.r#type,
        ),
        // No visit function in :52-432, so the merge visitor at :38-49 keeps the inner type.
        TypeNode::HiddenSuffix(h) => (
            "a hidden suffix; carbon-cli reads only the inner type",
            &h.r#type,
        ),
        TypeNode::PreOffset(p) => (
            "a pre-offset; carbon-cli reads only the inner type",
            &p.r#type,
        ),
        TypeNode::PostOffset(p) => (
            "a post-offset; carbon-cli reads only the inner type",
            &p.r#type,
        ),
        TypeNode::Sentinel(s) => (
            "a sentinel; carbon-cli reads only the inner type",
            &s.r#type,
        ),
        _ => return None,
    })
}

fn width(format: NumberFormat) -> Option<u64> {
    match format {
        NumberFormat::U8 | NumberFormat::I8 => Some(1),
        NumberFormat::U16 | NumberFormat::I16 => Some(2),
        NumberFormat::U32 | NumberFormat::I32 | NumberFormat::F32 => Some(4),
        NumberFormat::U64 | NumberFormat::I64 | NumberFormat::F64 => Some(8),
        NumberFormat::U128 | NumberFormat::I128 => Some(16),
        NumberFormat::ShortU16 => None,
    }
}

fn disc(d: &DiscriminatorNode) -> Disc {
    match d {
        DiscriminatorNode::Constant(c) => Disc::Constant {
            offset: c.offset,
            bytes: constant_value_bytes(&c.constant),
        },
        DiscriminatorNode::Field(f) => Disc::Field(f.name.to_string()),
        DiscriminatorNode::Size(s) => Disc::Size(s.size),
    }
}

fn constant_value_bytes(c: &ConstantValueNode) -> Result<Vec<u8>, String> {
    constant_bytes(&c.r#type, &c.value)
}

/// The envelope and the event discriminator, by `classicEventShape` (eventNodes.ts:129-171) or
/// `hiddenPrefixEventShape` (:178-210). `None` for any other shape.
fn event_shape(event: &EventNode) -> Option<(Option<Vec<u8>>, Vec<u8>)> {
    let constants: Vec<(u64, &ConstantValueNode)> = event
        .discriminators
        .iter()
        .filter_map(|d| match d {
            DiscriminatorNode::Constant(c) => Some((c.offset, &c.constant)),
            _ => None,
        })
        .collect();
    if constants.len() != event.discriminators.len() {
        return None;
    }
    let mut sorted = constants;
    sorted.sort_by_key(|(offset, _)| *offset);
    if sorted.len() >= 2 && sorted.first()?.0 == 0 {
        let envelope = constant_value_bytes(sorted.first()?.1)
            .ok()
            .filter(|b| !b.is_empty())?;
        let mut expected = envelope.len() as u64;
        let mut disc = Vec::new();
        for (offset, constant) in sorted.iter().skip(1) {
            if *offset != expected {
                return None;
            }
            let bytes = constant_value_bytes(constant).ok()?;
            expected += bytes.len() as u64;
            disc.extend(bytes);
        }
        return (!disc.is_empty()).then_some((Some(envelope), disc));
    }
    let [(0, constant)] = sorted.as_slice() else {
        return None;
    };
    let bytes = constant_value_bytes(constant)
        .ok()
        .filter(|b| !b.is_empty())?;
    let TypeNode::HiddenPrefix(h) = event.data.as_ref() else {
        return None;
    };
    let [prefix] = h.prefix.as_slice() else {
        return None;
    };
    let same = constant_value_bytes(prefix).ok()? == bytes;
    (same && matches!(h.r#type.as_ref(), TypeNode::Struct(_))).then_some((None, bytes))
}

/// The JSON of a type without docs, as `sameShape` (eventNodes.ts:218-245) compares it.
fn shape(ty: &TypeNode) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("docs");
                map.retain(|_, v| !v.is_null());
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(ty).unwrap_or_default();
    strip(&mut value);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    const U8: &str = r#"{"kind":"numberTypeNode","format":"u8","endian":"le"}"#;
    const U32: &str = r#"{"kind":"numberTypeNode","format":"u32","endian":"le"}"#;
    const STRING: &str = r#"{"kind":"stringTypeNode","encoding":"utf8"}"#;

    fn number(format: &str, endian: &str) -> String {
        format!(r#"{{"kind":"numberTypeNode","format":"{format}","endian":"{endian}"}}"#)
    }

    fn bytes_constant(hex: &str) -> String {
        format!(
            r#"{{"kind":"constantValueNode","type":{{"kind":"bytesTypeNode"}},"value":{{"kind":"bytesValueNode","encoding":"base16","data":"{hex}"}}}}"#
        )
    }

    fn constant_disc(offset: u64, hex: &str) -> String {
        let constant = bytes_constant(hex);
        format!(r#"{{"kind":"constantDiscriminatorNode","offset":{offset},"constant":{constant}}}"#)
    }

    fn field(name: &str, ty: &str) -> String {
        format!(r#"{{"kind":"structFieldTypeNode","name":"{name}","type":{ty}}}"#)
    }

    fn defined(name: &str, ty: &str) -> String {
        format!(r#"{{"kind":"definedTypeNode","name":"{name}","type":{ty}}}"#)
    }

    fn account(name: &str, fields: &[String], discs: &[String]) -> String {
        format!(
            r#"{{"kind":"accountNode","name":"{name}","data":{{"kind":"structTypeNode","fields":[{}]}},"discriminators":[{}]}}"#,
            fields.join(","),
            discs.join(",")
        )
    }

    /// An instruction with a u8 `discriminator` argument of value `tag`, and the accounts.
    fn ix(name: &str, tag: u8, accounts: &[&str]) -> String {
        let accounts: Vec<String> = accounts
            .iter()
            .map(|a| format!(r#"{{"kind":"instructionAccountNode","name":"{a}","isWritable":false,"isSigner":false}}"#))
            .collect();
        format!(
            r#"{{"kind":"instructionNode","name":"{name}","accounts":[{}],"arguments":[{{"kind":"instructionArgumentNode","name":"discriminator","type":{U8},"defaultValue":{{"kind":"numberValueNode","number":{tag}}}}}],"discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}}]}}"#,
            accounts.join(",")
        )
    }

    fn event(name: &str, data: &str, discs: &[String]) -> String {
        format!(
            r#"{{"kind":"eventNode","name":"{name}","data":{data},"discriminators":[{}]}}"#,
            discs.join(",")
        )
    }

    fn root(types: &[String], accounts: &[String], ixs: &[String], events: &[String]) -> String {
        format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","program":{{
            "kind":"programNode","name":"t","publicKey":"P","version":"0","errors":[],"pdas":[],
            "definedTypes":[{}],"accounts":[{}],"instructions":[{}],"events":[{}]}}}}"#,
            types.join(","),
            accounts.join(","),
            ixs.join(","),
            events.join(",")
        )
    }

    /// Every finding but the program-id note, which each Codama root gets.
    fn found(json: &str) -> Vec<(Level, String, String)> {
        let idl = Idl::from_json(json).unwrap();
        findings(json, &idl)
            .unwrap()
            .into_iter()
            .filter(|f| f.item != "program")
            .map(|f| (f.level, f.item, f.reason))
            .collect()
    }

    /// The findings for a struct `t` with one field `f` of this type.
    fn on_field(ty: &str) -> Vec<(Level, String, String)> {
        let body = format!(
            r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
            field("f", ty)
        );
        found(&root(&[defined("t", &body)], &[], &[], &[]))
    }

    /// One finding at `item`, of `level`, whose reason holds `text`.
    fn assert_one(found: &[(Level, String, String)], level: Level, item: &str, text: &str) {
        assert!(
            found.len() == 1
                && found[0].0 == level
                && found[0].1 == item
                && found[0].2.contains(text),
            "{found:?}"
        );
    }

    #[test]
    fn big_endian_number() {
        assert_one(
            &on_field(&number("u64", "be")),
            Level::Fail,
            "t.f",
            "big-endian u64",
        );
        assert!(on_field(&number("u8", "be")).is_empty());
    }

    #[test]
    fn short_u16_does_not_compile() {
        let found = on_field(&number("shortU16", "le"));
        assert_one(&found, Level::Fail, "t.f", "does not compile");
    }

    #[test]
    fn wide_bool() {
        let ty = format!(r#"{{"kind":"booleanTypeNode","size":{U32}}}"#);
        assert_one(&on_field(&ty), Level::Fail, "t.f", "a bool stored as a u32");
    }

    #[test]
    fn string_to_the_end() {
        assert_one(&on_field(STRING), Level::Fail, "t.f", "runs to the end");
    }

    #[test]
    fn size_prefix() {
        let narrow = format!(r#"{{"kind":"sizePrefixTypeNode","type":{STRING},"prefix":{U8}}}"#);
        assert_one(&on_field(&narrow), Level::Fail, "t.f", "a u8 length");
        let borsh = format!(r#"{{"kind":"sizePrefixTypeNode","type":{STRING},"prefix":{U32}}}"#);
        assert!(on_field(&borsh).is_empty());
        let around = format!(r#"{{"kind":"sizePrefixTypeNode","type":{U8},"prefix":{U32}}}"#);
        assert_one(&on_field(&around), Level::Fail, "t.f", "size prefix around");
    }

    #[test]
    fn fixed_size_wrapper() {
        let string = format!(r#"{{"kind":"fixedSizeTypeNode","size":8,"type":{STRING}}}"#);
        assert_one(
            &on_field(&string),
            Level::Fail,
            "t.f",
            "a string of fixed size 8",
        );
        let bytes = r#"{"kind":"fixedSizeTypeNode","size":8,"type":{"kind":"bytesTypeNode"}}"#;
        assert!(on_field(bytes).is_empty());
    }

    #[test]
    fn collection_count() {
        let remainder = format!(
            r#"{{"kind":"arrayTypeNode","item":{U8},"count":{{"kind":"remainderCountNode"}}}}"#
        );
        assert_one(
            &on_field(&remainder),
            Level::Fail,
            "t.f",
            "items to the end",
        );
        let wide = number("u64", "le");
        let set = format!(
            r#"{{"kind":"setTypeNode","item":{U8},"count":{{"kind":"prefixedCountNode","prefix":{wide}}}}}"#
        );
        assert_one(&on_field(&set), Level::Fail, "t.f", "a u64 item count");
    }

    #[test]
    fn fixed_option() {
        let coption =
            format!(r#"{{"kind":"optionTypeNode","fixed":true,"item":{U8},"prefix":{U32}}}"#);
        assert_one(
            &on_field(&coption),
            Level::Fail,
            "t.f",
            "a u32 tag, fixed size",
        );
    }

    #[test]
    fn options_without_a_tag() {
        let zeroable = format!(r#"{{"kind":"zeroableOptionTypeNode","item":{U32}}}"#);
        assert_one(&on_field(&zeroable), Level::Fail, "t.f", "zeroable option");
        let remainder = format!(r#"{{"kind":"remainderOptionTypeNode","item":{U32}}}"#);
        assert_one(
            &on_field(&remainder),
            Level::Fail,
            "t.f",
            "remainder option",
        );
    }

    #[test]
    fn hidden_prefix() {
        let ty = format!(
            r#"{{"kind":"hiddenPrefixTypeNode","type":{U8},"prefix":[{}]}}"#,
            bytes_constant("ff")
        );
        assert_one(&on_field(&ty), Level::Fail, "t.f", "a hidden prefix");
    }

    #[test]
    fn wrapper_without_a_visit_function() {
        let ty = format!(
            r#"{{"kind":"hiddenSuffixTypeNode","type":{U8},"suffix":[{}]}}"#,
            bytes_constant("ff")
        );
        assert_one(&on_field(&ty), Level::Fail, "t.f", "a hidden suffix");
    }

    #[test]
    fn enum_tags() {
        let wide = number("u16", "le");
        let ty = format!(
            r#"{{"kind":"enumTypeNode","size":{wide},"variants":[{{"kind":"enumEmptyVariantTypeNode","name":"a","discriminator":5}}]}}"#
        );
        let found = found(&root(&[defined("t", &ty)], &[], &[], &[]));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].2.contains("a u16 variant tag"), "{found:?}");
        assert!(found[1].2.contains("variant `a` has tag 5"), "{found:?}");
    }

    #[test]
    fn inline_enum_does_not_compile() {
        let ty = format!(
            r#"{{"kind":"enumTypeNode","size":{U8},"variants":[{{"kind":"enumEmptyVariantTypeNode","name":"a"}}]}}"#
        );
        assert_one(&on_field(&ty), Level::Fail, "t.f", "enum written inline");
    }

    #[test]
    fn notes_keep_the_values() {
        let inline = format!(
            r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
            field("g", U8)
        );
        assert_one(&on_field(&inline), Level::Note, "t.f", "inline struct");
        let flag = r#"{"kind":"tupleTypeNode","items":[{"kind":"booleanTypeNode","size":{"kind":"numberTypeNode","format":"u8","endian":"le"}}]}"#;
        let found = found(&root(&[defined("flag", flag)], &[], &[], &[]));
        assert_one(&found, Level::Note, "flag", "tuple of one bool");
    }

    #[test]
    fn field_discriminator_without_a_check() {
        let disc = r#"{"kind":"fieldDiscriminatorNode","name":"key","offset":0}"#.to_string();
        let accounts = [
            account("a", &[field("key", U8)], std::slice::from_ref(&disc)),
            account("b", &[field("key", U8)], &[disc]),
        ];
        let found = found(&root(&[], &accounts, &[], &[]));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].2.contains("only writes a comment"), "{found:?}");
    }

    #[test]
    fn skipped_bytes_must_be_the_removed_field() {
        // The constant covers `key`, which the decoder skips and then reads again.
        let accounts = [account("a", &[field("key", U8)], &[constant_disc(0, "01")])];
        let found = found(&root(&[], &accounts, &[], &[]));
        assert_one(&found, Level::Fail, "account a", "skips 1 bytes");
    }

    #[test]
    fn only_the_first_discriminator_is_checked() {
        let mut one = ix("go", 1, &[]);
        one = one.replace(
            r#"{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}]"#,
            r#"{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0},{"kind":"sizeDiscriminatorNode","size":9}]"#,
        );
        let found = found(&root(&[], &[], &[one], &[]));
        assert_one(
            &found,
            Level::Note,
            "instruction go",
            "does not check the size 9",
        );
    }

    #[test]
    fn unsupported_discriminator_value_stops_generation() {
        let wide = number("u128", "le");
        let constant = format!(
            r#"{{"kind":"constantDiscriminatorNode","offset":0,"constant":{{"kind":"constantValueNode","type":{wide},"value":{{"kind":"numberValueNode","number":1}}}}}}"#
        );
        let accounts = [account("a", &[field("key", &wide)], &[constant])];
        let found = found(&root(&[], &accounts, &[], &[]));
        assert!(
            found
                .iter()
                .any(|f| f.2.contains("a u128 number), so generation stops")),
            "{found:?}"
        );
    }

    #[test]
    fn same_check_decodes_as_the_first_by_name() {
        let found = found(&root(
            &[],
            &[],
            &[ix("zed", 1, &[]), ix("alpha", 1, &[])],
            &[],
        ));
        assert_one(
            &found,
            Level::Fail,
            "instruction zed",
            "tries `alpha` first",
        );
    }

    #[test]
    fn no_discriminator() {
        let lone = r#"{"kind":"instructionNode","name":"go","accounts":[],"arguments":[],"discriminators":[]}"#;
        assert_one(
            &found(&root(&[], &[], &[lone.to_string()], &[])),
            Level::Note,
            "instruction go",
            "no discriminator",
        );
        let other = lone.replace("\"go\"", "\"stop\"");
        let found = found(&root(&[], &[], &[lone.to_string(), other], &[]));
        assert!(found.iter().all(|f| f.0 == Level::Fail), "{found:?}");
    }

    #[test]
    fn duplicate_snake_case_accounts() {
        let found = found(&root(&[], &[], &[ix("go", 1, &["tokenA", "token_a"])], &[]));
        assert_one(&found, Level::Fail, "ix:go", "carbon-cli drops `token_a`");
    }

    /// An Anchor `emit_cpi!` event in Codama: the envelope tag, then the event tag.
    fn cpi_event(name: &str, envelope: &str, tag: &str) -> String {
        let data = format!(
            r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
            field("v", U8)
        );
        event(
            name,
            &data,
            &[constant_disc(0, envelope), constant_disc(8, tag)],
        )
    }

    #[test]
    fn events_framed_two_ways_are_all_skipped() {
        let envelope = "e445a52e51cb9a1d";
        let good = cpi_event("a", envelope, "0101010101010101");
        assert!(found(&root(&[], &[], &[], std::slice::from_ref(&good))).is_empty());
        let bad = event("b", U8, &[constant_disc(0, "02")]);
        let found_bad = found(&root(&[], &[], &[], &[good.clone(), bad]));
        assert_one(&found_bad, Level::Fail, "event b", "skips every event");
        let other = cpi_event("c", "ffffffffffffffff", "0202020202020202");
        let found_other = found(&root(&[], &[], &[], &[good, other]));
        assert_one(&found_other, Level::Fail, "event c", "different event-CPI");
    }

    #[test]
    fn event_and_defined_type_of_one_name() {
        let envelope = "e445a52e51cb9a1d";
        let other = format!(
            r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
            field("w", U32)
        );
        let types = [defined("a", &other)];
        let events = [cpi_event("a", envelope, "0101010101010101")];
        let found = found(&root(&types, &[], &[], &events));
        assert_one(&found, Level::Fail, "event a", "another shape");
    }

    /// An Anchor IDL with these instructions, accounts, types and events.
    fn anchor(address: &str, ixs: &str, accounts: &str, types: &str, events: &str) -> String {
        format!(
            r#"{{"address":"{address}","metadata":{{"name":"t","version":"1","spec":"0.1.0"}},
            "instructions":[{ixs}],"accounts":[{accounts}],"types":[{types}],"events":[{events}]}}"#
        )
    }

    const PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

    fn anchor_found(json: &str) -> Vec<(Level, String, String)> {
        let idl = Idl::from_json(json).unwrap();
        findings(json, &idl)
            .unwrap()
            .into_iter()
            .map(|f| (f.level, f.item, f.reason))
            .collect()
    }

    #[test]
    fn anchor_program_id() {
        let ok = anchor(PROGRAM, "", "", "", "");
        assert!(anchor_found(&ok).is_empty());
        let found = anchor_found(&anchor("A", "", "", "", ""));
        assert_one(&found, Level::Note, "program", "--program-id");
    }

    #[test]
    fn anchor_coption() {
        let ix = r#"{"name":"go","discriminator":[1],"accounts":[],"args":[{"name":"v","type":{"coption":"u64"}}]}"#;
        let found = anchor_found(&anchor(PROGRAM, ix, "", "", ""));
        assert_one(&found, Level::Fail, "ix:go.v", "a COption");
    }

    #[test]
    fn anchor_zero_copy_padding() {
        let account = r#"{"name":"Pool","discriminator":[1,1,1,1,1,1,1,1]}"#;
        let ty = r#"{"name":"Pool","serialization":"bytemuck","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
            {"name":"flag","type":"u8"},{"name":"amount","type":"u64"}]}}"#;
        let found = anchor_found(&anchor(PROGRAM, "", account, ty, ""));
        assert_one(
            &found,
            Level::Fail,
            "account Pool",
            "padding before `amount`",
        );
    }

    #[test]
    fn anchor_same_check() {
        let ixs = r#"{"name":"go","discriminator":[1],"accounts":[],"args":[]},
            {"name":"stop","discriminator":[1,2],"accounts":[],"args":[]}"#;
        let found = anchor_found(&anchor(PROGRAM, ixs, "", "", ""));
        assert_one(&found, Level::Fail, "instruction stop", "tries `go` first");
    }

    #[test]
    fn anchor_duplicate_snake_case_accounts() {
        let ix = r#"{"name":"go","discriminator":[1],"args":[],"accounts":[{"name":"tokenA"},{"name":"token_a"}]}"#;
        let found = anchor_found(&anchor(PROGRAM, ix, "", "", ""));
        assert_one(&found, Level::Fail, "ix:go", "carbon-cli drops `token_a`");
    }

    #[test]
    fn legacy_event_hashed_by_its_written_name() {
        let legacy = format!(
            r#"{{"version":"0.1.0","name":"t","metadata":{{"address":"{PROGRAM}"}},"instructions":[],
            "events":[{{"name":"Swap","fields":[]}},{{"name":"swap_done","fields":[]}}]}}"#
        );
        let found = anchor_found(&legacy);
        assert_one(
            &found,
            Level::Fail,
            "event swap_done",
            "Anchor hashes `event:SwapDone`",
        );
    }

    /// An instruction with two tag arguments that field discriminators name, and `extra`.
    fn two_tags(extra: &str) -> String {
        let arg = |name: &str, tag: u8| {
            format!(
                r#"{{"kind":"instructionArgumentNode","name":"{name}","type":{U8},"defaultValue":{{"kind":"numberValueNode","number":{tag}}}}}"#
            )
        };
        let disc = |name: &str, offset: u8| {
            format!(r#"{{"kind":"fieldDiscriminatorNode","name":"{name}","offset":{offset}}}"#)
        };
        format!(
            r#"{{"kind":"instructionNode","name":"harvest","accounts":[],"arguments":[{},{}{extra}],"discriminators":[{},{}]}}"#,
            arg("discriminator", 26),
            arg("sub", 4),
            disc("discriminator", 0),
            disc("sub", 1)
        )
    }

    #[test]
    fn skip_with_no_field_left_reads_nothing() {
        // token-2022 `harvestWithheldTokensToMint`: the decoder skips one tag, and reads an empty struct.
        assert!(found(&root(&[], &[], &[two_tags("")], &[])).is_empty());
        let fee = format!(r#",{{"kind":"instructionArgumentNode","name":"fee","type":{U32}}}"#);
        let found = found(&root(&[], &[], &[two_tags(&fee)], &[]));
        assert_one(&found, Level::Fail, "instruction harvest", "skips 1 bytes");
    }

    #[test]
    fn earlier_instruction_that_needs_more_accounts_does_not_take_the_data() {
        let all: Vec<String> = (0..18).map(|i| format!("a{i}")).collect();
        let names = |n: usize| all.iter().take(n).map(String::as_str).collect::<Vec<_>>();
        // `swapBaseIn` sorts first and needs 18 accounts, so a 17-account compact swap fails it.
        let ixs = [
            ix("swapBaseIn", 9, &names(18)),
            ix("swapBaseInCompact", 9, &names(17)),
        ];
        assert!(found(&root(&[], &[], &ixs, &[])).is_empty());
        let ixs = [
            ix("swapBaseIn", 9, &names(17)),
            ix("swapBaseInCompact", 9, &names(18)),
        ];
        let found = found(&root(&[], &[], &ixs, &[]));
        assert_one(
            &found,
            Level::Fail,
            "instruction swapBaseInCompact",
            "tries `swapBaseIn` first",
        );
        let anchor_ixs = r#"{"name":"swap","discriminator":[9],"accounts":[{"name":"a"},{"name":"b"}],"args":[]},
            {"name":"swap_compact","discriminator":[9],"accounts":[{"name":"a"}],"args":[]}"#;
        assert!(anchor_found(&anchor(PROGRAM, anchor_ixs, "", "", "")).is_empty());
    }

    #[test]
    fn anchor_groups_flatten_without_a_prefix() {
        let ix = r#"{"name":"bridge","discriminator":[1],"args":[],"accounts":[
            {"name":"cctp","accounts":[{"name":"message"}]},{"name":"wormhole","accounts":[{"name":"message"}]}]}"#;
        let found = anchor_found(&anchor(PROGRAM, ix, "", "", ""));
        assert_one(
            &found,
            Level::Fail,
            "ix:bridge",
            "carbon-cli drops `message`",
        );
    }

    #[test]
    fn size_of_a_link_chain_is_computed_once_per_type() {
        // Each type holds the next one twice, so a walk without a cache visits 2^40 nodes.
        let mut types = Vec::new();
        for i in 0..40 {
            let next = format!(r#"{{"kind":"definedTypeLinkNode","name":"t{}"}}"#, i + 1);
            let body = format!(
                r#"{{"kind":"structTypeNode","fields":[{},{}]}}"#,
                field("a", &next),
                field("b", &next)
            );
            types.push(defined(&format!("t{i}"), &body));
        }
        types.push(defined("t40", U8));
        let link = r#"{"kind":"definedTypeLinkNode","name":"t0"}"#;
        let disc = r#"{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}"#;
        let accounts = [account(
            "a",
            &[field("discriminator", link), field("x", U8)],
            &[disc.to_string()],
        )];
        let found = found(&root(&types, &accounts, &[], &[]));
        assert!(
            found.iter().any(|f| f.2.contains("skips 0 bytes")),
            "{found:?}"
        );
    }

    #[test]
    fn anchor_generic_or_type_alias_stops_carbon_cli() {
        let generic = r#"{"name":"W","generics":[{"kind":"type","name":"T"}],"type":{"kind":"struct","fields":[{"name":"v","type":{"generic":"T"}}]}}"#;
        let found = anchor_found(&anchor(PROGRAM, "", "", generic, ""));
        assert_one(&found, Level::Fail, "type W", "carbon-cli stops");
        let alias = r#"{"name":"Amount","type":{"kind":"type","alias":"u64"}}"#;
        let found = anchor_found(&anchor(PROGRAM, "", "", alias, ""));
        assert_one(&found, Level::Fail, "type Amount", "carbon-cli stops");
    }

    #[test]
    fn shank_accounts_have_no_discriminator() {
        let shank = format!(
            r#"{{"version":"0.1.0","name":"t","metadata":{{"origin":"shank","address":"{PROGRAM}"}},"instructions":[],
            "accounts":[{{"name":"A","type":{{"kind":"struct","fields":[]}}}},{{"name":"B","type":{{"kind":"struct","fields":[]}}}}]}}"#
        );
        let found = anchor_found(&shank);
        assert!(
            found.len() == 2
                && found
                    .iter()
                    .all(|f| f.0 == Level::Fail && f.2.contains("no discriminator")),
            "{found:?}"
        );
    }

    #[test]
    fn inline_struct_that_does_not_compile() {
        let inline = format!(
            r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
            field("g", U8)
        );
        let variant = format!(
            r#"{{"kind":"enumTypeNode","size":{U8},"variants":[{{"kind":"enumStructVariantTypeNode","name":"a","struct":{{"kind":"structTypeNode","fields":[{}]}}}}]}}"#,
            field("f", &inline)
        );
        let found_variant = found(&root(&[defined("e", &variant)], &[], &[], &[]));
        assert_one(&found_variant, Level::Fail, "e.a.f", "does not compile");
        let arg = format!(
            r#"{{"kind":"instructionNode","name":"go","accounts":[],"arguments":[{{"kind":"instructionArgumentNode","name":"p","type":{inline}}}],"discriminators":[]}}"#
        );
        let found_arg = found(&root(&[], &[], &[arg], &[]));
        assert!(
            found_arg.iter().any(|f| f.0 == Level::Fail
                && f.1 == "ix:go.p"
                && f.2.contains("does not compile")),
            "{found_arg:?}"
        );
    }

    #[test]
    fn fixed_size_of_the_same_size_keeps_the_values() {
        let key = r#"{"kind":"fixedSizeTypeNode","size":32,"type":{"kind":"publicKeyTypeNode"}}"#;
        assert_one(&on_field(key), Level::Note, "t.f", "the same size");
    }

    #[test]
    fn float_discriminator_value_is_masked() {
        // discriminatorHelpers.ts:36-37 masks the number, so 1.5 gives the byte 1.
        let disc = r#"{"kind":"constantDiscriminatorNode","offset":0,"constant":{"kind":"constantValueNode",
            "type":{"kind":"numberTypeNode","format":"u8","endian":"le"},"value":{"kind":"numberValueNode","number":1.5}}}"#;
        let accounts = [account(
            "a",
            &[field("discriminator", U8)],
            &[disc.to_string()],
        )];
        assert!(found(&root(&[], &accounts, &[], &[])).is_empty());
    }

    #[test]
    fn optional_account_slot_is_always_sent() {
        // nodes-from-anchor gives each instruction `optionalAccountStrategy: programId`, so a
        // `swap_x` always carries two accounts and `swap` can take it.
        let ixs = r#"{"name":"swap","discriminator":[9],"accounts":[{"name":"a"},{"name":"b"}],"args":[]},
            {"name":"swap_x","discriminator":[9],"accounts":[{"name":"a"},{"name":"b","optional":true}],"args":[]}"#;
        let found = anchor_found(&anchor(PROGRAM, ixs, "", "", ""));
        assert_one(
            &found,
            Level::Fail,
            "instruction swap_x",
            "tries `swap` first",
        );
    }

    #[test]
    fn anchor_array_with_a_generic_length_does_not_compile() {
        let ty = r#"{"name":"W","generics":[{"kind":"const","name":"N","type":"usize"}],
            "type":{"kind":"struct","fields":[{"name":"d","type":{"array":["u8",{"generic":"N"}]}}]}}"#;
        let found = anchor_found(&anchor(PROGRAM, "", "", ty, ""));
        assert_one(&found, Level::Fail, "type W", "does not compile");
    }

    #[test]
    fn inline_struct_inside_an_option_does_not_compile() {
        let inline = format!(
            r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
            field("g", U8)
        );
        let option = format!(r#"{{"kind":"optionTypeNode","prefix":{U8},"item":{inline}}}"#);
        assert_one(&on_field(&option), Level::Fail, "t.f", "does not compile");
        // extractStructArrayItems.ts:25-38 names the item of an array argument, so it compiles.
        let items = format!(
            r#"{{"kind":"arrayTypeNode","item":{inline},"count":{{"kind":"prefixedCountNode","prefix":{U32}}}}}"#
        );
        let arg = format!(
            r#"{{"kind":"instructionNode","name":"go","accounts":[],"arguments":[{{"kind":"instructionArgumentNode","name":"items","type":{items}}}],"discriminators":[]}}"#
        );
        let found = found(&root(&[], &[], &[arg], &[]));
        assert!(found.iter().all(|f| f.0 == Level::Note), "{found:?}");
    }

    #[test]
    fn no_line_when_the_size_walk_gives_up() {
        // 200 links deep is past the walk limit, so the size is not known and no line is right.
        let mut types = Vec::new();
        for i in 0..200 {
            let next = format!(r#"{{"kind":"definedTypeLinkNode","name":"t{}"}}"#, i + 1);
            let body = format!(
                r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
                field("a", &next)
            );
            types.push(defined(&format!("t{i}"), &body));
        }
        types.push(defined("t200", U8));
        let wrapper = r#"{"kind":"fixedSizeTypeNode","size":1,"type":{"kind":"definedTypeLinkNode","name":"t0"}}"#;
        let body = format!(
            r#"{{"kind":"structTypeNode","fields":[{}]}}"#,
            field("f", wrapper)
        );
        types.push(defined("holder", &body));
        assert!(found(&root(&types, &[], &[], &[])).is_empty());
    }
}
