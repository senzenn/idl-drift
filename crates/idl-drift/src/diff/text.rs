//! How changes name types, fields and bytes in their messages.

use std::fmt::Write;

use crate::model::{ArrayLen, Count, EnumFields, Field, GenericArg, OptionTag, Type};

/// `<what> changed (<name>)`. When the length changed too, the message gives both lengths,
/// because a new length means a new discriminator scheme, as `hint` says.
pub(super) fn disc_changed(what: &str, name: &str, old: &[u8], new: &[u8], hint: &str) -> String {
    if old.len() == new.len() {
        format!("{what} changed ({name})")
    } else {
        format!(
            "{what} LENGTH changed ({name}): {}->{} bytes ({hint})",
            old.len(),
            new.len()
        )
    }
}

pub(super) fn disc_hex(disc: &[u8]) -> String {
    let mut out = String::with_capacity(disc.len() * 2);
    for b in disc {
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub(super) fn size_text(size: Option<usize>) -> String {
    size.map_or_else(|| "none".into(), |n| format!("{n} bytes"))
}

pub(super) fn field_str(f: &Field) -> String {
    format!("{}: {}", f.name, tystr(&f.ty))
}

/// Name both sides, so a rename doesn't read as `u64 -> u64`.
pub(super) fn field_change(old: &Field, new: &Field) -> String {
    let (ot, nt) = (tystr(&old.ty), tystr(&new.ty));
    if old.name == new.name {
        format!("{}: {ot} -> {nt} (Borsh layout)", old.name)
    } else if old.ty == new.ty {
        format!("{} renamed to {} ({ot})", old.name, new.name)
    } else {
        format!("{}: {ot} -> {}: {nt} (Borsh layout)", old.name, new.name)
    }
}

pub(super) fn payload_str(fields: &EnumFields) -> String {
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

/// A type in Rust-like syntax, such as `Option<u64>` or `[u8; 32]`.
pub(super) fn tystr(t: &Type) -> String {
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
