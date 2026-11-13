//! The fixture files in a directory, and a test name for each.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The framework whose fixture files the tests read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framework {
    Shipstern,
    Carbon,
}

impl Framework {
    /// The kinds of file that `idl-verify --export-fixtures` writes for the framework.
    fn kinds(self) -> &'static [Kind] {
        match self {
            Self::Shipstern => &[Kind::Account, Kind::Transaction],
            Self::Carbon => &[Kind::Account, Kind::Instruction],
        }
    }
}

/// What a fixture file holds, from the end of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Account,
    /// A Shipstern `<signature>_tx.json`.
    Transaction,
    /// A Carbon `<signature>_..._instruction.json`.
    Instruction,
}

impl Kind {
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            Self::Account => "_account.json",
            Self::Transaction => "_tx.json",
            Self::Instruction => "_instruction.json",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Fixture {
    /// The file name, for messages.
    pub(crate) file: String,
    /// The name without `.json`.
    pub(crate) stem: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: Kind,
    pub(crate) test: String,
}

/// The fixture files of `framework` in `dir`, sorted by name, each with a unique test name.
/// Other files and subdirectories are ignored. A fixture name that is not UTF-8, or a link
/// that leads nowhere, is an error.
pub(crate) fn scan(dir: &Path, framework: Framework) -> Result<Vec<Fixture>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("the directory {} does not exist", dir.display()));
        }
        Err(error) => return Err(format!("cannot read {}: {error}", dir.display())),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let path = entry.path();
        let Some((file, kind)) = kind_of(&entry.file_name(), framework)? else {
            continue;
        };
        // A broken link has no metadata; a directory is not a fixture.
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => found.push((file, path, kind)),
            Ok(_) => {}
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        }
    }
    if found.is_empty() {
        let names: Vec<_> = framework
            .kinds()
            .iter()
            .map(|kind| format!("`*{}`", kind.suffix()))
            .collect();
        return Err(format!(
            "{} has no fixture file ({}); export some with `idl-verify --export-fixtures`",
            dir.display(),
            names.join(", ")
        ));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let stems: Vec<String> = found
        .iter()
        .map(|(file, ..)| file.trim_end_matches(".json").to_owned())
        .collect();
    let tests = test_names(stems.iter().map(String::as_str));
    Ok(found
        .into_iter()
        .zip(stems)
        .zip(tests)
        .map(|(((file, path, kind), stem), test)| Fixture {
            file,
            stem,
            path,
            kind,
            test,
        })
        .collect())
}

/// The name and kind of a fixture file of `framework`, or `None` for another file. A hidden
/// file is another file. A fixture name that is not UTF-8 is an error, because no test can
/// have it.
pub(crate) fn kind_of(
    name: &OsStr,
    framework: Framework,
) -> Result<Option<(String, Kind)>, String> {
    let text = name.to_string_lossy();
    let kind = framework
        .kinds()
        .iter()
        .copied()
        .find(|kind| text.ends_with(kind.suffix()));
    let Some(kind) = kind.filter(|_| !text.starts_with('.')) else {
        return Ok(None);
    };
    match name.to_str() {
        Some(file) => Ok(Some((file.to_owned(), kind))),
        None => Err(format!("the file name {text:?} is not UTF-8")),
    }
}

/// One Rust identifier per stem, in order. Each character outside `[A-Za-z0-9_]` becomes `_`,
/// and a name that starts with a digit gets a `_` in front. A name that an earlier stem already
/// has gets `_2`, `_3` and so on, so the names stay the same while the files stay the same.
pub(crate) fn test_names<'s>(stems: impl IntoIterator<Item = &'s str>) -> Vec<String> {
    let mut used = HashSet::new();
    stems
        .into_iter()
        .map(|stem| {
            let mut base: String = stem
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            if !base.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                base.insert(0, '_');
            }
            let mut name = base.clone();
            let mut n = 2;
            while !used.insert(name.clone()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_the_stem_and_fix_odd_characters() {
        let names = test_names([
            "E98AtWDa8whooJTw8nbbfz4oifvTcK7VxfaLb2uV2uEG_account",
            "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf_account",
            "my-curve.v2_account",
            "bondingcürve_account",
        ]);
        assert_eq!(
            names,
            [
                "E98AtWDa8whooJTw8nbbfz4oifvTcK7VxfaLb2uV2uEG_account",
                "_4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf_account",
                "my_curve_v2_account",
                "bondingc_rve_account",
            ]
        );
        for name in &names {
            assert!(syn::parse_str::<syn::Ident>(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn names_that_collide_get_a_number() {
        let names = test_names(["a-b_tx", "a_b_tx", "a.b_tx", "a_b_tx_2"]);
        assert_eq!(names, ["a_b_tx", "a_b_tx_2", "a_b_tx_3", "a_b_tx_2_2"]);
    }

    /// A new empty directory, removed again at the end of the test.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("idl-verify-fixtures-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str) -> &Self {
            std::fs::write(self.0.join(name), "{}").unwrap();
            self
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn kinds(fixtures: &[Fixture]) -> Vec<(&str, Kind, &str)> {
        fixtures
            .iter()
            .map(|f| (f.file.as_str(), f.kind, f.test.as_str()))
            .collect()
    }

    #[test]
    fn scan_keeps_the_files_of_the_framework_and_ignores_the_rest() {
        let dir = TempDir::new("mixed");
        dir.file("sigB_tx.json")
            .file("3Addr_account.json")
            .file("sigC_0_1_instruction.json")
            .file("README.md")
            .file("notes.json")
            .file("x_account.json.bak")
            .file(".hidden_account.json");
        std::fs::create_dir(dir.0.join("sub_account.json")).unwrap();

        let shipstern = scan(&dir.0, Framework::Shipstern).unwrap();
        assert_eq!(
            kinds(&shipstern),
            [
                ("3Addr_account.json", Kind::Account, "_3Addr_account"),
                ("sigB_tx.json", Kind::Transaction, "sigB_tx"),
            ]
        );
        assert_eq!(shipstern[0].path, dir.0.join("3Addr_account.json"));
        assert_eq!(shipstern[0].stem, "3Addr_account");

        let carbon = scan(&dir.0, Framework::Carbon).unwrap();
        assert_eq!(
            kinds(&carbon),
            [
                ("3Addr_account.json", Kind::Account, "_3Addr_account"),
                (
                    "sigC_0_1_instruction.json",
                    Kind::Instruction,
                    "sigC_0_1_instruction"
                ),
            ]
        );
    }

    #[test]
    fn scan_of_a_directory_with_no_fixture_is_an_error() {
        let dir = TempDir::new("empty");
        let error = scan(&dir.0, Framework::Carbon).unwrap_err();
        assert!(error.contains("has no fixture file"), "{error}");
        assert!(error.contains("`*_instruction.json`"), "{error}");

        dir.file("only_tx.json");
        let error = scan(&dir.0, Framework::Carbon).unwrap_err();
        assert!(error.contains("has no fixture file"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_fixture_name_that_is_not_utf8_is_an_error() {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"\xff_account.json");
        let error = kind_of(name, Framework::Carbon).unwrap_err();
        assert!(error.contains("not UTF-8"), "{error}");
        let other = std::ffi::OsStr::from_bytes(b"\xff.md");
        assert_eq!(kind_of(other, Framework::Carbon), Ok(None));
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_link_with_a_fixture_name_is_an_error() {
        let dir = TempDir::new("broken-link");
        dir.file("a_account.json");
        std::os::unix::fs::symlink(dir.0.join("gone.json"), dir.0.join("b_account.json")).unwrap();
        let error = scan(&dir.0, Framework::Shipstern).unwrap_err();
        assert!(error.contains("b_account.json"), "{error}");
    }

    #[test]
    fn scan_of_a_missing_directory_names_it() {
        let dir = TempDir::new("missing");
        let missing = dir.0.join("fixtures");
        let error = scan(&missing, Framework::Shipstern).unwrap_err();
        assert_eq!(
            error,
            format!("the directory {} does not exist", missing.display())
        );
    }
}
