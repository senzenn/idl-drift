//! Samples taken from a live cluster over JSON-RPC.

mod transaction;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use idl_drift::model::Idl;

use crate::classify::{
    account_table, cpi_envelopes, event_body, event_table, instruction_table, longest_match,
    DiscTable,
};
use crate::error::Error;
use crate::samples::{AccountSample, EventSource, Samples};
use crate::text::prefix_hex;

pub use transaction::{samples_from_transaction, TransactionSamples};

/// How much to take from a live cluster.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Successful transactions to read at most; failed ones are skipped unread.
    pub max_transactions: usize,
    /// Stop after this many successful transactions in a row bring no new instruction or
    /// event kind.
    pub stall: usize,
    /// Samples per instruction discriminator, per event discriminator, and per unknown prefix.
    pub per_instruction: usize,
    /// Accounts per IDL account type, and per unknown discriminator.
    pub per_type: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_transactions: 40,
            stall: 40,
            per_instruction: 5,
            per_type: 5,
        }
    }
}

/// Why a live sampling run stopped reading transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Every IDL instruction with a discriminator has its quota of samples.
    Quota,
    /// `stall` transactions in a row brought no new instruction or event kind.
    Stall,
    /// `max_transactions` successful transactions were read.
    MaxTransactions,
    /// The program has no older signatures.
    History,
    /// Older transactions ran the code from before the program's last deploy, in this slot.
    Upgrade(u64),
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Quota => "every IDL instruction has its quota of samples",
            Self::Stall => "the last --stall transactions brought no new kind",
            Self::MaxTransactions => "--max-tx transactions were read",
            Self::History => "the program has no older transactions",
            Self::Upgrade(slot) => {
                return write!(
                    f,
                    "older transactions ran the program before its last deploy, in slot {slot}"
                );
            }
        })
    }
}

/// What a live sampling run took, and why it stopped.
#[derive(Debug)]
pub struct Sampled {
    pub samples: Samples,
    /// Successful transactions read.
    pub transactions: usize,
    pub stop_reason: StopReason,
}

/// Samples of `program_id` from successful transactions, newest first, and the accounts they
/// pass. Pages back through the program's history until a [`StopReason`] holds. `rpc` sends
/// one JSON-RPC request and returns its `result`.
///
/// # Errors
///
/// When a request fails, so that a run never passes on samples it could not fetch.
pub fn sample(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, Error>,
    idl: &Idl,
    program_id: &str,
    limits: &Limits,
) -> Result<Sampled, Error> {
    let mut quota = Quota::new(idl, limits.per_instruction);
    let cpi = cpi_envelopes(idl);
    // Transactions from before the last deploy ran other code, so they give false findings.
    let deployed = deploy_slot(&mut rpc, program_id)?;
    let mut instructions = Vec::new();
    let mut events = Vec::new();
    // Accounts of instructions over quota are still sampled.
    let mut keys: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut read = 0usize;
    let mut quiet = 0usize;
    let mut before: Option<String> = None;

    let stop_reason = 'pages: loop {
        if read >= limits.max_transactions {
            break StopReason::MaxTransactions;
        }
        let mut config = json!({ "limit": SIGNATURES_PER_PAGE });
        if let Some(signature) = &before {
            config["before"] = json!(signature);
        }
        let page = rpc("getSignaturesForAddress", json!([program_id, config]))?;
        let page = Vec::<SignatureInfo>::deserialize(&page)
            .map_err(|e| Error::RpcResponse(format!("getSignaturesForAddress: {e}")))?;
        // An RPC that ignores `before` would return the same page forever.
        match page.last() {
            Some(last) if before.as_deref() != Some(last.signature.as_str()) => {
                before = Some(last.signature.clone());
            }
            _ => break StopReason::History,
        }

        // Failed transactions can carry data the program rejected, which is not a fault of the IDL.
        for info in page.iter().filter(|s| s.err.is_none()) {
            // Agave runs new code from the slot after its deploy.
            if let (Some(slot), Some(deployed)) = (info.slot, deployed) {
                if slot <= deployed {
                    break 'pages StopReason::Upgrade(deployed);
                }
            }
            // The highest version this parser reads; the RPC refuses newer ones with an error.
            let tx = rpc(
                "getTransaction",
                json!([info.signature, { "encoding": "json", "maxSupportedTransactionVersion": 1 }]),
            )?;
            // A node that no longer keeps the transaction returns null.
            if tx.is_null() {
                continue;
            }
            read += 1;
            let found = samples_from_transaction(idl, program_id, &tx)?;
            let mut new = false;
            for ix in found.instructions {
                for key in &ix.accounts {
                    if seen.insert(key.clone()) {
                        keys.push(key.clone());
                    }
                }
                let (keep, fresh) = quota.take(Group::Instruction, &ix.data);
                new |= fresh;
                if keep {
                    instructions.push(ix);
                }
            }
            for event in found.events {
                let (keep, fresh) = match event.source {
                    EventSource::Log => quota.take(Group::LogEvent, &event.data),
                    EventSource::Cpi => quota.take(
                        Group::CpiEvent,
                        event_body(&event, &cpi).unwrap_or_default(),
                    ),
                };
                new |= fresh;
                if keep {
                    events.push(event);
                }
            }
            quiet = if new { 0 } else { quiet + 1 };
            if quota.filled() {
                break 'pages StopReason::Quota;
            }
            if quiet >= limits.stall {
                break 'pages StopReason::Stall;
            }
            if read >= limits.max_transactions {
                break 'pages StopReason::MaxTransactions;
            }
        }
    };

    let accounts = fetch_accounts(&mut rpc, idl, program_id, &keys, limits.per_type)?;

    Ok(Sampled {
        samples: Samples {
            program_id: Some(program_id.to_string()),
            accounts,
            instructions,
            events,
        },
        transactions: read,
        stop_reason,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    Instruction,
    LogEvent,
    CpiEvent,
}

/// Caps the samples of each kind: an IDL discriminator, or the first bytes of data that
/// match none.
struct Quota<'a> {
    instructions: DiscTable<'a>,
    events: DiscTable<'a>,
    per_kind: usize,
    taken: HashMap<(Group, Option<usize>, String), usize>,
    /// IDL instructions whose discriminator has its quota.
    full: HashSet<usize>,
}

impl<'a> Quota<'a> {
    fn new(idl: &'a Idl, per_kind: usize) -> Self {
        Self {
            instructions: instruction_table(idl),
            events: event_table(idl),
            per_kind,
            taken: HashMap::new(),
            full: HashSet::new(),
        }
    }

    /// Whether to keep a sample of `data`, and whether its kind is new.
    fn take(&mut self, group: Group, data: &[u8]) -> (bool, bool) {
        let table = match group {
            Group::Instruction => &self.instructions,
            Group::LogEvent | Group::CpiEvent => &self.events,
        };
        let matched = longest_match(table, data);
        let (index, prefix) = kind(matched.as_deref(), data);
        let mut new = false;
        let count = self.taken.entry((group, index, prefix)).or_insert_with(|| {
            new = true;
            0
        });
        let keep = *count < self.per_kind;
        if keep {
            *count += 1;
        }
        if group == Group::Instruction && *count >= self.per_kind {
            self.full.extend(matched.into_iter().flatten());
        }
        (keep, new)
    }

    /// An IDL with no instruction discriminators never fills, so it never stops a run.
    fn filled(&self) -> bool {
        !self.instructions.is_empty() && self.full.len() == self.instructions.len()
    }
}

/// The accounts among `keys` that `program_id` owns, from `getMultipleAccounts` values.
/// Keeps at most `per_type` of each IDL account type and of each unknown discriminator.
///
/// # Errors
///
/// When an account does not have the shape the RPC documents.
pub fn accounts_from_rpc(
    idl: &Idl,
    program_id: &str,
    keys: &[String],
    values: &[Value],
    per_type: usize,
) -> Result<Vec<AccountSample>, Error> {
    let table = account_table(idl);
    // Unknown discriminators are kept too: they are what the discriminator check reports.
    let mut taken: HashMap<(Option<usize>, String), usize> = HashMap::new();
    let mut out = Vec::new();
    for (key, value) in keys.iter().zip(values) {
        let Some(account) = Option::<RpcAccount>::deserialize(value)
            .map_err(|e| Error::RpcResponse(format!("account {key}: {e}")))?
        else {
            continue;
        };
        if account.owner != program_id {
            continue;
        }
        let data = STANDARD
            .decode(&account.data.0)
            .map_err(|e| Error::RpcResponse(format!("account {key}: data: {e}")))?;
        let count = taken
            .entry(kind(longest_match(&table, &data).as_deref(), &data))
            .or_default();
        if *count >= per_type {
            continue;
        }
        *count += 1;
        out.push(AccountSample {
            address: key.clone(),
            data,
        });
    }
    Ok(out)
}

/// The rent-exempt minimum of an account with `data_len` bytes of data, from
/// `getMinimumBalanceForRentExemption`.
///
/// # Errors
///
/// When the request fails, or its result is not a number of lamports.
pub fn rent_exempt_minimum(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, Error>,
    data_len: usize,
) -> Result<u64, Error> {
    let result = rpc("getMinimumBalanceForRentExemption", json!([data_len]))?;
    result.as_u64().ok_or_else(|| {
        Error::RpcResponse(format!(
            "getMinimumBalanceForRentExemption: expected lamports, got {result}"
        ))
    })
}

/// What a sample counts against: the first IDL entry its discriminator matches, else the
/// first bytes of its data.
fn kind(matched: Option<&[usize]>, data: &[u8]) -> (Option<usize>, String) {
    match matched.and_then(<[usize]>::first) {
        Some(&index) => (Some(index), String::new()),
        None => (None, prefix_hex(data)),
    }
}

/// The accounts among `keys` that `program_id` owns, as [`accounts_from_rpc`] keeps them.
/// Owners come first, with no data: an account of another program, such as a program's
/// code, can hold 10 MiB, and only the program's own accounts are checked.
fn fetch_accounts(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    idl: &Idl,
    program_id: &str,
    keys: &[String],
    per_type: usize,
) -> Result<Vec<AccountSample>, Error> {
    let mut owned = Vec::new();
    for (key, value) in keys.iter().zip(multiple_accounts(rpc, keys, Some(0))?) {
        let account = Option::<RpcAccount>::deserialize(&value)
            .map_err(|e| Error::RpcResponse(format!("account {key}: {e}")))?;
        if account.is_some_and(|a| a.owner == program_id) {
            owned.push(key.clone());
        }
    }
    let values = multiple_accounts(rpc, &owned, None)?;
    accounts_from_rpc(idl, program_id, &owned, &values, per_type)
}

/// `getMultipleAccounts` values of `keys`, in order. With `data_len`, each holds only that
/// many bytes of data.
fn multiple_accounts(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    keys: &[String],
    data_len: Option<usize>,
) -> Result<Vec<Value>, Error> {
    let mut config = json!({ "encoding": "base64" });
    if let Some(length) = data_len {
        config["dataSlice"] = json!({ "offset": 0, "length": length });
    }
    let mut values = Vec::new();
    for chunk in keys.chunks(MAX_ACCOUNTS_PER_REQUEST) {
        let page = rpc("getMultipleAccounts", json!([chunk, config]))?;
        let page = AccountsPage::deserialize(&page)
            .map_err(|e| Error::RpcResponse(format!("getMultipleAccounts: {e}")))?;
        values.extend(page.value);
    }
    Ok(values)
}

const LOADER_V3: &str = "BPFLoaderUpgradeab1e11111111111111111111111";
const LOADER_V4: &str = "LoaderV411111111111111111111111111111111111";

/// The slot of the program's last deploy. `None` for a loader that keeps no slot, or a
/// program the RPC does not have.
fn deploy_slot(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    program_id: &str,
) -> Result<Option<u64>, Error> {
    let mut first_bytes =
        |address: &str, length: usize| -> Result<Option<(String, Vec<u8>)>, Error> {
            let config =
                json!({ "encoding": "base64", "dataSlice": { "offset": 0, "length": length } });
            let reply = rpc("getAccountInfo", json!([address, config]))?;
            let Some(account) = Option::<RpcAccount>::deserialize(&reply["value"])
                .map_err(|e| Error::RpcResponse(format!("getAccountInfo {address}: {e}")))?
            else {
                return Ok(None);
            };
            let data = STANDARD
                .decode(&account.data.0)
                .map_err(|e| Error::RpcResponse(format!("getAccountInfo {address}: data: {e}")))?;
            Ok(Some((account.owner, data)))
        };
    let slot_at = |data: &[u8], at: usize| {
        data.get(at..at + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
    };
    let Some((owner, data)) = first_bytes(program_id, 36)? else {
        return Ok(None);
    };
    match owner.as_str() {
        // `LoaderV4State` starts with the slot.
        LOADER_V4 => Ok(slot_at(&data, 0)),
        // `UpgradeableLoaderState::Program` (tag 2) names its ProgramData account, which
        // (tag 3) holds the slot after the tag.
        LOADER_V3 if data.get(..4) == Some(&[2, 0, 0, 0]) => {
            let Some(address) = data.get(4..36).map(|k| bs58::encode(k).into_string()) else {
                return Ok(None);
            };
            Ok(first_bytes(&address, 12)?
                .filter(|(_, d)| d.get(..4) == Some(&[3, 0, 0, 0]))
                .and_then(|(_, d)| slot_at(&d, 4)))
        }
        _ => Ok(None),
    }
}

/// The `getSignaturesForAddress` maximum per request.
const SIGNATURES_PER_PAGE: usize = 1000;

/// One entry of `getSignaturesForAddress`; `err` is set when the transaction failed.
#[derive(Deserialize)]
struct SignatureInfo {
    signature: String,
    #[serde(default)]
    err: Option<Value>,
    #[serde(default)]
    slot: Option<u64>,
}

/// The `getMultipleAccounts` limit on keys per request.
const MAX_ACCOUNTS_PER_REQUEST: usize = 100;

#[derive(Deserialize)]
struct AccountsPage {
    value: Vec<Value>,
}

/// An account from `getMultipleAccounts`; `data` is `[<base64>, "base64"]`.
#[derive(Deserialize)]
struct RpcAccount {
    owner: String,
    data: (String, String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tx;

    /// One account type, `Vault`, with discriminator [9, 9].
    fn idl() -> Idl {
        crate::testing::idl(r#""accounts":[{"name":"Vault","discriminator":[9,9]}]"#)
    }

    /// `rpc` with no program account, so that a run has no deploy slot.
    fn no_program(
        mut rpc: impl FnMut(&str, Value) -> Result<Value, Error>,
    ) -> impl FnMut(&str, Value) -> Result<Value, Error> {
        move |method, params| match method {
            "getAccountInfo" => Ok(json!({ "value": null })),
            _ => rpc(method, params),
        }
    }

    fn account(owner: &str, data: &[u8]) -> Value {
        let data = STANDARD.encode(data);
        json!({ "owner": owner, "data": [data, "base64"], "lamports": 1, "executable": false })
    }

    #[test]
    fn keeps_only_accounts_the_program_owns() {
        let keys = ["A", "B", "C"].map(String::from);
        let values = [
            account("Prog", &[9, 9, 1]),
            Value::Null,
            account("Other", &[9, 9, 2]),
        ];
        let accounts = accounts_from_rpc(&idl(), "Prog", &keys, &values, 5).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].address, "A");
        assert_eq!(accounts[0].data, [9, 9, 1]);
    }

    #[test]
    fn keeps_per_type_samples_including_unknown_types() {
        let data: [&[u8]; 7] = [
            &[9, 9, 0],
            &[9, 9, 1],
            &[9, 9, 2],
            &[7, 7, 7, 7, 7, 7, 7, 7, 0],
            &[7, 7, 7, 7, 7, 7, 7, 7, 1],
            &[7, 7, 7, 7, 7, 7, 7, 7, 2],
            &[5, 5],
        ];
        let keys: Vec<String> = (0..data.len()).map(|i| format!("K{i}")).collect();
        let values: Vec<Value> = data.iter().map(|d| account("Prog", d)).collect();
        let accounts = accounts_from_rpc(&idl(), "Prog", &keys, &values, 2).unwrap();
        let addresses: Vec<&str> = accounts.iter().map(|a| a.address.as_str()).collect();
        assert_eq!(addresses, ["K0", "K1", "K3", "K4", "K6"]);
    }

    #[test]
    fn a_failed_request_fails_the_run() {
        let rpc = |method: &str, _: Value| match method {
            "getSignaturesForAddress" => Ok(json!([{ "signature": "S1", "err": null }])),
            _ => Err(Error::RpcResponse(format!(
                "{method}: 429 Too Many Requests"
            ))),
        };
        let err = sample(no_program(rpc), &idl(), "Prog", &Limits::default()).unwrap_err();
        assert!(err.to_string().contains("getTransaction: 429"), "{err}");
    }

    #[test]
    fn samples_successful_transactions_and_the_accounts_they_touch() {
        let mut calls = Vec::new();
        let rpc = |method: &str, params: Value| {
            calls.push((method.to_string(), params.clone()));
            Ok(match method {
                "getSignaturesForAddress" => json!([
                    { "signature": "S1", "err": null },
                    { "signature": "S2", "err": { "InstructionError": [0, "Custom"] } }
                ]),
                "getTransaction" => tx(
                    &["Payer", "Vault", "Prog"],
                    &json!([{ "programIdIndex": 2, "accounts": [1, 0], "data": "Ldp" }]),
                ),
                "getMultipleAccounts" => json!({ "context": { "slot": 1 }, "value": [
                    account("Prog", &[9, 9, 1]),
                    account("11111111111111111111111111111111", &[]),
                ]}),
                _ => Value::Null,
            })
        };
        let samples = sample(rpc, &idl(), "Prog", &Limits::default())
            .unwrap()
            .samples;

        let fetched: Vec<&Value> = calls
            .iter()
            .filter(|(m, _)| m == "getTransaction")
            .map(|(_, p)| &p[0])
            .collect();
        assert_eq!(fetched, [&json!("S1")]);
        assert_eq!(samples.program_id.as_deref(), Some("Prog"));
        assert_eq!(samples.instructions.len(), 1);
        assert_eq!(samples.instructions[0].accounts, ["Vault", "Payer"]);
        let accounts: Vec<(&str, &[u8])> = samples
            .accounts
            .iter()
            .map(|a| (a.address.as_str(), a.data.as_slice()))
            .collect();
        assert_eq!(accounts, [("Vault", &[9u8, 9, 1][..])]);
        // Owners first with no data, then data only for the program's own accounts.
        let account_requests: Vec<&Value> = calls
            .iter()
            .filter(|(m, _)| m == "getMultipleAccounts")
            .map(|(_, p)| p)
            .collect();
        assert_eq!(
            account_requests,
            [
                &json!([["Vault", "Payer"], { "encoding": "base64", "dataSlice": { "offset": 0, "length": 0 } }]),
                &json!([["Vault"], { "encoding": "base64" }]),
            ]
        );
    }

    #[test]
    fn stops_at_the_last_deploy_and_skips_a_pruned_transaction() {
        let programdata = bs58::encode([7u8; 32]).into_string();
        let mut fetched = Vec::new();
        let rpc = |method: &str, params: Value| {
            Ok(match method {
                "getAccountInfo" if params[0] == "Prog" => json!({ "value":
                    account(LOADER_V3, &[&[2, 0, 0, 0][..], &[7; 32]].concat()) }),
                "getAccountInfo" if params[0] == programdata.as_str() => json!({ "value":
                    account(LOADER_V3, &[&[3, 0, 0, 0][..], &100u64.to_le_bytes()].concat()) }),
                "getSignaturesForAddress" => json!([
                    { "signature": "S1", "err": null, "slot": 102 },
                    { "signature": "S2", "err": null, "slot": 101 },
                    { "signature": "S3", "err": null, "slot": 100 },
                ]),
                "getTransaction" => {
                    fetched.push(params[0].clone());
                    if params[0] == "S1" {
                        Value::Null
                    } else {
                        tx(
                            &["Payer", "Prog"],
                            &json!([{ "programIdIndex": 1, "accounts": [0], "data": "2" }]),
                        )
                    }
                }
                _ => json!({ "value": [] }),
            })
        };
        let sampled = sample(rpc, &idl(), "Prog", &Limits::default()).unwrap();
        // S3 is in the deploy slot, so it ran the old code.
        assert_eq!(fetched, ["S1", "S2"]);
        assert_eq!(sampled.transactions, 1);
        assert_eq!(sampled.stop_reason, StopReason::Upgrade(100));
    }

    #[test]
    fn self_cpi_events_use_the_envelope_the_idl_declares() {
        let idl = crate::testing::idl(
            r#""events":[
                {"name":"A","discriminator":[7],"envelope_discriminator":[254]},
                {"name":"B","discriminator":[8],"envelope_discriminator":[254]}]"#,
        );
        let data = |bytes: &[u8]| bs58::encode(bytes).into_string();
        let rpc = |method: &str, _: Value| {
            Ok(match method {
                "getSignaturesForAddress" => json!([{ "signature": "S1", "err": null }]),
                "getTransaction" => tx(
                    &["Payer", "Prog"],
                    &json!([
                        { "programIdIndex": 1, "accounts": [0], "data": data(&[254, 7, 1]) },
                        { "programIdIndex": 1, "accounts": [0], "data": data(&[254, 8, 2]) },
                        { "programIdIndex": 1, "accounts": [0], "data": data(&[1]) }
                    ]),
                ),
                _ => json!({ "value": [Value::Null] }),
            })
        };
        // One sample per kind: the two events count as two kinds only after the envelope is removed.
        let limits = Limits {
            per_instruction: 1,
            ..Limits::default()
        };
        let samples = sample(no_program(rpc), &idl, "Prog", &limits)
            .unwrap()
            .samples;
        let events: Vec<(EventSource, &[u8])> = samples
            .events
            .iter()
            .map(|e| (e.source, e.data.as_slice()))
            .collect();
        assert_eq!(
            events,
            [
                (EventSource::Cpi, &[254, 7, 1][..]),
                (EventSource::Cpi, &[254, 8, 2][..])
            ]
        );
        assert_eq!(samples.instructions.len(), 1);
    }

    #[test]
    fn the_instruction_quota_keeps_the_accounts_of_dropped_instructions() {
        let rpc = |method: &str, params: Value| {
            Ok(match method {
                "getSignaturesForAddress" => json!([{ "signature": "S1", "err": null }]),
                "getTransaction" => tx(
                    &["A", "B", "Prog"],
                    &json!([
                        { "programIdIndex": 2, "accounts": [0], "data": "2" },
                        { "programIdIndex": 2, "accounts": [1], "data": "2" }
                    ]),
                ),
                _ => json!({ "value": params[0].as_array().unwrap().iter()
                    .map(|_| account("Prog", &[9, 9])).collect::<Vec<_>>() }),
            })
        };
        let limits = Limits {
            per_instruction: 1,
            ..Limits::default()
        };
        let samples = sample(no_program(rpc), &idl(), "Prog", &limits)
            .unwrap()
            .samples;
        assert_eq!(samples.instructions.len(), 1);
        let addresses: Vec<&str> = samples
            .accounts
            .iter()
            .map(|a| a.address.as_str())
            .collect();
        assert_eq!(addresses, ["A", "B"]);
    }

    #[test]
    fn samples_version_1_transactions() {
        let rpc = |method: &str, params: Value| match method {
            "getSignaturesForAddress" => Ok(json!([{ "signature": "S1", "err": null }])),
            // The RPC refuses a transaction newer than the request allows (code -32015).
            "getTransaction" if params[1]["maxSupportedTransactionVersion"].as_u64() < Some(1) => {
                Err(Error::Rpc(
                    "getTransaction: Transaction version (1) is not supported".into(),
                ))
            }
            "getTransaction" => {
                let mut v1 = tx(
                    &["Payer", "Prog"],
                    &json!([{ "programIdIndex": 1, "accounts": [0], "data": "2" }]),
                );
                v1["version"] = json!(1);
                v1["transaction"]["message"]["transactionConfig"] = json!({});
                Ok(v1)
            }
            _ => Ok(json!({ "value": [Value::Null] })),
        };
        let samples = sample(no_program(rpc), &idl(), "Prog", &Limits::default())
            .unwrap()
            .samples;
        assert_eq!(samples.instructions.len(), 1);
    }

    /// Instructions `a` and `b` with one-byte discriminators 1 and 2.
    fn idl_ab() -> Idl {
        crate::testing::idl(
            r#""instructions":[{"name":"a","discriminator":[1]},{"name":"b","discriminator":[2]}]"#,
        )
    }

    /// A program history, newest first, where transaction `S<i>` calls the program with the
    /// data `[history[i]]`. Pages honor `limit` and `before` as the RPC documents.
    fn history(history: Vec<u8>) -> impl FnMut(&str, Value) -> Result<Value, Error> {
        let index =
            |signature: &Value| -> usize { signature.as_str().unwrap()[1..].parse().unwrap() };
        move |method, params| {
            Ok(match method {
                "getSignaturesForAddress" => {
                    let limit = usize::try_from(params[1]["limit"].as_u64().unwrap()).unwrap();
                    let start = params[1].get("before").map_or(0, |b| index(b) + 1);
                    let page: Vec<Value> = (start..history.len())
                        .take(limit)
                        .map(|i| json!({ "signature": format!("S{i}"), "err": null }))
                        .collect();
                    json!(page)
                }
                "getTransaction" => {
                    let data = bs58::encode([history[index(&params[0])]]).into_string();
                    tx(
                        &["Payer", "Prog"],
                        &json!([{ "programIdIndex": 1, "accounts": [0], "data": data }]),
                    )
                }
                "getAccountInfo" => json!({ "value": null }),
                _ => json!({ "value": params[0].as_array().unwrap().iter()
                    .map(|_| Value::Null).collect::<Vec<_>>() }),
            })
        }
    }

    fn data_of(sampled: &Sampled) -> Vec<u8> {
        let ixs = &sampled.samples.instructions;
        ixs.iter().flat_map(|ix| ix.data.clone()).collect()
    }

    #[test]
    fn paging_finds_a_rare_instruction_on_page_two() {
        let mut calls = vec![1; SIGNATURES_PER_PAGE];
        calls.push(2);

        let first = sample(
            history(calls.clone()),
            &idl_ab(),
            "Prog",
            &Limits::default(),
        )
        .unwrap();
        assert!(!data_of(&first).contains(&2));
        assert_eq!(first.stop_reason, StopReason::MaxTransactions);

        let limits = Limits {
            max_transactions: 2000,
            stall: 2000,
            ..Limits::default()
        };
        let paged = sample(history(calls), &idl_ab(), "Prog", &limits).unwrap();
        assert_eq!(data_of(&paged), [1, 1, 1, 1, 1, 2]);
        assert_eq!(paged.transactions, SIGNATURES_PER_PAGE + 1);
        assert_eq!(paged.stop_reason, StopReason::History);
    }

    #[test]
    fn stops_when_transactions_bring_no_new_kind() {
        let limits = Limits {
            stall: 3,
            ..Limits::default()
        };
        let sampled = sample(history(vec![1; 30]), &idl_ab(), "Prog", &limits).unwrap();
        assert_eq!(sampled.transactions, 4);
        assert_eq!(sampled.stop_reason, StopReason::Stall);
    }

    #[test]
    fn the_quota_caps_a_common_instruction_and_stops_when_all_are_full() {
        let limits = Limits {
            per_instruction: 2,
            ..Limits::default()
        };
        let sampled = sample(
            history(vec![1, 1, 1, 2, 2, 1, 2]),
            &idl_ab(),
            "Prog",
            &limits,
        )
        .unwrap();
        assert_eq!(data_of(&sampled), [1, 1, 2, 2]);
        assert_eq!(sampled.transactions, 5);
        assert_eq!(sampled.stop_reason, StopReason::Quota);
    }
}
