//! Run a decoder under test as a child process and collect what it decodes.
//!
//! One JSON line per sample goes to the child's stdin: accounts, then instructions, then events.
//! `{"kind":"account","program":"<base58>","address":"...","data":"<base64>"}`,
//! `{"kind":"instruction","program":"<base58>","signature":"...","data":"<base64>","accounts":["<base58>"]}`,
//! `{"kind":"event","program":"<base58>","signature":"...","source":"log"|"cpi","data":"<base64>"}`.
//! Event data is `[discriminator][payload]`: idl-verify removes a self-CPI envelope first.
//! The child answers one line per sample, in order: `{"status":"ok","value":...}`,
//! `{"status":"error","error":"..."}` or `{"status":"skip","reason":"..."}`.
//! An instruction or event value is `{"name":"...","data":{...}}`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use idl_drift::model::Idl;
use serde_json::{json, Value as Json};

use crate::classify::{envelopes, event_body};
use crate::error::Error;
use crate::reference::{Outcome, Reference};
use crate::samples::Samples;

/// Longest wait for one reply; a parser decodes a sample in microseconds.
const REPLY_TIMEOUT: Duration = Duration::from_mins(1);

/// Spawn `command` and decode every sample through it. `idl` gives the self-CPI envelopes to
/// remove from events.
///
/// # Errors
///
/// When the child cannot start, closes early, or prints a line that is not a reply.
pub fn run_decoder(
    command: &[String],
    samples: &Samples,
    program_id: &str,
    idl: &Idl,
) -> Result<Reference, Error> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| Error::Decoder("no command given".into()))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Decoder(format!("cannot start `{program}`: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Decoder("no stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Decoder("no stdout".into()))?;

    let envelopes = envelopes(idl);
    let requests: Vec<Json> = samples
        .accounts
        .iter()
        .map(|a| {
            json!({ "kind": "account", "program": program_id, "address": a.address,
                    "data": STANDARD.encode(&a.data) })
        })
        .chain(samples.instructions.iter().map(|ix| {
            json!({ "kind": "instruction", "program": program_id, "signature": ix.signature,
                    "data": STANDARD.encode(&ix.data), "accounts": ix.accounts })
        }))
        .chain(samples.events.iter().map(|e| {
            // A sample without the IDL's envelope is already an error; send it whole.
            let body = event_body(e, &envelopes).unwrap_or(&e.data);
            json!({ "kind": "event", "program": program_id, "signature": e.signature,
                    "source": e.source.as_str(), "data": STANDARD.encode(body) })
        }))
        .collect();
    // Write on another thread: a child that answers as it reads would otherwise fill its
    // stdout pipe while this side still writes.
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for request in requests {
            writeln!(stdin, "{request}")?;
        }
        Ok(())
    });

    // Read on a third thread, so a decoder that stops answering cannot hang this process.
    let (lines_tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if lines_tx.send(line).is_err() {
                return;
            }
        }
    });

    let expected = samples.accounts.len() + samples.instructions.len() + samples.events.len();
    let mut outcomes = Vec::with_capacity(expected);
    while outcomes.len() < expected {
        match lines.recv_timeout(REPLY_TIMEOUT) {
            Ok(Ok(line)) if line.trim().is_empty() => {}
            Ok(Ok(line)) => outcomes.push(parse_reply(&line)?),
            Ok(Err(e)) => return Err(Error::Decoder(format!("read: {e}"))),
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                return Err(Error::Decoder(format!(
                    "no reply within {} s after {} of {expected} samples; the decoder was stopped",
                    REPLY_TIMEOUT.as_secs(),
                    outcomes.len()
                )));
            }
            // The decoder closed its output; its exit status says more than a broken pipe.
            Err(RecvTimeoutError::Disconnected) => {
                let status = child
                    .wait()
                    .map_or_else(|e| e.to_string(), |s| s.to_string());
                return Err(Error::Decoder(format!(
                    "the decoder stopped ({status}) after answering {} of {expected} samples",
                    outcomes.len()
                )));
            }
        }
    }
    // The protocol does not require the child to exit after its last reply, so stop it. Stop it
    // first: a child that no longer reads stdin can block the writer.
    let _ = child.kill();
    // Every reply is in, so a write error here means only that the child stopped reading.
    let _ = writer.join();
    let _ = child.wait();
    let mut instructions = outcomes.split_off(samples.accounts.len());
    let events = instructions.split_off(samples.instructions.len());
    Ok(Reference::from_decoder(outcomes, instructions, events))
}

fn parse_reply(line: &str) -> Result<Outcome, Error> {
    let excerpt = || line.chars().take(80).collect::<String>();
    let reply: Json = serde_json::from_str(line)
        .map_err(|e| Error::Decoder(format!("bad reply line `{}`: {e}", excerpt())))?;
    match reply["status"].as_str() {
        Some("ok") => Ok(Outcome::Ok(reply["value"].clone())),
        Some("error") => Ok(Outcome::Failed(
            reply["error"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string(),
        )),
        Some("skip") => Ok(Outcome::Skipped(
            reply["reason"]
                .as_str()
                .unwrap_or("no reason given")
                .to_string(),
        )),
        _ => Err(Error::Decoder(format!(
            "reply has no status: {}",
            excerpt()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::EVENT_IX_TAG;
    use crate::samples::{AccountSample, EventSample, EventSource, InstructionSample};

    fn idl() -> Idl {
        crate::testing::idl(r#""instructions":[],"events":[{"name":"Ev","discriminator":[7]}]"#)
    }

    fn samples() -> Samples {
        Samples {
            accounts: vec![AccountSample {
                address: "A".into(),
                data: vec![1, 2],
            }],
            instructions: vec![InstructionSample {
                signature: "S".into(),
                data: vec![3],
                accounts: vec!["K".into()],
                flags: None,
            }],
            ..Samples::default()
        }
    }

    #[test]
    fn replies_line_up_with_samples_and_errors_are_kept() {
        // `sh` checks that the program id arrives, then answers ok and error.
        let script = r#"read a; case "$a" in *'"program":"P"'*) echo '{"status":"ok","value":{"x":1}}';; *) echo '{"status":"error","error":"no program"}';; esac; read b; echo '{"status":"error","error":"nope"}'"#;
        let command = ["sh".to_string(), "-c".to_string(), script.to_string()];
        let reference = run_decoder(&command, &samples(), "P", &idl()).unwrap();
        assert_eq!(reference.accounts, [Outcome::Ok(json!({"x": 1}))]);
        assert_eq!(reference.instructions, [Outcome::Failed("nope".into())]);
        assert!(reference.honors_padding);
    }

    #[test]
    fn early_exit_and_garbage_name_the_cause() {
        let run = |script: &str| {
            let command = ["sh".to_string(), "-c".to_string(), script.to_string()];
            run_decoder(&command, &samples(), "P", &idl())
                .unwrap_err()
                .to_string()
        };
        let early = run("exit 3");
        assert!(
            early.contains("stopped (exit status: 3) after answering 0 of 2"),
            "{early}"
        );
        let garbage = run("read a; echo 'not json'");
        assert!(garbage.contains("bad reply line `not json`"), "{garbage}");
    }

    #[test]
    fn a_decoder_that_runs_on_after_its_last_reply_is_stopped() {
        let script = r#"read a; echo '{"status":"ok","value":1}'; read b; echo '{"status":"ok","value":2}'; exec sleep 30"#;
        let command = ["sh".to_string(), "-c".to_string(), script.to_string()];
        let start = std::time::Instant::now();
        run_decoder(&command, &samples(), "P", &idl()).unwrap();
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_short_answer_is_an_error_not_a_misalignment() {
        let command = [
            "sh".to_string(),
            "-c".to_string(),
            "read a; echo '{\"status\":\"ok\",\"value\":1}'".to_string(),
        ];
        let err = run_decoder(&command, &samples(), "P", &idl())
            .unwrap_err()
            .to_string();
        assert!(err.contains("after answering 1 of 2"), "{err}");
    }

    #[test]
    fn events_go_out_without_their_envelope_and_skips_are_kept() {
        let mut samples = samples();
        samples.events = vec![EventSample {
            signature: "E".into(),
            source: EventSource::Cpi,
            data: [EVENT_IX_TAG.as_slice(), &[7, 1]].concat(),
        }];
        // The event is ok only if it arrives as `[7, 1]` (base64 `BwE=`), without the tag.
        let script = r#"read a; echo '{"status":"skip","reason":"no accounts"}'; read b; echo '{"status":"error","error":"nope"}'; read c; if echo "$c" | grep -q '"kind":"event"' && echo "$c" | grep -q '"source":"cpi"' && echo "$c" | grep -q '"data":"BwE="'; then echo '{"status":"ok","value":1}'; else echo '{"status":"error","error":"bad event"}'; fi"#;
        let command = ["sh".to_string(), "-c".to_string(), script.to_string()];
        let reference = run_decoder(&command, &samples, "P", &idl()).unwrap();
        assert_eq!(reference.accounts, [Outcome::Skipped("no accounts".into())]);
        assert_eq!(reference.instructions, [Outcome::Failed("nope".into())]);
        assert_eq!(reference.events, [Outcome::Ok(json!(1))]);
    }
}
