use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use idl_drift::inspect::{shared_types, SharedType};
use idl_drift::model::Idl;
use idl_drift::onchain::{self, OnchainIdl, Source};
use idl_drift::{coverage, diff, layouts, Change, CoverageReport, Report, Severity, TypeLayout};
use owo_colors::OwoColorize;
use serde::Serialize;

/// Compare two versions of a Solana program IDL.
#[derive(Parser)]
#[command(name = "idl-drift", version, about)]
#[command(after_help = "Exit codes:
  0  diff: no change at or above --fail-on; inspect: every field resolved
  1  diff: at least one change at or above --fail-on; inspect: unmapped fields,
     or a padded zero-copy type that Borsh also reads
  2  idl-drift could not run: a file could not be read or parsed, an option is
     wrong, or the on-chain IDL could not be fetched or read")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Diff two IDLs and classify every change.
    Diff {
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
            onchain,
            rpc,
            source,
            json,
            fail_on,
        } => {
            // A shell can keep the env var set for file diffs, so only the flag needs --onchain.
            let rpc_flag = matches
                .subcommand_matches("diff")
                .and_then(|m| m.value_source("rpc"))
                == Some(ValueSource::CommandLine);
            if onchain {
                // A path that is not UTF-8 is also not a base58 address, so fetch rejects it.
                let program = old.to_string_lossy();
                run_onchain(
                    &program,
                    rpc.as_deref(),
                    source.map(Into::into),
                    &new,
                    json,
                    fail_on.into(),
                )
            } else if rpc_flag {
                Err(Failure::Usage(
                    "--rpc is the URL that --onchain reads from; add --onchain or drop --rpc"
                        .into(),
                ))
            } else {
                run_diff(&old, &new, json, fail_on.into())
            }
        }
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
    /// An option is wrong.
    Usage(String),
    /// The on-chain IDL could not be fetched or read.
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
    print_report(&report, None, json, fail_on)
}

fn run_onchain(
    program: &str,
    rpc: Option<&str>,
    source: Option<Source>,
    new: &Path,
    json: bool,
    fail_on: Severity,
) -> Result<u8, Failure> {
    let new = load(new)?;
    let url = rpc.unwrap_or_default().trim();
    if url.is_empty() {
        return Err(Failure::Usage(
            "--onchain needs an RPC URL; set IDL_DRIFT_RPC_URL or --rpc".into(),
        ));
    }
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build(),
    );
    let call = |method: &str, params: serde_json::Value| rpc_call(&agent, url, method, &params);
    let old = onchain::fetch(call, program, source).map_err(Failure::Onchain)?;
    let report = diff(&old.idl, &new);
    print_report(&report, Some(&old), json, fail_on)
}

/// The largest JSON-RPC answer to read: two accounts of the 10 MiB maximum, in base64.
const MAX_RESPONSE_BYTES: u64 = 32 << 20;

/// One JSON-RPC request, retried with backoff on rate limits and server errors. Errors
/// never include `url`, which can hold an API key.
fn rpc_call(
    agent: &ureq::Agent,
    url: &str,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, onchain::Error> {
    let fail = |detail: String| onchain::Error::Rpc(format!("{method}: {detail}"));
    // Other ureq errors can repeat the URL.
    let transport = |e: ureq::Error| match e {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::BodyExceedsLimit(_)
        | ureq::Error::Json(_) => fail(e.to_string()),
        _ => fail("the request failed; check that the RPC URL is a valid http(s) URL".into()),
    };
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    for attempt in 0..5 {
        let mut response = agent.post(url).send_json(&body).map_err(transport)?;
        let status = response.status().as_u16();
        if status == 429 || status >= 500 {
            std::thread::sleep(Duration::from_millis(500 << attempt));
            continue;
        }
        if status != 200 {
            return Err(fail(format!("HTTP {status}")));
        }
        let mut reply: serde_json::Value = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_json()
            .map_err(transport)?;
        // `Value` prints strings JSON-escaped, so control characters stay inert.
        if let Some(error) = reply.get("error") {
            return Err(fail(error.to_string()));
        }
        return Ok(reply["result"].take());
    }
    Err(fail(
        "still rate limited or failing after 5 attempts".into(),
    ))
}

fn print_report(
    report: &Report,
    onchain: Option<&OnchainIdl>,
    json: bool,
    fail_on: Severity,
) -> Result<u8, Failure> {
    let code = u8::from(report.fails_at(fail_on));
    written(if json {
        print_json(report, onchain, code)
    } else {
        print_human(report, onchain, code)
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

fn print_human(report: &Report, onchain: Option<&OnchainIdl>, exit_code: u8) -> io::Result<()> {
    let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = io::stdout().lock();
    if let Some(o) = onchain {
        writeln!(out, "on-chain IDL: {} {}", o.source, o.account)?;
    }
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

fn print_json(report: &Report, onchain: Option<&OnchainIdl>, exit_code: u8) -> io::Result<()> {
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
        onchain: onchain.map(|o| JsonOnchain {
            source: o.source.key(),
            account: &o.account,
        }),
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
