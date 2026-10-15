//! Instruction checks: classification, arguments, account count, fixed addresses and flags.

use std::collections::BTreeMap;

use idl_drift::model::{Account, Instruction};

use super::{missing_field_hint, sample_count, Groups, Outcomes, Verifier, LEGACY_DISCRIMINATORS};
use crate::classify::flatten;
use crate::report::{Check, Coverage, InstructionSummary, Range, Severity};
use crate::samples::KeyFlags;
use crate::text::{byte_range, of_samples, prefix_hex};

impl Verifier<'_> {
    pub(super) fn check_instructions(&mut self) -> (Coverage, Vec<InstructionSummary>) {
        let idl = self.idl;
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); idl.instructions.len()];
        let mut unknown = Groups::new();
        for (i, sample) in self.samples.instructions.iter().enumerate() {
            match self.classifier.instruction(sample) {
                Some(t) => groups[t].push(i),
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
            classified: self.samples.instructions.len() - sample_count(&unknown),
            not_sampled,
        };
        (coverage, summaries)
    }

    fn check_instruction(&mut self, ix: &Instruction, idxs: &[usize]) -> InstructionSummary {
        let subject = format!("instruction {}", ix.name);
        let disc = ix
            .discriminator_offset
            .saturating_add(ix.discriminator.len());
        let flat = flatten(&ix.accounts);
        let samples = &self.samples.instructions;

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
        self.report_account_counts(&subject, &flat, idxs);
        self.report_fixed_addresses(&subject, &flat, idxs);
        self.report_account_flags(&subject, &flat, idxs);
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

    fn report_account_counts(&mut self, subject: &str, flat: &[&Account], idxs: &[usize]) {
        let samples = &self.samples.instructions;
        let total = idxs.len();
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
            let example = samples[group[0]].signature.clone();
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

    fn report_fixed_addresses(&mut self, subject: &str, flat: &[&Account], idxs: &[usize]) {
        let samples = &self.samples.instructions;
        let program_id = self.program_id().to_string();
        for (pos, account) in flat.iter().enumerate() {
            let Some(address) = &account.address else {
                continue;
            };
            let wrong: Vec<(usize, &str)> = idxs
                .iter()
                .filter_map(|&i| {
                    let key = samples[i].accounts.get(pos)?;
                    // Anchor passes the program id in place of an absent optional account.
                    let absent = account.optional && *key == program_id;
                    (key != address && !absent).then_some((i, key.as_str()))
                })
                .collect();
            if let Some(&(first, key)) = wrong.first() {
                let example = samples[first].signature.clone();
                self.push(
                    Severity::Error,
                    Check::FixedAddress,
                    subject,
                    format!(
                        "account #{pos} `{}` must be {address}, but another key is passed in {} (such as {key})",
                        account.name,
                        of_samples(wrong.len(), idxs.len())
                    ),
                    Some("the IDL's account order does not match the program, or an earlier account is missing".into()),
                    Some(&example),
                );
            }
        }
    }

    /// A key's message flags are the union over all instructions, so only a missing privilege
    /// is evidence. A CPI can sign for a PDA, so only top-level samples count for `signer`.
    fn report_account_flags(&mut self, subject: &str, flat: &[&Account], idxs: &[usize]) {
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
                let Some(key) = samples[i].accounts.get(pos) else {
                    continue;
                };
                // Anchor passes the program id in place of an absent optional account.
                if account.optional && *key == program_id {
                    continue;
                }
                if account.writable && flags.writable.get(pos) == Some(&false) {
                    read_only.push((i, key.as_str()));
                }
                let must_sign = account.signer && !account.may_sign && !flags.cpi;
                if must_sign && flags.signer.get(pos) == Some(&false) {
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

#[cfg(test)]
mod tests {
    use idl_drift::model::{AccountItem, Idl};

    use crate::report::{Check, Report, Severity};
    use crate::samples::{InstructionSample, KeyFlags, Samples};
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
}
