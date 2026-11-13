//! `idl-drift watch`: one check of each line of the programs file, the change events, and the
//! webhook.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use idl_drift::markdown::escape_control;
use idl_drift::onchain::{self, Source};
use idl_drift::stale::{self, ExtendCheck, IdlWrite, Staleness, Status};
use idl_drift::{diff, Report};
use serde::Serialize;

use super::stale::{extend_text, idl_writes_text};
use super::watch_files::{parse_programs, Entry, Seen, SeenIdl, State};
use crate::{load, rpc_url, written, Failure, JsonCounts, WatchArgs};

/// What each check needs.
struct Context<'a> {
    agent: ureq::Agent,
    url: &'a str,
    webhook: Option<&'a str>,
    save: Option<&'a Path>,
    json: bool,
}

pub(crate) fn run(args: &WatchArgs) -> Result<u8, Failure> {
    let url = rpc_url(args.rpc.as_deref(), "watch")?;
    let text = std::fs::read_to_string(&args.programs)
        .map_err(|e| Failure::Input(format!("read {}: {e}", args.programs.display())))?;
    let base = args.programs.parent().unwrap_or(Path::new(""));
    let entries = parse_programs(&text, base)
        .map_err(|e| Failure::Input(format!("{}: {e}", args.programs.display())))?;
    if entries.is_empty() {
        return Err(Failure::Input(format!(
            "{}: no program to watch",
            args.programs.display()
        )));
    }
    let mut state = match &args.state {
        Some(path) => State::load(path).map_err(Failure::Input)?,
        None => State::default(),
    };
    state.retain(&entries);
    if let Some(dir) = &args.save_onchain {
        std::fs::create_dir_all(dir)
            .map_err(|e| Failure::Input(format!("create {}: {e}", dir.display())))?;
    }
    let ctx = Context {
        agent: idl_drift::rpc::agent(),
        url,
        webhook: args
            .webhook
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty()),
        save: args.save_onchain.as_deref(),
        json: args.json,
    };
    loop {
        let mut failed = false;
        for entry in &entries {
            match check_entry(entry, state.get(entry), &ctx)? {
                Ok(None) => {}
                Ok(Some(seen)) => state.record(seen),
                Err(message) => {
                    failed = true;
                    print_error(entry, &message, ctx.json)?;
                }
            }
        }
        // Each check writes the state, so a killed run repeats at most one check.
        if let Some(path) = &args.state {
            state
                .save(path)
                .map_err(|e| Failure::Input(format!("write {}: {e}", path.display())))?;
        }
        if args.once {
            return Ok(if failed { 2 } else { 0 });
        }
        std::thread::sleep(Duration::from_secs(args.interval));
    }
}

/// The local IDL (old) against the on-chain IDL (new) of one change event.
struct EventDiff<'a> {
    report: &'a Report,
}

impl Serialize for EventDiff<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("EventDiff", 3)?;
        fields.serialize_field("changes", &self.report.changes)?;
        fields.serialize_field("counts", &JsonCounts::from(self.report.counts()))?;
        let highest = self.report.changes.iter().map(|c| c.severity).max();
        fields.serialize_field("highest", &highest)?;
        fields.end()
    }
}

#[derive(Serialize)]
struct EventOnchain<'a> {
    source: Source,
    account: &'a str,
    /// With --save-onchain: the file that holds the IDL text.
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<String>,
}

#[derive(Serialize)]
struct EventPrevious<'a> {
    deploy_slot: u64,
    idls: &'a [SeenIdl],
}

/// One line of `watch --json`, and the webhook body.
#[derive(Serialize)]
struct ChangeEvent<'a> {
    event: &'static str,
    program: &'a str,
    /// The local IDL path as the programs file gives it.
    idl: &'a str,
    idl_path: String,
    /// What the last check saw; null on the first check.
    previous: Option<EventPrevious<'a>>,
    deploy_slot: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    extend_check: Option<&'a ExtendCheck>,
    status: &'static str,
    idls: &'a [IdlWrite],
    onchain: Option<EventOnchain<'a>>,
    diff: Option<EventDiff<'a>>,
}

/// Check one entry. `Ok(Some(seen))` when the slots changed and the event went out, `Err` for
/// a failure of this entry, and `Failure` when stdout fails.
fn check_entry(
    entry: &Entry,
    previous: Option<&Seen>,
    ctx: &Context,
) -> Result<Result<Option<Seen>, String>, Failure> {
    let mut call = |m: &str, p: serde_json::Value| {
        idl_drift::rpc::call(&ctx.agent, ctx.url, m, &p).map_err(onchain::Error::Rpc)
    };
    // The slots cost fewer requests than the extend check, so they go first.
    let observed = match stale::observe(&mut call, &entry.program, entry.source) {
        Ok(observed) => observed,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let seen = Seen::new(entry, &observed);
    if previous.is_some_and(|p| p.same(&seen)) {
        return Ok(Ok(None));
    }
    let now = match observed.into_staleness(&mut call) {
        Ok(now) => now,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let local = match load(&entry.path) {
        Ok(local) => local,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let onchain = if now.status == Status::NoIdl {
        None
    } else {
        match onchain::fetch_with_text(&mut call, &entry.program, entry.source) {
            Ok(found) => Some(found),
            Err(onchain::Error::BothSources {
                program,
                metadata,
                anchor,
            }) => {
                return Ok(Err(format!(
                    "program {program} has two on-chain IDLs, which can differ: {} {metadata} and {} {anchor}; add `metadata` or `anchor` after the local IDL path in the programs file",
                    Source::Metadata,
                    Source::Anchor
                )))
            }
            Err(e) => return Ok(Err(e.to_string())),
        }
    };
    let mut saved = None;
    if let (Some((found, text)), Some(dir)) = (&onchain, ctx.save) {
        // Two lines can read the two IDL accounts of one program.
        let path = dir.join(format!("{}-{}.json", now.program, found.source.key()));
        if let Err(e) = std::fs::write(&path, text) {
            return Ok(Err(format!("write {}: {e}", path.display())));
        }
        saved = Some(path.to_string_lossy().into_owned());
    }
    let report = onchain.as_ref().map(|(o, _)| diff(&local, &o.idl));
    let event = ChangeEvent {
        event: "change",
        program: &now.program,
        idl: &entry.idl,
        idl_path: entry.path.to_string_lossy().into_owned(),
        previous: previous.map(|p| EventPrevious {
            deploy_slot: p.deploy_slot,
            idls: &p.idls,
        }),
        deploy_slot: now.deploy_slot,
        extend_check: now.extend_check.as_ref(),
        status: now.status.key(),
        idls: &now.idls,
        onchain: onchain.as_ref().map(|(o, _)| EventOnchain {
            source: o.source,
            account: &o.account,
            saved,
        }),
        diff: report.as_ref().map(|report| EventDiff { report }),
    };
    let body = serde_json::to_string(&event).map_err(|e| Failure::Write(io::Error::other(e)))?;
    written(if ctx.json {
        writeln!(io::stdout().lock(), "{body}")
    } else {
        print_human(&event, &now, previous)
    })?;
    // The state keeps the old slots after a failed POST, so the next check sends it again.
    if let Some(url) = ctx.webhook {
        if let Err(e) = post_json(url, &body) {
            return Ok(Err(e));
        }
    }
    Ok(Ok(Some(seen)))
}

fn print_human(event: &ChangeEvent, now: &Staleness, previous: Option<&Seen>) -> io::Result<()> {
    let was = match previous {
        None => " (first check)".to_string(),
        Some(p) if p.deploy_slot != now.deploy_slot => format!(" (was {})", p.deploy_slot),
        Some(_) => String::new(),
    };
    let compared = match (&event.onchain, &event.diff) {
        (Some(onchain), Some(compared)) => {
            let (b, d, a, c) = compared.report.counts();
            format!(
                "; the local IDL against the {}: {b} breaking, {d} dangerous, {a} additive, {c} cosmetic",
                onchain.source
            )
        }
        _ => "; no on-chain IDL to compare".to_string(),
    };
    writeln!(
        io::stdout().lock(),
        "change: {} {}: deploy slot {}{}{was}; {}{}{compared}",
        now.program,
        escape_control(event.idl),
        now.deploy_slot,
        extend_text(now.extend_check.as_ref()),
        now.status.text(),
        idl_writes_text(&now.idls)
    )
}

#[derive(Serialize)]
struct ErrorEvent<'a> {
    event: &'static str,
    program: &'a str,
    idl: &'a str,
    idl_path: String,
    error: &'a str,
}

/// An entry that failed: a line on stderr, and with --json an error event on stdout. The
/// webhook gets only change events.
fn print_error(entry: &Entry, message: &str, json: bool) -> Result<(), Failure> {
    let _ = writeln!(
        io::stderr(),
        "error: {} {}: {}",
        entry.program,
        escape_control(&entry.idl),
        escape_control(message)
    );
    if json {
        let event = ErrorEvent {
            event: "error",
            program: &entry.program,
            idl: &entry.idl,
            idl_path: entry.path.to_string_lossy().into_owned(),
            error: message,
        };
        let body =
            serde_json::to_string(&event).map_err(|e| Failure::Write(io::Error::other(e)))?;
        written(writeln!(io::stdout().lock(), "{body}"))?;
    }
    Ok(())
}

/// A webhook that does not answer in this time is likely down.
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// POST the JSON text `body` to `url` one time. A 2xx answer is a success. The error text
/// never holds `url`, which can hold a secret.
fn post_json(url: &str, body: &str) -> Result<(), String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(WEBHOOK_TIMEOUT))
            .http_status_as_error(false)
            .build(),
    );
    let request = agent.post(url).header("content-type", "application/json");
    match request.send(body) {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(format!("webhook: HTTP {}", response.status().as_u16())),
        Err(
            e @ (ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed),
        ) => Err(format!("webhook: {e}")),
        Err(_) => Err(
            "webhook: the request failed; check that the webhook URL is a valid http(s) URL".into(),
        ),
    }
}
