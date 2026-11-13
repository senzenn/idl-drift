//! `idl-drift stale`: the reports of [`idl_drift::stale::check`].

use std::io::{self, Write};

use idl_drift::markdown::{escape, escape_control};
use idl_drift::onchain;
use idl_drift::stale::{self, ExtendCheck, IdlWrite, Staleness, Status};
use serde::Serialize;

use crate::{rpc_url, written, Failure, Format, StaleArgs};

/// The result of `stale` for one program, as given.
struct Row {
    program: String,
    result: Result<Staleness, stale::Error>,
}

pub(crate) fn run(args: &StaleArgs, format: Format) -> Result<u8, Failure> {
    let url = rpc_url(args.rpc.as_deref(), "stale")?;
    let agent = idl_drift::rpc::agent();
    let source = args.source.map(Into::into);
    let mut asked = std::collections::HashSet::new();
    let rows: Vec<Row> = args
        .programs
        .iter()
        .filter(|program| asked.insert(program.as_str()))
        .map(|program| {
            let call = |m: &str, p: serde_json::Value| {
                idl_drift::rpc::call(&agent, url, m, &p).map_err(onchain::Error::Rpc)
            };
            Row {
                program: program.clone(),
                result: stale::check(call, program, source),
            }
        })
        .collect();
    let code = if rows.iter().any(|r| r.result.is_err()) {
        2
    } else {
        u8::from(
            rows.iter()
                .any(|r| r.result.as_ref().is_ok_and(|s| s.status != Status::Fresh)),
        )
    };
    written(match format {
        Format::Human => print_human(&rows, code),
        Format::Json => print_json(&rows, code),
        Format::Markdown => print_markdown(&rows, code),
    })?;
    Ok(code)
}

/// `(fresh, stale, no IDL, error)` rows.
fn counts(rows: &[Row]) -> (usize, usize, usize, usize) {
    let status = |want: Status| {
        rows.iter()
            .filter(|r| r.result.as_ref().is_ok_and(|s| s.status == want))
            .count()
    };
    let errors = rows.iter().filter(|r| r.result.is_err()).count();
    (
        status(Status::Fresh),
        status(Status::Stale),
        status(Status::NoIdl),
        errors,
    )
}

/// What the extend check found, after the deploy slot.
pub(crate) fn extend_text(check: Option<&ExtendCheck>) -> String {
    match check {
        Some(ExtendCheck::Extend {
            upgrade_slot: Some(slot),
        }) => format!(" (an ExtendProgram; the last upgrade is in slot {slot})"),
        Some(ExtendCheck::Extend { upgrade_slot: None }) => {
            " (an ExtendProgram; no upgrade after the IDL write)".to_string()
        }
        Some(ExtendCheck::Unknown) => " (can be an ExtendProgram)".to_string(),
        _ => String::new(),
    }
}

/// `; <source> <account> last written in slot <slot> (<status>)` for each IDL account.
pub(crate) fn idl_writes_text(idls: &[IdlWrite]) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    for w in idls {
        let _ = write!(
            text,
            "; {} {} last written in slot {} ({})",
            w.source,
            w.account,
            w.slot,
            w.status.text()
        );
    }
    text
}

fn print_human(rows: &[Row], exit_code: u8) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for row in rows {
        match &row.result {
            Ok(s) => writeln!(
                out,
                "{}  {}  deploy slot {}{}{}",
                s.status.text(),
                s.program,
                s.deploy_slot,
                extend_text(s.extend_check.as_ref()),
                idl_writes_text(&s.idls)
            )?,
            Err(e) => writeln!(
                out,
                "error  {}  {}",
                escape_control(&row.program),
                escape_control(&e.to_string())
            )?,
        }
    }
    let (fresh, stale, none, errors) = counts(rows);
    writeln!(
        out,
        "summary: {stale} stale, {fresh} fresh, {none} no on-chain IDL, {errors} errors"
    )?;
    writeln!(out, "exit code: {exit_code}")
}

#[derive(Serialize)]
struct JsonRow<'a> {
    program: &'a str,
    /// `fresh`, `stale`, `no_idl` or `error`.
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    deploy_slot: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extend_check: Option<&'a ExtendCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idls: Option<&'a [IdlWrite]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct JsonCounts {
    fresh: usize,
    stale: usize,
    no_idl: usize,
    error: usize,
}

#[derive(Serialize)]
struct JsonReport<'a> {
    programs: Vec<JsonRow<'a>>,
    counts: JsonCounts,
    exit_code: u8,
}

fn print_json(rows: &[Row], exit_code: u8) -> io::Result<()> {
    let programs = rows
        .iter()
        .map(|row| match &row.result {
            Ok(s) => JsonRow {
                program: &s.program,
                status: s.status.key(),
                deploy_slot: Some(s.deploy_slot),
                extend_check: s.extend_check.as_ref(),
                idls: Some(&s.idls),
                error: None,
            },
            Err(e) => JsonRow {
                program: &row.program,
                status: "error",
                deploy_slot: None,
                extend_check: None,
                idls: None,
                error: Some(e.to_string()),
            },
        })
        .collect();
    let (fresh, stale, no_idl, error) = counts(rows);
    let json = JsonReport {
        programs,
        counts: JsonCounts {
            fresh,
            stale,
            no_idl,
            error,
        },
        exit_code,
    };
    let text = serde_json::to_string_pretty(&json).map_err(io::Error::other)?;
    writeln!(io::stdout().lock(), "{text}")
}

/// GitHub Markdown: a heading, the counts, the exit code, a table with one row per IDL
/// account, and the errors.
fn print_markdown(rows: &[Row], exit_code: u8) -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "### idl-drift stale\n")?;
    let (fresh, stale, none, errors) = counts(rows);
    writeln!(out, "| Status | Programs |\n|---|---:|")?;
    for (title, n) in [
        ("Stale", stale),
        ("No on-chain IDL", none),
        ("Fresh", fresh),
        ("Error", errors),
    ] {
        writeln!(out, "| {title} | {n} |")?;
    }
    let result = match exit_code {
        0 => "each on-chain IDL is fresh",
        1 => "at least one program has a stale IDL or no on-chain IDL",
        _ => "at least one program could not be checked",
    };
    writeln!(out, "\nExit code {exit_code}: {result}.\n")?;
    writeln!(
        out,
        "| Program | Status | Deploy slot | IDL account | Last write slot |\n|---|---|---|---|---:|"
    )?;
    for row in rows {
        let Ok(s) = &row.result else { continue };
        let deploy = format!("{}{}", s.deploy_slot, extend_text(s.extend_check.as_ref()));
        if s.idls.is_empty() {
            let status = s.status.text();
            writeln!(out, "| `{}` | {status} | {deploy} | none | |", s.program)?;
        }
        for w in &s.idls {
            writeln!(
                out,
                "| `{}` | {} | {deploy} | {} `{}` | {} |",
                s.program,
                w.status.text(),
                w.source,
                w.account,
                w.slot
            )?;
        }
    }
    let failed: Vec<(&Row, &stale::Error)> = rows
        .iter()
        .filter_map(|r| r.result.as_ref().err().map(|e| (r, e)))
        .collect();
    if !failed.is_empty() {
        writeln!(out, "\nPrograms that could not be checked:\n")?;
        for (row, e) in failed {
            writeln!(
                out,
                "- {}: {}",
                escape(&row.program),
                escape(&e.to_string())
            )?;
        }
    }
    Ok(())
}
