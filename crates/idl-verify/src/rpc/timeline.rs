//! The deploys of a program, and which IDL decodes the transactions of each slot range
//! between two deploys. A backfill uses it to pick the parser for each range.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use idl_drift::markdown::escape_control;
use idl_drift::model::Idl;
use idl_drift::stale::{LOADER_V3, LOADER_V4};

use super::{
    first_bytes, program_data_address, samples_from_transaction, transaction, transaction_record,
    u64_at, SignatureInfo, TransactionRecord, SIGNATURES_PER_PAGE,
};
use crate::error::Error;
use crate::report::{Check, Severity};
use crate::samples::Samples;
use crate::verify::verify;

/// How much a timeline run reads. Start from [`TimelineLimits::default`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TimelineLimits {
    /// Program transactions with samples to decode in each slot range.
    pub per_range: usize,
    /// Successful transactions to read at most in each slot range.
    pub max_transactions: usize,
    /// Transactions to read at most to find the deploys.
    pub max_scan: usize,
}

impl Default for TimelineLimits {
    fn default() -> Self {
        Self {
            per_range: 10,
            max_transactions: 40,
            max_scan: 100,
        }
    }
}

/// A transaction that put new code in the program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Deploy {
    pub slot: u64,
    pub signature: String,
    /// The first deploy of the program: no code of it ran before.
    pub first: bool,
}

/// Why the deploy scan stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEnd {
    /// The scan found the deploy that made the `ProgramData` account, so the list is complete
    /// from there. Before a close and a new deploy at the same address (SIMD-0432), the
    /// account had another life, which the scan does not list.
    FirstDeploy,
    /// The RPC has no older signatures, and none of the ones read is the first deploy.
    NoOlderSignatures,
    /// The scan read its limit of transactions or pages.
    ScanLimit,
    /// The program's loader cannot replace its code.
    FixedCode,
}

/// The deploys of one program, oldest first.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DeployHistory {
    /// The owner of the program account.
    pub loader: String,
    /// The accounts whose signatures the scan listed, in order: the upgrade authority, then
    /// each authority before it. Without an authority, the `ProgramData` or program account.
    pub listed: Vec<String>,
    pub deploys: Vec<Deploy>,
    pub end: HistoryEnd,
    /// Transactions the scan read.
    pub scanned: usize,
    /// Transactions the scan could not read, because the RPC no longer keeps them.
    pub unreadable: usize,
}

/// One slot range of the same program code, and how each IDL decodes its samples.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RangeResult {
    /// The first slot that ran this code. `None` when the range starts before the oldest
    /// deploy found.
    pub first_slot: Option<u64>,
    /// The last slot that ran this code. `None` for the code that runs now.
    pub last_slot: Option<u64>,
    /// The deploy that starts the range.
    pub deploy_signature: Option<String>,
    /// Transactions of the range that call the program or log its events.
    pub samples: usize,
    /// Successful transactions requested in the range.
    pub transactions_read: usize,
    /// Requested transactions that the RPC no longer keeps.
    pub unreadable: usize,
    /// For each IDL, in the order given: the samples that it decodes with no error finding.
    pub decoded: Vec<usize>,
    /// For each IDL, in the order given: its first error finding in the range.
    pub first_error: Vec<Option<FirstError>>,
}

/// The first error finding of one IDL in a range: why the IDL does not fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct FirstError {
    pub check: Check,
    pub subject: String,
    pub message: String,
    /// The transaction whose samples gave the finding.
    pub signature: String,
}

impl RangeResult {
    /// The IDL indices that decode every sample; empty for a range with no sample.
    #[must_use]
    pub fn fitting(&self) -> Vec<usize> {
        if self.samples == 0 {
            return Vec::new();
        }
        let all = self.decoded.iter().enumerate();
        all.filter(|(_, &n)| n == self.samples)
            .map(|(i, _)| i)
            .collect()
    }

    /// The IDL indices that decode the most samples, ties kept; empty when none decodes one.
    #[must_use]
    pub fn best(&self) -> Vec<usize> {
        let most = self.decoded.iter().copied().max().unwrap_or(0);
        let all = self.decoded.iter().enumerate();
        all.filter(|(_, &n)| n == most && n > 0)
            .map(|(i, _)| i)
            .collect()
    }

    /// A range with samples that no single IDL decodes.
    #[must_use]
    pub fn unfit(&self) -> bool {
        self.samples > 0 && self.fitting().is_empty()
    }
}

/// A step of a timeline run, for a progress line.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Progress<'a> {
    /// The deploy scan ended; `ranges` ranges follow.
    Scanned {
        history: &'a DeployHistory,
        ranges: usize,
    },
    /// Range `number` (from 1) of `ranges` is sampled.
    Range {
        number: usize,
        ranges: usize,
        result: &'a RangeResult,
    },
}

impl std::fmt::Display for Progress<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scanned { history, ranges } => write!(
                f,
                "{} found in {} transactions; {} to sample",
                counted(history.deploys.len(), "deploy", "deploys"),
                history.scanned,
                counted(*ranges, "range", "ranges")
            ),
            Self::Range {
                number,
                ranges,
                result,
            } => {
                let (first, last) = slots(result);
                write!(
                    f,
                    "range {number} of {ranges}, slots {first} to {last}: {} in {} transactions read",
                    counted(result.samples, "sample", "samples"),
                    result.transactions_read
                )
            }
        }
    }
}

/// The deploys of a program and the result of each slot range, oldest first.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Timeline {
    pub program: String,
    pub history: DeployHistory,
    pub ranges: Vec<RangeResult>,
}

impl Timeline {
    /// Ranges with samples that no single IDL decodes.
    #[must_use]
    pub fn unfit(&self) -> usize {
        self.ranges.iter().filter(|r| r.unfit()).count()
    }

    /// Whether any range has a sample. Without one, the run checked nothing.
    #[must_use]
    pub fn sampled(&self) -> bool {
        self.ranges.iter().any(|r| r.samples > 0)
    }

    /// 1 when a range has samples that no single IDL decodes, else 0.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        u8::from(self.unfit() > 0)
    }

    /// Pretty-printed JSON. `labels` names the IDLs, in the order given to [`timeline`].
    ///
    /// # Errors
    ///
    /// Only if serialization fails, which plain data like this does not.
    pub fn to_json(&self, labels: &[String]) -> serde_json::Result<String> {
        #[derive(Serialize)]
        struct IdlResult<'a> {
            idl: &'a str,
            decoded: usize,
            fit: &'static str,
            first_error: Option<&'a FirstError>,
        }
        #[derive(Serialize)]
        struct RangeView<'a> {
            first_slot: Option<u64>,
            last_slot: Option<u64>,
            deploy_signature: Option<&'a str>,
            samples: usize,
            transactions_read: usize,
            unreadable: usize,
            results: Vec<IdlResult<'a>>,
            fitting_idls: Vec<&'a str>,
            best: Vec<&'a str>,
        }
        #[derive(Serialize)]
        struct Scan<'a> {
            listed: &'a [String],
            transactions_read: usize,
            unreadable: usize,
            end: HistoryEnd,
        }
        #[derive(Serialize)]
        struct View<'a> {
            program: &'a str,
            loader: &'a str,
            idls: &'a [String],
            deploy_scan: Scan<'a>,
            deploys: &'a [Deploy],
            ranges: Vec<RangeView<'a>>,
            ranges_without_fit: usize,
            exit_code: u8,
        }
        let h = &self.history;
        let ranges = self.ranges.iter().map(|r| RangeView {
            first_slot: r.first_slot,
            last_slot: r.last_slot,
            deploy_signature: r.deploy_signature.as_deref(),
            samples: r.samples,
            transactions_read: r.transactions_read,
            unreadable: r.unreadable,
            results: labels
                .iter()
                .zip(&r.decoded)
                .zip(&r.first_error)
                .map(|((idl, &decoded), error)| IdlResult {
                    idl,
                    decoded,
                    fit: fit(decoded, r.samples),
                    first_error: error.as_ref(),
                })
                .collect(),
            fitting_idls: named(labels, &r.fitting()),
            best: named(labels, &r.best()),
        });
        serde_json::to_string_pretty(&View {
            program: &self.program,
            loader: &h.loader,
            idls: labels,
            deploy_scan: Scan {
                listed: &h.listed,
                transactions_read: h.scanned,
                unreadable: h.unreadable,
                end: h.end,
            },
            deploys: &h.deploys,
            ranges: ranges.collect(),
            ranges_without_fit: self.unfit(),
            exit_code: self.exit_code(),
        })
    }

    /// The human report: the deploy scan, a table with one row per range, and the ranges
    /// that no IDL fits. `labels` names the IDLs, in the order given to [`timeline`].
    ///
    /// # Errors
    ///
    /// When writing to `out` fails.
    pub fn write_human(&self, out: &mut impl Write, labels: &[String]) -> io::Result<()> {
        writeln!(out, "program {}", escape_control(&self.program))?;
        self.history.write_human(out)?;
        for (i, label) in labels.iter().enumerate() {
            writeln!(out, "IDL {}  {}", i + 1, escape_control(label))?;
        }
        writeln!(out)?;
        write_table(out, &self.rows(labels.len()))?;

        writeln!(out)?;
        for r in self.ranges.iter().filter(|r| r.unfit()) {
            self.write_unfit(out, r)?;
        }
        for r in self.ranges.iter().filter(|r| r.unreadable > 0) {
            let (first, last) = slots(r);
            let unread = counted(r.unreadable, "transaction", "transactions");
            writeln!(
                out,
                "note: slots {first} to {last}: the RPC no longer keeps {unread}"
            )?;
        }
        let ranges = counted(self.ranges.len(), "range", "ranges");
        write!(out, "summary: {ranges}")?;
        match self.unfit() {
            0 => write!(out, "; each one with samples has an IDL that decodes all")?,
            n => write!(out, "; {n} with samples that no single IDL decodes")?,
        }
        let empty = self.ranges.iter().filter(|r| r.samples == 0).count();
        if empty > 0 {
            write!(out, "; {empty} with no sample")?;
        }
        writeln!(out)
    }

    /// The error line of a range that no IDL fits, then the first error of each IDL.
    fn write_unfit(&self, out: &mut impl Write, r: &RangeResult) -> io::Result<()> {
        let (first, last) = slots(r);
        let samples = counted(r.samples, "sample", "samples");
        write!(
            out,
            "error: slots {first} to {last}: no IDL decodes all {samples}"
        )?;
        let best: Vec<String> = r.best().iter().map(|i| format!("IDL {}", i + 1)).collect();
        let most = r.decoded.iter().copied().max().unwrap_or(0);
        match best.len() {
            0 => write!(out, "; none decodes any")?,
            1 => write!(out, "; {} decodes the most, {most}", best.join(""))?,
            _ => write!(out, "; {} decode the most, {most}", best.join(" and "))?,
        }
        // A scan that did not reach the first deploy can leave older deploys in this range.
        if r.first_slot.is_none() && self.history.end != HistoryEnd::FixedCode {
            write!(out, "; the range can span several deploys")?;
        }
        writeln!(out)?;
        for (i, error) in r.first_error.iter().enumerate() {
            let Some(e) = error else { continue };
            writeln!(
                out,
                "  IDL {}: {} {}: {}; transaction {}",
                i + 1,
                idl_drift::allow::json_name(&e.check),
                escape_control(&e.subject),
                escape_control(&e.message),
                escape_control(&e.signature)
            )?;
        }
        Ok(())
    }

    /// The table: a head, then one row per range.
    fn rows(&self, idls: usize) -> Vec<Vec<String>> {
        let mut head = vec!["from slot".to_string(), "to slot".into(), "samples".into()];
        head.extend((1..=idls).map(|i| format!("IDL {i}")));
        head.extend(["fits".into(), "deploy".into()]);
        let mut rows = vec![head];
        for r in &self.ranges {
            let (first, last) = slots(r);
            let mut row = vec![first, last, r.samples.to_string()];
            row.extend(r.decoded.iter().map(|&n| match r.samples {
                0 => "-".to_string(),
                samples => format!("{n}/{samples}"),
            }));
            let fitting: Vec<String> = r.fitting().iter().map(|i| (i + 1).to_string()).collect();
            row.push(match (r.samples, fitting.is_empty()) {
                (0, _) => "-".into(),
                (_, true) => "none".into(),
                _ => fitting.join(","),
            });
            let deploy = r.deploy_signature.as_deref();
            row.push(deploy.map_or("-".into(), escape_control));
            rows.push(row);
        }
        rows
    }
}

impl DeployHistory {
    fn write_human(&self, out: &mut impl Write) -> io::Result<()> {
        let listed: Vec<String> = self.listed.iter().map(|a| escape_control(a)).collect();
        let oldest = "so the range from ? can hold older deploys";
        if self.end != HistoryEnd::FixedCode {
            writeln!(
                out,
                "{} found in {} transactions of {}",
                counted(self.deploys.len(), "deploy", "deploys"),
                self.scanned,
                listed.join(", then ")
            )?;
        }
        match self.end {
            HistoryEnd::FirstDeploy => {
                writeln!(
                    out,
                    "the scan reached the deploy that made the program, so the list is complete from there"
                )?;
            }
            HistoryEnd::NoOlderSignatures => writeln!(
                out,
                "the RPC has no older signatures of {}, and the first deploy is not among them, {oldest}",
                listed.last().map_or("", String::as_str)
            )?,
            HistoryEnd::ScanLimit => writeln!(
                out,
                "the scan stopped at --max-scan transactions before the first deploy, so the range from ? can span several deploys"
            )?,
            HistoryEnd::FixedCode => writeln!(
                out,
                "loader {}: it cannot replace the program's code, so the whole history is one range",
                escape_control(&self.loader)
            )?,
        }
        if self.unreadable > 0 {
            writeln!(
                out,
                "note: the RPC no longer keeps {} of the scan; a deploy among them is not listed",
                counted(self.unreadable, "transaction", "transactions")
            )?;
        }
        Ok(())
    }
}

/// The first and last slot of a range as text: `?` before the oldest deploy found, `now`
/// for the code that runs now.
fn slots(r: &RangeResult) -> (String, String) {
    (
        r.first_slot.map_or("?".into(), |s| s.to_string()),
        r.last_slot.map_or("now".into(), |s| s.to_string()),
    )
}

fn fit(decoded: usize, samples: usize) -> &'static str {
    match decoded {
        _ if samples == 0 => "no_sample",
        n if n == samples => "all",
        0 => "none",
        _ => "some",
    }
}

fn named<'a>(labels: &'a [String], indices: &[usize]) -> Vec<&'a str> {
    let found = indices.iter().filter_map(|&i| labels.get(i));
    found.map(String::as_str).collect()
}

fn counted(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Columns padded to their widest cell; the last one is not padded.
fn write_table(out: &mut impl Write, rows: &[Vec<String>]) -> io::Result<()> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|c| {
            let cells = rows.iter().filter_map(|r| r.get(c));
            cells.map(|s| s.chars().count()).max().unwrap_or(0)
        })
        .collect();
    for row in rows {
        let mut line = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c + 1 == row.len() {
                line.push_str(cell);
            } else {
                let width = widths.get(c).copied().unwrap_or(0);
                let _ = write!(line, "{cell:<width$}  ");
            }
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

/// List the deploys of `program_id`, then decode up to [`TimelineLimits::per_range`] program
/// transactions of each slot range with each of `idls`. `rpc` sends one JSON-RPC request and
/// returns its `result`.
///
/// # Errors
///
/// When a request fails, or the RPC has no program account at `program_id`.
pub fn timeline(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, Error>,
    program_id: &str,
    idls: &[Idl],
    limits: &TimelineLimits,
    mut progress: impl FnMut(Progress<'_>),
) -> Result<Timeline, Error> {
    let history = deploy_history(&mut rpc, program_id, limits.max_scan)?;
    let deploys = &history.deploys;
    let mut bounds = Vec::new();
    // Code ran before the oldest deploy found, unless that one is the first deploy.
    if !deploys.first().is_some_and(|d| d.first) {
        bounds.push((None, deploys.first()));
    }
    bounds.extend(
        deploys
            .iter()
            .enumerate()
            .map(|(i, d)| (Some(d), deploys.get(i + 1))),
    );
    let count = bounds.len();
    progress(Progress::Scanned {
        history: &history,
        ranges: count,
    });
    let mut ranges = Vec::new();
    for (i, (start, end)) in bounds.into_iter().enumerate() {
        let result = sample_range(&mut rpc, program_id, idls, start, end, limits)?;
        progress(Progress::Range {
            number: i + 1,
            ranges: count,
            result: &result,
        });
        ranges.push(result);
    }
    Ok(Timeline {
        program: program_id.to_string(),
        history,
        ranges,
    })
}

/// Pages of signatures to read at most for one list: the deploy scan, or one range.
const MAX_PAGES: usize = 10;

/// Pages of `ProgramData` signatures to read at most for one page of the authority.
const WINDOW_PAGES: usize = 5;

/// Decode the successful program transactions between deploy `start` (exclusive) and deploy
/// `end` (inclusive), newest first. Agave runs new code from the slot after its deploy.
fn sample_range(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    program_id: &str,
    idls: &[Idl],
    start: Option<&Deploy>,
    end: Option<&Deploy>,
    limits: &TimelineLimits,
) -> Result<RangeResult, Error> {
    let mut result = RangeResult {
        first_slot: start.map(|d| d.slot.saturating_add(1)),
        last_slot: end.map(|d| d.slot),
        deploy_signature: start.map(|d| d.signature.clone()),
        samples: 0,
        transactions_read: 0,
        unreadable: 0,
        decoded: vec![0; idls.len()],
        first_error: vec![None; idls.len()],
    };
    let until = start.map(|d| d.signature.as_str());
    let mut before = end.map(|d| d.signature.clone());
    for _ in 0..MAX_PAGES {
        let page = signatures(rpc, program_id, before.as_deref(), until)?;
        // An RPC that ignores `before` would return the same page forever.
        match page.last() {
            Some(last) if before.as_deref() != Some(last.signature.as_str()) => {
                before = Some(last.signature.clone());
            }
            _ => break,
        }
        for info in &page {
            let slot = slot_of(info)?;
            // The page is newest first, so the rest of it is older than the range.
            if start.is_some_and(|d| slot <= d.slot) {
                return Ok(result);
            }
            if end.is_some_and(|d| slot > d.slot) || info.err.is_some() {
                continue;
            }
            if result.samples >= limits.per_range
                || result.transactions_read >= limits.max_transactions
            {
                return Ok(result);
            }
            let tx = transaction(rpc, &info.signature)?;
            result.transactions_read += 1;
            if tx.is_null() {
                result.unreadable += 1;
                continue;
            }
            let found = idls
                .iter()
                .map(|idl| samples_from_transaction(idl, program_id, &tx))
                .collect::<Result<Vec<_>, _>>()?;
            // A transaction can name the program as an account without a call to it.
            if found
                .iter()
                .all(|f| f.instructions.is_empty() && f.events.is_empty())
            {
                continue;
            }
            result.samples += 1;
            let per_idl = result.decoded.iter_mut().zip(&mut result.first_error);
            for (((count, first), idl), found) in per_idl.zip(idls).zip(found) {
                let samples = Samples {
                    program_id: Some(program_id.to_string()),
                    instructions: found.instructions,
                    events: found.events,
                    ..Samples::default()
                };
                // The rule of idl-verify itself: an IDL fits when no finding is an error.
                let report = verify(idl, &samples, None);
                match report
                    .findings
                    .into_iter()
                    .find(|f| f.severity == Severity::Error)
                {
                    None => *count += 1,
                    Some(f) => {
                        first.get_or_insert(FirstError {
                            check: f.check,
                            subject: f.subject,
                            message: f.message,
                            signature: info.signature.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(result)
}

/// The deploys of `program_id`, from the signatures of its upgrade authority (or the
/// `ProgramData` account, or for loader v4 the program account, when no authority can sign).
/// Reads at most `max_scan` transactions.
///
/// # Errors
///
/// When a request fails, or the RPC has no program account at `program_id`.
pub fn deploy_history(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    program_id: &str,
    max_scan: usize,
) -> Result<DeployHistory, Error> {
    let (loader, plan) = scan_plan(rpc, program_id)?;
    let mut history = DeployHistory {
        loader,
        listed: Vec::new(),
        deploys: Vec::new(),
        end: HistoryEnd::FixedCode,
        scanned: 0,
        unreadable: 0,
    };
    let Some(Plan {
        target,
        start,
        skip_after,
    }) = plan
    else {
        return Ok(history);
    };
    // Other programs can pass the ProgramData account in each call, so the scan lists the
    // authority, and reads only its transactions that the ProgramData account lists too.
    let follow = start != target;
    history.listed.push(start.clone());
    let mut address = start;
    let mut before: Option<String> = None;
    let mut pages = 0;
    // A window that takes too many pages turns the filter off for the rest of the scan.
    let mut filter = follow;
    let end = 'scan: loop {
        if pages >= MAX_PAGES {
            break HistoryEnd::ScanLimit;
        }
        pages += 1;
        let cursor = before.clone();
        let page = signatures(rpc, &address, before.as_deref(), None)?;
        match page.last() {
            Some(last) if before.as_deref() != Some(last.signature.as_str()) => {
                before = Some(last.signature.clone());
            }
            _ => break HistoryEnd::NoOlderSignatures,
        }
        // The authority also signs each buffer write of `solana program deploy`. The first
        // page has no cursor, so its window starts after its newest signature.
        let touched = match (filter && page.len() > 1, page.first(), page.last()) {
            (true, Some(newest), Some(oldest)) => {
                let from = cursor.as_deref().unwrap_or(&newest.signature);
                let found = window(rpc, &target, from, slot_of(oldest)?)?;
                filter = found.is_some();
                found
            }
            _ => None,
        };
        for (i, info) in page.iter().enumerate() {
            let slot = slot_of(info)?;
            // A failed transaction changed nothing, and no deploy is newer than the last write.
            if info.err.is_some() || skip_after.is_some_and(|last| slot > last) {
                continue;
            }
            let outside = touched
                .as_ref()
                .is_some_and(|t| !t.contains(&info.signature));
            if outside && (i > 0 || cursor.is_some()) {
                continue;
            }
            if history.scanned >= max_scan {
                break 'scan HistoryEnd::ScanLimit;
            }
            history.scanned += 1;
            let tx = transaction(rpc, &info.signature)?;
            if tx.is_null() {
                history.unreadable += 1;
                continue;
            }
            let mut previous = None;
            for change in changes(&transaction_record(&tx)?, &history.loader, &target) {
                match change {
                    Change::Deploy { first } => {
                        history.deploys.push(Deploy {
                            slot,
                            signature: info.signature.clone(),
                            first,
                        });
                        if first {
                            break 'scan HistoryEnd::FirstDeploy;
                        }
                    }
                    Change::Authority { from, to } => {
                        if follow && to.as_deref() == Some(address.as_str()) {
                            previous = Some(from);
                        }
                    }
                }
            }
            // The authority before this one signed the older deploys.
            if let Some(from) = previous {
                history.listed.push(from.clone());
                address = from;
                before = Some(info.signature.clone());
                continue 'scan;
            }
        }
    };
    history.end = end;
    history.deploys.reverse();
    Ok(history)
}

/// The signatures of `target` older than `before` back to slot `oldest`. `None` when that
/// takes more than [`WINDOW_PAGES`] pages; then the scan reads each transaction.
fn window(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    target: &str,
    before: &str,
    oldest: u64,
) -> Result<Option<HashSet<String>>, Error> {
    let mut found = HashSet::new();
    let mut before = before.to_string();
    for _ in 0..WINDOW_PAGES {
        let page = signatures(rpc, target, Some(&before), None)?;
        let Some(last) = page.last() else {
            return Ok(Some(found));
        };
        // An RPC that ignores `before` would return the same page forever.
        if last.signature == before {
            return Ok(None);
        }
        found.extend(page.iter().map(|info| info.signature.clone()));
        // The same slot can go on in the next page.
        if slot_of(last)? < oldest {
            return Ok(Some(found));
        }
        before.clone_from(&last.signature);
    }
    Ok(None)
}

/// Where the deploy scan starts. `target` is the account that the loader's instructions for
/// this program change. `skip_after` is the last write slot, when no authority change can be
/// newer.
struct Plan {
    target: String,
    start: String,
    skip_after: Option<u64>,
}

/// The owner of the program account, and the scan plan; `None` for a loader that cannot
/// replace the program's code.
fn scan_plan(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    program_id: &str,
) -> Result<(String, Option<Plan>), Error> {
    let Some((loader, data)) = first_bytes(rpc, program_id, 48)? else {
        return Err(Error::Usage(format!(
            "the RPC has no account {program_id}; check --program"
        )));
    };
    let base58 = |key: &[u8]| bs58::encode(key).into_string();
    let plan = match loader.as_str() {
        LOADER_V3 => {
            let programdata = program_data_address(&data).ok_or_else(|| {
                Error::Usage(format!(
                    "{program_id} is an upgradeable loader account but not a program; check --program"
                ))
            })?;
            // ProgramData: tag 3, the slot of the last write, then an `Option<Pubkey>` authority.
            let state = first_bytes(rpc, &programdata, 45)?
                .map(|(_, d)| d)
                .filter(|d| d.get(..4) == Some(&[3, 0, 0, 0]));
            let authority = state
                .as_deref()
                .filter(|d| d.get(12) == Some(&1))
                .and_then(|d| d.get(13..45))
                .map(base58);
            Plan {
                skip_after: authority
                    .is_none()
                    .then(|| state.as_deref().and_then(|d| u64_at(d, 4)))
                    .flatten(),
                start: authority.unwrap_or_else(|| programdata.clone()),
                target: programdata,
            }
        }
        LOADER_V4 => {
            // `LoaderV4State`: slot, authority (the next version once finalized), status.
            let finalized = u64_at(&data, 40) == Some(2);
            let authority = data.get(8..40).filter(|_| !finalized).map(base58);
            Plan {
                skip_after: authority.is_none().then(|| u64_at(&data, 0)).flatten(),
                start: authority.unwrap_or_else(|| program_id.to_string()),
                target: program_id.to_string(),
            }
        }
        _ => return Ok((loader, None)),
    };
    Ok((loader, Some(plan)))
}

enum Change {
    Deploy { first: bool },
    Authority { from: String, to: Option<String> },
}

/// The instructions in `record` of `loader` that change `target`: the `ProgramData` account
/// for the upgradeable loader, the program account for loader v4.
fn changes(record: &TransactionRecord, loader: &str, target: &str) -> Vec<Change> {
    let inner = record.inner.iter().flat_map(|g| &g.instructions);
    let all = record.instructions.iter().chain(inner);
    all.filter(|ix| ix.program == loader)
        .filter_map(|ix| {
            let at = |i: usize| ix.accounts.get(i).map(String::as_str);
            let authority = || {
                Some(Change::Authority {
                    from: at(1)?.to_string(),
                    to: at(2).map(str::to_string),
                })
            };
            // Both loaders start an instruction with a u32 tag.
            let tag = u32::from_le_bytes(*ix.data.first_chunk::<4>()?);
            match (loader, tag) {
                // DeployWithMaxDataLen, then Upgrade.
                (LOADER_V3, 2) if at(1) == Some(target) => Some(Change::Deploy { first: true }),
                (LOADER_V3 | LOADER_V4, 3) if at(0) == Some(target) => {
                    Some(Change::Deploy { first: false })
                }
                // SetAuthority, SetAuthorityChecked; loader v4 TransferAuthority.
                (LOADER_V3, 4 | 7) | (LOADER_V4, 5) if at(0) == Some(target) => authority(),
                _ => None,
            }
        })
        .collect()
}

/// One page of the signatures of `address`, newest first.
fn signatures(
    rpc: &mut impl FnMut(&str, Value) -> Result<Value, Error>,
    address: &str,
    before: Option<&str>,
    until: Option<&str>,
) -> Result<Vec<SignatureInfo>, Error> {
    let mut config = json!({ "limit": SIGNATURES_PER_PAGE });
    if let Some(signature) = before {
        config["before"] = json!(signature);
    }
    if let Some(signature) = until {
        config["until"] = json!(signature);
    }
    let page = rpc("getSignaturesForAddress", json!([address, config]))?;
    Vec::<SignatureInfo>::deserialize(&page)
        .map_err(|e| Error::RpcResponse(format!("getSignaturesForAddress: {e}")))
}

/// The ranges are slots, so a signature without one cannot be placed.
fn slot_of(info: &SignatureInfo) -> Result<u64, Error> {
    info.slot.ok_or_else(|| {
        Error::RpcResponse(format!(
            "getSignaturesForAddress: {} has no slot",
            escape_control(&info.signature)
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tx;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use std::collections::HashMap;

    fn key(byte: u8) -> String {
        bs58::encode([byte; 32]).into_string()
    }

    /// A fake cluster: accounts, the signatures of each address and the transactions. Add
    /// them newest first. As on the RPC, `before` and `until` can be any known signature.
    #[derive(Default)]
    struct Chain {
        accounts: HashMap<String, (String, Vec<u8>)>,
        history: HashMap<String, Vec<(String, u64, bool)>>,
        /// The order of each signature on the chain: a higher one is older.
        age: HashMap<String, usize>,
        txs: HashMap<String, Value>,
        fetched: Vec<String>,
        listed: Vec<String>,
    }

    impl Chain {
        fn rpc(&mut self, method: &str, params: &Value) -> Result<Value, Error> {
            Ok(match method {
                "getAccountInfo" => {
                    let len = params[1]["dataSlice"]["length"].as_u64().unwrap();
                    match self.accounts.get(params[0].as_str().unwrap()) {
                        Some((owner, data)) => {
                            let end = data.len().min(usize::try_from(len).unwrap());
                            let data = STANDARD.encode(&data[..end]);
                            json!({ "value": { "owner": owner, "data": [data, "base64"] } })
                        }
                        None => json!({ "value": null }),
                    }
                }
                "getSignaturesForAddress" => {
                    let address = params[0].as_str().unwrap();
                    self.listed.push(address.to_string());
                    let age = |name: &str| Some(self.age[params[1].get(name)?.as_str()?]);
                    let (before, until) = (age("before"), age("until"));
                    let limit = usize::try_from(params[1]["limit"].as_u64().unwrap()).unwrap();
                    let list = self.history.get(address).cloned().unwrap_or_default();
                    let page: Vec<Value> = list
                        .iter()
                        .filter(|(s, ..)| before.is_none_or(|b| self.age[s] > b))
                        .filter(|(s, ..)| until.is_none_or(|u| self.age[s] < u))
                        .take(limit)
                        .map(|(signature, slot, failed)| {
                            let err = failed.then(|| json!({ "InstructionError": [0, "Custom"] }));
                            json!({ "signature": signature, "slot": slot, "err": err })
                        })
                        .collect();
                    json!(page)
                }
                "getTransaction" => {
                    let signature = params[0].as_str().unwrap().to_string();
                    let tx = self.txs.get(&signature).cloned().unwrap_or(Value::Null);
                    self.fetched.push(signature);
                    tx
                }
                _ => return Err(Error::Rpc(format!("{method}: not in the fake"))),
            })
        }

        fn list(&mut self, address: &str, signature: &str, slot: u64, failed: bool) {
            let next = self.age.len();
            self.age.entry(signature.to_string()).or_insert(next);
            let list = self.history.entry(address.to_string()).or_default();
            list.push((signature.to_string(), slot, failed));
        }

        /// A transaction with one instruction to `program`, which gets every key in order.
        /// Each key lists it.
        fn add(&mut self, signature: &str, slot: u64, program: &str, keys: &[&str], data: &[u8]) {
            let accounts: Vec<usize> = (0..keys.len()).collect();
            let ix = json!({ "programIdIndex": keys.len(), "accounts": accounts,
                "data": bs58::encode(data).into_string() });
            self.insert(signature, slot, program, keys, &json!([ix]), &json!([]));
        }

        /// As [`Chain::add`], but `program` runs as a CPI of a multisig, as Squads does.
        fn add_cpi(
            &mut self,
            signature: &str,
            slot: u64,
            program: &str,
            keys: &[&str],
            data: &[u8],
        ) {
            let accounts: Vec<usize> = (0..keys.len()).collect();
            let outer =
                json!({ "programIdIndex": keys.len() + 1, "accounts": accounts, "data": "1" });
            let inner = json!([{ "index": 0, "instructions": [{ "programIdIndex": keys.len(),
                "accounts": accounts, "data": bs58::encode(data).into_string(), "stackHeight": 2 }] }]);
            let mut all = keys.to_vec();
            all.push(program);
            self.insert(signature, slot, "Multisig", &all, &json!([outer]), &inner);
        }

        fn insert(
            &mut self,
            signature: &str,
            slot: u64,
            program: &str,
            keys: &[&str],
            ixs: &Value,
            inner: &Value,
        ) {
            let mut all = keys.to_vec();
            all.push(program);
            let mut tx = tx(&all, ixs);
            tx["transaction"]["signatures"] = json!([signature]);
            tx["meta"]["innerInstructions"] = inner.clone();
            self.txs.insert(signature.to_string(), tx);
            for address in &all {
                self.list(address, signature, slot, false);
            }
        }

        /// A successful call of `Prog` with `data`.
        fn call(&mut self, signature: &str, slot: u64, data: &[u8]) {
            self.add(signature, slot, "Prog", &["Payer"], data);
        }

        /// An upgradeable program `Prog` with `ProgramData` `key(7)`, last written in `slot`.
        fn upgradeable(slot: u64, authority: Option<&str>) -> Self {
            let mut chain = Self::default();
            let program = [&[2, 0, 0, 0][..], &[7; 32]].concat();
            let mut data = [&[3, 0, 0, 0][..], &slot.to_le_bytes()].concat();
            match authority {
                Some(a) => {
                    data.push(1);
                    data.extend(bs58::decode(a).into_vec().unwrap());
                }
                None => data.push(0),
            }
            let accounts = &mut chain.accounts;
            accounts.insert("Prog".into(), (LOADER_V3.into(), program));
            accounts.insert(key(7), (LOADER_V3.into(), data));
            chain
        }
    }

    fn upgrade(authority: &str) -> Vec<String> {
        let keys = [
            &key(7),
            "Prog",
            "Buffer",
            "Spill",
            "Rent",
            "Clock",
            authority,
        ];
        keys.map(str::to_string).to_vec()
    }

    fn first_deploy(authority: &str) -> Vec<String> {
        let keys = [
            authority,
            &key(7),
            "Prog",
            "Buffer",
            "Rent",
            "Clock",
            "System",
            authority,
        ];
        keys.map(str::to_string).to_vec()
    }

    fn refs(keys: &[String]) -> Vec<&str> {
        keys.iter().map(String::as_str).collect()
    }

    /// `a` knows instruction 1; `ab` knows 1 and 2.
    fn idls() -> Vec<Idl> {
        let a = r#""instructions":[{"name":"a","discriminator":[1]}]"#;
        let ab =
            r#""instructions":[{"name":"a","discriminator":[1]},{"name":"b","discriminator":[2]}]"#;
        vec![crate::testing::idl(a), crate::testing::idl(ab)]
    }

    /// First slot, last slot, `[read, unread, samples]` and the decoded counts of a range.
    type Counts = (Option<u64>, Option<u64>, [usize; 3], Vec<usize>);

    fn counts(t: &Timeline) -> Vec<Counts> {
        let r = t.ranges.iter();
        r.map(|r| {
            let read = [r.transactions_read, r.unreadable, r.samples];
            (r.first_slot, r.last_slot, read, r.decoded.clone())
        })
        .collect()
    }

    fn deploy(slot: u64, signature: &str, first: bool) -> Deploy {
        Deploy {
            slot,
            signature: signature.into(),
            first,
        }
    }

    fn run(chain: &mut Chain, idls: &[Idl], progress: &mut Vec<String>) -> Timeline {
        let rpc = |m: &str, p: Value| chain.rpc(m, &p);
        let limits = TimelineLimits::default();
        timeline(rpc, "Prog", idls, &limits, |p| progress.push(p.to_string())).unwrap()
    }

    /// Two upgrades and an extend, signed by `key(5)`, and history that ends before the
    /// first deploy. The middle range has no program transaction.
    fn two_upgrades() -> Chain {
        let auth = key(5);
        let mut chain = Chain::upgradeable(300, Some(&auth));
        chain.call("P4", 310, &[2]);
        // The RPC no longer keeps it.
        chain.list("Prog", "PX", 306, false);
        chain.call("P3", 305, &[1]);
        chain.list("Prog", "PF", 302, true);
        // An extend writes the ProgramData slot too, but keeps the code.
        let extend = [key(7), "Prog".into(), "System".into(), auth.clone()];
        chain.add(
            "E",
            300,
            LOADER_V3,
            &refs(&extend),
            &[6, 0, 0, 0, 0, 1, 0, 0],
        );
        // A failed upgrade changed nothing.
        chain.list(&auth, "F", 250, true);
        chain.add_cpi("U2", 200, LOADER_V3, &refs(&upgrade(&auth)), &[3, 0, 0, 0]);
        chain.add("U1", 100, LOADER_V3, &refs(&upgrade(&auth)), &[3, 0, 0, 0]);
        chain.call("P2", 90, &[1]);
        chain.call("P1", 80, &[9]);
        chain
    }

    #[test]
    fn upgrades_split_the_history_into_ranges_and_an_extend_does_not() {
        let mut chain = two_upgrades();
        let mut progress = Vec::new();
        let t = run(&mut chain, &idls(), &mut progress);
        assert_eq!(
            t.history.deploys,
            [deploy(100, "U1", false), deploy(200, "U2", false)]
        );
        assert_eq!(t.history.end, HistoryEnd::NoOlderSignatures);
        assert_eq!(t.history.listed, [key(5)]);
        // The extend lists the program too, but calls only the loader.
        assert_eq!(
            counts(&t),
            [
                (None, Some(100), [2, 0, 2], vec![1, 1]),
                (Some(101), Some(200), [0, 0, 0], vec![0, 0]),
                (Some(201), None, [4, 1, 2], vec![1, 2]),
            ]
        );
        let fitting: Vec<Vec<usize>> = t.ranges.iter().map(RangeResult::fitting).collect();
        assert_eq!(fitting, [vec![], vec![], vec![1]]);
        assert_eq!((t.unfit(), t.exit_code()), (1, 1));
        // Failed transactions are never read.
        assert_eq!(
            chain.fetched,
            ["E", "U2", "U1", "P2", "P1", "P4", "PX", "P3", "E"]
        );
        assert_eq!(progress.len(), 4, "{progress:?}");
        assert_eq!(
            progress[1],
            "range 1 of 3, slots ? to 100: 2 samples in 2 transactions read"
        );
    }

    #[test]
    fn an_unfit_range_names_each_best_idl_and_its_first_error() {
        let mut chain = two_upgrades();
        let t = run(&mut chain, &idls(), &mut Vec::new());
        let first = &t.ranges[0];
        // Both decode 1 of 2, so both are the best.
        assert_eq!(first.best(), [0, 1]);
        let error = first.first_error[0].as_ref().unwrap();
        assert_eq!(
            (
                error.check,
                error.subject.as_str(),
                error.signature.as_str()
            ),
            (Check::Discriminator, "instructions", "P1")
        );
        assert_eq!(t.ranges[2].first_error[0].as_ref().unwrap().signature, "P4");
        assert!(t.ranges[2].first_error[1].is_none());

        let labels = ["a.json".to_string(), "ab.json".to_string()];
        let mut out = Vec::new();
        t.write_human(&mut out, &labels).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("error: slots ? to 100: no IDL decodes all 2 samples; IDL 1 and IDL 2 decode the most, 1; the range can span several deploys\n  IDL 1: discriminator instructions: "),
            "{text}"
        );
        let json: Value = serde_json::from_str(&t.to_json(&labels).unwrap()).unwrap();
        assert_eq!(json["ranges"][0]["best"], json!(["a.json", "ab.json"]));
        assert_eq!(
            json["ranges"][0]["results"][0]["first_error"]["signature"],
            "P1"
        );
        assert_eq!(json["ranges"][2]["unreadable"], 1);
    }

    #[test]
    fn the_scan_stops_at_its_limit() {
        let mut chain = two_upgrades();
        let rpc = &mut |m: &str, p: Value| chain.rpc(m, &p);
        let history = deploy_history(rpc, "Prog", 1).unwrap();
        assert_eq!((history.end, history.scanned), (HistoryEnd::ScanLimit, 1));
        assert!(history.deploys.is_empty());
    }

    /// `solana program deploy` writes the buffer with the upgrade authority, so the authority
    /// signs many writes before each upgrade.
    #[test]
    fn buffer_writes_of_the_authority_do_not_use_up_the_scan() {
        let auth = key(5);
        let mut chain = Chain::upgradeable(4000, Some(&auth));
        let writes = |chain: &mut Chain, name: &str, top: u64, count: u64| {
            for i in 0..count {
                let write = [&[1, 0, 0, 0][..], &[0; 8]].concat();
                let signature = format!("{name}{i}");
                chain.add(&signature, top - i, LOADER_V3, &[name, &auth], &write);
            }
        };
        chain.call("P3", 4500, &[1, 5]);
        chain.add("U2", 4000, LOADER_V3, &refs(&upgrade(&auth)), &[3, 0, 0, 0]);
        writes(&mut chain, "W", 3999, 150);
        chain.call("P2", 3000, &[1, 5]);
        chain.add("U1", 2000, LOADER_V3, &refs(&upgrade(&auth)), &[3, 0, 0, 0]);
        // Enough writes that the second page of the authority starts with one.
        writes(&mut chain, "V", 1999, 1200);
        chain.call("P1", 45, &[1]);
        let first = first_deploy(&auth);
        chain.add("D", 40, LOADER_V3, &refs(&first), &[2, 0, 0, 0]);

        // `a` takes no argument, `a8` one u8: each fits one code version only.
        let a8 = r#""instructions":[{"name":"a","discriminator":[1],"args":[{"name":"x","type":"u8"}]}]"#;
        let idls = [idls().remove(0), crate::testing::idl(a8)];
        let t = run(&mut chain, &idls, &mut Vec::new());
        let deploys = [
            deploy(40, "D", true),
            deploy(2000, "U1", false),
            deploy(4000, "U2", false),
        ];
        assert_eq!(t.history.deploys, deploys);
        assert_eq!(chain.fetched[..3], ["U2", "U1", "D"]);
        assert_eq!(
            (t.history.end, t.history.scanned),
            (HistoryEnd::FirstDeploy, 3)
        );
        let fitting: Vec<Vec<usize>> = t.ranges.iter().map(RangeResult::fitting).collect();
        assert_eq!(fitting, [vec![0], vec![1], vec![1]]);
        assert_eq!(t.exit_code(), 0);
    }

    #[test]
    fn a_busy_programdata_account_turns_the_window_off_for_the_rest_of_the_scan() {
        // Other programs pass the ProgramData account in the slots of the first authority
        // page, so its window needs too many pages; the second page has no window.
        let auth = key(5);
        let mut chain = Chain::upgradeable(5000, Some(&auth));
        chain.add("U2", 5000, LOADER_V3, &refs(&upgrade(&auth)), &[3, 0, 0, 0]);
        for i in 0..1000 {
            chain.add(&format!("A{i}"), 4999 - i, "Other", &[&auth], &[1]);
            if i == 500 {
                for n in 0..6 * SIGNATURES_PER_PAGE {
                    chain.add(&format!("N{n}"), 4499, "Other", &[&key(7)], &[1]);
                }
            }
        }
        chain.add("U1", 2000, LOADER_V3, &refs(&upgrade(&auth)), &[3, 0, 0, 0]);
        let first = first_deploy(&auth);
        chain.add("D", 40, LOADER_V3, &refs(&first), &[2, 0, 0, 0]);

        let rpc = &mut |m: &str, p: Value| chain.rpc(m, &p);
        let history = deploy_history(rpc, "Prog", 2000).unwrap();
        assert_eq!(history.deploys.len(), 3);
        assert_eq!(history.scanned, 1003);
        let windows = chain.listed.iter().filter(|a| **a == key(7)).count();
        assert_eq!(windows, WINDOW_PAGES);
    }

    #[test]
    fn follows_an_authority_change_back_to_the_first_deploy() {
        let (new, old) = (key(5), key(6));
        let mut chain = Chain::upgradeable(200, Some(&new));
        chain.call("P2", 210, &[1]);
        chain.add("U2", 200, LOADER_V3, &refs(&upgrade(&new)), &[3, 0, 0, 0]);
        let handover = [key(7), old.clone(), new.clone()];
        chain.add("H", 150, LOADER_V3, &refs(&handover), &[4, 0, 0, 0]);
        chain.call("P1", 120, &[1]);
        chain.add(
            "D",
            100,
            LOADER_V3,
            &refs(&first_deploy(&old)),
            &[2, 0, 0, 0],
        );
        chain.list(&old, "Z", 50, false);

        let t = run(&mut chain, &idls(), &mut Vec::new());
        assert_eq!(
            t.history.deploys,
            [deploy(100, "D", true), deploy(200, "U2", false)]
        );
        assert_eq!(t.history.end, HistoryEnd::FirstDeploy);
        assert_eq!(t.history.listed, [new, old]);
        assert_eq!(
            counts(&t),
            [
                (Some(101), Some(200), [1, 0, 1], vec![1, 1]),
                (Some(201), None, [1, 0, 1], vec![1, 1]),
            ]
        );
        assert_eq!(t.exit_code(), 0);
        assert!(!chain.fetched.contains(&"Z".to_string()));
    }

    #[test]
    fn loader_v4_deploys_and_a_retract_that_is_not_one() {
        let auth = key(5);
        let mut chain = Chain::default();
        let state = [
            &200u64.to_le_bytes()[..],
            &[5; 32],
            &1u64.to_le_bytes(),
            b"\x7fELF",
        ]
        .concat();
        chain
            .accounts
            .insert("Prog".into(), (LOADER_V4.into(), state));
        chain.add("D2", 200, LOADER_V4, &["Prog", &auth], &[3, 0, 0, 0]);
        chain.add("R", 150, LOADER_V4, &["Prog", &auth], &[4, 0, 0, 0]);
        chain.add("D1", 100, LOADER_V4, &["Prog", &auth], &[3, 0, 0, 0]);

        let rpc = &mut |m: &str, p: Value| chain.rpc(m, &p);
        let history = deploy_history(rpc, "Prog", 100).unwrap();
        assert_eq!(
            history.deploys,
            [deploy(100, "D1", false), deploy(200, "D2", false)]
        );
        assert_eq!(history.end, HistoryEnd::NoOlderSignatures);
        assert_eq!(history.listed, [auth]);
    }

    #[test]
    fn without_an_authority_the_scan_lists_programdata_and_skips_newer_writes() {
        let mut chain = Chain::upgradeable(200, None);
        chain.add("N", 250, "Other", &[&key(7)], &[1]);
        chain.add("U", 200, LOADER_V3, &refs(&upgrade("Auth")), &[3, 0, 0, 0]);
        chain.add(
            "D",
            100,
            LOADER_V3,
            &refs(&first_deploy("Auth")),
            &[2, 0, 0, 0],
        );

        let rpc = &mut |m: &str, p: Value| chain.rpc(m, &p);
        let history = deploy_history(rpc, "Prog", 100).unwrap();
        assert_eq!(
            history.deploys,
            [deploy(100, "D", true), deploy(200, "U", false)]
        );
        assert_eq!(history.listed, [key(7)]);
        assert_eq!(chain.fetched, ["U", "D"]);
    }

    #[test]
    fn a_loader_that_cannot_replace_code_gives_one_range_with_no_scan() {
        let mut chain = Chain::default();
        let v2 = "BPFLoader2111111111111111111111111111111111";
        chain
            .accounts
            .insert("Prog".into(), (v2.into(), b"\x7fELF".to_vec()));
        let rpc = &mut |m: &str, p: Value| chain.rpc(m, &p);
        let history = deploy_history(rpc, "Prog", 100).unwrap();
        assert_eq!(
            (history.end, history.deploys.len()),
            (HistoryEnd::FixedCode, 0)
        );
        assert!(chain.listed.is_empty());
    }
}
