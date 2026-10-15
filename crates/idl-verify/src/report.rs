//! Verification results, and their human and JSON renderings.

use std::fmt::Write as _;
use std::io::{self, Write};

use owo_colors::OwoColorize;
use serde::Serialize;

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
    /// A self-CPI event sample without the envelope the IDL (or Anchor) puts before events.
    Envelope,
    Reference,
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
pub struct Range {
    pub min: usize,
    pub max: usize,
}

impl Range {
    pub(crate) fn of(values: impl IntoIterator<Item = usize>) -> Option<Self> {
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

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{}-{}", self.min, self.max)
        }
    }
}

#[derive(Debug, Clone, Serialize)]
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
    pub findings: Vec<Finding>,
}

/// What a live `--rpc` run read before it stopped.
#[derive(Debug, Clone, Serialize)]
pub struct SamplingSummary {
    /// Successful transactions read.
    pub transactions: usize,
    pub stop_reason: String,
}

impl Report {
    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .count()
    }

    /// 1 when any finding is an error, else 0. (2, for I/O and parse errors, is the CLI's.)
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        u8::from(self.count(Severity::Error) > 0)
    }

    /// Pretty-printed JSON, with counts and the exit code added.
    ///
    /// # Errors
    ///
    /// Only if serialization fails, which plain data like this does not.
    pub fn to_json(&self) -> serde_json::Result<String> {
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
            exit_code: self.exit_code(),
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
        if unsampled_types + unsampled_ixs + unsampled_events > 0 {
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
            format!("{n} {}", if n == 1 { one } else { many })
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
        s
    };
    writeln!(out, "{}", line(header))?;
    for row in rows {
        writeln!(out, "{}", line(row.each_ref().map(String::as_str)))?;
    }
    Ok(())
}
