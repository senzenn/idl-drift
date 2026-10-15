//! The allow file of `--allow`: one `<rule> <subject>` entry per line, for changes or
//! findings that a CI gate accepts.
//!
//! ```text
//! # A comment starts at a `#` at the start of a line or after a space.
//! field_changed Pool#1            # feeBps grew on purpose; all clients updated
//! bytes_used account Vault        # spare allocation, see the program source
//! ```

use std::fmt;

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

/// The line without its comment. Subjects such as `Pool#1` hold a `#`, so only a `#` at the
/// start or after whitespace starts a comment.
fn strip_comment(line: &str) -> &str {
    line.match_indices('#')
        .map(|(i, _)| i)
        .find(|&i| i == 0 || line[..i].ends_with(char::is_whitespace))
        .map_or(line, |i| &line[..i])
}
