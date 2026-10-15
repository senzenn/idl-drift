//! Verification results, and their human and JSON renderings.

use std::fmt::Write as _;
use std::io::{self, Write};

use owo_colors::OwoColorize;
use serde::Serialize;

use crate::text::plural;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Context, never a failure.
    Note,
    /// Worth a look; may be legitimate, such as spare allocation.
    Warning,
    /// The IDL does not match the data. Sets exit code 1.
    Error,
}

/// The check that produced a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    ProgramId,
    Discriminator,
    MissingType,
    Decode,
    BytesUsed,
    Layout,
    SizeDistribution,
    AccountCount,
    FixedAddress,
    /// An account the IDL marks writable or signer gets a key without that privilege.
    AccountFlags,
    /// An account that the IDL derives from seeds gets a key that the seeds do not give.
    Pda,
    /// A self-CPI event sample without the envelope the IDL (or Anchor) puts before events.
    Envelope,
    Reference,
    /// Too few of the IDL's items have a sample, with a required minimum set.
    Coverage,
    /// With `--upgrade-impact`: an account breaks under the new IDL, needs a realloc, or
    /// holds bytes after the data that the new IDL reads.
    UpgradeImpact,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub check: Check,
    /// What the finding is about: `account Vault`, `instruction deposit`, `event Swap` or
    /// `program`.
    pub subject: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Address or signature of one affected sample, to look up on an explorer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramInfo {
    pub name: String,
    pub version: String,
    pub address: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SizeCount {
    pub size: usize,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountSummary {
    pub name: String,
    pub samples: usize,
    /// `borsh`, `zero-copy repr(C)`, ...
    pub layout: String,
    /// Observed data sizes, discriminator included.
    pub sizes: Vec<SizeCount>,
    /// Bytes the decoder used, discriminator included; `None` when nothing decoded.
    pub bytes_used: Option<Range>,
    /// Size the IDL implies for a fixed-size type, discriminator included.
    pub expected_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstructionSummary {
    pub name: String,
    pub samples: usize,
    pub idl_accounts: usize,
    pub sample_accounts: Range,
    pub data_sizes: Range,
}

/// How many samples of an event came from each source.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct EventSources {
    /// `Program data:` log lines.
    pub log: usize,
    /// Self-CPI event instructions.
    pub cpi: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventSummary {
    pub name: String,
    pub samples: usize,
    pub sources: EventSources,
    /// Observed sizes, discriminator included, self-CPI envelope excluded.
    pub sizes: Vec<SizeCount>,
    /// Bytes the decoder used, discriminator included; `None` when nothing decoded.
    pub bytes_used: Option<Range>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Range<T = usize> {
    pub min: T,
    pub max: T,
}

impl<T: Copy + Ord> Range<T> {
    pub(crate) fn of(values: impl IntoIterator<Item = T>) -> Option<Self> {
        values.into_iter().fold(None, |acc, v| {
            Some(match acc {
                None => Self { min: v, max: v },
                Some(r) => Self {
                    min: r.min.min(v),
                    max: r.max.max(v),
                },
            })
        })
    }
}

impl<T: std::fmt::Display + PartialEq> std::fmt::Display for Range<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{}-{}", self.min, self.max)
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Coverage {
    pub sampled: usize,
    pub classified: usize,
    /// Types, instructions or events in the IDL that no sample exercises.
    pub not_sampled: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReferenceSummary {
    pub accounts_compared: usize,
    pub accounts_matched: usize,
    pub instructions_compared: usize,
    pub instructions_matched: usize,
    pub events_compared: usize,
    pub events_matched: usize,
    /// Samples not compared, such as zero-copy types with padding.
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub program: ProgramInfo,
    pub samples_program_id: Option<String>,
    pub account_coverage: Coverage,
    pub accounts: Vec<AccountSummary>,
    pub instruction_coverage: Coverage,
    pub instructions: Vec<InstructionSummary>,
    pub event_coverage: Coverage,
    pub events: Vec<EventSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<ReferenceSummary>,
    /// How a live run sampled; absent for a samples file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingSummary>,
    /// Only with `--upgrade-impact`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_impact: Option<UpgradeImpact>,
    pub findings: Vec<Finding>,
}

/// What a live `--rpc` run read before it stopped.
#[derive(Debug, Clone, Serialize)]
pub struct SamplingSummary {
    /// Successful transactions read.
    pub transactions: usize,
    pub stop_reason: String,
}

/// How a new IDL reads the sampled accounts. The counts are of the sample, not of every
/// account of the program.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradeImpact {
    pub rent_source: RentSource,
    /// The rate of the offline estimate; `None` when the cluster gave the minimums.
    pub lamports_per_byte: Option<u64>,
    pub types: Vec<TypeImpact>,
    /// Accounts whose discriminator matches no account type in the new IDL. They count as
    /// broken in `totals`.
    pub unknown: usize,
    pub totals: ImpactTotals,
}

/// Where the rent-exempt minimums come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RentSource {
    /// `getMinimumBalanceForRentExemption` on the sampled cluster.
    Rpc,
    /// `(128 + size) * lamports_per_byte`, with no request.
    Formula,
}

/// The accounts of one type in each class.
#[derive(Debug, Clone, Serialize)]
pub struct TypeImpact {
    pub name: String,
    pub sampled: usize,
    /// Decodes, and the new IDL reads every byte.
    pub ok: usize,
    pub shorter: ShorterAccounts,
    pub realloc: ReallocAccounts,
    pub broken: BrokenAccounts,
}

/// Accounts that decode with bytes left over.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ShorterAccounts {
    pub count: usize,
    /// Bytes after the data that the new IDL reads; `None` when `count` is 0.
    pub bytes: Option<Range>,
}

/// Accounts that decode only with zero bytes added at the end.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReallocAccounts {
    pub count: usize,
    /// The fewest bytes to add to each account; `None` when `count` is 0.
    pub bytes: Option<Range>,
    /// Lamports of rent to add to each account; `None` when `count` is 0.
    pub rent_each: Option<Range<u64>>,
    pub rent_total: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BrokenAccounts {
    pub count: usize,
    pub reasons: Vec<BrokenReason>,
}

/// Broken accounts that share a reason.
#[derive(Debug, Clone, Serialize)]
pub struct BrokenReason {
    /// Such as `` `amount` does not decode ``.
    pub reason: String,
    pub count: usize,
    /// The full error or the two values, for `example`.
    pub detail: String,
    /// Address of one such account.
    pub example: String,
}

/// The classes summed over all types, `unknown` included in `sampled` and `broken`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImpactTotals {
    pub sampled: usize,
    pub ok: usize,
    pub shorter: usize,
    pub realloc: usize,
    pub broken: usize,
    pub rent_total: u64,
}

impl Report {
    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .count()
    }

    /// Add an error for each kind of IDL item (account types, instructions, events) where
    /// less than `min_percent` of the items have a sample: a pass must not rest on little data.
    pub fn require_coverage(&mut self, min_percent: u8) {
        let kinds = [
            ("account types", self.accounts.len(), &self.account_coverage),
            (
                "instructions",
                self.instructions.len(),
                &self.instruction_coverage,
            ),
            ("events", self.events.len(), &self.event_coverage),
        ];
        let mut short = Vec::new();
        for (what, sampled, coverage) in kinds {
            let total = sampled + coverage.not_sampled.len();
            if total == 0
                || sampled.saturating_mul(100) >= usize::from(min_percent).saturating_mul(total)
            {
                continue;
            }
            short.push(Finding {
                severity: Severity::Error,
                check: Check::Coverage,
                subject: "program".to_string(),
                message: format!(
                    "only {sampled} of {total} {what} have a sample ({}%), less than the {min_percent}% required",
                    sampled.saturating_mul(100) / total
                ),
                hint: Some(
                    "with --rpc, raise --max-tx and --stall to read more transactions; else add samples of the items under \"not sampled\"".to_string(),
                ),
                example: None,
            });
        }
        self.findings.extend(short);
    }

    /// `true` when any finding is at `threshold` or above, for a stricter CI gate.
    #[must_use]
    pub fn fails_at(&self, threshold: Severity) -> bool {
        self.findings.iter().any(|f| f.severity >= threshold)
    }

    /// 1 when any finding is an error, else 0. (2, for I/O and parse errors, is the CLI's.)
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        u8::from(self.fails_at(Severity::Error))
    }

    /// Pretty-printed JSON, with counts and the exit code added.
    ///
    /// # Errors
    ///
    /// Only if serialization fails, which plain data like this does not.
    pub fn to_json(&self) -> serde_json::Result<String> {
        self.to_json_with_exit_code(self.exit_code())
    }

    /// As [`Self::to_json`], with the exit code a stricter gate gave.
    ///
    /// # Errors
    ///
    /// As [`Self::to_json`].
    pub fn to_json_with_exit_code(&self, exit_code: u8) -> serde_json::Result<String> {
        #[derive(Serialize)]
        struct Counts {
            errors: usize,
            warnings: usize,
            notes: usize,
        }
        #[derive(Serialize)]
        struct View<'a> {
            #[serde(flatten)]
            report: &'a Report,
            counts: Counts,
            exit_code: u8,
        }
        serde_json::to_string_pretty(&View {
            report: self,
            counts: Counts {
                errors: self.count(Severity::Error),
                warnings: self.count(Severity::Warning),
                notes: self.count(Severity::Note),
            },
            exit_code,
        })
    }

    /// The human report. `color` adds ANSI colors.
    ///
    /// # Errors
    ///
    /// When writing to `out` fails.
    pub fn write_human(&self, out: &mut impl Write, color: bool) -> io::Result<()> {
        let p = &self.program;
        writeln!(out, "{} {}  {}", p.name, p.version, p.address)?;
        if let Some(s) = &self.sampling {
            writeln!(
                out,
                "sampled {} transactions; stopped because {}",
                s.transactions, s.stop_reason
            )?;
        }
        self.write_tables(out)?;

        let unsampled_types = self.account_coverage.not_sampled.len();
        let unsampled_ixs = self.instruction_coverage.not_sampled.len();
        let unsampled_events = self.event_coverage.not_sampled.len();
        if let Some(impact) = &self.upgrade_impact {
            write_upgrade_impact(out, impact)?;
            if unsampled_types > 0 {
                writeln!(
                    out,
                    "\nnot sampled: {unsampled_types} of {} account types",
                    unsampled_types + self.accounts.len()
                )?;
            }
        } else if unsampled_types + unsampled_ixs + unsampled_events > 0 {
            write!(
                out,
                "\nnot sampled: {unsampled_types} of {} account types, {unsampled_ixs} of {} instructions",
                unsampled_types + self.accounts.len(),
                unsampled_ixs + self.instructions.len(),
            )?;
            let events = unsampled_events + self.events.len();
            if events > 0 {
                write!(out, ", {unsampled_events} of {events} events")?;
            }
            writeln!(out)?;
        }

        if let Some(r) = &self.reference {
            write!(
                out,
                "\nreference: {} of {} accounts and {} of {} instructions match",
                r.accounts_matched,
                r.accounts_compared,
                r.instructions_matched,
                r.instructions_compared
            )?;
            if r.events_compared > 0 {
                write!(
                    out,
                    ", {} of {} events match",
                    r.events_matched, r.events_compared
                )?;
            }
            if r.skipped > 0 {
                write!(out, " ({} samples skipped)", r.skipped)?;
            }
            writeln!(out)?;
        }

        self.write_findings(out, color)?;
        let count = |severity, one, many| {
            let n = self.count(severity);
            format!("{n} {}", plural(n, one, many))
        };
        writeln!(
            out,
            "\nsummary: {}, {}, {}",
            count(Severity::Error, "error", "errors"),
            count(Severity::Warning, "warning", "warnings"),
            count(Severity::Note, "note", "notes")
        )
    }

    fn write_tables(&self, out: &mut impl Write) -> io::Result<()> {
        let ac = &self.account_coverage;
        writeln!(
            out,
            "\naccounts: {} sampled, {} classified",
            ac.sampled, ac.classified
        )?;
        if !self.accounts.is_empty() {
            let rows: Vec<[String; 5]> = self
                .accounts
                .iter()
                .map(|a| {
                    [
                        a.name.clone(),
                        a.samples.to_string(),
                        sizes_cell(&a.sizes),
                        a.bytes_used
                            .map_or_else(|| "-".to_string(), |r| r.to_string()),
                        a.layout.clone(),
                    ]
                })
                .collect();
            write_table(out, ["type", "samples", "size", "used", "layout"], &rows)?;
        }
        // The sampled instructions and events are for the current program, not the new IDL.
        if self.upgrade_impact.is_some() {
            return Ok(());
        }

        let ic = &self.instruction_coverage;
        writeln!(
            out,
            "\ninstructions: {} sampled, {} classified",
            ic.sampled, ic.classified
        )?;
        if !self.instructions.is_empty() {
            let rows: Vec<[String; 5]> = self
                .instructions
                .iter()
                .map(|i| {
                    [
                        i.name.clone(),
                        i.samples.to_string(),
                        i.sample_accounts.to_string(),
                        i.idl_accounts.to_string(),
                        i.data_sizes.to_string(),
                    ]
                })
                .collect();
            write_table(
                out,
                ["name", "samples", "accounts", "idl accounts", "data"],
                &rows,
            )?;
        }

        let ec = &self.event_coverage;
        writeln!(
            out,
            "\nevents: {} sampled, {} classified",
            ec.sampled, ec.classified
        )?;
        if !self.events.is_empty() {
            let rows: Vec<[String; 6]> = self
                .events
                .iter()
                .map(|e| {
                    [
                        e.name.clone(),
                        e.samples.to_string(),
                        e.sources.log.to_string(),
                        e.sources.cpi.to_string(),
                        sizes_cell(&e.sizes),
                        e.bytes_used
                            .map_or_else(|| "-".to_string(), |r| r.to_string()),
                    ]
                })
                .collect();
            write_table(
                out,
                ["name", "samples", "log", "cpi", "size", "used"],
                &rows,
            )?;
        }
        Ok(())
    }

    /// Findings, errors first.
    fn write_findings(&self, out: &mut impl Write, color: bool) -> io::Result<()> {
        if !self.findings.is_empty() {
            writeln!(out)?;
        }
        let mut findings: Vec<&Finding> = self.findings.iter().collect();
        findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
        for f in findings {
            let label = match f.severity {
                Severity::Error => paint("error", color, |s| s.red().bold().to_string()),
                Severity::Warning => paint("warning", color, |s| s.yellow().bold().to_string()),
                Severity::Note => paint("note", color, |s| s.cyan().to_string()),
            };
            writeln!(out, "{label}: {}: {}", f.subject, f.message)?;
            if let Some(hint) = &f.hint {
                writeln!(out, "  hint: {hint}")?;
            }
            if let Some(example) = &f.example {
                writeln!(out, "  example: {example}")?;
            }
        }
        Ok(())
    }
}

fn write_upgrade_impact(out: &mut impl Write, impact: &UpgradeImpact) -> io::Result<()> {
    writeln!(
        out,
        "\nupgrade impact under the new IDL (sample counts, not all accounts of the program):"
    )?;
    let row = |name: &str, counts: [usize; 5], bytes: Option<String>, rent: Option<String>| {
        let [sampled, ok, shorter, broken, realloc] = counts.map(|n| n.to_string());
        let dash = |cell: Option<String>| cell.unwrap_or_else(|| "-".to_string());
        let name = name.to_string();
        [
            name,
            sampled,
            ok,
            shorter,
            broken,
            realloc,
            dash(bytes),
            dash(rent),
        ]
    };
    let mut rows: Vec<[String; 8]> = impact
        .types
        .iter()
        .map(|t| {
            row(
                &t.name,
                [
                    t.sampled,
                    t.ok,
                    t.shorter.count,
                    t.broken.count,
                    t.realloc.count,
                ],
                t.realloc.bytes.map(|r| r.to_string()),
                t.realloc.rent_each.map(|r| r.to_string()),
            )
        })
        .collect();
    if impact.unknown > 0 {
        let n = impact.unknown;
        rows.push(row("(unknown type)", [n, 0, 0, n, 0], None, None));
    }
    let t = &impact.totals;
    let counts = [t.sampled, t.ok, t.shorter, t.broken, t.realloc];
    rows.push(row(
        "total",
        counts,
        Some(String::new()),
        Some(String::new()),
    ));
    write_table(
        out,
        [
            "type",
            "sampled",
            "ok",
            "shorter",
            "broken",
            "realloc",
            "bytes",
            "rent each",
        ],
        &rows,
    )?;
    if t.realloc == 0 {
        return Ok(());
    }
    let accounts = plural(t.realloc, "account that needs", "accounts that need");
    match impact.lamports_per_byte {
        None => writeln!(
            out,
            "extra rent for the {} {accounts} a realloc: {} lamports, from getMinimumBalanceForRentExemption",
            t.realloc, t.rent_total
        ),
        Some(rate) => writeln!(
            out,
            "extra rent for the {} {accounts} a realloc: {} lamports, estimated offline as (128 + size) * {rate} lamports\n  {rate} lamports per byte is the mainnet rate since the SIMD-0437 feature set_lamports_per_byte_to_5080. A later SIMD-0437 step can lower it. A live run (--rpc) gets the exact value from the cluster.",
            t.realloc, t.rent_total
        ),
    }
}

fn paint(text: &str, color: bool, style: impl Fn(&str) -> String) -> String {
    if color {
        style(text)
    } else {
        text.to_string()
    }
}

fn sizes_cell(sizes: &[SizeCount]) -> String {
    match sizes {
        [] => "-".to_string(),
        [one] => one.size.to_string(),
        many => {
            let mut out = String::new();
            for (i, s) in many.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{}x{}", s.size, s.count);
            }
            out
        }
    }
}

fn write_table<const N: usize>(
    out: &mut impl Write,
    header: [&str; N],
    rows: &[[String; N]],
) -> io::Result<()> {
    let mut widths = header.map(str::len);
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.len());
        }
    }
    let line = |cells: [&str; N]| {
        let mut s = String::from(" ");
        for (i, (cell, w)) in cells.iter().zip(widths).enumerate() {
            // Name left-aligned, numbers right-aligned, the last column ragged.
            if i == 0 {
                let _ = write!(s, " {cell:<w$}");
            } else if i == N - 1 {
                let _ = write!(s, "  {cell}");
            } else {
                let _ = write!(s, "  {cell:>w$}");
            }
        }
        s.truncate(s.trim_end().len());
        s
    };
    writeln!(out, "{}", line(header))?;
    for row in rows {
        writeln!(out, "{}", line(row.each_ref().map(String::as_str)))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::{InstructionSample, Samples};
    use crate::testing::idl;

    #[test]
    fn low_coverage_is_an_error_only_below_the_minimum() {
        // Two instructions, one sampled: 50%. The IDL has no account types or events.
        let idl = idl(r#""instructions":[
                {"name":"a","discriminator":[1],"accounts":[],"args":[]},
                {"name":"b","discriminator":[2],"accounts":[],"args":[]}]"#);
        let samples = Samples {
            instructions: vec![InstructionSample {
                signature: "S".into(),
                data: vec![1],
                accounts: Vec::new(),
                flags: None,
            }],
            ..Samples::default()
        };
        let report = crate::verify(&idl, &samples, None);
        assert_eq!(report.exit_code(), 0);

        let mut enough = report.clone();
        enough.require_coverage(50);
        assert_eq!(enough.exit_code(), 0);

        let mut short = report;
        short.require_coverage(51);
        assert_eq!(short.exit_code(), 1);
        let finding = short.findings.last().unwrap();
        assert_eq!(finding.check, Check::Coverage);
        assert_eq!(
            finding.message,
            "only 1 of 2 instructions have a sample (50%), less than the 51% required"
        );
    }
}
