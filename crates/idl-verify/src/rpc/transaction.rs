//! Parse one `getTransaction` result into the program's instruction and event samples.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;

use idl_drift::model::Idl;

use crate::classify::cpi_envelopes;
use crate::error::Error;
use crate::samples::{EventSample, EventSource, InstructionSample, KeyFlags};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::EVENT_IX_TAG;
    use crate::testing::tx;
    use serde_json::json;

    /// No events, so Anchor's tag marks a self-CPI event.
    fn idl() -> Idl {
        crate::testing::idl(r#""instructions":[]"#)
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
