//! Real pump_fun samples and crafted cases through a guarded Carbon decoder.

use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use carbon_core::account::AccountDecoder;
use carbon_core::instruction::InstructionDecoder;
use carbon_guard::IDL;
use carbon_pumpfun_decoder::PumpfunDecoder;
use idl_drift_guard::{guard, Alert, Counts, Guard, Kind, Outcome, Window};
use serde_json::Value;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};

const SAMPLES: &str = include_str!("../../../../fixtures/live/pump_fun/samples-2026-10-16.json");
const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// The outcomes with a count, in `Outcome::ALL` order. `Counts` is `#[non_exhaustive]`.
fn nonzero(counts: &Counts) -> Vec<(Outcome, u64)> {
    Outcome::ALL
        .iter()
        .map(|o| (*o, counts.get(*o)))
        .filter(|(_, n)| *n > 0)
        .collect()
}

fn key(text: &str) -> [u8; 32] {
    bs58::decode(text).into_vec().unwrap().try_into().unwrap()
}

fn account(owner: [u8; 32], data: Vec<u8>) -> Account {
    Account {
        lamports: 1,
        data,
        owner: owner.into(),
        executable: false,
        rent_epoch: 0,
    }
}

fn instruction(program: [u8; 32], keys: &[[u8; 32]], data: Vec<u8>) -> Instruction {
    Instruction {
        program_id: program.into(),
        accounts: keys
            .iter()
            .map(|k| AccountMeta::new_readonly((*k).into(), false))
            .collect(),
        data,
    }
}

fn fixture() -> (Arc<Guard>, Arc<Mutex<Vec<Alert>>>, [u8; 32]) {
    let alerts = Arc::new(Mutex::new(Vec::new()));
    let sink = alerts.clone();
    let guard = Guard::from_idl(IDL)
        .unwrap()
        .alert_after(3, Window::Updates(100), move |alert| {
            sink.lock().unwrap().push(alert.clone())
        });
    let program = *guard.program();
    (Arc::new(guard), alerts, program)
}

/// Decode with the bare and the guarded decoder; the results must be the same.
fn decode_account(
    guarded: &impl for<'a> AccountDecoder<
        'a,
        AccountType = carbon_pumpfun_decoder::accounts::PumpfunAccount,
    >,
    account: &Account,
) -> Option<String> {
    let want = PumpfunDecoder
        .decode_account(account)
        .map(|d| format!("{d:?}"));
    let got = guarded.decode_account(account).map(|d| format!("{d:?}"));
    assert_eq!(got, want);
    got
}

fn decode_instruction(
    guarded: &impl for<'a> InstructionDecoder<
        'a,
        InstructionType = carbon_pumpfun_decoder::instructions::PumpfunInstruction,
    >,
    instruction: &Instruction,
) -> Option<String> {
    let want = PumpfunDecoder
        .decode_instruction(instruction)
        .map(|d| format!("{d:?}"));
    let got = guarded
        .decode_instruction(instruction)
        .map(|d| format!("{d:?}"));
    assert_eq!(got, want);
    got
}

#[test]
fn real_samples_pass_through_and_count() {
    let (pump, alerts, program) = fixture();
    let decoder = guard!(carbon, PumpfunDecoder, pump.clone());
    let samples: Value = serde_json::from_str(SAMPLES).unwrap();
    for sample in samples["accounts"].as_array().unwrap() {
        let data = STANDARD.decode(sample["data"].as_str().unwrap()).unwrap();
        assert!(decode_account(&decoder, &account(program, data)).is_some());
    }
    for sample in samples["instructions"].as_array().unwrap() {
        let data = STANDARD.decode(sample["data"].as_str().unwrap()).unwrap();
        let keys: Vec<[u8; 32]> = sample["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| key(k.as_str().unwrap()))
            .collect();
        decode_instruction(&decoder, &instruction(program, &keys, data));
    }
    // A `cpi` event sample is the instruction data on chain: the self-CPI tag, then the event.
    let event_authority = [7; 32];
    for sample in samples["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["source"] == "cpi")
    {
        let data = STANDARD.decode(sample["data"].as_str().unwrap()).unwrap();
        assert_eq!(data[..8], EVENT_IX_TAG);
        assert!(
            decode_instruction(&decoder, &instruction(program, &[event_authority], data)).is_some()
        );
    }

    let counts = pump.snapshot();
    // Carbon decodes each live account and drops the bytes after its type, in silence.
    assert_eq!(nonzero(&counts.accounts), [(Outcome::ExtraBytes, 12)]);
    // Carbon returns `None` for the 3 discriminators that it does not know (14 samples).
    assert_eq!(
        nonzero(&counts.instructions),
        [
            (Outcome::Decoded, 25),
            (Outcome::UnknownDiscriminator, 14),
            (Outcome::ExtraBytes, 2)
        ]
    );
    // `TradeEvent` has a string, so the IDL fixes no length for it.
    assert_eq!(nonzero(&counts.events), [(Outcome::Decoded, 10)]);

    let alerts = alerts.lock().unwrap().clone();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    let first = &alerts[0].first;
    assert_eq!(
        (first.kind, first.outcome),
        (Kind::Account, Outcome::ExtraBytes)
    );
    assert_eq!(first.type_name.as_deref(), Some("Global"));
    // Carbon decoders do not see the slot or the account address.
    assert_eq!((first.slot, first.key.clone()), (None, None));
    println!("{}", alerts[0]);
}

#[test]
fn crafted_cases_count_each_miss() {
    let (pump, _, program) = fixture();
    let decoder = guard!(carbon, PumpfunDecoder, pump.clone());
    let samples: Value = serde_json::from_str(SAMPLES).unwrap();
    let curve = STANDARD
        .decode(samples["accounts"][1]["data"].as_str().unwrap())
        .unwrap();
    let decode = |data: Vec<u8>, owner| decode_account(&decoder, &account(owner, data));

    let exact = curve[..115].to_vec();
    assert!(decode(exact.clone(), program).is_some());
    let grown = [&exact[..], &[0xaa; 10]].concat();
    assert_eq!(decode(grown, program), decode(exact, program));
    assert!(decode([&[0xff; 8][..], &curve[8..]].concat(), program).is_none());
    assert!(decode(curve[..28].to_vec(), program).is_none());
    assert!(decode(vec![1, 2, 3], program).is_none());
    assert!(decode(curve.clone(), [9; 32]).is_none());

    let claim = samples["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| STANDARD.decode(e["data"].as_str().unwrap()).unwrap())
        .find(|e| e[..8] == [226, 214, 246, 33, 7, 242, 147, 229])
        .unwrap();
    let ix = |data: Vec<u8>| decode_instruction(&decoder, &instruction(program, &[[7; 32]], data));
    assert!(ix([&EVENT_IX_TAG[..], &claim].concat()).is_some());
    assert!(ix([&EVENT_IX_TAG[..], &claim[..20]].concat()).is_none());
    assert!(ix([&EVENT_IX_TAG[..], &[0xff; 8], &claim[8..]].concat()).is_none());

    let counts = pump.snapshot();
    assert_eq!(
        nonzero(&counts.accounts),
        [
            (Outcome::Decoded, 2),
            (Outcome::Filtered, 1),
            (Outcome::UnknownDiscriminator, 1),
            (Outcome::ShortData, 2),
            (Outcome::ExtraBytes, 1)
        ]
    );
    assert_eq!(
        nonzero(&counts.events),
        [
            (Outcome::Decoded, 1),
            (Outcome::UnknownDiscriminator, 1),
            (Outcome::ShortData, 1)
        ]
    );
}

// A name of the caller: the items of the macro must not hide it.
struct Guarded;

#[test]
fn the_macro_keeps_the_names_of_the_caller() {
    let (pump, _, program) = fixture();
    let decoder = guard!(
        carbon,
        {
            let _: Guarded = Guarded;
            PumpfunDecoder
        },
        pump.clone()
    );
    assert!(decode_account(&decoder, &account(program, vec![1, 2, 3])).is_none());
}

#[test]
fn a_guard_for_another_program_is_reported_at_the_first_value() {
    let wrong = Arc::new(Guard::from_idl_for([9; 32], IDL).unwrap());
    let decoder = guard!(carbon, PumpfunDecoder, wrong.clone());
    let samples: Value = serde_json::from_str(SAMPLES).unwrap();
    let curve = STANDARD
        .decode(samples["accounts"][1]["data"].as_str().unwrap())
        .unwrap();
    let pump = key("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
    assert!(decode_account(&decoder, &account(pump, curve)).is_some());
    assert_eq!(wrong.program_mismatch(), Some(&pump));
    assert_eq!(
        nonzero(&wrong.snapshot().accounts),
        [(Outcome::Filtered, 1)]
    );
}
