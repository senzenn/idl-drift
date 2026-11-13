//! Predict where Shipstern's Anchor converter, codama-from-anchor (Shipstern 84ad2363), stops
//! `include_shipstern_parser!` with `compile_error!`.
//!
//! The scan follows the converter's steps on idl-drift's model. The model rewrites some JSON
//! shapes, so the scan reads those from the raw JSON.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::enum_tags;
use crate::inspect::{shared_types, zero_copy_roots};
use crate::layout::TypeIndex;
use crate::model::{
    const_len, AccountItem, ArrayLen, EnumFields, Field, Format, GenericArg, GenericParam, Idl,
    Instruction, ReprKind, Seed, Type, TypeDef, TypeDefTy,
};

/// A part of an Anchor IDL that Shipstern's converter rejects. One is enough to stop
/// `include_shipstern_parser!` with `compile_error!`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rejection {
    /// Such as `account Pool`, `instruction swap` or `program`.
    pub item: String,
    /// The codama-from-anchor `Error` variant, such as `UnknownLayout`, or `EnumTag` for a
    /// panic of the macro itself.
    pub rule: &'static str,
    pub reason: String,
}

/// codama-from-anchor `MAX_ARRAY_LEN` and `FIXED_TYPE_SIZE_LIMIT`.
const SIZE_LIMIT: u64 = 10 * 1024 * 1024;
/// codama-from-anchor `NESTING_LIMIT`.
const NESTING_LIMIT: usize = 128;
/// codama-from-anchor `TYPE_NODE_LIMIT`.
const TYPE_NODE_LIMIT: usize = 100_000;

/// The type names that codama-from-anchor `types::leaf_type_node` knows.
const LEAVES: [&str; 17] = [
    "bool", "pubkey", "string", "bytes", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32",
    "i64", "i128", "f32", "f64", "shortU16",
];

/// What Shipstern's converter rejects in this Anchor IDL. The lines come in the order that the
/// converter checks; it stops at the first. The macro uses Codama JSON as it is, so for Codama
/// JSON only the macro's enum tag checks run.
///
/// # Errors
///
/// When `json` is not JSON.
pub fn rejections(json: &str, idl: &Idl) -> Result<Vec<Rejection>, serde_json::Error> {
    let raw: Value = serde_json::from_str(json)?;
    if raw.get("kind").is_some() || raw.get("program").is_some_and(Value::is_object) {
        return Ok(raw
            .get("program")
            .map(enum_tags::rejections)
            .unwrap_or_default());
    }
    let mut out = Vec::new();
    format_rejections(&raw, &mut out);
    // The converter stops before it reads a type, so more lines would describe nothing it reads.
    if out.is_empty() && matches!(idl.format, Format::Anchor | Format::LegacyAnchor) {
        Scan::new(idl, &raw, &mut out).run();
    }
    let mut seen = BTreeSet::new();
    out.retain(|r| seen.insert(r.clone()));
    Ok(out)
}

fn rejection(item: impl Into<String>, rule: &'static str, reason: impl Into<String>) -> Rejection {
    Rejection {
        item: item.into(),
        rule,
        reason: reason.into(),
    }
}

const SECTIONS: [(&str, &str); 3] = [
    ("instructions", "instruction"),
    ("accounts", "account"),
    ("events", "event"),
];

fn items<'a>(raw: &'a Value, section: &str) -> impl Iterator<Item = (String, &'a Value)> {
    raw.get(section)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, item)| {
            let name = item.get("name").and_then(Value::as_str);
            (name.map_or_else(|| format!("#{i}"), str::to_string), item)
        })
}

/// The checks before the converter reads any type: the spec, the legacy upgrade and the
/// discriminator keys (codama-from-anchor lib.rs `upgrade_legacy`, `check_legacy`, `check_spec`).
fn format_rejections(raw: &Value, out: &mut Vec<Rejection>) {
    let spec = raw.pointer("/metadata/spec");
    let legacy = spec.is_none()
        && raw.get("name").is_some_and(Value::is_string)
        && raw.get("instructions").is_some_and(Value::is_array);
    if !legacy {
        if spec.and_then(Value::as_str) != Some("0.1.0") {
            let reason = "not Anchor spec 0.1.0, and not a legacy IDL with a top-level `name` and `instructions`";
            out.push(rejection("program", "UnsupportedSpec", reason));
            return;
        }
        for (section, kind) in SECTIONS {
            for (name, item) in items(raw, section) {
                if item
                    .get("discriminator")
                    .and_then(Value::as_array)
                    .is_none()
                {
                    let reason = "has no `discriminator` list";
                    out.push(rejection(format!("{kind} {name}"), "Json", reason));
                }
            }
        }
        return;
    }
    if let Some(origin) = raw.pointer("/metadata/origin").and_then(Value::as_str) {
        if origin != "anchor" {
            let reason = format!("legacy IDL is not from Anchor: metadata.origin is {origin:?}");
            out.push(rejection("program", "NotAnchor", reason));
        }
    }
    for (section, kind) in SECTIONS {
        for (name, item) in items(raw, section) {
            if let Some(key) = ["discriminant", "discriminator"]
                .into_iter()
                .find(|key| item.get(key).is_some())
            {
                let reason =
                    format!("has its own `{key}`, which the legacy upgrade replaces with a hash");
                out.push(rejection(format!("{kind} {name}"), "NotAnchor", reason));
            }
        }
    }
    let address = raw.pointer("/metadata/address").and_then(Value::as_str);
    if address.is_none_or(str::is_empty) {
        let reason = "legacy IDL has no program id in `metadata.address`";
        out.push(rejection("program", "MissingAddress", reason));
    }
    if raw.get("state").is_some_and(|state| !state.is_null()) {
        let reason = "a `state` section in a legacy IDL is not supported";
        out.push(rejection("program", "LegacyUnsupported", reason));
    }
    for (section, kind) in [("types", "type"), ("accounts", "account")] {
        for (name, item) in items(raw, section) {
            let generics = item.get("generics").and_then(Value::as_array);
            if generics.is_some_and(|g| !g.is_empty()) {
                let reason = "a generic type in a legacy IDL is not supported";
                out.push(rejection(
                    format!("{kind} {name}"),
                    "LegacyUnsupported",
                    reason,
                ));
            }
        }
    }
    // Anchor's legacy upgrade needs `version`; idl-drift reads the IDL without it.
    if out.is_empty() && !raw.get("version").is_some_and(Value::is_string) {
        let reason = "cannot upgrade legacy Anchor IDL: missing field `version`";
        out.push(rejection("program", "Legacy", reason));
    }
}

/// A rejection that only the raw JSON shows, with its codama-from-anchor `Error` variant.
type RawError = (&'static str, String);

/// Type shapes that idl-drift's model rewrites, but that the converter rejects in a spec 0.1.0
/// IDL: `"publicKey"`, `{"defined": "<name>"}` (`types::type_node_at`), and an array length
/// written as a bare name (`types::array_len`).
fn raw_unknown(ty: &Value, out: &mut Vec<RawError>) {
    let unknown = || {
        (
            "UnrecognizedType",
            format!("unrecognized Anchor IDL type {ty}"),
        )
    };
    match ty {
        // The model reads `u256` and `i256`, and the scan rejects them there.
        Value::String(s) if !LEAVES.contains(&s.as_str()) && s != "u256" && s != "i256" => {
            out.push(unknown());
        }
        Value::Object(obj) => {
            if obj.get("defined").is_some_and(Value::is_string) {
                out.push(unknown());
                return;
            }
            if let Some(len @ Value::String(_)) = obj.get("array").and_then(|a| a.get(1)) {
                out.push(("InvalidArrayLength", format!("invalid array length {len}")));
            }
            for key in ["vec", "option", "coption", "alias", "value"] {
                if let Some(inner) = obj.get(key) {
                    raw_unknown(inner, out);
                }
            }
            if let Some(Value::Array(pair)) = obj.get("array") {
                pair.iter().take(1).for_each(|item| raw_unknown(item, out));
            }
            let args = obj.get("defined").and_then(|d| d.get("generics"));
            for arg in args.and_then(Value::as_array).into_iter().flatten() {
                arg.get("type")
                    .into_iter()
                    .for_each(|t| raw_unknown(t, out));
            }
            let fields = obj
                .get("fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            let variants = obj
                .get("variants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            let variant_fields = variants
                .filter_map(|v| v.get("fields").and_then(Value::as_array))
                .flatten();
            for field in fields.chain(variant_fields) {
                raw_unknown(field.get("type").unwrap_or(field), out);
            }
        }
        _ => {}
    }
}

/// Undo the model's legacy key rewrites on one constant type, as Anchor's upgrade does.
fn legacy_constant_type(value: &mut Value) {
    match value {
        Value::String(s) if s == "publicKey" => *s = "pubkey".to_string(),
        Value::Object(map) => {
            match map.get("defined") {
                Some(Value::String(name)) if name == "usize" => {
                    *value = Value::from("u64");
                    return;
                }
                Some(Value::String(name)) => {
                    let name = name.clone();
                    map.insert("defined".into(), serde_json::json!({ "name": name }));
                }
                _ => {}
            }
            map.values_mut().for_each(legacy_constant_type);
        }
        Value::Array(items) => items.iter_mut().for_each(legacy_constant_type),
        _ => {}
    }
}

/// The size and alignment of a type in zero-copy memory.
#[derive(Clone, Copy)]
struct Shape {
    size: u64,
    align: u64,
}

/// A type definition after conversion. Its generic uses are expanded in place.
#[derive(Clone)]
enum Body {
    Struct(Vec<Field>),
    Tuple(Vec<Type>),
    /// The payload types of all variants.
    Enum(Vec<Type>),
    Alias(Type),
}

impl Body {
    /// The type that a use of this body becomes when the converter expands or inlines it.
    fn into_type(self) -> Option<Type> {
        match self {
            Body::Struct(fields) => Some(Type::Struct(fields)),
            Body::Tuple(items) => Some(Type::Tuple(items)),
            Body::Alias(ty) => Some(ty),
            Body::Enum(_) => None,
        }
    }
}

/// A generic argument as `types::Generics` binds it. A missing one reads as JSON `null`.
#[derive(Clone)]
enum Bound {
    Type(Type),
    Const(String),
    Missing,
}

type Scope = BTreeMap<String, Bound>;

/// The converted parts that the later checks read. `None` for a part whose conversion failed.
#[derive(Default)]
struct Converted {
    /// Item, discriminator length and the fields of an account or event payload.
    accounts: Vec<(String, usize, Option<Vec<Field>>)>,
    events: Vec<(String, usize, Option<Vec<Field>>)>,
    constants: Vec<Option<Type>>,
    /// Item, discriminator length and the arguments by camel-case name.
    instructions: Vec<(String, usize, Option<Arguments>)>,
}

/// Instruction arguments by camel-case name, the discriminator first.
type Arguments = Vec<(String, Type)>;

/// The checks on the converted IDL, in codama-from-anchor order: `layout::pad_zero_copy`,
/// `v01::program_node` and `passes::run`.
struct Scan<'a, 'o> {
    idl: &'a Idl,
    index: TypeIndex<'a>,
    generic: BTreeMap<&'a str, &'a TypeDef>,
    plain: BTreeSet<&'a str>,
    claimed: BTreeSet<&'a str>,
    legacy: bool,
    /// For a spec 0.1.0 IDL, the names in its own `types`. Only the legacy upgrade moves an
    /// account's inline `type` there; idl-drift always does.
    declared: Option<BTreeSet<String>>,
    /// Types whose raw fields are bare types (`true`), or a mix of bare and named (`false`).
    bare_fields: BTreeMap<String, bool>,
    /// `raw_unknown` reasons by `type <name>`, `instruction <name>.<arg>` and `constant <name>`.
    raw_errors: BTreeMap<String, Vec<RawError>>,
    raw_constants: Vec<(String, Option<Value>)>,
    budget: usize,
    /// The converter stopped: the type nesting or the node budget ran out.
    stopped: bool,
    expanded: BTreeSet<String>,
    bodies: BTreeMap<String, Body>,
    sizes: BTreeMap<String, Option<u64>>,
    shapes: BTreeMap<&'a str, Option<Shape>>,
    out: &'o mut Vec<Rejection>,
}

impl<'a, 'o> Scan<'a, 'o> {
    fn new(idl: &'a Idl, raw: &Value, out: &'o mut Vec<Rejection>) -> Self {
        let spec = raw.pointer("/metadata/spec").is_some();
        let mut raw_errors: BTreeMap<String, Vec<RawError>> = BTreeMap::new();
        let mut bare_fields = BTreeMap::new();
        for (name, def) in items(raw, "types") {
            let body = def.get("type").unwrap_or(&Value::Null);
            let fields = body.get("fields").and_then(Value::as_array);
            if body.get("kind").and_then(Value::as_str) == Some("struct") {
                let named = |f: &&Value| f.get("name").is_some() && f.get("type").is_some();
                let fields = fields.map(Vec::as_slice).unwrap_or_default();
                match fields.iter().filter(named).count() {
                    n if n == fields.len() => {}
                    0 => _ = bare_fields.insert(name.clone(), true),
                    _ => _ = bare_fields.insert(name.clone(), false),
                }
            }
            if spec {
                let mut reasons = Vec::new();
                raw_unknown(body, &mut reasons);
                raw_errors.insert(format!("type {name}"), reasons);
            }
        }
        if spec {
            for (ix, item) in items(raw, "instructions") {
                for (arg, def) in items(item, "args") {
                    let mut reasons = Vec::new();
                    raw_unknown(def.get("type").unwrap_or(&Value::Null), &mut reasons);
                    raw_errors.insert(format!("instruction {ix}.{arg}"), reasons);
                }
            }
        }
        let mut raw_constants = Vec::new();
        for (name, def) in items(raw, "constants") {
            let ty = def
                .get("type")
                .filter(|t| !t.is_null() && t.as_str() != Some(""));
            if let Some(ty) = ty.filter(|_| spec) {
                let mut reasons = Vec::new();
                raw_unknown(ty, &mut reasons);
                raw_errors.insert(format!("constant {name}"), reasons);
            }
            raw_constants.push((name, ty.cloned()));
        }
        raw_errors.retain(|_, reasons| !reasons.is_empty());
        Self {
            idl,
            index: TypeIndex::new(idl),
            generic: idl
                .types
                .iter()
                .filter(|t| !t.generics.is_empty())
                .map(|t| (t.name.as_str(), t))
                .collect(),
            plain: idl
                .types
                .iter()
                .filter(|t| t.generics.is_empty())
                .map(|t| t.name.as_str())
                .collect(),
            claimed: idl
                .accounts
                .iter()
                .map(|a| a.name.as_str())
                .chain(idl.events.iter().map(|e| e.name.as_str()))
                .collect(),
            legacy: idl.format == Format::LegacyAnchor,
            declared: spec.then(|| items(raw, "types").map(|(name, _)| name).collect()),
            bare_fields,
            raw_errors,
            raw_constants,
            budget: TYPE_NODE_LIMIT,
            stopped: false,
            expanded: BTreeSet::new(),
            bodies: BTreeMap::new(),
            sizes: BTreeMap::new(),
            shapes: BTreeMap::new(),
            out,
        }
    }

    fn push(&mut self, item: &str, rule: &'static str, reason: impl Into<String>) {
        self.out.push(rejection(item, rule, reason));
    }

    /// A type that the converter emits as a Rust type: not generic, and not an account or event.
    fn unclaimed(&self) -> Vec<&'a TypeDef> {
        let mut seen = BTreeSet::new();
        self.idl
            .types
            .iter()
            .filter(|t| t.generics.is_empty() && !self.claimed.contains(t.name.as_str()))
            .filter(|t| seen.insert(t.name.as_str()))
            .collect()
    }

    /// A non-generic type with this name. A generic one cannot be an account or event payload.
    fn plain_def(&self, name: &str) -> Option<&'a TypeDef> {
        self.index.get(name).filter(|t| t.generics.is_empty())
    }

    /// The type of an account or event, as `v01::payload` finds it.
    fn payload_def(&self, name: &str) -> Option<&'a TypeDef> {
        let declared = self.declared.as_ref().is_none_or(|t| t.contains(name));
        self.plain_def(name).filter(|_| declared)
    }

    fn run(&mut self) {
        self.zero_copy();
        let unclaimed = self.unclaimed();
        let converted = self.convert_items(&unclaimed);
        self.renderable(&unclaimed, &converted);
        self.names_and_discriminators(&unclaimed);
        let idl = self.idl;
        self.ident("program", &idl.metadata.name);
        if !self.stopped {
            self.flattened_arguments(&converted);
        }
    }

    /// `v01::program_node`: the emitted types, accounts, constants, events and instructions.
    fn convert_items(&mut self, unclaimed: &[&'a TypeDef]) -> Converted {
        let idl = self.idl;
        let mut converted = Converted::default();
        for td in unclaimed {
            let item = format!("type {}", td.name);
            if !self.ident(&item, &td.name) || self.raw_failed(&item, &format!("type {}", td.name))
            {
                continue;
            }
            if let Some(body) = self.body(&item, td, &Scope::new(), 0) {
                self.bodies.insert(td.name.clone(), body);
            }
        }
        for account in &idl.accounts {
            let item = format!("account {}", account.name);
            let fields = self.payload(&item, &account.name, account.discriminator.len());
            converted
                .accounts
                .push((item, account.discriminator.len(), fields));
        }
        for (name, ty) in self.raw_constants.clone() {
            let item = format!("constant {name}");
            let ty = self.constant(&item, ty);
            converted.constants.push(ty);
        }
        for event in &idl.events {
            let item = format!("event {}", event.name);
            let fields = self.payload(&item, &event.name, event.discriminator.len());
            converted
                .events
                .push((item, event.discriminator.len(), fields));
        }
        for ix in &idl.instructions {
            let item = format!("instruction {}", ix.name);
            let args = self.instruction(&item, ix);
            converted
                .instructions
                .push((item, ix.discriminator.len(), args));
        }
        converted
    }

    /// `v01::payload` and `account_node` or `event_node`.
    fn payload(&mut self, item: &str, name: &str, disc_len: usize) -> Option<Vec<Field>> {
        let Some(td) = self.payload_def(name) else {
            self.push(item, "TypeMissing", "no type definition");
            return None;
        };
        if disc_len == 0 {
            self.push(item, "EmptyDiscriminator", "discriminator is empty");
            return None;
        }
        if self.raw_failed(item, &format!("type {name}")) {
            return None;
        }
        let Some(Type::Struct(fields)) = self.body(item, td, &Scope::new(), 0)?.into_type() else {
            self.push(item, "TypeNotStruct", "type is not a struct");
            return None;
        };
        self.ident(item, name).then_some(fields)
    }

    /// `v01::constant_node`: `"bytes"` is bytes, no type is a string.
    fn constant(&mut self, item: &str, ty: Option<Value>) -> Option<Type> {
        let Some(mut value) = ty else {
            return Some(Type::String);
        };
        if value.as_str() == Some("bytes") {
            return Some(Type::Bytes);
        }
        if self.raw_failed(item, item) {
            return None;
        }
        if self.legacy {
            legacy_constant_type(&mut value);
        }
        // A shape that the model cannot read gives no prediction.
        let ty: Type = serde_json::from_value(value).ok()?;
        self.convert(item, &ty, &Scope::new(), 0)
    }

    /// `v01::instruction_node`: the arguments, then the instruction and account names.
    fn instruction(&mut self, item: &str, ix: &'a Instruction) -> Option<Arguments> {
        if ix.discriminator.is_empty() {
            self.push(item, "EmptyDiscriminator", "discriminator is empty");
            return None;
        }
        let mut args = vec![(
            "discriminator".to_string(),
            Type::Array(Box::new(Type::U8), ArrayLen::Value(ix.discriminator.len())),
        )];
        for arg in &ix.args {
            if !self.ident(item, &arg.name) {
                return None;
            }
            let name = camel_case(&arg.name);
            if name == "discriminator" {
                let reason =
                    "an argument named `discriminator` clashes with the instruction discriminator";
                self.push(item, "DiscriminatorArgument", reason);
                return None;
            }
            if self.raw_failed(item, &format!("{item}.{}", arg.name)) {
                return None;
            }
            let ty = self.convert(item, &arg.ty, &Scope::new(), 0)?;
            args.push((name, ty));
        }
        if !self.ident(item, &ix.name) {
            return None;
        }
        for name in flat_account_names(&ix.accounts) {
            if !self.ident(item, &name) {
                return None;
            }
        }
        Some(args)
    }

    /// Push the raw reasons stored under `key` for `item`. `true` when there are some.
    fn raw_failed(&mut self, item: &str, key: &str) -> bool {
        let Some(reasons) = self.raw_errors.get(key).cloned() else {
            return false;
        };
        for (rule, reason) in reasons {
            self.push(item, rule, reason);
        }
        true
    }

    /// `ident`: the generated Rust name must start with an ASCII letter.
    fn ident(&mut self, item: &str, name: &str) -> bool {
        let ok = camel_case(name).starts_with(|c: char| c.is_ascii_alphabetic());
        if !ok {
            let reason = format!("name {name:?} does not convert to a Rust identifier");
            self.push(item, "InvalidName", reason);
        }
        ok
    }

    /// Each node of a type spends one unit, as `types::Generics::spend` does.
    fn enter(&mut self, item: &str, depth: usize) -> Option<()> {
        if self.stopped {
            return None;
        }
        if depth >= NESTING_LIMIT {
            self.push(item, "RecursionLimit", "type nesting exceeds 128 levels");
            self.stopped = true;
            return None;
        }
        self.spend(item)
    }

    fn spend(&mut self, item: &str) -> Option<()> {
        if self.stopped {
            return None;
        }
        let Some(left) = self.budget.checked_sub(1) else {
            let reason = "type expansion exceeds 100000 type nodes (nested generics?)";
            self.push(item, "TooLarge", reason);
            self.stopped = true;
            return None;
        };
        self.budget = left;
        Some(())
    }

    /// `types::type_node_at` on a type definition's body.
    fn body(&mut self, item: &str, td: &'a TypeDef, scope: &Scope, depth: usize) -> Option<Body> {
        self.enter(item, depth)?;
        let depth = depth + 1;
        match &td.ty {
            TypeDefTy::Struct { fields } => match self.bare_fields.get(&td.name) {
                Some(false) => {
                    let reason =
                        "unrecognized Anchor IDL type: a struct with named and bare fields";
                    self.push(item, "UnrecognizedType", reason);
                    None
                }
                Some(true) => {
                    let mut items = Vec::with_capacity(fields.len());
                    for f in fields {
                        items.push(self.convert(item, &f.ty, scope, depth)?);
                    }
                    Some(Body::Tuple(items))
                }
                None => self.fields(item, fields, scope, depth).map(Body::Struct),
            },
            TypeDefTy::Enum { variants, .. } => {
                let mut types = Vec::new();
                for v in variants {
                    if !self.ident(item, &v.name) {
                        return None;
                    }
                    match &v.fields {
                        EnumFields::Named(fields) => {
                            let fields = self.fields(item, fields, scope, depth)?;
                            types.extend(fields.into_iter().map(|f| f.ty));
                        }
                        EnumFields::Tuple(items) => {
                            for ty in items {
                                types.push(self.convert(item, ty, scope, depth)?);
                            }
                        }
                    }
                }
                Some(Body::Enum(types))
            }
            TypeDefTy::Type { alias } => self.convert(item, alias, scope, depth).map(Body::Alias),
        }
    }

    /// `types::struct_type`: each field name, then its type.
    fn fields(
        &mut self,
        item: &str,
        fields: &[Field],
        scope: &Scope,
        depth: usize,
    ) -> Option<Vec<Field>> {
        let mut out = Vec::with_capacity(fields.len());
        for f in fields {
            if !self.ident(item, &f.name) {
                return None;
            }
            let ty = self.convert(item, &f.ty, scope, depth)?;
            out.push(Field {
                name: f.name.clone(),
                ty,
            });
        }
        Some(out)
    }

    /// `types::type_node_at`: the type with each generic use expanded and each length known.
    fn convert(&mut self, item: &str, ty: &Type, scope: &Scope, depth: usize) -> Option<Type> {
        self.enter(item, depth)?;
        let depth = depth + 1;
        Some(match ty {
            Type::U256 | Type::I256 | Type::Map(..) | Type::Set(_) | Type::Tuple(_) => {
                let reason = format!("unrecognized Anchor IDL type {}", type_word(ty));
                self.push(item, "UnrecognizedType", reason);
                return None;
            }
            Type::Array(inner, len) => {
                let inner = self.convert(item, inner, scope, depth)?;
                let len = self.array_len(item, len, scope)?;
                Type::Array(Box::new(inner), ArrayLen::Value(len))
            }
            Type::Vec(inner) => Type::Vec(Box::new(self.convert(item, inner, scope, depth)?)),
            Type::Option(inner) => Type::Option(Box::new(self.convert(item, inner, scope, depth)?)),
            Type::COption(inner) => {
                Type::COption(Box::new(self.convert(item, inner, scope, depth)?))
            }
            Type::Defined { name, generics } => {
                return self.defined(item, name, generics, scope, depth);
            }
            Type::Generic(name) => {
                let Some(Bound::Type(bound)) = scope.get(name).cloned() else {
                    let reason = format!("generic argument \"{name}\" is not bound");
                    self.push(item, "GenericArgMissing", reason);
                    return None;
                };
                return self.convert(item, &bound, scope, depth);
            }
            leaf => leaf.clone(),
        })
    }

    /// `types::array_len`: a number, or a bound const argument, up to 10 MiB.
    fn array_len(&mut self, item: &str, len: &ArrayLen, scope: &Scope) -> Option<usize> {
        let invalid = |text: String| format!("invalid array length {text}");
        let len = match len {
            ArrayLen::Value(len) => *len,
            ArrayLen::Generic(name) => {
                let Some(Bound::Const(value)) = scope.get(name) else {
                    let reason = invalid(format!("{{\"generic\":\"{name}\"}}"));
                    self.push(item, "InvalidArrayLength", reason);
                    return None;
                };
                let Some(len) = const_len(value) else {
                    self.push(item, "InvalidArrayLength", invalid(value.clone()));
                    return None;
                };
                len
            }
        };
        if u64::try_from(len).map_or(true, |len| len > SIZE_LIMIT) {
            self.push(item, "InvalidArrayLength", invalid(len.to_string()));
            return None;
        }
        Some(len)
    }

    /// `types::defined_type`: a link to a plain type, or the generic type's body, expanded
    /// with the arguments bound to its parameters.
    fn defined(
        &mut self,
        item: &str,
        name: &str,
        args: &[GenericArg],
        scope: &Scope,
        depth: usize,
    ) -> Option<Type> {
        if args.is_empty() {
            // codama-from-anchor `usize_to_u64` reads a legacy `{"defined": "usize"}` as `u64`.
            if self.legacy && name == "usize" {
                return Some(Type::U64);
            }
            if self.generic.contains_key(name) {
                let reason = format!("generic type `{name}` is used without arguments");
                self.push(item, "GenericArgsMissing", reason);
                return None;
            }
            if !self.plain.contains(name) {
                self.push(
                    item,
                    "UndefinedType",
                    format!("type `{name}` is not defined"),
                );
                return None;
            }
            return Some(Type::Defined {
                name: name.to_string(),
                generics: Vec::new(),
            });
        }
        let Some(td) = self.generic.get(name).copied() else {
            let reason = format!("generic type `{name}` is not defined");
            self.push(item, "GenericTypeMissing", reason);
            return None;
        };
        if matches!(td.ty, TypeDefTy::Enum { .. }) {
            let reason = format!("generic enum `{name}` would expand inline, which the generated parser cannot render");
            self.push(item, "GenericEnum", reason);
            return None;
        }
        let mut inner = Scope::new();
        for (i, param) in td.generics.iter().enumerate() {
            let bound = match args.get(i) {
                Some(arg) => self.resolve_arg(item, arg, scope)?,
                None => Bound::Missing,
            };
            // A const parameter reads only a const argument, and a type parameter a type.
            let bound = match (param, bound) {
                (GenericParam::Const { .. }, b @ Bound::Const(_))
                | (GenericParam::Type { .. }, b @ Bound::Type(_)) => b,
                _ => Bound::Missing,
            };
            inner.insert(param.name().to_string(), bound);
        }
        if self.expanded.insert(name.to_string()) && self.raw_failed(item, &format!("type {name}"))
        {
            return None;
        }
        self.body(item, td, &inner, depth)?.into_type()
    }

    /// `types::Generics::resolve`: an argument with the caller's bindings put in.
    fn resolve_arg(&mut self, item: &str, arg: &GenericArg, scope: &Scope) -> Option<Bound> {
        match arg {
            // A const argument can name a const parameter of the caller.
            GenericArg::Const { value } => Some(match scope.get(value) {
                Some(Bound::Const(outer)) => Bound::Const(outer.clone()),
                Some(Bound::Missing) => Bound::Missing,
                _ => Bound::Const(value.clone()),
            }),
            // Anchor passes a const parameter on as `{"kind": "type", "type": {"generic": "N"}}`.
            GenericArg::Type { ty } => match (&**ty, ty_const(ty, scope)) {
                (Type::Generic(_), Some(value)) => {
                    if const_len(&value).is_none() {
                        let reason = format!("invalid array length {value}");
                        self.push(item, "InvalidArrayLength", reason);
                        return None;
                    }
                    Some(Bound::Const(value))
                }
                _ => self.resolve(item, ty, scope, 0).map(Bound::Type),
            },
        }
    }

    fn resolve(&mut self, item: &str, ty: &Type, scope: &Scope, depth: usize) -> Option<Type> {
        if depth > NESTING_LIMIT {
            self.push(item, "RecursionLimit", "type nesting exceeds 128 levels");
            self.stopped = true;
            return None;
        }
        self.spend(item)?;
        let depth = depth + 1;
        let boxed = |s: &mut Self, inner: &Type| s.resolve(item, inner, scope, depth).map(Box::new);
        Some(match ty {
            Type::Generic(name) => {
                let Some(Bound::Type(bound)) = scope.get(name).cloned() else {
                    let reason = format!("generic argument {name:?} is not bound");
                    self.push(item, "GenericArgMissing", reason);
                    return None;
                };
                for _ in 0..type_nodes(&bound) {
                    self.spend(item)?;
                }
                bound
            }
            Type::Array(inner, len) => {
                let len = match len {
                    ArrayLen::Generic(name) => match scope.get(name) {
                        Some(Bound::Const(value)) => {
                            let Some(len) = const_len(value) else {
                                let reason = format!("invalid array length {value}");
                                self.push(item, "InvalidArrayLength", reason);
                                return None;
                            };
                            ArrayLen::Value(len)
                        }
                        _ => len.clone(),
                    },
                    ArrayLen::Value(_) => len.clone(),
                };
                Type::Array(boxed(self, inner)?, len)
            }
            Type::Vec(inner) => Type::Vec(boxed(self, inner)?),
            Type::Option(inner) => Type::Option(boxed(self, inner)?),
            Type::COption(inner) => Type::COption(boxed(self, inner)?),
            Type::Defined { name, generics } => {
                let mut args = Vec::with_capacity(generics.len());
                for arg in generics {
                    args.push(match arg {
                        GenericArg::Type { ty } => GenericArg::Type {
                            ty: boxed(self, ty)?,
                        },
                        GenericArg::Const { value } => match scope.get(value) {
                            Some(Bound::Const(outer)) => GenericArg::Const {
                                value: outer.clone(),
                            },
                            _ => arg.clone(),
                        },
                    });
                }
                Type::Defined {
                    name: name.clone(),
                    generics: args,
                }
            }
            other => other.clone(),
        })
    }

    /// `v01::check_renderable`: each type node, the sums of each account and instruction, and
    /// each event with its discriminator.
    fn renderable(&mut self, unclaimed: &[&'a TypeDef], converted: &Converted) {
        if self.stopped {
            return;
        }
        let mut chains = BTreeMap::new();
        for td in unclaimed {
            let item = format!("type {}", td.name);
            let Some(body) = self.bodies.get(&td.name).cloned() else {
                continue;
            };
            let checked = match body {
                Body::Enum(types) => types.iter().all(|ty| self.check(&item, ty)),
                body => body.into_type().is_none_or(|ty| self.check(&item, &ty)),
            };
            if checked
                && self
                    .alias_depth(&td.name, &mut chains, &mut Vec::new())
                    .is_none()
            {
                self.push(&item, "RecursionLimit", "type nesting exceeds 128 levels");
            }
        }
        for (item, disc_len, fields) in &converted.accounts {
            let Some(fields) = fields else { continue };
            let types: Vec<&Type> = fields.iter().map(|f| &f.ty).collect();
            if self.too_large(item, *disc_len, &types) {
                continue;
            }
            for ty in &types {
                if !self.check(item, ty) {
                    break;
                }
            }
        }
        for (item, disc_len, fields) in &converted.events {
            let Some(fields) = fields else { continue };
            // The event data is the payload behind a hidden prefix of the discriminator.
            let payload = Type::Struct(fields.clone());
            if !self.too_large(item, *disc_len, &[&payload]) {
                self.check(item, &payload);
            }
        }
        for (item, disc_len, args) in &converted.instructions {
            let Some(args) = args else { continue };
            let types: Vec<&Type> = args.iter().skip(1).map(|(_, ty)| ty).collect();
            if self.too_large(item, *disc_len, &types) {
                continue;
            }
            for ty in &types {
                if !self.check(item, ty) {
                    break;
                }
            }
        }
    }

    /// `v01::check_fixed_size` on the discriminator and these types together.
    fn too_large(&mut self, item: &str, disc_len: usize, types: &[&Type]) -> bool {
        let mut total = Some(u64::try_from(disc_len).unwrap_or(u64::MAX));
        for ty in types {
            let size = self.fixed_size(ty);
            total = total.zip(size).map(|(a, b)| a.saturating_add(b));
        }
        let large = total.is_some_and(|size| size > SIZE_LIMIT);
        if large {
            self.push(
                item,
                "FixedTypeTooLarge",
                "fixed-size type exceeds the 10 MiB limit",
            );
        }
        large
    }

    /// `v01::check_node` on one converted type. `false` after the first rejection.
    fn check(&mut self, item: &str, ty: &Type) -> bool {
        if let Type::COption(inner) = ty {
            if self.fixed_size(inner).is_none() {
                self.push(
                    item,
                    "VariableSizeCOption",
                    "coption of a type with no fixed size",
                );
                return false;
            }
        }
        if self.fixed_size(ty).is_some_and(|size| size > SIZE_LIMIT) {
            self.push(
                item,
                "FixedTypeTooLarge",
                "fixed-size type exceeds the 10 MiB limit",
            );
            return false;
        }
        ty.children()
            .into_iter()
            .all(|child| self.check(item, child))
    }

    /// `v01::fixed_size_of_type` on a converted type. A link finds only an emitted type, so an
    /// account or event type, an enum and a cycle have no size.
    fn fixed_size(&mut self, ty: &Type) -> Option<u64> {
        self.fixed_size_at(ty, 0)
    }

    /// The converter stops past 128 levels, so a deeper chain has no size (`v01.rs:284`).
    fn fixed_size_at(&mut self, ty: &Type, depth: usize) -> Option<u64> {
        if depth > NESTING_LIMIT {
            return None;
        }
        let depth = depth + 1;
        Some(match ty {
            Type::Bool | Type::U8 | Type::I8 => 1,
            Type::U16 | Type::I16 => 2,
            Type::U32 | Type::I32 | Type::F32 => 4,
            Type::U64 | Type::I64 | Type::F64 => 8,
            Type::U128 | Type::I128 => 16,
            Type::Pubkey => 32,
            Type::COption(inner) => self.fixed_size_at(inner, depth)?.saturating_add(4),
            Type::Array(inner, ArrayLen::Value(len)) => self
                .fixed_size_at(inner, depth)?
                .saturating_mul(u64::try_from(*len).unwrap_or(u64::MAX)),
            Type::Tuple(items) => self.sum_sizes(items.iter(), depth)?,
            Type::Struct(fields) => self.sum_sizes(fields.iter().map(|f| &f.ty), depth)?,
            Type::Defined { name, .. } => {
                if let Some(known) = self.sizes.get(name) {
                    return *known;
                }
                self.sizes.insert(name.clone(), None);
                let body = self.bodies.get(name).cloned()?;
                let size = match body {
                    Body::Enum(_) => None,
                    body => body
                        .into_type()
                        .and_then(|ty| self.fixed_size_at(&ty, depth)),
                };
                self.sizes.insert(name.clone(), size);
                size?
            }
            _ => return None,
        })
    }

    fn sum_sizes<'t>(
        &mut self,
        types: impl Iterator<Item = &'t Type>,
        depth: usize,
    ) -> Option<u64> {
        let mut total = 0u64;
        for ty in types {
            total = total.saturating_add(self.fixed_size_at(ty, depth)?);
        }
        Some(total)
    }

    /// `v01::alias_depth`: the longest chain of aliases from `name`. `None` past 128 links or on
    /// a cycle, which never ends.
    fn alias_depth(
        &self,
        name: &str,
        known: &mut BTreeMap<String, usize>,
        path: &mut Vec<String>,
    ) -> Option<usize> {
        if path.len() > NESTING_LIMIT || path.iter().any(|p| p == name) {
            return None;
        }
        if let Some(&longest) = known.get(name) {
            return (path.len() + longest <= NESTING_LIMIT).then_some(longest);
        }
        let body = match self.bodies.get(name) {
            Some(Body::Alias(ty)) => ty,
            Some(Body::Tuple(items)) if items.len() == 1 => items.first()?,
            _ => return Some(0),
        };
        let mut links = Vec::new();
        collect_links(body, &mut links);
        path.push(name.to_string());
        let mut longest = 0;
        for link in links {
            longest = longest.max(self.alias_depth(&link, known, path)? + 1);
        }
        path.pop();
        known.insert(name.to_string(), longest);
        Some(longest)
    }

    /// `layout::pad_zero_copy`: every enum in a `bytemuck` account, and every
    /// `bytemuckunsafe` root, needs a known layout; then a padded type must not be Borsh too.
    fn zero_copy(&mut self) {
        let idl = self.idl;
        let accounts: Vec<&'a TypeDef> = idl
            .accounts
            .iter()
            .filter_map(|a| self.payload_def(&a.name))
            .collect();
        let (in_bytemuck, roots) = zero_copy_roots(idl, &self.index, &accounts);
        let reason = "is in zero-copy memory, but its layout cannot be computed";
        let mut unknown = false;
        for td in &idl.types {
            if in_bytemuck.contains(td.name.as_str()) && matches!(td.ty, TypeDefTy::Enum { .. }) {
                self.push(&format!("type {}", td.name), "UnknownLayout", reason);
                unknown = true;
            }
        }
        for root in roots {
            if self.struct_shape(root, 0).is_none() {
                self.push(&format!("type {}", root.name), "UnknownLayout", reason);
                unknown = true;
            }
        }
        if unknown {
            return;
        }
        for t in shared_types(idl) {
            let reason = "has implicit repr(C) padding but Borsh also reads it, so one of the two would be misread";
            self.push(&format!("type {}", t.name), "PaddedBorshType", reason);
        }
    }

    /// `layout::Layouts::struct_layout`. Unlike idl-drift's own layout, a missing `repr` and
    /// any `repr(Rust)` have no layout here.
    fn struct_shape(&mut self, td: &'a TypeDef, depth: usize) -> Option<Shape> {
        if depth > NESTING_LIMIT || !td.generics.is_empty() {
            return None;
        }
        // The converter reads the `fields` key, which an enum or alias body does not have.
        let fields: &'a [Field] = match &td.ty {
            TypeDefTy::Struct { fields } => fields,
            _ => &[],
        };
        let repr = td.repr?;
        let transparent = repr.kind == ReprKind::Transparent;
        // A bare field has no `type` key, so only `repr(transparent)` gives it a layout.
        if repr.kind == ReprKind::Rust || (!transparent && self.bare_fields.contains_key(&td.name))
        {
            return None;
        }
        let mut shapes = Vec::with_capacity(fields.len());
        for f in fields {
            shapes.push(self.field_shape(&f.ty, depth)?);
        }
        if transparent {
            if shapes.iter().any(|s| s.size == 0 && s.align != 1) {
                return None;
            }
            let mut nonzero = shapes.iter().filter(|s| s.size != 0);
            let shape = *nonzero.next()?;
            return nonzero.next().is_none().then_some(shape);
        }
        let (mut offset, mut widest) = (0u64, 1u64);
        for shape in &shapes {
            let align = if repr.packed { 1 } else { shape.align };
            offset = offset
                .checked_next_multiple_of(align)?
                .checked_add(shape.size)?;
            widest = widest.max(align);
        }
        let explicit = match repr.align {
            None => 1,
            Some(align) if align.is_power_of_two() && align <= 1 << 29 => {
                u64::try_from(align).ok()?
            }
            Some(_) => return None,
        };
        let align = widest.max(explicit);
        Some(Shape {
            size: offset.checked_next_multiple_of(align)?,
            align,
        })
    }

    /// `layout::Layouts::field_layout`, on SBF: a 128-bit integer aligns to 8, a pubkey to 1.
    fn field_shape(&mut self, ty: &'a Type, depth: usize) -> Option<Shape> {
        if depth > NESTING_LIMIT {
            return None;
        }
        let (size, align) = match ty {
            Type::Bool | Type::U8 | Type::I8 => (1, 1),
            Type::U16 | Type::I16 => (2, 2),
            Type::U32 | Type::I32 | Type::F32 => (4, 4),
            Type::U64 | Type::I64 | Type::F64 => (8, 8),
            Type::U128 | Type::I128 => (16, 8),
            Type::Pubkey => (32, 1),
            Type::Array(item, ArrayLen::Value(len)) => {
                let item = self.field_shape(item, depth)?;
                let len = u64::try_from(*len).ok()?;
                (item.size.checked_mul(len)?, item.align)
            }
            Type::Defined { name, .. } => {
                if let Some(known) = self.shapes.get(name.as_str()) {
                    return *known;
                }
                // A type that holds itself reads this `None`.
                self.shapes.insert(name.as_str(), None);
                let td = self.index.get(name)?;
                let shape = match &td.ty {
                    TypeDefTy::Type { alias } if td.generics.is_empty() => {
                        self.field_shape(alias, depth + 1)
                    }
                    _ => self.struct_shape(td, depth + 1),
                };
                self.shapes.insert(name.as_str(), shape);
                return shape;
            }
            _ => return None,
        };
        Some(Shape { size, align })
    }

    /// `v01::check_names` on the emitted types, and `v01::check_items` on accounts, events
    /// and instructions.
    fn names_and_discriminators(&mut self, unclaimed: &[&'a TypeDef]) {
        let idl = self.idl;
        self.collisions("type", unclaimed.iter().map(|t| t.name.as_str()));
        let accounts: Vec<(&str, &[u8])> = idl
            .accounts
            .iter()
            .map(|a| (a.name.as_str(), a.discriminator.as_slice()))
            .collect();
        let events: Vec<(&str, &[u8])> = idl
            .events
            .iter()
            .map(|e| (e.name.as_str(), e.discriminator.as_slice()))
            .collect();
        let instructions: Vec<(&str, &[u8])> = idl
            .instructions
            .iter()
            .map(|ix| (ix.name.as_str(), ix.discriminator.as_slice()))
            .collect();
        for (kind, items) in [
            ("account", accounts),
            ("event", events),
            ("instruction", instructions),
        ] {
            self.collisions(kind, items.iter().map(|(name, _)| *name));
            // The account parser has no collision group, so the first of two equal ones wins.
            let rejects_equal = kind == "account";
            for (i, (name, disc)) in items.iter().enumerate() {
                let item = format!("{kind} {name}");
                for (j, (other, other_disc)) in items.iter().enumerate() {
                    if !disc.is_empty()
                        && other_disc.len() > disc.len()
                        && other_disc.starts_with(disc)
                    {
                        let reason = format!("discriminator {disc:?} is a prefix of `{other}`'s");
                        self.push(&item, "AmbiguousDiscriminator", reason);
                        break;
                    }
                    if rejects_equal && j < i && !disc.is_empty() && other_disc == disc {
                        let reason = format!("same discriminator as `{other}`");
                        self.push(&item, "DuplicateDiscriminator", reason);
                        break;
                    }
                }
            }
        }
    }

    fn collisions<'n>(&mut self, kind: &str, names: impl Iterator<Item = &'n str>) {
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        for name in names {
            if let Some(first) = seen.insert(camel_case(name), name) {
                let reason = format!("same Rust name as `{first}`");
                self.push(&format!("{kind} {name}"), "NameCollision", reason);
            }
        }
    }

    /// `passes::unwrap_instruction_args_defined_types` inlines a type that only one argument
    /// uses, then `flatten_instruction_data_arguments` puts a struct's fields in the list.
    fn flattened_arguments(&mut self, converted: &Converted) {
        let mut uses: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        let payloads = converted.accounts.iter().chain(&converted.events);
        for fields in payloads.filter_map(|(_, _, fields)| fields.as_ref()) {
            for f in fields {
                count_links(&f.ty, &mut uses);
            }
        }
        for body in self.bodies.values() {
            let types: Vec<&Type> = match body {
                Body::Struct(fields) => fields.iter().map(|f| &f.ty).collect(),
                Body::Tuple(types) | Body::Enum(types) => types.iter().collect(),
                Body::Alias(ty) => vec![ty],
            };
            for ty in types {
                count_links(ty, &mut uses);
            }
        }
        for ty in converted.constants.iter().flatten() {
            count_links(ty, &mut uses);
        }
        let idl = self.idl;
        for (ix, (_, _, args)) in idl.instructions.iter().zip(&converted.instructions) {
            let Some(args) = args else { continue };
            for ty in self.pda_seed_types(ix, args) {
                count_links(&ty, &mut uses);
            }
            for (_, ty) in args.iter().skip(1) {
                match ty {
                    Type::Defined { name, .. } => {
                        let entry = uses.entry(name.clone()).or_default();
                        entry.0 += 1;
                        entry.1 += 1;
                    }
                    ty => count_links(ty, &mut uses),
                }
            }
        }
        for (item, _, args) in &converted.instructions {
            let Some(args) = args else { continue };
            let mut names = vec!["discriminator".to_string()];
            for (name, ty) in args.iter().skip(1) {
                let inlined = match ty {
                    Type::Defined { name, .. } if uses.get(name) == Some(&(1, 1)) => {
                        self.bodies.get(name).cloned().and_then(Body::into_type)
                    }
                    _ => None,
                };
                match inlined.as_ref().unwrap_or(ty) {
                    Type::Struct(fields) => {
                        names.extend(fields.iter().map(|f| camel_case(&f.name)));
                    }
                    _ => names.push(name.clone()),
                }
            }
            let mut seen = BTreeSet::new();
            let duplicates: BTreeSet<String> = names
                .into_iter()
                .filter(|n| !seen.insert(n.clone()))
                .collect();
            if !duplicates.is_empty() {
                let list: Vec<String> = duplicates.into_iter().collect();
                let reason =
                    format!("duplicate arguments {list:?} after camel case or struct flattening");
                self.push(item, "ConflictingFlattenedArguments", reason);
            }
        }
    }

    /// The argument types of the PDA seeds that `passes::unwrap_instruction_args_defined_types`
    /// counts: each seed resolves, and this program owns the PDA (`v01::pda_default`).
    fn pda_seed_types(&self, ix: &Instruction, args: &[(String, Type)]) -> Vec<Type> {
        fn singles<'i>(items: &'i [AccountItem], out: &mut Vec<&'i crate::model::Account>) {
            for item in items {
                match item {
                    AccountItem::Single(account) => out.push(account),
                    AccountItem::Composite { accounts, .. } => singles(accounts, out),
                }
            }
        }
        // `upgrade_legacy` drops every PDA.
        if self.legacy {
            return Vec::new();
        }
        let mut accounts = Vec::new();
        singles(&ix.accounts, &mut accounts);
        let arg = |path: &str| {
            let wanted = camel_case(path);
            args.iter()
                .find(|(name, _)| *name == wanted)
                .map(|(_, ty)| ty)
        };
        let resolves = |seed: &Seed| match seed {
            Seed::Arg(path) => !path.contains('.') && arg(path).is_some(),
            Seed::Account(_) | Seed::Const(_) => true,
            Seed::Other => false,
        };
        let mut out = Vec::new();
        for account in accounts {
            let Some(pda) = &account.pda else { continue };
            if account.address.as_deref().is_some_and(|a| !a.is_empty())
                || !pda.seeds.iter().chain(&pda.program).all(resolves)
            {
                continue;
            }
            let owned = match &pda.program {
                Some(Seed::Const(bytes)) => {
                    bytes.is_empty() || bs58::encode(bytes).into_string() == self.idl.address
                }
                _ => true,
            };
            for seed in pda.seeds.iter().filter(|_| owned) {
                // `v01::seed_type` gives a string or bytes argument without its length prefix.
                if let Seed::Arg(path) = seed {
                    match arg(path) {
                        Some(Type::String | Type::Bytes) | None => {}
                        Some(ty) => out.push(ty.clone()),
                    }
                }
            }
        }
        out
    }
}

/// The value of a const parameter that `ty` names, when `ty` is `{"generic": "<name>"}`.
fn ty_const(ty: &Type, scope: &Scope) -> Option<String> {
    let Type::Generic(name) = ty else {
        return None;
    };
    match scope.get(name) {
        Some(Bound::Const(value)) => Some(value.clone()),
        _ => None,
    }
}

/// `passes::count_links` on a converted type, which has no generic use left.
fn count_links(ty: &Type, uses: &mut BTreeMap<String, (usize, usize)>) {
    if let Type::Defined { name, .. } = ty {
        uses.entry(name.clone()).or_default().0 += 1;
        return;
    }
    ty.children()
        .into_iter()
        .for_each(|child| count_links(child, uses));
}

fn collect_links(ty: &Type, out: &mut Vec<String>) {
    if let Type::Defined { name, .. } = ty {
        out.push(name.clone());
    }
    ty.children()
        .into_iter()
        .for_each(|child| collect_links(child, out));
}

/// The number of nodes in a type, which a copy of it spends.
fn type_nodes(ty: &Type) -> usize {
    1 + ty.children().into_iter().map(type_nodes).sum::<usize>()
}

fn type_word(ty: &Type) -> &'static str {
    match ty {
        Type::U256 => "\"u256\"",
        Type::I256 => "\"i256\"",
        Type::Map(..) => "map",
        Type::Set(_) => "set",
        _ => "tuple",
    }
}

/// The account names of an instruction after Codama flattens its groups (codama-from-anchor
/// `v01::instruction_accounts`): a group adds its name as a prefix only when a name repeats.
fn flat_account_names(items: &[AccountItem]) -> Vec<String> {
    fn walk(items: &[AccountItem], prefix: Option<&str>, out: &mut Vec<String>) {
        let should_prefix = prefix.is_some() || has_duplicate_names(items, &mut BTreeSet::new());
        for item in items {
            match item {
                AccountItem::Composite { name, accounts } => {
                    let group = should_prefix.then(|| prefixed(prefix, name));
                    walk(accounts, group.as_deref(), out);
                }
                AccountItem::Single(account) => out.push(prefixed(prefix, &account.name)),
            }
        }
    }
    fn has_duplicate_names(items: &[AccountItem], seen: &mut BTreeSet<String>) -> bool {
        items.iter().any(|item| match item {
            AccountItem::Composite { accounts, .. } => has_duplicate_names(accounts, seen),
            AccountItem::Single(account) => !seen.insert(camel_case(&account.name)),
        })
    }
    fn prefixed(prefix: Option<&str>, name: &str) -> String {
        prefix.map_or_else(|| name.to_string(), |p| format!("{p}_{name}"))
    }
    let mut out = Vec::new();
    walk(items, None, &mut out);
    out
}

/// The JavaScript `camelCase` of `@codama/nodes`, as codama-from-anchor `case.rs` ports it.
pub(crate) fn camel_case(input: &str) -> String {
    let pascal = title_words(input).concat();
    let mut chars = pascal.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_lowercase().to_string() + chars.as_str()
    })
}

/// The JavaScript `snakeCase` of `@codama/nodes`.
pub(crate) fn snake_case(input: &str) -> String {
    title_words(input).join("_").to_ascii_lowercase()
}

/// The capitalized words of JavaScript `titleCase`. Only ASCII letters and digits are word
/// characters, and `SCREAMING_SNAKE` splits on `_` alone.
fn title_words(input: &str) -> Vec<String> {
    let capitalize = |word: &str| {
        let mut chars = word.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        })
    };
    let upper_or_digit = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    let mut parts = input.split('_');
    let head = parts.next().unwrap_or_default();
    let rest: Vec<&str> = parts.collect();
    let screaming = head.starts_with(|c: char| c.is_ascii_uppercase())
        && head.chars().all(upper_or_digit)
        && !rest.is_empty()
        && rest
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(upper_or_digit));
    if screaming {
        return input.split('_').map(capitalize).collect();
    }
    let mut spaced = String::with_capacity(input.len() * 2);
    for c in input.chars() {
        if c.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(c);
    }
    spaced
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(capitalize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules that `rejections` finds in one case of the fixture, and the rule that
    /// Shipstern's converter gave for it (none when it converts).
    fn case(name: &str) -> (BTreeSet<&'static str>, Option<String>) {
        let path = format!(
            "{}/tests/fixtures/frameworks/shipstern_rules.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let cases: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let json = cases[name]["idl"].to_string();
        let found = rejections(&json, &Idl::from_json(&json).unwrap()).unwrap();
        let expected = cases[name]["rule"].as_str().map(str::to_string);
        (found.into_iter().map(|r| r.rule).collect(), expected)
    }

    macro_rules! rule_cases {
        ($($name:ident),* $(,)?) => {$(
            #[test]
            fn $name() {
                let (found, expected) = case(stringify!($name));
                assert_eq!(found, expected.as_deref().into_iter().collect());
            }
        )*};
    }

    rule_cases!(
        clean,
        unsupported_spec,
        missing_discriminator,
        shank_origin,
        legacy_own_discriminator,
        legacy_missing_address,
        legacy_state,
        legacy_generic,
        enum_in_bytemuck,
        repr_rust_packed,
        padded_borsh_type,
        type_missing,
        empty_discriminator,
        type_not_struct,
        invalid_name,
        discriminator_argument,
        unrecognized_type,
        undefined_type,
        generic_args_missing,
        generic_type_missing,
        generic_enum,
        generic_arg_missing,
        invalid_array_length,
        variable_size_coption,
        fixed_type_too_large,
        name_collision,
        ambiguous_discriminator,
        duplicate_discriminator,
        conflicting_flattened_arguments,
        two_uses_do_not_flatten,
        coption_of_generic_struct,
        coption_in_generic_body,
        spec_public_key,
        spec_defined_string,
        legacy_usize,
        account_too_large,
        event_too_large,
        coption_of_account_type,
        generic_const_too_large,
        generic_missing_arg,
        fields_named_underscore,
        constant_undefined,
        pda_seed_counts_a_use,
        constant_counts_a_use,
        legacy_no_version,
        self_referential_generic,
        mutual_generics,
        generic_doubles_each_level,
        const_param_passed_as_type_arg,
        bare_array_length,
    );

    #[test]
    fn deep_link_chain_stops_at_the_nesting_limit() {
        // 20001 structs that each hold the next: the size walk must stop at 128 levels, as
        // the converter does, and so the coption has no fixed size.
        let types: Vec<String> = (0..=20_001)
            .map(|i| {
                let ty = if i == 20_001 {
                    r#""u8""#.to_string()
                } else {
                    format!(r#"{{"defined":{{"name":"T{}"}}}}"#, i + 1)
                };
                format!(r#"{{"name":"T{i}","type":{{"kind":"struct","fields":[{{"name":"a","type":{ty}}}]}}}}"#)
            })
            .collect();
        let json = format!(
            r#"{{"address":"11111111111111111111111111111111","metadata":{{"name":"t","version":"0.1.0","spec":"0.1.0"}},
            "instructions":[{{"name":"go","discriminator":[1],"accounts":[],"args":[{{"name":"v","type":{{"coption":{{"defined":{{"name":"T0"}}}}}}}}]}}],
            "types":[{}]}}"#,
            types.join(",")
        );
        let rules: Vec<&str> = rejections(&json, &Idl::from_json(&json).unwrap())
            .unwrap()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert_eq!(rules, ["VariableSizeCOption"]);
    }

    #[test]
    fn lines_follow_the_order_of_the_converter() {
        // The converter checks names before sizes, so it reports the account name first.
        let json = r#"{"address":"11111111111111111111111111111111","metadata":{"name":"t","version":"0.1.0","spec":"0.1.0"},
            "instructions":[{"name":"go","discriminator":[1],"accounts":[{"name":"1bad"}],"args":[]}],
            "types":[{"name":"P","type":{"kind":"struct","fields":[{"name":"v","type":{"coption":"string"}}]}}]}"#;
        let rules: Vec<&str> = rejections(json, &Idl::from_json(json).unwrap())
            .unwrap()
            .into_iter()
            .map(|r| r.rule)
            .collect();
        assert_eq!(rules, ["InvalidName", "VariableSizeCOption"]);
    }

    #[test]
    fn camel_case_matches_codama() {
        // The pairs of codama-from-anchor `case.rs`, from `@codama/nodes` 1.11.0.
        let cases = [
            ("amount_in", "amountIn"),
            ("SwapResult", "swapResult"),
            ("USDC", "uSDC"),
            ("MAX_FEE_BPS", "maxFeeBps"),
            ("v2_pool", "v2Pool"),
            ("__leading", "leading"),
        ];
        for (input, expected) in cases {
            assert_eq!(camel_case(input), expected, "{input}");
        }
        assert_eq!(snake_case("setAB"), "set_a_b");
    }
}
