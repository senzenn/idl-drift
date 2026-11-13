use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};
use idl_drift::allow::AllowList;
use idl_drift::markdown::{escape, escape_control};
use idl_drift::model::{Format, Idl};
use idl_verify::export::{write_fixtures, ExportSummary, FillFile, FixtureFormat};
use idl_verify::report::Check;
use idl_verify::rpc::{
    rent_exempt_minimum, sample, timeline, Limits, Progress, StopReason, TimelineLimits,
    TransactionRecord,
};
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
// The bools are separate command-line flags, one each.
#[allow(clippy::struct_excessive_bools)]
#[derive(Parser)]
#[command(name = "idl-verify", version, about, long_about)]
#[command(after_help = "Exit codes:
  0  no finding at or above --fail-on (notes may be present)
  1  at least one finding at or above --fail-on (default: error)
  2  idl-verify could not run: a file could not be read or parsed, an option is
     wrong, the RPC or the decoder failed, or nothing was sampled

`idl-verify timeline --help`: which IDL decodes each slot range between deploys.")]
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
    /// instruction or event kind (a pruned one counts too), or this many pages of signatures
    /// in a row hold only failed transactions.
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
    /// Write the samples to this directory as test fixtures, in the format of
    /// --fixture-format. A file of the same name is replaced.
    #[arg(long, value_name = "DIR", requires = "fixture_format")]
    export_fixtures: Option<PathBuf>,
    /// The format of --export-fixtures: shipstern-mock or carbon-test-utils files. From
    /// --samples, shipstern gets account files only: a samples file keeps no whole transaction.
    #[arg(long, value_enum, value_name = "FORMAT", requires = "export_fixtures")]
    fixture_format: Option<Framework>,
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
    /// Write an allow file that accepts each finding at or above --fail-on: a new FILE, or the
    /// --allow file. The report and exit code are then the ones that `--allow FILE` gives.
    #[arg(long, value_name = "FILE")]
    write_allow: Option<PathBuf>,
    /// With --write-allow: also remove the entries of the --allow file that match no finding.
    /// The JSON report lists them in `pruned_allows`.
    #[arg(long, requires = "write_allow")]
    prune_allow: bool,
}

/// List the deploys of a program, and tell which IDL decodes the transactions of each slot
/// range between two deploys, so that a backfill decodes each range with the right IDL.
///
/// The deploys come from the transactions of the upgrade authority (each authority before it
/// too), and from the `ProgramData` account when the program is immutable. For each range,
/// up to --per-range successful program transactions are decoded with each IDL. An IDL fits
/// a transaction when idl-verify finds no error in its samples.
#[derive(Parser)]
#[command(
    name = "idl-verify timeline",
    bin_name = "idl-verify timeline",
    version,
    about
)]
#[command(after_help = "Exit codes:
  0  each range with samples has an IDL that decodes all of them
  1  a range has samples that no single IDL decodes
  2  idl-verify could not run: a file could not be read or parsed, an option is
     wrong, the RPC failed, or no range has a sample")]
struct TimelineCli {
    /// An IDL to try on each range: Anchor (0.30 spec or legacy) or Codama JSON. Give it
    /// once per IDL version.
    #[arg(long, value_name = "PATH", required = true)]
    idl: Vec<PathBuf>,
    /// The program. Defaults to the address of the first --idl.
    #[arg(long, value_name = "ID")]
    program: Option<String>,
    /// JSON-RPC URL. Prefer the env var: `ps` and shell history show a URL given on the
    /// command line.
    #[arg(
        long,
        value_name = "URL",
        env = "IDL_VERIFY_RPC_URL",
        hide_env_values = true
    )]
    rpc: Option<String>,
    /// Program transactions to decode in each range.
    #[arg(long, value_name = "N", value_parser = at_least_one(),
        default_value_t = TimelineLimits::default().per_range)]
    per_range: usize,
    /// Successful transactions to read at most in each range.
    #[arg(long, value_name = "N", value_parser = at_least_one(),
        default_value_t = TimelineLimits::default().max_transactions)]
    max_tx: usize,
    /// Transactions to read at most to find the deploys.
    #[arg(long, value_name = "N", value_parser = at_least_one(),
        default_value_t = TimelineLimits::default().max_scan)]
    max_scan: usize,
    /// Print the result as JSON.
    #[arg(long)]
    json: bool,
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

/// `FixtureFormat` as a CLI value.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Framework {
    Shipstern,
    Carbon,
}

impl From<Framework> for FixtureFormat {
    fn from(framework: Framework) -> Self {
        match framework {
            Framework::Shipstern => Self::Shipstern,
            Framework::Carbon => Self::Carbon,
        }
    }
}

fn main() -> ExitCode {
    // A mode, not a subcommand, so that the options and errors of a check stay the same.
    if std::env::args_os().nth(1).is_some_and(|a| a == "timeline") {
        return timeline_main();
    }
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

fn timeline_main() -> ExitCode {
    let cli = match TimelineCli::try_parse_from(std::env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(e) => {
            if e.use_stderr() && std::env::args_os().any(|a| a == "--json") {
                let text = e.to_string();
                let text = text.trim();
                print_error_json(text.strip_prefix("error: ").unwrap_or(text));
            }
            e.exit()
        }
    };
    match run_timeline(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {}", escape_control(&e.to_string()));
            if cli.json && !matches!(e, Error::Write(_)) {
                print_error_json(&e.to_string());
            }
            ExitCode::from(2)
        }
    }
}

fn run_timeline(cli: &TimelineCli) -> Result<u8, Error> {
    let idls = cli
        .idl
        .iter()
        .map(|path| read_idl(path))
        .collect::<Result<Vec<_>, _>>()?;
    let program = match (&cli.program, idls.first()) {
        (Some(program), _) => program.as_str(),
        (None, Some(idl)) => idl.address.as_str(),
        (None, None) => "",
    };
    check_program(program)?;
    let url = rpc_url(cli.rpc.as_deref())?;
    let agent = idl_drift::rpc::agent();
    let call = |method: &str, params: Value| rpc_call(&agent, url, method, &params);
    let mut limits = TimelineLimits::default();
    limits.per_range = cli.per_range;
    limits.max_transactions = cli.max_tx;
    limits.max_scan = cli.max_scan;
    // A long run on a public RPC can fail late, so show each step as it ends.
    let progress = |step: Progress<'_>| {
        let _ = writeln!(io::stderr(), "timeline: {step}");
    };
    let timeline = timeline(call, program, &idls, &limits, progress)?;
    // A run that decoded nothing has checked nothing; passing it would hide a wrong address.
    if !timeline.sampled() {
        return Err(Error::NothingSampled(format!(
            "no range of {program} has a successful transaction that calls it; raise --max-tx, or check the program address"
        )));
    }
    let labels: Vec<String> = cli.idl.iter().map(|p| p.display().to_string()).collect();
    let mut out = io::stdout().lock();
    if cli.json {
        let text = timeline
            .to_json(&labels)
            .map_err(|e| Error::Write(io::Error::other(e)))?;
        writeln!(out, "{text}").map_err(Error::Write)?;
    } else {
        timeline
            .write_human(&mut out, &labels)
            .map_err(Error::Write)?;
    }
    Ok(timeline.exit_code())
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

/// The flag pairs that clap cannot reject: each one would make one of the two flags do nothing.
fn check_sampling_flags(cli: &Cli, matches: &ArgMatches) -> Result<(), Error> {
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
    Ok(())
}

fn run(cli: &Cli, matches: &ArgMatches) -> Result<u8, Error> {
    check_sampling_flags(cli, matches)?;
    // Before any RPC request, so that a typo in the allow file costs nothing.
    let allow = cli.allow.as_deref().map(read_allow).transpose()?;
    if let Some(path) = &cli.write_allow {
        idl_drift::allow::check_write(path, cli.allow.as_deref()).map_err(Error::Usage)?;
    }
    let idl = read_idl(&cli.idl)?;
    // Before any RPC request: idl-verify checks no Shank account, so such a run checks nothing.
    if cli.upgrade_impact && idl.format == Format::Shank {
        return Err(Error::NothingSampled(
            "--upgrade-impact checks accounts only, and idl-verify does not check the accounts of a Shank IDL".into(),
        ));
    }
    let (samples, records, live) = if let Some(path) = &cli.samples {
        let (samples, warnings) = Samples::from_json_with_warnings(&read(path)?)?;
        for warning in warnings {
            let _ = writeln!(io::stderr(), "warning: {}: {warning}", path.display());
        }
        if nothing_to_check(&samples, cli.upgrade_impact) {
            return Err(Error::NothingSampled(format!(
                "{} holds no samples, or only accounts with no data{}",
                path.display(),
                accounts_only_note(cli)
            )));
        }
        (samples, Vec::new(), None)
    } else {
        let (samples, records, live) = sample_live(cli, &idl)?;
        (samples, records, Some(live))
    };
    let program = cli
        .program
        .as_deref()
        .or(samples.program_id.as_deref())
        .unwrap_or(&idl.address);
    if let (Some(dir), Some(framework)) = (&cli.export_fixtures, cli.fixture_format) {
        let from_file = cli.samples.is_some();
        export_fixtures(
            dir,
            framework.into(),
            program,
            &samples,
            &records,
            from_file,
        )?;
    }
    let reference = match (&cli.reference, &cli.decoder, &cli.compare_idl) {
        (_, _, Some(path)) if cli.upgrade_impact => {
            Some(Reference::for_upgrade(&read_idl(path)?, &idl, &samples))
        }
        (_, _, Some(path)) => Some(Reference::from_idl(&read_idl(path)?, &idl, &samples)),
        (Some(path), _, None) => Some(Reference::from_json(&read(path)?, &samples)?),
        (None, Some(command), None) => {
            let words: Vec<String> = command.split_whitespace().map(str::to_string).collect();
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
    if let Some((list, _)) = &allow {
        report.allow(list);
    }
    let fail_on = cli.fail_on.into();
    if let Some(path) = &cli.write_allow {
        let old = allow.as_ref().map(|(_, text)| text.as_str());
        write_allow(&mut report, path, old, fail_on, cli.prune_allow)?;
    }
    if let Some(path) = &cli.allow {
        warn_unused(&report, path);
    }
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
/// The transaction records are only for a Shipstern fixture export.
fn sample_live<'c>(
    cli: &'c Cli,
    idl: &Idl,
) -> Result<(Samples, Vec<TransactionRecord>, Live<'c>), Error> {
    let url = rpc_url(cli.rpc.as_deref())?;
    let agent = idl_drift::rpc::agent();
    let program = cli.program.as_deref().unwrap_or(&idl.address);
    check_program(program)?;
    let call = |method: &str, params: Value| rpc_call(&agent, url, method, &params);
    let mut limits = Limits::default();
    limits.max_transactions = cli.max_tx;
    limits.stall = cli.stall;
    limits.keep_transactions = cli.fixture_format == Some(Framework::Shipstern);
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
        sampled.records,
        Live {
            summary,
            agent,
            url,
        },
    ))
}

/// Write each fixture file as it is built, and print on stderr what was written and what was
/// left out.
fn export_fixtures(
    dir: &Path,
    format: FixtureFormat,
    program: &str,
    samples: &Samples,
    records: &[TransactionRecord],
    from_file: bool,
) -> Result<(), Error> {
    let save = |path: PathBuf| move |source| Error::Save { path, source };
    let mut write = |name: &str, fill: &mut FillFile<'_>| {
        // The directory is made at the first file, so a run that writes none leaves none.
        std::fs::create_dir_all(dir).map_err(save(dir.to_path_buf()))?;
        let path = dir.join(name);
        let file = std::fs::File::create(&path).map_err(save(path.clone()))?;
        let mut file = io::BufWriter::new(file);
        fill(&mut file)
            .and_then(|()| file.flush())
            .map_err(save(path))
    };
    let mut out = ExportSummary::default();
    let result = write_fixtures(format, program, samples, records, &mut write, &mut out);
    let mut stderr = io::stderr().lock();
    // Before a write error returns, so that its cause is not the only line.
    for line in &out.skipped {
        let _ = writeln!(stderr, "warning: fixture export: {}", escape_control(line));
    }
    result?;
    if out.files() == 0 {
        let needs = match format {
            FixtureFormat::Shipstern if from_file => "from a samples file it needs an account with a base58 address, because the file keeps no whole transaction",
            FixtureFormat::Shipstern => "it needs an account with a base58 address, or a transaction with a base58 signature",
            FixtureFormat::Carbon => "it needs an account with a base58 address, or an instruction with a base58 signature and keys",
        };
        return Err(Error::Usage(format!(
            "fixture export: no sample gives a {format} fixture file: {needs}"
        )));
    }
    let (second, kind) = match format {
        FixtureFormat::Shipstern => (out.transactions, "transaction"),
        FixtureFormat::Carbon => (out.instructions, "instruction"),
    };
    let _ = writeln!(
        stderr,
        "exported {} account and {second} {kind} files for {format} to {}",
        out.accounts,
        dir.display()
    );
    if format == FixtureFormat::Shipstern && from_file && !samples.instructions.is_empty() {
        let _ = writeln!(
            stderr,
            "note: a samples file keeps no whole transaction, so no <signature>_tx.json is written; export from a live run (--rpc) for those"
        );
    }
    Ok(())
}

fn rpc_url(rpc: Option<&str>) -> Result<&str, Error> {
    let url = rpc.unwrap_or_default().trim();
    if url.is_empty() {
        return Err(Error::Usage(
            "the RPC URL is empty; set --rpc or IDL_VERIFY_RPC_URL".into(),
        ));
    }
    Ok(url)
}

/// A bad key gets an empty answer from the RPC, which would look like a quiet program.
fn check_program(program: &str) -> Result<(), Error> {
    if bs58::decode(program)
        .into_vec()
        .map_or(true, |k| k.len() != 32)
    {
        return Err(Error::Usage(format!(
            "`{program}` is not a base58 program address; check --program or the IDL's address"
        )));
    }
    Ok(())
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

/// The allow file and its text, which `--write-allow` keeps when it updates the file.
fn read_allow(path: &Path) -> Result<(AllowList<Check>, String), Error> {
    let text = read(path)?;
    Ok((parse_allow(&text, path)?, text))
}

fn parse_allow(text: &str, path: &Path) -> Result<AllowList<Check>, Error> {
    AllowList::parse(text).map_err(|source| Error::Allow {
        path: path.to_path_buf(),
        source,
    })
}

const ALLOW_HEADER: &str = "\
# Allow file written by `idl-verify --write-allow`: the findings that the baseline accepts.
# Each line is `<check> <subject>`. To give a reason, add a space, a `#` and the text.
";

/// Write the --allow file, without its unused entries when `prune` is set, then each finding
/// at or above `fail_on` that is not allowed, in JSON report order. Apply it as --allow does.
fn write_allow(
    report: &mut Report,
    path: &Path,
    old: Option<&str>,
    fail_on: Severity,
    prune: bool,
) -> Result<(), Error> {
    let stale = if prune {
        report.unused_allows.clone().unwrap_or_default()
    } else {
        Vec::new()
    };
    let new = report
        .findings
        .iter()
        .filter(|f| !f.allowed && f.severity >= fail_on)
        .map(|f| (&f.check, f.subject.as_str()));
    let baseline =
        idl_drift::allow::Baseline::build(old, stale.iter().map(|e| e.line), ALLOW_HEADER, new);
    for item in &baseline.skipped {
        let _ = writeln!(io::stderr(), "warning: --write-allow: {item}");
    }
    let save = |source| Error::Save {
        path: path.to_path_buf(),
        source,
    };
    idl_drift::allow::write_atomic(path, &baseline.text).map_err(save)?;
    report.allow(&parse_allow(&baseline.text, path)?);
    report.pruned_allows = prune.then_some(stale);
    Ok(())
}

/// Warn on stderr about each entry of the allow file that matches no finding. Call it after
/// the write, so that a pruned entry is reported as removed.
fn warn_unused(report: &Report, path: &Path) {
    let (entries, removed) = match (&report.pruned_allows, &report.unused_allows) {
        (Some(pruned), _) => (pruned.as_slice(), "; --prune-allow removed it"),
        (None, unused) => (unused.as_deref().unwrap_or_default(), ""),
    };
    for entry in entries {
        let _ = writeln!(
            io::stderr(),
            "warning: allow file {}: line {}: no finding matches `{entry}`{removed}",
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
