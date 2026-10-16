//! A Shipstern parser as the decoder under test for `idl-verify --decoder`.
//!
//! It reads one JSON request per line on stdin and writes one JSON reply per line on stdout,
//! as `docs/decoder-protocol.md` says.

use std::io::{BufRead, Write};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use shipstern_core::{
    instruction::InstructionUpdate, AccountUpdate, AccountUpdateInfo, KeyBytes, Parser,
};

mod parser {
    shipstern_proc_macro::include_shipstern_parser!("../../../fixtures/live/pump_fun/idl.json");
}
use parser::pump::{resolve_event_default, AccountParser, InstructionParser};

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Shipstern serializes a parse result as `{"<kind>": {"<Variant>": inner}}`.
fn variant(value: Value, kind: &str) -> Result<(String, Value), String> {
    let only = value
        .get(kind)
        .and_then(Value::as_object)
        .filter(|o| o.len() == 1);
    only.and_then(|o| o.iter().next())
        .map(|(name, inner)| (name.clone(), inner.clone()))
        .ok_or_else(|| format!("unexpected {kind} shape: {value}"))
}

/// The protocol shape of an instruction or event: its name and its arguments.
fn named(value: Value, kind: &str) -> Result<Value, String> {
    let (name, inner) = variant(value, kind)?;
    Ok(json!({"name": name, "data": inner.get("args").cloned().unwrap_or_else(|| json!({}))}))
}

fn key(text: &str) -> Result<[u8; 32], String> {
    let bytes = bs58::decode(text)
        .into_vec()
        .map_err(|e| format!("bad key {text:?}: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| format!("bad key length: {text:?}"))
}

async fn account(req: &Value, data: Vec<u8>) -> Result<Value, String> {
    let update = AccountUpdate {
        account: Some(AccountUpdateInfo {
            // The protocol allows an empty address. The parser does not read it.
            pubkey: key(req["address"].as_str().unwrap_or_default())
                .unwrap_or_default()
                .to_vec(),
            owner: key(req["program"].as_str().unwrap_or_default())?.to_vec(),
            data,
            ..Default::default()
        }),
        ..Default::default()
    };
    let parsed = AccountParser
        .parse(&update)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let (_, fields) = variant(to_json(&parsed)?, "account")?;
    Ok(fields)
}

async fn instruction(req: &Value, data: Vec<u8>) -> Result<Value, String> {
    let accounts = req["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| key(k.as_str().unwrap_or_default()).map(KeyBytes))
        .collect::<Result<_, _>>()?;
    let update = InstructionUpdate {
        program: KeyBytes(key(req["program"].as_str().unwrap_or_default())?),
        accounts,
        data,
        shared: Default::default(),
        inner: Vec::new(),
        // A sample is one instruction, not a transaction: a top-level path and no log lines.
        path: vec![0u32].into(),
        log_range: 0..0,
    };
    let parsed = InstructionParser
        .parse(&update)
        .await
        .map_err(|e| format!("{e:?}"))?;
    named(to_json(&parsed.instruction)?, "instruction")
}

fn event(data: &[u8]) -> Result<Value, String> {
    let parsed = resolve_event_default(&[], data).map_err(|e| format!("{e:?}"))?;
    named(to_json(&parsed)?, "event")
}

async fn reply(line: &str) -> Value {
    let answer = async {
        let req: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let data = STANDARD
            .decode(req["data"].as_str().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        match req["kind"].as_str() {
            Some("account") => account(&req, data).await,
            Some("instruction") => instruction(&req, data).await,
            Some("event") => event(&data),
            other => Err(format!("unknown kind {other:?}")),
        }
    };
    match answer.await {
        Ok(value) => json!({"status": "ok", "value": value}),
        Err(error) => json!({"status": "error", "error": error}),
    }
}

fn main() -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        writeln!(out, "{}", runtime.block_on(reply(&line)))?;
        out.flush()?;
    }
    Ok(())
}
