//! Convert a Codama `RootNode`, the shape Shipstern's `include_shipstern_parser!` loads,
//! into the flat [`Idl`].

use crate::model::{
    Account, AccountItem, ArrayLen, Discriminator, EnumFields, EnumVariant, ErrorCode, Event,
    Field, Format, Idl, Instruction, Metadata, NamedDisc, Type, TypeDef, TypeDefTy,
};
use codama_nodes::{
    AccountNode, BytesEncoding, BytesValueNode, CountNode, DefaultValueStrategy, DefinedTypeNode,
    DiscriminatorNode, Endianness, EnumVariantTypeNode, ErrorNode, EventNode,
    InstructionAccountNode, InstructionInputValueNode, InstructionNode, IsSigner, NestedTypeNode,
    NestedTypeNodeTrait, NumberFormat, NumberTypeNode, RootNode, StructFieldTypeNode,
    StructTypeNode, TypeNode, TypeNodeTrait, ValueNode,
};

/// Convert a Codama root into the flat IDL.
#[must_use]
pub fn from_codama(root: RootNode) -> Idl {
    let program = root.program;
    let mut types: Vec<TypeDef> = program
        .defined_types
        .iter()
        .map(convert_defined_type)
        .collect();

    let accounts: Vec<NamedDisc> = program
        .accounts
        .iter()
        .map(|a| {
            let (disc, offset) = extract_account_discriminator(a);
            // Codama keeps account data inline; give it a `types` entry like Anchor does.
            types.push(account_as_type_def(a));
            NamedDisc {
                name: a.name.to_string(),
                discriminator: disc,
                discriminator_offset: offset,
                // A size discriminator is the account's length, so it doubles as its size.
                size: a
                    .size
                    .or(match a.discriminators.first() {
                        Some(DiscriminatorNode::Size(d)) => Some(d.size),
                        _ => None,
                    })
                    .and_then(|n| usize::try_from(n).ok()),
                payload_offset: numeric_constant_discriminator(a).then_some(0),
            }
        })
        .collect();

    let event_discs: Vec<Event> = program
        .events
        .iter()
        .map(|e| {
            types.push(event_as_type_def(e));
            let (discriminator, envelope_discriminator, envelope_payload_offset) =
                extract_event_parts(e);
            Event {
                name: e.name.to_string(),
                discriminator,
                envelope_discriminator,
                envelope_payload_offset,
            }
        })
        .collect();

    Idl {
        address: program.public_key,
        metadata: Metadata {
            name: program.name.to_string(),
            version: program.version,
            spec: String::new(),
        },
        instructions: program
            .instructions
            .iter()
            .map(convert_instruction)
            .collect(),
        accounts,
        events: event_discs,
        errors: program.errors.iter().map(convert_error).collect(),
        types,
        format: Format::Codama,
    }
}

fn convert_error(e: &ErrorNode) -> ErrorCode {
    ErrorCode {
        code: e.code,
        name: e.name.to_string(),
        msg: if e.message.is_empty() {
            None
        } else {
            Some(e.message.clone())
        },
    }
}

fn convert_instruction(ix: &InstructionNode) -> Instruction {
    let (discriminator, discriminator_offset) = extract_ix_discriminator(ix);
    let accounts = ix.accounts.iter().map(convert_ix_account).collect();
    // Omitted args are the discriminator, not data.
    let args = ix
        .arguments
        .iter()
        .filter(|a| a.default_value_strategy != Some(DefaultValueStrategy::Omitted))
        .map(|a| Field {
            name: a.name.to_string(),
            ty: convert_type(&a.r#type),
        })
        .collect();
    Instruction {
        name: ix.name.to_string(),
        discriminator,
        discriminator_offset,
        accounts,
        args,
    }
}

fn convert_ix_account(a: &InstructionAccountNode) -> AccountItem {
    AccountItem::Single(Account {
        name: a.name.to_string(),
        writable: a.is_writable,
        signer: matches!(a.is_signer, IsSigner::True | IsSigner::Either),
        optional: a.is_optional.unwrap_or(false),
        address: match a.default_value.as_ref() {
            Some(InstructionInputValueNode::PublicKeyValue(pk)) => Some(pk.public_key.clone()),
            _ => None,
        },
    })
}

fn convert_defined_type(dt: &DefinedTypeNode) -> TypeDef {
    TypeDef {
        name: dt.name.to_string(),
        serialization: default_serialization(),
        repr: None,
        generics: Vec::new(),
        ty: convert_type_def_ty(&dt.r#type),
    }
}

fn account_as_type_def(a: &AccountNode) -> TypeDef {
    let ty = match &a.data {
        NestedTypeNode::Value(st) => TypeDefTy::Struct {
            fields: struct_fields_skipping_omitted(st),
        },
        wrapped => TypeDefTy::Type {
            alias: Type::Generic(wrapper_kind(wrapped).to_string()),
        },
    };
    TypeDef {
        name: a.name.to_string(),
        serialization: default_serialization(),
        repr: None,
        generics: Vec::new(),
        ty,
    }
}

fn event_as_type_def(e: &EventNode) -> TypeDef {
    // A hidden prefix around the struct is the self-CPI envelope, read separately.
    let data = match e.data.as_ref() {
        TypeNode::HiddenPrefix(hp) => hp.r#type.as_ref(),
        other => other,
    };
    TypeDef {
        name: e.name.to_string(),
        serialization: default_serialization(),
        repr: None,
        generics: Vec::new(),
        ty: convert_type_def_ty(data),
    }
}

/// Name of the wrapper node, kept as a `Generic` so the wrapped layout reads as unresolved.
fn wrapper_kind<T: TypeNodeTrait>(n: &NestedTypeNode<T>) -> &'static str {
    match n {
        NestedTypeNode::FixedSize(_) => "fixedSizeTypeNode",
        NestedTypeNode::HiddenPrefix(_) => "hiddenPrefixTypeNode",
        NestedTypeNode::HiddenSuffix(_) => "hiddenSuffixTypeNode",
        NestedTypeNode::PostOffset(_) => "postOffsetTypeNode",
        NestedTypeNode::PreOffset(_) => "preOffsetTypeNode",
        NestedTypeNode::Sentinel(_) => "sentinelTypeNode",
        NestedTypeNode::SizePrefix(_) => "sizePrefixTypeNode",
        NestedTypeNode::Value(_) => "value",
    }
}

fn struct_fields_skipping_omitted(st: &StructTypeNode) -> Vec<Field> {
    st.fields
        .iter()
        .filter(|f| f.default_value_strategy != Some(DefaultValueStrategy::Omitted))
        .map(|f| Field {
            name: f.name.to_string(),
            ty: convert_type(&f.r#type),
        })
        .collect()
}

fn convert_type_def_ty(t: &TypeNode) -> TypeDefTy {
    match t {
        TypeNode::Struct(st) => TypeDefTy::Struct {
            fields: struct_fields_skipping_omitted(st),
        },
        TypeNode::Enum(en) => TypeDefTy::Enum {
            variants: en.variants.iter().map(convert_enum_variant).collect(),
            tag: enum_tag(&en.size),
        },
        other => TypeDefTy::Type {
            alias: convert_type(other),
        },
    }
}

fn convert_enum_variant(v: &EnumVariantTypeNode) -> EnumVariant {
    match v {
        EnumVariantTypeNode::Empty(e) => EnumVariant {
            name: e.name.to_string(),
            fields: EnumFields::default(),
            discriminator: e.discriminator,
        },
        EnumVariantTypeNode::Struct(s) => EnumVariant {
            name: s.name.to_string(),
            fields: match &s.r#struct {
                NestedTypeNode::Value(st) => EnumFields::Named(struct_fields_skipping_omitted(st)),
                wrapped => {
                    EnumFields::Tuple(vec![Type::Generic(wrapper_kind(wrapped).to_string())])
                }
            },
            discriminator: s.discriminator,
        },
        EnumVariantTypeNode::Tuple(t) => EnumVariant {
            name: t.name.to_string(),
            fields: match &t.tuple {
                NestedTypeNode::Value(items) => {
                    EnumFields::Tuple(items.items.iter().map(convert_type).collect())
                }
                wrapped => {
                    EnumFields::Tuple(vec![Type::Generic(wrapper_kind(wrapped).to_string())])
                }
            },
            discriminator: t.discriminator,
        },
    }
}

/// `None` for Borsh's `u8` tag, so it compares equal to an Anchor enum.
fn enum_tag(size: &NestedTypeNode<NumberTypeNode>) -> Option<Type> {
    match size {
        NestedTypeNode::Value(n) if n.format == NumberFormat::U8 => None,
        NestedTypeNode::Value(n) => Some(number_type(n)),
        _ => Some(Type::Generic("enumSize".to_string())),
    }
}

fn default_serialization() -> String {
    "borsh".to_string()
}

/// Map a Codama type to the flat model. Encodings with no Borsh equivalent (big-endian
/// numbers, non-`u32` length prefixes, remainder or zeroable options) become `Generic`.
fn convert_type(t: &TypeNode) -> Type {
    match t {
        TypeNode::SizePrefix(sp) if is_borsh_len_prefix(&sp.prefix) => match sp.r#type.as_ref() {
            TypeNode::String(_) => Type::String,
            TypeNode::Bytes(_) => Type::Bytes,
            _ => Type::Generic("sizePrefixTypeNode".to_string()),
        },
        TypeNode::FixedSize(fs) => match fs.r#type.as_ref() {
            TypeNode::Bytes(_) | TypeNode::String(_) => {
                Type::Array(Box::new(Type::U8), ArrayLen::Value(fs.size))
            }
            _ => Type::Generic("fixedSizeTypeNode".to_string()),
        },
        TypeNode::Array(arr) => match arr.count.as_ref() {
            CountNode::Fixed(fc) => match usize::try_from(fc.value) {
                Ok(len) => Type::Array(Box::new(convert_type(&arr.item)), ArrayLen::Value(len)),
                Err(_) => Type::Generic("fixedCountNode".to_string()),
            },
            CountNode::Prefixed(p) if is_borsh_len_prefix(&p.prefix) => {
                Type::Vec(Box::new(convert_type(&arr.item)))
            }
            CountNode::Prefixed(_) => Type::Generic("prefixedCountNode".to_string()),
            CountNode::Remainder(_) => Type::Generic("remainderCountNode".to_string()),
        },
        TypeNode::Boolean(_) => Type::Bool,
        TypeNode::Number(n) => number_type(n),
        TypeNode::Amount(a) => number_type(a.number.get_nested_type_node()),
        TypeNode::SolAmount(a) => number_type(a.number.get_nested_type_node()),
        TypeNode::DateTime(d) => number_type(d.number.get_nested_type_node()),
        TypeNode::PublicKey(_) => Type::Pubkey,
        TypeNode::Option(o) => {
            let prefix = o.prefix.get_nested_type_node();
            let le = prefix.endian == Endianness::Le;
            match (prefix.format, o.fixed.unwrap_or(false)) {
                (NumberFormat::U8, false) if le => Type::Option(Box::new(convert_type(&o.item))),
                (NumberFormat::U32, true) if le => Type::COption(Box::new(convert_type(&o.item))),
                _ => Type::Generic("optionTypeNode".to_string()),
            }
        }
        TypeNode::Link(link) => Type::Defined {
            name: link.name.to_string(),
            generics: vec![],
        },
        TypeNode::Map(m) => match m.count.as_ref() {
            CountNode::Prefixed(p) if is_borsh_len_prefix(&p.prefix) => Type::Map(
                Box::new(convert_type(m.key.as_ref())),
                Box::new(convert_type(m.value.as_ref())),
            ),
            _ => Type::Generic("mapTypeNode".to_string()),
        },
        TypeNode::Set(st) => match st.count.as_ref() {
            CountNode::Prefixed(p) if is_borsh_len_prefix(&p.prefix) => {
                Type::Set(Box::new(convert_type(st.item.as_ref())))
            }
            _ => Type::Generic("setTypeNode".to_string()),
        },
        TypeNode::Tuple(t) => Type::Tuple(t.items.iter().map(convert_type).collect()),
        // Inline structs and enums as field types have no flat equivalent yet. Real IDLs
        // almost always link to a defined type instead.
        TypeNode::Struct(_) => Type::Generic("structTypeNode".to_string()),
        TypeNode::Enum(_) => Type::Generic("enumTypeNode".to_string()),
        // Bare strings and bytes run to the end of the data; Borsh has no such encoding.
        TypeNode::String(_) => Type::Generic("stringTypeNode".to_string()),
        TypeNode::Bytes(_) => Type::Generic("bytesTypeNode".to_string()),
        TypeNode::SizePrefix(_) => Type::Generic("sizePrefixTypeNode".to_string()),
        TypeNode::HiddenPrefix(_) => Type::Generic("hiddenPrefixTypeNode".to_string()),
        TypeNode::HiddenSuffix(_) => Type::Generic("hiddenSuffixTypeNode".to_string()),
        TypeNode::PreOffset(_) => Type::Generic("preOffsetTypeNode".to_string()),
        TypeNode::PostOffset(_) => Type::Generic("postOffsetTypeNode".to_string()),
        TypeNode::Sentinel(_) => Type::Generic("sentinelTypeNode".to_string()),
        TypeNode::RemainderOption(_) => Type::Generic("remainderOptionTypeNode".to_string()),
        TypeNode::ZeroableOption(_) => Type::Generic("zeroableOptionTypeNode".to_string()),
    }
}

/// Borsh writes every length as a little-endian `u32`.
fn is_borsh_len_prefix(prefix: &NestedTypeNode<NumberTypeNode>) -> bool {
    matches!(
        prefix,
        NestedTypeNode::Value(n) if n.format == NumberFormat::U32 && n.endian == Endianness::Le
    )
}

fn number_type(n: &NumberTypeNode) -> Type {
    let ty = match n.format {
        NumberFormat::U8 => Type::U8,
        NumberFormat::U16 => Type::U16,
        NumberFormat::U32 => Type::U32,
        NumberFormat::U64 => Type::U64,
        NumberFormat::U128 => Type::U128,
        NumberFormat::I8 => Type::I8,
        NumberFormat::I16 => Type::I16,
        NumberFormat::I32 => Type::I32,
        NumberFormat::I64 => Type::I64,
        NumberFormat::I128 => Type::I128,
        NumberFormat::F32 => Type::F32,
        NumberFormat::F64 => Type::F64,
        // Variable-length (1 to 3 bytes); not a Borsh scalar.
        NumberFormat::ShortU16 => return Type::Generic("shortu16".to_string()),
    };
    // Single bytes have no byte order.
    if n.endian == Endianness::Be && !matches!(ty, Type::U8 | Type::I8) {
        return Type::Generic(format!("{ty:?}be").to_lowercase());
    }
    ty
}

fn extract_ix_discriminator(ix: &InstructionNode) -> (Discriminator, usize) {
    extract_discriminator(&ix.discriminators, |name| {
        let arg = ix.arguments.iter().find(|a| a.name.as_str() == name)?;
        match arg.default_value.as_ref().as_ref()? {
            InstructionInputValueNode::BytesValue(b) => Some(decode_bytes(b)),
            InstructionInputValueNode::NumberValue(n) => number_to_bytes(n, &arg.r#type),
            InstructionInputValueNode::ConstantValue(c) => {
                value_to_bytes(c.value.as_ref(), c.r#type.as_ref())
            }
            _ => None,
        }
    })
}

/// A numeric constant discriminator is a check on a byte the body re-reads as its first
/// field, so Shipstern's account parser decodes from offset 0 (SPL Governance's `accountType`).
fn numeric_constant_discriminator(a: &AccountNode) -> bool {
    matches!(
        a.discriminators.first(),
        Some(DiscriminatorNode::Constant(c)) if matches!(c.constant.value.as_ref(), ValueNode::Number(_))
    )
}

fn extract_account_discriminator(a: &AccountNode) -> (Discriminator, usize) {
    let st = a.data.get_nested_type_node();
    extract_discriminator(&a.discriminators, |name| {
        let field = st.fields.iter().find(|f| f.name.as_str() == name)?;
        field_default_bytes(field)
    })
}

fn extract_event_discriminator(e: &EventNode) -> (Discriminator, usize) {
    let fields: &[StructFieldTypeNode] = match e.data.as_ref() {
        TypeNode::Struct(st) => st.fields.as_slice(),
        TypeNode::HiddenPrefix(hp) => match hp.r#type.as_ref() {
            TypeNode::Struct(st) => st.fields.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    extract_discriminator(&e.discriminators, |name| {
        let field = fields.iter().find(|f| f.name.as_str() == name)?;
        field_default_bytes(field)
    })
}

/// The event discriminator and the self-CPI envelope, by Shipstern's rule
/// (`proc-macro/src/parse.rs`): Codama has no node for the envelope, so in an all-constant
/// chain sorted by offset, one constant at offset 0 is the tag and the next one starts the
/// event. The `hiddenPrefix` around the data only hides those bytes from the struct.
fn extract_event_parts(e: &EventNode) -> (Discriminator, Discriminator, Option<usize>) {
    let mut constants: Vec<(usize, Vec<u8>)> = e
        .discriminators
        .iter()
        .filter_map(|d| match d {
            DiscriminatorNode::Constant(c) => Some((
                usize::try_from(c.offset).ok()?,
                value_to_bytes(c.constant.value.as_ref(), c.constant.r#type.as_ref())?,
            )),
            _ => None,
        })
        .collect();
    // Shipstern accepts an envelope only in an all-constant chain.
    if constants.len() != e.discriminators.len() {
        constants.clear();
    }
    constants.sort_by_key(|(offset, _)| *offset);
    match constants.as_slice() {
        [(0, tag), (offset, disc), ..] if *offset > 0 => (disc.clone(), tag.clone(), Some(*offset)),
        _ => (extract_event_discriminator(e).0, Vec::new(), None),
    }
}

fn field_default_bytes(field: &StructFieldTypeNode) -> Option<Vec<u8>> {
    match field.default_value.as_ref().as_ref()? {
        ValueNode::Bytes(b) => Some(decode_bytes(b)),
        ValueNode::Number(n) => number_to_bytes(n, &field.r#type),
        ValueNode::Constant(c) => value_to_bytes(c.value.as_ref(), c.r#type.as_ref()),
        _ => None,
    }
}

/// The discriminator bytes and the offset they start at.
fn extract_discriminator(
    discs: &[DiscriminatorNode],
    resolve_field: impl Fn(&str) -> Option<Vec<u8>>,
) -> (Discriminator, usize) {
    let Some(d) = discs.first() else {
        return (Vec::new(), 0);
    };
    match d {
        DiscriminatorNode::Constant(c) => (
            value_to_bytes(c.constant.value.as_ref(), c.constant.r#type.as_ref())
                .unwrap_or_default(),
            usize::try_from(c.offset).unwrap_or(usize::MAX),
        ),
        DiscriminatorNode::Field(f) => (
            resolve_field(f.name.as_str()).unwrap_or_default(),
            usize::try_from(f.offset).unwrap_or(usize::MAX),
        ),
        DiscriminatorNode::Size(_) => (Vec::new(), 0),
    }
}

fn value_to_bytes(v: &ValueNode, ty: &TypeNode) -> Option<Vec<u8>> {
    match v {
        ValueNode::Bytes(b) => Some(decode_bytes(b)),
        ValueNode::Number(n) => number_to_bytes(n, ty),
        _ => None,
    }
}

/// Encode a numeric discriminator at its declared width and byte order, so a `u32` tag of 2
/// is four bytes. `None` when the type isn't a number or the value doesn't fit.
fn number_to_bytes(n: &codama_nodes::NumberValueNode, ty: &TypeNode) -> Option<Vec<u8>> {
    let number = match ty {
        TypeNode::Number(nt) => nt,
        TypeNode::Amount(a) => a.number.get_nested_type_node(),
        TypeNode::SolAmount(a) => a.number.get_nested_type_node(),
        TypeNode::DateTime(d) => d.number.get_nested_type_node(),
        _ => return None,
    };
    let width = match number.format {
        NumberFormat::U8 | NumberFormat::I8 => 1,
        NumberFormat::U16 | NumberFormat::I16 => 2,
        NumberFormat::U32 | NumberFormat::I32 => 4,
        NumberFormat::U64 | NumberFormat::I64 => 8,
        NumberFormat::U128 | NumberFormat::I128 => 16,
        _ => return None,
    };
    let value: i128 = match n.number {
        codama_nodes::Number::UnsignedInteger(v) => i128::from(v),
        codama_nodes::Number::SignedInteger(v) => i128::from(v),
        codama_nodes::Number::Float(_) => return None,
    };
    let signed = matches!(
        number.format,
        NumberFormat::I8
            | NumberFormat::I16
            | NumberFormat::I32
            | NumberFormat::I64
            | NumberFormat::I128
    );
    let bits = width * 8;
    let fits = if signed {
        bits >= 128 || (-(1i128 << (bits - 1))..(1i128 << (bits - 1))).contains(&value)
    } else {
        value >= 0 && (bits >= 128 || value < (1i128 << bits))
    };
    if !fits {
        return None;
    }
    let mut bytes = value.to_le_bytes()[..width].to_vec();
    if number.endian == Endianness::Be {
        bytes.reverse();
    }
    Some(bytes)
}

/// The bytes behind a Codama [`BytesValueNode`], whatever its encoding, so a discriminator
/// written in base16 in one version and base58 in the next compares equal. Malformed input
/// falls back to the raw string bytes.
#[must_use]
pub fn decode_bytes(b: &BytesValueNode) -> Vec<u8> {
    match b.encoding {
        BytesEncoding::Base16 => decode_hex(&b.data),
        BytesEncoding::Base58 => bs58::decode(&b.data)
            .into_vec()
            .unwrap_or_else(|_| b.data.as_bytes().to_vec()),
        BytesEncoding::Base64 => {
            use base64::{engine::general_purpose::STANDARD, Engine};
            STANDARD
                .decode(&b.data)
                .unwrap_or_else(|_| b.data.as_bytes().to_vec())
        }
        BytesEncoding::Utf8 => b.data.as_bytes().to_vec(),
    }
}

/// Decode `data` under a Codama encoding name: `base16`, `base58`, `base64` or `utf8`.
#[must_use]
pub fn decode_encoded_bytes(encoding: &str, data: &str) -> Vec<u8> {
    let node = match encoding {
        "base16" => BytesValueNode::base16(data),
        "base58" => BytesValueNode::base58(data),
        "base64" => BytesValueNode::base64(data),
        _ => BytesValueNode::utf8(data),
    };
    decode_bytes(&node)
}

/// Works on bytes, not string slices, so non-ASCII input can't split a character.
fn decode_hex(s: &str) -> Vec<u8> {
    let raw = s.trim().as_bytes();
    let digits: Vec<u8> = std::iter::repeat_n(b'0', raw.len() % 2)
        .chain(raw.iter().copied())
        .collect();
    let decoded: Option<Vec<u8>> = digits
        .chunks(2)
        .map(|pair| {
            let hex = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(hex, 16).ok()
        })
        .collect();
    decoded.unwrap_or_else(|| raw.to_vec())
}

/// Turn string error codes (`"code": "6000"`) into numbers, as Shipstern does.
pub fn fix_string_error_codes(value: &mut serde_json::Value) {
    let Some(errors) = value
        .get_mut("program")
        .and_then(|p| p.get_mut("errors"))
        .and_then(|e| e.as_array_mut())
    else {
        return;
    };
    for error in errors {
        if let Some(code) = error.get_mut("code") {
            if let Some(s) = code.as_str().and_then(|s| s.parse::<u64>().ok()) {
                *code = serde_json::Value::Number(s.into());
            }
        }
    }
}
