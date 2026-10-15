//! Account checks: classification, decoding, unused bytes and zero-copy layout.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use idl_drift::model::{Idl, NamedDisc, Type, TypeDef};

use super::{
    bytes_used, missing_field_hint, sample_count, size_counts, Groups, Outcomes, Verifier,
};
use crate::decode::Value;
use crate::layout::StructLayout;
use crate::reference::Outcome;
use crate::report::{AccountSummary, Check, Coverage, Range, Severity};
use crate::text::{bytes, of_samples, plural, prefix_hex};

enum AccountLayout {
    Borsh,
    /// Cast from the account bytes at `repr(C)` offsets.
    ZeroCopy(StructLayout),
}

impl Verifier<'_> {
    pub(super) fn check_accounts(&mut self) -> (Coverage, Vec<AccountSummary>) {
        let idl = self.idl;
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); idl.accounts.len()];
        let by_size = self.classifier.accounts.is_empty() && !self.classifier.sized.is_empty();
        let mut unknown = Groups::new();
        let mut empty = Vec::new();
        for (i, sample) in self.samples.accounts.iter().enumerate() {
            if let Some(t) = self.classifier.account(&sample.data) {
                groups[t].push(i);
                continue;
            }
            let len = sample.data.len();
            // A single fit classifies the sample above, so here there are none or several.
            let key = match self.classifier.fits(len).as_slice() {
                [] if len == 0 => {
                    empty.push(i);
                    continue;
                }
                [] if by_size => format!("{len}-byte data matches no account type size"),
                [] => format!(
                    "{} matches no account discriminator in the IDL",
                    prefix_hex(&sample.data)
                ),
                many => {
                    let names: Vec<&str> = many
                        .iter()
                        .map(|t| idl.accounts[*t].name.as_str())
                        .collect();
                    format!("{len}-byte data fits {} alike", names.join(" and "))
                }
            };
            unknown.entry(key).or_default().push(i);
        }

        let nothing_to_match = self.classifier.accounts.is_empty()
            && self.classifier.sized.is_empty()
            && !idl.accounts.is_empty();
        let hint = unknown_account_hint(idl, &self.classifier.sized, by_size);
        self.report_unclassified(
            "account",
            &unknown,
            nothing_to_match.then_some("add the discriminators or the account sizes to the IDL; idl-verify computes discriminators only for legacy (pre-0.30) Anchor IDLs"),
            &hint,
            |v, i| v.samples.accounts[i].address.clone(),
        );
        self.report_empty_accounts(&empty);

        let mut summaries = Vec::new();
        let mut not_sampled = Vec::new();
        for (nd, idxs) in idl.accounts.iter().zip(&groups) {
            if idxs.is_empty() {
                not_sampled.push(nd.name.clone());
            } else {
                summaries.push(self.check_account_type(nd, idxs));
            }
        }
        let coverage = Coverage {
            sampled: self.samples.accounts.len(),
            classified: self.samples.accounts.len() - sample_count(&unknown) - empty.len(),
            not_sampled,
        };
        (coverage, summaries)
    }

    /// An account with no data is not an IDL fault, so it is a note.
    fn report_empty_accounts(&mut self, empty: &[usize]) {
        let Some(&first) = empty.first() else {
            return;
        };
        let example = self.samples.accounts[first].address.clone();
        self.push(
            Severity::Note,
            Check::Discriminator,
            "accounts",
            format!(
                "{} {} no data; not checked",
                empty.len(),
                plural(empty.len(), "account holds", "accounts hold")
            ),
            Some("an account with no data has no discriminator, so no IDL can describe it; a generated parser such as Shipstern's reports it as an unknown account".into()),
            Some(&example),
        );
    }

    fn check_account_type(&mut self, nd: &NamedDisc, idxs: &[usize]) -> AccountSummary {
        let subject = format!("account {}", nd.name);
        let disc = nd.payload_start();
        let accounts = &self.samples.accounts;
        let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
        for &i in idxs {
            *sizes.entry(accounts[i].data.len()).or_default() += 1;
        }
        let mut summary = AccountSummary {
            name: nd.name.clone(),
            samples: idxs.len(),
            layout: "unknown".to_string(),
            sizes: size_counts(&sizes),
            bytes_used: None,
            expected_size: None,
        };

        let Some(td) = self.decoder.types().get(&nd.name) else {
            self.push(
                Severity::Error,
                Check::MissingType,
                &subject,
                "has no entry in the IDL's `types`, so its data cannot be decoded".into(),
                None,
                None,
            );
            return summary;
        };
        summary.layout = if td.is_zero_copy() {
            let repr = td
                .repr
                .map_or_else(|| "repr(C)".to_string(), |r| r.to_string());
            format!("zero-copy {repr}")
        } else {
            "borsh".to_string()
        };
        let Some(layout) = self.account_layout(&subject, td) else {
            return summary;
        };
        let zero_copy = match &layout {
            AccountLayout::ZeroCopy(l) => Some(l),
            AccountLayout::Borsh => None,
        };

        let expected = match &zero_copy {
            Some(layout) => disc.checked_add(layout.size),
            None => self
                .decoder
                .types()
                .borsh_size(&Type::Defined {
                    name: nd.name.clone(),
                    generics: Vec::new(),
                })
                .ok()
                .flatten()
                .and_then(|n| n.checked_add(disc)),
        };
        summary.expected_size = expected;

        let outcomes: Outcomes = idxs
            .iter()
            .map(|&i| {
                let body = accounts[i].data.get(disc..).unwrap_or_default();
                let result = self
                    .decoder
                    .type_def(td, body)
                    .map(|d| (d.value, disc + d.len))
                    .map_err(|e| e.offset_by(disc));
                (i, result)
            })
            .collect();
        summary.bytes_used = bytes_used(&outcomes);

        let hint = self.short_accounts_hint(&outcomes, expected);
        self.report_decode_failures(&subject, &outcomes, hint, |v, i| {
            v.samples.accounts[i].address.clone()
        });
        if expected.is_some() {
            self.report_fixed_leftover(&subject, nd, td, zero_copy, &outcomes);
            self.report_size_distribution(&subject, &sizes);
        } else {
            self.report_variable_leftover(&subject, &outcomes);
        }

        let padded = zero_copy.filter(|l| l.padding_len() > 0);
        self.compare_accounts(&subject, &outcomes, padded);
        summary
    }

    /// How `td` is laid out in account data; `None` (after a warning) when the zero-copy
    /// layout cannot be computed.
    fn account_layout(&mut self, subject: &str, td: &TypeDef) -> Option<AccountLayout> {
        if !td.is_zero_copy() {
            return Some(AccountLayout::Borsh);
        }
        let layout = match self.decoder.types().struct_layout(td) {
            Ok(layout) => layout,
            Err(e) => {
                self.push(
                    Severity::Warning,
                    Check::Layout,
                    subject,
                    format!("cannot verify: the zero-copy layout is unknown ({e})"),
                    Some("only zero-copy structs whose layout the IDL pins down can be checked: repr(C), no options, vecs or enums".into()),
                    None,
                );
                return None;
            }
        };
        // Pod refuses implicit padding at compile time, so a gap means the IDL is wrong.
        if td.serialization == "bytemuck" && layout.padding_len() > 0 {
            self.push(
                Severity::Error,
                Check::Layout,
                subject,
                format!(
                    "the IDL's repr(C) layout has {} of implicit padding ({}), but a `bytemuck` (Pod) type cannot have any",
                    bytes(layout.padding_len()),
                    padding_list(&layout)
                ),
                Some("the IDL's `repr` or field list is wrong; a missing `packed: true` is the usual cause".into()),
                None,
            );
        }
        Some(AccountLayout::ZeroCopy(layout))
    }

    /// A hint for decode failures when every failing account is shorter than the layout.
    fn short_accounts_hint(&self, outcomes: &Outcomes, expected: Option<usize>) -> Option<String> {
        let expected = expected?;
        let failing: Vec<usize> = outcomes
            .iter()
            .filter(|(_, r)| r.is_err())
            .map(|(i, _)| self.samples.accounts[*i].data.len())
            .collect();
        if failing.is_empty() || failing.iter().any(|len| *len >= expected) {
            return None;
        }
        let sizes = Range::of(failing).map_or_else(String::new, |r| r.to_string());
        Some(format!(
            "the IDL layout needs {expected} bytes and these accounts are {sizes} bytes: they may predate a program upgrade, or the IDL is out of date"
        ))
    }

    fn report_size_distribution(&mut self, subject: &str, sizes: &BTreeMap<usize, usize>) {
        if sizes.len() < 2 {
            return;
        }
        let list: Vec<String> = sizes.iter().map(|(s, c)| format!("{s} ({c})")).collect();
        self.push(
            Severity::Note,
            Check::SizeDistribution,
            subject,
            format!(
                "this fixed-size type comes in {} sizes: {}",
                sizes.len(),
                list.join(", ")
            ),
            Some(
                "the odd sizes may be accounts created before a program upgrade, or reallocated"
                    .into(),
            ),
            None,
        );
    }

    /// Unused bytes on a fixed-size type: padding the IDL dropped, spare allocation, or a
    /// missing field.
    fn report_fixed_leftover(
        &mut self,
        subject: &str,
        nd: &NamedDisc,
        td: &TypeDef,
        zero_copy: Option<&StructLayout>,
        outcomes: &Outcomes,
    ) {
        let accounts = &self.samples.accounts;
        let disc = nd.payload_start();
        let mut classes: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (i, result) in outcomes {
            if let Ok((_, used)) = result {
                let size = accounts[*i].data.len();
                if size > *used {
                    classes.entry((size, *used)).or_default().push(*i);
                }
            }
        }
        let total = outcomes.len();
        let missing_field = missing_field_hint(outcomes, |i| accounts[i].data.as_slice());
        for ((size, used), idxs) in classes {
            let unused = size - used;
            let count = of_samples(idxs.len(), total);
            let example = accounts[idxs[0]].address.clone();
            // A Borsh-labeled struct whose padded repr(C) size is exactly the account size.
            let padded = zero_copy
                .is_none()
                .then(|| self.decoder.types().struct_layout(td).ok())
                .flatten()
                .filter(|l| l.padding_len() > 0 && disc.checked_add(l.size) == Some(size));
            // Anchor's `space` often reserves more than a Borsh type needs. A zero-copy
            // layout sets the size, so its unused bytes stay an error.
            let zeros = zero_copy.is_none()
                && idxs.iter().all(|&i| {
                    accounts[i]
                        .data
                        .get(used..)
                        .is_some_and(|rest| rest.iter().all(|b| *b == 0))
                });
            let (severity, message, hint) = if let Some(layout) = padded {
                let shifted = layout
                    .padding
                    .iter()
                    .find_map(|p| p.before.as_deref())
                    .map_or_else(String::new, |f| {
                        format!(" A decoder built from it reads `{f}` and every later field at the wrong offset.")
                    });
                (
                    Severity::Error,
                    format!("{unused} of {size} bytes unused in {count}"),
                    format!(
                        "{size} bytes is exactly the repr(C) layout ({disc} + {}), which pads {}. The IDL lays this type out as Borsh, without the padding.{shifted} Keep the Anchor IDL's `serialization` and `repr`, or add the padding as explicit fields.",
                        layout.size,
                        padding_list(&layout)
                    ),
                )
            } else if Some(size) == round_up_8(used) {
                (
                    Severity::Warning,
                    format!("{} after the data in {count}", bytes(unused)),
                    "the account size is the data size rounded up to 8 bytes, which suggests spare allocation; confirm against the program source".to_string(),
                )
            } else if zeros {
                (
                    Severity::Warning,
                    format!("{unused} of {size} bytes unused in {count}, all zero"),
                    "zero bytes after the data suggest spare allocation (Anchor's `space` can be larger than the type); a missing field that is zero in every sample looks the same, so confirm against the program source".to_string(),
                )
            } else {
                let mut hint = missing_field.clone().unwrap_or_else(|| "the IDL may be missing fields or padding, or the program allocates more space than the type needs".to_string());
                if nd.size == Some(used) {
                    let _ = write!(
                        hint,
                        " (the IDL's declared account size, {used}, leaves them out too)"
                    );
                }
                (
                    Severity::Error,
                    format!("{unused} of {size} bytes unused in {count}"),
                    hint,
                )
            };
            self.push(
                severity,
                Check::BytesUsed,
                subject,
                message,
                Some(hint),
                Some(&example),
            );
        }
    }

    /// Unused bytes on a variable-size type are normal (accounts are allocated at their
    /// maximum size) as long as they are zero.
    fn report_variable_leftover(&mut self, subject: &str, outcomes: &Outcomes) {
        let accounts = &self.samples.accounts;
        let mut affected = Vec::new();
        let mut nonzero = Vec::new();
        let mut most = 0;
        for (i, result) in outcomes {
            let Ok((_, used)) = result else { continue };
            let data = &accounts[*i].data;
            let Some(rest) = data.get(*used..).filter(|r| !r.is_empty()) else {
                continue;
            };
            affected.push(*i);
            most = most.max(rest.len());
            if rest.iter().any(|b| *b != 0) {
                nonzero.push(*i);
            }
        }
        let total = outcomes.len();
        if let Some(&first) = nonzero.first() {
            let example = accounts[first].address.clone();
            let hint = missing_field_hint(outcomes, |i| accounts[i].data.as_slice())
                .unwrap_or_else(|| "the IDL may be missing trailing fields".into());
            self.push(
                Severity::Warning,
                Check::BytesUsed,
                subject,
                format!(
                    "non-zero bytes follow the decoded data in {}",
                    of_samples(nonzero.len(), total)
                ),
                Some(hint),
                Some(&example),
            );
        } else if !affected.is_empty() {
            self.push(
                Severity::Note,
                Check::BytesUsed,
                subject,
                format!(
                    "up to {} of zeros follow the decoded data in {} (spare allocation)",
                    bytes(most),
                    of_samples(affected.len(), total)
                ),
                None,
                None,
            );
        }
    }

    fn compare_accounts(
        &mut self,
        subject: &str,
        outcomes: &Outcomes,
        padded: Option<&StructLayout>,
    ) {
        let Some(reference) = self.reference else {
            return;
        };
        let decoded: Vec<(usize, &Value)> = outcomes
            .iter()
            .filter_map(|(i, r)| r.as_ref().ok().map(|(v, _)| (*i, v)))
            .collect();
        if decoded.is_empty() {
            return;
        }
        if let Some(layout) = padded.filter(|_| !reference.honors_padding) {
            self.reference_summary.skipped += decoded.len();
            self.push(
                Severity::Note,
                Check::Reference,
                subject,
                format!(
                    "not compared with the reference decoder, which ignores the {} of repr(C) padding in zero-copy types",
                    bytes(layout.padding_len())
                ),
                None,
                None,
            );
            return;
        }
        let entries: Vec<(usize, Option<&Outcome>, &Value)> = decoded
            .iter()
            .map(|&(i, v)| (i, reference.accounts.get(i), v))
            .collect();
        let (compared, matched) = self.compare(subject, &entries, |i| {
            self.samples.accounts[i].address.clone()
        });
        self.reference_summary.accounts_compared += compared;
        self.reference_summary.accounts_matched += matched;
    }
}

fn unknown_account_hint(idl: &Idl, sized: &[(usize, usize)], by_size: bool) -> String {
    if by_size {
        let known: Vec<String> = sized
            .iter()
            .map(|(t, size)| format!("{} {size}", idl.accounts[*t].name))
            .collect();
        format!(
            "the IDL tells these accounts apart by size ({}); a longer account has room the IDL does not declare, or is a type it lacks",
            known.join(", ")
        )
    } else {
        "the program uses an account type the IDL does not declare, or these accounts belong to another program".to_string()
    }
}

fn padding_list(layout: &StructLayout) -> String {
    let parts: Vec<String> = layout.padding.iter().map(ToString::to_string).collect();
    parts.join(", ")
}

fn round_up_8(n: usize) -> Option<usize> {
    n.div_ceil(8).checked_mul(8)
}

#[cfg(test)]
mod tests {
    use crate::report::{Check, Severity};
    use crate::samples::{AccountSample, Samples};
    use crate::testing::{idl, only};
    use crate::verify;
    use idl_drift::model::Idl;

    const DISC: [u8; 2] = [7, 7];

    /// One account type `A` with discriminator [7, 7] and the given type definition.
    fn account_idl(type_def: &str) -> Idl {
        idl(&format!(
            r#""accounts":[{{"name":"A","discriminator":[7,7]}}],"instructions":[],"types":[{type_def}]"#
        ))
    }

    fn accounts(bodies: &[&[u8]]) -> Samples {
        Samples {
            accounts: bodies
                .iter()
                .enumerate()
                .map(|(i, body)| AccountSample {
                    address: format!("addr{i}"),
                    data: [&DISC[..], body].concat(),
                })
                .collect(),
            ..Samples::default()
        }
    }

    const STRING_TYPE: &str =
        r#"{"name":"A","type":{"kind":"struct","fields":[{"name":"s","type":"string"}]}}"#;
    const U32_TYPE: &str =
        r#"{"name":"A","type":{"kind":"struct","fields":[{"name":"v","type":"u32"}]}}"#;

    #[test]
    fn zeros_after_a_variable_size_type_are_spare_allocation() {
        let report = verify(
            &account_idl(STRING_TYPE),
            &accounts(&[&[1, 0, 0, 0, b'x', 0, 0, 0]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Note, Check::BytesUsed));
    }

    #[test]
    fn non_zero_bytes_after_a_variable_size_type_are_a_warning() {
        let report = verify(
            &account_idl(STRING_TYPE),
            &accounts(&[&[1, 0, 0, 0, b'x', 9]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Warning, Check::BytesUsed));
    }

    #[test]
    fn size_rounded_up_to_8_is_a_warning() {
        // 2 + 4 bytes used, 8 allocated.
        let report = verify(
            &account_idl(U32_TYPE),
            &accounts(&[&[1, 0, 0, 0, 0, 0]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Warning, Check::BytesUsed));
        assert_eq!(f.message, "2 bytes after the data in 1 of 1 sample");
    }

    #[test]
    fn unexplained_unused_bytes_are_an_error() {
        let report = verify(
            &account_idl(U32_TYPE),
            &accounts(&[&[1, 0, 0, 0, 5, 5, 5, 5, 5, 5, 5, 5]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Error, Check::BytesUsed));
        assert_eq!(f.message, "8 of 14 bytes unused in 1 of 1 sample");
        assert_eq!(
            f.hint.as_deref(),
            Some("the same 8 bytes follow in every sample: the IDL is likely missing an 8-byte field at the end")
        );
    }

    #[test]
    fn zero_tail_is_a_warning_on_a_borsh_type_and_an_error_on_a_zero_copy_type() {
        // perpetuals custody: 1117 bytes used of 2000, and every unused byte is zero.
        let body: &[u8] = &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let report = verify(&account_idl(U32_TYPE), &accounts(&[body]), None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Warning, Check::BytesUsed));
        assert_eq!(f.message, "8 of 14 bytes unused in 1 of 1 sample, all zero");
        assert_eq!(report.exit_code(), 0);

        let zero_copy = r#"{"name":"A","serialization":"bytemuck","type":{"kind":"struct","fields":[{"name":"v","type":"u32"}]}}"#;
        let report = verify(&account_idl(zero_copy), &accounts(&[body]), None);
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Error, Check::BytesUsed));
    }

    #[test]
    fn sizes_of_a_fixed_type_are_counted() {
        let report = verify(
            &account_idl(U32_TYPE),
            &accounts(&[&[1, 0, 0, 0], &[2, 0, 0, 0, 0, 0]]),
            None,
        );
        let note = report
            .findings
            .iter()
            .find(|f| f.check == Check::SizeDistribution)
            .unwrap();
        assert_eq!(
            note.message,
            "this fixed-size type comes in 2 sizes: 6 (1), 8 (1)"
        );
    }

    #[test]
    fn zero_copy_type_without_a_known_layout_is_a_warning() {
        let report = verify(
            &account_idl(
                r#"{"name":"A","serialization":"bytemuck","type":{"kind":"struct","fields":[{"name":"o","type":{"option":"u8"}}]}}"#,
            ),
            &accounts(&[&[0]]),
            None,
        );
        let f = only(&report);
        assert_eq!((f.severity, f.check), (Severity::Warning, Check::Layout));
    }

    #[test]
    fn account_without_a_type_definition_is_an_error() {
        let report = verify(
            &account_idl(r#"{"name":"B","type":{"kind":"struct"}}"#),
            &accounts(&[&[]]),
            None,
        );
        assert_eq!(only(&report).check, Check::MissingType);
    }

    #[test]
    fn accounts_with_no_data_are_a_note_not_an_error() {
        // perpetuals owns an empty account on mainnet; no discriminator can match it.
        let mut samples = accounts(&[&[5, 0, 0, 0]]);
        samples.accounts.push(AccountSample {
            address: "empty".into(),
            data: Vec::new(),
        });
        let report = verify(&account_idl(U32_TYPE), &samples, None);
        let f = only(&report);
        assert_eq!(
            (
                f.severity,
                f.check,
                f.message.as_str(),
                f.example.as_deref()
            ),
            (
                Severity::Note,
                Check::Discriminator,
                "1 account holds no data; not checked",
                Some("empty")
            )
        );
        assert_eq!(report.exit_code(), 0);
        assert_eq!(
            (
                report.account_coverage.sampled,
                report.account_coverage.classified
            ),
            (2, 1)
        );
    }

    #[test]
    fn idl_without_discriminators_says_so() {
        // A string field leaves no fixed size to tell accounts apart by.
        let idl = idl(&format!(
            r#""accounts":[{{"name":"A"}}],"instructions":[],"types":[{STRING_TYPE}]"#
        ));
        let report = verify(&idl, &accounts(&[&[1, 0, 0, 0]]), None);
        let f = only(&report);
        assert_eq!(f.check, Check::Discriminator);
        assert!(f
            .message
            .starts_with("the IDL gives no account discriminators"));
    }

    #[test]
    fn accounts_without_discriminators_are_told_apart_by_size() {
        let idl = idl(
            r#""accounts":[{"name":"Small","size":4},{"name":"Big"}],"instructions":[],
                "types":[{"name":"Small","type":{"kind":"struct","fields":[{"name":"a","type":"u32"}]}},
                {"name":"Big","type":{"kind":"struct","fields":[{"name":"a","type":"u64"},{"name":"b","type":"u64"}]}}]"#,
        );
        let raw = |bodies: &[&[u8]]| Samples {
            accounts: bodies
                .iter()
                .enumerate()
                .map(|(i, b)| AccountSample {
                    address: format!("addr{i}"),
                    data: b.to_vec(),
                })
                .collect(),
            ..Samples::default()
        };
        // 4 bytes by the declared size, 16 by the computed Borsh size.
        let report = verify(&idl, &raw(&[&[1, 0, 0, 0], &[0; 16]]), None);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.accounts.len(), 2);
        // A length that no type has is an error that names the length.
        let report = verify(&idl, &raw(&[&[0; 7]]), None);
        let f = only(&report);
        assert_eq!(f.check, Check::Discriminator);
        assert!(
            f.message
                .starts_with("7-byte data matches no account type size"),
            "{}",
            f.message
        );
    }
}
