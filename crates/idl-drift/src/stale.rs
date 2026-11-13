//! Whether the IDL that a program publishes on chain is older than its code: the slot of the
//! program's last deploy against the slot of the last successful transaction on each IDL
//! account.

use std::fmt;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::onchain::{self, Source};

/// The upgradeable BPF loader (loader v3).
pub const LOADER_V3: &str = "BPFLoaderUpgradeab1e11111111111111111111111";
/// Loader v4.
pub const LOADER_V4: &str = "LoaderV411111111111111111111111111111111111";

/// What the program account tells about the program's last deploy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deploy {
    /// The slot that the loader keeps: the last deploy, or for the upgradeable loader also a
    /// later `ExtendProgram`.
    Slot(u64),
    /// No account is at the address.
    NoAccount,
    /// The account keeps no deploy slot, by its owner: it is not a program of the upgradeable
    /// loader or of loader v4, or it is an upgradeable program with no `ProgramData` slot.
    NoSlot { owner: String },
}

impl Deploy {
    /// The slot that the loader keeps, when the account keeps one.
    #[must_use]
    pub fn slot(&self) -> Option<u64> {
        match self {
            Self::Slot(slot) => Some(*slot),
            Self::NoAccount | Self::NoSlot { .. } => None,
        }
    }
}

/// The last deploy of `program`. The upgradeable loader keeps the slot in the `ProgramData`
/// account, and loader v4 at the start of the program account. `rpc` sends one JSON-RPC
/// request and returns its `result`; `bad_response` makes the error of an answer that does
/// not have the documented shape.
///
/// # Errors
///
/// When a request fails, or an answer does not have the shape that the RPC documents.
pub fn deploy_slot<E>(
    rpc: impl FnMut(&str, Value) -> Result<Value, E>,
    program: &str,
    bad_response: impl Fn(String) -> E,
) -> Result<Deploy, E> {
    read_deploy(rpc, program, bad_response).map(|(deploy, _)| deploy)
}

/// As [`deploy_slot`], with the `ProgramData` address of an upgradeable program.
fn read_deploy<E>(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, E>,
    program: &str,
    bad_response: impl Fn(String) -> E,
) -> Result<(Deploy, Option<String>), E> {
    let mut first_bytes = |address: &str, length: usize| -> Result<Option<(String, Vec<u8>)>, E> {
        let config =
            json!({ "encoding": "base64", "dataSlice": { "offset": 0, "length": length } });
        let reply = rpc("getAccountInfo", json!([address, config]))?;
        let Some(account) = Option::<RpcAccount>::deserialize(&reply["value"])
            .map_err(|e| bad_response(format!("getAccountInfo {address}: {e}")))?
        else {
            return Ok(None);
        };
        let data = STANDARD
            .decode(&account.data.0)
            .map_err(|e| bad_response(format!("getAccountInfo {address}: data: {e}")))?;
        Ok(Some((account.owner, data)))
    };
    let slot_at = |data: &[u8], at: usize| {
        data.get(at..)
            .and_then(<[u8]>::first_chunk::<8>)
            .map(|b| u64::from_le_bytes(*b))
    };
    let Some((owner, data)) = first_bytes(program, 36)? else {
        return Ok((Deploy::NoAccount, None));
    };
    let mut programdata = None;
    let slot = match owner.as_str() {
        // `LoaderV4State` starts with the slot.
        LOADER_V4 => slot_at(&data, 0),
        // `UpgradeableLoaderState::Program` (tag 2) names its ProgramData account, which
        // (tag 3) holds the slot after the tag.
        LOADER_V3 if data.get(..4) == Some(&[2, 0, 0, 0]) => {
            match data.get(4..36).map(|k| bs58::encode(k).into_string()) {
                Some(address) => {
                    let slot = first_bytes(&address, 12)?
                        .filter(|(_, d)| d.get(..4) == Some(&[3, 0, 0, 0]))
                        .and_then(|(_, d)| slot_at(&d, 4));
                    programdata = Some(address);
                    slot
                }
                None => None,
            }
        }
        _ => None,
    };
    let deploy = slot.map_or(Deploy::NoSlot { owner }, Deploy::Slot);
    Ok((deploy, programdata))
}

/// An account from `getAccountInfo`; `data` is `[<base64>, "base64"]`.
#[derive(Deserialize)]
struct RpcAccount {
    owner: String,
    data: (String, String),
}

/// Whether the on-chain IDL is older than the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// No deploy came after the IDL write.
    Fresh,
    /// A deploy came after the IDL write.
    Stale,
    /// The program has no IDL account. Only a program has this status.
    NoIdl,
}

impl Status {
    /// The name of the status in JSON output.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::NoIdl => "no_idl",
        }
    }

    /// The status as the reports print it.
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::NoIdl => "no on-chain IDL",
        }
    }
}

/// The last write to one IDL account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct IdlWrite {
    pub source: Source,
    /// The address of the IDL account.
    pub account: String,
    /// The slot of the newest successful transaction that names the account.
    pub slot: u64,
    /// [`Status::Fresh`] or [`Status::Stale`].
    pub status: Status,
}

/// What wrote the slot that the upgradeable loader keeps. [`check`] reads it only when an IDL
/// write is older than that slot, because `ExtendProgram` also sets the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExtendCheck {
    /// An `Upgrade` or a `DeployWithMaxDataLen`.
    Upgrade,
    /// An `ExtendProgram`. `upgrade_slot` is the slot of the newest upgrade after the oldest
    /// IDL write, or `None` when there is none.
    Extend { upgrade_slot: Option<u64> },
    /// The check stopped at its limit, met a pruned transaction, or reached the end of the
    /// history above the oldest IDL write, so the slot can be an `ExtendProgram`.
    Unknown,
}

/// The result of [`check`] for one program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Staleness {
    /// The program in base58.
    pub program: String,
    /// The slot that the loader keeps: the last deploy, or a later `ExtendProgram`.
    pub deploy_slot: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extend_check: Option<ExtendCheck>,
    /// Each IDL account that counts, the Program Metadata account first.
    pub idls: Vec<IdlWrite>,
    /// [`Status::Stale`] when an IDL account is stale.
    pub status: Status,
}

/// The slots that [`check`] compares, before the extend check. A caller that runs often can
/// compare these first, and run [`Observed::into_staleness`] only when they change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Observed {
    /// The program in base58.
    pub program: String,
    pub deploy_slot: u64,
    /// Each IDL account that counts: its source, address, and the slot of its newest
    /// successful transaction.
    pub writes: Vec<(Source, String, u64)>,
    programdata: Option<String>,
}

/// Why [`check`] cannot tell the status of a program.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The program id is not an address, or a request or its answer failed.
    Onchain(onchain::Error),
    /// No account is at the program address.
    NoProgram(String),
    /// The program account keeps no deploy slot.
    NoDeploySlot { program: String, owner: String },
    /// The newest transactions of an IDL account hold no successful one, so the slot of its
    /// last write is unknown.
    NoWrite {
        program: String,
        source: Source,
        account: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Onchain(e) => write!(f, "{e}"),
            Self::NoProgram(program) => write!(
                f,
                "no account at {program}; check the address and the cluster of the RPC URL"
            ),
            Self::NoDeploySlot { program, owner } if owner == LOADER_V3 => write!(
                f,
                "program {program}: its ProgramData account holds no deploy slot, so the program can be closed"
            ),
            Self::NoDeploySlot { program, owner } => write!(
                f,
                "program {program}: its owner {owner} keeps no deploy slot; only the upgradeable loader and loader v4 do"
            ),
            Self::NoWrite {
                program,
                source,
                account,
            } => write!(
                f,
                "program {program}: the newest {IDL_PAGE} transactions of its {source} {account} hold no successful one, so the slot of the last IDL write is unknown"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<onchain::Error> for Error {
    fn from(e: onchain::Error) -> Self {
        Self::Onchain(e)
    }
}

/// One entry of `getSignaturesForAddress`; `err` is set when the transaction failed.
#[derive(Deserialize)]
struct SignatureInfo {
    signature: String,
    slot: u64,
    #[serde(default)]
    err: Option<Value>,
}

/// Signatures to read per IDL account. A failed transaction wrote nothing, so the newest
/// successful one of this page counts.
const IDL_PAGE: usize = 100;

fn signatures(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
    address: &str,
    config: &Value,
) -> Result<Vec<SignatureInfo>, onchain::Error> {
    let page = rpc("getSignaturesForAddress", json!([address, config]))?;
    Vec::<SignatureInfo>::deserialize(&page)
        .map_err(|e| onchain::Error::RpcResponse(format!("getSignaturesForAddress: {e}")))
}

/// Compare the last deploy of `program` with the last successful transaction on each of its
/// IDL accounts, or only on the account of `source`. An IDL account is stale when a deploy
/// came after that transaction. `rpc` sends one JSON-RPC request and returns its `result`.
///
/// The newest successful transaction that names an IDL account counts as its last write. A
/// later transaction that only names the account, such as a lamport transfer, hides a stale
/// IDL. When the slot of an upgradeable program is newer than an IDL write, `check` reads
/// what wrote the slot, with a bound on the requests: see [`ExtendCheck`].
///
/// # Errors
///
/// When `program` is not an address or not a deployed program with a deploy slot, a request
/// fails, or the newest transactions of an IDL account hold no successful one.
pub fn check(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
    program: &str,
    source: Option<Source>,
) -> Result<Staleness, Error> {
    observe(&mut rpc, program, source)?.into_staleness(rpc)
}

/// The slots that [`check`] compares, with no extend check.
///
/// # Errors
///
/// As [`check`].
pub fn observe(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
    program: &str,
    source: Option<Source>,
) -> Result<Observed, Error> {
    let key =
        onchain::parse_key(program).ok_or_else(|| onchain::Error::Program(program.to_string()))?;
    let program = bs58::encode(key).into_string();
    let (deploy, programdata) = read_deploy(&mut rpc, &program, onchain::Error::RpcResponse)?;
    let deploy_slot = match deploy {
        Deploy::Slot(slot) => slot,
        Deploy::NoAccount => return Err(Error::NoProgram(program)),
        Deploy::NoSlot { owner } => return Err(Error::NoDeploySlot { program, owner }),
    };
    let mut writes = Vec::new();
    for (found, account) in onchain::idl_accounts(&mut rpc, &key)? {
        if source.is_some_and(|asked| asked != found) {
            continue;
        }
        let page = signatures(&mut rpc, &account, &json!({ "limit": IDL_PAGE }))?;
        let Some(newest) = page.iter().find(|e| e.err.is_none()) else {
            return Err(Error::NoWrite {
                program,
                source: found,
                account,
            });
        };
        writes.push((found, account, newest.slot));
    }
    Ok(Observed {
        program,
        deploy_slot,
        writes,
        programdata,
    })
}

impl Observed {
    /// The status of each IDL account and of the program, after the extend check.
    ///
    /// # Errors
    ///
    /// When a request of the extend check fails.
    pub fn into_staleness(
        self,
        mut rpc: impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
    ) -> Result<Staleness, Error> {
        let deploy_slot = self.deploy_slot;
        // The check costs requests, so it runs only when an IDL write is older than the slot.
        let floor = self
            .writes
            .iter()
            .map(|(_, _, slot)| *slot)
            .filter(|slot| *slot < deploy_slot)
            .min();
        let extend_check = match (floor, &self.programdata) {
            (Some(floor), Some(programdata)) => {
                Some(extend_check(&mut rpc, programdata, deploy_slot, floor)?)
            }
            _ => None,
        };
        // A write in the deploy slot can come after the deploy, so it counts as fresh.
        let stale = |slot: u64| {
            slot < deploy_slot
                && match extend_check {
                    Some(ExtendCheck::Extend { upgrade_slot }) => {
                        upgrade_slot.is_some_and(|upgrade| slot < upgrade)
                    }
                    _ => true,
                }
        };
        let idls: Vec<IdlWrite> = self
            .writes
            .into_iter()
            .map(|(source, account, slot)| IdlWrite {
                source,
                account,
                slot,
                status: if stale(slot) {
                    Status::Stale
                } else {
                    Status::Fresh
                },
            })
            .collect();
        let status = if idls.is_empty() {
            Status::NoIdl
        } else if idls.iter().any(|w| w.status == Status::Stale) {
            Status::Stale
        } else {
            Status::Fresh
        };
        Ok(Staleness {
            program: self.program,
            deploy_slot,
            extend_check,
            idls,
            status,
        })
    }
}

/// `UpgradeableLoaderInstruction` tags; bincode writes the variant index as a `u32`.
const DEPLOY_WITH_MAX_DATA_LEN: u32 = 2;
const UPGRADE: u32 = 3;
const EXTEND_PROGRAM: u32 = 6;
/// Only in solana-loader-v3-interface 5 and 6.
const EXTEND_PROGRAM_CHECKED: u32 = 9;

/// Bounds of the extend check: at most 2 + `EXTEND_PAGES` + `EXTEND_FETCHES` requests. Other
/// programs can name a `ProgramData` account in many transactions.
const EXTEND_PAGE: usize = 1000;
const EXTEND_PAGES: usize = 3;
const EXTEND_FETCHES: usize = 10;

/// What a transaction did to a `ProgramData` account.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Write {
    Upgrade,
    Extend,
    Other,
}

/// Read what wrote `slot` into `programdata`, and when it is an `ExtendProgram`, the newest
/// upgrade after `floor`. It reads the successful transactions that name the account, newest
/// first, from `slot` down to `floor`.
fn extend_check(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
    programdata: &str,
    slot: u64,
    floor: u64,
) -> Result<ExtendCheck, onchain::Error> {
    let mut before = later_signature(rpc, slot);
    let mut fetched = 0;
    let mut extended = false;
    for _ in 0..EXTEND_PAGES {
        let mut config = json!({ "limit": EXTEND_PAGE });
        if let Some(signature) = &before {
            config["before"] = json!(signature);
        }
        let page = signatures(rpc, programdata, &config)?;
        for entry in &page {
            if entry.slot > slot || entry.err.is_some() {
                continue;
            }
            if entry.slot <= floor {
                return Ok(finish(extended));
            }
            if fetched == EXTEND_FETCHES {
                return Ok(ExtendCheck::Unknown);
            }
            fetched += 1;
            let config = json!({ "encoding": "json", "maxSupportedTransactionVersion": 0 });
            let tx = rpc("getTransaction", json!([entry.signature, config]))?;
            // A pruned transaction can be the upgrade, so the check cannot rule one out.
            if tx.is_null() {
                return Ok(ExtendCheck::Unknown);
            }
            match classify(&tx, programdata) {
                Write::Upgrade if entry.slot == slot => return Ok(ExtendCheck::Upgrade),
                // An upgrade before the slot explains the slot only after an extend in it.
                Write::Upgrade if extended => {
                    return Ok(ExtendCheck::Extend {
                        upgrade_slot: Some(entry.slot),
                    })
                }
                Write::Upgrade => return Ok(ExtendCheck::Unknown),
                Write::Extend if entry.slot == slot => extended = true,
                Write::Extend | Write::Other => {}
            }
        }
        // The history ends above the floor, so an upgrade can be in the part that is missing.
        if page.len() < EXTEND_PAGE {
            return Ok(ExtendCheck::Unknown);
        }
        before = page.last().map(|e| e.signature.clone());
    }
    Ok(ExtendCheck::Unknown)
}

/// The result when the list reached the floor with no upgrade after it.
fn finish(extended: bool) -> ExtendCheck {
    if extended {
        ExtendCheck::Extend { upgrade_slot: None }
    } else {
        ExtendCheck::Unknown
    }
}

/// A signature of the first block after `slot`. Agave reads `before` as a place in the ledger,
/// also for a transaction that does not name the address, so the list then starts at `slot`.
/// When this fails, the list starts at the newest transaction.
fn later_signature(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
    slot: u64,
) -> Option<String> {
    let next = rpc("getBlocksWithLimit", json!([slot.saturating_add(1), 1])).ok()?;
    let next = next.as_array()?.first()?.as_u64()?;
    let config = json!({ "transactionDetails": "signatures", "rewards": false,
        "maxSupportedTransactionVersion": 0 });
    let block = rpc("getBlock", json!([next, config])).ok()?;
    block["signatures"]
        .as_array()?
        .first()?
        .as_str()
        .map(str::to_string)
}

/// The loader instructions of `tx`, outer and inner, that name `programdata`.
fn classify(tx: &Value, programdata: &str) -> Write {
    let message = &tx["transaction"]["message"];
    let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let mut keys: Vec<String> = list(&message["accountKeys"])
        .iter()
        .filter_map(|k| k.as_str().map(str::to_string))
        .collect();
    // A version 0 transaction loads more keys from lookup tables: writable, then read-only.
    for kind in ["writable", "readonly"] {
        let loaded = list(&tx["meta"]["loadedAddresses"][kind]);
        keys.extend(loaded.iter().filter_map(|k| k.as_str().map(str::to_string)));
    }
    let key = |index: &Value| {
        index
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| keys.get(i))
            .map(String::as_str)
    };
    let inner = list(&tx["meta"]["innerInstructions"])
        .iter()
        .flat_map(|group| list(&group["instructions"]))
        .collect::<Vec<_>>();
    let mut found = Write::Other;
    for ix in list(&message["instructions"]).iter().chain(&inner) {
        let names = list(&ix["accounts"])
            .iter()
            .any(|a| key(a) == Some(programdata));
        if key(&ix["programIdIndex"]) != Some(LOADER_V3) || !names {
            continue;
        }
        let data = ix["data"]
            .as_str()
            .and_then(|d| bs58::decode(d).into_vec().ok())
            .unwrap_or_default();
        match data.first_chunk::<4>().map(|tag| u32::from_le_bytes(*tag)) {
            Some(DEPLOY_WITH_MAX_DATA_LEN | UPGRADE) => return Write::Upgrade,
            Some(EXTEND_PROGRAM | EXTEND_PROGRAM_CHECKED) => found = Write::Extend,
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
    const ANCHOR_ACCOUNT: &str = "AYgC53tU5BbP2NAnv5nConJxAdpQZctvmZK88pu69xRs";

    fn account(owner: &str, data: &[u8]) -> Value {
        json!({ "owner": owner, "data": [STANDARD.encode(data), "base64"],
            "lamports": 1, "executable": false })
    }

    /// An RPC for `PROGRAM`: its account (`program`), its `ProgramData` slot when it has one,
    /// and the IDL accounts that exist, each with the slot of its newest transaction.
    fn chain(
        program: Option<Value>,
        programdata_slot: Option<u64>,
        idls: Vec<(&'static str, &'static str, Option<u64>)>,
    ) -> impl FnMut(&str, Value) -> Result<Value, onchain::Error> {
        let programdata = bs58::encode([7u8; 32]).into_string();
        move |method, params| {
            Ok(match method {
                "getAccountInfo" if params[0] == PROGRAM => json!({ "value": program }),
                "getAccountInfo" if params[0] == programdata.as_str() => json!({ "value":
                    programdata_slot.map(|s| account(LOADER_V3,
                        &[&[3, 0, 0, 0][..], &s.to_le_bytes()].concat())) }),
                "getMultipleAccounts" => {
                    let value: Vec<Value> = params[0]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|key| {
                            idls.iter()
                                .find(|(a, _, _)| key == *a)
                                .map_or(Value::Null, |(_, owner, _)| account(owner, &[]))
                        })
                        .collect();
                    json!({ "context": { "slot": 1 }, "value": value })
                }
                "getSignaturesForAddress" => {
                    let slot = idls
                        .iter()
                        .find(|(a, _, _)| params[0] == *a)
                        .and_then(|(_, _, slot)| *slot);
                    json!(slot.map_or_else(Vec::new, |s| vec![
                        json!({ "signature": "S", "slot": s, "err": null })
                    ]))
                }
                _ => panic!("unexpected {method}"),
            })
        }
    }

    fn upgradeable() -> Value {
        account(LOADER_V3, &[&[2, 0, 0, 0][..], &[7; 32]].concat())
    }

    #[test]
    fn reads_the_deploy_slot_of_each_loader() {
        let deploy = |program, slot| {
            deploy_slot(
                chain(program, slot, vec![]),
                PROGRAM,
                onchain::Error::RpcResponse,
            )
            .unwrap()
        };
        assert_eq!(deploy(Some(upgradeable()), Some(100)), Deploy::Slot(100));
        let v4 = account(LOADER_V4, &[&42u64.to_le_bytes()[..], &[0; 40]].concat());
        assert_eq!(deploy(Some(v4), None), Deploy::Slot(42));
        assert_eq!(deploy(None, None), Deploy::NoAccount);
        // A closed upgradeable program, and a program of the immutable loader v2.
        let owner = LOADER_V3.to_string();
        assert_eq!(deploy(Some(upgradeable()), None), Deploy::NoSlot { owner });
        let v2 = "BPFLoader2111111111111111111111111111111111";
        let owner = v2.to_string();
        assert_eq!(
            deploy(Some(account(v2, &[0; 36])), None),
            Deploy::NoSlot { owner }
        );
    }

    #[test]
    fn a_closed_program_or_an_idl_with_no_history_is_an_error() {
        let err = check(chain(Some(upgradeable()), None, vec![]), PROGRAM, None).unwrap_err();
        assert!(err.to_string().contains("can be closed"), "{err}");
        let pruned = vec![(ANCHOR_ACCOUNT, PROGRAM, None)];
        let err = check(chain(Some(upgradeable()), Some(1), pruned), PROGRAM, None).unwrap_err();
        assert!(matches!(err, Error::NoWrite { .. }), "{err}");
        let err = check(chain(None, None, vec![]), "not-a-key", None).unwrap_err();
        assert!(
            matches!(err, Error::Onchain(onchain::Error::Program(_))),
            "{err}"
        );
    }
}
