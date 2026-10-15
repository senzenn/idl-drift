use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use idl_drift::inspect::{shared_types, SharedType};
use idl_drift::model::Idl;
use idl_drift::{coverage, diff, layouts, Change, CoverageReport, Report, Severity, TypeLayout};
use owo_colors::OwoColorize;
use serde::Serialize;

/// Semantic drift detection for Solana program IDLs.
#[derive(Parser)]
#[command(name = "idl-drift", version, about)]
#[command(after_help = "Exit codes:
  0  diff: no change at or above --fail-on; inspect: every field resolved
  1  diff: at least one change at or above --fail-on; inspect: unmapped fields,
     or a padded zero-copy type that Borsh also reads
  2  a file could not be read or parsed")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Diff two IDLs and classify every change.
    Diff {
        /// Baseline (old) IDL: Anchor or Codama JSON.
        old: PathBuf,
        /// Current (new) IDL: Anchor or Codama JSON.
        new: PathBuf,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Lowest tier that sets exit code 1.
        #[arg(long, value_enum, default_value_t = Tier::Breaking)]
        fail_on: Tier,
    },
    /// Report fields whose byte layout could not be resolved, and padded zero-copy types
    /// that Borsh also reads.
    Inspect {
        /// IDL to scan: Anchor or Codama JSON.
        idl: PathBuf,
        /// Also print every zero-copy type field by field, with its gaps and both sizes.
        #[arg(long)]
        layout: bool,
    },
}

/// `Severity` as a CLI value; the lib enum stays free of clap.
#[derive(Clone, Copy, ValueEnum)]
enum Tier {
    Breaking,
    Dangerous,
    Additive,
    Cosmetic,
}

impl From<Tier> for Severity {
    fn from(tier: Tier) -> Self {
        match tier {
            Tier::Breaking => Self::Breaking,
            Tier::Dangerous => Self::Dangerous,
            Tier::Additive => Self::Additive,
            Tier::Cosmetic => Self::Cosmetic,
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Help and version are not errors; a usage error exits 2, so JSON must say so.
            if e.use_stderr() && std::env::args_os().any(|a| a == "--json") {
                let text = e.to_string();
                let text = text.trim();
                print_error_json(text.strip_prefix("error: ").unwrap_or(text));
            }
            e.exit()
        }
    };
    let json = matches!(cli.cmd, Cmd::Diff { json: true, .. });
    let result = match cli.cmd {
        Cmd::Diff {
            old,
            new,
            json,
            fail_on,
        } => run_diff(&old, &new, json, fail_on.into()),
        Cmd::Inspect { idl, layout } => run_inspect(&idl, layout),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {e}");
            // A failed report write can leave part of a JSON object; do not add a second one.
            if json && !matches!(e, Failure::Write(_)) {
                print_error_json(&e.to_string());
            }
            ExitCode::from(2)
        }
    }
}

/// Why a run exits 2.
enum Failure {
    /// A file could not be read or parsed.
    Input(String),
    Write(io::Error),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(message) => f.write_str(message),
            Self::Write(e) => write!(f, "write report: {e}"),
        }
    }
}

/// The `--json` form of a run that exits 2. The keys are the same as in `idl-verify`.
fn print_error_json(message: &str) {
    let _ = writeln!(
        io::stdout(),
        "{:#}",
        serde_json::json!({ "error": message, "exit_code": 2 })
    );
}

/// A reader that stops early, such as `head`, closes the pipe. That does not change the
/// result, so the exit code stays the same.
fn written(result: io::Result<()>) -> Result<(), Failure> {
    match result {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(Failure::Write(e)),
        _ => Ok(()),
    }
}

fn load(path: &Path) -> Result<Idl, Failure> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| Failure::Input(format!("read {}: {e}", path.display())))?;
    Idl::from_json(&src).map_err(|e| Failure::Input(format!("parse {}: {e}", path.display())))
}

fn run_diff(old: &Path, new: &Path, json: bool, fail_on: Severity) -> Result<u8, Failure> {
    let report = diff(&load(old)?, &load(new)?);
    let code = u8::from(report.fails_at(fail_on));
    written(if json {
        print_json(&report, code)
    } else {
        print_human(&report, code)
    })?;
    Ok(code)
}

fn run_inspect(path: &Path, layout: bool) -> Result<u8, Failure> {
    let idl = load(path)?;
    let report = coverage(&idl);
    let shared = shared_types(&idl);
    written(print_coverage(&report, &shared))?;
    if layout {
        written(print_layouts(&layouts(&idl)))?;
    }
    Ok(u8::from(!report.fully_mapped() || !shared.is_empty()))
}

fn print_layouts(types: &[TypeLayout]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "zero-copy types: {}", types.len())?;
    for t in types {
        let repr = t
            .repr
            .map_or_else(|| "no repr".to_string(), |r| r.to_string());
        let borsh = t
            .borsh_size
            .map_or_else(|| "varies".to_string(), |n| n.to_string());
        match &t.layout {
            Ok(layout) => {
                writeln!(
                    out,
                    "\n{} ({} {repr}): repr(C) size {}, Borsh size {borsh}, padding {}",
                    t.name,
                    t.serialization,
                    layout.size,
                    layout.padding_len()
                )?;
                writeln!(out, "  offset  size  align  field")?;
                for f in &layout.fields {
                    writeln!(
                        out,
                        "  {:>6}  {:>4}  {:>5}  {}",
                        f.offset, f.size, f.align, f.name
                    )?;
                }
                for gap in &layout.padding {
                    writeln!(out, "  gap at {}: {gap}", gap.offset)?;
                }
            }
            Err(e) => writeln!(
                out,
                "\n{} ({} {repr}): cannot compute the zero-copy layout: {e}",
                t.name, t.serialization
            )?,
        }
    }
    Ok(())
}

fn print_coverage(report: &CoverageReport, shared: &[SharedType]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "instructions: {}  accounts: {}  events: {}  types: {}",
        report.instructions, report.accounts, report.events, report.types
    )?;
    writeln!(out, "unmapped (Generic) fields: {}", report.unmapped.len())?;
    for u in &report.unmapped {
        writeln!(out, "UNMAPPED: {} -> Generic(\"{}\")", u.path, u.kind)?;
    }
    writeln!(
        out,
        "padded zero-copy types also read as Borsh: {}",
        shared.len()
    )?;
    for t in shared {
        let gaps: Vec<String> = t.padding.iter().map(ToString::to_string).collect();
        writeln!(
            out,
            "SHARED: {} -> padding {}; Borsh reads it without the padding",
            t.name,
            gaps.join(", ")
        )?;
    }
    Ok(())
}

const TIERS: [(Severity, &str); 4] = [
    (Severity::Breaking, "BREAKING"),
    (Severity::Dangerous, "DANGEROUS"),
    (Severity::Additive, "ADDITIVE"),
    (Severity::Cosmetic, "COSMETIC"),
];

fn print_human(report: &Report, exit_code: u8) -> io::Result<()> {
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = io::stdout().lock();
    for (severity, tag) in TIERS {
        for change in report.changes.iter().filter(|c| c.severity == severity) {
            writeln!(
                out,
                "  [{}] {}",
                paint(tag, severity, color),
                change.message
            )?;
        }
    }
    let (b, d, a, c) = report.counts();
    writeln!(
        out,
        "summary: {b} breaking, {d} dangerous, {a} additive, {c} cosmetic"
    )?;
    writeln!(out, "exit code: {exit_code}")
}

fn paint(tag: &str, severity: Severity, color: bool) -> String {
    if !color {
        return tag.to_string();
    }
    match severity {
        Severity::Breaking => tag.red().bold().to_string(),
        Severity::Dangerous => tag.yellow().bold().to_string(),
        Severity::Additive => tag.green().to_string(),
        Severity::Cosmetic => tag.dimmed().to_string(),
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    /// Every change with its rule (`kind`) and `subject`, for allow-lists in CI.
    changes: &'a [Change],
    breaking: Vec<&'a str>,
    dangerous: Vec<&'a str>,
    additive: Vec<&'a str>,
    cosmetic: Vec<&'a str>,
    counts: JsonCounts,
    exit_code: u8,
}

#[derive(Serialize)]
struct JsonCounts {
    breaking: usize,
    dangerous: usize,
    additive: usize,
    cosmetic: usize,
}

fn print_json(report: &Report, exit_code: u8) -> io::Result<()> {
    let messages = |severity| {
        report
            .changes
            .iter()
            .filter(|c| c.severity == severity)
            .map(|c| c.message.as_str())
            .collect()
    };
    let (breaking, dangerous, additive, cosmetic) = report.counts();
    let view = JsonReport {
        changes: &report.changes,
        breaking: messages(Severity::Breaking),
        dangerous: messages(Severity::Dangerous),
        additive: messages(Severity::Additive),
        cosmetic: messages(Severity::Cosmetic),
        counts: JsonCounts {
            breaking,
            dangerous,
            additive,
            cosmetic,
        },
        exit_code,
    };
    let text = serde_json::to_string_pretty(&view).map_err(io::Error::other)?;
    writeln!(io::stdout().lock(), "{text}")
}
