//! Instruction checks: classification, arguments, account count, fixed addresses and flags.

use std::collections::BTreeMap;

use idl_drift::model::{Account, Instruction, OptionalAccounts};

use super::{missing_field_hint, Groups, Outcomes, Verifier, LEGACY_DISCRIMINATORS};
use crate::classify::{flatten, IDL_IX_TAG};
use crate::report::{Check, Coverage, InstructionSummary, Range, Severity};
use crate::samples::KeyFlags;
use crate::text::{byte_range, of_samples, prefix_hex};

impl Verifier<'_> {
    pub(super) fn check_instructions(&mut self) -> (Coverage, Vec<InstructionSummary>) {
        let idl = self.idl;
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); idl.instructions.len()];
        let mut unknown = Groups::new();
        let mut idl_ix = Vec::new();
        for (i, sample) in self.samples.instructions.iter().enumerate() {
            match self.classifier.instruction(sample) {
                Some(t) => groups[t].push(i),
                None if self.is_anchor_builtin(&sample.data, &IDL_IX_TAG) => idl_ix.push(i),
                None => unknown
                    .entry(format!(
                        "{} matches no instruction discriminator in the IDL",
                        prefix_hex(&sample.data)
                    ))
                    .or_default()
                    .push(i),
            }
        }

        let nothing_to_match =
            self.classifier.instructions.is_empty() && !idl.instructions.is_empty();
        self.report_unclassified(
            "instruction",
            &unknown,
            nothing_to_match.then_some(LEGACY_DISCRIMINATORS),
            "the program has an instruction the IDL does not declare",
            |v, i| v.samples.instructions[i].signature.clone(),
        );
        self.report_anchor_builtin(
            "instruction",
            "IDL instruction",
            &IDL_IX_TAG,
            &idl_ix,
            |v, i| v.samples.instructions[i].signature.clone(),
        );

        let mut summaries = Vec::new();
        let mut not_sampled = Vec::new();
        for (ix, idxs) in idl.instructions.iter().zip(&groups) {
            if idxs.is_empty() {
                not_sampled.push(ix.name.clone());
            } else {
                summaries.push(self.check_instruction(ix, idxs));
            }
        }
        let coverage = Coverage {
            sampled: self.samples.instructions.len(),
            classified: groups.iter().map(Vec::len).sum(),
            not_sampled,
        };
        (coverage, summaries)
    }

    fn check_instruction(&mut self, ix: &Instruction, idxs: &[usize]) -> InstructionSummary {
        let subject = format!("instruction {}", ix.name);
        let disc = ix.payload_start();
        let flat = flatten(&ix.accounts);
        let samples = &self.samples.instructions;
        let omitted = ix.optional_accounts == OptionalAccounts::Omitted;
        let places: Places = idxs
            .iter()
            .filter(|&&i| omitted && samples[i].accounts.len() < flat.len())
            .map(|&i| (i, omitted_places(&flat, samples[i].accounts.len())))
            .collect();

        let outcomes: Outcomes = idxs
            .iter()
            .map(|&i| {
                let body = samples[i].data.get(disc..).unwrap_or_default();
                let result = self
                    .decoder
                    .borsh_fields(&ix.args, body)
                    .map(|d| (d.value, disc + d.len))
                    .map_err(|e| e.offset_by(disc));
                (i, result)
            })
            .collect();

        self.report_decode_failures(&subject, &outcomes, None, |v, i| {
            v.samples.instructions[i].signature.clone()
        });
        self.report_trailing_args(&subject, &outcomes);
        self.report_account_counts(&subject, &flat, idxs, omitted);
        self.report_fixed_addresses(&subject, &flat, idxs, &places);
        self.report_account_flags(&subject, &flat, idxs, &places);
        self.report_pda_addresses(&subject, &flat, &outcomes, &places);
        self.compare_instructions(&subject, ix, &outcomes);

        let zero = Range { min: 0, max: 0 };
        InstructionSummary {
            name: ix.name.clone(),
            samples: idxs.len(),
            idl_accounts: flat.len(),
            sample_accounts: Range::of(idxs.iter().map(|&i| samples[i].accounts.len()))
                .unwrap_or(zero),
            data_sizes: Range::of(idxs.iter().map(|&i| samples[i].data.len())).unwrap_or(zero),
        }
    }

    /// Instruction data has no allocation slack, so any byte after the arguments matters.
    fn report_trailing_args(&mut self, subject: &str, outcomes: &Outcomes) {
        let samples = &self.samples.instructions;
        let trailing: Vec<(usize, usize)> = outcomes
            .iter()
            .filter_map(|(i, r)| {
                let used = r.as_ref().ok()?.1;
                let extra = samples[*i].data.len().checked_sub(used)?;
                (extra > 0).then_some((*i, extra))
            })
            .collect();
        let (Some(&(first, _)), Some(extra)) = (
            trailing.first(),
            Range::of(trailing.iter().map(|(_, n)| *n)),
        ) else {
            return;
        };
        let example = samples[first].signature.clone();
        let hint =
            missing_field_hint(outcomes, |i| samples[i].data.as_slice()).unwrap_or_else(|| {
                "the IDL may be missing arguments, or declares one narrower than the program reads"
                    .into()
            });
        self.push(
            Severity::Error,
            Check::BytesUsed,
            subject,
            format!(
                "{} after the arguments in {}",
                byte_range(extra),
                of_samples(trailing.len(), outcomes.len())
            ),
            Some(hint),
            Some(&example),
        );
    }

    fn compare_instructions(&mut self, subject: &str, ix: &Instruction, outcomes: &Outcomes) {
        let Some(reference) = self.reference else {
            return;
        };
        let lone_arg = match ix.args.as_slice() {
            [arg] => Some(arg.name.as_str()),
            _ => None,
        };
        let (compared, matched) = self.compare_named(
            subject,
            &ix.name,
            lone_arg,
            outcomes,
            &reference.instructions,
            |i| self.samples.instructions[i].signature.clone(),
        );
        self.reference_summary.instructions_compared += compared;
        self.reference_summary.instructions_matched += matched;
    }

    fn report_account_counts(
        &mut self,
        subject: &str,
        flat: &[&Account],
        idxs: &[usize],
        omitted: bool,
    ) {
        let samples = &self.samples.instructions;
        let total = idxs.len();
        let optional: Vec<&str> = flat
            .iter()
            .filter(|a| a.optional)
            .map(|a| a.name.as_str())
            .collect();
        let mut short: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut extra = Vec::new();
        for &i in idxs {
            let n = samples[i].accounts.len();
            if n < flat.len() {
                short.entry(n).or_default().push(i);
            } else if n > flat.len() {
                extra.push(n - flat.len());
            }
        }
        for (n, group) in short {
            let example = samples[group[0]].signature.clone();
            let left_out = flat.len() - n;
            let tail_only = left_out == optional.len() && flat[n..].iter().all(|a| a.optional);
            if omitted && left_out <= optional.len() && !tail_only {
                let some = if left_out == optional.len() {
                    String::new()
                } else {
                    format!("{left_out} of ")
                };
                self.push(
                    Severity::Note,
                    Check::AccountCount,
                    subject,
                    format!(
                        "{n} accounts passed where the IDL lists {}, in {}; the instruction leaves out absent optional accounts: {some}{}",
                        flat.len(),
                        of_samples(group.len(), total),
                        optional.join(", ")
                    ),
                    Some("each later account moves one position earlier, so a generated parser that reads accounts by position may read them wrong; the other account checks skip an account whose position is not certain".into()),
                    Some(&example),
                );
                continue;
            }
            let missing = &flat[n..];
            let required = missing.iter().any(|a| !a.optional);
            let names: Vec<String> = missing
                .iter()
                .map(|a| match &a.address {
                    Some(addr) => format!("{} (fixed: {addr})", a.name),
                    None => a.name.clone(),
                })
                .collect();
            let fixed = missing.iter().all(|a| a.address.is_some());
            // Shipstern reads an absent optional tail account as `None`, so nothing breaks.
            let (severity, hint) = if !required {
                (Severity::Note, "every missing account is optional at the tail; a generated parser reads them as absent")
            } else if fixed {
                (Severity::Error, "every missing account has a fixed address, so the program likely stopped requiring them; a parser that checks the IDL's account count rejects these instructions")
            } else {
                (
                    Severity::Error,
                    "a parser that checks the IDL's account count rejects these instructions",
                )
            };
            self.push(
                severity,
                Check::AccountCount,
                subject,
                format!(
                    "{n} accounts passed where the IDL lists {}, in {}; missing: {}",
                    flat.len(),
                    of_samples(group.len(), total),
                    names.join(", ")
                ),
                Some(hint.to_string()),
                Some(&example),
            );
        }
        if let Some(most) = extra.iter().max() {
            self.push(
                Severity::Note,
                Check::AccountCount,
                subject,
                format!(
                    "up to {most} accounts passed beyond the {} the IDL lists (remaining accounts), in {}",
                    flat.len(),
                    of_samples(extra.len(), total)
                ),
                None,
                None,
            );
        }
    }

    /// An Anchor `address` is a constraint that the program checks. A Codama default only fills
    /// the slot when the caller gives no key, so another key there is not an error. The default
    /// key at another IDL position is a warning, because a free slot can also hold that key.
    fn report_fixed_addresses(
        &mut self,
        subject: &str,
        flat: &[&Account],
        idxs: &[usize],
        places: &Places,
    ) {
        let samples = &self.samples.instructions;
        let program_id = self.program_id().to_string();
        for (pos, account) in flat.iter().enumerate() {
            let Some(address) = &account.address else {
                continue;
            };
            // Another account with the same address can hold that key at its own position.
            let shared = flat
                .iter()
                .enumerate()
                .any(|(p, a)| p != pos && a.address.as_ref() == Some(address));
            let mut moved = Vec::new();
            let mut wrong = Vec::new();
            for &i in idxs {
                let keys = &samples[i].accounts;
                let Some(key) = place(places, i, pos).and_then(|at| keys.get(at)) else {
                    continue;
                };
                // Anchor passes the program id in place of an absent optional account.
                if key == address || (account.optional && *key == program_id) {
                    continue;
                }
                // Keys after the IDL's accounts are remaining accounts, which can be any key.
                match keys.iter().take(flat.len()).position(|k| k == address) {
                    Some(at) if account.address_is_default && !shared => moved.push((i, at)),
                    _ => wrong.push((i, key.as_str())),
                }
            }
            let mut found = Vec::new();
            if let Some(&(first, at)) = moved.first() {
                found.push((
                    Severity::Warning,
                    first,
                    format!(
                        "account #{pos} `{}` defaults to {address}, but that key is passed at #{at} instead, in {}",
                        account.name,
                        of_samples(moved.len(), idxs.len())
                    ),
                    "the IDL's account order likely does not match the program, or an earlier account is missing",
                ));
            }
            if let Some(&(first, key)) = wrong.first() {
                let (severity, verb, hint) = if account.address_is_default {
                    (Severity::Note, "defaults to", "a Codama default fills the account only when the caller gives no key, so the program can accept another key; a generated parser reads the key that is passed")
                } else {
                    (Severity::Error, "must be", "the IDL's account order does not match the program, or an earlier account is missing")
                };
                found.push((
                    severity,
                    first,
                    format!(
                        "account #{pos} `{}` {verb} {address}, but another key is passed in {} (such as {key})",
                        account.name,
                        of_samples(wrong.len(), idxs.len())
                    ),
                    hint,
                ));
            }
            for (severity, first, message, hint) in found {
                let example = samples[first].signature.clone();
                self.push(
                    severity,
                    Check::FixedAddress,
                    subject,
                    message,
                    Some(hint.into()),
                    Some(&example),
                );
            }
        }
    }

    /// A key's message flags are the union over all instructions, so only a missing privilege
    /// is evidence. A CPI can sign for a PDA, so only top-level samples count for `signer`.
    fn report_account_flags(
        &mut self,
        subject: &str,
        flat: &[&Account],
        idxs: &[usize],
        places: &Places,
    ) {
        let samples = &self.samples.instructions;
        let program_id = self.program_id().to_string();
        let flagged: Vec<(usize, &KeyFlags)> = idxs
            .iter()
            .filter_map(|&i| Some((i, samples[i].flags.as_ref()?)))
            .collect();
        let top_level = flagged.iter().filter(|(_, f)| !f.cpi).count();
        let mut found = Vec::new();
        for (pos, account) in flat.iter().enumerate() {
            let mut read_only = Vec::new();
            let mut unsigned = Vec::new();
            for &(i, flags) in &flagged {
                let Some(at) = place(places, i, pos) else {
                    continue;
                };
                let Some(key) = samples[i].accounts.get(at) else {
                    continue;
                };
                // Anchor passes the program id in place of an absent optional account.
                if account.optional && *key == program_id {
                    continue;
                }
                if account.writable && flags.writable.get(at) == Some(&false) {
                    read_only.push((i, key.as_str()));
                }
                let must_sign = account.signer && !account.may_sign && !flags.cpi;
                if must_sign && flags.signer.get(at) == Some(&false) {
                    unsigned.push((i, key.as_str()));
                }
            }
            if let Some(&(first, key)) = read_only.first() {
                found.push((
                    first,
                    format!(
                        "account #{pos} `{}` is writable in the IDL, but the key passed there is read-only in {} (such as {key})",
                        account.name,
                        of_samples(read_only.len(), flagged.len())
                    ),
                ));
            }
            if let Some(&(first, key)) = unsigned.first() {
                found.push((
                    first,
                    format!(
                        "account #{pos} `{}` is a signer in the IDL, but the key passed there does not sign the transaction in {} (top-level calls only; such as {key})",
                        account.name,
                        of_samples(unsigned.len(), top_level)
                    ),
                ));
            }
        }
        for (first, message) in found {
            let example = samples[first].signature.clone();
            self.push(
                Severity::Error,
                Check::AccountFlags,
                subject,
                message,
                Some("the IDL's account order likely does not match the program, or an earlier account is missing".into()),
                Some(&example),
            );
        }
    }
}

/// Where each IDL account sits among a sample's keys, by sample index, for samples that leave
/// out absent optional accounts. Other samples pass each account at its own position.
pub(super) type Places = BTreeMap<usize, Vec<Option<usize>>>;

/// The position in sample `i` of the IDL account at `pos`.
fn place(places: &Places, i: usize, pos: usize) -> Option<usize> {
    places
        .get(&i)
        .map_or(Some(pos), |at| at.get(pos).copied().flatten())
}

/// Where each IDL account sits among `passed` keys, fewer than the IDL lists, when absent
/// optional accounts are left out. `None` for an account that is absent or not certain.
fn omitted_places(flat: &[&Account], passed: usize) -> Vec<Option<usize>> {
    let optional = flat.iter().filter(|a| a.optional).count();
    let left_out = flat.len().saturating_sub(passed);
    if left_out > optional {
        // A required account is missing too, and it can be at any position.
        return vec![None; flat.len()];
    }
    let mut before = 0;
    flat.iter()
        .enumerate()
        .map(|(pos, account)| {
            if account.optional {
                before += 1;
                return None;
            }
            // Left out before `pos`: at least those that cannot all be after it, at most the
            // optional accounts before it. The position is certain only when the two agree.
            let least = left_out.saturating_sub(optional - before);
            (least == left_out.min(before)).then(|| pos - least)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use idl_drift::model::{AccountItem, Idl};

    use crate::classify::{IDL_ACCOUNT_DISCRIMINATOR, IDL_IX_TAG};
    use crate::report::{Check, Report, Severity};
    use crate::samples::{AccountSample, InstructionSample, KeyFlags, Samples};
    use crate::testing::{idl, only};
    use crate::verify;

    /// An IDL with only the given instructions.
    fn ix_idl(instructions: &str) -> Idl {
        idl(&format!(
            r#""accounts":[],"instructions":{instructions},"types":[]"#
        ))
    }

    fn instructions(list: &[(&[u8], &[&str])]) -> Samples {
        Samples {
            instructions: list
                .iter()
                .map(|(data, keys)| InstructionSample {
                    signature: "sig".into(),
                    data: data.to_vec(),
                    accounts: keys.iter().map(ToString::to_string).collect(),
                    flags: None,
                })
                .collect(),
            ..Samples::default()
        }
    }

    #[test]
    fn constant_trailing_bytes_suggest_a_missing_field() {
        let idl = ix_idl(r#"[{"name":"go","discriminator":[1]}]"#);
        let some_u64: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        let report = verify(
            &idl,
            &instructions(&[(some_u64, &[]), (some_u64, &[])]),
            None,
        );
        assert_eq!(
            only(&report).hint.as_deref(),
            Some("the same 9 bytes follow in every sample: the IDL is likely missing a 9-byte field at the end; it fits an Option of a u64")
        );
        // Lengths that vary keep the general hint.
        let report = verify(
            &idl,
            &instructions(&[(&[1, 5], &[]), (&[1, 5, 5], &[])]),
            None,
        );
        assert_eq!(
            only(&report).hint.as_deref(),
            Some(
                "the IDL may be missing arguments, or declares one narrower than the program reads"
            )
        );
    }

    #[test]
    fn shared_discriminator_dispatches_by_account_count() {
        let idl = ix_idl(
            r#"[{"name":"one","discriminator":[1],"accounts":[{"name":"a"}]},
                {"name":"two","discriminator":[1],"accounts":[{"name":"a"},{"name":"b"}],
                 "args":[{"name":"x","type":"u8"}]}]"#,
        );
        let report = verify(&idl, &instructions(&[(&[1, 9], &["k1", "k2"])]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.instructions[0].name, "two");
    }

    #[test]
    fn anchor_idl_instruction_and_account_are_a_note_not_an_error() {
        // Anchor adds both to each program built without `no-idl`, and its IDL lists neither.
        let data = [&IDL_IX_TAG[..], &[2]].concat();
        let mut samples = instructions(&[(&data, &["Idl", "Auth"])]);
        samples.accounts.push(AccountSample {
            address: "Idl".into(),
            data: [&IDL_ACCOUNT_DISCRIMINATOR[..], &[0; 36]].concat(),
        });
        let report = verify(&ix_idl("[]"), &samples, None);
        let found: Vec<(Severity, Check, &str)> = report
            .findings
            .iter()
            .map(|f| (f.severity, f.check, f.message.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    Severity::Note,
                    Check::Discriminator,
                    "1 account is Anchor's IdlAccount (0x184662bf3a907b9e); not checked"
                ),
                (
                    Severity::Note,
                    Check::Discriminator,
                    "1 instruction is Anchor's IDL instruction (0x40f4bc78a7e9690a); not checked"
                ),
            ]
        );
        assert_eq!(
            (
                report.account_coverage.classified,
                report.instruction_coverage.classified
            ),
            (0, 0)
        );
    }

    /// Token-2022's transfer-fee instructions: a `u8` tag, then a `u8` sub-tag.
    fn token_2022() -> Idl {
        Idl::from_json(include_str!(
            "../../../../fixtures/codama/token_2022_transfer_fee.json"
        ))
        .unwrap()
    }

    #[test]
    fn a_sample_matches_only_if_every_discriminator_matches() {
        // initializeTransferFeeConfig is [26, 0] and also takes one account.
        let samples = instructions(&[(&[26, 4], &["Mint"])]);
        let report = verify(&token_2022(), &samples, None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.instructions[0].name, "harvestWithheldTokensToMint");
    }

    #[test]
    fn args_start_after_every_discriminator() {
        // transferCheckedWithFee: [26, 1], amount u64, decimals u8, fee u64.
        let mut data = vec![26, 1];
        data.extend(5u64.to_le_bytes());
        data.push(6);
        data.extend(7u64.to_le_bytes());
        let samples = instructions(&[(&data, &["Src", "Mint", "Dst", "Auth"])]);
        let report = verify(&token_2022(), &samples, None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    #[test]
    fn optional_tail_accounts_left_out_are_a_note_not_a_warning() {
        // SPL Governance castVote passes 11 of 13 accounts live; Shipstern parses it.
        let lenient = ix_idl(
            r#"[{"name":"vote","discriminator":[1],"accounts":[
                {"name":"realm"},{"name":"voterWeightRecord","optional":true}]}]"#,
        );
        let report = verify(&lenient, &instructions(&[(&[1], &["R"])]), None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Note, Check::AccountCount));
        // A required account left out is still an error.
        let strict = ix_idl(
            r#"[{"name":"vote","discriminator":[1],"accounts":[{"name":"realm"},{"name":"proposal"}]}]"#,
        );
        let report = verify(&strict, &instructions(&[(&[1], &["R"])]), None);
        assert_eq!(only(&report).severity, Severity::Error);
    }

    #[test]
    fn absent_optional_account_may_be_the_program_id() {
        let idl = ix_idl(
            r#"[{"name":"go","discriminator":[1],"accounts":[
                {"name":"rent","optional":true,"address":"SysvarRent111111111111111111111111111111111"}]}]"#,
        );
        let report = verify(&idl, &instructions(&[(&[1], &["P"])]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    fn go(accounts: &str) -> Idl {
        ix_idl(&format!(
            r#"[{{"name":"go","discriminator":[1],"accounts":{accounts}}}]"#
        ))
    }

    /// One `go` sample: its keys, `(signer, writable)` per key, and whether it is a CPI.
    fn flagged(keys: &[&str], flags: &[(bool, bool)], cpi: bool) -> Samples {
        let mut samples = instructions(&[(&[1], keys)]);
        samples.instructions[0].flags = Some(KeyFlags {
            signer: flags.iter().map(|f| f.0).collect(),
            writable: flags.iter().map(|f| f.1).collect(),
            cpi,
        });
        samples
    }

    fn flag_findings(report: &Report) -> Vec<String> {
        report
            .findings
            .iter()
            .filter(|f| f.check == Check::AccountFlags)
            .map(|f| f.message.clone())
            .collect()
    }

    /// The program takes `vault` (writable), then `authority` (signer).
    const SWAPPED: &str =
        r#"[{"name":"authority","signer":true},{"name":"vault","writable":true}]"#;
    const VAULT_THEN_AUTH: [(bool, bool); 2] = [(false, true), (true, false)];

    #[test]
    fn swapped_accounts_lack_the_privileges_the_idl_gives_them() {
        let samples = flagged(&["Vault", "Auth"], &VAULT_THEN_AUTH, false);
        let found = flag_findings(&verify(&go(SWAPPED), &samples, None));
        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(
            found[0].contains("#0 `authority` is a signer"),
            "{found:#?}"
        );
        assert!(found[1].contains("#1 `vault` is writable"), "{found:#?}");
        // A CPI can sign for a PDA, so only the writable check holds there.
        let samples = flagged(&["Vault", "Auth"], &VAULT_THEN_AUTH, true);
        let found = flag_findings(&verify(&go(SWAPPED), &samples, None));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].contains("`vault` is writable"), "{found:#?}");
    }

    #[test]
    fn a_dropped_account_shifts_a_read_only_key_into_a_writable_slot() {
        // The program takes `authority`, `mint`, `vault`; the IDL lost `mint`.
        let idl = go(r#"[{"name":"authority","signer":true},{"name":"vault","writable":true}]"#);
        let samples = flagged(
            &["Auth", "Mint", "Vault"],
            &[(true, false), (false, false), (false, true)],
            false,
        );
        let found = flag_findings(&verify(&idl, &samples, None));
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].contains("#1 `vault` is writable in the IDL, but the key passed there is read-only in 1 of 1 sample (such as Mint)"),
            "{found:#?}"
        );
    }

    #[test]
    fn extra_privileges_absent_optionals_and_either_signers_are_not_errors() {
        let mut idl = go(
            r#"[{"name":"authority","signer":true},{"name":"vault","writable":true},
            {"name":"mint"},{"name":"payer","signer":true},{"name":"rent","optional":true,"writable":true}]"#,
        );
        // Codama `isSigner: "either"`.
        if let AccountItem::Single(payer) = &mut idl.instructions[0].accounts[3] {
            payer.may_sign = true;
        }
        let samples = flagged(
            &["Auth", "Vault", "Mint", "Payer", "P"],
            &[
                (true, true),
                (false, true),
                (false, true),
                (false, false),
                (false, false),
            ],
            false,
        );
        let report = verify(&idl, &samples, None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    #[test]
    fn samples_without_flags_skip_the_flag_check() {
        let samples = Samples::from_json(
            r#"{"instructions":[{"signature":"S","data":"AQ==","accounts":["Vault","Auth"]}]}"#,
        )
        .unwrap();
        let report = verify(&go(SWAPPED), &samples, None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    /// A Codama instruction account; `more` adds fields such as `"isOptional":true`.
    fn codama_account(name: &str, writable: bool, more: &str) -> String {
        format!(
            r#"{{"kind":"instructionAccountNode","name":"{name}","isWritable":{writable},
            "isSigner":false{more}}}"#
        )
    }

    fn default_key(key: &str) -> String {
        format!(r#","defaultValue":{{"kind":"publicKeyValueNode","publicKey":"{key}"}}"#)
    }

    /// A Codama IDL of program `P` with one instruction `go`, discriminator `[1]`.
    fn codama_go(strategy: &str, accounts: &[String]) -> Idl {
        Idl::from_json(&format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","program":{{
            "kind":"programNode","name":"t","publicKey":"P","version":"0","accounts":[],
            "errors":[],"definedTypes":[],"pdas":[],"instructions":[{{"kind":"instructionNode",
            "name":"go","optionalAccountStrategy":"{strategy}","accounts":[{}],"arguments":[],
            "discriminators":[{{"kind":"constantDiscriminatorNode","offset":0,"constant":{{
            "kind":"constantValueNode","type":{{"kind":"numberTypeNode","format":"u8","endian":"le"}},
            "value":{{"kind":"numberValueNode","number":1}}}}}}]}}]}}}}"#,
            accounts.join(",")
        ))
        .unwrap()
    }

    #[test]
    fn a_codama_size_discriminator_must_match_too() {
        // A native program can take one tag at two data lengths, when callers pad the struct.
        // The Codama spec lets the size discriminator come first or last.
        for size_first in [false, true] {
            let ix = |name: &str, size: usize, extra: &str| {
                let field =
                    r#"{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}"#;
                let sized = format!(r#"{{"kind":"sizeDiscriminatorNode","size":{size}}}"#);
                let discs = if size_first {
                    format!("{sized},{field}")
                } else {
                    format!("{field},{sized}")
                };
                format!(
                    r#"{{"kind":"instructionNode","name":"{name}","accounts":[],"arguments":[
                    {{"kind":"instructionArgumentNode","name":"discriminator","type":{{"kind":"numberTypeNode","format":"u8","endian":"le"}},
                    "defaultValue":{{"kind":"numberValueNode","number":7}},"defaultValueStrategy":"omitted"}},
                    {{"kind":"instructionArgumentNode","name":"amount","type":{{"kind":"numberTypeNode","format":"u64","endian":"le"}}}}{extra}],
                    "discriminators":[{discs}]}}"#
                )
            };
            let pad = r#",{"kind":"instructionArgumentNode","name":"pad","type":{"kind":"fixedSizeTypeNode","size":7,"type":{"kind":"bytesTypeNode"}}}"#;
            let idl = Idl::from_json(&format!(
                r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","program":{{
                "kind":"programNode","name":"t","publicKey":"P","version":"0","accounts":[],
                "errors":[],"definedTypes":[],"pdas":[],"instructions":[{},{}]}}}}"#,
                ix("short", 9, ""),
                ix("padded", 16, pad)
            ))
            .unwrap();
            let mut short = vec![7];
            short.extend(5u64.to_le_bytes());
            let mut padded = short.clone();
            padded.extend([0; 7]);
            let report = verify(&idl, &instructions(&[(&short, &[]), (&padded, &[])]), None);
            assert!(
                report.findings.is_empty(),
                "{size_first}: {:#?}",
                report.findings
            );
            // The tag matches, but no instruction has this size.
            let report = verify(&idl, &instructions(&[(&short[..6], &[])]), None);
            assert_eq!(
                (only(&report).severity, only(&report).check),
                (Severity::Error, Check::Discriminator)
            );
        }
    }

    #[test]
    fn a_size_discriminator_alone_selects_an_instruction() {
        let ix = |name: &str, size: usize, format: &str| {
            format!(
                r#"{{"kind":"instructionNode","name":"{name}","accounts":[],"arguments":[
                {{"kind":"instructionArgumentNode","name":"v","type":{{"kind":"numberTypeNode","format":"{format}","endian":"le"}}}}],
                "discriminators":[{{"kind":"sizeDiscriminatorNode","size":{size}}}]}}"#
            )
        };
        let idl = Idl::from_json(&format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","program":{{
            "kind":"programNode","name":"t","publicKey":"P","version":"0","accounts":[],
            "errors":[],"definedTypes":[],"pdas":[],"instructions":[{},{}]}}}}"#,
            ix("big", 8, "u64"),
            ix("small", 1, "u8")
        ))
        .unwrap();
        let report = verify(&idl, &instructions(&[(&[5; 8], &[]), (&[5], &[])]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.instruction_coverage.classified, 2);
    }

    #[test]
    fn a_codama_default_key_is_not_a_constraint() {
        // Token-2022 `instructionsSysvarOrContextState` defaults to this sysvar, but the
        // program also takes a context-state account there.
        const SYSVAR: &str = "Sysvar1nstructions1111111111111111111111111";
        let found = |idl: &Idl, keys: &[&str]| -> Vec<(Severity, Check)> {
            verify(idl, &instructions(&[(&[1], keys)]), None)
                .findings
                .iter()
                .map(|f| (f.severity, f.check))
                .collect()
        };
        let codama = codama_go(
            "programId",
            &[
                codama_account("a", false, ""),
                codama_account("sysvar", false, &default_key(SYSVAR)),
                codama_account("b", false, ""),
            ],
        );
        assert_eq!(
            found(&codama, &["A", "Context", "B"]),
            [(Severity::Note, Check::FixedAddress)]
        );
        // The default key one position late means the accounts likely moved.
        let report = verify(&codama, &instructions(&[(&[1], &["A", "B", SYSVAR])]), None);
        assert_eq!(
            (only(&report).severity, only(&report).message.as_str()),
            (Severity::Warning, "account #1 `sysvar` defaults to Sysvar1nstructions1111111111111111111111111, but that key is passed at #2 instead, in 1 of 1 sample")
        );
        // A remaining account can be any key.
        assert_eq!(
            found(&codama, &["A", "Context", "B", SYSVAR]),
            [
                (Severity::Note, Check::AccountCount),
                (Severity::Note, Check::FixedAddress)
            ]
        );
        // An Anchor `address` is a constraint.
        let anchor = go(&format!(
            r#"[{{"name":"a"}},{{"name":"sysvar","address":"{SYSVAR}"}},{{"name":"b"}}]"#
        ));
        assert_eq!(
            found(&anchor, &["A", "Context", "B"]),
            [(Severity::Error, Check::FixedAddress)]
        );
    }

    #[test]
    fn left_out_optional_accounts_shift_later_accounts() {
        const RENT: &str = "SysvarRent111111111111111111111111111111111";
        let optional = r#","isOptional":true"#;
        let idl = codama_go(
            "omitted",
            &[
                codama_account("a", false, ""),
                codama_account("o1", false, optional),
                codama_account("b", true, ""),
                codama_account("o2", false, optional),
                codama_account("c", false, &default_key(RENT)),
            ],
        );
        // Either `o1` or `o2` is left out, so `b` can be at #1 or #2. `c` is at #3.
        let mut samples = instructions(&[
            (&[1], &["A", "B", "O2", RENT]),
            (&[1], &["A", "O1", "B", RENT]),
            (&[1], &["A", "B", "O2", "X"]),
        ]);
        for sample in &mut samples.instructions {
            let writable = sample.accounts.iter().map(|k| k == "B").collect();
            sample.flags = Some(KeyFlags {
                signer: vec![false; 4],
                writable,
                cpi: false,
            });
        }
        let report = verify(&idl, &samples, None);
        let found: Vec<(Severity, Check, &str)> = report
            .findings
            .iter()
            .map(|f| (f.severity, f.check, f.message.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (Severity::Note, Check::AccountCount, "4 accounts passed where the IDL lists 5, in 3 of 3 samples; the instruction leaves out absent optional accounts: 1 of o1, o2"),
                (Severity::Note, Check::FixedAddress, "account #4 `c` defaults to SysvarRent111111111111111111111111111111111, but another key is passed in 1 of 3 samples (such as X)"),
            ]
        );
    }
}
