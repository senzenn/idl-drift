//! The program-id check of `idl-drift source`. An Anchor workspace keeps the program id in
//! `declare_id!`, in the `[programs.<cluster>]` tables of `Anchor.toml` and in the IDL
//! `address`. A new keypair or a move to another cluster can change one copy and not the
//! others.
//!
//! ```no_run
//! use idl_drift::source::{check, discover};
//!
//! let report = check(discover(std::path::Path::new("."))?);
//! // An empty directory or a wrong root gives no id to compare, and that is not a pass.
//! if report.nothing_to_compare() {
//!     return Err("no program has a declare_id! or an IDL address to compare with another id".into());
//! }
//! std::process::exit(i32::from(report.fails_at(idl_drift::Severity::Breaking)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod manifest;

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

/// Where a program id was read from. Paths are relative to the workspace root, except the
/// path that [`read_idl`] gets, which stays as given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "leg", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Leg {
    /// A `declare_id!` in the crate root of the program: its `[lib] path`, else `src/lib.rs`.
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
    /// `declare_id!`, or a legacy IDL with no address. An IDL address that is not a non-empty
    /// string is its JSON text, such as `""` or `null`, so the check reports it as invalid.
    pub id: String,
}

/// The rule a finding came from. The names in JSON output are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
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
    /// The program name in `snake_case`, as `Anchor.toml` and Cargo write it: the Codama name
    /// `orderEngine` is `order_engine`.
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

    /// `true` when a rule compared two valid ids of a program: its `declare_id!` with an id in
    /// another place, the chain included, or with no `declare_id!`, its IDL address with
    /// `Anchor.toml`. No rule compares two clusters, or two cfg values, with each other.
    #[must_use]
    pub fn compared(&self) -> bool {
        // (declare_id!, Anchor.toml, IDL or chain) for each program.
        let mut found: BTreeMap<String, (bool, bool, bool)> = BTreeMap::new();
        for id in self.ids.iter().filter(|id| is_program_id(&id.id)) {
            let legs = found.entry(program_key(&id.program)).or_default();
            match id.leg {
                Leg::DeclareId { .. } => legs.0 = true,
                Leg::AnchorToml { .. } => legs.1 = true,
                Leg::IdlAddress { .. } | Leg::Onchain { .. } => legs.2 = true,
            }
        }
        found
            .values()
            .any(|&(declared, toml, idl)| (declared && (toml || idl)) || (toml && idl))
    }

    /// `true` when the report compared no id and found nothing, as for a wrong ROOT or files
    /// that hold no id. A caller must not treat such a report as a pass.
    #[must_use]
    pub fn nothing_to_compare(&self) -> bool {
        !self.compared() && self.findings.is_empty()
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
#[non_exhaustive]
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

/// The string of each distinct `declare_id!("...")` that rustc can put in the crate's `ID`, in
/// order, up to `MAX_DECLARED + 1` values. Comments and strings are skipped.
///
/// These are the ones at the crate root or in a `cfg_if!` branch, and the ones in a root
/// module that a root `use` imports `ID` from, as `use keys::ID` or `use keys::{ID, id}`. A
/// local or imported `ID` shadows a glob, so `use <module>::*` counts only when there is
/// neither. A `use` in a `cfg_if!` branch is a root `use`. Another block, such as
/// `mod jupiter { .. }`, a function body or the body of another macro, is not the root. A cfg
/// is not evaluated, so the id of a cfg that is never true, such as `#[cfg(any())]`, still
/// counts as one of the cfg values.
fn declare_ids(rust: &str) -> Vec<String> {
    let tokens = rust_tokens(rust);
    let (imports, at_root) = root_imports(&tokens);
    let explicit: BTreeSet<&str> = imports
        .iter()
        .filter(|(_, glob)| !glob)
        .map(|(module, _)| *module)
        .collect();
    let modules = if at_root || !explicit.is_empty() {
        explicit
    } else {
        imports.iter().map(|(module, _)| *module).collect()
    };
    let mut blocks = Blocks::default();
    let mut ids: Vec<String> = Vec::new();
    for i in 0..tokens.len() {
        blocks.step(&tokens, i);
        let sets_id = match blocks.scope() {
            Scope::Root => true,
            Scope::Module(module) => modules.contains(module),
            Scope::Other => false,
        };
        let Some(id) = declared_at(&tokens, i).filter(|_| sets_id) else {
            continue;
        };
        if !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_string());
            // `check` reports a larger set as Breaking, so more values change nothing.
            if ids.len() > MAX_DECLARED {
                break;
            }
        }
    }
    ids
}

/// The string of the `declare_id!("...")` that starts at `tokens[i]`.
fn declared_at<'a>(tokens: &[Token<'a>], i: usize) -> Option<&'a str> {
    match tokens.get(i..)? {
        [Token::Ident("declare_id"), Token::Punct(b'!'), Token::Punct(b'(' | b'[' | b'{'), Token::Str(id), ..] => {
            Some(id)
        }
        _ => None,
    }
}

/// The `(module, glob)` of each `<module>::ID` and `<module>::*` that a root `use` imports, and
/// whether a `declare_id!` is at the root.
fn root_imports<'a>(tokens: &[Token<'a>]) -> (Vec<(&'a str, bool)>, bool) {
    let mut imports = Vec::new();
    let mut at_root = false;
    let mut blocks = Blocks::default();
    let mut statement = None;
    for (i, token) in tokens.iter().enumerate() {
        blocks.step(tokens, i);
        if blocks.scope() != Scope::Root {
            continue;
        }
        match token {
            // In a return type, `use<..>` captures lifetimes and imports nothing.
            Token::Ident("use") if tokens.get(i + 1) != Some(&Token::Punct(b'<')) => {
                statement = Some(i + 1);
            }
            Token::Punct(b';') => {
                if let Some(start) = statement.take() {
                    use_tree(&tokens[start..i], &mut imports);
                }
            }
            _ => at_root |= declared_at(tokens, i).is_some(),
        }
    }
    (imports, at_root)
}

/// Push `(module, glob)` for each `<module>::ID` and `<module>::*` in the tree of a `use`. A
/// renamed `ID`, as in `keys::ID as KEY`, does not set the crate's `ID`.
fn use_tree<'a>(tree: &[Token<'a>], imports: &mut Vec<(&'a str, bool)>) {
    // The last path segment before each open `{` group.
    let mut groups: Vec<Option<&'a str>> = Vec::new();
    let (mut module, mut leaf, mut renamed) = (None, None, false);
    for token in tree.iter().chain(std::iter::once(&Token::Punct(b','))) {
        match token {
            Token::Ident("as") => renamed = true,
            Token::Ident(name) if !renamed => (module, leaf) = (leaf, Some(*name)),
            Token::Punct(b'*') => {
                if let Some(module) = leaf.take() {
                    imports.push((module, true));
                }
            }
            Token::Punct(b'{') => groups.push(leaf),
            Token::Punct(b',' | b'}') => {
                if let (false, Some("ID"), Some(module)) = (renamed, leaf, module) {
                    imports.push((module, false));
                }
                if *token == Token::Punct(b'}') {
                    groups.pop();
                }
                (module, leaf, renamed) = (None, groups.last().copied().flatten(), false);
            }
            _ => {}
        }
    }
}

/// What a `{` block is, for the crate root.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Block<'a> {
    /// The body of a `cfg_if!`. Its direct blocks are its branches.
    CfgIf,
    /// A `cfg_if!` branch. rustc puts its items where the `cfg_if!` is.
    Branch,
    /// A `mod <name>` at the crate root.
    Module(&'a str),
    /// Any other block, such as a function body or the body of another macro.
    Other,
}

/// Where a token is, for the crate's `ID`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Scope<'a> {
    Root,
    Module(&'a str),
    Other,
}

/// The blocks that are open at a token.
#[derive(Default)]
struct Blocks<'a> {
    stack: Vec<Block<'a>>,
    /// The `Other` blocks in `stack`. The count keeps each step O(1) on a deep nest.
    other: usize,
    /// The root module that holds the token.
    module: Option<&'a str>,
}

impl<'a> Blocks<'a> {
    /// Open or close a block when `tokens[i]` is a brace.
    fn step(&mut self, tokens: &[Token<'a>], i: usize) {
        match tokens[i] {
            Token::Punct(b'{') => {
                let block = match (self.stack.last(), &tokens[..i]) {
                    (Some(Block::CfgIf), _) => Block::Branch,
                    _ if self.other > 0 => Block::Other,
                    // `cfg_if!` or a path that ends in it, such as `cfg_if::cfg_if!`.
                    (_, [.., Token::Ident("cfg_if"), Token::Punct(b'!')]) => Block::CfgIf,
                    (_, [.., Token::Ident("mod"), Token::Ident(name)]) if self.module.is_none() => {
                        Block::Module(name)
                    }
                    _ => Block::Other,
                };
                match block {
                    Block::Other => self.other += 1,
                    Block::Module(name) => self.module = Some(name),
                    Block::CfgIf | Block::Branch => {}
                }
                self.stack.push(block);
            }
            Token::Punct(b'}') => match self.stack.pop() {
                Some(Block::Other) => self.other -= 1,
                Some(Block::Module(_)) => self.module = None,
                Some(Block::CfgIf | Block::Branch) | None => {}
            },
            _ => {}
        }
    }

    fn scope(&self) -> Scope<'a> {
        match (self.other, self.module) {
            (0, None) => Scope::Root,
            (0, Some(module)) => Scope::Module(module),
            _ => Scope::Other,
        }
    }
}

/// The program name and `address` of an IDL. It reads the JSON and not the diff model, so
/// an IDL that the model rejects still gives its id. The address is `None` when the IDL has no
/// address key, and the JSON text of the value when that is not a non-empty string.
fn idl_address(json: &str) -> Result<(String, Option<String>), String> {
    let value: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    // Codama tools read only `program`, so another `address` key must not hide its id.
    let (names, addresses): (&[&str], &[&str]) =
        if value.get("kind").and_then(Value::as_str) == Some("rootNode") {
            (&["/program/name"], &["/program/publicKey"])
        } else {
            // Anchor spec, then legacy Anchor or Shank.
            (
                &["/metadata/name", "/name"],
                &["/address", "/metadata/address"],
            )
        };
    let name = names
        .iter()
        .find_map(|p| {
            value
                .pointer(p)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .ok_or("the IDL has no program name")?;
    // A client reads the first key that is there, so a bad value must not fall back.
    let address = addresses
        .iter()
        .find_map(|p| value.pointer(p))
        .map(|v| match v.as_str() {
            Some(text) if !text.is_empty() => text.to_string(),
            _ => v.to_string(),
        });
    Ok((name.to_string(), address))
}

/// Read every program id in the Anchor workspace at `root`: `Anchor.toml`, the crate root of
/// each `programs/*` crate (its `[lib] path`, else `src/lib.rs`) and `target/idl/*.json`. A
/// missing file or directory is not an error, because CI often has only some of them.
pub fn discover(root: &Path) -> Result<Vec<ProgramId>, SourceError> {
    let real_root = root.canonicalize().map_err(|source| SourceError::Read {
        path: root.display().to_string(),
        source,
    })?;
    let read = |path: &str| read_optional(root, &real_root, path);
    let mut ids = Vec::new();

    let toml_path = "Anchor.toml";
    if let Some(text) = read(toml_path)? {
        let programs = manifest::anchor_programs(&text)
            .map_err(|message| parse_error(root, toml_path, message))?;
        ids.extend(programs.into_iter().map(|p| ProgramId {
            program: p.program,
            leg: Leg::AnchorToml {
                path: toml_path.to_string(),
                cluster: p.cluster,
            },
            id: p.id,
        }));
    }

    for dir in list_dir(root, &real_root, "programs", true)? {
        let manifest_path = format!("programs/{dir}/Cargo.toml");
        let Some(manifest) = read(&manifest_path)? else {
            continue;
        };
        let manifest::CargoLib {
            name: program,
            path,
        } = manifest::cargo_lib(&manifest)
            .and_then(|lib| lib.ok_or_else(|| "no `[package]` table".to_string()))
            .map_err(|message| parse_error(root, &manifest_path, message))?;
        // Cargo reads `[lib] path` from the crate directory. An absolute path stays as it is,
        // so `read_optional` refuses it when it is out of ROOT.
        let lib_path = if Path::new(&path).is_absolute() {
            path
        } else {
            format!("programs/{dir}/{path}")
        };
        let Some(lib) = read(&lib_path)? else {
            continue;
        };
        let mut found = declare_ids(&lib);
        // An empty id tells `check` that the crate root was read and declares no id.
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
/// key, so its program is its address. `None` when the IDL has no name.
fn onchain_address(text: &str, program: &str) -> Option<String> {
    let (_, address) = idl_address(text).ok()?;
    Some(address.unwrap_or_else(|| program.to_string()))
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

/// The legs of each program, by its `snake_case` [`program_key`], so `a_mm` and `am_m` stay
/// apart. Onchain legs are left out.
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
    fn declare_ids_in_a_cfg_if_branch_or_a_re_exported_module_count() {
        // Both are the program's own id: `cfg_if!` picks one, and `pub use` puts it at the root.
        let cfg_if = r#"cfg_if::cfg_if! { if #[cfg(feature = "devnet")] { declare_id!("Dev111"); }
            else { declare_id!("Main111"); } }"#;
        assert_eq!(declare_ids(cfg_if), ["Dev111", "Main111"]);
        let module = r#"mod program_id { use super::*; declare_id!("Mod111"); }
            pub use program_id::*;"#;
        assert_eq!(declare_ids(module), ["Mod111"]);
        let by_id = r#"pub mod ids { declare_id!("Id111"); }
            pub use ids::ID;"#;
        assert_eq!(declare_ids(by_id), ["Id111"]);
        // A module that no root `use` imports `ID` from names another program, and a function
        // body is not the crate root.
        let other = r#"mod jupiter { declare_id!("Cpi111"); } pub use jupiter::Swap;
            fn f() { declare_id!("Fn111"); }
            mod raydium { cfg_if! { if #[cfg(x)] { declare_id!("CfgCpi111"); } } }"#;
        assert_eq!(declare_ids(other), Vec::<String>::new());
    }

    #[test]
    fn only_a_cfg_if_branch_keeps_a_declare_id_at_the_root() {
        // Another macro can drop its body, or put it in a module.
        let unused = r#"macro_rules! unused { ($($t:tt)*) => {}; }
            unused! { declare_id!("Drop111"); }
            mod keys { declare_id!("Keys111"); }
            pub use keys::{ID, id};"#;
        assert_eq!(declare_ids(unused), ["Keys111"]);
        let items = r#"macro_rules! items { ($($i:item)*) => { $($i)* }; }
            declare_id!("Root111");
            items! { pub mod spl_token { declare_id!("Spl111"); } }"#;
        assert_eq!(declare_ids(items), ["Root111"]);
        // In a branch, a module or a function body is not the crate root.
        let nested = r#"declare_id!("Root111");
            cfg_if! { if #[cfg(feature = "cpi")] {
                mod other { declare_id!("Cpi111"); }
                fn f() { declare_id!("Fn111"); }
            } }"#;
        assert_eq!(declare_ids(nested), ["Root111"]);
    }

    #[test]
    fn an_explicit_id_import_wins_over_a_glob() {
        let group = r#"mod program_id { declare_id!("Glob111"); }
            mod keys { declare_id!("Keys111"); }
            pub use program_id::*;
            pub use keys::{ID, id};"#;
        assert_eq!(declare_ids(group), ["Keys111"]);
        // The cfg is not evaluated, and a private `use` also sets the root `ID`.
        let private = r#"#[cfg(any())] mod program_id { declare_id!("Glob111"); }
            #[cfg(any())] pub use program_id::*;
            mod keys { declare_id!("Keys111"); }
            use keys::{ID, id};"#;
        assert_eq!(declare_ids(private), ["Keys111"]);
    }

    #[test]
    fn a_root_declare_id_shadows_a_glob_import() {
        let glob = r#"declare_id!("Root111");
            pub mod ids { declare_id!("Glob111"); }
            pub use ids::*;"#;
        assert_eq!(declare_ids(glob), ["Root111"]);
        // A macro that is not called, and a `use` in a function body, do not count.
        let unused = r#"macro_rules! not_called { () => { declare_id!("Macro111"); } }
            mod program_id { declare_id!("Mod111"); }
            fn f() { use program_id::*; }
            declare_id!("Root111");"#;
        assert_eq!(declare_ids(unused), ["Root111"]);
    }

    #[test]
    fn a_use_in_a_cfg_if_branch_is_a_root_use() {
        let src = r#"mod devnet { declare_id!("Dev111"); }
            mod mainnet { declare_id!("Main111"); }
            cfg_if::cfg_if! {
                if #[cfg(feature = "devnet")] { pub use devnet::ID; }
                else { pub use mainnet::ID; }
            }"#;
        assert_eq!(declare_ids(src), ["Dev111", "Main111"]);
    }

    #[test]
    fn declare_ids_stop_after_more_values_than_a_cfg_set_needs() {
        let distinct: Vec<String> = (0..40)
            .map(|i| format!("declare_id!(\"Id{i}\");"))
            .collect();
        assert_eq!(declare_ids(&distinct.concat()).len(), MAX_DECLARED + 1);
        assert_eq!(declare_ids(&"declare_id!(\"A\");".repeat(100)), ["A"]);
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
    fn idl_address_reads_spec_and_legacy_idls() {
        let spec = format!(
            r#"{{"address":"{ID}","metadata":{{"name":"amm","version":"0.1.0","spec":"0.1.0"}}}}"#
        );
        assert_eq!(idl_address(&spec), Ok(("amm".into(), Some(ID.into()))));
        let legacy = r#"{"version":"0.1.0","name":"vault","instructions":[]}"#;
        assert_eq!(idl_address(legacy), Ok(("vault".into(), None)));
        let legacy_address = r#"{"name":"vault","metadata":{"address":"X"}}"#;
        assert_eq!(
            idl_address(legacy_address),
            Ok(("vault".into(), Some("X".into())))
        );
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
}
