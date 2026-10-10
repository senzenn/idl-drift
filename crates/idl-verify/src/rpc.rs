//! Samples taken from a live cluster over JSON-RPC.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use idl_drift::model::Idl;

use crate::error::Error;
use crate::samples::{AccountSample, InstructionSample, Samples};
use crate::verify::{disc_table, longest_match, prefix_hex};

/// Anchor's `EVENT_IX_TAG` in little-endian. `emit_cpi!` calls the program itself with this
/// prefix, which is an event, not an instruction in the IDL.
const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

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
    instructions: Vec<CompiledInstruction>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompiledInstruction {
    program_id_index: usize,
    accounts: Vec<usize>,
    data: String,
}

/// The instructions in one `getTransaction` result (`json` encoding) that call
/// `program_id`, with their account keys resolved.
///
/// # Errors
///
/// When the transaction JSON does not have the shape the RPC documents.
pub fn instructions_from_transaction(
    program_id: &str,
    tx: &Value,
) -> Result<Vec<InstructionSample>, Error> {
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
            std::iter::once(top).chain(cpis.flat_map(|g| &g.instructions))
        });

    let mut out = Vec::new();
    for ix in executed {
        if key(ix.program_id_index)? != program_id {
            continue;
        }
        let data = bs58::decode(&ix.data)
            .into_vec()
            .map_err(|e| Error::RpcResponse(format!("{signature}: instruction data: {e}")))?;
        if data.starts_with(&EVENT_IX_TAG) {
            continue;
        }
        out.push(InstructionSample {
            signature: signature.clone(),
            data,
            accounts: ix
                .accounts
                .iter()
                .map(|&i| key(i))
                .collect::<Result<_, _>>()?,
        });
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
#[derive(Debug)]
pub struct Limits {
    /// Recent signatures to read; failed transactions among them are skipped.
    pub signatures: usize,
    pub instructions: usize,
    /// Accounts per IDL account type, and per unknown discriminator.
    pub per_type: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            signatures: 40,
            instructions: 20,
            per_type: 5,
        }
    }
}

/// Samples of `program_id` from recent successful transactions and the accounts they pass.
/// `rpc` sends one JSON-RPC request and returns its `result`.
///
/// # Errors
///
/// When a request fails, so that a run never passes on samples it could not fetch.
pub fn sample(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, Error>,
    idl: &Idl,
    program_id: &str,
    limits: &Limits,
) -> Result<Samples, Error> {
    let signatures = rpc(
        "getSignaturesForAddress",
        json!([program_id, { "limit": limits.signatures }]),
    )?;
    let signatures = Vec::<SignatureInfo>::deserialize(&signatures)
        .map_err(|e| Error::RpcResponse(format!("getSignaturesForAddress: {e}")))?;

    // Failed transactions can carry data the program rejected, which is not a fault of the IDL.
    let mut instructions = Vec::new();
    for info in signatures.iter().filter(|s| s.err.is_none()) {
        // The highest version this parser reads; the RPC refuses newer ones with an error.
        let tx = rpc(
            "getTransaction",
            json!([info.signature, { "encoding": "json", "maxSupportedTransactionVersion": 1 }]),
        )?;
        instructions.extend(instructions_from_transaction(program_id, &tx)?);
    }

    let mut seen = HashSet::new();
    let keys: Vec<String> = instructions
        .iter()
        .flat_map(|ix| &ix.accounts)
        .filter(|k| seen.insert(k.as_str()))
        .cloned()
        .collect();
    instructions.truncate(limits.instructions);

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

    Ok(Samples {
        program_id: Some(program_id.to_string()),
        accounts,
        instructions,
    })
}

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
        let samples = sample(rpc, &idl(), "Prog", &Limits::default()).unwrap();

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
    fn the_instruction_limit_keeps_the_accounts_of_dropped_instructions() {
        let rpc = |method: &str, params: Value| {
            Ok(match method {
                "getSignaturesForAddress" => json!([{ "signature": "S1", "err": null }]),
                "getTransaction" => tx(
                    &["A", "B", "Prog"],
                    &json!([
                        { "programIdIndex": 2, "accounts": [0], "data": "2" },
                        { "programIdIndex": 2, "accounts": [1], "data": "3" }
                    ]),
                ),
                _ => json!({ "value": params[0].as_array().unwrap().iter()
                    .map(|_| account("Prog", &[9, 9])).collect::<Vec<_>>() }),
            })
        };
        let limits = Limits {
            instructions: 1,
            ..Limits::default()
        };
        let samples = sample(rpc, &idl(), "Prog", &limits).unwrap();
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
        let samples = sample(rpc, &idl(), "Prog", &Limits::default()).unwrap();
        assert_eq!(samples.instructions.len(), 1);
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
        let ixs = instructions_from_transaction("Prog", &tx).unwrap();
        assert_eq!(ixs.len(), 1);
        assert_eq!(ixs[0].signature, "Sig1");
        assert_eq!(ixs[0].accounts, ["Vault", "Payer"]);
        assert_eq!(ixs[0].data, [1, 2, 3]);
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
        let ixs = instructions_from_transaction("Prog", &tx).unwrap();
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
        let ixs = instructions_from_transaction("Prog", &tx).unwrap();
        assert_eq!(ixs[0].accounts, ["R1", "W1", "R2", "Payer"]);
    }

    #[test]
    fn skips_self_cpi_events() {
        let event = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d, 7, 7];
        let tx = tx(
            &["Payer", "Prog"],
            &json!([
                { "programIdIndex": 1, "accounts": [0], "data": bs58::encode(event).into_string() },
                { "programIdIndex": 1, "accounts": [0], "data": "2" }
            ]),
        );
        let ixs = instructions_from_transaction("Prog", &tx).unwrap();
        assert_eq!(ixs.len(), 1);
        assert_eq!(ixs[0].data, [1]);
    }

    #[test]
    fn rejects_keys_with_terminal_control_characters() {
        let tx = tx(
            &["Payer\u{1b}[2J", "Prog"],
            &json!([{ "programIdIndex": 1, "accounts": [0], "data": "2" }]),
        );
        let err = instructions_from_transaction("Prog", &tx)
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

            let result = instructions_from_transaction(&program_id, &tx);

            let executed: Vec<&Ix> = top.iter().chain(inner.iter().flat_map(|(_, ixs)| ixs)).collect();
            let valid = executed.iter().all(|ix| ix.0 < n && ix.1.iter().all(|&a| a < n) && !ix.3);
            if valid {
                proptest::prop_assert!(result.is_ok(), "{result:?}");
            }
            if let Ok(samples) = result {
                let to_program: Vec<&&Ix> = executed
                    .iter()
                    .filter(|ix| names.get(ix.0) == Some(&program_id) && !ix.2.starts_with(&EVENT_IX_TAG))
                    .collect();
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
