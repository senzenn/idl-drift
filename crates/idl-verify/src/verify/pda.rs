//! PDA check: derive each account address from the seeds in the IDL and compare it with the
//! keys that the sample passes.

use idl_drift::model::{Account, Pda, Seed};
use sha2::{Digest, Sha256};

use super::instructions::Places;
use super::{Outcomes, Verifier};
use crate::decode::Value;
use crate::report::{Check, Severity};
use crate::text::of_samples;

/// Solana allows at most 16 seeds, the bump included, of at most 32 bytes each.
const MAX_SEEDS: usize = 16;
const MAX_SEED_LEN: usize = 32;

type Key = [u8; 32];

impl Verifier<'_> {
    /// Two accounts with the same flags can change places with no flag finding, but a derived
    /// address fits only its own position. A seed that cannot be resolved skips the sample.
    pub(super) fn report_pda_addresses(
        &mut self,
        subject: &str,
        flat: &[&Account],
        outcomes: &Outcomes,
        places: &Places,
    ) {
        if flat.iter().all(|a| a.pda.is_none()) {
            return;
        }
        let Some(program_id) = decode_key(self.program_id()) else {
            return;
        };
        let samples = &self.samples.instructions;
        let all_keys: Vec<Vec<Option<Key>>> = outcomes
            .iter()
            .map(|(i, _)| {
                let keys = &samples[*i].accounts;
                match places.get(i) {
                    // Line the keys up with the IDL, and drop a key whose account is not certain.
                    Some(at) => at
                        .iter()
                        .map(|s| s.and_then(|s| keys.get(s)).and_then(|k| decode_key(k)))
                        .collect(),
                    None => keys.iter().map(|k| decode_key(k)).collect(),
                }
            })
            .collect();
        let mut findings = Vec::new();
        for (pos, account) in flat.iter().enumerate() {
            let Some(pda) = &account.pda else {
                continue;
            };
            let mut checked = 0;
            let mut moved = Vec::new();
            let mut nowhere = Vec::new();
            for ((i, decoded), keys) in outcomes.iter().zip(&all_keys) {
                let sample = SampleSeeds {
                    flat,
                    keys,
                    args: decoded.as_ref().ok().map(|(value, _)| value),
                    program_id,
                };
                let Some(found) = sample.find(pda, pos) else {
                    continue;
                };
                checked += 1;
                match found {
                    Found::Here => {}
                    Found::At(at) => moved.push((*i, at)),
                    Found::Nowhere => nowhere.push(*i),
                }
            }
            if let Some(&(first, at)) = moved.first() {
                findings.push((
                    Severity::Error,
                    first,
                    format!(
                        "account #{pos} `{}` derives from its seeds in the IDL to the key passed at #{at}, not #{pos}, in {}",
                        account.name,
                        of_samples(moved.len(), checked)
                    ),
                    "the IDL's account order likely does not match the program, or an earlier account is missing",
                ));
            }
            if let Some(&first) = nowhere.first() {
                let key = samples[first].accounts.get(pos).map_or("", String::as_str);
                findings.push((
                    Severity::Note,
                    first,
                    format!(
                        "account #{pos} `{}`: the key passed there does not derive from its seeds in the IDL, in {} (such as {key})",
                        account.name,
                        of_samples(nowhere.len(), checked)
                    ),
                    "the seeds may read data that the IDL does not show, or the account order does not match the program",
                ));
            }
        }
        for (severity, first, message, hint) in findings {
            let example = samples[first].signature.clone();
            self.push(
                severity,
                Check::Pda,
                subject,
                message,
                Some(hint.to_string()),
                Some(&example),
            );
        }
    }
}

/// Where a sample passes the address that an account's seeds give.
enum Found {
    Here,
    At(usize),
    Nowhere,
}

/// What one sample gives to resolve seeds that name an account or an argument.
struct SampleSeeds<'s> {
    flat: &'s [&'s Account],
    keys: &'s [Option<Key>],
    /// The decoded instruction arguments; `None` when they do not decode.
    args: Option<&'s Value>,
    program_id: Key,
}

impl SampleSeeds<'_> {
    /// Where the sample passes the address that `pda` gives for account `pos`; `None` when a
    /// seed cannot be resolved.
    fn find(&self, pda: &Pda, pos: usize) -> Option<Found> {
        let key = (*self.keys.get(pos)?)?;
        // Anchor passes the program id in place of an absent optional account.
        if self.flat.get(pos)?.optional && key == self.program_id {
            return None;
        }
        let seeds = pda
            .seeds
            .iter()
            .map(|seed| self.seed(seed, pos))
            .collect::<Option<Vec<_>>>()?;
        if seeds.len() >= MAX_SEEDS || seeds.iter().any(|s| s.len() > MAX_SEED_LEN) {
            return None;
        }
        let declared: Option<Key> = match &pda.program {
            Some(seed) => Some(self.seed(seed, pos)?.try_into().ok()?),
            None => None,
        };
        // A program id in the IDL can come from another deployment, so try the sampled one too.
        let mut found = Found::Nowhere;
        for program in declared.iter().chain([&self.program_id]) {
            match derive(&seeds, program, self.keys, pos) {
                Some(at) if at == pos => return Some(Found::Here),
                Some(at) if matches!(found, Found::Nowhere) => found = Found::At(at),
                _ => {}
            }
        }
        Some(found)
    }

    /// The bytes of `seed` when checking account `pos`.
    fn seed(&self, seed: &Seed, pos: usize) -> Option<Vec<u8>> {
        match seed {
            Seed::Const(bytes) => Some(bytes.clone()),
            Seed::Account(name) => {
                let names: Vec<&str> = self.flat.iter().map(|a| a.name.as_str()).collect();
                let at = by_name(&names, name)?;
                // An account cannot derive from its own key. Anchor writes this when the real
                // seed reads the account's data.
                if at == pos {
                    return None;
                }
                let key = (*self.keys.get(at)?)?;
                // An absent optional account gives no seed.
                if self.flat.get(at)?.optional && key == self.program_id {
                    return None;
                }
                Some(key.to_vec())
            }
            Seed::Arg(path) => {
                // Each part of `params.id` names a field of a struct, as in Anchor's client.
                let mut value = self.args?;
                for name in path.split('.') {
                    let Value::Struct(fields) = value else {
                        return None;
                    };
                    let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                    value = &fields.get(by_name(&names, name)?)?.1;
                }
                arg_bytes(value)
            }
            Seed::Other => None,
        }
    }
}

/// The position of `name` in `names`: the one exact match, or else the one match that ignores
/// case and underscores. Legacy Anchor seeds give `snake_case` paths for `camelCase` names.
fn by_name(names: &[&str], name: &str) -> Option<usize> {
    let loose = |s: &str| s.replace('_', "").to_lowercase();
    let only = |same: &dyn Fn(&str) -> bool| -> Vec<usize> {
        (0..names.len()).filter(|&i| same(names[i])).collect()
    };
    match only(&|n| n == name).as_slice() {
        [at] => Some(*at),
        [] => match only(&|n| loose(n) == loose(name)).as_slice() {
            [at] => Some(*at),
            _ => None,
        },
        _ => None,
    }
}

/// An argument as a seed, the way Anchor's client encodes it: a number in little-endian order
/// at its own width, a key as its 32 bytes, a string or byte array as its bytes.
fn arg_bytes(value: &Value) -> Option<Vec<u8>> {
    Some(match value {
        Value::U8(v) => vec![*v],
        Value::U16(v) => v.to_le_bytes().to_vec(),
        Value::U32(v) => v.to_le_bytes().to_vec(),
        Value::U64(v) => v.to_le_bytes().to_vec(),
        Value::U128(v) => v.to_le_bytes().to_vec(),
        Value::I8(v) => v.to_le_bytes().to_vec(),
        Value::I16(v) => v.to_le_bytes().to_vec(),
        Value::I32(v) => v.to_le_bytes().to_vec(),
        Value::I64(v) => v.to_le_bytes().to_vec(),
        Value::I128(v) => v.to_le_bytes().to_vec(),
        Value::Pubkey(key) => key.to_vec(),
        Value::String(s) => s.as_bytes().to_vec(),
        Value::Bytes(bytes) => bytes.clone(),
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect::<Option<Vec<u8>>>()?,
        _ => return None,
    })
}

fn decode_key(key: &str) -> Option<Key> {
    bs58::decode(key).into_vec().ok()?.try_into().ok()
}

/// The position in `keys` of an address that `seeds` give under `program`, `pos` first.
/// Every bump counts. A match proves the address, so no curve check is needed.
fn derive(seeds: &[Vec<u8>], program: &Key, keys: &[Option<Key>], pos: usize) -> Option<usize> {
    let mut prefix = Sha256::new();
    for seed in seeds {
        prefix.update(seed);
    }
    let mut elsewhere = None;
    for bump in (0..=u8::MAX).rev() {
        let mut hash = prefix.clone();
        hash.update([bump]);
        hash.update(program);
        hash.update(b"ProgramDerivedAddress");
        let address = Some(Key::from(hash.finalize()));
        if keys.get(pos) == Some(&address) {
            return Some(pos);
        }
        elsewhere = elsewhere.or_else(|| keys.iter().position(|k| *k == address));
    }
    elsewhere
}

#[cfg(test)]
mod tests {
    use idl_drift::model::Idl;

    use crate::report::{Report, Severity};
    use crate::samples::{InstructionSample, Samples};
    use crate::verify;

    /// From a live voltr `deposit_vault`: the depositor, the vault, the depositor's token
    /// account (an ATA), the mint and the token program.
    const KEYS: [&str; 5] = [
        "FA5nYKRL48MySGgLK3m5kXpmozWf37iGHNqCDRxW1Gri",
        "1VTBKh5gG75QjwnxX8CgpvTKgwSwLgCRLgaeGw8zzhG",
        "5sSMpeM3t4fHTzRg1Rg7uddRgujL8T7AJjcgYzHZQDyZ",
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    ];
    const SWAPPED: [&str; 5] = [KEYS[0], KEYS[2], KEYS[1], KEYS[3], KEYS[4]];
    const VOLTR: &str = "vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8";

    /// An Anchor IDL of voltr with one instruction `go`, discriminator `[1]`.
    fn anchor(accounts: &str, args: &str) -> Idl {
        Idl::from_json(&format!(
            r#"{{"address":"{VOLTR}","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
            "instructions":[{{"name":"go","discriminator":[1],"accounts":{accounts},"args":{args}}}]}}"#
        ))
        .unwrap()
    }

    fn sample(data: &[u8], keys: &[&str]) -> Samples {
        Samples {
            instructions: vec![InstructionSample {
                signature: "sig".into(),
                data: data.to_vec(),
                accounts: keys.iter().map(ToString::to_string).collect(),
                flags: None,
            }],
            ..Samples::default()
        }
    }

    fn findings(report: &Report) -> Vec<(Severity, &str)> {
        report
            .findings
            .iter()
            .map(|f| (f.severity, f.message.as_str()))
            .collect()
    }

    /// `ata` derives from `user`, `token_program` and `mint` under the ATA program.
    fn anchor_accounts(ata_then_vault: bool) -> String {
        let ata = r#"{"name":"ata","writable":true,"pda":{"seeds":[
            {"kind":"account","path":"user"},{"kind":"account","path":"token_program"},
            {"kind":"account","path":"mint"}],"program":{"kind":"const","value":[140,151,37,143,
            78,36,137,241,187,61,16,41,20,142,13,131,11,90,19,153,218,255,16,132,4,142,123,216,
            219,233,248,89]}}}"#;
        let vault = r#"{"name":"vault","writable":true}"#;
        let (first, second) = if ata_then_vault {
            (ata, vault)
        } else {
            (vault, ata)
        };
        format!(
            r#"[{{"name":"user","signer":true}},{first},{second},{{"name":"mint"}},{{"name":"token_program"}}]"#
        )
    }

    #[test]
    fn swapped_accounts_with_the_same_flags_fail_the_seed_check() {
        let report = verify(
            &anchor(&anchor_accounts(false), "[]"),
            &sample(&[1], &KEYS),
            None,
        );
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        let report = verify(
            &anchor(&anchor_accounts(true), "[]"),
            &sample(&[1], &KEYS),
            None,
        );
        assert_eq!(
            findings(&report),
            [(Severity::Error, "account #1 `ata` derives from its seeds in the IDL to the key passed at #2, not #1, in 1 of 1 sample")]
        );
    }

    #[test]
    fn an_argument_seed_comes_from_the_decoded_arguments() {
        let idl = anchor(
            r#"[{"name":"order","pda":{"seeds":[{"kind":"const","value":[111,114,100,101,114]},
            {"kind":"arg","path":"id"}]}}]"#,
            r#"[{"name":"id","type":"u64"}]"#,
        );
        // The PDA of `["order", 7u64.to_le_bytes()]` under voltr.
        let order = ["DYFXbWma5QHoWZbUWoQ5qszknvT3g45Qapxkhakfaa1F"];
        let report = verify(&idl, &sample(&[1, 7, 0, 0, 0, 0, 0, 0, 0], &order), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        let report = verify(&idl, &sample(&[1, 8, 0, 0, 0, 0, 0, 0, 0], &order), None);
        assert_eq!(
            findings(&report),
            [(Severity::Note, "account #0 `order`: the key passed there does not derive from its seeds in the IDL, in 1 of 1 sample (such as DYFXbWma5QHoWZbUWoQ5qszknvT3g45Qapxkhakfaa1F)")]
        );
    }

    #[test]
    fn a_seed_can_read_a_field_inside_a_struct_argument() {
        let idl = Idl::from_json(&format!(
            r#"{{"address":"{VOLTR}","metadata":{{"name":"t","version":"0","spec":"0.1.0"}},
            "instructions":[{{"name":"go","discriminator":[1],"accounts":[
                {{"name":"order","pda":{{"seeds":[{{"kind":"const","value":[111,114,100,101,114]}},
                {{"kind":"arg","path":"params.inner.id"}}]}}}},
                {{"name":"other","pda":{{"seeds":[{{"kind":"arg","path":"params.flag.unwrap_or(0)"}}]}}}}],
            "args":[{{"name":"params","type":{{"defined":{{"name":"Params"}}}}}}]}}],
            "types":[
                {{"name":"Params","type":{{"kind":"struct","fields":[{{"name":"flag","type":"u8"}},
                {{"name":"inner","type":{{"defined":{{"name":"Inner"}}}}}}]}}}},
                {{"name":"Inner","type":{{"kind":"struct","fields":[{{"name":"id","type":"u64"}}]}}}}]}}"#
        ))
        .unwrap();
        // `order` is the PDA of `["order", 7u64.to_le_bytes()]`; `other` has a seed that
        // cannot be resolved, so the check skips it.
        let keys = ["DYFXbWma5QHoWZbUWoQ5qszknvT3g45Qapxkhakfaa1F", KEYS[0]];
        let report = verify(&idl, &sample(&[1, 9, 7, 0, 0, 0, 0, 0, 0, 0], &keys), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        let report = verify(&idl, &sample(&[1, 9, 8, 0, 0, 0, 0, 0, 0, 0], &keys), None);
        assert_eq!(
            findings(&report),
            [(Severity::Note, "account #0 `order`: the key passed there does not derive from its seeds in the IDL, in 1 of 1 sample (such as DYFXbWma5QHoWZbUWoQ5qszknvT3g45Qapxkhakfaa1F)")]
        );
    }

    /// A Codama IDL of voltr with one instruction `go`, discriminator `[1]`, whose accounts are
    /// `KEYS` with `ata` and `vault` swapped. `extra` puts an optional account at that position.
    fn codama(strategy: &str, extra: Option<usize>) -> Idl {
        let account = |name: &str, writable: bool, signer: bool, default: &str| {
            format!(
                r#"{{"kind":"instructionAccountNode","name":"{name}","isWritable":{writable},
                "isSigner":{signer}{default}}}"#
            )
        };
        let seed = |name: &str| {
            format!(
                r#"{{"kind":"pdaSeedValueNode","name":"{name}","value":{{"kind":"accountValueNode","name":"{name}"}}}}"#
            )
        };
        let ata = account(
            "ata",
            true,
            false,
            &format!(
                r#","defaultValue":{{"kind":"pdaValueNode","pda":{{"kind":"pdaLinkNode","name":"ata"}},
                "seeds":[{},{},{}]}}"#,
                seed("user"),
                seed("tokenProgram"),
                seed("mint")
            ),
        );
        let mut accounts = vec![
            account("user", false, true, ""),
            ata,
            account("vault", true, false, ""),
            account("mint", false, false, ""),
            account("tokenProgram", false, false, ""),
        ];
        if let Some(at) = extra {
            accounts.insert(at, account("extra", false, false, r#","isOptional":true"#));
        }
        let variable = |name: &str| {
            format!(
                r#"{{"kind":"variablePdaSeedNode","name":"{name}","type":{{"kind":"publicKeyTypeNode"}}}}"#
            )
        };
        Idl::from_json(&format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","additionalPrograms":[],
            "program":{{"kind":"programNode","name":"t","publicKey":"{VOLTR}","version":"0",
            "accounts":[],"errors":[],"definedTypes":[],
            "pdas":[{{"kind":"pdaNode","name":"ata","programId":"ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
            "seeds":[{},{},{}]}}],
            "instructions":[{{"kind":"instructionNode","name":"go","accounts":[{}],"arguments":[],
            "optionalAccountStrategy":"{strategy}",
            "discriminators":[{{"kind":"constantDiscriminatorNode","offset":0,"constant":{{
            "kind":"constantValueNode","type":{{"kind":"numberTypeNode","format":"u8","endian":"le"}},
            "value":{{"kind":"numberValueNode","number":1}}}}}}]}}]}}}}"#,
            variable("user"),
            variable("tokenProgram"),
            variable("mint"),
            accounts.join(",")
        ))
        .unwrap()
    }

    #[test]
    fn codama_pda_defaults_feed_the_seed_check() {
        let idl = codama("programId", None);
        assert!(verify(&idl, &sample(&[1], &SWAPPED), None)
            .findings
            .is_empty());
        let report = verify(&idl, &sample(&[1], &KEYS), None);
        assert_eq!(
            findings(&report),
            [(Severity::Error, "account #1 `ata` derives from its seeds in the IDL to the key passed at #2, not #1, in 1 of 1 sample")]
        );
    }

    #[test]
    fn a_left_out_optional_account_moves_the_seed_check() {
        // `extra` is left out, so each later key sits one position earlier.
        let idl = codama("omitted", Some(1));
        let note = (Severity::Note, "5 accounts passed where the IDL lists 6, in 1 of 1 sample; the instruction leaves out absent optional accounts: extra");
        let report = verify(&idl, &sample(&[1], &SWAPPED), None);
        assert_eq!(findings(&report), [note]);
        let report = verify(&idl, &sample(&[1], &KEYS), None);
        assert_eq!(
            findings(&report),
            [note, (Severity::Error, "account #2 `ata` derives from its seeds in the IDL to the key passed at #3, not #2, in 1 of 1 sample")]
        );
    }
}
