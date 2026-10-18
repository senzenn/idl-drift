//! Convert a Codama `RootNode`, the shape Shipstern's `include_shipstern_parser!` loads,
//! into the flat [`Idl`].

use crate::model::{
    default_serialization, discriminator_end, Account, AccountItem, ArrayLen, Count, Discriminator,
    EnumFields, EnumVariant, ErrorCode, Event, Field, Format, Idl, Instruction, Metadata,
    NamedDisc, OptionTag, OptionalAccounts, Pda, Seed, Type, TypeDef, TypeDefTy,
};
use codama_nodes::{
    AccountNode, BytesEncoding, BytesValueNode, CamelCaseString, ConstantPdaSeedValue, CountNode,
    DefaultValueStrategy, DefinedTypeNode, DiscriminatorNode, Endianness, EnumVariantTypeNode,
    ErrorNode, EventNode, InstructionAccountNode, InstructionArgumentNode,
    InstructionInputValueNode, InstructionNode, IsSigner, NestedTypeNode, NestedTypeNodeTrait,
    NumberFormat, NumberTypeNode, PdaNode, PdaSeedNode, PdaSeedValueValue, PdaValueNode,
    PdaValuePda, PdaValueProgramId, RootNode, StructFieldTypeNode, TypeNode, TypeNodeTrait,
    ValueNode,
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
            let discs = extract_account_discriminators(a);
            // Codama keeps account data inline; give it a `types` entry like Anchor does.
            types.push(account_as_type_def(a, discs.end()));
            NamedDisc {
                name: a.name.to_string(),
                discriminator: discs.first,
                discriminator_offset: discs.offset,
                extra_discriminators: discs.extra,
                // A size discriminator is the account's length, so it doubles as its size.
                size: a
                    .size
                    .or(match a.discriminators.first() {
                        Some(DiscriminatorNode::Size(d)) => Some(d.size),
                        _ => None,
                    })
                    .and_then(|n| usize::try_from(n).ok()),
                payload_offset: numeric_constant_discriminator(a).then_some(0),
                size_discriminator: size_discriminator(&a.discriminators),
            }
        })
        .collect();

    let events: Vec<Event> = program
        .events
        .iter()
        .map(|e| {
            let parts = extract_event_parts(e);
            let event = Event {
                name: e.name.to_string(),
                discriminator: parts.discriminator,
                discriminator_offset: parts.discriminator_offset,
                extra_discriminators: parts.extra_discriminators,
                envelope_discriminator: parts.envelope,
                envelope_payload_offset: parts.envelope_payload_offset,
            };
            types.push(event_as_type_def(e, &event));
            event
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
            .map(|ix| convert_instruction(ix, &program.pdas))
            .collect(),
        accounts,
        events,
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

fn convert_instruction(ix: &InstructionNode, pdas: &[PdaNode]) -> Instruction {
    let discs = extract_ix_discriminators(ix);
    let accounts = ix
        .accounts
        .iter()
        .map(|a| convert_ix_account(a, ix, pdas))
        .collect();
    let covered = covered_fields(
        ix.arguments.iter().map(|a| {
            let omitted = a.default_value_strategy == Some(DefaultValueStrategy::Omitted);
            (omitted, a.r#type.as_ref(), arg_default_bytes(a))
        }),
        discs.end(),
    );
    let args = ix
        .arguments
        .iter()
        .skip(covered)
        .map(|a| Field {
            name: a.name.to_string(),
            ty: convert_type(&a.r#type),
        })
        .collect();
    Instruction {
        name: ix.name.to_string(),
        discriminator: discs.first,
        discriminator_offset: discs.offset,
        extra_discriminators: discs.extra,
        size_discriminator: any_size_discriminator(&ix.discriminators),
        accounts,
        args,
        optional_accounts: match ix.optional_account_strategy.unwrap_or_default() {
            codama_nodes::OptionalAccountStrategy::ProgramId => OptionalAccounts::ProgramId,
            codama_nodes::OptionalAccountStrategy::Omitted => OptionalAccounts::Omitted,
        },
    }
}

/// Shipstern reads only the first discriminator of an account.
fn size_discriminator(discs: &[DiscriminatorNode]) -> Option<usize> {
    match discs.first() {
        Some(DiscriminatorNode::Size(d)) => usize::try_from(d.size).ok(),
        _ => None,
    }
}

/// Codama requires every discriminator of an instruction to match, so a size node counts at
/// any position. A program can take one tag at two data lengths.
fn any_size_discriminator(discs: &[DiscriminatorNode]) -> Option<usize> {
    discs.iter().find_map(|d| match d {
        DiscriminatorNode::Size(d) => usize::try_from(d.size).ok(),
        _ => None,
    })
}

fn convert_ix_account(
    a: &InstructionAccountNode,
    ix: &InstructionNode,
    pdas: &[PdaNode],
) -> AccountItem {
    let address = match a.default_value.as_ref() {
        Some(InstructionInputValueNode::PublicKeyValue(pk)) => Some(pk.public_key.clone()),
        _ => None,
    };
    AccountItem::Single(Account {
        name: a.name.to_string(),
        writable: a.is_writable,
        signer: matches!(a.is_signer, IsSigner::True | IsSigner::Either),
        may_sign: matches!(a.is_signer, IsSigner::Either),
        optional: a.is_optional.unwrap_or(false),
        address_is_default: address.is_some(),
        address,
        pda: match a.default_value.as_ref() {
            Some(InstructionInputValueNode::PdaValue(v)) => convert_pda(v, ix, pdas),
            _ => None,
        },
    })
}

/// The seeds of a `pdaValueNode`. A link to a PDA of another program is not followed.
fn convert_pda(v: &PdaValueNode, ix: &InstructionNode, pdas: &[PdaNode]) -> Option<Pda> {
    let node = match v.pda.as_ref() {
        PdaValuePda::Pda(node) => node,
        PdaValuePda::PdaLink(link) if link.program.is_none() => {
            pdas.iter().find(|p| p.name == link.name)?
        }
        PdaValuePda::PdaLink(_) => return None,
    };
    let seeds = node
        .seeds
        .iter()
        .map(|seed| match seed {
            PdaSeedNode::Constant(c) => const_seed(&c.r#type, &c.value),
            PdaSeedNode::Variable(var) => {
                let value = v.seeds.iter().find(|s| s.name == var.name);
                match value.map(|s| s.value.as_ref()) {
                    Some(PdaSeedValueValue::Account(account)) => {
                        Seed::Account(account.name.to_string())
                    }
                    Some(PdaSeedValueValue::Argument(arg))
                        if arg_seed_fits(&var.r#type, ix, &arg.name) =>
                    {
                        Seed::Arg(arg.name.to_string())
                    }
                    _ => Seed::Other,
                }
            }
        })
        .collect();
    let program = match v.program_id.as_ref() {
        Some(PdaValueProgramId::Account(account)) => Some(Seed::Account(account.name.to_string())),
        Some(PdaValueProgramId::Argument(arg)) => Some(Seed::Arg(arg.name.to_string())),
        None => node.program_id.as_deref().map(key_seed),
    };
    Some(Pda { seeds, program })
}

fn const_seed(ty: &TypeNode, value: &ConstantPdaSeedValue) -> Seed {
    let bytes = match (ty, value) {
        (TypeNode::Bytes(_), ConstantPdaSeedValue::Bytes(b)) => Some(decode_bytes(b)),
        (TypeNode::String(s), ConstantPdaSeedValue::String(v))
            if s.encoding == BytesEncoding::Utf8 =>
        {
            Some(v.string.as_bytes().to_vec())
        }
        (TypeNode::PublicKey(_), ConstantPdaSeedValue::PublicKey(k)) => {
            return key_seed(&k.public_key)
        }
        (_, ConstantPdaSeedValue::Number(n)) => number_to_bytes(n, ty),
        _ => None,
    };
    bytes.map_or(Seed::Other, Seed::Const)
}

fn key_seed(key: &str) -> Seed {
    match bs58::decode(key).into_vec() {
        Ok(bytes) if bytes.len() == 32 => Seed::Const(bytes),
        _ => Seed::Other,
    }
}

/// An argument seed is the argument's own bytes: little-endian at the argument's width, a raw
/// key, or raw string or byte data. Only seed types that encode the value that way fit.
fn arg_seed_fits(seed: &TypeNode, ix: &InstructionNode, name: &CamelCaseString) -> bool {
    let Some(arg) = ix.arguments.iter().find(|a| a.name == *name) else {
        return false;
    };
    match (seed, arg.r#type.as_ref()) {
        (TypeNode::Number(s), TypeNode::Number(a)) => {
            s.format == a.format
                && s.format != NumberFormat::ShortU16
                && s.endian == Endianness::Le
                && a.endian == Endianness::Le
        }
        (TypeNode::PublicKey(_), TypeNode::PublicKey(_)) | (TypeNode::Bytes(_), _) => true,
        (TypeNode::String(s), _) => s.encoding == BytesEncoding::Utf8,
        (TypeNode::FixedSize(s), _) => matches!(s.r#type.as_ref(), TypeNode::Bytes(_)),
        _ => false,
    }
}

/// Codama has no zero-copy types or generics, so every type is plain Borsh.
fn borsh_type_def(name: String, ty: TypeDefTy) -> TypeDef {
    TypeDef {
        name,
        serialization: default_serialization(),
        repr: None,
        generics: Vec::new(),
        ty,
    }
}

fn convert_defined_type(dt: &DefinedTypeNode) -> TypeDef {
    borsh_type_def(dt.name.to_string(), convert_type_def_ty(&dt.r#type))
}

/// `disc_end` is where the discriminator bytes end, which the leading omitted fields fill.
fn account_as_type_def(a: &AccountNode, disc_end: usize) -> TypeDef {
    let ty = match &a.data {
        NestedTypeNode::Value(st) => TypeDefTy::Struct {
            fields: struct_fields(&st.fields, disc_end),
        },
        wrapped => TypeDefTy::Type {
            alias: unresolved(wrapper_kind(wrapped), wrapped),
        },
    };
    borsh_type_def(a.name.to_string(), ty)
}

fn event_as_type_def(e: &EventNode, event: &Event) -> TypeDef {
    // A hidden prefix around the struct holds the event's discriminators, read separately, so
    // no field of the struct holds them.
    let (data, disc_end) = match e.data.as_ref() {
        TypeNode::HiddenPrefix(hp) => (hp.r#type.as_ref(), 0),
        other => (
            other,
            event
                .envelope_payload_offset
                .unwrap_or(0)
                .saturating_add(event.payload_start()),
        ),
    };
    let ty = match data {
        TypeNode::Struct(st) => TypeDefTy::Struct {
            fields: struct_fields(&st.fields, disc_end),
        },
        other => convert_type_def_ty(other),
    };
    borsh_type_def(e.name.to_string(), ty)
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

/// The fields after the omitted ones that fill the first `disc_end` bytes. Codama keeps an
/// omitted field in the data, so every other field stays.
fn struct_fields(fields: &[StructFieldTypeNode], disc_end: usize) -> Vec<Field> {
    let covered = covered_fields(
        fields.iter().map(|f| {
            let omitted = f.default_value_strategy == Some(DefaultValueStrategy::Omitted);
            (omitted, f.r#type.as_ref(), field_default_bytes(f))
        }),
        disc_end,
    );
    fields
        .iter()
        .skip(covered)
        .map(|f| Field {
            name: f.name.to_string(),
            ty: convert_type(&f.r#type),
        })
        .collect()
}

/// How many leading fields lie in the first `disc_end` bytes, which the discriminators hold.
/// Each is `(omitted, type, default bytes)`. Only an omitted field of a known size counts.
fn covered_fields<'a>(
    fields: impl Iterator<Item = (bool, &'a TypeNode, Option<Vec<u8>>)>,
    disc_end: usize,
) -> usize {
    let mut end = 0usize;
    let mut count = 0;
    for (omitted, ty, default) in fields {
        // A bare `bytesTypeNode` has no fixed size, so its default gives the length.
        let size = fixed_size(ty).or_else(|| default.map(|d| d.len()));
        match size.and_then(|s| end.checked_add(s)) {
            Some(next) if omitted && next <= disc_end => {
                end = next;
                count += 1;
            }
            _ => break,
        }
    }
    count
}

/// The byte size of a number or a fixed-size type; `None` for any other type.
fn fixed_size(ty: &TypeNode) -> Option<usize> {
    match ty {
        TypeNode::Number(n) => number_width(n.format),
        TypeNode::FixedSize(fs) => Some(fs.size),
        _ => None,
    }
}

fn convert_type_def_ty(t: &TypeNode) -> TypeDefTy {
    match t {
        TypeNode::Struct(st) => TypeDefTy::Struct {
            fields: struct_fields(&st.fields, 0),
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
                NestedTypeNode::Value(st) => EnumFields::Named(struct_fields(&st.fields, 0)),
                wrapped => EnumFields::Tuple(vec![unresolved(wrapper_kind(wrapped), wrapped)]),
            },
            discriminator: s.discriminator,
        },
        EnumVariantTypeNode::Tuple(t) => EnumVariant {
            name: t.name.to_string(),
            fields: match &t.tuple {
                NestedTypeNode::Value(items) => {
                    EnumFields::Tuple(items.items.iter().map(convert_type).collect())
                }
                wrapped => EnumFields::Tuple(vec![unresolved(wrapper_kind(wrapped), wrapped)]),
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
        wrapped => Some(unresolved("enumSize", wrapped)),
    }
}

/// Map a Codama type to the flat model. Borsh encodings keep their Anchor types, so the
/// two formats compare equal. Other encodings get Codama-only types; the rest are `Generic`.
fn convert_type(t: &TypeNode) -> Type {
    match convert_type_node(t) {
        Type::Generic(kind) => unresolved(&kind, t),
        ty => ty,
    }
}

/// A node that idl-drift cannot read, as `Generic("<kind>#<sha256>")`. The hash of the node
/// without its docs makes a changed node compare unequal to the old one.
fn unresolved(kind: &str, node: &impl serde::Serialize) -> Type {
    use sha2::{Digest, Sha256};
    let mut value = serde_json::to_value(node).unwrap_or_default();
    strip_docs(&mut value);
    // A CI gate trusts this hash, so it must resist a crafted collision: SHA-256, not a fast hash.
    // serde_json sorts object keys, so the same node gives the same text in every build.
    let hash = Sha256::digest(value.to_string().as_bytes());
    Type::Generic(format!("{kind}#{hash:x}"))
}

fn strip_docs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("docs");
            map.values_mut().for_each(strip_docs);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_docs),
        _ => {}
    }
}

fn convert_type_node(t: &TypeNode) -> Type {
    match t {
        TypeNode::SizePrefix(sp) => match (sp.r#type.as_ref(), is_borsh_len_prefix(&sp.prefix)) {
            (TypeNode::String(_), true) => Type::String,
            (TypeNode::Bytes(_), true) => Type::Bytes,
            (TypeNode::String(_), false) => counted(Type::String, &sp.prefix),
            (TypeNode::Bytes(_), false) => counted(Type::Bytes, &sp.prefix),
            // Around a struct the prefix sets a byte window, which the model cannot express.
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
            count => collection(Type::Vec(Box::new(convert_type(&arr.item))), count),
        },
        TypeNode::Boolean(_) => Type::Bool,
        TypeNode::Number(n) => number_type(n),
        TypeNode::Amount(a) => number_type(a.number.get_nested_type_node()),
        TypeNode::SolAmount(a) => number_type(a.number.get_nested_type_node()),
        TypeNode::DateTime(d) => number_type(d.number.get_nested_type_node()),
        TypeNode::PublicKey(_) => Type::Pubkey,
        TypeNode::Option(o) => {
            let item = Box::new(convert_type(&o.item));
            let fixed = o.fixed.unwrap_or(false);
            match (prefix_type(&o.prefix), fixed) {
                (Some(Type::U8), false) => Type::Option(item),
                (Some(Type::U32), true) => Type::COption(item),
                (Some(ty), fixed) => Type::CodamaOption {
                    item,
                    tag: OptionTag::Prefix {
                        ty: Box::new(ty),
                        fixed,
                    },
                },
                (None, _) => Type::Generic("optionTypeNode".to_string()),
            }
        }
        // A custom zero value is rare, and a wrong guess at it would misread the item.
        TypeNode::ZeroableOption(z) if z.zero_value.is_none() => Type::CodamaOption {
            item: Box::new(convert_type(&z.item)),
            tag: OptionTag::Zeroable,
        },
        TypeNode::RemainderOption(o) => Type::CodamaOption {
            item: Box::new(convert_type(&o.item)),
            tag: OptionTag::Remainder,
        },
        TypeNode::Link(link) => Type::Defined {
            name: link.name.to_string(),
            generics: vec![],
        },
        TypeNode::Map(m) => collection(
            Type::Map(
                Box::new(convert_type(m.key.as_ref())),
                Box::new(convert_type(m.value.as_ref())),
            ),
            &m.count,
        ),
        TypeNode::Set(st) => collection(
            Type::Set(Box::new(convert_type(st.item.as_ref()))),
            &st.count,
        ),
        TypeNode::Tuple(t) => Type::Tuple(t.items.iter().map(convert_type).collect()),
        TypeNode::Struct(st) => Type::Struct(struct_fields(&st.fields, 0)),
        // Shipstern cannot read an inline enum, and real IDLs link to a defined type.
        TypeNode::Enum(_) => Type::Generic("enumTypeNode".to_string()),
        // Bare strings and bytes run to the end of the data.
        TypeNode::String(_) => Type::Counted {
            ty: Box::new(Type::String),
            count: Count::Remainder,
        },
        TypeNode::Bytes(_) => Type::Counted {
            ty: Box::new(Type::Bytes),
            count: Count::Remainder,
        },
        TypeNode::HiddenPrefix(_) => Type::Generic("hiddenPrefixTypeNode".to_string()),
        TypeNode::HiddenSuffix(_) => Type::Generic("hiddenSuffixTypeNode".to_string()),
        TypeNode::PreOffset(_) => Type::Generic("preOffsetTypeNode".to_string()),
        TypeNode::PostOffset(_) => Type::Generic("postOffsetTypeNode".to_string()),
        TypeNode::Sentinel(_) => Type::Generic("sentinelTypeNode".to_string()),
        TypeNode::ZeroableOption(_) => Type::Generic("zeroableOptionTypeNode".to_string()),
    }
}

/// `ty` (a `Bytes` or `String`) with a byte length in `prefix` instead of Borsh's `u32`.
fn counted(ty: Type, prefix: &NestedTypeNode<NumberTypeNode>) -> Type {
    match prefix_type(prefix) {
        Some(p) => Type::Counted {
            ty: Box::new(ty),
            count: Count::Prefix(Box::new(p)),
        },
        None => Type::Generic("sizePrefixTypeNode".to_string()),
    }
}

/// `ty` (a `Vec`, `Map` or `Set`) with its item count from `count`. Borsh's `u32` prefix keeps
/// the plain type.
fn collection(ty: Type, count: &CountNode) -> Type {
    let count = match count {
        CountNode::Prefixed(p) if is_borsh_len_prefix(&p.prefix) => return ty,
        CountNode::Prefixed(p) => match prefix_type(&p.prefix) {
            Some(prefix) => Count::Prefix(Box::new(prefix)),
            None => return Type::Generic("prefixedCountNode".to_string()),
        },
        CountNode::Remainder(_) => Count::Remainder,
        CountNode::Fixed(_) => return Type::Generic("fixedCountNode".to_string()),
    };
    Type::Counted {
        ty: Box::new(ty),
        count,
    }
}

/// The integer type of a length or tag prefix. A float or a wrapped number gives `None`.
fn prefix_type(prefix: &NestedTypeNode<NumberTypeNode>) -> Option<Type> {
    match prefix {
        NestedTypeNode::Value(n) if !matches!(n.format, NumberFormat::F32 | NumberFormat::F64) => {
            Some(number_type(n))
        }
        _ => None,
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
        // Solana's compact length: its encoding has no byte order.
        NumberFormat::ShortU16 => return Type::ShortU16,
    };
    // Single bytes have no byte order.
    if n.endian == Endianness::Be && !matches!(ty, Type::U8 | Type::I8) {
        return Type::BigEndian(Box::new(ty));
    }
    ty
}

fn extract_ix_discriminators(ix: &InstructionNode) -> Discs {
    extract_discriminators(&ix.discriminators, |name| {
        let arg = ix.arguments.iter().find(|a| a.name.as_str() == name)?;
        arg_default_bytes(arg)
    })
}

fn arg_default_bytes(arg: &InstructionArgumentNode) -> Option<Vec<u8>> {
    match arg.default_value.as_ref().as_ref()? {
        InstructionInputValueNode::BytesValue(b) => Some(decode_bytes(b)),
        InstructionInputValueNode::NumberValue(n) => number_to_bytes(n, &arg.r#type),
        InstructionInputValueNode::ConstantValue(c) => {
            value_to_bytes(c.value.as_ref(), c.r#type.as_ref())
        }
        _ => None,
    }
}

/// A numeric constant discriminator is a check on a byte the body re-reads as its first
/// field, so Shipstern's account parser decodes from offset 0 (SPL Governance's `accountType`).
/// A size discriminator before the constant does not change where the body starts.
fn numeric_constant_discriminator(a: &AccountNode) -> bool {
    matches!(
        a.discriminators.iter().find(|d| !matches!(d, DiscriminatorNode::Size(_))),
        Some(DiscriminatorNode::Constant(c)) if matches!(c.constant.value.as_ref(), ValueNode::Number(_))
    )
}

fn extract_account_discriminators(a: &AccountNode) -> Discs {
    let st = a.data.get_nested_type_node();
    extract_discriminators(&a.discriminators, |name| {
        let field = st.fields.iter().find(|f| f.name.as_str() == name)?;
        field_default_bytes(field)
    })
}

fn extract_event_discriminators(e: &EventNode) -> Discs {
    let fields: &[StructFieldTypeNode] = match e.data.as_ref() {
        TypeNode::Struct(st) => st.fields.as_slice(),
        TypeNode::HiddenPrefix(hp) => match hp.r#type.as_ref() {
            TypeNode::Struct(st) => st.fields.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    extract_discriminators(&e.discriminators, |name| {
        let field = fields.iter().find(|f| f.name.as_str() == name)?;
        field_default_bytes(field)
    })
}

/// An event's discriminators and self-CPI envelope.
struct EventParts {
    discriminator: Discriminator,
    discriminator_offset: usize,
    extra_discriminators: Vec<(usize, Discriminator)>,
    envelope: Discriminator,
    envelope_payload_offset: Option<usize>,
}

/// The event discriminators and the self-CPI envelope, by Shipstern's rule
/// (`proc-macro/src/parse.rs`, `rebased_event_discriminators`): Codama has no node for the
/// envelope, so in an all-constant chain sorted by offset, one constant at offset 0 is the
/// tag. The next one starts the event, and every later one is checked too, all at offsets
/// after the envelope. The `hiddenPrefix` around the data only hides those bytes from the struct.
fn extract_event_parts(e: &EventNode) -> EventParts {
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
        [(0, tag), (base, disc), rest @ ..] if *base > 0 => EventParts {
            discriminator: disc.clone(),
            discriminator_offset: 0,
            extra_discriminators: rest
                .iter()
                .map(|(offset, bytes)| (offset - base, bytes.clone()))
                .collect(),
            envelope: tag.clone(),
            envelope_payload_offset: Some(*base),
        },
        _ => {
            let discs = extract_event_discriminators(e);
            EventParts {
                discriminator: discs.first,
                discriminator_offset: discs.offset,
                extra_discriminators: discs.extra,
                envelope: Vec::new(),
                envelope_payload_offset: None,
            }
        }
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

/// The discriminators of an account, instruction or event.
struct Discs {
    /// The first one, which Shipstern dispatches on.
    first: Discriminator,
    offset: usize,
    /// The later ones, by offset. Codama requires them to match too.
    extra: Vec<(usize, Discriminator)>,
}

impl Discs {
    fn end(&self) -> usize {
        discriminator_end(self.offset, &self.first, &self.extra)
    }
}

/// A later discriminator with unknown bytes (such as an enum value) is left out. A size
/// discriminator first leaves `first` empty, because Shipstern then dispatches on the length.
fn extract_discriminators(
    discs: &[DiscriminatorNode],
    resolve_field: impl Fn(&str) -> Option<Vec<u8>>,
) -> Discs {
    let bytes = |d: &DiscriminatorNode| match d {
        DiscriminatorNode::Constant(c) => (
            usize::try_from(c.offset).unwrap_or(usize::MAX),
            value_to_bytes(c.constant.value.as_ref(), c.constant.r#type.as_ref())
                .unwrap_or_default(),
        ),
        DiscriminatorNode::Field(f) => (
            usize::try_from(f.offset).unwrap_or(usize::MAX),
            resolve_field(f.name.as_str()).unwrap_or_default(),
        ),
        DiscriminatorNode::Size(_) => (0, Vec::new()),
    };
    let Some((offset, first)) = discs.first().map(bytes) else {
        return Discs {
            first: Vec::new(),
            offset: 0,
            extra: Vec::new(),
        };
    };
    let extra = discs
        .iter()
        .skip(1)
        .map(bytes)
        .filter(|(_, d)| !d.is_empty())
        .collect();
    Discs {
        first,
        offset,
        extra,
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
    let width = number_width(number.format)?;
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

/// The byte width of an integer format. A float or `shortU16` gives `None`.
fn number_width(format: NumberFormat) -> Option<usize> {
    match format {
        NumberFormat::U8 | NumberFormat::I8 => Some(1),
        NumberFormat::U16 | NumberFormat::I16 => Some(2),
        NumberFormat::U32 | NumberFormat::I32 => Some(4),
        NumberFormat::U64 | NumberFormat::I64 => Some(8),
        NumberFormat::U128 | NumberFormat::I128 => Some(16),
        _ => None,
    }
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
