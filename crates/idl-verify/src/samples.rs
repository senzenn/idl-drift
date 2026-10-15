//! Loader for `samples.json`: raw account and instruction data captured from a cluster.
//!
//! ```json
//! {
//!   "programId": "vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8",
//!   "accounts": [{ "address": "...", "data": "<base64>" }],
//!   "instructions": [{ "signature": "...", "data": "<base64>", "accounts": ["<base58>"],
//!                      "signer": [true], "writable": [false], "cpi": false }],
//!   "events": [{ "signature": "...", "source": "log", "data": "<base64>" }]
//! }
//! ```
//!
//! Any other keys, such as an account's `type` label, are ignored: samples are classified
//! by discriminator, never by a label. `signer`, `writable` and `cpi` are optional, so older
//! files still load.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;

use crate::error::Error;

/// Every sample in one `samples.json`, with `data` decoded from base64.
#[derive(Debug, Clone, Default)]
pub struct Samples {
    pub program_id: Option<String>,
    pub accounts: Vec<AccountSample>,
    pub instructions: Vec<InstructionSample>,
    pub events: Vec<EventSample>,
}

/// One account's raw data.
#[derive(Debug, Clone)]
pub struct AccountSample {
    pub address: String,
    pub data: Vec<u8>,
}

/// One instruction addressed to the program: its data and account keys in order.
#[derive(Debug, Clone)]
pub struct InstructionSample {
    pub signature: String,
    pub data: Vec<u8>,
    pub accounts: Vec<String>,
    /// What the transaction message says about each key; `None` when the sample has no flags.
    pub flags: Option<KeyFlags>,
}

/// Message-level privileges of an instruction's keys, one entry per key in `accounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFlags {
    /// The key signs the transaction.
    pub signer: Vec<bool>,
    /// The message makes the key writable.
    pub writable: Vec<bool>,
    /// The instruction is a CPI, not a top-level instruction.
    pub cpi: bool,
}

/// Where an event sample came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventSource {
    /// A `Program data:` log line inside the program's own invoke frame.
    Log,
    /// A self-CPI event instruction; `data` still holds the envelope, such as Anchor's tag.
    Cpi,
}

/// One event the program emitted, as raw bytes.
#[derive(Debug, Clone)]
pub struct EventSample {
    pub signature: String,
    pub source: EventSource,
    pub data: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SamplesJson {
    #[serde(default)]
    program_id: Option<String>,
    #[serde(default)]
    accounts: Vec<AccountJson>,
    #[serde(default)]
    instructions: Vec<InstructionJson>,
    #[serde(default)]
    events: Vec<EventJson>,
}

#[derive(Deserialize)]
struct EventJson {
    #[serde(default)]
    signature: String,
    source: EventSource,
    data: String,
}

#[derive(Deserialize)]
struct AccountJson {
    #[serde(default)]
    address: String,
    data: String,
}

#[derive(Deserialize)]
struct InstructionJson {
    #[serde(default)]
    signature: String,
    data: String,
    #[serde(default)]
    accounts: Vec<String>,
    #[serde(default)]
    signer: Option<Vec<bool>>,
    #[serde(default)]
    writable: Option<Vec<bool>>,
    #[serde(default)]
    cpi: Option<bool>,
}

impl Samples {
    /// Parse `samples.json` text.
    ///
    /// # Errors
    ///
    /// When the text is not samples JSON, or a `data` field is not base64.
    pub fn from_json(src: &str) -> Result<Self, Error> {
        let raw: SamplesJson = serde_json::from_str(src).map_err(Error::SamplesJson)?;
        let accounts = raw
            .accounts
            .into_iter()
            .enumerate()
            .map(|(index, a)| {
                Ok(AccountSample {
                    data: decode_base64(&a.data, "accounts", index)?,
                    address: a.address,
                })
            })
            .collect::<Result<_, Error>>()?;
        let instructions = raw
            .instructions
            .into_iter()
            .enumerate()
            .map(|(index, ix)| {
                let flags = match (ix.signer, ix.writable, ix.cpi) {
                    (None, None, None) => None,
                    (Some(signer), Some(writable), Some(cpi))
                        if signer.len() == ix.accounts.len()
                            && writable.len() == ix.accounts.len() =>
                    {
                        Some(KeyFlags {
                            signer,
                            writable,
                            cpi,
                        })
                    }
                    _ => return Err(Error::SampleFlags { index }),
                };
                Ok(InstructionSample {
                    data: decode_base64(&ix.data, "instructions", index)?,
                    signature: ix.signature,
                    accounts: ix.accounts,
                    flags,
                })
            })
            .collect::<Result<_, Error>>()?;
        let events = raw
            .events
            .into_iter()
            .enumerate()
            .map(|(index, e)| {
                Ok(EventSample {
                    data: decode_base64(&e.data, "events", index)?,
                    signature: e.signature,
                    source: e.source,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self {
            program_id: raw.program_id.filter(|id| !id.is_empty()),
            accounts,
            instructions,
            events,
        })
    }
}

impl Samples {
    /// The samples as `samples.json` text, so a live run can be saved and rerun offline.
    #[must_use]
    pub fn to_json(&self) -> String {
        let accounts: Vec<serde_json::Value> = self
            .accounts
            .iter()
            .map(|a| serde_json::json!({ "address": a.address, "data": STANDARD.encode(&a.data) }))
            .collect();
        let instructions: Vec<serde_json::Value> = self
            .instructions
            .iter()
            .map(|ix| {
                let mut v = serde_json::json!({ "signature": ix.signature,
                                                "data": STANDARD.encode(&ix.data),
                                                "accounts": ix.accounts });
                if let Some(flags) = &ix.flags {
                    v["signer"] = serde_json::json!(flags.signer);
                    v["writable"] = serde_json::json!(flags.writable);
                    v["cpi"] = serde_json::json!(flags.cpi);
                }
                v
            })
            .collect();
        let events: Vec<serde_json::Value> = self
            .events
            .iter()
            .map(|e| {
                let source = match e.source {
                    EventSource::Log => "log",
                    EventSource::Cpi => "cpi",
                };
                serde_json::json!({ "signature": e.signature, "source": source,
                                    "data": STANDARD.encode(&e.data) })
            })
            .collect();
        let doc = serde_json::json!({
            "programId": self.program_id,
            "accounts": accounts,
            "instructions": instructions,
            "events": events,
        });
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    }
}

fn decode_base64(data: &str, list: &'static str, index: usize) -> Result<Vec<u8>, Error> {
    STANDARD.decode(data).map_err(|source| Error::SampleData {
        list,
        index,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_and_ignores_labels() {
        let samples = Samples::from_json(
            r#"{"programId":"P","transactions":3,
                "accounts":[{"address":"A","type":"Vault","data":"AQID"}],
                "instructions":[{"signature":"S","data":"BA==","accounts":["X","Y"]}]}"#,
        )
        .unwrap();
        assert_eq!(samples.program_id.as_deref(), Some("P"));
        assert_eq!(samples.accounts[0].data, [1, 2, 3]);
        assert_eq!(samples.instructions[0].data, [4]);
        assert_eq!(samples.instructions[0].accounts, ["X", "Y"]);
    }

    #[test]
    fn events_are_optional_and_keep_their_source() {
        let samples = Samples::from_json(
            r#"{"events":[{"signature":"S","source":"cpi","data":"5EWlLlHLmh0="}]}"#,
        )
        .unwrap();
        assert_eq!(samples.events[0].source, EventSource::Cpi);
        assert_eq!(samples.events[0].data[..2], [0xe4, 0x45]);
        assert!(Samples::from_json("{}").unwrap().events.is_empty());
    }

    #[test]
    fn saved_samples_load_back_unchanged() {
        let src = r#"{"programId":"P","accounts":[{"address":"A","data":"AQID"}],
            "instructions":[{"signature":"S","data":"BA==","accounts":["X"]},
                {"signature":"S","data":"BA==","accounts":["X"],"signer":[true],"writable":[false],"cpi":true}],
            "events":[{"signature":"S","source":"log","data":"BQY="}]}"#;
        let samples = Samples::from_json(src).unwrap();
        let again = Samples::from_json(&samples.to_json()).unwrap();
        assert_eq!(again.to_json(), samples.to_json());
        assert_eq!(again.events[0].source, EventSource::Log);
        assert_eq!(again.instructions[0].accounts, ["X"]);
        assert!(again.instructions[0].flags.is_none());
        let flags = KeyFlags {
            signer: vec![true],
            writable: vec![false],
            cpi: true,
        };
        assert_eq!(again.instructions[1].flags, Some(flags));
    }

    #[test]
    fn flags_must_come_together_and_cover_every_account() {
        for ix in [
            r#"{"data":"BA==","accounts":["X","Y"],"signer":[true],"writable":[true,false],"cpi":false}"#,
            r#"{"data":"BA==","accounts":["X"],"signer":[true]}"#,
        ] {
            let err = Samples::from_json(&format!(r#"{{"instructions":[{ix}]}}"#))
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("instructions[0]: give `signer`"), "{err}");
        }
    }

    #[test]
    fn bad_base64_names_the_sample() {
        let err = Samples::from_json(r#"{"accounts":[{"data":"AQID"},{"data":"%%"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("accounts[1].data is not valid base64"),
            "{err}"
        );
    }
}
