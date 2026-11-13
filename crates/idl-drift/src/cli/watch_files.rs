//! The files of `idl-drift watch`: the programs file, and the state file that keeps the slots
//! that the last check saw. They are not library API, so the formats can change with the CLI.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use idl_drift::onchain::Source;
use idl_drift::stale::Observed;
use serde::{Deserialize, Serialize};

/// One line of the programs file: `<program> <local IDL path> [metadata|anchor]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The program in base58.
    pub(crate) program: String,
    /// The local IDL path as the file gives it. The state file keys on it.
    pub(crate) idl: String,
    /// The local IDL path, relative to the directory of the programs file when it is relative.
    pub(crate) path: PathBuf,
    /// The only IDL account that counts, when the line gives one.
    pub(crate) source: Option<Source>,
    /// The line number, from 1.
    line: usize,
}

/// Why the programs file cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgramsError {
    line: usize,
    message: String,
}

impl fmt::Display for ProgramsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Read the programs file `text`. Each line is `<program> <local IDL path>`, then optionally
/// `metadata` or `anchor`. `#` starts a comment. A relative path is relative to `base`.
pub(crate) fn parse_programs(text: &str, base: &Path) -> Result<Vec<Entry>, ProgramsError> {
    let mut entries: Vec<Entry> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let fail = |message: String| ProgramsError { line: n, message };
        let fields: Vec<&str> = line
            .split_whitespace()
            .take_while(|f| !f.starts_with('#'))
            .collect();
        let (program, idl, source) = match fields[..] {
            [] => continue,
            [program, idl] => (program, idl, None),
            [program, idl, source] => (program, idl, Some(source)),
            [_] => return Err(fail("give `<program> <local IDL path>`".into())),
            _ => {
                return Err(fail(
                    "too many fields; give `<program> <local IDL path> [metadata|anchor]`".into(),
                ))
            }
        };
        let key: [u8; 32] = bs58::decode(program)
            .into_vec()
            .ok()
            .and_then(|k| k.try_into().ok())
            .ok_or_else(|| fail(format!("`{program}` is not a base58 program address")))?;
        let source = match source {
            None => None,
            Some("metadata") => Some(Source::Metadata),
            Some("anchor") => Some(Source::Anchor),
            Some(other) => {
                return Err(fail(format!(
                    "the source is `metadata` or `anchor`, not `{other}`"
                )))
            }
        };
        let program = bs58::encode(key).into_string();
        if let Some(first) = entries
            .iter()
            .find(|e| e.program == program && e.idl == idl)
        {
            return Err(fail(format!("repeats line {}", first.line)));
        }
        entries.push(Entry {
            program,
            idl: idl.to_string(),
            path: base.join(idl),
            source,
            line: n,
        });
    }
    Ok(entries)
}

/// The last write slot of one IDL account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SeenIdl {
    source: Source,
    account: String,
    slot: u64,
}

/// The slots that a check saw for one entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Seen {
    program: String,
    /// The local IDL path as the programs file gives it.
    idl: String,
    pub(crate) deploy_slot: u64,
    pub(crate) idls: Vec<SeenIdl>,
}

impl Seen {
    pub(crate) fn new(entry: &Entry, now: &Observed) -> Self {
        Self {
            program: entry.program.clone(),
            idl: entry.idl.clone(),
            deploy_slot: now.deploy_slot,
            idls: now
                .writes
                .iter()
                .map(|(source, account, slot)| SeenIdl {
                    source: *source,
                    account: account.clone(),
                    slot: *slot,
                })
                .collect(),
        }
    }

    /// Whether the two have the same slots.
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.deploy_slot == other.deploy_slot && self.idls == other.idls
    }
}

/// The version of the state file format.
const STATE_VERSION: u32 = 1;

/// The state file: what the last check saw for each entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    version: u32,
    programs: Vec<Seen>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            programs: Vec::new(),
        }
    }
}

impl State {
    /// Read the state file at `path`. A file that does not exist is an empty state.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let state: Self = serde_json::from_str(&text)
            .map_err(|e| format!("state file {}: {e}", path.display()))?;
        if state.version != STATE_VERSION {
            return Err(format!(
                "state file {}: version {} is not {STATE_VERSION}",
                path.display(),
                state.version
            ));
        }
        Ok(state)
    }

    /// Write the state to `path` in one step, so that a killed run leaves the old file. The
    /// first run makes the directory, as a CI cache that has no entry yet leaves none.
    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        idl_drift::allow::write_atomic(path, &format!("{text}\n"))
    }

    /// What the last check saw for `entry`.
    pub(crate) fn get(&self, entry: &Entry) -> Option<&Seen> {
        self.programs
            .iter()
            .find(|s| s.program == entry.program && s.idl == entry.idl)
    }

    /// Keep `seen` as what the last check saw for its entry.
    pub(crate) fn record(&mut self, seen: Seen) {
        match self
            .programs
            .iter_mut()
            .find(|s| s.program == seen.program && s.idl == seen.idl)
        {
            Some(old) => *old = seen,
            None => self.programs.push(seen),
        }
    }

    /// Drop what the state keeps for entries that are not in `entries`.
    pub(crate) fn retain(&mut self, entries: &[Entry]) {
        self.programs.retain(|s| {
            entries
                .iter()
                .any(|e| e.program == s.program && e.idl == s.idl)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUMP: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

    #[test]
    fn reads_lines_comments_and_sources() {
        let text = format!(
            "# program  idl\n\n{PUMP} idls/pump.json   # the bonding curve\n  {PUMP}\tother.json anchor\n"
        );
        let entries = parse_programs(&text, Path::new("/w")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, Path::new("/w/idls/pump.json"));
        assert_eq!((entries[0].line, entries[0].source), (3, None));
        assert_eq!(entries[1].source, Some(Source::Anchor));
        let absolute = parse_programs(&format!("{PUMP} /abs/x.json"), Path::new("/w")).unwrap();
        assert_eq!(absolute[0].path, Path::new("/abs/x.json"));
    }

    #[test]
    fn rejects_lines_it_cannot_read() {
        let err = |text: &str| {
            parse_programs(text, Path::new("."))
                .unwrap_err()
                .to_string()
        };
        assert_eq!(err(PUMP), "line 1: give `<program> <local IDL path>`");
        assert!(err(&format!("{PUMP} a b c")).contains("too many fields"));
        assert!(err("nope a.json").contains("`nope` is not a base58 program address"));
        assert!(err(&format!("{PUMP} a.json both")).contains("not `both`"));
        let twice = format!("{PUMP} a.json\n{PUMP} a.json metadata");
        assert_eq!(err(&twice), "line 2: repeats line 1");
    }

    #[test]
    fn the_state_file_round_trips_and_keeps_only_listed_entries() {
        let dir = std::env::temp_dir().join(format!("idl-drift-state-{}", std::process::id()));
        let path = dir.join("new/state.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(State::load(&path).unwrap(), State::default());

        let entries = parse_programs(&format!("{PUMP} a.json\n{PUMP} b.json"), &dir).unwrap();
        let seen = |entry: &Entry, deploy_slot| Seen {
            program: entry.program.clone(),
            idl: entry.idl.clone(),
            deploy_slot,
            idls: vec![SeenIdl {
                source: Source::Anchor,
                account: "A".into(),
                slot: 5,
            }],
        };
        let mut state = State::default();
        state.record(seen(&entries[0], 7));
        state.record(seen(&entries[1], 7));
        state.retain(&entries[..1]);
        state.save(&path).unwrap();
        let back = State::load(&path).unwrap();
        assert_eq!(back, state);
        assert!(back.get(&entries[0]).unwrap().same(&seen(&entries[0], 7)));
        assert!(back.get(&entries[1]).is_none());
        assert!(!back.get(&entries[0]).unwrap().same(&seen(&entries[0], 8)));

        std::fs::write(&path, r#"{"version":2,"programs":[]}"#).unwrap();
        assert!(State::load(&path)
            .unwrap_err()
            .contains("version 2 is not 1"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
