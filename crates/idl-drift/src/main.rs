use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::parser::ValueSource;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use idl_drift::allow::{json_name, AllowEntry, AllowList};
use idl_drift::inspect::{shared_types, SharedType};
use idl_drift::markdown::{escape, escape_control};
use idl_drift::model::Idl;
use idl_drift::onchain::{self, OnchainIdl, Source};
use idl_drift::shipstern;
use idl_drift::source::{self, Finding, Leg, ProgramId, SourceReport};
use idl_drift::{
    coverage, diff, layouts, Change, ChangeKind, CoverageReport, Report, Severity, TypeLayout,
};
use owo_colors::OwoColorize;
use serde::Serialize;

/// Compare two versions of a Solana program IDL.
#[derive(Parser)]
#[command(name = "idl-drift", version, about)]
#[command(after_help = "Exit codes:
  0  diff: no change at or above --fail-on; source: no finding at or above --fail-on;
     inspect: every field resolved
  1  diff: at least one change at or above --fail-on; source: at least one finding at or
     above --fail-on; inspect: unmapped fields, an undefined type, or a padded zero-copy
     type that Borsh also reads
  2  idl-drift could not run: a file could not be read or parsed, an option is
     wrong, or the on-chain IDL or program account could not be fetched or read")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Diff two IDLs and classify every change.
    Diff(DiffArgs),
    /// Report fields whose byte layout could not be resolved, and padded zero-copy types
    /// that Borsh also reads.
    Inspect {
        /// IDL to scan: Anchor or Codama JSON.
        idl: PathBuf,
        /// Also print every zero-copy type field by field, with its gaps and both sizes.
        #[arg(long)]
        layout: bool,
    },
    /// Check that an Anchor workspace gives one program id in `declare_id!`, Anchor.toml,
    /// target/idl and, with --onchain, on chain.
    Source(SourceCheckArgs),
}

#[derive(Args)]
struct DiffArgs {
    /// Baseline (old) IDL: Anchor or Codama JSON. With --onchain, a program id.
    old: PathBuf,
    /// Current (new) IDL: Anchor or Codama JSON.
    new: PathBuf,
    /// Read OLD as a program id, and use the IDL that the program publishes on chain as
    /// the baseline: `diff --onchain <PROGRAM> <NEW>`.
    #[arg(long)]
    onchain: bool,
    /// JSON-RPC URL for --onchain. Prefer the env var: `ps` and shell history show a URL
    /// given on the command line.
    #[arg(
        long,
        value_name = "URL",
        env = "IDL_DRIFT_RPC_URL",
        hide_env_values = true
    )]
    rpc: Option<String>,
    /// With --onchain, read the IDL from this account. Needed when the program has both,
    /// because the two can differ.
    #[arg(long, value_enum, requires = "onchain")]
    source: Option<SourceArg>,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Print the report as GitHub Markdown, for a job summary or a pull request comment.
    #[arg(long, conflicts_with = "json")]
    markdown: bool,
    /// Lowest tier that sets exit code 1.
    #[arg(long, value_enum, default_value_t = Tier::Breaking)]
    fail_on: Tier,
    /// Allow file: one `<kind> <subject>` per line. A change that it lists does not set
    /// exit code 1, and stays in the report as allowed.
    #[arg(long, value_name = "FILE")]
    allow: Option<PathBuf>,
}

#[derive(Args)]
struct SourceCheckArgs {
    /// The Anchor workspace: the directory that holds Anchor.toml.
    #[arg(default_value = ".")]
    root: PathBuf,
    /// One more IDL to check, such as a published copy outside target/idl. Repeat it for
    /// more IDLs.
    #[arg(long, value_name = "PATH")]
    idl: Vec<PathBuf>,
    /// Also check that a program is deployed at each `declare_id!`, and that the IDL it
    /// publishes on chain gives the same address.
    #[arg(long)]
    onchain: bool,
    /// JSON-RPC URL for --onchain. Prefer the env var: `ps` and shell history show a URL
    /// given on the command line.
    #[arg(
        long,
        value_name = "URL",
        env = "IDL_DRIFT_RPC_URL",
        hide_env_values = true
    )]
    rpc: Option<String>,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Print the report as GitHub Markdown, for a job summary or a pull request comment.
    #[arg(long, conflicts_with = "json")]
    markdown: bool,
    /// Lowest tier that sets exit code 1.
    #[arg(long, value_enum, default_value_t = Tier::Breaking)]
    fail_on: Tier,
}

/// How `diff` and `source` print their report.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Human,
    Json,
    Markdown,
}

/// `Severity` as a CLI value; the lib enum stays free of clap.
#[derive(Clone, Copy, ValueEnum)]
enum Tier {
    Breaking,
    Dangerous,
    Additive,
    Cosmetic,
}

/// `onchain::Source` as a CLI value.
#[derive(Clone, Copy, ValueEnum)]
enum SourceArg {
    /// The canonical `idl` account of the Program Metadata program.
    Metadata,
    /// The legacy Anchor IDL account.
    Anchor,
}

impl From<SourceArg> for Source {
    fn from(source: SourceArg) -> Self {
        match source {
            SourceArg::Metadata => Self::Metadata,
            SourceArg::Anchor => Self::Anchor,
        }
    }
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
    let parsed = Cli::command()
        .try_get_matches()
        .and_then(|m| Ok((Cli::from_arg_matches(&m)?, m)));
    let (cli, matches) = match parsed {
        Ok(parsed) => parsed,
        Err(e) => {
            // Help and version are not errors; a usage error exits 2, so JSON or Markdown says so.
            if e.use_stderr() {
                let text = e.to_string();
                let text = text.trim();
                let text = text.strip_prefix("error: ").unwrap_or(text);
                if std::env::args_os().any(|a| a == "--json") {
                    print_error_json(text);
                } else if std::env::args_os().any(|a| a == "--markdown") {
                    print_error_markdown(text.lines().next().unwrap_or_default());
                }
            }
            e.exit()
        }
    };
    let format = match &cli.cmd {
        Cmd::Diff(DiffArgs { json: true, .. })
        | Cmd::Source(SourceCheckArgs { json: true, .. }) => Format::Json,
        Cmd::Diff(DiffArgs { markdown: true, .. })
        | Cmd::Source(SourceCheckArgs { markdown: true, .. }) => Format::Markdown,
        _ => Format::Human,
    };
    // A shell can keep the env var set for runs off chain, so only the flag needs --onchain.
    let rpc_flag = |cmd: &str| {
        matches
            .subcommand_matches(cmd)
            .and_then(|m| m.value_source("rpc"))
            == Some(ValueSource::CommandLine)
    };
    let result = match cli.cmd {
        Cmd::Diff(args) => run_diff(&args, rpc_flag("diff"), format),
        Cmd::Inspect { idl, layout } => run_inspect(&idl, layout),
        Cmd::Source(args) => run_source(&args, rpc_flag("source"), format),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {}", escape_control(&e.to_string()));
            // A failed report write can leave part of a report; do not add to it.
            if !matches!(e, Failure::Write(_)) {
                match format {
                    Format::Json => print_error_json(&e.to_string()),
                    Format::Markdown => print_error_markdown(&e.to_string()),
                    Format::Human => {}
                }
            }
            ExitCode::from(2)
        }
    }
}

/// Why a run exits 2.
enum Failure {
    /// A file could not be read or parsed.
    Input(String),
    /// An option is wrong.
    Usage(String),
    /// The on-chain IDL or program account could not be fetched or read.
    Onchain(onchain::Error),
    Write(io::Error),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(message) | Self::Usage(message) => f.write_str(message),
            Self::Onchain(e) => write!(f, "{e}"),
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

/// The `--markdown` form of a run that exits 2, so that a job summary shows the cause.
fn print_error_markdown(message: &str) {
    let _ = writeln!(
        io::stdout(),
        "**idl-drift could not run (exit code 2):** {}",
        escape(message)
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
    load_with_text(path).map(|(_, idl)| idl)
}

/// The IDL and its text. The Shipstern checks read the Codama nodes, which `Idl` does not keep.
fn load_with_text(path: &Path) -> Result<(String, Idl), Failure> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| Failure::Input(format!("read {}: {e}", path.display())))?;
    let idl = Idl::from_json(&src)
        .map_err(|e| Failure::Input(format!("parse {}: {e}", path.display())))?;
    Ok((src, idl))
}

fn load_allow(path: &Path) -> Result<AllowList<ChangeKind>, Failure> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Failure::Input(format!("read {}: {e}", path.display())))?;
    AllowList::parse(&text)
        .map_err(|e| Failure::Usage(format!("allow file {}: {e}", path.display())))
}

fn run_diff(args: &DiffArgs, rpc_flag: bool, format: Format) -> Result<u8, Failure> {
    rpc_needs_onchain(rpc_flag, args.onchain)?;
    // Before any RPC request, so that a typo in the allow file costs nothing.
    let allow = args.allow.as_deref().map(load_allow).transpose()?;
    let (mut report, onchain) = if args.onchain {
        let new = load(&args.new)?;
        // A path that is not UTF-8 is also not a base58 address, so fetch rejects it.
        let program = args.old.to_string_lossy();
        let old = fetch_onchain(&program, args.rpc.as_deref(), args.source.map(Into::into))?;
        (diff(&old.idl, &new), Some(old))
    } else {
        (diff(&load(&args.old)?, &load(&args.new)?), None)
    };
    let unused = allow.as_ref().map(|list| report.allow(list));
    if let (Some(unused), Some(path)) = (&unused, &args.allow) {
        for entry in unused {
            let _ = writeln!(
                io::stderr(),
                "warning: allow file {}: line {}: no change matches `{entry}`",
                path.display(),
                entry.line
            );
        }
    }
    let fail_on = args.fail_on.into();
    let code = u8::from(report.fails_at(fail_on));
    let (old, new) = (args.old.to_string_lossy(), args.new.to_string_lossy());
    let view = View {
        old: &old,
        new: &new,
        onchain: onchain.as_ref(),
        fail_on,
        unused: unused.as_deref(),
        exit_code: code,
    };
    written(match format {
        Format::Human => print_human(&report, &view),
        Format::Json => print_json(&report, &view),
        Format::Markdown => print_markdown(&report, &view),
    })?;
    Ok(code)
}

fn rpc_needs_onchain(rpc_flag: bool, onchain: bool) -> Result<(), Failure> {
    if rpc_flag && !onchain {
        return Err(Failure::Usage(
            "--rpc is the URL that --onchain reads from; add --onchain or drop --rpc".into(),
        ));
    }
    Ok(())
}

/// The URL for --onchain, from --rpc or `IDL_DRIFT_RPC_URL`.
fn rpc_url(rpc: Option<&str>) -> Result<&str, Failure> {
    let url = rpc.unwrap_or_default().trim();
    if url.is_empty() {
        return Err(Failure::Usage(
            "--onchain needs an RPC URL; set IDL_DRIFT_RPC_URL or --rpc".into(),
        ));
    }
    Ok(url)
}

fn fetch_onchain(
    program: &str,
    rpc: Option<&str>,
    source: Option<Source>,
) -> Result<OnchainIdl, Failure> {
    let url = rpc_url(rpc)?;
    let agent = idl_drift::rpc::agent();
    let call = |method: &str, params: serde_json::Value| {
        idl_drift::rpc::call(&agent, url, method, &params).map_err(onchain::Error::Rpc)
    };
    onchain::fetch(call, program, source).map_err(Failure::Onchain)
}

/// What a diff report shows besides the changes.
struct View<'a> {
    /// OLD and NEW as given, for the Markdown heading.
    old: &'a str,
    new: &'a str,
    onchain: Option<&'a OnchainIdl>,
    fail_on: Severity,
    /// With --allow: the entries that match no change.
    unused: Option<&'a [&'a AllowEntry<ChangeKind>]>,
    exit_code: u8,
}

fn run_inspect(path: &Path, layout: bool) -> Result<u8, Failure> {
    let (src, idl) = load_with_text(path)?;
    let report = coverage(&idl);
    let shared = shared_types(&idl);
    // Only a Codama IDL has account nodes to read, so do not parse an Anchor IDL again.
    let skipped = if idl.format == idl_drift::model::Format::Codama {
        shipstern::skipped_accounts(&src)
            .map_err(|e| Failure::Input(format!("parse {}: {e}", path.display())))?
    } else {
        Vec::new()
    };
    written(print_coverage(&report, &shared))?;
    written(print_shipstern(
        &shipstern::ambiguous_instructions(&idl),
        &skipped,
    ))?;
    if layout {
        written(print_layouts(&layouts(&idl)))?;
    }
    // Shipstern warnings leave the exit code as it is: other parsers read these IDLs.
    Ok(u8::from(!report.fully_mapped() || !shared.is_empty()))
}

fn print_shipstern(ambiguous: &[String], skipped: &[(String, &str)]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "instructions that Shipstern cannot tell apart: {}",
        ambiguous.len()
    )?;
    for group in ambiguous {
        writeln!(out, "SHIPSTERN: {}", escape_control(group))?;
    }
    writeln!(out, "account types that Shipstern skips: {}", skipped.len())?;
    for (name, why) in skipped {
        writeln!(out, "SHIPSTERN: {}: {why}", escape_control(name))?;
    }
    Ok(())
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
                    escape_control(&t.name),
                    t.serialization,
                    layout.size,
                    layout.padding_len()
                )?;
                writeln!(out, "  offset  size  align  field")?;
                for f in &layout.fields {
                    writeln!(
                        out,
                        "  {:>6}  {:>4}  {:>5}  {}",
                        f.offset,
                        f.size,
                        f.align,
                        escape_control(&f.name)
                    )?;
                }
                for gap in &layout.padding {
                    let gap_text = escape_control(&gap.to_string());
                    writeln!(out, "  gap at {}: {gap_text}", gap.offset)?;
                }
                for f in layout.fields.iter().filter(|f| f.padding > 0) {
                    let name = escape_control(&f.name);
                    writeln!(out, "  padding inside `{name}`: {}", f.padding)?;
                }
            }
            Err(e) => writeln!(
                out,
                "\n{} ({} {repr}): cannot compute the zero-copy layout: {}",
                escape_control(&t.name),
                t.serialization,
                escape_control(&e.to_string())
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
        let (path, kind) = (escape_control(&u.path), escape_control(&u.kind));
        writeln!(out, "UNMAPPED: {path} -> Generic(\"{kind}\")")?;
    }
    writeln!(out, "undefined types: {}", report.undefined.len())?;
    for name in &report.undefined {
        writeln!(out, "UNDEFINED: {}", escape_control(name))?;
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
            escape_control(&t.name),
            escape_control(&gaps.join(", "))
        )?;
    }
    Ok(())
}

const TIERS: [(Severity, &str, &str); 4] = [
    (Severity::Breaking, "BREAKING", "Breaking"),
    (Severity::Dangerous, "DANGEROUS", "Dangerous"),
    (Severity::Additive, "ADDITIVE", "Additive"),
    (Severity::Cosmetic, "COSMETIC", "Cosmetic"),
];

/// Changes or findings in report order: by tier, Breaking first.
fn by_tier<T>(
    items: &[T],
    severity: fn(&T) -> Severity,
) -> impl Iterator<Item = (&T, &'static str, &'static str)> {
    TIERS.into_iter().flat_map(move |(tier, tag, title)| {
        items
            .iter()
            .filter(move |i| severity(i) == tier)
            .map(move |i| (i, tag, title))
    })
}

fn print_human(report: &Report, view: &View) -> io::Result<()> {
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = io::stdout().lock();
    if let Some(o) = view.onchain {
        writeln!(
            out,
            "on-chain IDL: {} {}",
            o.source,
            escape_control(&o.account)
        )?;
    }
    for (change, tag, _) in by_tier(&report.changes, |c| c.severity) {
        let allowed = if change.allowed { "[ALLOWED] " } else { "" };
        writeln!(
            out,
            "  {allowed}[{}] {}",
            paint(tag, change.severity, color),
            escape_control(&change.message)
        )?;
    }
    let (b, d, a, c) = report.counts();
    write!(
        out,
        "summary: {b} breaking, {d} dangerous, {a} additive, {c} cosmetic"
    )?;
    let allowed = report.changes.iter().filter(|c| c.allowed).count();
    if allowed > 0 {
        write!(out, ", {allowed} allowed")?;
    }
    writeln!(out)?;
    writeln!(out, "exit code: {}", view.exit_code)
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

/// GitHub Markdown: a heading, the counts per tier, the exit code, a table of changes, and
/// the allowed changes in a `<details>` block.
fn print_markdown(report: &Report, view: &View) -> io::Result<()> {
    let mut out = io::stdout().lock();
    if let Some(o) = view.onchain {
        writeln!(
            out,
            "### idl-drift diff: on-chain IDL of {} to {}\n",
            escape(view.old),
            escape(view.new)
        )?;
        writeln!(out, "On-chain IDL: {} {}\n", o.source, escape(&o.account))?;
    } else {
        writeln!(
            out,
            "### idl-drift diff: {} to {}\n",
            escape(view.old),
            escape(view.new)
        )?;
    }
    let allowed = report.changes.iter().filter(|c| c.allowed).count();
    write_counts(&mut out, "Changes", report.counts())?;
    if allowed > 0 {
        writeln!(out, "| Allowed | {allowed} |")?;
    }
    write_exit_line(&mut out, "change", view.fail_on, view.exit_code)?;
    let changes = || by_tier(&report.changes, |c| c.severity);
    if report.changes.len() > allowed {
        writeln!(out)?;
        let rows = changes().filter(|(c, _, _)| !c.allowed).map(change_row);
        write_table(&mut out, rows)?;
    }
    if allowed > 0 {
        let noun = if allowed == 1 { "change" } else { "changes" };
        writeln!(
            out,
            "\n<details>\n<summary>{allowed} allowed {noun}</summary>\n"
        )?;
        write_table(
            &mut out,
            changes().filter(|(c, _, _)| c.allowed).map(change_row),
        )?;
        writeln!(out, "\n</details>")?;
    }
    if let Some(unused) = view.unused.filter(|u| !u.is_empty()) {
        writeln!(out, "\nAllow entries that match no change:\n")?;
        for entry in unused {
            writeln!(out, "- line {}: {}", entry.line, escape(&entry.to_string()))?;
        }
    }
    Ok(())
}

/// The number of changes or findings in each tier, as a Markdown table.
fn write_counts(
    out: &mut impl Write,
    noun: &str,
    (b, d, a, c): (usize, usize, usize, usize),
) -> io::Result<()> {
    writeln!(out, "| Tier | {noun} |\n|---|---:|")?;
    for ((_, _, title), n) in TIERS.iter().zip([b, d, a, c]) {
        writeln!(out, "| {title} | {n} |")?;
    }
    Ok(())
}

fn write_exit_line(
    out: &mut impl Write,
    noun: &str,
    fail_on: Severity,
    exit_code: u8,
) -> io::Result<()> {
    let result = if exit_code == 0 { "no" } else { "at least one" };
    writeln!(
        out,
        "\nExit code {exit_code}: {result} {noun} at or above `{}`.",
        json_name(&fail_on)
    )
}

/// One row of a Markdown table of changes or findings: tier, rule, subject and message.
type Row<'a> = (&'static str, String, &'a str, &'a str);

fn change_row<'a>((c, _, title): (&'a Change, &str, &'static str)) -> Row<'a> {
    (title, json_name(&c.kind), &c.subject, &c.message)
}

fn write_table<'a>(out: &mut impl Write, rows: impl Iterator<Item = Row<'a>>) -> io::Result<()> {
    writeln!(
        out,
        "| Tier | Rule | Subject | Message |\n|---|---|---|---|"
    )?;
    for (title, rule, subject, message) in rows {
        writeln!(
            out,
            "| {title} | `{rule}` | {} | {} |",
            escape(subject),
            escape(message)
        )?;
    }
    Ok(())
}

#[derive(Serialize)]
struct JsonReport<'a> {
    /// The account that the old IDL came from, with --onchain.
    #[serde(skip_serializing_if = "Option::is_none")]
    onchain: Option<JsonOnchain<'a>>,
    /// Every change with its rule (`kind`) and `subject`, for allow-lists in CI.
    changes: &'a [Change],
    breaking: Vec<&'a str>,
    dangerous: Vec<&'a str>,
    additive: Vec<&'a str>,
    cosmetic: Vec<&'a str>,
    counts: JsonCounts,
    /// With --allow: the entries that match no change.
    #[serde(skip_serializing_if = "Option::is_none")]
    unused_allows: Option<&'a [&'a AllowEntry<ChangeKind>]>,
    exit_code: u8,
}

#[derive(Serialize)]
struct JsonOnchain<'a> {
    source: &'static str,
    account: &'a str,
}

#[derive(Serialize)]
struct JsonCounts {
    breaking: usize,
    dangerous: usize,
    additive: usize,
    cosmetic: usize,
}

impl From<(usize, usize, usize, usize)> for JsonCounts {
    fn from((breaking, dangerous, additive, cosmetic): (usize, usize, usize, usize)) -> Self {
        Self {
            breaking,
            dangerous,
            additive,
            cosmetic,
        }
    }
}

fn print_json(report: &Report, view: &View) -> io::Result<()> {
    // Allowed changes stay in `changes` only, so that the tier lists agree with `counts`.
    let messages = |severity| {
        report
            .changes
            .iter()
            .filter(|c| c.severity == severity && !c.allowed)
            .map(|c| c.message.as_str())
            .collect()
    };
    let json = JsonReport {
        onchain: view.onchain.map(|o| JsonOnchain {
            source: o.source.key(),
            account: &o.account,
        }),
        changes: &report.changes,
        breaking: messages(Severity::Breaking),
        dangerous: messages(Severity::Dangerous),
        additive: messages(Severity::Additive),
        cosmetic: messages(Severity::Cosmetic),
        counts: report.counts().into(),
        unused_allows: view.unused,
        exit_code: view.exit_code,
    };
    let text = serde_json::to_string_pretty(&json).map_err(io::Error::other)?;
    writeln!(io::stdout().lock(), "{text}")
}

fn run_source(args: &SourceCheckArgs, rpc_flag: bool, format: Format) -> Result<u8, Failure> {
    rpc_needs_onchain(rpc_flag, args.onchain)?;
    // Before the scan, so that a missing URL costs nothing.
    let url = args
        .onchain
        .then(|| rpc_url(args.rpc.as_deref()))
        .transpose()?;
    let root = &args.root;
    if !root.is_dir() {
        return Err(Failure::Input(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let input = |e: source::SourceError| Failure::Input(e.to_string());
    let mut ids = source::discover(root).map_err(input)?;
    for path in &args.idl {
        ids.push(source::read_idl(path).map_err(input)?);
    }
    let mut report = source::check(ids);
    if let Some(url) = url {
        let agent = idl_drift::rpc::agent();
        source::check_onchain(&mut report, |m, p| {
            idl_drift::rpc::call(&agent, url, m, &p).map_err(onchain::Error::Rpc)
        })
        .map_err(Failure::Onchain)?;
    }
    // A wrong ROOT, or files that give no id, has nothing to compare. An exit 0 would pass it.
    if report.nothing_to_compare() {
        return Err(Failure::Input(format!(
            "nothing to compare under {}: no program has a declare_id! or an IDL address to compare with another id",
            root.display()
        )));
    }
    let fail_on = args.fail_on.into();
    let code = u8::from(report.fails_at(fail_on));
    let root = root.to_string_lossy();
    written(match format {
        Format::Human => print_source_human(&report, code),
        Format::Json => print_source_json(&report, &root, code),
        Format::Markdown => print_source_markdown(&report, &root, fail_on, code),
    })?;
    Ok(code)
}

/// Where the report read an id.
fn leg_text(leg: &Leg) -> String {
    match leg {
        Leg::DeclareId { path } => format!("declare_id! in {path}"),
        Leg::AnchorToml { path, cluster } => format!("[programs.{cluster}] in {path}"),
        Leg::IdlAddress { path } => format!("address in {path}"),
        Leg::Onchain { source, account } => {
            format!("address in the on-chain IDL ({source} account {account})")
        }
        _ => "another place".to_string(),
    }
}

/// An empty id is a file that holds none, such as a `lib.rs` with no `declare_id!`.
fn id_text(id: &str) -> &str {
    if id.is_empty() {
        "(none)"
    } else {
        id
    }
}

fn print_source_human(report: &SourceReport, exit_code: u8) -> io::Result<()> {
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = io::stdout().lock();
    for id in &report.ids {
        writeln!(
            out,
            "{}  {}  {}",
            escape_control(&id.program),
            escape_control(&leg_text(&id.leg)),
            escape_control(id_text(&id.id))
        )?;
    }
    for (finding, tag, _) in by_tier(&report.findings, |f| f.severity) {
        writeln!(
            out,
            "  [{}] {}: {}",
            paint(tag, finding.severity, color),
            escape_control(&finding.subject),
            escape_control(&finding.message)
        )?;
    }
    let (b, d, a, c) = report.counts();
    writeln!(
        out,
        "summary: {b} breaking, {d} dangerous, {a} additive, {c} cosmetic"
    )?;
    writeln!(out, "exit code: {exit_code}")
}

/// GitHub Markdown: a heading, the counts per tier, the exit code, a table of the ids, and a
/// table of the findings.
fn print_source_markdown(
    report: &SourceReport,
    root: &str,
    fail_on: Severity,
    exit_code: u8,
) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "### idl-drift source: {}\n", escape(root))?;
    write_counts(&mut out, "Findings", report.counts())?;
    write_exit_line(&mut out, "finding", fail_on, exit_code)?;
    writeln!(out, "\n| Program | Place | Program id |\n|---|---|---|")?;
    for id in &report.ids {
        writeln!(
            out,
            "| {} | {} | {} |",
            escape(&id.program),
            escape(&leg_text(&id.leg)),
            escape(id_text(&id.id))
        )?;
    }
    if !report.findings.is_empty() {
        writeln!(out)?;
        let rows = by_tier(&report.findings, |f| f.severity).map(|(f, _, title)| {
            (
                title,
                json_name(&f.kind),
                f.subject.as_str(),
                f.message.as_str(),
            )
        });
        write_table(&mut out, rows)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct JsonSource<'a> {
    /// ROOT as given.
    root: &'a str,
    /// Each program id read, with its place (`leg`).
    ids: &'a [ProgramId],
    findings: &'a [Finding],
    counts: JsonCounts,
    exit_code: u8,
}

fn print_source_json(report: &SourceReport, root: &str, exit_code: u8) -> io::Result<()> {
    let json = JsonSource {
        root,
        ids: &report.ids,
        findings: &report.findings,
        counts: report.counts().into(),
        exit_code,
    };
    let text = serde_json::to_string_pretty(&json).map_err(io::Error::other)?;
    writeln!(io::stdout().lock(), "{text}")
}
