//! Samples taken from a live cluster over JSON-RPC.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use idl_drift::model::Idl;

use crate::error::Error;
use crate::samples::{
    AccountSample, EventSample, EventSource, InstructionSample, KeyFlags, Samples,
};
use crate::verify::{cpi_envelopes, disc_table, event_body, longest_match, prefix_hex, Disc};

#[derive(Deserialize)]
struct Transaction {
    transaction: Body,
    #[serde(default)]
    meta: Option<Meta>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(default)]
    inner_instructions: Option<Vec<InnerInstructions>>,
    #[serde(default)]
    loaded_addresses: Option<LoadedAddresses>,
    #[serde(default)]
    log_messages: Option<Vec<String>>,
}

/// Keys a v0 transaction loads from address lookup tables. Instruction indices continue
/// past the static keys into `writable`, then `readonly`.
#[derive(Deserialize)]
struct LoadedAddresses {
    writable: Vec<String>,
    readonly: Vec<String>,
}

/// The CPIs made by the top-level instruction at `index`.
#[derive(Deserialize)]
struct InnerInstructions {
    index: usize,
    instructions: Vec<CompiledInstruction>,
}

#[derive(Deserialize)]
struct Body {
    signatures: Vec<String>,
    message: Message,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    account_keys: Vec<String>,
    #[serde(default)]
    header: Option<Header>,
    instructions: Vec<CompiledInstruction>,
}

/// The first `signers` keys sign. The last `readonly_signed` signers and the last
/// `readonly_unsigned` static keys are read-only.
#[derive(Deserialize, Clone, Copy)]
struct Header {
    #[serde(rename = "numRequiredSignatures")]
    signers: usize,
    #[serde(rename = "numReadonlySignedAccounts")]
    readonly_signed: usize,
    #[serde(rename = "numReadonlyUnsignedAccounts")]
    readonly_unsigned: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompiledInstruction {
    program_id_index: usize,
    accounts: Vec<usize>,
    data: String,
}

/// What one transaction holds for a program.
#[derive(Debug, Default)]
pub struct TransactionSamples {
    pub instructions: Vec<InstructionSample>,
    pub events: Vec<EventSample>,
}

/// `(signer, writable)` per key, static keys then loaded ones, by the header rule that
/// `solana-message` 4.6 applies to legacy, v0 and v1 messages. A later version gets none.
fn key_privileges(
    version: Option<&Value>,
    message: &Message,
    loaded: Option<&LoadedAddresses>,
) -> Option<Vec<(bool, bool)>> {
    // `version` is absent when the request sets no maximum version.
    let known = match version {
        None => true,
        Some(Value::String(v)) => v == "legacy",
        Some(v) => v.as_u64().is_some_and(|v| v <= 1),
    };
    let header = message.header.filter(|_| known)?;
    let statics = message.account_keys.len();
    let signers = header.signers;
    let writable_signers = signers.saturating_sub(header.readonly_signed);
    let writable_unsigned = statics
        .saturating_sub(signers)
        .saturating_sub(header.readonly_unsigned);
    let static_keys = (0..statics).map(|i| {
        if i < signers {
            (true, i < writable_signers)
        } else {
            (false, i - signers < writable_unsigned)
        }
    });
    let loaded_keys = loaded.into_iter().flat_map(|l| {
        let writable = l.writable.iter().map(|_| (false, true));
        writable.chain(l.readonly.iter().map(|_| (false, false)))
    });
    Some(static_keys.chain(loaded_keys).collect())
}

fn key_flags(privileges: &[(bool, bool)], accounts: &[usize], cpi: bool) -> KeyFlags {
    let of = |i: &usize| privileges.get(*i).copied().unwrap_or_default();
    KeyFlags {
        signer: accounts.iter().map(|i| of(i).0).collect(),
        writable: accounts.iter().map(|i| of(i).1).collect(),
        cpi,
    }
}

/// The instructions in one `getTransaction` result (`json` encoding) that call
/// `program_id`, with their account keys resolved, and the events the program emitted.
/// `idl` gives the envelopes that mark a self-CPI event.
///
/// # Errors
///
/// When the transaction JSON does not have the shape the RPC documents.
pub fn samples_from_transaction(
    idl: &Idl,
    program_id: &str,
    tx: &Value,
) -> Result<TransactionSamples, Error> {
    let version = tx.get("version");
    let tx = Transaction::deserialize(tx).map_err(|e| Error::RpcResponse(e.to_string()))?;
    let signature = tx
        .transaction
        .signatures
        .first()
        .cloned()
        .ok_or_else(|| Error::RpcResponse("transaction has no signature".into()))?;
    let loaded = tx.meta.as_ref().and_then(|m| m.loaded_addresses.as_ref());
    let keys: Vec<&String> = tx
        .transaction
        .message
        .account_keys
        .iter()
        .chain(
            loaded
                .into_iter()
                .flat_map(|l| l.writable.iter().chain(&l.readonly)),
        )
        .collect();
    // The report prints keys and signatures, so a hostile RPC must not reach the terminal.
    if std::iter::once(&signature)
        .chain(keys.iter().copied())
        .any(|s| s.chars().any(char::is_control))
    {
        return Err(Error::RpcResponse(
            "a key or signature holds control characters".into(),
        ));
    }
    let key = |index: usize| {
        keys.get(index).map(|&k| k.clone()).ok_or_else(|| {
            Error::RpcResponse(format!(
                "{signature}: account index {index} is past the {} keys",
                keys.len()
            ))
        })
    };

    let inner = tx
        .meta
        .as_ref()
        .and_then(|m| m.inner_instructions.as_deref())
        .unwrap_or_default();
    let executed = tx
        .transaction
        .message
        .instructions
        .iter()
        .enumerate()
        .flat_map(|(i, top)| {
            let cpis = inner.iter().filter(move |g| g.index == i);
            std::iter::once((false, top))
                .chain(cpis.flat_map(|g| g.instructions.iter().map(|ix| (true, ix))))
        });
    let privileges = key_privileges(version, &tx.transaction.message, loaded);

    let cpi = cpi_envelopes(idl);
    let mut out = TransactionSamples::default();
    for (nested, ix) in executed {
        if key(ix.program_id_index)? != program_id {
            continue;
        }
        let data = bs58::decode(&ix.data)
            .into_vec()
            .map_err(|e| Error::RpcResponse(format!("{signature}: instruction data: {e}")))?;
        if cpi.iter().any(|e| e.wraps(&data)) {
            out.events.push(EventSample {
                signature: signature.clone(),
                source: EventSource::Cpi,
                data,
            });
            continue;
        }
        out.instructions.push(InstructionSample {
            signature: signature.clone(),
            data,
            accounts: ix
                .accounts
                .iter()
                .map(|&i| key(i))
                .collect::<Result<_, _>>()?,
            flags: privileges
                .as_deref()
                .map(|p| key_flags(p, &ix.accounts, nested)),
        });
    }

    let logs = tx
        .meta
        .as_ref()
        .and_then(|m| m.log_messages.as_deref())
        .unwrap_or_default();
    let logged = log_events(program_id, logs)
        .map_err(|e| Error::RpcResponse(format!("{signature}: Program data log: {e}")))?;
    out.events
        .extend(logged.into_iter().map(|data| EventSample {
            signature: signature.clone(),
            source: EventSource::Log,
            data,
        }));
    Ok(out)
}

/// The `Program data:` payloads logged while `program_id` is the innermost running program.
/// A nested program's lines are its own, as in Shipstern's `resolve_events_from_logs`.
fn log_events(program_id: &str, logs: &[String]) -> Result<Vec<Vec<u8>>, base64::DecodeError> {
    let mut frames: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    // The runtime drops every line after this marker, so frames can stay open.
    for line in logs.iter().take_while(|l| *l != "Log truncated") {
        if let Some(encoded) = line.strip_prefix("Program data: ") {
            if frames.last() == Some(&program_id) {
                let mut data = Vec::new();
                // `sol_log_data` writes one base64 chunk per slice, separated by spaces.
                for chunk in encoded.split_whitespace() {
                    data.extend(STANDARD.decode(chunk)?);
                }
                out.push(data);
            }
            continue;
        }
        let Some((id, rest)) = line
            .strip_prefix("Program ")
            .and_then(|l| l.split_once(' '))
        else {
            continue;
        };
        // Program ids are base58, so labels such as `log:` and `return:` never match.
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        if rest.starts_with("invoke [") {
            frames.push(id);
        } else if rest == "success" || rest.starts_with("failed: ") {
            frames.pop();
        }
    }
    Ok(out)
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
    let table = disc_table(
        idl.accounts
            .iter()
            .map(|a| (a.discriminator_offset, a.discriminator.as_slice())),
    );
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
        let group = match longest_match(&table, &data).and_then(|m| m.first().copied()) {
            Some(index) => (Some(index), String::new()),
            None => (None, prefix_hex(&data)),
        };
        let count = taken.entry(group).or_default();
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
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Quota => "every IDL instruction has its quota of samples",
            Self::Stall => "the last --stall transactions brought no new kind",
            Self::MaxTransactions => "--max-tx transactions were read",
            Self::History => "the program has no older transactions",
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    Instruction,
    LogEvent,
    CpiEvent,
}

/// Caps the samples of each kind: an IDL discriminator, or the first bytes of data that
/// match none.
struct Quota<'a> {
    instructions: Vec<(Disc<'a>, usize)>,
    events: Vec<(Disc<'a>, usize)>,
    per_kind: usize,
    taken: HashMap<(Group, Option<usize>, String), usize>,
    /// IDL instructions whose discriminator has its quota.
    full: HashSet<usize>,
}

impl<'a> Quota<'a> {
    fn new(idl: &'a Idl, per_kind: usize) -> Self {
        Self {
            instructions: disc_table(
                idl.instructions
                    .iter()
                    .map(|ix| (ix.discriminator_offset, ix.discriminator.as_slice())),
            ),
            events: disc_table(idl.events.iter().map(|e| (0, e.discriminator.as_slice()))),
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
        let kind = match matched.as_ref().and_then(|m| m.first()) {
            Some(&index) => (Some(index), String::new()),
            None => (None, prefix_hex(data)),
        };
        let mut new = false;
        let count = self
            .taken
            .entry((group, kind.0, kind.1))
            .or_insert_with(|| {
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
            // The highest version this parser reads; the RPC refuses newer ones with an error.
            let tx = rpc(
                "getTransaction",
                json!([info.signature, { "encoding": "json", "maxSupportedTransactionVersion": 1 }]),
            )?;
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

    let mut values = Vec::new();
    for chunk in keys.chunks(MAX_ACCOUNTS_PER_REQUEST) {
        let page = rpc(
            "getMultipleAccounts",
            json!([chunk, { "encoding": "base64" }]),
        )?;
        let page = AccountsPage::deserialize(&page)
            .map_err(|e| Error::RpcResponse(format!("getMultipleAccounts: {e}")))?;
        values.extend(page.value);
    }
    let accounts = accounts_from_rpc(idl, program_id, &keys, &values, limits.per_type)?;

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

/// The `getSignaturesForAddress` maximum per request.
const SIGNATURES_PER_PAGE: usize = 1000;

/// One entry of `getSignaturesForAddress`; `err` is set when the transaction failed.
#[derive(Deserialize)]
struct SignatureInfo {
    signature: String,
    #[serde(default)]
    err: Option<Value>,
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
    use crate::verify::EVENT_IX_TAG;
    use serde_json::json;

    fn idl() -> Idl {
        Idl::from_json(
            r#"{"address":"Prog","metadata":{"name":"t","version":"0","spec":"0.1.0"},
                "accounts":[{"name":"Vault","discriminator":[9,9]}]}"#,
        )
        .unwrap()
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
        let err = sample(rpc, &idl(), "Prog", &Limits::default()).unwrap_err();
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
    }

    #[test]
    fn self_cpi_events_use_the_envelope_the_idl_declares() {
        let idl = Idl::from_json(
            r#"{"address":"Prog","metadata":{"name":"t","version":"0","spec":"0.1.0"},"events":[
                {"name":"A","discriminator":[7],"envelope_discriminator":[254]},
                {"name":"B","discriminator":[8],"envelope_discriminator":[254]}]}"#,
        )
        .unwrap();
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
        let samples = sample(rpc, &idl, "Prog", &limits).unwrap().samples;
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
        let samples = sample(rpc, &idl(), "Prog", &limits).unwrap().samples;
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
        let samples = sample(rpc, &idl(), "Prog", &Limits::default())
            .unwrap()
            .samples;
        assert_eq!(samples.instructions.len(), 1);
    }

    /// Instructions `a` and `b` with one-byte discriminators 1 and 2.
    fn idl_ab() -> Idl {
        Idl::from_json(
            r#"{"address":"Prog","metadata":{"name":"t","version":"0","spec":"0.1.0"},
                "instructions":[{"name":"a","discriminator":[1]},{"name":"b","discriminator":[2]}]}"#,
        )
        .unwrap()
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

    fn tx(keys: &[&str], instructions: &Value) -> Value {
        json!({
            "transaction": {
                "signatures": ["Sig1"],
                "message": { "accountKeys": keys, "instructions": instructions }
            },
            "meta": {
                "err": null,
                "innerInstructions": [],
                "loadedAddresses": { "writable": [], "readonly": [] }
            }
        })
    }

    #[test]
    fn keeps_a_top_level_instruction_to_the_program() {
        let tx = tx(
            &["Payer", "Vault", "Prog"],
            &json!([{ "programIdIndex": 2, "accounts": [1, 0], "data": "Ldp" }]),
        );
        let ixs = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .instructions;
        assert_eq!(ixs.len(), 1);
        assert_eq!(ixs[0].signature, "Sig1");
        assert_eq!(ixs[0].accounts, ["Vault", "Payer"]);
        assert_eq!(ixs[0].data, [1, 2, 3]);
    }

    #[test]
    fn flags_follow_the_message_header() {
        // Payer signs and is writable, Auth signs read-only, Vault is writable, Prog is
        // read-only; then lookup-table keys L1 (writable) and L2 (read-only).
        let mut tx = tx(
            &["Payer", "Auth", "Vault", "Prog"],
            &json!([{ "programIdIndex": 3, "accounts": [1, 2, 0, 4, 5], "data": "2" }]),
        );
        tx["transaction"]["message"]["header"] = json!({ "numRequiredSignatures": 2,
            "numReadonlySignedAccounts": 1, "numReadonlyUnsignedAccounts": 1 });
        tx["meta"]["loadedAddresses"] = json!({ "writable": ["L1"], "readonly": ["L2"] });
        tx["meta"]["innerInstructions"] = json!([{ "index": 0, "instructions": [
            { "programIdIndex": 3, "accounts": [2], "data": "2" }
        ]}]);
        tx["version"] = json!(0);
        let ixs = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .instructions;
        let expected = KeyFlags {
            signer: vec![true, false, true, false, false],
            writable: vec![false, true, true, true, false],
            cpi: false,
        };
        assert_eq!(ixs[0].flags, Some(expected));
        assert_eq!(ixs[1].flags.as_ref().map(|f| f.cpi), Some(true));
        tx["version"] = json!(1);
        let ixs = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .instructions;
        assert!(ixs[0].flags.is_some());
        // A later message version can order keys another way.
        tx["version"] = json!(2);
        let ixs = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .instructions;
        assert!(ixs[0].flags.is_none());
    }

    #[test]
    fn keeps_inner_instructions_in_execution_order() {
        let mut tx = tx(
            &["Payer", "Other", "Prog"],
            &json!([
                { "programIdIndex": 1, "accounts": [0], "data": "2" },
                { "programIdIndex": 2, "accounts": [0], "data": "3" }
            ]),
        );
        tx["meta"]["innerInstructions"] = json!([{ "index": 0, "instructions": [
            { "programIdIndex": 2, "accounts": [1], "data": "4", "stackHeight": 2 },
            { "programIdIndex": 1, "accounts": [], "data": "5", "stackHeight": 2 }
        ]}]);
        let ixs = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .instructions;
        let got: Vec<_> = ixs
            .iter()
            .map(|ix| (ix.data.clone(), ix.accounts.clone()))
            .collect();
        assert_eq!(
            got,
            [
                (vec![3], vec!["Other".to_string()]),
                (vec![2], vec!["Payer".to_string()])
            ]
        );
    }

    #[test]
    fn resolves_lookup_table_keys_after_the_static_keys() {
        let mut tx = tx(
            &["Payer", "Prog"],
            &json!([{ "programIdIndex": 1, "accounts": [3, 2, 4, 0], "data": "2" }]),
        );
        tx["meta"]["loadedAddresses"] = json!({ "writable": ["W1"], "readonly": ["R1", "R2"] });
        let ixs = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .instructions;
        assert_eq!(ixs[0].accounts, ["R1", "W1", "R2", "Payer"]);
    }

    #[test]
    fn events_keep_execution_order_across_cpis() {
        let event = |b: u8| bs58::encode([&EVENT_IX_TAG[..], &[b]].concat()).into_string();
        let mut tx = tx(
            &["Payer", "Prog"],
            &json!([
                { "programIdIndex": 1, "accounts": [0], "data": "2" },
                { "programIdIndex": 1, "accounts": [0], "data": event(2) }
            ]),
        );
        tx["meta"]["innerInstructions"] = json!([{ "index": 0, "instructions": [
            { "programIdIndex": 1, "accounts": [0], "data": event(1) }
        ]}]);
        let events = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .events;
        let last: Vec<u8> = events
            .iter()
            .filter_map(|e| e.data.last().copied())
            .collect();
        assert_eq!(last, [1, 2]);
    }

    #[test]
    fn keeps_self_cpi_events_as_events_with_their_tag() {
        let event = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d, 7, 7];
        let tx = tx(
            &["Payer", "Prog"],
            &json!([
                { "programIdIndex": 1, "accounts": [0], "data": bs58::encode(event).into_string() },
                { "programIdIndex": 1, "accounts": [0], "data": "2" }
            ]),
        );
        let found = samples_from_transaction(&idl(), "Prog", &tx).unwrap();
        assert_eq!(found.instructions.len(), 1);
        assert_eq!(found.instructions[0].data, [1]);
        assert_eq!(found.events.len(), 1);
        assert_eq!(found.events[0].source, EventSource::Cpi);
        assert_eq!(found.events[0].data, event);
    }

    fn with_logs(logs: &[&str]) -> Value {
        let mut tx = tx(&["Payer", "Prog"], &json!([]));
        tx["meta"]["logMessages"] = json!(logs);
        tx
    }

    #[test]
    fn keeps_data_lines_only_in_the_programs_own_frame() {
        let tx = with_logs(&[
            "Program Prog invoke [1]",
            "Program log: Instruction: Deposit",
            "Program data: AQ==",
            "Program Other invoke [2]",
            "Program data: Ag==",
            "Program Other success",
            "Program data: Aw==",
            "Program Prog consumed 5000 of 200000 compute units",
            "Program Prog success",
            "Program data: BA==",
        ]);
        let events = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .events;
        let got: Vec<(EventSource, &[u8])> = events
            .iter()
            .map(|e| (e.source, e.data.as_slice()))
            .collect();
        assert_eq!(
            got,
            [(EventSource::Log, &[1][..]), (EventSource::Log, &[3][..])]
        );
    }

    #[test]
    fn keeps_data_lines_before_a_log_truncation() {
        let tx = with_logs(&[
            "Program Prog invoke [1]",
            "Program data: AQ==",
            "Log truncated",
            "Program data: Ag==",
        ]);
        let events = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap()
            .events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, [1]);
    }

    #[test]
    fn bad_base64_in_a_data_line_is_a_response_error() {
        let tx = with_logs(&["Program Prog invoke [1]", "Program data: %%%%"]);
        let err = samples_from_transaction(&idl(), "Prog", &tx).unwrap_err();
        assert!(matches!(err, Error::RpcResponse(_)), "{err}");
    }

    #[test]
    fn rejects_keys_with_terminal_control_characters() {
        let tx = tx(
            &["Payer\u{1b}[2J", "Prog"],
            &json!([{ "programIdIndex": 1, "accounts": [0], "data": "2" }]),
        );
        let err = samples_from_transaction(&idl(), "Prog", &tx)
            .unwrap_err()
            .to_string();
        assert!(!err.contains('\u{1b}'), "{err:?}");
    }

    /// A compiled instruction: program index, account indices, data, and whether the data is
    /// corrupted into invalid base58.
    type Ix = (usize, Vec<usize>, Vec<u8>, bool);

    fn ix_strategy(max_index: usize) -> impl proptest::strategy::Strategy<Value = Ix> {
        use proptest::prelude::*;
        let data = prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..12),
            proptest::collection::vec(any::<u8>(), 0..4).prop_map(|tail| [
                &EVENT_IX_TAG[..],
                &tail
            ]
            .concat()),
        ];
        (
            0..max_index,
            proptest::collection::vec(0..max_index, 0..5),
            data,
            proptest::bool::weighted(0.1),
        )
    }

    fn compiled(ix: &Ix) -> Value {
        let data = if ix.3 {
            "0OIl".to_string()
        } else {
            bs58::encode(&ix.2).into_string()
        };
        json!({ "programIdIndex": ix.0, "accounts": ix.1, "data": data })
    }

    proptest::proptest! {
        /// Keys are named `k<i>` after their index in the combined list: static keys, then
        /// lookup-table writable keys, then readonly keys, as the RPC documents.
        #[test]
        fn samples_match_the_instructions_they_came_from(
            (s, w, r) in (1usize..5, 0usize..3, 0usize..3),
            program in 0usize..4,
            top in proptest::collection::vec(ix_strategy(10), 0..5),
            inner in proptest::collection::vec((0usize..5, proptest::collection::vec(ix_strategy(10), 0..3)), 0..3),
        ) {
            let n = s + w + r;
            let names: Vec<String> = (0..n).map(|i| format!("k{i}")).collect();
            let program_id = format!("k{}", program % n);
            let inner: Vec<_> = inner.into_iter().filter(|(i, _)| *i < top.len()).collect();
            let mut tx = tx(
                &names[..s].iter().map(String::as_str).collect::<Vec<_>>(),
                &Value::Array(top.iter().map(compiled).collect()),
            );
            tx["meta"]["loadedAddresses"] = json!({ "writable": names[s..s + w], "readonly": names[s + w..] });
            tx["meta"]["innerInstructions"] = Value::Array(inner.iter().map(|(index, ixs)| json!({
                "index": index, "instructions": ixs.iter().map(compiled).collect::<Vec<_>>()
            })).collect());

            let result = samples_from_transaction(&idl(), &program_id, &tx);

            // Execution order: each top-level instruction, then the CPIs it made.
            let executed: Vec<&Ix> = top
                .iter()
                .enumerate()
                .flat_map(|(i, t)| {
                    std::iter::once(t).chain(
                        inner.iter().filter(move |(index, _)| *index == i).flat_map(|(_, ixs)| ixs),
                    )
                })
                .collect();
            let valid = executed.iter().all(|ix| ix.0 < n && ix.1.iter().all(|&a| a < n) && !ix.3);
            if valid {
                proptest::prop_assert!(result.is_ok(), "{result:?}");
            }
            if let Ok(found) = result {
                let (events, to_program): (Vec<&&Ix>, Vec<&&Ix>) = executed
                    .iter()
                    .filter(|ix| names.get(ix.0) == Some(&program_id))
                    .partition(|ix| ix.2.starts_with(&EVENT_IX_TAG));
                let event_data: Vec<&[u8]> = events.iter().map(|ix| ix.2.as_slice()).collect();
                let found_data: Vec<&[u8]> = found.events.iter().map(|e| e.data.as_slice()).collect();
                proptest::prop_assert_eq!(found_data, event_data);
                let samples = found.instructions;
                proptest::prop_assert_eq!(samples.len(), to_program.len());
                for sample in &samples {
                    proptest::prop_assert!(
                        to_program.iter().any(|ix| ix.2 == sample.data
                            && ix.1.iter().map(|&a| format!("k{a}")).collect::<Vec<_>>() == sample.accounts),
                        "no source instruction for {sample:?}"
                    );
                }
            }
        }
    }
}
