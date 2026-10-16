//! A Carbon decoder as the decoder under test for `idl-verify --decoder`.
//!
//! It reads one JSON request per line on stdin and writes one JSON reply per line on stdout,
//! as `docs/decoder-protocol.md` says.

use std::io::{BufRead, Write};
use std::str::FromStr;

use base64::{engine::general_purpose::STANDARD, Engine};
use carbon_core::{account::AccountDecoder, instruction::InstructionDecoder};
use carbon_pumpfun_decoder::{instructions::CpiEvent, PumpfunDecoder};
use serde_json::{json, Value};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

/// Anchor's self-CPI event tag. `CpiEvent::decode` expects it in front of the event.
const EVENT_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

fn key(text: &str) -> Result<Pubkey, String> {
    Pubkey::from_str(text).map_err(|e| format!("bad key {text:?}: {e}"))
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn account(req: &Value, data: Vec<u8>) -> Result<Value, String> {
    let owner = key(req["program"].as_str().unwrap_or_default())?;
    let account = solana_account::Account {
        data,
        owner,
        ..Default::default()
    };
    let decoded = PumpfunDecoder
        .decode_account(&account)
        .ok_or("no account type matched")?;
    // Carbon serializes the account enum as `{"type": <Name>, "data": {fields}}`.
    Ok(to_json(&decoded.data)?["data"].clone())
}

fn instruction(req: &Value, data: Vec<u8>) -> Result<Value, String> {
    let accounts = req["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| key(k.as_str().unwrap_or_default()).map(|k| AccountMeta::new_readonly(k, false)))
        .collect::<Result<_, _>>()?;
    let program_id = key(req["program"].as_str().unwrap_or_default())?;
    let ix = Instruction {
        program_id,
        accounts,
        data,
    };
    let decoded = to_json(
        &PumpfunDecoder
            .decode_instruction(&ix)
            .ok_or("no instruction matched")?,
    )?;
    // Carbon serializes the instruction enum as `{"type": <Name>, "data": {"data": {args}, ...}}`.
    let mut args = decoded["data"]["data"].clone();
    // Carbon keeps an Anchor struct argument (such as `params`) as one field. The protocol
    // accepts the struct itself. A lone enum argument has PascalCase keys, so it stays wrapped.
    if let Value::Object(o) = &args {
        if let (1, Some(Value::Object(inner))) = (o.len(), o.values().next()) {
            if !inner
                .keys()
                .any(|k| k.starts_with(|c: char| c.is_ascii_uppercase()))
            {
                args = Value::Object(inner.clone());
            }
        }
    }
    Ok(json!({"name": decoded["type"], "data": args}))
}

fn event(data: &[u8]) -> Result<Value, String> {
    let decoded = CpiEvent::decode(&[&EVENT_TAG[..], data].concat()).ok_or("no event matched")?;
    // Carbon serializes the event enum as `{"<Name>": {fields}}`.
    match to_json(&decoded)? {
        Value::Object(o) if o.len() == 1 => o
            .into_iter()
            .next()
            .map(|(name, data)| json!({"name": name, "data": data}))
            .ok_or_else(|| "empty event".to_string()),
        other => Err(format!("unexpected event shape: {other}")),
    }
}

fn reply(line: &str) -> Value {
    let answer = (|| {
        let req: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let data = STANDARD
            .decode(req["data"].as_str().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        match req["kind"].as_str() {
            Some("account") => account(&req, data),
            Some("instruction") => instruction(&req, data),
            Some("event") => event(&data),
            other => Err(format!("unknown kind {other:?}")),
        }
    })();
    match answer {
        Ok(value) => json!({"status": "ok", "value": value}),
        Err(error) => json!({"status": "error", "error": error}),
    }
}

fn main() -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        writeln!(out, "{}", reply(&line))?;
        out.flush()?;
    }
    Ok(())
}
