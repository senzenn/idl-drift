//! Real pump_fun samples and crafted cases through a guarded Shipstern parser.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use idl_drift_guard::{guard, Alert, Counts, Guard, Kind, Outcome, Window};
use serde_json::Value;
use shipstern_core::instruction::{InstructionShared, InstructionUpdate};
use shipstern_core::{AccountUpdate, AccountUpdateInfo, KeyBytes, Parser};
use shipstern_guard::{parser::pump, IDL};

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

/// A generated parser does not wait on I/O, so one poll ends it.
fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("the parser waited"),
    }
}

fn key(text: &str) -> [u8; 32] {
    bs58::decode(text).into_vec().unwrap().try_into().unwrap()
}

fn account(owner: [u8; 32], address: [u8; 32], data: Vec<u8>, slot: u64) -> AccountUpdate {
    AccountUpdate {
        account: Some(AccountUpdateInfo {
            pubkey: address.to_vec(),
            owner: owner.to_vec(),
            data,
            ..Default::default()
        }),
        slot,
        ..Default::default()
    }
}

fn instruction(
    program: [u8; 32],
    accounts: &[[u8; 32]],
    data: Vec<u8>,
    slot: u64,
) -> InstructionUpdate {
    InstructionUpdate {
        program: KeyBytes(program),
        accounts: accounts.iter().copied().map(KeyBytes).collect(),
        data,
        shared: Arc::new(InstructionShared {
            slot,
            signature: vec![slot as u8; 64],
            ..Default::default()
        }),
        inner: Vec::new(),
        path: vec![0u32].into(),
        log_range: 0..0,
    }
}

struct Fixture {
    guard: Arc<Guard>,
    alerts: Arc<Mutex<Vec<Alert>>>,
    program: [u8; 32],
}

fn fixture() -> Fixture {
    let alerts = Arc::new(Mutex::new(Vec::new()));
    let sink = alerts.clone();
    let guard = Guard::from_idl(IDL)
        .unwrap()
        .alert_after(3, Window::Updates(100), move |alert| {
            sink.lock().unwrap().push(alert.clone())
        });
    let program = *guard.program();
    Fixture {
        guard: Arc::new(guard),
        alerts,
        program,
    }
}

/// Parse with the bare and the guarded parser; the results must be the same.
fn both<P: Parser>(
    bare: &P,
    guarded: &impl Parser<Input = P::Input, Output = P::Output>,
    input: &P::Input,
) -> String
where
    P::Output: std::fmt::Debug,
{
    let want = format!("{:?}", ready(bare.parse(input)));
    let got = format!("{:?}", ready(guarded.parse(input)));
    assert_eq!(got, want);
    got
}

#[test]
fn real_samples_pass_through_and_count() {
    let Fixture {
        guard: pump,
        alerts,
        program,
    } = fixture();
    let accounts = guard!(shipstern, pump::AccountParser, pump.clone());
    let instructions = guard!(shipstern, pump::InstructionParser, pump.clone());
    assert_eq!(accounts.id(), pump::AccountParser.id());
    assert_eq!(instructions.id(), pump::InstructionParser.id());
    assert_eq!(accounts.prefilter(), pump::AccountParser.prefilter());

    let samples: Value = serde_json::from_str(SAMPLES).unwrap();
    for (i, sample) in samples["accounts"].as_array().unwrap().iter().enumerate() {
        let data = STANDARD.decode(sample["data"].as_str().unwrap()).unwrap();
        let update = account(
            program,
            key(sample["address"].as_str().unwrap()),
            data,
            100 + i as u64,
        );
        assert!(both(&pump::AccountParser, &accounts, &update).starts_with("Ok("));
    }
    for (i, sample) in samples["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let data = STANDARD.decode(sample["data"].as_str().unwrap()).unwrap();
        let keys: Vec<[u8; 32]> = sample["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| key(k.as_str().unwrap()))
            .collect();
        both(
            &pump::InstructionParser,
            &instructions,
            &instruction(program, &keys, data, 200 + i as u64),
        );
    }
    // A `cpi` event sample is the instruction data on chain: the self-CPI tag, then the event.
    for sample in samples["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["source"] == "cpi")
    {
        let data = STANDARD.decode(sample["data"].as_str().unwrap()).unwrap();
        assert_eq!(data[..8], EVENT_IX_TAG);
        both(
            &pump::InstructionParser,
            &instructions,
            &instruction(program, &[program], data, 300),
        );
    }

    let counts = pump.snapshot();
    // Each live account is longer than its IDL type: BondingCurve 166 of 115 bytes, Global
    // 1088 of 1045, GlobalVolumeAccumulator 600 of 544, UserVolumeAccumulator 137 of 90.
    assert_eq!(nonzero(&counts.accounts), [(Outcome::ExtraBytes, 12)]);
    // 3 discriminators that the IDL does not have (14 samples), and 2 `buy` with a byte more.
    assert_eq!(
        nonzero(&counts.instructions),
        [
            (Outcome::Decoded, 25),
            (Outcome::UnknownDiscriminator, 14),
            (Outcome::ExtraBytes, 2)
        ]
    );
    // Shipstern's instruction parser skips self-CPI events; it reads them with the outer one.
    assert_eq!(nonzero(&counts.events), [(Outcome::Filtered, 10)]);

    let alerts = alerts.lock().unwrap().clone();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    let first = &alerts[0].first;
    assert_eq!(alerts[0].misses, 3);
    assert_eq!(
        (first.kind, first.outcome),
        (Kind::Account, Outcome::ExtraBytes)
    );
    assert_eq!(first.type_name.as_deref(), Some("Global"));
    assert_eq!((first.len, first.expected_len), (1088, Some(1045)));
    assert_eq!(first.slot, Some(100));
    assert_eq!(
        first.key.as_deref(),
        Some(&key("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf")[..])
    );
    println!("{}", alerts[0]);

    let text = idl_drift_guard::prometheus([&*pump]);
    assert!(text.contains(
        "idl_drift_guard_updates_total{program=\"6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P\",name=\"pump\",kind=\"account\",outcome=\"extra_bytes\"} 12\n"
    ));
}

#[test]
fn crafted_cases_count_each_miss() {
    let Fixture {
        guard: pump,
        program,
        ..
    } = fixture();
    let accounts = guard!(shipstern, pump::AccountParser, pump.clone());
    let instructions = guard!(shipstern, pump::InstructionParser, pump.clone());
    let samples: Value = serde_json::from_str(SAMPLES).unwrap();
    // A real BondingCurve, 166 bytes.
    let curve = STANDARD
        .decode(samples["accounts"][1]["data"].as_str().unwrap())
        .unwrap();
    let address = [3; 32];
    let parse = |data: Vec<u8>, owner| {
        both(
            &pump::AccountParser,
            &accounts,
            &account(owner, address, data, 7),
        )
    };

    // Cut to the 115 bytes of the IDL type: the guard sees no miss.
    let exact = curve[..115].to_vec();
    assert!(parse(exact.clone(), program).starts_with("Ok("));
    // The same account with 10 bytes appended decodes to the same value; the guard sees them.
    let grown = [&exact[..], &[0xaa; 10]].concat();
    assert_eq!(parse(grown, program), parse(exact, program));
    let unknown = [&[0xff; 8][..], &curve[8..]].concat();
    assert!(parse(unknown, program).starts_with("Err("));
    assert!(parse(curve[..28].to_vec(), program).starts_with("Err("));
    assert!(parse(vec![1, 2, 3], program).starts_with("Err("));
    assert_eq!(parse(curve.clone(), [9; 32]), "Err(Filtered)");

    let sell = STANDARD
        .decode(
            samples["instructions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| {
                    STANDARD.decode(s["data"].as_str().unwrap()).unwrap()[..8]
                        == [51, 230, 133, 164, 1, 127, 131, 173]
                })
                .unwrap()["data"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let keys = [[1; 32]; 16];
    let ix = |data: Vec<u8>, program| {
        both(
            &pump::InstructionParser,
            &instructions,
            &instruction(program, &keys, data, 8),
        )
    };
    assert!(ix(sell.clone(), program).starts_with("Ok("));
    assert!(ix(sell[..12].to_vec(), program).starts_with("Err("));
    assert!(
        ix([&[0xee; 8][..], &sell[8..]].concat(), program).starts_with("Err(DiscriminatorNotFound")
    );
    assert_eq!(ix(sell, [9; 32]), "Err(Filtered)");

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
        nonzero(&counts.instructions),
        [
            (Outcome::Decoded, 1),
            (Outcome::Filtered, 1),
            (Outcome::UnknownDiscriminator, 1),
            (Outcome::ShortData, 1)
        ]
    );
}

// Names of the caller: the items of the macro must not hide them.
struct Guarded;
struct GuardInput;

#[test]
fn the_macro_keeps_the_names_of_the_caller() {
    let Fixture { guard: pump, .. } = fixture();
    let parser = guard!(
        shipstern,
        {
            let _: (Guarded, GuardInput) = (Guarded, GuardInput);
            pump::AccountParser
        },
        pump.clone()
    );
    assert_eq!(parser.id(), pump::AccountParser.id());
}

#[test]
fn a_guard_for_another_program_is_reported_at_once() {
    let wrong = Arc::new(Guard::from_idl_for([9; 32], IDL).unwrap());
    let _parser = guard!(shipstern, pump::AccountParser, wrong.clone());
    assert_eq!(
        wrong.program_mismatch(),
        Some(&key("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"))
    );
}
