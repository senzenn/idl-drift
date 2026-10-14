use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use idl_drift::model::Idl;
use idl_verify::rpc::{sample, Limits};
use idl_verify::{run_decoder, verify, Error, Reference, Report, Samples};
use serde_json::{json, Value};

/// Check a Solana program IDL against real account and instruction data.
///
/// Each sample is classified by discriminator and decoded from the IDL alone. Findings
/// cover unknown discriminators, data that fails to decode, bytes the IDL leaves unused
/// (such as zero-copy padding a converter dropped), instructions whose account lists don't
/// match the IDL, and values that disagree with a reference decoder.
#[derive(Parser)]
#[command(name = "idl-verify", version, about, long_about)]
#[command(after_help = "Exit codes:
  0  no errors (warnings and notes may be present)
  1  at least one error
  2  a file could not be read or parsed")]
struct Cli {
    /// Program IDL: Anchor (0.30+ spec) or Codama JSON.
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
    /// Reference decoder output to compare values with (see tools/reference).
    #[arg(long, value_name = "PATH", conflicts_with = "decoder")]
    reference: Option<PathBuf>,
    /// Decoder under test: a command that reads one JSON line per sample on stdin and
    /// answers one per line (see README, "Decoder under test"). Split on whitespace.
    #[arg(long, value_name = "CMD")]
    decoder: Option<String>,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<u8, Error> {
    let idl_src = read(&cli.idl)?;
    let idl = Idl::from_json(&idl_src).map_err(|source| Error::Idl {
        path: cli.idl.clone(),
        source,
    })?;
    let samples = if let Some(path) = &cli.samples {
        Samples::from_json(&read(path)?)?
    } else {
        let url = cli.rpc.as_deref().unwrap_or_default();
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(30)))
                .http_status_as_error(false)
                .build(),
        );
        let program = cli.program.as_deref().unwrap_or(&idl.address);
        let call = |method: &str, params: Value| rpc_call(&agent, url, method, &params);
        sample(call, &idl, program, &Limits::default())?
    };
    let reference = match (&cli.reference, &cli.decoder) {
        (Some(path), _) => Some(Reference::from_json(&read(path)?, &samples)?),
        (None, Some(command)) => {
            let words: Vec<String> = command.split_whitespace().map(str::to_string).collect();
            let program = cli
                .program
                .as_deref()
                .or(samples.program_id.as_deref())
                .unwrap_or(&idl.address);
            Some(run_decoder(&words, &samples, program)?)
        }
        (None, None) => None,
    };

    let report = verify(&idl, &samples, reference.as_ref());
    print(&report, cli.json).map_err(Error::Write)?;
    Ok(report.exit_code())
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

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn print(report: &Report, json: bool) -> io::Result<()> {
    let stdout = io::stdout();
    let color = stdout.is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = stdout.lock();
    if json {
        let text = report.to_json().map_err(io::Error::other)?;
        writeln!(out, "{text}")
    } else {
        report.write_human(&mut out, color)
    }
}
