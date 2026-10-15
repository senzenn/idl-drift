//! Checks against the captured mainnet fixtures in `fixtures/live`. Each known-bad case
//! mutates real data in one way and asserts that exactly that problem is reported.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use idl_drift::model::Idl;
use idl_verify::decode::{Decoder, Value};
use idl_verify::layout::TypeIndex;
use idl_verify::report::Check;
use idl_verify::{verify, Finding, Reference, Report, Samples, Severity};
use serde_json::Value as Json;

fn fixture(path: &str) -> String {
    let full = format!("{}/../../fixtures/live/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{full}: {e}"))
}

fn json(path: &str) -> Json {
    serde_json::from_str(&fixture(path)).unwrap()
}

fn run(idl: &Json, samples: &Json) -> Report {
    let idl = Idl::from_json(&idl.to_string()).unwrap();
    let samples = Samples::from_json(&samples.to_string()).unwrap();
    verify(&idl, &samples, None)
}

fn run_fixture(program: &str) -> Report {
    run(
        &json(&format!("{program}/idl.json")),
        &json(&format!("{program}/samples.json")),
    )
}

fn with_reference(program: &str, reference: &str) -> Report {
    let idl = Idl::from_json(&fixture(&format!("{program}/idl.json"))).unwrap();
    let samples = Samples::from_json(&fixture(&format!("{program}/samples.json"))).unwrap();
    let reference = Reference::from_json(reference, &samples).unwrap();
    verify(&idl, &samples, Some(&reference))
}

fn findings(report: &Report, severity: Severity) -> Vec<&Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.severity == severity)
        .collect()
}

/// The single error in `report`, which must have no others.
fn only_error(report: &Report) -> &Finding {
    let errors = findings(report, Severity::Error);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    errors[0]
}

fn data(entry: &Json) -> Vec<u8> {
    STANDARD.decode(entry["data"].as_str().unwrap()).unwrap()
}

fn set_data(entry: &mut Json, bytes: &[u8]) {
    entry["data"] = Json::String(STANDARD.encode(bytes));
}

/// Index of the first sample whose data starts with `disc`.
fn find(list: &Json, disc: &[u8]) -> usize {
    list.as_array()
        .unwrap()
        .iter()
        .position(|e| data(e).starts_with(disc))
        .unwrap()
}

const ADAPTOR_ADD_RECEIPT: [u8; 8] = [105, 99, 219, 155, 77, 241, 7, 119];
const VAULT: [u8; 8] = [211, 8, 232, 43, 2, 152, 117, 119];
const DEPOSIT_VAULT: [u8; 8] = [0x7e, 0xe0, 0x15, 0xff, 0xe4, 0x35, 0x75, 0x21];

#[test]
fn voltr_vault_anchor_idl_is_clean() {
    let report = run_fixture("voltr_vault");
    assert_eq!(report.count(Severity::Error), 0, "{:#?}", report.findings);
    assert_eq!(report.count(Severity::Warning), 0, "{:#?}", report.findings);
    assert_eq!(report.account_coverage.classified, 16);
    assert_eq!(report.instruction_coverage.classified, 20);
    // 25 mainnet events over 7 types, captured with --save-samples on 2026-10-14.
    assert_eq!(report.event_coverage.classified, 25);
    let receipt = report
        .accounts
        .iter()
        .find(|a| a.name == "AdaptorAddReceipt")
        .unwrap();
    assert_eq!(receipt.bytes_used.map(|r| r.max), Some(152));
    assert_eq!(receipt.expected_size, Some(152));
}

#[test]
fn jupiter_vaults_anchor_idl_is_clean() {
    let report = run_fixture("jupiter_vaults");
    assert_eq!(report.count(Severity::Error), 0, "{:#?}", report.findings);
    assert_eq!(report.count(Severity::Warning), 0, "{:#?}", report.findings);
    assert_eq!(report.account_coverage.classified, 20);
    assert_eq!(report.instruction_coverage.classified, 20);
}

#[test]
fn codama_conversion_that_drops_padding_is_flagged() {
    let report = run(
        &json("voltr_vault/idl.js-codama.json"),
        &json("voltr_vault/samples.json"),
    );
    let error = only_error(&report);
    assert_eq!(error.check, Check::BytesUsed);
    assert_eq!(error.subject, "account adaptorAddReceipt");
    assert_eq!(error.message, "7 of 152 bytes unused in 5 of 5 samples");
    let hint = error.hint.as_deref().unwrap();
    assert!(hint.contains("7 bytes before `lastUpdatedEpoch`"), "{hint}");
    assert_eq!(report.exit_code(), 1);
}

#[test]
fn field_after_padding_is_read_at_its_repr_c_offset() {
    // The fixtures hold 0 at both offsets, so write a value only the padded layout sees.
    let samples = json("voltr_vault/samples.json");
    let i = find(&samples["accounts"], &ADAPTOR_ADD_RECEIPT);
    let mut bytes = data(&samples["accounts"][i]);
    bytes[88..96].copy_from_slice(&987_654_321u64.to_le_bytes());

    let idl = Idl::from_json(&fixture("voltr_vault/idl.json")).unwrap();
    let types = TypeIndex::new(&idl);
    let td = types.get("AdaptorAddReceipt").unwrap();
    let decoded = Decoder::new(types.clone())
        .type_def(td, &bytes[8..])
        .unwrap();
    let Value::Struct(fields) = decoded.value else {
        panic!("expected a struct");
    };
    let epoch = fields
        .iter()
        .find(|(n, _)| n == "last_updated_epoch")
        .unwrap();
    assert_eq!(epoch.1, Value::U64(987_654_321));
}

#[test]
fn values_match_the_anchor_reference_decoder() {
    let voltr = with_reference("voltr_vault", &fixture("voltr_vault/reference.json"));
    assert_eq!(voltr.count(Severity::Error), 0, "{:#?}", voltr.findings);
    let r = voltr.reference.unwrap();
    // AdaptorAddReceipt (5 samples) has padding, which anchor's coder ignores.
    assert_eq!(
        (r.accounts_matched, r.accounts_compared, r.skipped),
        (11, 11, 5)
    );
    assert_eq!((r.instructions_matched, r.instructions_compared), (20, 20));

    let jupiter = with_reference("jupiter_vaults", &fixture("jupiter_vaults/reference.json"));
    assert_eq!(jupiter.count(Severity::Error), 0, "{:#?}", jupiter.findings);
    let r = jupiter.reference.unwrap();
    assert_eq!(
        (r.accounts_matched, r.accounts_compared, r.skipped),
        (20, 20, 0)
    );
    assert_eq!((r.instructions_matched, r.instructions_compared), (20, 20));
}

#[test]
fn reference_value_mismatch_is_flagged() {
    let mut reference = json("voltr_vault/reference.json");
    let i = reference["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .position(|a| a["type"] == "Vault")
        .unwrap();
    reference["accounts"][i]["reference"]["value"]["asset"]["total_value"] =
        serde_json::json!({ "bn": "1" });
    let report = with_reference("voltr_vault", &reference.to_string());
    let error = only_error(&report);
    assert_eq!(error.check, Check::Reference);
    assert_eq!(error.subject, "account Vault");
    assert_eq!(
        error.message,
        "values differ from the reference decoder in 1 of 5 samples, first at \
         `asset.total_value`: idl-verify read 1208595200, the reference 1"
    );
}

#[test]
fn account_type_missing_from_the_idl_is_flagged() {
    let mut idl = json("voltr_vault/idl.json");
    idl["accounts"]
        .as_array_mut()
        .unwrap()
        .retain(|a| a["name"] != "Protocol");
    let report = run(&idl, &json("voltr_vault/samples.json"));
    let error = only_error(&report);
    assert_eq!(error.check, Check::Discriminator);
    assert_eq!(
        error.message,
        "0x2d27652b73488328 matches no account discriminator in the IDL, in 1 sample"
    );
}

#[test]
fn instruction_missing_from_the_idl_is_flagged() {
    let mut idl = json("voltr_vault/idl.json");
    idl["instructions"]
        .as_array_mut()
        .unwrap()
        .retain(|ix| ix["name"] != "deposit_vault");
    let report = run(&idl, &json("voltr_vault/samples.json"));
    let error = only_error(&report);
    assert_eq!(error.check, Check::Discriminator);
    assert_eq!(
        error.message,
        "0x7ee015ffe4357521 matches no instruction discriminator in the IDL, in 6 samples"
    );
}

#[test]
fn truncated_account_fails_to_decode() {
    let mut samples = json("voltr_vault/samples.json");
    let i = find(&samples["accounts"], &VAULT);
    let bytes = data(&samples["accounts"][i]);
    set_data(&mut samples["accounts"][i], &bytes[..500]);
    let report = run(&json("voltr_vault/idl.json"), &samples);
    let error = only_error(&report);
    assert_eq!(error.check, Check::Decode);
    assert!(
        error
            .message
            .starts_with("fails to decode in 1 of 5 samples: `"),
        "{}",
        error.message
    );
    assert_eq!(
        error.hint.as_deref(),
        Some("the IDL layout needs 928 bytes and these accounts are 500 bytes: they may predate a program upgrade, or the IDL is out of date")
    );
}

#[test]
fn missing_fixed_address_accounts_are_flagged() {
    // How Jupiter `dex` failed: the program stopped requiring trailing fixed accounts.
    let mut samples = json("voltr_vault/samples.json");
    let i = find(&samples["instructions"], &DEPOSIT_VAULT);
    samples["instructions"][i]["accounts"]
        .as_array_mut()
        .unwrap()
        .truncate(11);
    let report = run(&json("voltr_vault/idl.json"), &samples);
    let error = only_error(&report);
    assert_eq!(error.check, Check::AccountCount);
    assert_eq!(
        error.message,
        "11 accounts passed where the IDL lists 13, in 1 of 6 samples; missing: \
         lp_token_program (fixed: TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA), \
         system_program (fixed: 11111111111111111111111111111111)"
    );
    assert!(error
        .hint
        .as_deref()
        .unwrap()
        .starts_with("every missing account has a fixed address"));
}

#[test]
fn account_at_a_fixed_address_position_is_checked() {
    let mut samples = json("voltr_vault/samples.json");
    let i = find(&samples["instructions"], &DEPOSIT_VAULT);
    samples["instructions"][i]["accounts"]
        .as_array_mut()
        .unwrap()
        .swap(11, 12);
    let report = run(&json("voltr_vault/idl.json"), &samples);
    let errors = findings(&report, Severity::Error);
    assert_eq!(errors.len(), 2, "{errors:#?}");
    assert!(errors.iter().all(|e| e.check == Check::FixedAddress));
    assert!(errors[0].message.starts_with(
        "account #11 `lp_token_program` must be TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
    ));
}

#[test]
fn trailing_instruction_bytes_are_flagged() {
    let mut samples = json("voltr_vault/samples.json");
    let i = find(&samples["instructions"], &DEPOSIT_VAULT);
    let mut bytes = data(&samples["instructions"][i]);
    bytes.push(0);
    set_data(&mut samples["instructions"][i], &bytes);
    let report = run(&json("voltr_vault/idl.json"), &samples);
    let error = only_error(&report);
    assert_eq!(error.check, Check::BytesUsed);
    assert_eq!(
        error.message,
        "1 byte after the arguments in 1 of 6 samples"
    );
}

#[test]
fn samples_from_another_program_are_a_warning() {
    let mut samples = json("voltr_vault/samples.json");
    samples["programId"] = Json::String("11111111111111111111111111111111".into());
    let report = run(&json("voltr_vault/idl.json"), &samples);
    let warnings = findings(&report, Severity::Warning);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].check, Check::ProgramId);
    assert_eq!(report.exit_code(), 0);
}

#[test]
fn bytemuck_type_with_padding_contradicts_pod() {
    // Read without `packed`, VaultState has gaps that Pod would refuse to compile.
    let mut idl = json("jupiter_vaults/idl.json");
    let ty = idl["types"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|t| t["name"] == "VaultState")
        .unwrap();
    ty["repr"] = serde_json::json!({ "kind": "c" });
    let report = run(&idl, &json("jupiter_vaults/samples.json"));
    let pod = findings(&report, Severity::Error)
        .into_iter()
        .find(|f| f.check == Check::Layout)
        .expect("a Pod finding");
    assert_eq!(pod.subject, "account VaultState");
    assert!(
        pod.message
            .contains("a `bytemuck` (Pod) type cannot have any"),
        "{}",
        pod.message
    );
}

#[test]
fn u32_discriminator_reads_the_amount_at_the_right_offset() {
    // shipstern#342: a u32 discriminator read as one byte turned 241,082 lamports into
    // 4,044,684,787,712.
    let idl = r#"{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
      "program":{"kind":"programNode","name":"system","publicKey":"11111111111111111111111111111111","version":"0",
        "accounts":[],"definedTypes":[],"pdas":[],"errors":[],
        "instructions":[{"kind":"instructionNode","name":"transfer","accounts":[],
          "arguments":[
            {"kind":"instructionArgumentNode","name":"discriminator","defaultValueStrategy":"omitted",
              "type":{"kind":"numberTypeNode","format":"u32","endian":"le"},
              "defaultValue":{"kind":"numberValueNode","number":2}},
            {"kind":"instructionArgumentNode","name":"lamports","type":{"kind":"numberTypeNode","format":"u64","endian":"le"}}],
          "discriminators":[{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}]}]}}"#;
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend(241_082u64.to_le_bytes());
    let samples = serde_json::json!({
        "programId": "11111111111111111111111111111111",
        "instructions": [{ "signature": "s", "data": STANDARD.encode(&data), "accounts": [] }]
    });
    let report = verify(
        &Idl::from_json(idl).unwrap(),
        &Samples::from_json(&samples.to_string()).unwrap(),
        None,
    );
    assert!(report.findings.is_empty(), "{:#?}", report.findings);
    assert_eq!(report.instructions[0].name, "transfer");
}

#[test]
fn numeric_constant_discriminator_is_the_body_first_field() {
    // SPL Governance: byte 0 is the discriminator and `accountType` at once. Shipstern's
    // parser decodes the body from offset 0 and reads the real account right; so must this.
    let account = |disc: u8, weight: u64| {
        let mut bytes = vec![disc];
        bytes.extend_from_slice(&weight.to_le_bytes());
        serde_json::json!({ "address": "A", "type": "voteRecord", "data": STANDARD.encode(bytes) })
    };
    let idl = serde_json::json!({
        "kind": "rootNode", "standard": "codama", "version": "1.5.1",
        "program": {
            "kind": "programNode", "name": "gov", "publicKey": "11111111111111111111111111111111",
            "version": "0.1.0", "instructions": [], "definedTypes": [], "pdas": [], "errors": [],
            "accounts": [{
                "kind": "accountNode", "name": "voteRecord",
                "discriminators": [{ "kind": "constantDiscriminatorNode", "offset": 0, "constant": {
                    "kind": "constantValueNode",
                    "type": { "kind": "numberTypeNode", "format": "u8", "endian": "le" },
                    "value": { "kind": "numberValueNode", "number": 12 } } }],
                "data": { "kind": "structTypeNode", "fields": [
                    { "kind": "structFieldTypeNode", "name": "discriminator", "defaultValueStrategy": "omitted",
                      "type": { "kind": "fixedSizeTypeNode", "size": 1, "type": { "kind": "bytesTypeNode" } },
                      "defaultValue": { "kind": "bytesValueNode", "data": "0c", "encoding": "base16" } },
                    { "kind": "structFieldTypeNode", "name": "accountType",
                      "type": { "kind": "numberTypeNode", "format": "u8", "endian": "le" } },
                    { "kind": "structFieldTypeNode", "name": "weight",
                      "type": { "kind": "numberTypeNode", "format": "u64", "endian": "le" } }
                ] }
            }]
        }
    });
    let samples = |accounts: Vec<Json>| serde_json::json!({ "programId": "11111111111111111111111111111111", "accounts": accounts, "instructions": [] });
    let report = run(&idl, &samples(vec![account(12, 7), account(12, 9)]));
    assert!(
        findings(&report, Severity::Error).is_empty(),
        "{:#?}",
        report.findings
    );
    // Another first byte is another account type, not a shifted read of this one.
    let report = run(&idl, &samples(vec![account(13, 7)]));
    assert_eq!(only_error(&report).check, Check::Discriminator);
}

#[test]
fn event_field_the_idl_leaves_out_is_flagged() {
    // Without its last field, a timestamp, DepositVaultEvent leaves 8 non-zero bytes unread.
    // Narrowing that u64 to a u32 would not do: its high 4 bytes are zero, so a note.
    let mut idl = json("voltr_vault/idl.json");
    let ty = idl["types"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|t| t["name"] == "DepositVaultEvent")
        .unwrap();
    ty["type"]["fields"].as_array_mut().unwrap().pop();
    let report = run(&idl, &json("voltr_vault/samples.json"));
    let error = only_error(&report);
    assert_eq!(
        (error.check, error.subject.as_str()),
        (Check::BytesUsed, "event DepositVaultEvent")
    );
    assert!(
        error.message.starts_with("8 bytes after the payload"),
        "{}",
        error.message
    );
}

#[test]
fn pump_fun_trade_event_carries_fields_its_idl_lacks() {
    // Mainnet, 2026-10-14: pump.fun emits each event as a log line and as a self-CPI, and
    // writes 24 bytes past the on-chain IDL's TradeEvent in both.
    let report = run_fixture("pump_fun");
    let error = only_error(&report);
    assert_eq!(error.subject, "event TradeEvent");
    assert!(
        error
            .message
            .starts_with("24 bytes after the payload in 10 of 10 samples"),
        "{}",
        error.message
    );
    let trade = report
        .events
        .iter()
        .find(|e| e.name == "TradeEvent")
        .unwrap();
    assert_eq!((trade.sources.log, trade.sources.cpi), (5, 5));
}
