use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};
use idl_drift::allow::AllowList;
use idl_drift::markdown::{escape, escape_control};
use idl_drift::model::Idl;
use idl_verify::report::Check;
use idl_verify::rpc::{rent_exempt_minimum, sample, Limits, StopReason};
use idl_verify::{
    run_decoder, verify, verify_upgrade, Error, Reference, Report, Samples, SamplingSummary,
    Severity,
};
use serde_json::{json, Value};

/// Check a Solana program IDL against real account, instruction and event data.
///
/// Each sample is classified by discriminator and decoded from the IDL alone. Findings
/// cover unknown discriminators, data that fails to decode, bytes the IDL leaves unused
/// (such as zero-copy padding a converter dropped), instructions whose account lists don't
/// match the IDL, and values that disagree with a reference decoder.
#[derive(Parser)]
#[command(name = "idl-verify", version, about, long_about)]
#[command(after_help = "Exit codes:
  0  no finding at or above --fail-on (notes may be present)
  1  at least one finding at or above --fail-on (default: error)
  2  idl-verify could not run: a file could not be read or parsed, an option is
     wrong, the RPC or the decoder failed, or nothing was sampled")]
struct Cli {
    /// Program IDL: Anchor (0.30 spec or legacy) or Codama JSON.
    #[arg(long, value_name = "PATH")]
    idl: PathBuf,
    /// Captured samples JSON: base64 account and instruction data.
    #[arg(long, value_name = "PATH", required_unless_present = "rpc")]
    samples: Option<PathBuf>,
    /// Take samples from recent transactions over this JSON-RPC URL instead.
    /// Prefer the env var: `ps` and shell history show a URL given on the command line.
    #[arg(
        long,
        value_name = "URL",
        env = "IDL_VERIFY_RPC_URL",
        hide_env_values = true
    )]
    rpc: Option<String>,
    /// Program to sample with --rpc. Defaults to the IDL's address.
    #[arg(long, value_name = "ID", requires = "rpc")]
    program: Option<String>,
    /// With --rpc: read at most this many successful transactions, paging back through
    /// the program's history.
    #[arg(long, value_name = "N", value_parser = at_least_one(),
        default_value_t = Limits::default().max_transactions)]
    max_tx: usize,
    /// With --rpc: stop after this many successful transactions in a row bring no new
    /// instruction or event kind.
    #[arg(long, value_name = "N", value_parser = at_least_one(),
        default_value_t = Limits::default().stall)]
    stall: usize,
    /// Reference decoder output to compare values with (see tools/reference).
    #[arg(long, value_name = "PATH", conflicts_with = "decoder")]
    reference: Option<PathBuf>,
    /// Decoder under test: a command that reads one JSON line per sample on stdin and
    /// answers one per line (see docs/decoder-protocol.md). Split on whitespace.
    #[arg(long, value_name = "CMD")]
    decoder: Option<String>,
    /// Another IDL of the same program, such as the one before a migration: decode the
    /// samples with it too and compare values and discriminators.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["reference", "decoder"])]
    compare_idl: Option<PathBuf>,
    /// Treat --idl as the IDL of a program upgrade: count the sampled accounts that decode,
    /// need a realloc (with the extra rent), or break. Checks accounts only. Give the current
    /// IDL with --compare-idl to also find fields that read another value.
    #[arg(long, conflicts_with_all = ["reference", "decoder"])]
    upgrade_impact: bool,
    /// Write what a live run sampled to this file, to rerun it offline with --samples.
    /// Live runs only: it cannot be combined with --samples.
    #[arg(long, value_name = "PATH")]
    save_samples: Option<PathBuf>,
    /// Fail (exit 1) when less than this percent of the IDL's account types, instructions
    /// or events have a sample. Without it, a run passes on any number of samples.
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
    min_coverage: Option<u8>,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Print the report as GitHub Markdown, for a job summary or a pull request comment.
    #[arg(long, conflicts_with = "json")]
    markdown: bool,
    /// Lowest severity that sets exit code 1.
    #[arg(long, value_enum, default_value_t = Tier::Error)]
    fail_on: Tier,
    /// Allow file: one `<check> <subject>` per line. A finding that it lists does not set
    /// exit code 1, and stays in the report as allowed.
    #[arg(long, value_name = "FILE")]
    allow: Option<PathBuf>,
}

/// `Severity` as a CLI value; the lib enum stays free of clap.
#[derive(Clone, Copy, ValueEnum)]
enum Tier {
    Error,
    Warning,
}

impl From<Tier> for Severity {
    fn from(tier: Tier) -> Self {
        match tier {
            Tier::Error => Self::Error,
            Tier::Warning => Self::Warning,
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
    match run(&cli, &matches) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {}", escape_control(&e.to_string()));
            // A failed report write can leave part of a report; do not add to it.
            if !matches!(e, Error::Write(_)) {
                if cli.json {
                    print_error_json(&e.to_string());
                } else if cli.markdown {
                    print_error_markdown(&e.to_string());
                }
            }
            ExitCode::from(2)
        }
    }
}

/// The `--json` form of a run that exits 2. The keys are the same as in `idl-drift`.
fn print_error_json(message: &str) {
    let _ = writeln!(
        io::stdout(),
        "{:#}",
        json!({ "error": message, "exit_code": 2 })
    );
}

/// The `--markdown` form of a run that exits 2, so that a job summary shows the cause.
fn print_error_markdown(message: &str) {
    let _ = writeln!(
        io::stdout(),
        "**idl-verify could not run (exit code 2):** {}",
        escape(message)
    );
}

/// Only accounts with no data give nothing to check, since no IDL can describe them.
/// `--upgrade-impact` checks accounts only.
fn nothing_to_check(samples: &Samples, accounts_only: bool) -> bool {
    samples.accounts.iter().all(|a| a.data.is_empty())
        && (accounts_only || (samples.instructions.is_empty() && samples.events.is_empty()))
}

/// The end of a "nothing to check" message: why accounts alone count.
fn accounts_only_note(cli: &Cli) -> &'static str {
    if cli.upgrade_impact {
        "; --upgrade-impact checks accounts only"
    } else {
        ""
    }
}

fn run(cli: &Cli, matches: &ArgMatches) -> Result<u8, Error> {
    // `--samples` wins over `--rpc`, so a save request would write nothing.
    if cli.samples.is_some() && cli.save_samples.is_some() {
        return Err(Error::Usage(
            "--save-samples saves only live samples, and --samples turns live sampling off; drop one of the two".into(),
        ));
    }
    // The limits have defaults, so only a value given on the command line conflicts.
    let limit = [("max_tx", "--max-tx"), ("stall", "--stall")]
        .into_iter()
        .find(|(id, _)| matches.value_source(id) == Some(ValueSource::CommandLine));
    if let (Some(_), Some((_, flag))) = (&cli.samples, limit) {
        return Err(Error::Usage(format!(
            "{flag} limits only live sampling, and --samples turns live sampling off; drop one of the two"
        )));
    }
    // Before any RPC request, so that a typo in the allow file costs nothing.
    let allow = cli.allow.as_deref().map(read_allow).transpose()?;
    let idl = read_idl(&cli.idl)?;
    let (samples, live) = if let Some(path) = &cli.samples {
        let samples = Samples::from_json(&read(path)?)?;
        if nothing_to_check(&samples, cli.upgrade_impact) {
            return Err(Error::NothingSampled(format!(
                "{} holds no samples, or only accounts with no data{}",
                path.display(),
                accounts_only_note(cli)
            )));
        }
        (samples, None)
    } else {
        let (samples, live) = sample_live(cli, &idl)?;
        (samples, Some(live))
    };
    let reference = match (&cli.reference, &cli.decoder, &cli.compare_idl) {
        // --upgrade-impact compares account values only, so the old IDL decodes only those.
        (_, _, Some(path)) if cli.upgrade_impact => {
            let accounts = Samples {
                accounts: samples.accounts.clone(),
                ..Samples::default()
            };
            Some(Reference::from_idl(&read_idl(path)?, &idl, &accounts))
        }
        (_, _, Some(path)) => Some(Reference::from_idl(&read_idl(path)?, &idl, &samples)),
        (Some(path), _, None) => Some(Reference::from_json(&read(path)?, &samples)?),
        (None, Some(command), None) => {
            let words: Vec<String> = command.split_whitespace().map(str::to_string).collect();
            let program = cli
                .program
                .as_deref()
                .or(samples.program_id.as_deref())
                .unwrap_or(&idl.address);
            Some(run_decoder(&words, &samples, program, &idl)?)
        }
        (None, None, None) => None,
    };

    let mut report = match (cli.upgrade_impact, &live) {
        (false, _) => verify(&idl, &samples, reference.as_ref()),
        (true, Some(live)) => {
            let mut rent = |len| {
                rent_exempt_minimum(
                    |method: &str, params| rpc_call(&live.agent, live.url, method, &params),
                    len,
                )
            };
            verify_upgrade(&idl, &samples, reference.as_ref(), Some(&mut rent))?
        }
        (true, None) => verify_upgrade(&idl, &samples, reference.as_ref(), None)?,
    };
    report.sampling = live.map(|l| l.summary);
    if let Some(min) = cli.min_coverage {
        report.require_coverage(min);
    }
    if let (Some(list), Some(path)) = (&allow, &cli.allow) {
        apply_allow(&mut report, list, path);
    }
    let fail_on = cli.fail_on.into();
    let code = u8::from(report.fails_at(fail_on));
    print(&report, cli, fail_on, code).map_err(Error::Write)?;
    Ok(code)
}

/// A live run: what it read, and the connection, kept to ask the cluster for rent-exempt
/// minimums.
struct Live<'c> {
    summary: SamplingSummary,
    agent: ureq::Agent,
    url: &'c str,
}

/// Sample the program over JSON-RPC, and save the samples when `--save-samples` asks.
fn sample_live<'c>(cli: &'c Cli, idl: &Idl) -> Result<(Samples, Live<'c>), Error> {
    let url = cli.rpc.as_deref().unwrap_or_default().trim();
    if url.is_empty() {
        return Err(Error::Usage(
            "the RPC URL is empty; set --rpc or IDL_VERIFY_RPC_URL".into(),
        ));
    }
    let agent = idl_drift::rpc::agent();
    let program = cli.program.as_deref().unwrap_or(&idl.address);
    // A bad key gets an empty answer from the RPC, which would look like a quiet program.
    if bs58::decode(program)
        .into_vec()
        .map_or(true, |k| k.len() != 32)
    {
        return Err(Error::Usage(format!(
            "`{program}` is not a base58 program address; check --program or the IDL's address"
        )));
    }
    let call = |method: &str, params: Value| rpc_call(&agent, url, method, &params);
    let limits = Limits {
        max_transactions: cli.max_tx,
        stall: cli.stall,
        ..Limits::default()
    };
    let sampled = sample(call, idl, program, &limits)?;
    let s = &sampled.samples;
    // A run that saw nothing has checked nothing; passing it would hide a wrong address.
    if nothing_to_check(s, cli.upgrade_impact) {
        let what = if cli.upgrade_impact {
            "account of it with data"
        } else {
            "instruction, account or event of it"
        };
        // A stop at the last deploy is not a wrong address: the older transactions ran old code.
        let advice = match sampled.stop_reason {
            StopReason::Upgrade(_) => format!("sampling stopped because {}", sampled.stop_reason),
            // The newest transactions can name the program as an account without a call to it.
            _ if sampled.transactions > 0 => format!(
                "raise --max-tx and --stall to read more, or check the program address{}",
                accounts_only_note(cli)
            ),
            _ => format!("check the program address{}", accounts_only_note(cli)),
        };
        return Err(Error::NothingSampled(format!(
            "read {} successful transactions of {program} and found no {what}; {advice}",
            sampled.transactions,
        )));
    }
    if let Some(path) = &cli.save_samples {
        std::fs::write(path, s.to_json()).map_err(|source| Error::Save {
            path: path.clone(),
            source,
        })?;
    }
    let summary = SamplingSummary {
        transactions: sampled.transactions,
        stop_reason: sampled.stop_reason.to_string(),
    };
    Ok((
        sampled.samples,
        Live {
            summary,
            agent,
            url,
        },
    ))
}

/// One JSON-RPC request with the retry rules of `idl_drift::rpc`. Errors never include `url`.
fn rpc_call(agent: &ureq::Agent, url: &str, method: &str, params: &Value) -> Result<Value, Error> {
    idl_drift::rpc::call(agent, url, method, params).map_err(Error::Rpc)
}

fn at_least_one() -> clap::builder::RangedU64ValueParser<usize> {
    clap::builder::RangedU64ValueParser::new().range(1..)
}

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn read_allow(path: &Path) -> Result<AllowList<Check>, Error> {
    AllowList::parse(&read(path)?).map_err(|source| Error::Allow {
        path: path.to_path_buf(),
        source,
    })
}

/// Mark the allowed findings, and warn on stderr about each entry that matches none.
fn apply_allow(report: &mut Report, list: &AllowList<Check>, path: &Path) {
    report.allow(list);
    for entry in report.unused_allows.iter().flatten() {
        let _ = writeln!(
            io::stderr(),
            "warning: allow file {}: line {}: no finding matches `{entry}`",
            path.display(),
            entry.line
        );
    }
}

fn read_idl(path: &Path) -> Result<Idl, Error> {
    Idl::from_json(&read(path)?).map_err(|source| Error::Idl {
        path: path.to_path_buf(),
        source,
    })
}

fn print(report: &Report, cli: &Cli, fail_on: Severity, exit_code: u8) -> io::Result<()> {
    let stdout = io::stdout();
    let color = stdout.is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = stdout.lock();
    if cli.json {
        let text = report
            .to_json_with_exit_code(exit_code)
            .map_err(io::Error::other)?;
        writeln!(out, "{text}")
    } else if cli.markdown {
        let samples = cli
            .samples
            .as_deref()
            .map_or_else(|| "live samples".to_string(), |p| p.display().to_string());
        let inputs = format!("{} with {samples}", cli.idl.display());
        report.write_markdown(&mut out, &inputs, fail_on, exit_code)
    } else {
        report.write_human(&mut out, color)
    }
}
