use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};
use idl_drift::model::Idl;
use idl_verify::rpc::{sample, Limits};
use idl_verify::{
    run_decoder, verify, Error, Reference, Report, Samples, SamplingSummary, Severity,
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
    #[arg(long, value_name = "N", requires = "rpc", value_parser = at_least_one(),
        default_value_t = Limits::default().max_transactions)]
    max_tx: usize,
    /// With --rpc: stop after this many successful transactions in a row bring no new
    /// instruction or event kind.
    #[arg(long, value_name = "N", requires = "rpc", value_parser = at_least_one(),
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
    /// Write what a live run sampled to this file, to rerun it offline with --samples.
    /// Live runs only: it cannot be combined with --samples.
    #[arg(long, value_name = "PATH", requires = "rpc")]
    save_samples: Option<PathBuf>,
    /// Fail (exit 1) when less than this percent of the IDL's account types, instructions
    /// or events have a sample. Without it, a run passes on any number of samples.
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
    min_coverage: Option<u8>,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Lowest severity that sets exit code 1.
    #[arg(long, value_enum, default_value_t = Tier::Error)]
    fail_on: Tier,
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
            // Help and version are not errors; a usage error exits 2, so JSON must say so.
            if e.use_stderr() && std::env::args_os().any(|a| a == "--json") {
                let text = e.to_string();
                let text = text.trim();
                print_error_json(text.strip_prefix("error: ").unwrap_or(text));
            }
            e.exit()
        }
    };
    match run(&cli, &matches) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {e}");
            // A failed report write can leave part of a JSON object; do not add a second one.
            if cli.json && !matches!(e, Error::Write(_)) {
                print_error_json(&e.to_string());
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

/// Only accounts with no data give nothing to check, since no IDL can describe them.
fn nothing_to_check(samples: &Samples) -> bool {
    samples.accounts.iter().all(|a| a.data.is_empty())
        && samples.instructions.is_empty()
        && samples.events.is_empty()
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
    let idl = read_idl(&cli.idl)?;
    let (samples, sampling) = if let Some(path) = &cli.samples {
        let samples = Samples::from_json(&read(path)?)?;
        if nothing_to_check(&samples) {
            return Err(Error::NothingSampled(format!(
                "{} holds no samples, or only accounts with no data",
                path.display()
            )));
        }
        (samples, None)
    } else {
        let url = cli.rpc.as_deref().unwrap_or_default().trim();
        if url.is_empty() {
            return Err(Error::Usage(
                "the RPC URL is empty; set --rpc or IDL_VERIFY_RPC_URL".into(),
            ));
        }
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(30)))
                .http_status_as_error(false)
                .build(),
        );
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
        let sampled = sample(call, &idl, program, &limits)?;
        let s = &sampled.samples;
        // A run that saw nothing has checked nothing; passing it would hide a wrong address.
        if nothing_to_check(s) {
            return Err(Error::NothingSampled(format!(
                "read {} successful transactions of {program} and found no instruction, account or event of it; check the program address",
                sampled.transactions
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
        (sampled.samples, Some(summary))
    };
    let reference = match (&cli.reference, &cli.decoder, &cli.compare_idl) {
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

    let mut report = verify(&idl, &samples, reference.as_ref());
    report.sampling = sampling;
    if let Some(min) = cli.min_coverage {
        report.require_coverage(min);
    }
    let code = u8::from(report.fails_at(cli.fail_on.into()));
    print(&report, cli.json, code).map_err(Error::Write)?;
    Ok(code)
}

/// One JSON-RPC request, retried with backoff on rate limits and server errors. Errors
/// never include `url`.
fn rpc_call(agent: &ureq::Agent, url: &str, method: &str, params: &Value) -> Result<Value, Error> {
    let fail = |detail: String| Error::Rpc(format!("{method}: {detail}"));
    // Other ureq errors can repeat the URL, which holds the API key.
    let transport = |e: ureq::Error| match e {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Json(_) => fail(e.to_string()),
        _ => fail("the request failed; check that the RPC URL is a valid http(s) URL".into()),
    };
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
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
        let mut reply: Value = response.body_mut().read_json().map_err(transport)?;
        if let Some(error) = reply.get("error") {
            return Err(fail(error.to_string()));
        }
        return Ok(reply["result"].take());
    }
    Err(fail(
        "still rate limited or failing after 5 attempts".into(),
    ))
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

fn read_idl(path: &Path) -> Result<Idl, Error> {
    Idl::from_json(&read(path)?).map_err(|source| Error::Idl {
        path: path.to_path_buf(),
        source,
    })
}

fn print(report: &Report, json: bool, exit_code: u8) -> io::Result<()> {
    let stdout = io::stdout();
    let color = stdout.is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = stdout.lock();
    if json {
        let text = report
            .to_json_with_exit_code(exit_code)
            .map_err(io::Error::other)?;
        writeln!(out, "{text}")
    } else {
        report.write_human(&mut out, color)
    }
}
