use super::*;
use idl_drift::model::Idl;

fn idl(types: &str) -> Idl {
    crate::testing::idl(&format!(r#""types":{types}"#))
}

fn decode(idl: &Idl, name: &str, data: &[u8]) -> Result<Decoded, DecodeError> {
    let index = TypeIndex::new(idl);
    let td = index.get(name).unwrap();
    Decoder::new(index).type_def(td, data)
}

#[test]
fn borsh_struct_reports_bytes_used() {
    let idl = idl(r#"[{"name":"S","type":{"kind":"struct","fields":[
            {"name":"a","type":"u16"},{"name":"s","type":"string"},{"name":"o","type":{"option":"u8"}}]}}]"#);
    let d = decode(&idl, "S", &[1, 0, 2, 0, 0, 0, b'h', b'i', 1, 7, 99]).unwrap();
    assert_eq!(d.len, 10);
    assert_eq!(
        d.value,
        Value::Struct(vec![
            ("a".into(), Value::U16(1)),
            ("s".into(), Value::String("hi".into())),
            ("o".into(), Value::Option(Some(Box::new(Value::U8(7))))),
        ])
    );
}

#[test]
fn zero_copy_reads_past_padding() {
    let idl = idl(
        r#"[{"name":"Z","serialization":"bytemuckunsafe","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
            {"name":"a","type":"u8"},{"name":"b","type":"u64"}]}}]"#,
    );
    let mut data = vec![5, 0xee, 0xee, 0xee, 0xee, 0xee, 0xee, 0xee];
    data.extend(9u64.to_le_bytes());
    let d = decode(&idl, "Z", &data).unwrap();
    assert_eq!(d.len, 16);
    assert_eq!(
        d.value,
        Value::Struct(vec![
            ("a".into(), Value::U8(5)),
            ("b".into(), Value::U64(9))
        ])
    );
}

#[test]
fn short_data_names_the_field() {
    let idl = idl(r#"[{"name":"S","type":{"kind":"struct","fields":[
            {"name":"inner","type":{"defined":{"name":"I"}}}]}},
            {"name":"I","type":{"kind":"struct","fields":[{"name":"v","type":"u64"}]}}]"#);
    let err = decode(&idl, "S", &[1, 2, 3]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "`inner.v` at offset 0: needs 8 bytes, 3 left"
    );
}

#[test]
fn huge_vec_length_is_rejected_without_allocating() {
    let idl = idl(
        r#"[{"name":"S","type":{"kind":"struct","fields":[{"name":"v","type":{"vec":"u64"}}]}}]"#,
    );
    let err = decode(&idl, "S", &[0xff, 0xff, 0xff, 0xff, 0, 0]).unwrap_err();
    assert_eq!(
        err.kind,
        DecodeErrorKind::TooLong {
            len: 0xffff_ffff,
            remaining: 2
        }
    );
}

#[test]
fn enum_reads_declared_tag_width_and_values() {
    // Codama `size: u32` and an explicit variant discriminator, as converted.
    let idl = idl(
        r#"[{"name":"E","type":{"kind":"enum","tag":"u32","variants":[{"name":"A"},{"name":"B","discriminator":5}]}}]"#,
    );
    let d = decode(&idl, "E", &[5, 0, 0, 0]).unwrap();
    assert_eq!(d.len, 4);
    assert_eq!(
        d.value,
        Value::Enum {
            variant: "B".into(),
            value: Box::new(Value::Tuple(vec![]))
        }
    );
    let err = decode(&idl, "E", &[1, 0, 0, 0]).unwrap_err();
    assert_eq!(
        err.kind,
        DecodeErrorKind::InvalidEnumTag {
            tag: 1,
            variants: 2
        }
    );
}

#[test]
fn enum_with_the_default_tag_reads_a_tuple_variant() {
    let idl = idl(
        r#"[{"name":"E","type":{"kind":"enum","variants":[{"name":"A"},{"name":"B","fields":["u8"]}]}}]"#,
    );
    assert_eq!(
        decode(&idl, "E", &[1, 4]).unwrap().value,
        Value::Enum {
            variant: "B".into(),
            value: Box::new(Value::Tuple(vec![Value::U8(4)]))
        }
    );
}

#[test]
fn generic_type_and_const_arguments_resolve() {
    let idl = idl(r#"[{"name":"Outer","type":{"kind":"struct","fields":[
              {"name":"w","type":{"defined":{"name":"Buf","generics":[
                {"kind":"type","type":"u16"},{"kind":"const","value":"2"}]}}}]}},
            {"name":"Buf","generics":[{"kind":"type","name":"T"},{"kind":"const","name":"N","type":"usize"}],
             "type":{"kind":"struct","fields":[{"name":"v","type":{"array":[{"generic":"T"},{"generic":"N"}]}}]}}]"#);
    let d = decode(&idl, "Outer", &[1, 0, 2, 0]).unwrap();
    assert_eq!(d.len, 4);
    let Value::Struct(outer) = d.value else {
        panic!()
    };
    assert_eq!(
        outer[0].1,
        Value::Struct(vec![(
            "v".into(),
            Value::List(vec![Value::U16(1), Value::U16(2)])
        )])
    );
}

#[test]
fn u256_reads_32_bytes() {
    let idl = idl("[]");
    let decoder = Decoder::new(TypeIndex::new(&idl));
    let mut data = vec![7u8; 32];
    data.push(1);
    let d = decoder.borsh(&Type::I256, &data).unwrap();
    assert_eq!((d.value, d.len), (Value::I256([7; 32]), 32));
}

#[test]
fn huge_array_length_is_an_error_not_a_panic() {
    let idl = idl(
        r#"[{"name":"S","type":{"kind":"struct","fields":[{"name":"v","type":{"array":["u8",4000000000000]}}]}},
            {"name":"Z","serialization":"bytemuck","type":{"kind":"struct","fields":[{"name":"v","type":{"array":["u8",4000000000000]}}]}}]"#,
    );
    for name in ["S", "Z"] {
        let err = decode(&idl, name, &[1, 2]).unwrap_err();
        assert_eq!(
            err.kind,
            DecodeErrorKind::UnexpectedEnd {
                needed: 4_000_000_000_000,
                remaining: 2
            },
            "{name}"
        );
    }
}

#[test]
fn coption_none_still_holds_the_value_bytes() {
    let idl = idl("[]");
    let decoder = Decoder::new(TypeIndex::new(&idl));
    let mut data = vec![0, 0, 0, 0];
    data.extend([0u8; 8]);
    let d = decoder
        .borsh(&Type::COption(Box::new(Type::U64)), &data)
        .unwrap();
    assert_eq!((d.value, d.len), (Value::Option(None), 12));
}

#[test]
fn vec_of_an_empty_struct_fails_like_borsh() {
    // pump.fun `distributeCreatorFeesEvent` in an IDL where `Shareholder` has no fields.
    let idl = idl(
        r#"[{"name":"Shareholder","type":{"kind":"struct","fields":[]}},
            {"name":"Ev","type":{"kind":"struct","fields":[
                {"name":"fee","type":"u8"},{"name":"shareholders","type":{"vec":{"defined":{"name":"Shareholder"}}}}]}},
            {"name":"Ok","type":{"kind":"struct","fields":[
                {"name":"maybe","type":{"vec":{"option":{"defined":{"name":"Shareholder"}}}}}]}}]"#,
    );
    // Borsh checks the item type before the length, so zero items fail too.
    for len in [3u32, 0] {
        let mut data = vec![7];
        data.extend(len.to_le_bytes());
        let err = decode(&idl, "Ev", &data).unwrap_err();
        assert_eq!(
            (err.path(), err.offset, err.kind),
            (
                "shareholders".to_string(),
                1,
                DecodeErrorKind::ZeroSizedItems("Shareholder".into())
            )
        );
    }
    // An option takes a tag byte in Rust, so a Vec of options decodes.
    assert_eq!(decode(&idl, "Ok", &[1, 0, 0, 0, 0]).map(|d| d.len), Ok(5));
}

#[test]
fn vec_of_an_inline_empty_struct_fails_like_borsh() {
    // A Codama `structTypeNode` with no fields, written inline as the item type.
    let idl = idl("[]");
    let decoder = Decoder::new(TypeIndex::new(&idl));
    let ty = Type::Vec(Box::new(Type::Struct(Vec::new())));
    let err = decoder.borsh(&ty, &[0, 0, 0, 0]).unwrap_err();
    assert_eq!(err.kind, DecodeErrorKind::ZeroSizedItems("struct".into()));
}

#[test]
fn codama_only_encodings_decode_and_bad_tags_are_errors() {
    let num = |format: &str, endian: &str| {
        format!(r#"{{"kind":"numberTypeNode","format":"{format}","endian":"{endian}"}}"#)
    };
    let args = [
        format!(
            r#"{{"kind":"structTypeNode","fields":[{{"kind":"structFieldTypeNode","name":"a","type":{}}}]}}"#,
            num("u8", "le")
        ),
        format!(
            r#"{{"kind":"sizePrefixTypeNode","type":{{"kind":"stringTypeNode","encoding":"utf8"}},"prefix":{}}}"#,
            num("u8", "le")
        ),
        num("u16", "be"),
        num("shortU16", "le"),
        format!(
            r#"{{"kind":"optionTypeNode","item":{},"prefix":{}}}"#,
            num("u8", "le"),
            num("u16", "be")
        ),
        r#"{"kind":"zeroableOptionTypeNode","item":{"kind":"publicKeyTypeNode"}}"#.to_string(),
        format!(
            r#"{{"kind":"arrayTypeNode","item":{},"count":{{"kind":"remainderCountNode"}}}}"#,
            num("u16", "le")
        ),
    ];
    let args: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(i, ty)| format!(r#"{{"kind":"instructionArgumentNode","name":"a{i}","type":{ty}}}"#))
        .collect();
    let idl = Idl::from_json(&format!(
        r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
        "program":{{"kind":"programNode","name":"t","publicKey":"11111111111111111111111111111111",
        "version":"0","accounts":[],"pdas":[],"errors":[],"definedTypes":[],
        "instructions":[{{"kind":"instructionNode","name":"set","accounts":[],"discriminators":[],
        "arguments":[{}]}}]}}}}"#,
        args.join(",")
    ))
    .unwrap();
    let fields = &idl.instructions[0].args;
    let decoder = Decoder::new(TypeIndex::new(&idl));
    let mut data = vec![7, 2, b'h', b'i', 1, 2, 0x80, 0x01, 0, 1, 9];
    data.extend([0u8; 32]);
    data.extend([5, 0, 6, 0]);
    let d = decoder.borsh_fields(fields, &data).unwrap();
    let values: Vec<Value> = match d.value {
        Value::Struct(fields) => fields.into_iter().map(|(_, v)| v).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        values,
        [
            Value::Struct(vec![("a".to_string(), Value::U8(7))]),
            Value::String("hi".to_string()),
            Value::U16(0x0102),
            Value::U16(128),
            Value::Option(Some(Box::new(Value::U8(9)))),
            Value::Option(None),
            Value::List(vec![Value::U16(5), Value::U16(6)]),
        ]
    );
    assert_eq!(d.len, data.len());

    // An option tag of 2, and a shortU16 that is not in its shortest form.
    let bad_tag = decoder.borsh(&fields[4].ty, &[0, 2, 9]).unwrap_err();
    assert_eq!(bad_tag.kind, DecodeErrorKind::InvalidOptionTag(2));
    let long_form = decoder.borsh(&Type::ShortU16, &[0x80, 0x00]).unwrap_err();
    assert_eq!(long_form.kind, DecodeErrorKind::InvalidShortU16);
}
