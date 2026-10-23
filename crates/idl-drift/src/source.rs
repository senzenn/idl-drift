//! The program-id check of `idl-drift source`. An Anchor workspace keeps the program id in
//! `declare_id!`, in the `[programs.<cluster>]` tables of `Anchor.toml` and in the IDL
//! `address`. A new keypair or a move to another cluster can change one copy and not the
//! others.
//!
//! ```no_run
//! use idl_drift::source::{check, discover};
//!
//! let report = check(discover(std::path::Path::new("."))?);
//! std::process::exit(i32::from(report.fails_at(idl_drift::Severity::Breaking)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use heck::ToSnakeCase;
use serde::Serialize;
use serde_json::{json, Value};

use crate::onchain::{self, Source};
use crate::Severity;

/// The most bytes that [`discover`] and [`read_idl`] read from one file.
pub const MAX_FILE_BYTES: usize = 16 << 20;

/// Where a program id was read from. Paths are relative to the workspace root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "leg", rename_all = "snake_case")]
pub enum Leg {
    /// A `declare_id!` in the program's `src/lib.rs`.
    DeclareId { path: String },
    /// An entry of a `[programs.<cluster>]` table.
    AnchorToml { path: String, cluster: String },
    /// The `address` of an IDL.
    IdlAddress { path: String },
    /// The `address` of the IDL that the program publishes on chain. `source` is
    /// [`Source::key`] and `account` is the address of the IDL account.
    Onchain { source: String, account: String },
}

/// One program id and where it was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProgramId {
    /// The program name as its source writes it.
    pub program: String,
    #[serde(flatten)]
    pub leg: Leg,
    /// Base58. Empty when the file was read but holds no id: a `lib.rs` with no
    /// `declare_id!`, or a legacy IDL with no address.
    pub id: String,
}

/// The rule a finding came from. The names in JSON output are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    InvalidId,
    DeclareIdMissing,
    DeclareIdDuplicate,
    AnchorTomlMismatch,
    IdlAddressMismatch,
    IdlAddressMissing,
    OnchainMismatch,
    OnchainMissing,
    OnchainIdlMissing,
}

/// One problem with the program ids of a program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub kind: FindingKind,
    /// The program name, in lowercase with `_` for `-`.
    pub program: String,
    /// What the finding is about: a program, `<program>@<cluster>` or a file.
    pub subject: String,
    pub message: String,
}

/// The ids that were read and the findings on them, ordered by program.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SourceReport {
    pub ids: Vec<ProgramId>,
    pub findings: Vec<Finding>,
}

impl SourceReport {
    /// `true` when a finding is at `threshold` or above.
    #[must_use]
    pub fn fails_at(&self, threshold: Severity) -> bool {
        self.findings.iter().any(|f| f.severity >= threshold)
    }

    /// `true` when a program has a valid id in two places, the chain included, so the check
    /// compared something. A wrong ROOT reads no file, and a pass on it would mean nothing.
    #[must_use]
    pub fn compared(&self) -> bool {
        let mut valid: BTreeMap<String, usize> = BTreeMap::new();
        for id in self.ids.iter().filter(|id| is_program_id(&id.id)) {
            *valid.entry(program_key(&id.program)).or_default() += 1;
        }
        valid.values().any(|&n| n > 1)
    }

    /// The number of findings per tier: `(breaking, dangerous, additive, cosmetic)`.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for f in &self.findings {
            match f.severity {
                Severity::Breaking => counts.0 += 1,
                Severity::Dangerous => counts.1 += 1,
                Severity::Additive => counts.2 += 1,
                Severity::Cosmetic => counts.3 += 1,
            }
        }
        counts
    }
}

/// Why [`discover`] or [`read_idl`] cannot read a file.
#[derive(Debug)]
pub enum SourceError {
    /// A file or directory exists but cannot be read.
    Read { path: String, source: io::Error },
    /// A file does not have the expected content.
    Parse { path: String, message: String },
    /// A file resolves, through a symlink, to a path outside the workspace root.
    OutsideRoot { path: String },
    /// A path that must be a regular file is a directory, a FIFO or a device.
    NotAFile { path: String },
    /// A file is larger than `limit` bytes.
    TooLarge { path: String, limit: usize },
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "read {path}: {source}"),
            Self::Parse { path, message } => write!(f, "parse {path}: {message}"),
            Self::OutsideRoot { path } => {
                write!(f, "{path} resolves to a path outside the workspace root")
            }
            Self::NotAFile { path } => write!(f, "{path} is not a regular file"),
            Self::TooLarge { path, limit } => write!(f, "{path} is larger than {limit} bytes"),
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { .. }
            | Self::OutsideRoot { .. }
            | Self::NotAFile { .. }
            | Self::TooLarge { .. } => None,
        }
    }
}

/// The string of each `declare_id!("...")` at the crate root of `rust`, in order. Comments
/// and string literals are skipped, so an old id in a comment does not count. A
/// `declare_id!` in a module, such as `mod jupiter { declare_id!(..) }`, names a CPI target.
fn declare_ids(rust: &str) -> Vec<String> {
    let tokens = rust_tokens(rust);
    let mut depth = 0usize;
    let mut ids = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::Punct(b'{') => depth += 1,
            Token::Punct(b'}') => depth = depth.saturating_sub(1),
            Token::Ident("declare_id") if depth == 0 => {
                if let [Token::Punct(b'!'), Token::Punct(b'(' | b'[' | b'{'), Token::Str(id), ..] =
                    &tokens[i + 1..]
                {
                    ids.push((*id).to_string());
                }
            }
            _ => {}
        }
    }
    ids
}

/// The `(cluster, program, id)` entries of the `[programs.<cluster>]` tables of an
/// `Anchor.toml`, in file order. Other tables are ignored.
fn anchor_toml_programs(text: &str) -> Result<Vec<(String, String, String)>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut cluster: Option<String> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        match toml_line(raw) {
            TomlLine::Blank => {}
            TomlLine::Table(key, array) => {
                cluster = None;
                if key.first().map(String::as_str) == Some("programs") {
                    match (key.as_slice(), array) {
                        ([_, name], false) => cluster = Some(name.clone()),
                        _ => {
                            return Err(format!(
                            "line {line}: only `[programs.<cluster>]` tables can hold program ids"
                        ))
                        }
                    }
                }
            }
            entry => {
                let Some(cluster) = &cluster else { continue };
                let parsed = match entry {
                    TomlLine::Pair(key, value) => program_entry(key, value),
                    _ => None,
                };
                let Some((program, id)) = parsed else {
                    return Err(format!(
                        "line {line}: write a program as `name = \"id\"` or `name = {{ address = \"id\" }}`"
                    ));
                };
                if !seen.insert((cluster.clone(), program.to_string())) {
                    return Err(format!(
                        "line {line}: {program} is twice in [programs.{cluster}]"
                    ));
                }
                out.push((cluster.clone(), program.to_string(), id.to_string()));
            }
        }
    }
    Ok(out)
}

/// The program name and `address` of an IDL. It reads the JSON and not the diff model, so
/// an IDL that the model rejects still gives its id. The address is `None` when there is none.
fn idl_address(json: &str) -> Result<(String, Option<String>), String> {
    let value: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let text = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    // Codama tools read only `program`, so another `address` key must not hide its id.
    let (name, address) = if value.get("kind").and_then(Value::as_str) == Some("rootNode") {
        (text("/program/name"), text("/program/publicKey"))
    } else {
        // Anchor spec, then legacy Anchor or Shank.
        (
            text("/metadata/name").or_else(|| text("/name")),
            text("/address").or_else(|| text("/metadata/address")),
        )
    };
    let name = name.ok_or("the IDL has no program name")?;
    Ok((name.to_string(), address.map(str::to_string)))
}

/// Read every program id in the Anchor workspace at `root`: `Anchor.toml`,
/// `programs/*/src/lib.rs` and `target/idl/*.json`. A missing file or directory is not an
/// error, because CI often has only some of them.
pub fn discover(root: &Path) -> Result<Vec<ProgramId>, SourceError> {
    let real_root = root.canonicalize().map_err(|source| SourceError::Read {
        path: root.display().to_string(),
        source,
    })?;
    let read = |path: &str| read_optional(root, &real_root, path);
    let mut ids = Vec::new();

    let toml_path = "Anchor.toml";
    if let Some(text) = read(toml_path)? {
        let programs =
            anchor_toml_programs(&text).map_err(|message| parse_error(root, toml_path, message))?;
        ids.extend(
            programs
                .into_iter()
                .map(|(cluster, program, id)| ProgramId {
                    program,
                    leg: Leg::AnchorToml {
                        path: toml_path.to_string(),
                        cluster,
                    },
                    id,
                }),
        );
    }

    for dir in list_dir(root, &real_root, "programs", true)? {
        let manifest_path = format!("programs/{dir}/Cargo.toml");
        let Some(manifest) = read(&manifest_path)? else {
            continue;
        };
        let program = cargo_lib_name(&manifest).ok_or_else(|| {
            parse_error(
                root,
                &manifest_path,
                "no `[lib] name` or `[package] name`".into(),
            )
        })?;
        let lib_path = format!("programs/{dir}/src/lib.rs");
        let Some(lib) = read(&lib_path)? else {
            continue;
        };
        let mut found = declare_ids(&lib);
        // An empty id tells `check` that lib.rs was read and declares no id.
        if found.is_empty() {
            found.push(String::new());
        }
        ids.extend(found.into_iter().map(|id| ProgramId {
            program: program.clone(),
            leg: Leg::DeclareId {
                path: lib_path.clone(),
            },
            id,
        }));
    }

    for file in list_dir(root, &real_root, "target/idl", false)? {
        if Path::new(&file).extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let path = format!("target/idl/{file}");
        let Some(json) = read(&path)? else {
            continue;
        };
        let (program, address) = idl_address(&json).map_err(|m| parse_error(root, &path, m))?;
        ids.push(ProgramId {
            program,
            leg: Leg::IdlAddress { path },
            id: address.unwrap_or_default(),
        });
    }
    Ok(ids)
}

/// The `address` leg of the IDL at `path`, such as one outside `target/idl`. The leg path is
/// `path` as given.
pub fn read_idl(path: &Path) -> Result<ProgramId, SourceError> {
    let shown = path.display().to_string();
    let json = read_capped(path, &shown)?;
    let (program, address) = idl_address(&json).map_err(|message| SourceError::Parse {
        path: shown.clone(),
        message,
    })?;
    Ok(ProgramId {
        program,
        leg: Leg::IdlAddress { path: shown },
        id: address.unwrap_or_default(),
    })
}

/// Compare the ids of each program with its `declare_id!`, the id that the binary accepts.
/// `Onchain` legs are not compared: [`check_onchain`] adds them with their findings.
#[must_use]
pub fn check(ids: Vec<ProgramId>) -> SourceReport {
    let findings = check_all(&ids);
    SourceReport { ids, findings }
}

/// Add the chain legs: a program account at each `declare_id!`, and the address in the IDL
/// that the program publishes. `call` sends one JSON-RPC request and returns its `result`.
///
/// # Errors
///
/// When a request fails, the RPC answer has another shape, or an IDL account exists but
/// does not hold an IDL that can be read.
pub fn check_onchain(
    report: &mut SourceReport,
    mut call: impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
) -> Result<(), onchain::Error> {
    let groups: Vec<(String, Vec<String>)> = programs(&report.ids)
        .into_values()
        .map(|p| {
            let ids = declared(&p.legs).into_iter().map(str::to_string).collect();
            (p.name, ids)
        })
        .collect();
    for (program, ids) in groups {
        // `check` reports a larger set as Breaking, so the skip is not silent.
        if ids.is_empty() || ids.len() > MAX_DECLARED {
            continue;
        }
        let subject = format!("{program}@chain");
        let finding = |severity, kind, message| Finding {
            severity,
            kind,
            program: program.clone(),
            subject: subject.clone(),
            message,
        };
        let mut deployed = Vec::new();
        let mut why = None;
        for id in &ids {
            match not_a_program(&mut call, id)? {
                None => deployed.push(id),
                Some(reason) => why = Some(reason),
            }
        }
        if deployed.is_empty() {
            let message = match (ids.len(), why) {
                (1, Some(why)) => why,
                (n, _) => {
                    format!("none of the {n} declare_id! values of {program} is a deployed program")
                }
            };
            report.findings.push(finding(
                Severity::Breaking,
                FindingKind::OnchainMissing,
                message,
            ));
            continue;
        }
        for id in deployed {
            let idls = match onchain::fetch_text(&mut call, id, None) {
                Ok(idl) => vec![idl],
                // The two accounts can differ, so check each one.
                Err(onchain::Error::BothSources { .. }) => [Source::Metadata, Source::Anchor]
                    .into_iter()
                    .map(|s| onchain::fetch_text(&mut call, id, Some(s)))
                    .collect::<Result<_, _>>()?,
                Err(onchain::Error::NotFound { .. }) => {
                    let message = format!(
                        "program {id} has no on-chain IDL account, so a client cannot read its IDL from chain"
                    );
                    report.findings.push(finding(
                        Severity::Dangerous,
                        FindingKind::OnchainIdlMissing,
                        message,
                    ));
                    continue;
                }
                Err(e) => return Err(e),
            };
            for o in idls {
                let Some(address) = onchain_address(&o.text, &o.program) else {
                    let message = format!(
                        "the IDL in the {} {} has no address that can be read, so it cannot be checked",
                        o.source, o.account
                    );
                    report.findings.push(finding(
                        Severity::Breaking,
                        FindingKind::OnchainMismatch,
                        message,
                    ));
                    continue;
                };
                if address != *id {
                    let message = format!(
                        "the IDL in the {} {} gives address {address}, but declare_id! gives {id}",
                        o.source, o.account
                    );
                    report.findings.push(finding(
                        Severity::Breaking,
                        FindingKind::OnchainMismatch,
                        message,
                    ));
                }
                report.ids.push(ProgramId {
                    program: program.clone(),
                    leg: Leg::Onchain {
                        source: o.source.key().to_string(),
                        account: o.account,
                    },
                    id: address,
                });
            }
        }
    }
    Ok(())
}

/// The address of an on-chain IDL, read as for the file legs. A legacy IDL has no address
/// key, so its program is its address. `None` when no address can be read: the IDL has no
/// name, or an address key that is empty or not a string.
fn onchain_address(text: &str, program: &str) -> Option<String> {
    let (_, address) = idl_address(text).ok()?;
    if address.is_some() {
        return address;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let keyed = ["/address", "/metadata/address", "/program/publicKey"]
        .iter()
        .any(|p| value.pointer(p).is_some());
    (!keyed).then(|| program.to_string())
}

/// Why no program is at `id`, or `None` when an executable account is there.
fn not_a_program(
    call: &mut impl FnMut(&str, Value) -> Result<Value, onchain::Error>,
    id: &str,
) -> Result<Option<String>, onchain::Error> {
    // Only the flags are needed. A loader v2 program holds its whole ELF in the account.
    let params = json!([
        id,
        { "encoding": "base64", "dataSlice": { "offset": 0, "length": 0 } }
    ]);
    let result = call("getAccountInfo", params)?;
    let account = result
        .get("value")
        .ok_or_else(|| onchain::Error::RpcResponse("getAccountInfo: no `value`".into()))?;
    if account.is_null() {
        return Ok(Some(format!(
            "no account is at {id}, so the program is not deployed at its declare_id!"
        )));
    }
    match account.get("executable").and_then(Value::as_bool) {
        Some(true) => Ok(None),
        Some(false) => Ok(Some(format!(
            "the account at {id} is not a program, so the program is not deployed at its declare_id!"
        ))),
        None => Err(onchain::Error::RpcResponse(
            "getAccountInfo: no `executable` flag".into(),
        )),
    }
}

/// The legs of one program, and the name that its findings use.
struct Program<'a> {
    name: String,
    legs: Vec<&'a ProgramId>,
}

/// The legs of each program, by a key that ignores case, `-` and `_`: Anchor.toml keys and
/// Cargo names use `snake_case`, and Codama IDL names use `camelCase`. Onchain legs are left out.
fn programs(ids: &[ProgramId]) -> BTreeMap<String, Program<'_>> {
    let mut groups: BTreeMap<String, Program<'_>> = BTreeMap::new();
    for id in ids
        .iter()
        .filter(|id| !matches!(id.leg, Leg::Onchain { .. }))
    {
        let key = program_key(&id.program);
        groups
            .entry(key.clone())
            .or_insert_with(|| Program {
                name: key,
                legs: Vec::new(),
            })
            .legs
            .push(id);
    }
    groups
}

/// The name that groups the legs of a program, in `snake_case`: Anchor.toml keys and Cargo
/// names use it, and Codama writes `orderEngine` for `order_engine`. `a_mm` stays apart from
/// `am_m`.
fn program_key(name: &str) -> String {
    name.to_snake_case()
}

/// The distinct valid `declare_id!` values of a program, in file order.
fn declared<'a>(legs: &[&'a ProgramId]) -> Vec<&'a str> {
    let mut seen = BTreeSet::new();
    legs.iter()
        .filter(|l| matches!(l.leg, Leg::DeclareId { .. }) && is_program_id(&l.id))
        .map(|l| l.id.as_str())
        .filter(|id| seen.insert(*id))
        .collect()
}

/// More `declare_id!` values than a real `cfg` set has. Each one costs an RPC request with
/// `--onchain`, so a larger set is Breaking and the chain check skips it.
const MAX_DECLARED: usize = 16;

fn check_all(ids: &[ProgramId]) -> Vec<Finding> {
    let mut out = Vec::new();
    for program in programs(ids).values() {
        check_program(&program.name, &program.legs, &mut out);
    }
    out
}

fn check_program(program: &str, legs: &[&ProgramId], out: &mut Vec<Finding>) {
    let declared = check_declare_ids(program, legs, out);
    // With more than one declare_id!, an id that matches any of them is not a mismatch.
    let declared_set: BTreeSet<&str> = declared.iter().copied().collect();
    let mismatch = |id: &str| !declared.is_empty() && !declared_set.contains(id);
    // DeclareIdDuplicate lists the values, so each mismatch gives only their count.
    let expected = match declared.as_slice() {
        [one] => format!("declare_id! is {one}"),
        many => format!("which is not one of the {} declare_id! values", many.len()),
    };
    // With no declare_id!, the IDL and Anchor.toml can still disagree with each other.
    let toml_ids: Vec<&str> = legs
        .iter()
        .filter(|l| matches!(l.leg, Leg::AnchorToml { .. }) && is_program_id(&l.id))
        .map(|l| l.id.as_str())
        .collect();
    let mut push = |severity, kind, subject, message| {
        out.push(finding(severity, kind, program, subject, message));
    };

    for leg in legs {
        let Leg::AnchorToml { path, cluster } = &leg.leg else {
            continue;
        };
        if !is_program_id(&leg.id) {
            let subject = format!("{path}:{cluster}");
            let message = invalid_message(&leg.id, &subject);
            push(Severity::Breaking, FindingKind::InvalidId, subject, message);
        } else if mismatch(&leg.id) {
            push(
                Severity::Breaking,
                FindingKind::AnchorTomlMismatch,
                format!("{program}@{cluster}"),
                format!(
                    "[programs.{cluster}] {} is {}, {expected}: Anchor's tools use the program at that id on {cluster}, and the program rejects each instruction",
                    leg.program, leg.id
                ),
            );
        }
    }

    for leg in legs {
        let Leg::IdlAddress { path } = &leg.leg else {
            continue;
        };
        if leg.id.is_empty() {
            push(
                Severity::Cosmetic,
                FindingKind::IdlAddressMissing,
                program.to_string(),
                format!(
                    "{path} has no address (legacy IDL): the IDL id of {program} cannot be checked"
                ),
            );
        } else if !is_program_id(&leg.id) {
            push(
                Severity::Breaking,
                FindingKind::InvalidId,
                path.clone(),
                invalid_message(&leg.id, path),
            );
        } else if mismatch(&leg.id) {
            push(
                Severity::Breaking,
                FindingKind::IdlAddressMismatch,
                program.to_string(),
                format!(
                    "{path} address is {}, {expected}: clients and IDL lookups use the wrong program",
                    leg.id
                ),
            );
        } else if declared.is_empty()
            && !toml_ids.is_empty()
            && !toml_ids.contains(&leg.id.as_str())
        {
            push(
                Severity::Breaking,
                FindingKind::IdlAddressMismatch,
                program.to_string(),
                format!(
                    "{path} address is {}, and Anchor.toml gives {}: no declare_id! was found, and clients and IDL lookups use one of them wrongly",
                    leg.id,
                    toml_ids.join(" or ")
                ),
            );
        }
    }
}

/// The distinct valid `declare_id!` values of a program. Pushes the findings on them.
fn check_declare_ids<'a>(
    program: &str,
    legs: &[&'a ProgramId],
    out: &mut Vec<Finding>,
) -> Vec<&'a str> {
    let declared = declared(legs);
    let mut written = false;
    let mut empty_lib = None;
    for leg in legs {
        let Leg::DeclareId { path } = &leg.leg else {
            continue;
        };
        if leg.id.is_empty() {
            empty_lib.get_or_insert(path);
            continue;
        }
        written = true;
        if !is_program_id(&leg.id) {
            out.push(finding(
                Severity::Breaking,
                FindingKind::InvalidId,
                program,
                path.clone(),
                invalid_message(&leg.id, path),
            ));
        }
    }
    // A crate with no other leg is a library. A program with no crate here, such as one that
    // Anchor.toml loads from `deps/`, has its source elsewhere. Neither one is drift.
    let named_elsewhere = legs.iter().any(|l| !matches!(l.leg, Leg::DeclareId { .. }));
    if let (false, Some(path), true) = (written, empty_lib, named_elsewhere) {
        out.push(finding(
            Severity::Cosmetic,
            FindingKind::DeclareIdMissing,
            program,
            program.to_string(),
            format!(
                "no declare_id! at the crate root of {path}: the ids of {program} cannot be checked"
            ),
        ));
    }
    // Two root declare_id! values compile only with `cfg`, so the program picks one per build.
    if declared.len() > MAX_DECLARED {
        out.push(finding(
            Severity::Breaking,
            FindingKind::DeclareIdDuplicate,
            program,
            program.to_string(),
            format!(
                "{program} has {} declare_id! values, more than the {MAX_DECLARED} that a cfg set can need: they are not checked on chain",
                declared.len()
            ),
        ));
    } else if declared.len() > 1 {
        out.push(finding(
            Severity::Dangerous,
            FindingKind::DeclareIdDuplicate,
            program,
            program.to_string(),
            format!(
                "{program} picks its declare_id! with cfg from {} values: {}: an id that matches any of them passes, so a wrong id for one build is not found",
                declared.len(),
                declared.join(", ")
            ),
        ));
    }
    declared
}

fn finding(
    severity: Severity,
    kind: FindingKind,
    program: &str,
    subject: String,
    message: String,
) -> Finding {
    Finding {
        severity,
        kind,
        program: program.to_string(),
        subject,
        message,
    }
}

fn invalid_message(id: &str, subject: &str) -> String {
    format!("invalid program id in {subject}: `{id}` is not base58 of 32 bytes")
}

/// Base58 of 32 bytes is 32 to 44 characters. The length check comes first, because bs58
/// decode is quadratic in the input length.
fn is_program_id(id: &str) -> bool {
    (32..=44).contains(&id.len())
        && bs58::decode(id)
            .into_vec()
            .is_ok_and(|bytes| bytes.len() == 32)
}

fn parse_error(root: &Path, path: &str, message: String) -> SourceError {
    SourceError::Parse {
        path: root.join(path).display().to_string(),
        message,
    }
}

/// The text of `root/path`, or `None` when it does not exist. The file must resolve to a
/// path under `real_root`, so a symlink in the workspace cannot make the check read any file.
fn read_optional(root: &Path, real_root: &Path, path: &str) -> Result<Option<String>, SourceError> {
    let full = root.join(path);
    let shown = full.display().to_string();
    let real = match full.canonicalize() {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SourceError::Read {
                path: shown,
                source,
            })
        }
    };
    if !real.starts_with(real_root) {
        return Err(SourceError::OutsideRoot { path: shown });
    }
    read_capped(&real, &shown).map(Some)
}

/// The UTF-8 text of the regular file at `path`, of at most [`MAX_FILE_BYTES`]. `shown` is
/// the path for errors.
fn read_capped(path: &Path, shown: &str) -> Result<String, SourceError> {
    let read_error = |source| SourceError::Read {
        path: shown.to_string(),
        source,
    };
    // A FIFO blocks `open` until a writer comes, so the type is checked first.
    if !std::fs::metadata(path).map_err(read_error)?.is_file() {
        return Err(SourceError::NotAFile {
            path: shown.to_string(),
        });
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes))
        .map_err(read_error)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(SourceError::TooLarge {
            path: shown.to_string(),
            limit: MAX_FILE_BYTES,
        });
    }
    String::from_utf8(bytes).map_err(|e| read_error(io::Error::new(io::ErrorKind::InvalidData, e)))
}

/// The sorted names of the directories (or files) in `root/path`, empty when it does not
/// exist. The sort makes the order of the ids the same on each machine.
fn list_dir(
    root: &Path,
    real_root: &Path,
    path: &str,
    dirs: bool,
) -> Result<Vec<String>, SourceError> {
    let full = root.join(path);
    let read_error = |source| SourceError::Read {
        path: full.display().to_string(),
        source,
    };
    // A symlinked directory must not list, or name in an error, files out of ROOT.
    let real = match full.canonicalize() {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(read_error(e)),
    };
    if !real.starts_with(real_root) {
        return Err(SourceError::OutsideRoot {
            path: full.display().to_string(),
        });
    }
    let entries = std::fs::read_dir(&real).map_err(read_error)?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(read_error)?;
        let is_dir = entry.path().is_dir();
        if let Ok(name) = entry.file_name().into_string() {
            if is_dir == dirs {
                names.push(name);
            }
        }
    }
    names.sort();
    Ok(names)
}

/// The crate name that Anchor gives a program: `[lib] name`, else `[package] name`.
fn cargo_lib_name(text: &str) -> Option<String> {
    let mut table = String::new();
    let mut lib = None;
    let mut package = None;
    for raw in text.lines() {
        match toml_line(raw) {
            TomlLine::Table(key, array) => {
                table = if array { String::new() } else { key.join(".") }
            }
            TomlLine::Pair(key, value) if toml_key(key) == "name" => {
                let slot = match table.as_str() {
                    "lib" => &mut lib,
                    "package" => &mut package,
                    _ => continue,
                };
                if slot.is_none() {
                    *slot = toml_string_value(value);
                }
            }
            _ => {}
        }
    }
    lib.or(package).map(str::to_string)
}

/// A Rust token, as far as [`declare_ids`] needs one.
#[derive(Debug, PartialEq)]
enum Token<'a> {
    Ident(&'a str),
    /// The text between the quotes, with escapes as written.
    Str(&'a str),
    Punct(u8),
}

/// The identifiers, string literals and ASCII punctuation of `src`, without comments.
fn rust_tokens(src: &str) -> Vec<Token<'_>> {
    let b = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if b[i..].starts_with(b"//") {
            i = b[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(b.len(), |p| i + p);
        } else if b[i..].starts_with(b"/*") {
            i = block_comment_end(b, i);
        } else if c == b'"' {
            let end = string_end(b, i + 1);
            tokens.push(Token::Str(&src[i + 1..end]));
            i = end + 1;
        } else if c == b'\'' {
            i = char_literal_end(src, i);
        } else if c == b'_' || c.is_ascii_alphanumeric() {
            let start = i;
            while i < b.len() && (b[i] == b'_' || b[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            let ident = &src[start..i];
            let raw = matches!(ident, "r" | "br" | "cr")
                .then(|| raw_string(src, i))
                .flatten();
            if let Some((text, next)) = raw {
                tokens.push(Token::Str(text));
                i = next;
            } else {
                tokens.push(Token::Ident(ident));
            }
        } else {
            if c.is_ascii_punctuation() {
                tokens.push(Token::Punct(c));
            }
            i += 1;
        }
    }
    tokens
}

/// The index after the `*/` that closes the block comment at `i`. Rust block comments nest.
fn block_comment_end(b: &[u8], mut i: usize) -> usize {
    let mut depth = 0usize;
    while i < b.len() {
        if b[i..].starts_with(b"/*") {
            depth += 1;
            i += 2;
        } else if b[i..].starts_with(b"*/") {
            depth = depth.saturating_sub(1);
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// The index of the `"` that closes a string whose text starts at `i`.
fn string_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i,
            _ => i += 1,
        }
    }
    b.len()
}

/// The index after the char literal at `i`, or after the `'` of a lifetime. A `'"'` must not
/// open a string.
fn char_literal_end(src: &str, i: usize) -> usize {
    let rest = &src[i + 1..];
    if rest.starts_with('\\') {
        // The escaped char can be a quote, so the search starts after it.
        return src
            .get(i + 3..)
            .and_then(|s| s.find('\''))
            .map_or(src.len(), |p| i + 3 + p + 1);
    }
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(c), Some('\'')) => i + 1 + c.len_utf8() + 1,
        _ => i + 1,
    }
}

/// The text of the raw string at `i`, just after its `r`, and the index after it. With no
/// closing delimiter the string runs to the end of the file, as in rustc, so no byte is read twice.
fn raw_string(src: &str, i: usize) -> Option<(&str, usize)> {
    let rest = &src[i..];
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    let body = rest[hashes..].strip_prefix('"')?;
    let start = i + hashes + 1;
    let close = format!("\"{}", "#".repeat(hashes));
    Some(match body.find(&close) {
        Some(len) => (&src[start..start + len], start + len + close.len()),
        None => (&src[start..], src.len()),
    })
}

/// One line of TOML, without its comment.
enum TomlLine<'a> {
    Blank,
    /// A `[table]` or `[[array]]` header: the parts of its key, and whether it is an array.
    Table(Vec<String>, bool),
    /// `key = value`, both trimmed.
    Pair(&'a str, &'a str),
    /// Any other line, such as a part of a value on more than one line.
    Other,
}

fn toml_line(raw: &str) -> TomlLine<'_> {
    let line = strip_toml_comment(raw).trim();
    let key = |inner: &str| inner.split('.').map(|k| toml_key(k).to_string()).collect();
    if line.is_empty() {
        TomlLine::Blank
    } else if let Some(inner) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
        TomlLine::Table(key(inner), true)
    } else if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
        TomlLine::Table(key(inner), false)
    } else if let Some((k, v)) = line.split_once('=') {
        TomlLine::Pair(k.trim(), v.trim())
    } else {
        TomlLine::Other
    }
}

/// The line without its `#` comment. A `#` in a string is not a comment.
fn strip_toml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match quote {
            Some(_) if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return &line[..i],
            Some(_) | None => {}
        }
    }
    line
}

/// A bare or quoted key, without its quotes.
fn toml_key(raw: &str) -> &str {
    let key = raw.trim();
    ['"', '\'']
        .iter()
        .find_map(|&q| key.strip_prefix(q).and_then(|k| k.strip_suffix(q)))
        .unwrap_or(key)
}

/// The text of the string that starts `value`, and the rest of `value` after it.
fn toml_string(value: &str) -> Option<(&str, &str)> {
    let quote = value.chars().next().filter(|&c| c == '"' || c == '\'')?;
    let body = &value[1..];
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote == '"' {
            escaped = true;
        } else if c == quote {
            return Some((&body[..i], &body[i + 1..]));
        }
    }
    None
}

/// The text of `value` when it is one string and nothing else.
fn toml_string_value(value: &str) -> Option<&str> {
    toml_string(value)
        .filter(|(_, rest)| rest.trim().is_empty())
        .map(|(text, _)| text)
}

/// The program name and id of a `[programs.<cluster>]` entry.
fn program_entry<'a>(key: &'a str, value: &'a str) -> Option<(&'a str, &'a str)> {
    let name = toml_key(key);
    let bare = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if name.is_empty() || !bare {
        return None;
    }
    let id = toml_string_value(value).or_else(|| inline_address(value))?;
    Some((name, id))
}

/// The `address` of Anchor's `{ address = "...", path = "...", idl = "..." }` form.
fn inline_address(value: &str) -> Option<&str> {
    let mut rest = value.strip_prefix('{')?;
    let mut address = None;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('}') {
            return address.filter(|_| after.trim().is_empty());
        }
        let (key, after) = rest.split_once('=')?;
        let (text, after) = toml_string(after.trim_start())?;
        if toml_key(key) == "address" {
            address = Some(text);
        }
        rest = after.trim_start();
        rest = rest.strip_prefix(',').unwrap_or(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "29dv8e1WcjL4w6a7HDaHbUfXrF12yiJiVcKQ1qgeT3rF";

    #[test]
    fn declare_ids_skip_comments_and_strings() {
        let src = format!(
            r#"
// declare_id!("Line111");
/* declare_id!("Block111"); /* nested */ declare_id!("Block222"); */
const NOTE: &str = "declare_id!(\"Str111\") /*";
const QUOTE: char = '"';
fn f<'a>(_: &'a str) {{}}
anchor_lang::declare_id! (
    "{ID}"
);
my_declare_id!("Other111");
mod jupiter {{ use anchor_lang::declare_id; declare_id!("Cpi111"); }}
"#
        );
        assert_eq!(declare_ids(&src), [ID]);
    }

    #[test]
    fn declare_ids_keep_every_id_in_order() {
        let src = r#"#[cfg(feature = "devnet")] declare_id!("A");
#[cfg(not(feature = "devnet"))] declare_id!{r"B"}"#;
        assert_eq!(declare_ids(src), ["A", "B"]);
    }

    #[test]
    fn cr_string_does_not_hide_a_comment() {
        // In `cr"\"` the backslash is text. Read as an escape, the comment becomes code.
        let src = format!(
            "const C: &core::ffi::CStr = cr\"\\\" /* \"; declare_id!(\"Old111\"); */;\ndeclare_id!(\"{ID}\");"
        );
        assert_eq!(declare_ids(&src), [ID]);
    }

    #[test]
    fn unclosed_raw_string_runs_to_the_end_of_the_file() {
        // Each unclosed `r#"` searched the rest of the file again, which is quadratic.
        let src = format!("{}\ndeclare_id!(\"{ID}\");", "r#\"".repeat(20_000));
        assert_eq!(declare_ids(&src), Vec::<String>::new());
    }

    #[test]
    fn long_id_is_rejected_before_decode() {
        // bs58 decode is quadratic in the input length.
        let start = std::time::Instant::now();
        assert!(!is_program_id(&"2".repeat(200 << 10)));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn anchor_toml_reads_only_programs_tables() {
        let text = r#"
[features]
amm = "not-a-program"

[programs.localnet]
amm = "A" # a comment
"my-vault" = { address = "B", idl = "target/idl/vault.json" }

[provider]
cluster = "Localnet"

# [programs.devnet]
[[test.genesis]]
address = "C"
program = "x.so"
"#;
        assert_eq!(
            anchor_toml_programs(text),
            Ok(vec![
                ("localnet".into(), "amm".into(), "A".into()),
                ("localnet".into(), "my-vault".into(), "B".into()),
            ])
        );
    }

    #[test]
    fn anchor_toml_rejects_a_malformed_program_line() {
        let err = anchor_toml_programs("[programs.devnet]\namm = A\n");
        assert!(err.is_err_and(|e| e.starts_with("line 2:")));
    }

    #[test]
    fn idl_address_reads_spec_and_legacy_idls() {
        let spec = format!(
            r#"{{"address":"{ID}","metadata":{{"name":"amm","version":"0.1.0","spec":"0.1.0"}}}}"#
        );
        assert_eq!(idl_address(&spec), Ok(("amm".into(), Some(ID.into()))));
        let legacy = r#"{"version":"0.1.0","name":"vault","instructions":[]}"#;
        assert_eq!(idl_address(legacy), Ok(("vault".into(), None)));
    }

    #[test]
    fn idl_address_reads_an_idl_that_the_diff_model_rejects() {
        let spec = format!(
            r#"{{"address":"{ID}","metadata":{{"name":"amm","version":"0.1.0","spec":"0.2.0"}},
            "instructions":[{{"name":"x","discriminator":[1],"accounts":[],"args":[{{"name":"a","type":"u512"}}]}}]}}"#
        );
        assert_eq!(idl_address(&spec), Ok(("amm".into(), Some(ID.into()))));
        // Codama tools read only `program`, so a top-level `address` does not count.
        let codama = format!(
            r#"{{"kind":"rootNode","address":"Other111","program":{{"kind":"programNode","name":"amm","publicKey":"{ID}"}}}}"#
        );
        assert_eq!(idl_address(&codama), Ok(("amm".into(), Some(ID.into()))));
    }

    #[test]
    fn cargo_lib_name_prefers_lib_over_package() {
        let text = "[package]\nname = \"my-amm\"\n\n[lib]\nname = \"my_amm\"\n";
        assert_eq!(cargo_lib_name(text).as_deref(), Some("my_amm"));
        assert_eq!(
            cargo_lib_name("[package]\nname = \"vault\"\n").as_deref(),
            Some("vault")
        );
    }
}
