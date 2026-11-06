//! `serialization` and `repr` rules: a change counts only where it moves bytes that a
//! zero-copy account is read from.

mod common;

use common::{fixture, max_sev, messages, parse};
use idl_drift::model::Format;
use idl_drift::{diff, Severity};

/// A `Vault` account with these fields and this serialization.
fn vault(serialization: &str, fields: &str) -> String {
    format!(
        r#"{{"address":"A","metadata":{{"name":"s","version":"1","spec":"0.1.0"}},
  "instructions":[{{"name":"noop","discriminator":[1],"accounts":[],"args":[]}}],
  "accounts":[{{"name":"Vault","discriminator":[1,2,3,4,5,6,7,8]}}],
  "types":[{{"name":"Vault","serialization":"{serialization}","type":{{"kind":"struct","fields":[{fields}]}}}}]}}"#
    )
}

fn borsh_to_bytemuck(fields: &str) -> Option<Severity> {
    max_sev(&vault("borsh", fields), &vault("bytemuck", fields))
}

#[test]
fn serialization_change_without_padding_is_cosmetic() {
    // A single u64 has no padding, so Borsh and repr(C) write the same bytes.
    assert_eq!(
        borsh_to_bytemuck(r#"{"name":"bal","type":"u64"}"#),
        Some(Severity::Cosmetic)
    );
}

#[test]
fn serialization_change_with_padding_is_breaking() {
    // u8 then u64: repr(C) puts 7 bytes of padding before `bal`.
    let fields = r#"{"name":"flag","type":"u8"},{"name":"bal","type":"u64"}"#;
    assert_eq!(
        messages(&parse(&vault("borsh", fields)), &parse(&vault("bytemuck", fields))),
        ["serialization changed (Vault): borsh -> bytemuck: moves `Vault.bal` from offset 1 to 8 (zero-copy layout)"]
    );
}

#[test]
fn serialization_change_with_only_trailing_padding_is_dangerous() {
    // u64 then u8: 7 bytes of padding at the end, every field keeps its offset.
    assert_eq!(
        borsh_to_bytemuck(r#"{"name":"bal","type":"u64"},{"name":"flag","type":"u8"}"#),
        Some(Severity::Dangerous)
    );
}

#[test]
fn repr_change_on_reachable_type_is_breaking() {
    let packed = r#"{
  "address":"A","metadata":{"name":"zc","version":"0.1.0","spec":"0.1.0"},
  "accounts":[{"name":"Pool","discriminator":[1,1,1,1,1,1,1,1]}],
  "types":[{"name":"Pool","serialization":"bytemuck","repr":{"kind":"c","packed":true},
    "type":{"kind":"struct","fields":[{"name":"a","type":"u8"},{"name":"b","type":"u64"}]}}]
}"#;
    let unpacked = packed.replace(r#","packed":true"#, "");
    assert_eq!(
        messages(&parse(packed), &parse(&unpacked)),
        ["repr changed (Pool): repr(C, packed) -> repr(C): moves `Pool.b` from offset 1 to 8 (zero-copy layout)"]
    );
}

#[test]
fn repr_change_on_nested_type_that_moves_outer_fields_is_breaking() {
    // `Inner` has no padding of its own, but unpacking it moves `v` inside zero-copy `Outer`.
    let packed = r#"{
  "address":"A","metadata":{"name":"n","version":"0","spec":"0.1.0"},
  "accounts":[{"name":"Outer","discriminator":[1,1,1,1,1,1,1,1]}],
  "types":[{"name":"Outer","serialization":"bytemuck","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
      {"name":"tag","type":"u8"},{"name":"inner","type":{"defined":{"name":"Inner"}}}]}},
    {"name":"Inner","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[{"name":"v","type":"u64"}]}}]
}"#;
    let unpacked = packed.replace(r#","packed":true"#, "");
    assert_eq!(
        messages(&parse(packed), &parse(&unpacked)),
        ["repr changed (Inner): repr(C, packed) -> repr(C): moves `Outer.inner.v` from offset 1 to 8 (zero-copy layout)"]
    );
}

#[test]
fn item_size_change_in_a_last_array_field_is_breaking() {
    // `items` is the last field, so no field after it shows that `items[1].x` moves.
    let pool = |pool_attrs: &str, inner_attrs: &str, len: usize| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
  "accounts":[{{"name":"Pool","discriminator":[1,2,3,4,5,6,7,8]}}],
  "types":[{{"name":"Pool",{pool_attrs}"type":{{"kind":"struct","fields":[
      {{"name":"a","type":"u64"}},{{"name":"items","type":{{"array":[{{"defined":{{"name":"Inner"}}}},{len}]}}}}]}}}},
    {{"name":"Inner",{inner_attrs}"type":{{"kind":"struct","fields":[
      {{"name":"x","type":"u64"}},{{"name":"y","type":"u8"}}]}}}}]}}"#
        )
    };
    let zc = r#""serialization":"bytemuckunsafe","repr":{"kind":"c"},"#;
    let packed = r#""serialization":"bytemuckunsafe","repr":{"kind":"c","packed":true},"#;
    // The item stride goes from 16 to 9 bytes, and from Borsh 9 to repr(C) 16.
    assert_eq!(
        max_sev(&pool(zc, zc, 4), &pool(zc, packed, 4)),
        Some(Severity::Breaking)
    );
    assert_eq!(
        max_sev(&pool("", "", 4), &pool(zc, zc, 4)),
        Some(Severity::Breaking)
    );
    // With one item, `x` and `y` keep their offsets and `Pool` keeps its size.
    assert_eq!(
        max_sev(&pool(zc, zc, 1), &pool(zc, packed, 1)),
        Some(Severity::Cosmetic)
    );
    // 14 nested arrays of 2 items: a walk into each last item would pass the work limit.
    let grid = (0..14).fold(r#""u64""#.to_string(), |ty, _| {
        format!(r#"{{"array":[{ty},2]}}"#)
    });
    assert_eq!(
        borsh_to_bytemuck(&format!(r#"{{"name":"grid","type":{grid}}}"#)),
        Some(Severity::Cosmetic)
    );
}

#[test]
fn repr_change_on_borsh_only_type_is_cosmetic() {
    // Only an instruction arg holds `Inner`, and Borsh ignores repr.
    let old = r#"{
  "address":"A","metadata":{"name":"b","version":"0","spec":"0.1.0"},
  "instructions":[{"name":"f","discriminator":[1],"accounts":[],
    "args":[{"name":"i","type":{"defined":{"name":"Inner"}}}]}],
  "types":[{"name":"Inner","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[
    {"name":"a","type":"u8"},{"name":"v","type":"u64"}]}}]
}"#;
    let new = old.replace(r#","packed":true"#, "");
    assert_eq!(max_sev(old, &new), Some(Severity::Cosmetic));
}

#[test]
fn nested_packed_types_that_all_drop_their_layout_keep_the_bytes() {
    // Each change alone moves `inner.y`; together, Borsh reads every byte where the packed
    // account had it.
    let packed = r#"{"address":"P","metadata":{"name":"p","version":"0.1.0","spec":"0.1.0"},
  "accounts":[{"name":"Outer","discriminator":[1,1,1,1,1,1,1,1]}],
  "types":[
    {"name":"Outer","serialization":"bytemuckunsafe","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[
      {"name":"a","type":"u8"},{"name":"inner","type":{"defined":{"name":"Inner"}}}]}},
    {"name":"Inner","serialization":"bytemuckunsafe","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[
      {"name":"x","type":"u8"},{"name":"y","type":"u64"}]}}]}"#;
    let borsh = packed.replace(
        r#""serialization":"bytemuckunsafe","repr":{"kind":"c","packed":true},"#,
        "",
    );
    let report = diff(&parse(packed), &parse(&borsh));
    assert_eq!(report.counts(), (0, 0, 0, 2), "{:?}", report.changes);
}

#[test]
fn zero_copy_type_read_only_as_an_arg_is_borsh() {
    // Instruction args are always Borsh, so the zero-copy padding before `amount` is never read.
    let old = r#"{"address":"P","metadata":{"name":"p","version":"0.1.0","spec":"0.1.0"},
      "instructions":[{"name":"f","discriminator":[1],"accounts":[],"args":[{"name":"r","type":{"defined":{"name":"Receipt"}}}]}],
      "types":[{"name":"Receipt","serialization":"bytemuck","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
        {"name":"flag","type":"u8"},{"name":"amount","type":"u64"}]}}]}"#;
    let new = old.replace(r#""serialization":"bytemuck","repr":{"kind":"c"},"#, "");
    assert_eq!(
        messages(&parse(old), &parse(&new)),
        ["serialization and repr changed (Receipt): bytemuck repr(C) -> borsh none: only read as Borsh, which ignores both"]
    );
}

#[test]
fn repr_change_with_an_uncomputable_layout_is_breaking() {
    // The IDL gives no width for `Status`, so neither layout is known, but unpacking still
    // moves `amount` if `Status` is one byte.
    let old = r#"{"address":"P","metadata":{"name":"p","version":"0.1.0","spec":"0.1.0"},
      "accounts":[{"name":"Pool","discriminator":[1,1,1,1,1,1,1,1]}],
      "types":[{"name":"Pool","serialization":"bytemuck","repr":{"kind":"c","packed":true},"type":{"kind":"struct","fields":[
          {"name":"status","type":{"defined":{"name":"Status"}}},{"name":"amount","type":"u64"}]}},
        {"name":"Status","type":{"kind":"enum","variants":[{"name":"Open"},{"name":"Closed"}]}}]}"#;
    let new = old.replace(r#","packed":true"#, "");
    assert_eq!(
        messages(&parse(old), &parse(&new)),
        ["repr changed (Pool): repr(C, packed) -> repr(C): the zero-copy layout of Pool can't be computed, so a field may move: `status`: `Status` is an enum, which has no defined zero-copy layout"]
    );
}

#[test]
fn nested_type_serialization_does_not_change_a_zero_copy_layout() {
    // `Cfg` is read in place inside `Pool` with the same repr, whatever its own serialization.
    // The enum keeps the layout of `Pool` unknown, so only the inputs of that layout count.
    let old = r#"{"address":"P","metadata":{"name":"p","version":"0.1.0","spec":"0.1.0"},
      "accounts":[{"name":"Pool","discriminator":[1,1,1,1,1,1,1,1]}],
      "types":[{"name":"Pool","serialization":"bytemuck","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
          {"name":"status","type":{"defined":{"name":"Status"}}},{"name":"cfg","type":{"defined":{"name":"Cfg"}}}]}},
        {"name":"Cfg","serialization":"bytemuck","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
          {"name":"flag","type":"u8"},{"name":"amount","type":"u64"}]}},
        {"name":"Status","type":{"kind":"enum","variants":[{"name":"Open"},{"name":"Closed"}]}}]}"#;
    let new = old.replace(
        r#"{"name":"Cfg","serialization":"bytemuck","#,
        r#"{"name":"Cfg","#,
    );
    assert_eq!(
        messages(&parse(old), &parse(&new)),
        ["serialization changed (Cfg): bytemuck -> borsh: no zero-copy reading depends on this change, so the bytes are the same"]
    );
}

#[test]
fn codama_conversion_that_drops_zero_copy_layout_is_flagged() {
    // `npx codama convert` drops serialization and repr. Only types whose padding moves
    // fields misread; the rest are byte-identical.
    let report = diff(
        &fixture("live/voltr_vault/idl.json"),
        &fixture("live/voltr_vault/idl.js-codama.json"),
    );
    let at = |sev: Severity| -> Vec<&str> {
        report
            .changes
            .iter()
            .filter(|c| c.severity == sev)
            .map(|c| c.message.split(':').next().unwrap_or_default())
            .collect()
    };
    assert_eq!(
        at(Severity::Breaking),
        ["serialization and repr changed (adaptorAddReceipt)"]
    );
    assert_eq!(
        at(Severity::Dangerous),
        ["serialization and repr changed (requestWithdrawVaultReceipt)"]
    );
    assert_eq!(at(Severity::Cosmetic).len(), 11);
    assert_eq!(report.changes.len(), 13);
}

#[test]
fn shipstern_padding_fields_are_the_same_bytes() {
    // Shipstern's converter writes each repr(C) gap of a zero-copy type as a u8 array field.
    let receipt = |attrs: &str, fields: &str| {
        format!(
            r#"{{"address":"P","metadata":{{"name":"p","version":"0.1.0","spec":"0.1.0"}},
      "accounts":[{{"name":"Receipt","discriminator":[1,2,3,4,5,6,7,8]}}],
      "types":[{{"name":"Receipt",{attrs}"type":{{"kind":"struct","fields":[{fields}]}}}}]}}"#
        )
    };
    let zero_copy = r#""serialization":"bytemuckunsafe","repr":{"kind":"c"},"#;
    let anchor = parse(&receipt(
        zero_copy,
        r#"{"name":"bump","type":"u8"},{"name":"epoch","type":"u64"},{"name":"flag","type":"u8"}"#,
    ));
    let codama = |fields: &str| {
        let mut codama = parse(&receipt("", fields));
        codama.format = Format::Codama;
        codama
    };
    let shipstern = |gap: usize| {
        codama(&format!(
            r#"{{"name":"bump","type":"u8"}},{{"name":"padding_before_epoch","type":{{"array":["u8",{gap}]}}}},
            {{"name":"epoch","type":"u64"}},{{"name":"flag","type":"u8"}},{{"name":"padding_end","type":{{"array":["u8",7]}}}}"#
        ))
    };
    let report = diff(&anchor, &shipstern(7));
    assert_eq!(report.counts(), (0, 0, 0, 1), "{:?}", report.changes);
    // A gap of another size moves `epoch`.
    assert_eq!(
        diff(&anchor, &shipstern(6))
            .changes
            .iter()
            .map(|c| c.severity)
            .max(),
        Some(Severity::Breaking)
    );
    // A real field with the padding name keeps it, so the gap is `paddingBeforeEpoch2`.
    let taken = r#"{"name":"padding_before_epoch","type":"u8"},"#;
    let report = diff(
        &parse(&receipt(
            zero_copy,
            &format!(r#"{taken}{{"name":"epoch","type":"u64"}}"#),
        )),
        &codama(&format!(
            r#"{taken}{{"name":"padding_before_epoch_2","type":{{"array":["u8",7]}}}},{{"name":"epoch","type":"u64"}}"#
        )),
    );
    assert_eq!(report.counts(), (0, 0, 0, 1), "{:?}", report.changes);
}

#[test]
fn fields_with_the_same_camel_case_name_pair_by_position() {
    // `padding_0` and `_padding_0` are both `padding0` in camelCase, as in dynamic_bonding_curve.
    let anchor = r#"{"address":"P","metadata":{"name":"p","version":"0.1.0","spec":"0.1.0"},
      "accounts":[{"name":"Meta","discriminator":[1,2,3,4,5,6,7,8]}],
      "types":[{"name":"Meta","serialization":"bytemuck","repr":{"kind":"c"},"type":{"kind":"struct","fields":[
        {"name":"padding_0","type":{"array":["u8",8]}},{"name":"x","type":"u64"},
        {"name":"_padding_0","type":"u8"},{"name":"y","type":{"array":["u8",7]}}]}}]}"#;
    let mut codama =
        parse(&anchor.replace(r#""serialization":"bytemuck","repr":{"kind":"c"},"#, ""));
    codama.format = Format::Codama;
    let report = diff(&parse(anchor), &codama);
    assert_eq!(report.counts(), (0, 0, 0, 1), "{:?}", report.changes);
}
