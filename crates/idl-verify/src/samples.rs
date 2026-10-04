//! Loader for `samples.json`: raw account and instruction data captured from a cluster.
//!
//! ```json
//! {
//!   "programId": "vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8",
//!   "accounts": [{ "address": "...", "data": "<base64>" }],
//!   "instructions": [{ "signature": "...", "data": "<base64>", "accounts": ["<base58>"] }]
//! }
//! ```
//!
//! Any other keys, such as an account's `type` label, are ignored: samples are classified
//! by discriminator, never by a label.

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
                Ok(InstructionSample {
                    data: decode_base64(&ix.data, "instructions", index)?,
                    signature: ix.signature,
                    accounts: ix.accounts,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self {
            program_id: raw.program_id.filter(|id| !id.is_empty()),
            accounts,
            instructions,
        })
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
