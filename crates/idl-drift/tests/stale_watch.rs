//! `idl-drift stale` and `idl-drift watch` against a local JSON-RPC server.
// The helpers of the mock server are not tests, so they need the allow too.
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use idl_drift::onchain::{anchor_idl_address, metadata_address, METADATA_PROGRAM};
use idl_drift::stale::{LOADER_V3, LOADER_V4};
use serde_json::{json, Value};

/// What the server knows: accounts, the transactions that name each address, and blocks.
#[derive(Default)]
struct Chain {
    accounts: HashMap<String, (String, Vec<u8>)>,
    /// Per address, newest first: (signature, slot, success).
    history: HashMap<String, Vec<(String, u64, bool)>>,
    /// The signatures of each block.
    blocks: BTreeMap<u64, Vec<String>>,
    transactions: HashMap<String, Value>,
}

type Shared = Arc<Mutex<Chain>>;

fn key(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn b58(key: &[u8]) -> String {
    bs58::encode(key).into_string()
}

/// The upgradeable loader instructions that the tests send, by bincode tag.
const UPGRADE: u8 = 3;
const EXTEND: u8 = 6;

impl Chain {
    /// A transaction in `slot` that names `addresses`, and its signature.
    fn tx(&mut self, slot: u64, ok: bool, addresses: &[String], body: Option<Value>) -> String {
        let signature = format!("Sig{}", self.blocks.values().map(Vec::len).sum::<usize>());
        self.blocks.entry(slot).or_default().push(signature.clone());
        for address in addresses {
            let list = self.history.entry(address.clone()).or_default();
            let at = list
                .iter()
                .position(|(_, s, _)| *s < slot)
                .unwrap_or(list.len());
            list.insert(at, (signature.clone(), slot, ok));
        }
        if let Some(body) = body {
            self.transactions.insert(signature.clone(), body);
        }
        signature
    }

    /// A loader instruction with `tag` on the program at `key(byte)` in `slot`. With `inner`,
    /// another program sends it by CPI, as a multisig does.
    fn loader_tx(&mut self, byte: u8, slot: u64, tag: u8, inner: bool) {
        let (programdata, program) = (b58(&key(byte + 100)), b58(&key(byte)));
        let keys = json!(["Authority", programdata, program, LOADER_V3, "Multisig"]);
        let ix =
            json!({ "programIdIndex": 3, "accounts": [1, 2, 0], "data": b58(&[tag, 0, 0, 0]) });
        let (outer, inner_list) = if inner {
            let call = json!({ "programIdIndex": 4, "accounts": [0], "data": "" });
            (json!([call]), json!([{ "index": 0, "instructions": [ix] }]))
        } else {
            (json!([ix]), json!([]))
        };
        let body = json!({ "slot": slot,
            "transaction": { "signatures": ["x"], "message": { "accountKeys": keys, "instructions": outer } },
            "meta": { "err": null, "innerInstructions": inner_list,
                "loadedAddresses": { "writable": [], "readonly": [] } } });
        self.tx(slot, true, &[programdata, program], Some(body));
    }

    /// An upgradeable program at `key(byte)` with its `ProgramData` at `key(byte + 100)`.
    fn upgradeable(&mut self, byte: u8, slot: u64) -> String {
        let programdata = key(byte + 100);
        let data = [&[2, 0, 0, 0][..], &programdata].concat();
        self.accounts
            .insert(b58(&key(byte)), (LOADER_V3.into(), data));
        self.deploy(byte, slot);
        b58(&key(byte))
    }

    fn programdata_slot(&mut self, byte: u8, slot: u64) {
        let data = [&[3, 0, 0, 0][..], &slot.to_le_bytes(), &[0; 33]].concat();
        self.accounts
            .insert(b58(&key(byte + 100)), (LOADER_V3.into(), data));
    }

    /// An upgrade of the program at `key(byte)` in `slot`.
    fn deploy(&mut self, byte: u8, slot: u64) {
        self.programdata_slot(byte, slot);
        self.loader_tx(byte, slot, UPGRADE, false);
    }

    /// An `ExtendProgram` of the program at `key(byte)` in `slot`, which also sets the slot.
    fn extend(&mut self, byte: u8, slot: u64) {
        self.programdata_slot(byte, slot);
        self.loader_tx(byte, slot, EXTEND, false);
    }

    /// A Program Metadata `idl` account of the program at `key(byte)`, written in `slot`.
    fn metadata_idl(&mut self, byte: u8, idl: &str, slot: u64) -> String {
        let program = key(byte);
        let mut data = vec![2];
        data.extend(program);
        data.extend([0; 32]);
        data.extend([1, 1]);
        data.extend(*b"idl\0\0\0\0\0\0\0\0\0\0\0\0\0");
        data.extend([1, 0, 1, 0]);
        data.extend(u32::try_from(idl.len()).unwrap().to_le_bytes());
        data.extend([0; 5]);
        data.extend(idl.as_bytes());
        let address = b58(&metadata_address(&program).unwrap());
        self.accounts
            .insert(address.clone(), (METADATA_PROGRAM.into(), data));
        self.tx(slot, true, std::slice::from_ref(&address), None);
        address
    }

    /// An Anchor IDL account of the program at `key(byte)`, written in `slot`.
    fn anchor_idl(&mut self, byte: u8, idl: &str, slot: u64) -> String {
        let program = key(byte);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(idl.as_bytes()).unwrap();
        let body = z.finish().unwrap();
        let mut data = vec![24, 70, 98, 191, 58, 144, 123, 158];
        data.extend([7; 32]);
        data.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        data.extend(body);
        let address = b58(&anchor_idl_address(&program).unwrap());
        self.accounts.insert(address.clone(), (b58(&program), data));
        self.tx(slot, true, std::slice::from_ref(&address), None);
        address
    }

    fn account(&self, address: &Value, config: &Value) -> Value {
        let Some((owner, data)) = address.as_str().and_then(|a| self.accounts.get(a)) else {
            return Value::Null;
        };
        let slice = &config["dataSlice"];
        let data = match (slice["offset"].as_u64(), slice["length"].as_u64()) {
            (Some(at), Some(len)) => {
                let at = usize::try_from(at).unwrap().min(data.len());
                let end = at
                    .saturating_add(usize::try_from(len).unwrap())
                    .min(data.len());
                &data[at..end]
            }
            _ => &data[..],
        };
        json!({ "owner": owner, "lamports": 1, "executable": true,
            "data": [STANDARD.encode(data), "base64"] })
    }

    /// `getSignaturesForAddress`: `before` is any signature, as in Agave; the list starts
    /// below its slot when the address did not sign it.
    fn signatures(&self, params: &Value) -> Value {
        let list = params[0]
            .as_str()
            .and_then(|a| self.history.get(a))
            .cloned()
            .unwrap_or_default();
        let config = &params[1];
        let limit = usize::try_from(config["limit"].as_u64().unwrap_or(1000)).unwrap();
        let start = config["before"].as_str().map_or(0, |before| {
            if let Some(at) = list.iter().position(|(s, _, _)| s == before) {
                return at + 1;
            }
            let slot = self
                .blocks
                .iter()
                .find(|(_, sigs)| sigs.iter().any(|s| s == before))
                .map(|(slot, _)| *slot)
                .unwrap();
            list.iter()
                .position(|(_, s, _)| *s < slot)
                .unwrap_or(list.len())
        });
        let page: Vec<Value> = list[start..]
            .iter()
            .take(limit)
            .map(|(signature, slot, ok)| {
                let err = if *ok {
                    Value::Null
                } else {
                    json!({ "InstructionError": [0, "Custom"] })
                };
                json!({ "signature": signature, "slot": slot, "err": err })
            })
            .collect();
        json!(page)
    }

    fn answer(&self, method: &str, params: &Value) -> Value {
        match method {
            "getAccountInfo" => {
                json!({ "context": { "slot": 1 }, "value": self.account(&params[0], &params[1]) })
            }
            "getMultipleAccounts" => {
                let value: Vec<Value> = params[0]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| self.account(a, &params[1]))
                    .collect();
                json!({ "context": { "slot": 1 }, "value": value })
            }
            "getSignaturesForAddress" => self.signatures(params),
            "getBlocksWithLimit" => {
                let (start, limit) = (params[0].as_u64().unwrap(), params[1].as_u64().unwrap());
                let slots: Vec<u64> = self
                    .blocks
                    .range(start..)
                    .map(|(s, _)| *s)
                    .take(usize::try_from(limit).unwrap())
                    .collect();
                json!(slots)
            }
            "getBlock" => json!({ "signatures": self.blocks[&params[0].as_u64().unwrap()] }),
            "getTransaction" => self
                .transactions
                .get(params[0].as_str().unwrap())
                .cloned()
                .unwrap_or(Value::Null),
            _ => panic!("unexpected {method}"),
        }
    }
}

/// Read one HTTP request from `stream` and return its body.
fn read_request(reader: &mut impl BufRead) -> Vec<u8> {
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap();
        }
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    body
}

/// A local server that calls `answer` with each request body, and its URL.
fn serve(answer: impl Fn(&[u8]) -> String + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let body = answer(&read_request(&mut reader));
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    url
}

/// A JSON-RPC server over `chain`.
fn rpc(chain: &Shared) -> String {
    let chain = chain.clone();
    serve(move |body| {
        let request: Value = serde_json::from_slice(body).unwrap();
        let method = request["method"].as_str().unwrap();
        let result = chain.lock().unwrap().answer(method, &request["params"]);
        json!({ "jsonrpc": "2.0", "id": 1, "result": result }).to_string()
    })
}

/// A webhook that keeps each body it gets.
fn webhook() -> (String, Arc<Mutex<Vec<Value>>>) {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let kept = bodies.clone();
    let url = serve(move |body| {
        kept.lock()
            .unwrap()
            .push(serde_json::from_slice(body).unwrap());
        String::new()
    });
    (url, bodies)
}

fn drift(url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_idl-drift"))
        .args(args)
        .env("IDL_DRIFT_RPC_URL", url)
        .env_remove("IDL_DRIFT_WEBHOOK_URL")
        .output()
        .unwrap()
}

fn json_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

const IDL: &str = r#"{"address":"11111111111111111111111111111111","metadata":{"name":"p","version":"0.1.0","spec":"0.1.0"},"instructions":[]}"#;

#[test]
fn stale_tells_fresh_stale_and_no_idl_apart() {
    let chain = Shared::default();
    let (fresh, stale, none, v4) = {
        let mut c = chain.lock().unwrap();
        let fresh = c.upgradeable(1, 100);
        c.anchor_idl(1, IDL, 100);
        let stale = c.upgradeable(2, 100);
        c.metadata_idl(2, IDL, 99);
        let none = c.upgradeable(3, 100);
        let v4 = b58(&key(4));
        let data = [&50u64.to_le_bytes()[..], &[0; 40]].concat();
        c.accounts.insert(v4.clone(), (LOADER_V4.into(), data));
        c.metadata_idl(4, IDL, 60);
        (fresh, stale, none, v4)
    };
    let url = rpc(&chain);

    let out = drift(&url, &["stale", &fresh, &v4, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", json_of(&out));
    let json = json_of(&out);
    assert_eq!(json["programs"][0]["status"], "fresh");
    assert_eq!(json["programs"][0]["idls"][0]["source"], "anchor");
    // Loader v4 keeps the slot at the start of the program account.
    assert_eq!(json["programs"][1]["deploy_slot"], 50);

    let out = drift(&url, &["stale", &stale, &none, &fresh, "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let json = json_of(&out);
    let statuses: Vec<&str> = json["programs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["stale", "no_idl", "fresh"]);
    assert_eq!(
        json["counts"],
        json!({ "fresh": 1, "stale": 1, "no_idl": 1, "error": 0 })
    );
    assert_eq!(json["programs"][0]["idls"][0]["slot"], 99);

    let out = drift(&url, &["stale", &stale]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with(&format!(
            "stale  {stale}  deploy slot 100; Program Metadata account "
        )),
        "{text}"
    );
    assert!(text.contains("last written in slot 99 (stale)\n"), "{text}");
}

#[test]
fn stale_reports_a_missing_program_as_an_error_and_still_checks_the_others() {
    let chain = Shared::default();
    let fresh = {
        let mut c = chain.lock().unwrap();
        let fresh = c.upgradeable(1, 100);
        c.anchor_idl(1, IDL, 101);
        fresh
    };
    let url = rpc(&chain);
    let missing = b58(&key(9));
    let out = drift(&url, &["stale", &missing, &fresh, "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let json = json_of(&out);
    assert_eq!(json["programs"][0]["status"], "error");
    assert!(json["programs"][0]["error"]
        .as_str()
        .unwrap()
        .starts_with(&format!("no account at {missing}")));
    assert_eq!(json["programs"][1]["status"], "fresh");

    let out = drift(&url, &["stale", &fresh, "--markdown"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Exit code 0: each on-chain IDL is fresh."),
        "{text}"
    );
}

#[test]
fn stale_needs_an_rpc_url() {
    let out = drift("", &["stale", &b58(&key(1)), "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        json_of(&out)["error"],
        "stale needs an RPC URL; set IDL_DRIFT_RPC_URL or --rpc"
    );
}

/// A scratch directory with the programs file and the local IDL of one program.
fn workspace(name: &str, program: &str, local: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("idl-drift-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("idls")).unwrap();
    std::fs::write(dir.join("idls/p.json"), local).unwrap();
    std::fs::write(
        dir.join("programs.txt"),
        format!("# watched\n{program} idls/p.json\n"),
    )
    .unwrap();
    dir
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new("../../fixtures/manual").join(name)).unwrap()
}

fn lines(out: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn watch_emits_a_change_once_keeps_the_state_and_posts_the_webhook() {
    let onchain = fixture("02_new_instruction_new.json");
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        c.anchor_idl(1, &onchain, 101);
        program
    };
    let url = rpc(&chain);
    let (hook, posted) = webhook();
    let dir = workspace("watch", &program, &fixture("02_new_instruction_old.json"));
    let (programs, state, saved) = (
        dir.join("programs.txt"),
        dir.join("state.json"),
        dir.join("onchain"),
    );
    let watch = |extra: &[&str]| {
        let mut args = vec![
            "watch",
            "--programs",
            programs.to_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
            "--once",
            "--json",
        ];
        args.extend_from_slice(extra);
        drift(&url, &args)
    };

    let out = watch(&[
        "--webhook",
        &hook,
        "--save-onchain",
        saved.to_str().unwrap(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = lines(&out);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event["event"], "change");
    assert_eq!(
        (event["program"].as_str(), event["idl"].as_str()),
        (Some(program.as_str()), Some("idls/p.json"))
    );
    assert_eq!(event["previous"], Value::Null);
    assert_eq!(
        (event["deploy_slot"].as_u64(), event["status"].as_str()),
        (Some(100), Some("fresh"))
    );
    assert_eq!(event["onchain"]["source"], "anchor");
    assert_eq!(event["diff"]["highest"], "additive");
    assert!(event["diff"]["counts"]["additive"].as_u64().unwrap() > 0);
    let file = event["onchain"]["saved"].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(file).unwrap(), onchain);
    assert_eq!(*posted.lock().unwrap(), events);
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(kept["programs"][0]["deploy_slot"], 100);

    // Nothing changed on chain, so a second run prints and posts nothing.
    let out = watch(&["--webhook", &hook]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(posted.lock().unwrap().len(), 1);

    // A deploy with no IDL update is a change, and the IDL is now stale.
    chain.lock().unwrap().deploy(1, 200);
    let events = lines(&watch(&[]));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["previous"]["deploy_slot"], 100);
    assert_eq!(
        (
            events[0]["deploy_slot"].as_u64(),
            events[0]["status"].as_str()
        ),
        (Some(200), Some("stale"))
    );

    // A new IDL with no deploy is a change too.
    chain.lock().unwrap().anchor_idl(1, &onchain, 201);
    let events = lines(&watch(&[]));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["status"], "fresh");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn watch_keeps_the_old_state_when_the_webhook_or_the_local_idl_fails() {
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        c.metadata_idl(1, IDL, 101);
        program
    };
    let url = rpc(&chain);
    let dir = workspace("watch-fail", &program, IDL);
    let (programs, state) = (dir.join("programs.txt"), dir.join("state.json"));
    let args = |hook: &'static str| {
        vec![
            "watch".to_string(),
            "--programs".into(),
            programs.to_string_lossy().into_owned(),
            "--state".into(),
            state.to_string_lossy().into_owned(),
            "--once".into(),
            "--json".into(),
            "--webhook".into(),
            hook.into(),
        ]
    };
    let run = |hook| {
        let args = args(hook);
        drift(&url, &args.iter().map(String::as_str).collect::<Vec<_>>())
    };

    // Nothing listens on port 1.
    let out = run("http://127.0.0.1:1/t0ken");
    assert_eq!(out.status.code(), Some(2));
    let events = lines(&out);
    assert_eq!(events.len(), 2);
    assert_eq!(
        (events[0]["event"].as_str(), events[1]["event"].as_str()),
        (Some("change"), Some("error"))
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("webhook: "), "{stderr}");
    assert!(!stderr.contains("t0ken"), "{stderr}");
    let (hook, posted) = webhook();
    let hook: &'static str = hook.leak();
    let out = run(hook);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(lines(&out).len(), 1);
    assert_eq!(posted.lock().unwrap().len(), 1);

    std::fs::remove_file(dir.join("idls/p.json")).unwrap();
    chain.lock().unwrap().deploy(1, 300);
    let out = run(hook);
    assert_eq!(out.status.code(), Some(2));
    let events = lines(&out);
    assert_eq!(events[0]["event"], "error");
    assert!(
        events[0]["error"].as_str().unwrap().starts_with("read "),
        "{}",
        events[0]
    );
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(kept["programs"][0]["deploy_slot"], 100);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn watch_rejects_a_bad_programs_file_before_any_request() {
    let dir = workspace("watch-bad", "nope", IDL);
    let programs = dir.join("programs.txt");
    let out = drift(
        "http://127.0.0.1:1",
        &["watch", "--programs", programs.to_str().unwrap(), "--once"],
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("line 2: `nope` is not a base58 program address"),
        "{stderr}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn watch_needs_a_source_for_a_program_with_two_idls() {
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        c.metadata_idl(1, IDL, 101);
        c.anchor_idl(1, IDL, 90);
        program
    };
    let url = rpc(&chain);
    let dir = workspace("watch-both", &program, IDL);
    let programs = dir.join("programs.txt");
    let watch = || {
        drift(
            &url,
            &[
                "watch",
                "--programs",
                programs.to_str().unwrap(),
                "--once",
                "--json",
            ],
        )
    };
    let events = lines(&watch());
    assert_eq!(events[0]["event"], "error");
    assert!(
        events[0]["error"]
            .as_str()
            .unwrap()
            .ends_with("add `metadata` or `anchor` after the local IDL path in the programs file"),
        "{}",
        events[0]
    );
    std::fs::write(&programs, format!("{program} idls/p.json anchor\n")).unwrap();
    let out = watch();
    assert_eq!(out.status.code(), Some(0));
    let events = lines(&out);
    assert_eq!(
        (
            events[0]["status"].as_str(),
            events[0]["onchain"]["source"].as_str()
        ),
        (Some("stale"), Some("anchor"))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stale_skips_a_failed_transaction_on_the_idl_account() {
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        let account = c.anchor_idl(1, IDL, 50);
        c.tx(120, false, &[account], None);
        program
    };
    let out = drift(&rpc(&chain), &["stale", &program, "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", json_of(&out));
    assert_eq!(json_of(&out)["programs"][0]["idls"][0]["slot"], 50);
}

#[test]
fn a_source_picks_the_idl_account_and_each_account_has_a_status() {
    // A program that moved to the Program Metadata IDL keeps its old Anchor IDL account.
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        c.metadata_idl(1, IDL, 101);
        c.anchor_idl(1, IDL, 90);
        program
    };
    let url = rpc(&chain);
    let out = drift(&url, &["stale", &program, "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let statuses: Vec<Value> = json_of(&out)["programs"][0]["idls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["status"].clone())
        .collect();
    assert_eq!(statuses, ["fresh", "stale"]);
    let out = drift(&url, &["stale", "--source", "metadata", &program, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", json_of(&out));
    assert_eq!(
        json_of(&out)["programs"][0]["idls"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let dir = workspace("watch-source", &program, IDL);
    let programs = dir.join("programs.txt");
    std::fs::write(&programs, format!("{program} idls/p.json metadata\n")).unwrap();
    let out = drift(
        &url,
        &[
            "watch",
            "--programs",
            programs.to_str().unwrap(),
            "--once",
            "--json",
        ],
    );
    let events = lines(&out);
    assert_eq!(events[0]["status"], "fresh", "{}", events[0]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn watch_saves_the_idl_of_each_source_in_its_own_file() {
    let other = IDL.replace(r#""name":"p""#, r#""name":"q""#);
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        c.metadata_idl(1, IDL, 101);
        c.anchor_idl(1, &other, 102);
        program
    };
    let dir = workspace("watch-two-files", &program, IDL);
    std::fs::write(dir.join("idls/q.json"), IDL).unwrap();
    let programs = dir.join("programs.txt");
    let lines_text = format!("{program} idls/p.json metadata\n{program} idls/q.json anchor\n");
    std::fs::write(&programs, lines_text).unwrap();
    let saved = dir.join("onchain");
    let out = drift(
        &rpc(&chain),
        &[
            "watch",
            "--programs",
            programs.to_str().unwrap(),
            "--once",
            "--json",
            "--save-onchain",
            saved.to_str().unwrap(),
        ],
    );
    let events = lines(&out);
    assert_eq!(events.len(), 2, "{}", String::from_utf8_lossy(&out.stderr));
    for (event, text) in events.iter().zip([IDL, other.as_str()]) {
        let file = event["onchain"]["saved"].as_str().unwrap();
        assert_eq!(std::fs::read_to_string(file).unwrap(), text, "{event}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stale_does_not_count_an_extend_as_a_deploy() {
    // ExtendProgram also sets the ProgramData slot (Agave `common_extend_program`).
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        c.anchor_idl(1, IDL, 105);
        c.extend(1, 200);
        program
    };
    let url = rpc(&chain);
    let check = || {
        let out = drift(&url, &["stale", &program, "--json"]);
        let row = json_of(&out)["programs"][0].clone();
        (
            out.status.code(),
            row["status"].clone(),
            row["extend_check"].clone(),
        )
    };
    assert_eq!(
        check(),
        (
            Some(0),
            json!("fresh"),
            json!({ "result": "extend", "upgrade_slot": null })
        )
    );
    // An upgrade by CPI, as from a multisig.
    {
        let mut c = chain.lock().unwrap();
        c.programdata_slot(1, 300);
        c.loader_tx(1, 300, UPGRADE, true);
    }
    assert_eq!(
        check(),
        (Some(1), json!("stale"), json!({ "result": "upgrade" }))
    );
    chain.lock().unwrap().extend(1, 400);
    assert_eq!(
        check(),
        (
            Some(1),
            json!("stale"),
            json!({ "result": "extend", "upgrade_slot": 300 })
        )
    );
}

#[test]
fn the_extend_check_stops_at_its_limit() {
    let chain = Shared::default();
    let program = {
        let mut c = chain.lock().unwrap();
        let program = c.upgradeable(1, 100);
        c.anchor_idl(1, IDL, 105);
        // Other programs name the ProgramData account too.
        for slot in 110..125 {
            c.tx(slot, true, &[b58(&key(101))], None);
        }
        c.extend(1, 200);
        program
    };
    let url = rpc(&chain);
    let out = drift(&url, &["stale", &program, "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        json_of(&out)["programs"][0]["extend_check"],
        json!({ "result": "unknown" })
    );
    let text = String::from_utf8_lossy(&drift(&url, &["stale", &program]).stdout).into_owned();
    assert!(
        text.contains("deploy slot 200 (can be an ExtendProgram)"),
        "{text}"
    );
}

#[test]
fn the_extend_check_says_unknown_when_it_cannot_see_the_history() {
    let chain = Shared::default();
    let (pruned, short) = {
        let mut c = chain.lock().unwrap();
        // The upgrade in slot 150 is pruned, so getTransaction gives null for it.
        let pruned = c.upgradeable(1, 100);
        c.anchor_idl(1, IDL, 105);
        c.programdata_slot(1, 150);
        c.tx(150, true, &[b58(&key(101)), pruned.clone()], None);
        c.extend(1, 200);
        // The history of the ProgramData account ends above the IDL write.
        let short = b58(&key(2));
        let data = [&[2, 0, 0, 0][..], &key(102)].concat();
        c.accounts.insert(short.clone(), (LOADER_V3.into(), data));
        c.anchor_idl(2, IDL, 105);
        c.extend(2, 200);
        (pruned, short)
    };
    let url = rpc(&chain);
    for program in [pruned, short] {
        let out = drift(&url, &["stale", &program, "--json"]);
        let row = json_of(&out)["programs"][0].clone();
        assert_eq!(out.status.code(), Some(1), "{row}");
        assert_eq!(row["extend_check"], json!({ "result": "unknown" }), "{row}");
    }
}
