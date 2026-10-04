use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use idl_drift::model::Idl;
use idl_verify::{verify, Error, Reference, Report, Samples};

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
    #[arg(long, value_name = "PATH")]
    samples: PathBuf,
    /// Reference decoder output to compare values with (see tools/reference).
    #[arg(long, value_name = "PATH")]
    reference: Option<PathBuf>,
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
    let samples = Samples::from_json(&read(&cli.samples)?)?;
    let reference = match &cli.reference {
        Some(path) => Some(Reference::from_json(&read(path)?, &samples)?),
        None => None,
    };

    let report = verify(&idl, &samples, reference.as_ref());
    print(&report, cli.json).map_err(Error::Write)?;
    Ok(report.exit_code())
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
