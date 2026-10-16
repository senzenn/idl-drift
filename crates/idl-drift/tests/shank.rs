//! Reading Shank IDLs: instruction tags from `discriminant`, no account discriminators.

mod common;

use common::{idl_kinds, parse};
use idl_drift::model::Type;
use idl_drift::{ChangeKind, Severity};

fn shank(create: u64, burn: u64) -> String {
    format!(
        r#"{{"version":"1.0.0","name":"t","metadata":{{"origin":"shank","address":"A"}},
        "instructions":[
          {{"name":"Create","accounts":[],"args":[],"discriminant":{{"type":"u8","value":{create}}}}},
          {{"name":"Burn","accounts":[],"args":[],"discriminant":{{"type":"u8","value":{burn}}}}}],
        "accounts":[{{"name":"Asset","type":{{"kind":"struct","fields":[
          {{"name":"owners","type":{{"hashMap":["publicKey","u64"]}}}}]}}}}]}}"#
    )
}

#[test]
fn shank_tags_come_from_discriminant() {
    let idl = parse(&shank(0, 7));
    let discs: Vec<&[u8]> = idl
        .instructions
        .iter()
        .map(|i| i.discriminator.as_slice())
        .collect();
    assert_eq!(discs, [&[0u8][..], &[7]]);
    assert!(idl.accounts[0].discriminator.is_empty());
    let fields = match &idl.types[0].ty {
        idl_drift::model::TypeDefTy::Struct { fields } => fields,
        other => panic!("struct expected, got {other:?}"),
    };
    assert_eq!(
        fields[0].ty,
        Type::Map(Box::new(Type::Pubkey), Box::new(Type::U64))
    );
}

#[test]
fn swapped_shank_tags_are_breaking() {
    let kinds = idl_kinds(&parse(&shank(0, 1)), &parse(&shank(1, 0)));
    assert_eq!(
        kinds,
        [
            (Severity::Breaking, ChangeKind::DiscriminatorChanged),
            (Severity::Breaking, ChangeKind::DiscriminatorChanged),
        ]
    );
}

#[test]
fn shank_tag_wider_than_its_type_fails() {
    let err = idl_drift::model::Idl::from_json(&shank(0, 256)).unwrap_err();
    assert!(err
        .to_string()
        .starts_with("Shank instruction Burn: `discriminant` must be"));
}
