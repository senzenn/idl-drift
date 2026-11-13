//! The allow file of `--allow`: one `<rule> <subject>` entry per line, for changes or
//! findings that a CI gate accepts.
//!
//! ```text
//! # A comment starts at a `#` at the start of a line or after a space.
//! field_changed Pool#1            # feeBps grew on purpose; all clients updated
//! bytes_used account Vault        # spare allocation, see the program source
//! ```
//!
//! [`Baseline`], [`check_write`] and [`write_atomic`] write the file for `--write-allow`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::{value, DeserializeOwned, IntoDeserializer};
use serde::Serialize;

/// One entry of an allow file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AllowEntry<K> {
    /// The line in the file, counted from 1.
    pub line: usize,
    /// The rule: a diff `kind` or a verify `check`.
    pub rule: K,
    /// The subject, matched exactly. It can hold spaces, such as `account Vault`.
    pub subject: String,
}

impl<K: Serialize> fmt::Display for AllowEntry<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", json_name(&self.rule), self.subject)
    }
}

/// The name that the JSON report gives a rule or a severity, such as `field_changed` or
/// `breaking`. An allow file uses the same rule names. Empty when `value` does not
/// serialize to a string.
pub fn json_name<K: Serialize>(value: &K) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

/// The entries of an allow file, with rule names of type `K`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowList<K> {
    entries: Vec<AllowEntry<K>>,
}

/// Why an allow file cannot be read. A bad line stops the run, so a typo does not let a
/// change through or hide a stale entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowError {
    /// The line has a rule name and no subject.
    MissingSubject { line: usize },
    /// The rule is not a name that the tool reports.
    UnknownRule { line: usize, rule: String },
}

impl fmt::Display for AllowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubject { line } => {
                write!(f, "line {line}: give a rule name and a subject")
            }
            Self::UnknownRule { line, rule } => {
                write!(f, "line {line}: unknown rule name `{rule}`")
            }
        }
    }
}

impl std::error::Error for AllowError {}

impl<K: PartialEq> AllowList<K> {
    /// Read an allow file. `K` gives the rule names: each one must deserialize from its name.
    pub fn parse(text: &str) -> Result<Self, AllowError>
    where
        K: DeserializeOwned,
    {
        let mut entries = Vec::new();
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let content = strip_comment(raw).trim();
            if content.is_empty() {
                continue;
            }
            let (name, subject) = content
                .split_once(char::is_whitespace)
                .map(|(name, rest)| (name, rest.trim()))
                .filter(|(_, subject)| !subject.is_empty())
                .ok_or(AllowError::MissingSubject { line })?;
            let de: value::StrDeserializer<'_, value::Error> = name.into_deserializer();
            let rule = K::deserialize(de).map_err(|_| AllowError::UnknownRule {
                line,
                rule: name.to_string(),
            })?;
            entries.push(AllowEntry {
                line,
                rule,
                subject: subject.to_string(),
            });
        }
        Ok(Self { entries })
    }

    /// The entries in file order.
    #[must_use]
    pub fn entries(&self) -> &[AllowEntry<K>] {
        &self.entries
    }

    /// `true` when an entry has this rule and subject.
    #[must_use]
    pub fn allows(&self, rule: &K, subject: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.rule == *rule && e.subject == subject)
    }
}

/// The Unicode line breaks. `str::lines` splits only at `\n`, but an editor can split at each.
const LINE_BREAKS: [char; 7] = [
    '\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}',
];

/// The line `<rule> <subject>`, or `None` when the subject has a line break, or when
/// [`AllowList::parse`] reads another subject back: a `#` after whitespace, or edge whitespace.
#[must_use]
pub fn entry_line<K>(rule: &K, subject: &str) -> Option<String>
where
    K: Serialize + DeserializeOwned + PartialEq,
{
    if subject.contains(&LINE_BREAKS[..]) {
        return None;
    }
    let line = format!("{} {subject}", json_name(rule));
    let list = AllowList::<K>::parse(&line).ok()?;
    match list.entries() {
        [e] if e.rule == *rule && e.subject == subject => Some(line),
        _ => None,
    }
}

/// The allow file that `--write-allow` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    /// The text of the file.
    pub text: String,
    /// The items that an allow file cannot hold.
    pub skipped: Vec<Unwritable>,
}

/// `<rule> <subject>` of an item that an allow file cannot hold, because [`AllowList::parse`]
/// would not read the same subject back. See [`entry_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unwritable(pub String);

impl fmt::Display for Unwritable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "an allow file cannot hold `{}`: the subject has a line break, a `#` after a space, or a space at one end",
            visible(&self.0)
        )
    }
}

/// `text` with control characters and non-ASCII whitespace as `\u{..}`, so that a warning
/// shows the character that stops the write.
fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() || (c.is_whitespace() && !c.is_ascii()) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

impl Baseline {
    /// `old` (or `header` for a new file) without the `stale` lines, counted from 1 as in
    /// [`AllowEntry::line`], then one entry per `(rule, subject)` of `new`, in order, once.
    #[must_use]
    pub fn build<'a, K>(
        old: Option<&str>,
        stale: impl IntoIterator<Item = usize>,
        header: &str,
        new: impl IntoIterator<Item = (&'a K, &'a str)>,
    ) -> Self
    where
        K: 'a + Serialize + DeserializeOwned + PartialEq,
    {
        let stale: BTreeSet<usize> = stale.into_iter().collect();
        let mut text = String::new();
        match old {
            // `str::lines` splits as `split_inclusive` does, so the line numbers match `parse`.
            Some(old) => {
                for (index, line) in old.split_inclusive('\n').enumerate() {
                    if !stale.contains(&(index + 1)) {
                        text.push_str(line);
                    }
                }
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
            }
            None => text.push_str(header),
        }
        let mut seen = BTreeSet::new();
        let mut skipped = Vec::new();
        for (rule, subject) in new {
            if !seen.insert((json_name(rule), subject)) {
                continue;
            }
            if let Some(line) = entry_line(rule, subject) {
                text.push_str(&line);
                text.push('\n');
            } else {
                skipped.push(Unwritable(format!("{} {subject}", json_name(rule))));
            }
        }
        Self { text, skipped }
    }
}

/// Check that `--write-allow` may write `path`: its directory exists and takes a new file, and
/// no file is there or it is the file of `allow`, which the run then updates.
///
/// # Errors
///
/// A message for the user when `path` cannot be written.
pub fn check_write(path: &Path, allow: Option<&Path>) -> Result<(), String> {
    let (Some(_), Some(dir)) = (path.file_name(), path.parent()) else {
        return Err(format!(
            "--write-allow: {} does not name a file",
            path.display()
        ));
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    if !dir.is_dir() {
        return Err(format!(
            "--write-allow: the directory {} does not exist",
            dir.display()
        ));
    }
    // A dangling symlink counts as a file, so that the rename cannot replace it.
    if fs::symlink_metadata(path).is_ok() && !allow.is_some_and(|allow| same_file(allow, path)) {
        return Err(format!(
            "--write-allow: {} exists; give it with --allow too to update it, or remove it",
            path.display()
        ));
    }
    // Make a temp file now: a directory with no write access then stops the run before any
    // RPC request.
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    create_temp(&target)
        .and_then(|(temp, file)| {
            drop(file);
            fs::remove_file(temp)
        })
        .map_err(|e| {
            format!(
                "--write-allow: cannot write next to {}: {e}",
                target.display()
            )
        })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Write `text` to a temp file next to `path`, then rename it to `path`. Thus a reader sees
/// the old file or the new one, never a part of it. A symlink stays, and its target gets the text.
///
/// # Errors
///
/// When `path` names no file, or the write or the rename fails.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
    let (temp, file) = create_temp(&target)?;
    let result = fill_and_rename(file, &temp, &target, text, permissions);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A new file next to `target`. The process id, the time and a counter make the name, so a
/// temp file that a killed run left does not block a later run.
fn create_temp(target: &Path) -> io::Result<(PathBuf, fs::File)> {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let name = target.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the path does not name a file")
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for _ in 0..100 {
        let count = COUNT.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".{}.{nanos}.{count}.tmp", std::process::id()));
        let temp = target.with_file_name(temp_name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for a temp file",
    ))
}

fn fill_and_rename(
    mut file: fs::File,
    temp: &Path,
    target: &Path,
    text: &str,
    permissions: Option<fs::Permissions>,
) -> io::Result<()> {
    file.write_all(text.as_bytes())?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
    }
    file.sync_all()?;
    // Windows cannot rename an open file.
    drop(file);
    fs::rename(temp, target)
}

/// The line without its comment. Subjects such as `Pool#1` hold a `#`, so only a `#` at the
/// start or after whitespace starts a comment.
fn strip_comment(line: &str) -> &str {
    line.match_indices('#')
        .map(|(i, _)| i)
        .find(|&i| i == 0 || line[..i].ends_with(char::is_whitespace))
        .map_or(line, |i| &line[..i])
}

#[cfg(test)]
mod tests {
    use super::{create_temp, entry_line, write_atomic, AllowList, Unwritable};
    use crate::ChangeKind;

    #[test]
    fn the_reader_reads_back_each_line_that_the_writer_writes() {
        let rule = ChangeKind::FieldChanged;
        for subject in ["Pool#1", "account Vault", "a  b", "a\tb#c"] {
            let line = entry_line(&rule, subject).unwrap();
            let list = AllowList::<ChangeKind>::parse(&line).unwrap();
            assert!(list.allows(&rule, subject), "{subject:?}");
        }
        // An editor can split a line at each of these, though `str::lines` does not.
        let breaks = [
            "a\rb",
            "a\u{b}b",
            "a\u{c}b",
            "a\u{85}b",
            "a\u{2028}b",
            "a\u{2029}b",
        ];
        for subject in ["#x", "a #b", "a\tb #c", "a\nb", "a\r\nb", " a", "a ", ""]
            .into_iter()
            .chain(breaks)
        {
            assert_eq!(entry_line(&rule, subject), None, "{subject:?}");
        }
        let warning = Unwritable("field_changed a\u{a0}".into()).to_string();
        assert!(warning.contains("`field_changed a\\u{a0}`"), "{warning}");
    }

    #[test]
    fn temp_files_get_new_names_and_go_away_on_error() {
        let dir = std::env::temp_dir().join(format!("idl-drift-temp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target")).unwrap();
        let target = dir.join("target");
        let (a, _) = create_temp(&target).unwrap();
        let (b, _) = create_temp(&target).unwrap();
        assert_ne!(a, b);
        std::fs::remove_file(a).unwrap();
        std::fs::remove_file(b).unwrap();
        // A rename onto a directory fails after the temp file exists.
        assert!(write_atomic(&target, "x").is_err());
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["target"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
