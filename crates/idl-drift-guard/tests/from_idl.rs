//! `Guard::from_idl` on real IDLs. The sizes are the sizes of the live samples next to each
//! IDL, which idl-verify also reports as the expected size.

use std::collections::BTreeMap;

use idl_drift_guard::{Guard, Kind, Outcome, Update, Verdict};

#[allow(clippy::unwrap_used)]
fn fixture(path: &str) -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/");
    std::fs::read_to_string(format!("{root}{path}")).unwrap()
}

fn sizes(guard: &Guard, kind: Kind) -> BTreeMap<String, Option<usize>> {
    guard
        .shapes(kind)
        .map(|(name, len)| (name.to_string(), len))
        .collect()
}

#[test]
fn borsh_sizes_of_pump_fun() {
    let guard = Guard::from_idl(&fixture("live/pump_fun/idl.json")).unwrap();
    assert_eq!(guard.name(), "pump");
    assert_eq!(
        bs58::encode(guard.program()).into_string(),
        "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
    );
    let accounts = sizes(&guard, Kind::Account);
    assert_eq!(accounts["BondingCurve"], Some(115));
    assert_eq!(accounts["Global"], Some(1045));
    assert_eq!(accounts["GlobalVolumeAccumulator"], Some(544));
    assert_eq!(accounts["UserVolumeAccumulator"], Some(90));
    let instructions = sizes(&guard, Kind::Instruction);
    assert_eq!(instructions["buy"], Some(25));
    assert_eq!(instructions["sell"], Some(24));
    assert_eq!(instructions["claim_cashback_v2"], Some(8));
    // Strings and options have no fixed size.
    assert_eq!(instructions["create"], None);
    assert_eq!(instructions["update_buyback_config"], None);
    let events = sizes(&guard, Kind::Event);
    assert_eq!(events["TradeEvent"], None);
    // The self-CPI tag, the event discriminator, two pubkeys and a u64.
    assert_eq!(events["ClaimCashbackEvent"], Some(8 + 72));
}

#[test]
fn packed_zero_copy_sizes_of_jupiter_vaults() {
    let guard = Guard::from_idl(&fixture("live/jupiter_vaults/idl.json")).unwrap();
    let accounts = sizes(&guard, Kind::Account);
    for (name, len) in [
        ("Branch", 47),
        ("Position", 71),
        ("Tick", 40),
        ("TickHasDebtArray", 267),
        ("TickIdLiquidation", 57),
        ("VaultConfig", 219),
        ("VaultState", 127),
    ] {
        assert_eq!(accounts[name], Some(len), "{name}");
    }
}

#[test]
fn padded_zero_copy_sizes_of_voltr_vault() {
    let guard = Guard::from_idl(&fixture("live/voltr_vault/idl.json")).unwrap();
    let accounts = sizes(&guard, Kind::Account);
    for (name, len) in [
        ("AdaptorAddReceipt", 152),
        ("Protocol", 110),
        ("StrategyInitReceipt", 192),
        ("Vault", 928),
    ] {
        assert_eq!(accounts[name], Some(len), "{name}");
    }
}

#[test]
fn codama_idl_loads_with_its_address() {
    // Codama keeps no zero-copy flag, so a Borsh decoder from this IDL reads 145 bytes of
    // a 152-byte account. The guard expects what that decoder reads.
    let guard = Guard::from_idl(&fixture("live/voltr_vault/idl.js-codama.json")).unwrap();
    assert_eq!(
        bs58::encode(guard.program()).into_string(),
        "vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8"
    );
    assert_eq!(sizes(&guard, Kind::Account)["adaptorAddReceipt"], Some(145));
}

const BONDING_CURVE: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
const BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

fn bytes(prefix: &[u8], len: usize) -> Vec<u8> {
    let mut data = prefix.to_vec();
    data.resize(len, 0);
    data
}

#[test]
fn each_outcome_from_the_bytes_and_the_verdict() {
    let guard = Guard::from_idl(&fixture("live/pump_fun/idl.json")).unwrap();
    let program = *guard.program();
    let other = [9u8; 32];
    let account = |verdict, data: &[u8]| {
        guard.observe(
            Kind::Account,
            verdict,
            &Update {
                program: Some(&program),
                data,
                ..Update::default()
            },
        )
    };
    let curve = |len| bytes(&BONDING_CURVE, len);
    assert_eq!(account(Verdict::Decoded, &curve(115)), Outcome::Decoded);
    // The pump case: the decoder accepts a grown account and drops the new bytes.
    assert_eq!(account(Verdict::Decoded, &curve(166)), Outcome::ExtraBytes);
    assert_eq!(
        account(Verdict::Decoded, &curve(100)),
        Outcome::SizeMismatch
    );
    assert_eq!(account(Verdict::Failed, &curve(100)), Outcome::ShortData);
    assert_eq!(account(Verdict::Failed, &curve(115)), Outcome::DecodeError);
    assert_eq!(account(Verdict::Failed, &[1, 2, 3]), Outcome::ShortData);
    assert_eq!(
        account(Verdict::Failed, &bytes(&[0xff; 8], 115)),
        Outcome::UnknownDiscriminator
    );
    // A decoder that knows a type the IDL does not: no miss.
    assert_eq!(
        account(Verdict::Decoded, &bytes(&[0xff; 8], 115)),
        Outcome::Decoded
    );
    let foreign = Update {
        program: Some(&other),
        data: &curve(115),
        ..Update::default()
    };
    assert_eq!(
        guard.observe(Kind::Account, Verdict::Failed, &foreign),
        Outcome::Filtered
    );

    let instruction = |verdict, data: &[u8]| {
        guard.observe(
            Kind::Instruction,
            verdict,
            &Update {
                program: Some(&program),
                data,
                ..Update::default()
            },
        )
    };
    assert_eq!(
        instruction(Verdict::Decoded, &bytes(&BUY, 26)),
        Outcome::ExtraBytes
    );
    assert_eq!(
        instruction(Verdict::Failed, &bytes(&BUY, 24)),
        Outcome::ShortData
    );
    let claim_cashback = [226, 214, 246, 33, 7, 242, 147, 229];
    let event = [&EVENT_IX_TAG[..], &claim_cashback].concat();
    assert_eq!(
        instruction(Verdict::Decoded, &bytes(&event, 80)),
        Outcome::Decoded
    );
    let unknown_event = [&EVENT_IX_TAG[..], &[0xff; 8]].concat();
    assert_eq!(
        instruction(Verdict::Failed, &bytes(&unknown_event, 80)),
        Outcome::UnknownDiscriminator
    );

    let snapshot = guard.snapshot();
    assert_eq!(snapshot.accounts.decoded, 2);
    assert_eq!(snapshot.accounts.extra_bytes, 1);
    assert_eq!(snapshot.accounts.size_mismatch, 1);
    assert_eq!(snapshot.accounts.short_data, 2);
    assert_eq!(snapshot.accounts.decode_error, 1);
    assert_eq!(snapshot.accounts.unknown_discriminator, 1);
    assert_eq!(snapshot.accounts.filtered, 1);
    assert_eq!(snapshot.instructions.extra_bytes, 1);
    assert_eq!(snapshot.instructions.short_data, 1);
    // Self-CPI event instructions count as events.
    assert_eq!(snapshot.events.decoded, 1);
    assert_eq!(snapshot.events.unknown_discriminator, 1);
}

#[test]
fn an_expected_length_replaces_the_idl_size() {
    let guard = Guard::from_idl(&fixture("live/pump_fun/idl.json"))
        .unwrap()
        .expect_len(Kind::Account, "BondingCurve", 166)
        .unwrap();
    let program = *guard.program();
    let data = bytes(&BONDING_CURVE, 166);
    let update = Update {
        program: Some(&program),
        data: &data,
        ..Update::default()
    };
    assert_eq!(
        guard.observe(Kind::Account, Verdict::Decoded, &update),
        Outcome::Decoded
    );
    assert!(Guard::from_idl(&fixture("live/pump_fun/idl.json"))
        .unwrap()
        .expect_len(Kind::Account, "Nope", 1)
        .is_err());
}

#[test]
fn a_minimum_length_accepts_longer_data() {
    // Live `buy` instructions have 25 and 26 bytes.
    let guard = Guard::from_idl(&fixture("live/pump_fun/idl.json"))
        .unwrap()
        .expect_min_len(Kind::Instruction, "buy", 25)
        .unwrap();
    let program = *guard.program();
    let buy = |verdict, len| {
        let data = bytes(&BUY, len);
        let update = Update {
            program: Some(&program),
            data: &data,
            ..Update::default()
        };
        guard.observe(Kind::Instruction, verdict, &update)
    };
    assert_eq!(buy(Verdict::Decoded, 25), Outcome::Decoded);
    assert_eq!(buy(Verdict::Decoded, 26), Outcome::Decoded);
    assert_eq!(buy(Verdict::Decoded, 24), Outcome::SizeMismatch);
    assert_eq!(buy(Verdict::Failed, 24), Outcome::ShortData);
}

#[test]
fn bad_idls_are_errors() {
    assert!(Guard::from_idl("not json").is_err());
    let no_address =
        r#"{"address": "", "metadata": {"name": "x", "version": "0.1.0", "spec": "0.1.0"}}"#;
    let error = Guard::from_idl(no_address).unwrap_err().to_string();
    assert!(error.contains("address"), "{error}");
}
