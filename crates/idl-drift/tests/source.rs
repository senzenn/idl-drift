//! `source`: the program id in `declare_id!`, `Anchor.toml` and the IDL must agree.

use std::io::Write;
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use idl_drift::onchain;
use idl_drift::source::{
    check, check_onchain, discover, read_idl, FindingKind, Leg, ProgramId, SourceError,
    SourceReport,
};
use idl_drift::Severity;
use serde_json::{json, Value};

const ID: &str = "29dv8e1WcjL4w6a7HDaHbUfXrF12yiJiVcKQ1qgeT3rF";
const OTHER: &str = "4SsxzsqrisRmgB5N7NfXVEuU9e9sssebVUyhzmD1rNpH";

fn workspace(name: &str) -> SourceReport {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/source");
    match discover(&root.join(name)) {
        Ok(ids) => check(ids),
        Err(e) => panic!("{name}: {e}"),
    }
}

fn rules(report: &SourceReport) -> Vec<(FindingKind, Severity, &str)> {
    report
        .findings
        .iter()
        .map(|f| (f.kind, f.severity, f.subject.as_str()))
        .collect()
}

fn id(program: &str, leg: Leg, id: &str) -> ProgramId {
    ProgramId {
        program: program.into(),
        leg,
        id: id.into(),
    }
}

fn declare(program: &str) -> Leg {
    Leg::DeclareId {
        path: format!("programs/{program}/src/lib.rs"),
    }
}

fn toml(cluster: &str) -> Leg {
    Leg::AnchorToml {
        path: "Anchor.toml".into(),
        cluster: cluster.into(),
    }
}

fn idl(program: &str) -> Leg {
    Leg::IdlAddress {
        path: format!("target/idl/{program}.json"),
    }
}

#[test]
fn consistent_workspace_has_no_findings() {
    let report = workspace("consistent");
    // Two clusters, one declare_id! (the commented one is skipped) and the IDL.
    assert_eq!(report.ids.len(), 4);
    assert_eq!(rules(&report), []);
}

#[test]
fn drifted_workspace_reports_each_mismatch() {
    assert_eq!(
        rules(&workspace("drift")),
        [
            (
                FindingKind::AnchorTomlMismatch,
                Severity::Breaking,
                "amm@mainnet"
            ),
            (FindingKind::IdlAddressMismatch, Severity::Breaking, "amm"),
            (FindingKind::DeclareIdMissing, Severity::Cosmetic, "vault"),
        ]
    );
}

#[test]
fn invalid_id_is_breaking_and_not_compared() {
    let report = check(vec![
        id("amm", declare("amm"), ID),
        id("amm", toml("devnet"), "abc"),
    ]);
    assert_eq!(
        rules(&report),
        [(
            FindingKind::InvalidId,
            Severity::Breaking,
            "Anchor.toml:devnet"
        )]
    );
}

#[test]
fn names_that_differ_in_case_and_dash_are_one_program() {
    let report = check(vec![
        id("My-Amm", declare("my-amm"), ID),
        id("my_amm", idl("my_amm"), OTHER),
    ]);
    assert_eq!(
        rules(&report),
        [(
            FindingKind::IdlAddressMismatch,
            Severity::Breaking,
            "my_amm"
        )]
    );
}

#[test]
fn a_codama_camel_case_name_is_the_same_program_as_its_crate() {
    let report = check(vec![
        id("order_engine", declare("order_engine"), ID),
        id("orderEngine", idl("orderEngine"), OTHER),
    ]);
    assert_eq!(
        rules(&report),
        [(
            FindingKind::IdlAddressMismatch,
            Severity::Breaking,
            "order_engine"
        )]
    );
}

#[test]
fn crates_whose_names_differ_only_by_underscore_stay_apart() {
    let report = check(vec![
        id("a_mm", declare("a_mm"), ID),
        id("am_m", declare("am_m"), OTHER),
        id("a_mm", toml("mainnet"), OTHER),
        id("am_m", toml("mainnet"), ID),
    ]);
    assert_eq!(
        rules(&report),
        [
            (
                FindingKind::AnchorTomlMismatch,
                Severity::Breaking,
                "a_mm@mainnet"
            ),
            (
                FindingKind::AnchorTomlMismatch,
                Severity::Breaking,
                "am_m@mainnet"
            ),
        ]
    );
}

#[test]
fn with_no_declare_id_the_idl_must_match_anchor_toml() {
    let ids = |idl_id| {
        vec![
            id("amm", toml("mainnet"), ID),
            id("amm", idl("amm"), idl_id),
        ]
    };
    assert_eq!(rules(&check(ids(ID))), []);
    assert_eq!(
        rules(&check(ids(OTHER))),
        [(FindingKind::IdlAddressMismatch, Severity::Breaking, "amm")]
    );
}

#[test]
fn compared_needs_an_id_in_two_places() {
    assert!(!check(vec![id("amm", toml("mainnet"), ID)]).compared());
    assert!(!check(vec![
        id("amm", declare("amm"), ""),
        id("amm", toml("mainnet"), ID)
    ])
    .compared());
    assert!(check(vec![
        id("amm", declare("amm"), ID),
        id("amm", toml("mainnet"), ID)
    ])
    .compared());
    // The chain leg is a second place too.
    let rpc = chain(&[ID], &[(ID, idl_json(ID))]);
    assert!(onchain_report(vec![id("amm", declare("amm"), ID)], rpc).compared());
    // With no declare_id!, the IDL is compared with Anchor.toml.
    assert!(check(vec![
        id("amm", toml("mainnet"), ID),
        id("amm", idl("amm"), ID)
    ])
    .compared());
    // No rule compares two clusters, or two cfg values, with each other.
    assert!(!check(vec![
        id("amm", toml("localnet"), ID),
        id("amm", toml("devnet"), ID)
    ])
    .compared());
    assert!(!check(vec![
        id("amm", declare("amm"), ID),
        id("amm", declare("amm"), OTHER)
    ])
    .compared());
}

#[test]
fn a_report_that_compared_nothing_and_found_nothing_is_not_a_pass() {
    assert!(check(Vec::new()).nothing_to_compare());
    // A finding is a result, also when no id was compared.
    assert!(!check(vec![id("amm", toml("mainnet"), "abc")]).nothing_to_compare());
}

#[test]
fn two_declare_ids_are_dangerous_and_each_cluster_may_use_either() {
    let report = check(vec![
        id("amm", declare("amm"), ID),
        id("amm", declare("amm"), OTHER),
        id("amm", toml("localnet"), ID),
        id("amm", toml("mainnet"), OTHER),
    ]);
    assert_eq!(
        rules(&report),
        [(FindingKind::DeclareIdDuplicate, Severity::Dangerous, "amm")]
    );
}

#[test]
fn more_declare_ids_than_a_cfg_set_needs_is_breaking() {
    let ids = (0u8..17)
        .map(|i| id("amm", declare("amm"), &bs58::encode([i; 32]).into_string()))
        .collect();
    assert_eq!(
        rules(&check(ids)),
        [(FindingKind::DeclareIdDuplicate, Severity::Breaking, "amm")]
    );
}

#[test]
fn legacy_idl_without_address_is_cosmetic() {
    let report = check(vec![
        id("amm", declare("amm"), ID),
        id("amm", idl("amm"), ""),
    ]);
    assert_eq!(
        rules(&report),
        [(FindingKind::IdlAddressMissing, Severity::Cosmetic, "amm")]
    );
}

#[test]
fn a_library_crate_or_a_program_with_no_crate_is_not_drift() {
    // A library crate has no declare_id! and no other leg.
    assert_eq!(rules(&check(vec![id("shared", declare("shared"), "")])), []);
    // Anchor.toml loads a program from `deps/`, so its crate is not in the workspace.
    assert_eq!(rules(&check(vec![id("lockup", toml("localnet"), ID)])), []);
}

/// A fake RPC. `deployed` are program accounts, and each `(program, json)` is an Anchor IDL
/// account of that program.
fn chain(
    deployed: &[&str],
    idls: &[(&str, String)],
) -> impl FnMut(&str, Value) -> Result<Value, onchain::Error> {
    let programs: Vec<(&str, bool)> = deployed.iter().map(|d| (*d, true)).collect();
    let accounts = idls
        .iter()
        .map(|(program, json)| anchor_idl_account(program, json))
        .collect();
    rpc(&programs, accounts)
}

/// A fake RPC. Each `(address, executable)` is an account that `getAccountInfo` gives, and
/// each `(address, account)` is an IDL account.
#[allow(clippy::unwrap_used)]
fn rpc(
    programs: &[(&str, bool)],
    accounts: Vec<(String, Value)>,
) -> impl FnMut(&str, Value) -> Result<Value, onchain::Error> {
    let programs: Vec<(String, bool)> = programs
        .iter()
        .map(|(address, executable)| ((*address).to_string(), *executable))
        .collect();
    move |method, params| {
        let value = match method {
            "getAccountInfo" => {
                let id = params[0].as_str().unwrap();
                json!(programs.iter().find(|(address, _)| address == id).map(
                    |(_, executable)| json!({ "executable": executable, "lamports": 1, "data": ["", "base64"] })
                ))
            }
            "getMultipleAccounts" => params[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| {
                    accounts
                        .iter()
                        .find(|(address, _)| key == address.as_str())
                        .map_or(Value::Null, |(_, account)| account.clone())
                })
                .collect(),
            other => panic!("unexpected RPC method {other}"),
        };
        Ok(json!({ "context": { "slot": 1 }, "value": value }))
    }
}

#[allow(clippy::unwrap_used)]
fn key(address: &str) -> [u8; 32] {
    bs58::decode(address)
        .into_vec()
        .unwrap()
        .try_into()
        .unwrap()
}

/// The address and RPC account of the Anchor IDL account of `program` that holds `json`.
#[allow(clippy::unwrap_used)]
fn anchor_idl_account(program: &str, json: &str) -> (String, Value) {
    let address = bs58::encode(onchain::anchor_idl_address(&key(program)).unwrap()).into_string();
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(json.as_bytes()).unwrap();
    let body = z.finish().unwrap();
    let mut data = vec![24, 70, 98, 191, 58, 144, 123, 158];
    data.extend([0; 32]);
    data.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
    data.extend(body);
    let account = json!({
        "owner": program, "lamports": 1, "executable": false,
        "data": [STANDARD.encode(data), "base64"],
    });
    (address, account)
}

/// The address and RPC account of the canonical Program Metadata IDL account of `program`
/// that holds `json` with no compression, in the layout of the `onchain` unit tests.
#[allow(clippy::unwrap_used)]
fn metadata_idl_account(program: &str, json: &str) -> (String, Value) {
    let address = bs58::encode(onchain::metadata_address(&key(program)).unwrap()).into_string();
    let mut data = vec![2];
    data.extend(key(program));
    data.extend([0; 32]);
    data.extend([1, 1]);
    data.extend(*b"idl\0\0\0\0\0\0\0\0\0\0\0\0\0");
    data.extend([1, 0, 1, 0]);
    data.extend(u32::try_from(json.len()).unwrap().to_le_bytes());
    data.extend([0; 5]);
    data.extend(json.as_bytes());
    let account = json!({
        "owner": onchain::METADATA_PROGRAM, "lamports": 1, "executable": false,
        "data": [STANDARD.encode(data), "base64"],
    });
    (address, account)
}

fn idl_json(address: &str) -> String {
    format!(
        r#"{{"address":"{address}","metadata":{{"name":"amm","version":"0.1.0","spec":"0.1.0"}},"instructions":[]}}"#
    )
}

#[allow(clippy::unwrap_used)]
fn onchain_report(
    ids: Vec<ProgramId>,
    rpc: impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
) -> SourceReport {
    let mut report = check(ids);
    check_onchain(&mut report, rpc).unwrap();
    report
}

#[test]
fn program_not_deployed_at_its_declare_id_is_breaking() {
    let report = onchain_report(vec![id("amm", declare("amm"), ID)], chain(&[], &[]));
    assert_eq!(
        rules(&report),
        [(FindingKind::OnchainMissing, Severity::Breaking, "amm@chain")]
    );
}

#[test]
fn deployed_program_with_no_idl_account_is_dangerous() {
    let report = onchain_report(vec![id("amm", declare("amm"), ID)], chain(&[ID], &[]));
    assert_eq!(
        rules(&report),
        [(
            FindingKind::OnchainIdlMissing,
            Severity::Dangerous,
            "amm@chain"
        )]
    );
}

#[test]
fn onchain_idl_with_another_address_is_breaking() {
    let rpc = chain(&[ID], &[(ID, idl_json(OTHER))]);
    let report = onchain_report(vec![id("amm", declare("amm"), ID)], rpc);
    assert_eq!(
        rules(&report),
        [(
            FindingKind::OnchainMismatch,
            Severity::Breaking,
            "amm@chain"
        )]
    );
    assert_eq!(report.counts(), (1, 0, 0, 0));
    assert!(report.fails_at(Severity::Breaking));
    let onchain = report.ids.last().map(|i| (&i.leg, i.id.as_str()));
    assert!(
        matches!(onchain, Some((Leg::Onchain { source, .. }, OTHER)) if source == "anchor"),
        "{onchain:?}"
    );
}

#[test]
fn onchain_idl_that_diff_cannot_load_still_gives_its_address() {
    let spec_2 = idl_json(ID).replace(r#""spec":"0.1.0""#, r#""spec":"0.2.0""#);
    let report = onchain_report(
        vec![id("amm", declare("amm"), ID)],
        chain(&[ID], &[(ID, spec_2)]),
    );
    assert_eq!(rules(&report), []);
    assert_eq!(report.ids.last().map(|i| i.id.as_str()), Some(ID));
}

#[test]
fn onchain_idl_with_no_readable_address_is_breaking() {
    let ids = || vec![id("amm", declare("amm"), ID)];
    let no_name = format!(r#"{{"address":"{OTHER}","instructions":[]}}"#);
    let report = onchain_report(ids(), chain(&[ID], &[(ID, no_name)]));
    assert_eq!(
        rules(&report),
        [(
            FindingKind::OnchainMismatch,
            Severity::Breaking,
            "amm@chain"
        )]
    );
    // A legacy IDL has no address key, so the program is its address.
    let legacy = r#"{"version":"0.1.0","name":"amm","instructions":[]}"#.to_string();
    let report = onchain_report(ids(), chain(&[ID], &[(ID, legacy)]));
    assert_eq!(rules(&report), []);
}

#[test]
fn one_deployed_cfg_id_is_enough() {
    let ids = vec![
        id("amm", declare("amm"), ID),
        id("amm", declare("amm"), OTHER),
    ];
    let report = onchain_report(ids, chain(&[OTHER], &[]));
    assert_eq!(
        rules(&report),
        [
            (FindingKind::DeclareIdDuplicate, Severity::Dangerous, "amm"),
            (
                FindingKind::OnchainIdlMissing,
                Severity::Dangerous,
                "amm@chain"
            ),
        ]
    );
}

#[test]
fn account_at_the_declare_id_that_is_not_a_program_is_breaking() {
    let report = onchain_report(
        vec![id("amm", declare("amm"), ID)],
        rpc(&[(ID, false)], Vec::new()),
    );
    assert_eq!(
        rules(&report),
        [(FindingKind::OnchainMissing, Severity::Breaking, "amm@chain")]
    );
}

#[test]
fn each_of_two_onchain_idl_accounts_is_checked() {
    let accounts = vec![
        metadata_idl_account(ID, &idl_json(ID)),
        anchor_idl_account(ID, &idl_json(OTHER)),
    ];
    let report = onchain_report(
        vec![id("amm", declare("amm"), ID)],
        rpc(&[(ID, true)], accounts),
    );
    assert_eq!(
        rules(&report),
        [(
            FindingKind::OnchainMismatch,
            Severity::Breaking,
            "amm@chain"
        )]
    );
    // The Metadata account gives ID and the Anchor one gives OTHER.
    let onchain: Vec<&str> = report.ids[1..].iter().map(|i| i.id.as_str()).collect();
    assert_eq!(onchain, [ID, OTHER]);
}

#[test]
fn onchain_idl_with_an_empty_address_is_breaking() {
    let empty = idl_json("");
    let report = onchain_report(
        vec![id("amm", declare("amm"), ID)],
        chain(&[ID], &[(ID, empty)]),
    );
    assert_eq!(
        rules(&report),
        [(
            FindingKind::OnchainMismatch,
            Severity::Breaking,
            "amm@chain"
        )]
    );
}

#[test]
fn more_declare_ids_than_a_cfg_set_needs_are_not_checked_on_chain() {
    let ids = (0u8..17)
        .map(|i| id("amm", declare("amm"), &bs58::encode([i; 32]).into_string()))
        .collect();
    let mut report = check(ids);
    let result = check_onchain(&mut report, |method, _| panic!("{method} was sent"));
    assert!(result.is_ok());
    assert_eq!(
        rules(&report),
        [(FindingKind::DeclareIdDuplicate, Severity::Breaking, "amm")]
    );
}

#[test]
#[allow(clippy::unwrap_used)]
fn idl_address_that_is_not_a_string_is_breaking() {
    let dir = std::env::temp_dir().join(format!("idl-drift-source-address-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cases = [
        ("empty", r#"{"address":"","metadata":{"name":"amm"}}"#),
        ("legacy", r#"{"name":"amm","metadata":{"address":null}}"#),
        (
            "codama",
            r#"{"kind":"rootNode","program":{"kind":"programNode","name":"amm","publicKey":[1,2]}}"#,
        ),
    ];
    let mut reports = Vec::new();
    for (name, json) in cases {
        let path = dir.join(format!("{name}.json"));
        std::fs::write(&path, json).unwrap();
        let leg = read_idl(&path);
        reports.push((path.display().to_string(), leg));
    }
    std::fs::remove_dir_all(&dir).unwrap();
    for (path, leg) in reports {
        let report = check(vec![id("amm", declare("amm"), ID), leg.unwrap()]);
        assert_eq!(
            rules(&report),
            [(FindingKind::InvalidId, Severity::Breaking, path.as_str())]
        );
    }
}

#[cfg(unix)]
#[test]
fn idl_that_is_not_a_regular_file_is_an_error() {
    let read = read_idl(Path::new("/dev/null"));
    assert!(
        matches!(read, Err(SourceError::NotAFile { .. })),
        "{read:?}"
    );
}

#[test]
#[allow(clippy::unwrap_used)]
fn the_crate_root_is_the_lib_path_that_cargo_builds() {
    let root = std::env::temp_dir().join(format!("idl-drift-source-lib-{}", std::process::id()));
    let src = root.join("programs/amm/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        root.join("Anchor.toml"),
        format!("[programs.localnet]\namm = \"{ID}\"\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("programs/amm/Cargo.toml"),
        "[package]\nname = \"amm\"\n\n[lib]\npath = \"src/program.rs\"\n",
    )
    .unwrap();
    std::fs::write(src.join("lib.rs"), format!("declare_id!(\"{OTHER}\");")).unwrap();
    std::fs::write(src.join("program.rs"), format!("declare_id!(\"{ID}\");")).unwrap();
    let ids = discover(&root);
    std::fs::remove_dir_all(&root).unwrap();
    let report = check(ids.unwrap());
    assert_eq!(rules(&report), []);
    assert!(report.compared());
}

#[cfg(unix)]
#[test]
#[allow(clippy::unwrap_used)]
fn idl_symlink_out_of_the_root_is_an_error() {
    let dir = std::env::temp_dir().join(format!("idl-drift-source-{}", std::process::id()));
    let root = dir.join("root");
    std::fs::create_dir_all(root.join("target/idl")).unwrap();
    std::fs::write(dir.join("outside.json"), idl_json(ID)).unwrap();
    std::os::unix::fs::symlink(dir.join("outside.json"), root.join("target/idl/amm.json")).unwrap();
    let file = discover(&root);
    std::fs::remove_file(root.join("target/idl/amm.json")).unwrap();
    // A symlinked directory must not list the files out of ROOT, or name them.
    std::fs::create_dir_all(dir.join("secret/runner-creds")).unwrap();
    std::os::unix::fs::symlink(dir.join("secret"), root.join("programs")).unwrap();
    let listed = discover(&root);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        matches!(file, Err(SourceError::OutsideRoot { .. })),
        "{file:?}"
    );
    assert!(
        matches!(&listed, Err(SourceError::OutsideRoot { path }) if path.ends_with("programs")),
        "{listed:?}"
    );
}
