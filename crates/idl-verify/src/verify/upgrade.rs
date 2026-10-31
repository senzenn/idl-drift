//! Upgrade impact: how a new IDL reads the sampled accounts, and the rent of a realloc.

use std::collections::{BTreeMap, BTreeSet};

use idl_drift::model::{Idl, TypeDef};
use serde_json::{Map, Value as Json};

use super::{Outcomes, Verifier};
use crate::decode::{DecodeError, DecodeErrorKind, Decoded, Value};
use crate::error::Error;
use crate::reference::{first_difference, normalize, to_json, Outcome, Reference};
use crate::report::{
    BrokenAccounts, BrokenReason, Check, ImpactTotals, Range, ReallocAccounts, RentSource, Report,
    Severity, ShorterAccounts, TypeImpact, UpgradeImpact,
};
use crate::samples::Samples;
use crate::text::{byte_range, of_samples};

/// Mainnet's rent rate since the SIMD-0437 feature `set_lamports_per_byte_to_5080` (slot
/// 446256000). A later SIMD-0437 step can lower it, so a live run asks the cluster instead.
pub const OFFLINE_LAMPORTS_PER_BYTE: u64 = 5080;

/// Bytes that the runtime adds to the data length for rent (`ACCOUNT_STORAGE_OVERHEAD`).
const ACCOUNT_STORAGE_OVERHEAD: u64 = 128;

/// The largest account data size (`MAX_PERMITTED_DATA_LENGTH`, 10 MiB).
const MAX_ACCOUNT_SIZE: usize = 10 * 1024 * 1024;

/// The rent-exempt minimum of an account with `data_len` bytes of data, offline:
/// `(128 + data_len) * OFFLINE_LAMPORTS_PER_BYTE`, as Agave's `Rent::minimum_balance` gives it.
///
/// # Errors
///
/// When `data_len` is more than the 10 MiB account size limit.
pub fn offline_rent_minimum(data_len: usize) -> Result<u64, Error> {
    u64::try_from(data_len)
        .ok()
        .filter(|_| data_len <= MAX_ACCOUNT_SIZE)
        .and_then(|n| n.checked_add(ACCOUNT_STORAGE_OVERHEAD))
        .and_then(|n| n.checked_mul(OFFLINE_LAMPORTS_PER_BYTE))
        .ok_or_else(|| {
            Error::Rent(format!(
                "{data_len} bytes is more than the 10 MiB account size limit"
            ))
        })
}

/// Decode each account sample with `new`, the IDL of a program upgrade, and put it in a
/// class: ok, shorter, needs a realloc, or broken. Instructions and events are not checked.
///
/// `old` holds the decodes of the current IDL ([`Reference::from_idl`]): a field that both
/// IDLs name and that decodes to another value breaks the account. `cluster_rent` gives the
/// rent-exempt minimum of a data size, such as from [`crate::rpc::rent_exempt_minimum`];
/// without it, [`offline_rent_minimum`] estimates it.
///
/// # Errors
///
/// When `cluster_rent` fails, or gives a smaller minimum for a larger size.
pub fn verify_upgrade(
    new: &Idl,
    samples: &Samples,
    old: Option<&Reference>,
    cluster_rent: Option<&mut dyn FnMut(usize) -> Result<u64, Error>>,
) -> Result<Report, Error> {
    let mut v = Verifier::new(new, samples, old);
    v.upgrade = Some(Vec::new());
    v.check_program_id();
    let accounts = v.check_accounts();
    let types = v.upgrade.take().unwrap_or_default();
    let unknown = samples
        .accounts
        .iter()
        .filter(|a| !a.data.is_empty() && v.classifier.account(&a.data).is_none())
        .count();
    let (rent_source, lamports_per_byte, minimums) = match cluster_rent {
        Some(rent) => (RentSource::Rpc, None, minimums(&types, samples, rent)?),
        None => (
            RentSource::Formula,
            Some(OFFLINE_LAMPORTS_PER_BYTE),
            minimums(&types, samples, &mut offline_rent_minimum)?,
        ),
    };
    let mut totals = ImpactTotals {
        sampled: unknown,
        broken: unknown,
        ..ImpactTotals::default()
    };
    let mut impacts = Vec::new();
    for t in &types {
        let impact = v.report_upgrade_type(t, &minimums)?;
        totals.sampled += impact.sampled;
        totals.ok += impact.ok;
        totals.shorter += impact.shorter.count;
        totals.realloc += impact.realloc.count;
        totals.broken += impact.broken.count;
        totals.rent_total = add_lamports(totals.rent_total, impact.realloc.rent_total)?;
        impacts.push(impact);
    }
    let mut report = v.into_report(accounts, Default::default(), Default::default());
    // The old IDL feeds the classes, not a value comparison of its own.
    report.reference = None;
    report.upgrade_impact = Some(UpgradeImpact {
        rent_source,
        lamports_per_byte,
        types: impacts,
        unknown,
        totals,
    });
    Ok(report)
}

/// What the new IDL makes of one existing account.
enum Class {
    Ok,
    /// Decodes with this many bytes left over.
    Shorter(usize),
    /// Decodes only with this many zero bytes added at the end.
    Realloc(usize),
    /// `reason` groups accounts; `detail` holds the error or the two values.
    Broken {
        reason: String,
        detail: String,
    },
}

/// The class of each sample of one account type, by sample index.
pub(super) struct TypeClasses {
    name: String,
    classes: Vec<(usize, Class)>,
}

impl Verifier<'_> {
    /// Put each sample of an account type in its upgrade class. `disc` is where the type
    /// starts in the data.
    pub(super) fn classify_for_upgrade(
        &mut self,
        name: &str,
        td: &TypeDef,
        disc: usize,
        outcomes: &Outcomes,
    ) {
        let classes = outcomes
            .iter()
            .map(|(i, result)| (*i, self.upgrade_class(td, disc, *i, result)))
            .collect();
        if let Some(types) = &mut self.upgrade {
            types.push(TypeClasses {
                name: name.to_string(),
                classes,
            });
        }
    }

    fn upgrade_class(
        &self,
        td: &TypeDef,
        disc: usize,
        i: usize,
        result: &Result<(Value, usize), DecodeError>,
    ) -> Class {
        let data = &self.samples.accounts[i].data;
        let filled;
        let (value, used, added) = match result {
            Ok((value, used)) => (value, *used, 0),
            Err(e) => {
                let room = MAX_ACCOUNT_SIZE.saturating_sub(data.len());
                let Some(missing) = missing_bytes(e).filter(|_| room > 0) else {
                    return broken(e);
                };
                let body = data.get(disc..).unwrap_or_default();
                match self.zero_fill(td, body, missing, room) {
                    Ok((added, decoded)) => {
                        filled = decoded;
                        (&filled.value, data.len(), added)
                    }
                    Err(e) => return broken(&e.offset_by(disc)),
                }
            }
        };
        if let Some(Outcome::Ok(theirs)) = self.reference.and_then(|r| r.accounts.get(i)) {
            let (ours, theirs) = shared_fields(value, theirs);
            if let Some(d) = first_difference(&ours, &theirs) {
                let field = if d.path.is_empty() {
                    "the value".to_string()
                } else {
                    format!("`{}`", d.path)
                };
                return Class::Broken {
                    reason: format!("{field} reads another value than under the old IDL"),
                    detail: format!(
                        "{field} reads {} under the new IDL and {} under the old IDL",
                        d.ours, d.theirs
                    ),
                };
            }
        }
        if added > 0 {
            Class::Realloc(added)
        } else if used < data.len() {
            Class::Shorter(data.len() - used)
        } else {
            Class::Ok
        }
    }

    /// The fewest zero bytes, at most `room`, that let `td` decode when added after `body`,
    /// and that decode. `missing` is what the failed read lacks without them.
    fn zero_fill(
        &self,
        td: &TypeDef,
        body: &[u8],
        missing: usize,
        room: usize,
    ) -> Result<(usize, Decoded), DecodeError> {
        let mut padded = body.to_vec();
        let mut decode = |n: usize| {
            padded.resize(body.len().saturating_add(n), 0);
            self.decoder.type_def(td, &padded)
        };
        // The bytes that the failed read lacks are a lower bound. Double from there until
        // the data decodes, then search between the two counts.
        let start = missing.clamp(1, room);
        let mut low = start - 1;
        let mut n = start;
        let (mut high, mut best) = loop {
            match decode(n) {
                Ok(decoded) => break (n, decoded),
                Err(e) if n == room || missing_bytes(&e).is_none() => return Err(e),
                Err(_) => {
                    low = n;
                    n = n.saturating_mul(2).min(room);
                }
            }
        };
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            match decode(mid) {
                Ok(decoded) => (high, best) = (mid, decoded),
                Err(_) => low = mid,
            }
        }
        Ok((high, best))
    }

    /// The impact row of one account type, and its findings.
    fn report_upgrade_type(
        &mut self,
        t: &TypeClasses,
        minimums: &BTreeMap<usize, u64>,
    ) -> Result<TypeImpact, Error> {
        let accounts = &self.samples.accounts;
        let total = t.classes.len();
        let mut ok = 0;
        let mut shorter = Vec::new();
        let mut realloc = Vec::new();
        let mut reasons: BTreeMap<&str, (Vec<usize>, &str)> = BTreeMap::new();
        for (i, class) in &t.classes {
            match class {
                Class::Ok => ok += 1,
                Class::Shorter(n) => shorter.push((*i, *n)),
                Class::Realloc(n) => {
                    let len = accounts[*i].data.len();
                    realloc.push((*i, *n, extra_rent(minimums, len, *n)?));
                }
                Class::Broken { reason, detail } => {
                    reasons
                        .entry(reason)
                        .or_insert_with(|| (Vec::new(), detail))
                        .0
                        .push(*i);
                }
            }
        }
        let rent_total = realloc
            .iter()
            .try_fold(0, |sum, (_, _, rent)| add_lamports(sum, *rent))?;
        let impact = TypeImpact {
            name: t.name.clone(),
            sampled: total,
            ok,
            shorter: ShorterAccounts {
                count: shorter.len(),
                bytes: Range::of(shorter.iter().map(|(_, n)| *n)),
            },
            realloc: ReallocAccounts {
                count: realloc.len(),
                bytes: Range::of(realloc.iter().map(|(_, n, _)| *n)),
                rent_each: Range::of(realloc.iter().map(|(_, _, rent)| *rent)),
                rent_total,
            },
            broken: BrokenAccounts {
                count: reasons.values().map(|(idxs, _)| idxs.len()).sum(),
                reasons: reasons
                    .iter()
                    .map(|(reason, (idxs, detail))| BrokenReason {
                        reason: (*reason).to_string(),
                        count: idxs.len(),
                        detail: (*detail).to_string(),
                        example: accounts[idxs[0]].address.clone(),
                    })
                    .collect(),
            },
        };

        let examples = (
            realloc
                .first()
                .map(|(i, _, _)| accounts[*i].address.clone()),
            shorter.first().map(|(i, _)| accounts[*i].address.clone()),
        );
        self.report_upgrade_findings(&impact, examples);
        Ok(impact)
    }

    /// One error for each reason that breaks accounts, one warning for the accounts that
    /// need a realloc, and one note for the shorter ones, with an address of each.
    fn report_upgrade_findings(
        &mut self,
        impact: &TypeImpact,
        (realloc_example, shorter_example): (Option<String>, Option<String>),
    ) {
        let subject = format!("account {}", impact.name);
        let total = impact.sampled;
        for r in &impact.broken.reasons {
            self.push(
                Severity::Error,
                Check::UpgradeImpact,
                &subject,
                format!(
                    "breaks under the new IDL in {}: {}",
                    of_samples(r.count, total),
                    r.detail
                ),
                Some("the new program cannot read these accounts as they are; migrate them, or keep the layout of the fields that they hold".into()),
                Some(&r.example),
            );
        }
        let realloc = &impact.realloc;
        if let (Some(n), Some(rent)) = (realloc.bytes, realloc.rent_each) {
            self.push(
                Severity::Warning,
                Check::UpgradeImpact,
                &subject,
                format!(
                    "needs a realloc of {} under the new IDL in {}",
                    byte_range(n),
                    of_samples(realloc.count, total)
                ),
                Some(format!(
                    "the new layout is longer than these accounts; reallocate each account before the new program reads it, and add {rent} lamports of rent to each ({} for these samples)",
                    realloc.rent_total
                )),
                realloc_example.as_deref(),
            );
        }
        if let Some(n) = impact.shorter.bytes {
            self.push(
                Severity::Note,
                Check::UpgradeImpact,
                &subject,
                format!(
                    "{} after the data that the new IDL reads, in {}",
                    byte_range(n),
                    of_samples(impact.shorter.count, total)
                ),
                Some("the new type can be smaller than the old one, or the accounts have spare room; they still decode".into()),
                shorter_example.as_deref(),
            );
        }
    }
}

/// The bytes that a read past the end of the data lacks, which zero bytes added at the end
/// can supply; `None` for other errors.
fn missing_bytes(e: &DecodeError) -> Option<usize> {
    match e.kind {
        DecodeErrorKind::UnexpectedEnd { needed, remaining } => {
            Some(needed.saturating_sub(remaining))
        }
        _ => None,
    }
}

fn broken(e: &DecodeError) -> Class {
    let path = e.path();
    let reason = match (&e.kind, path.is_empty()) {
        // A live account holds complete data, so a length past its end reads other bytes.
        (DecodeErrorKind::TooLong { .. }, true) => "a length prefix points past the data".into(),
        (DecodeErrorKind::TooLong { .. }, false) => {
            format!("the length prefix of `{path}` points past the data")
        }
        (_, true) => "the data does not decode".into(),
        (_, false) => format!("`{path}` does not decode"),
    };
    Class::Broken {
        reason,
        detail: e.to_string(),
    }
}

/// `ours` and `theirs` cut down to the struct fields that both IDLs name and the list items
/// that both read, also inside a present option or the same enum variant. So a new field, or
/// a reserved array that gives bytes to one, is not a difference. Other values compare whole.
fn shared_fields(ours: &Value, theirs: &Json) -> (Json, Json) {
    match (ours, theirs) {
        (Value::Struct(fields), Json::Object(map)) => {
            let mut by_key: BTreeMap<String, (&String, &Json)> = BTreeMap::new();
            for (k, v) in map {
                by_key.entry(normalize(k)).or_insert((k, v));
            }
            let (mut a, mut b) = (Map::new(), Map::new());
            for (name, value) in fields {
                if let Some(&(k, v)) = by_key.get(&normalize(name)) {
                    let (x, y) = shared_fields(value, v);
                    a.insert(name.clone(), x);
                    b.insert(k.clone(), y);
                }
            }
            (Json::Object(a), Json::Object(b))
        }
        (Value::List(items), Json::Array(list)) => {
            let (a, b) = items
                .iter()
                .zip(list)
                .map(|(x, y)| shared_fields(x, y))
                .unzip();
            (Json::Array(a), Json::Array(b))
        }
        // `to_json` writes a present option as its value, and a variant as a one-key object.
        (Value::Option(Some(inner)), other) if !other.is_null() => shared_fields(inner, other),
        (Value::Enum { variant, value }, Json::Object(map))
            if map.len() == 1 && map.keys().all(|k| normalize(k) == normalize(variant)) =>
        {
            let (a, b) = map
                .iter()
                .map(|(k, v)| {
                    let (x, y) = shared_fields(value, v);
                    ((variant.clone(), x), (k.clone(), y))
                })
                .unzip();
            (Json::Object(a), Json::Object(b))
        }
        _ => (to_json(ours), theirs.clone()),
    }
}

/// The rent-exempt minimum of each size that a realloc starts from or ends at: one call
/// for each size.
fn minimums(
    types: &[TypeClasses],
    samples: &Samples,
    minimum: &mut dyn FnMut(usize) -> Result<u64, Error>,
) -> Result<BTreeMap<usize, u64>, Error> {
    let mut sizes = BTreeSet::new();
    for (i, class) in types.iter().flat_map(|t| &t.classes) {
        if let Class::Realloc(added) = class {
            let len = samples.accounts[*i].data.len();
            sizes.insert(len);
            sizes.insert(len.saturating_add(*added));
        }
    }
    sizes
        .into_iter()
        .map(|size| Ok((size, minimum(size)?)))
        .collect()
}

/// The rent to add when an account of `len` bytes grows by `added`.
fn extra_rent(minimums: &BTreeMap<usize, u64>, len: usize, added: usize) -> Result<u64, Error> {
    let to = len.saturating_add(added);
    minimums
        .get(&to)
        .zip(minimums.get(&len))
        .and_then(|(to, from)| to.checked_sub(*from))
        .ok_or_else(|| {
            Error::Rent(format!(
                "the rent-exempt minimum of {to} bytes is less than that of {len} bytes"
            ))
        })
}

fn add_lamports(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b)
        .ok_or_else(|| Error::Rent("the sum of the extra rent is more than a u64 holds".into()))
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value as Json};

    use super::*;
    use crate::rpc::rent_exempt_minimum;
    use crate::samples::AccountSample;
    use crate::testing::idl;

    const DISC: [u8; 2] = [7, 7];

    /// One account type `A` with discriminator [7, 7] and these fields.
    fn account_idl(fields: &str) -> Idl {
        idl(&format!(
            r#""accounts":[{{"name":"A","discriminator":[7,7]}}],"instructions":[],
                "types":[{{"name":"A","type":{{"kind":"struct","fields":{fields}}}}}]"#
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

    /// The impact of `new` on `samples`, offline, compared with `old` when given.
    fn impact(new: &Idl, old: Option<&Idl>, samples: &Samples) -> Report {
        let old = old.map(|old| Reference::from_idl(old, new, samples));
        verify_upgrade(new, samples, old.as_ref(), None).unwrap()
    }

    fn row(report: &Report) -> &TypeImpact {
        &report.upgrade_impact.as_ref().unwrap().types[0]
    }

    const U32: &str = r#"[{"name":"a","type":"u32"}]"#;

    #[test]
    fn data_that_the_new_idl_reads_to_the_end_is_ok() {
        let report = impact(&account_idl(U32), None, &accounts(&[&[1, 0, 0, 0]]));
        assert_eq!(row(&report).ok, 1);
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
    }

    #[test]
    fn bytes_left_over_are_a_note() {
        let report = impact(&account_idl(U32), None, &accounts(&[&[1, 0, 0, 0, 9, 9]]));
        assert_eq!(row(&report).shorter.count, 1);
        let f = crate::testing::only(&report);
        assert_eq!(
            (f.severity, f.check, f.message.as_str()),
            (
                Severity::Note,
                Check::UpgradeImpact,
                "2 bytes after the data that the new IDL reads, in 1 of 1 sample"
            )
        );
    }

    #[test]
    fn realloc_is_the_fewest_zero_bytes_that_decode() {
        // The string ends the data; the new array and option need 8 + 1 bytes more.
        let new = account_idl(
            r#"[{"name":"s","type":"string"},{"name":"r","type":{"array":["u8",8]}},{"name":"o","type":{"option":"u32"}}]"#,
        );
        let report = impact(&new, None, &accounts(&[&[2, 0, 0, 0, b'h', b'i']]));
        let realloc = &row(&report).realloc;
        assert_eq!(
            (realloc.count, realloc.bytes, realloc.rent_each),
            (
                1,
                Some(Range { min: 9, max: 9 }),
                Some(Range {
                    min: 9 * OFFLINE_LAMPORTS_PER_BYTE,
                    max: 9 * OFFLINE_LAMPORTS_PER_BYTE
                })
            )
        );
        let f = crate::testing::only(&report);
        assert_eq!(
            (f.severity, f.check),
            (Severity::Warning, Check::UpgradeImpact)
        );
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_decode_error_that_zero_bytes_cannot_fix_is_broken() {
        let new = account_idl(r#"[{"name":"flag","type":"bool"},{"name":"n","type":"u64"}]"#);
        let report = impact(&new, None, &accounts(&[&[7]]));
        let broken = &row(&report).broken;
        assert_eq!(broken.count, 1);
        assert_eq!(broken.reasons[0].reason, "`flag` does not decode");
        let f = crate::testing::only(&report);
        assert_eq!(
            (f.severity, f.check),
            (Severity::Error, Check::UpgradeImpact)
        );
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn a_length_prefix_past_the_data_is_broken() {
        // The new IDL reads the old u32, 1000, as the length of `v`.
        let new = account_idl(r#"[{"name":"v","type":"bytes"}]"#);
        let report = impact(&new, None, &accounts(&[&[0xe8, 0x03, 0, 0]]));
        let broken = &row(&report).broken;
        assert_eq!(
            (broken.count, broken.reasons[0].reason.as_str()),
            (1, "the length prefix of `v` points past the data")
        );
    }

    #[test]
    fn a_shared_field_that_reads_another_value_is_broken() {
        let old = account_idl(r#"[{"name":"a","type":"u32"},{"name":"b","type":"u32"}]"#);
        let samples = accounts(&[&[1, 0, 0, 0, 2, 0, 0, 0]]);
        // `a` grows to a u64 and takes the bytes of `b`: only the old IDL shows it.
        let widened = account_idl(r#"[{"name":"a","type":"u64"},{"name":"b","type":"u32"}]"#);
        assert_eq!(row(&impact(&widened, None, &samples)).realloc.count, 1);
        let report = impact(&widened, Some(&old), &samples);
        assert_eq!(
            row(&report).broken.reasons[0].detail,
            "`a` reads 8589934593 under the new IDL and 1 under the old IDL"
        );
        // A field only the new IDL has is not a difference.
        let appended = account_idl(
            r#"[{"name":"a","type":"u32"},{"name":"b","type":"u32"},{"name":"c","type":"u64"}]"#,
        );
        assert_eq!(
            row(&impact(&appended, Some(&old), &samples)).realloc.count,
            1
        );
    }

    /// The new IDL's `{a: 5, b: 0}`: it adds `b` to a struct that the old IDL reads as `{a: 5}`.
    fn with_new_field() -> Value {
        Value::Struct(vec![("a".into(), Value::U8(5)), ("b".into(), Value::U8(0))])
    }

    #[test]
    fn shared_fields_look_inside_an_option() {
        let ours = Value::Option(Some(Box::new(with_new_field())));
        assert_eq!(
            shared_fields(&ours, &json!({"a": 5})),
            (json!({"a": 5}), json!({"a": 5}))
        );
        // An option that only one IDL reads as present is still a difference.
        assert_eq!(shared_fields(&ours, &Json::Null).0, to_json(&ours));
    }

    #[test]
    fn shared_fields_look_inside_the_same_enum_variant() {
        let ours = Value::Enum {
            variant: "on".into(),
            value: Box::new(with_new_field()),
        };
        assert_eq!(
            shared_fields(&ours, &json!({"On": {"a": 5}})),
            (json!({"on": {"a": 5}}), json!({"On": {"a": 5}}))
        );
        // Another variant compares whole.
        assert_eq!(shared_fields(&ours, &json!({"Off": {}})).0, to_json(&ours));
    }

    #[test]
    fn an_unknown_discriminator_is_broken() {
        let mut samples = accounts(&[&[1, 0, 0, 0]]);
        samples.accounts[0].data[0] = 9;
        let report = impact(&account_idl(U32), None, &samples);
        let impact = report.upgrade_impact.as_ref().unwrap();
        assert_eq!((impact.unknown, impact.totals.broken), (1, 1));
        assert_eq!(crate::testing::only(&report).check, Check::Discriminator);
    }

    #[test]
    fn the_cluster_gives_the_rent_once_per_size() {
        let new = account_idl(r#"[{"name":"a","type":"u32"},{"name":"b","type":"u64"}]"#);
        let samples = accounts(&[&[1, 0, 0, 0], &[2, 0, 0, 0]]);
        let mut asked = Vec::new();
        let mut rent = |len| {
            rent_exempt_minimum(
                |method: &str, params: Json| {
                    asked.push((method.to_string(), params.clone()));
                    Ok(json!(1000 + 10 * params[0].as_u64().unwrap()))
                },
                len,
            )
        };
        let report = verify_upgrade(&new, &samples, None, Some(&mut rent)).unwrap();
        // Both accounts grow from 6 to 14 bytes: 80 lamports more each.
        let impact = report.upgrade_impact.as_ref().unwrap();
        assert_eq!(impact.rent_source, RentSource::Rpc);
        assert_eq!(impact.types[0].realloc.rent_total, 160);
        assert_eq!(
            asked,
            [
                ("getMinimumBalanceForRentExemption".to_string(), json!([6])),
                ("getMinimumBalanceForRentExemption".to_string(), json!([14]))
            ]
        );
    }

    #[test]
    fn the_offline_rent_is_the_mainnet_rate() {
        // getMinimumBalanceForRentExemption on mainnet, 2026-10-15.
        assert_eq!(offline_rent_minimum(0).unwrap(), 650_240);
        assert_eq!(offline_rent_minimum(100).unwrap(), 1_158_240);
        assert!(offline_rent_minimum(MAX_ACCOUNT_SIZE + 1).is_err());
    }
}
