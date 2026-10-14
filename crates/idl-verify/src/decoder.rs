//! Run a decoder under test as a child process and collect what it decodes.
//!
//! One JSON line per sample goes to the child's stdin, accounts first, then instructions:
//! `{"kind":"account","program":"<base58>","address":"...","data":"<base64>"}` or
//! `{"kind":"instruction","program":"<base58>","signature":"...","data":"<base64>","accounts":["<base58>"]}`.
//! The child answers one line per sample, in order: `{"status":"ok","value":...}` or
//! `{"status":"error","error":"..."}`. An instruction value is `{"name":"...","data":{...}}`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value as Json};

use crate::error::Error;
use crate::reference::{Outcome, Reference};
use crate::samples::Samples;

/// Spawn `command` and decode every sample through it.
///
/// # Errors
///
/// When the child cannot start, closes early, or prints a line that is not a reply.
pub fn run_decoder(
    command: &[String],
    samples: &Samples,
    program_id: &str,
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
        .collect();
    // Write on another thread: a child that answers as it reads would otherwise fill its
    // stdout pipe while this side still writes.
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for request in requests {
            writeln!(stdin, "{request}")?;
        }
        Ok(())
    });

    let expected = samples.accounts.len() + samples.instructions.len();
    let mut outcomes = Vec::with_capacity(expected);
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| Error::Decoder(format!("read: {e}")))?;
        if line.trim().is_empty() {
            continue;
        }
        outcomes.push(parse_reply(&line)?);
        if outcomes.len() == expected {
            break;
        }
    }
    writer
        .join()
        .map_err(|_| Error::Decoder("writer thread panicked".into()))?
        .map_err(|e| Error::Decoder(format!("write: {e}")))?;
    let _ = child.wait();
    if outcomes.len() != expected {
        return Err(Error::Decoder(format!(
            "the decoder answered {} of {expected} samples",
            outcomes.len()
        )));
    }
    let instructions = outcomes.split_off(samples.accounts.len());
    Ok(Reference::from_decoder(outcomes, instructions))
}

fn parse_reply(line: &str) -> Result<Outcome, Error> {
    let reply: Json =
        serde_json::from_str(line).map_err(|e| Error::Decoder(format!("bad reply line: {e}")))?;
    match reply["status"].as_str() {
        Some("ok") => Ok(Outcome::Ok(reply["value"].clone())),
        Some("error") => Ok(Outcome::Failed(
            reply["error"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string(),
        )),
        _ => Err(Error::Decoder(format!(
            "reply has no status: {}",
            line.chars().take(80).collect::<String>()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::{AccountSample, InstructionSample};

    fn samples() -> Samples {
        Samples {
            program_id: None,
            accounts: vec![AccountSample {
                address: "A".into(),
                data: vec![1, 2],
            }],
            instructions: vec![InstructionSample {
                signature: "S".into(),
                data: vec![3],
                accounts: vec!["K".into()],
            }],
        }
    }

    #[test]
    fn replies_line_up_with_samples_and_errors_are_kept() {
        // `sh` answers ok for the account and error for the instruction.
        // `sh` checks that the program id arrives, then answers ok and error.
        let script = r#"read a; case "$a" in *'"program":"P"'*) echo '{"status":"ok","value":{"x":1}}';; *) echo '{"status":"error","error":"no program"}';; esac; read b; echo '{"status":"error","error":"nope"}'"#;
        let command = ["sh".to_string(), "-c".to_string(), script.to_string()];
        let reference = run_decoder(&command, &samples(), "P").unwrap();
        assert_eq!(reference.accounts, [Outcome::Ok(json!({"x": 1}))]);
        assert_eq!(reference.instructions, [Outcome::Failed("nope".into())]);
        assert!(reference.honors_padding);
    }

    #[test]
    fn a_short_answer_is_an_error_not_a_misalignment() {
        let command = [
            "sh".to_string(),
            "-c".to_string(),
            "read a; echo '{\"status\":\"ok\",\"value\":1}'".to_string(),
        ];
        let err = run_decoder(&command, &samples(), "P")
            .unwrap_err()
            .to_string();
        assert!(err.contains("answered 1 of 2"), "{err}");
    }
}
