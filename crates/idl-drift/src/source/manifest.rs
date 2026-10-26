//! `Anchor.toml` and `Cargo.toml` read as TOML, not line by line, so dotted keys and inline
//! tables count the same as table headers.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use toml::{Table, Value};

/// One program id of an `Anchor.toml`.
#[derive(Debug)]
pub(super) struct AnchorProgram {
    pub(super) cluster: String,
    pub(super) program: String,
    pub(super) id: String,
}

/// Every `[programs.<cluster>]` entry: `name = "id"` or `name = { address = "id", .. }`, in any
/// TOML form (tables, dotted keys, inline tables). Other tables are ignored. The parsed TOML
/// keeps no file order, so the entries are sorted by cluster, then program.
///
/// Err(message) on invalid TOML or an entry of another shape.
pub(super) fn anchor_programs(text: &str) -> Result<Vec<AnchorProgram>, String> {
    let root: Table = parse(text)?;
    let Some(programs) = root.get("programs") else {
        return Ok(Vec::new());
    };
    let programs = programs
        .as_table()
        .ok_or("`programs` must be a table of clusters")?;
    let mut out = Vec::new();
    for (cluster, entries) in programs {
        let entries = entries
            .as_table()
            .ok_or_else(|| format!("[programs.{cluster}] must be a table of programs"))?;
        for (program, entry) in entries {
            let id = match entry {
                Value::String(id) => Some(id.as_str()),
                Value::Table(table) => table.get("address").and_then(Value::as_str),
                _ => None,
            }
            .ok_or_else(|| {
                format!(
                    "{program} in [programs.{cluster}]: write a program as `name = \"id\"` or `name = {{ address = \"id\" }}`"
                )
            })?;
            out.push(AnchorProgram {
                cluster: cluster.clone(),
                program: program.clone(),
                id: id.to_string(),
            });
        }
    }
    // A build with toml's `preserve_order` feature iterates in file order instead.
    out.sort_by(|a, b| (&a.cluster, &a.program).cmp(&(&b.cluster, &b.program)));
    Ok(out)
}

/// The lib target of a program crate.
#[derive(Debug)]
pub(super) struct CargoLib {
    pub(super) name: String,
    /// As written, relative to the directory of the `Cargo.toml`.
    pub(super) path: String,
}

#[derive(Deserialize)]
struct Manifest {
    package: Option<Package>,
    lib: Option<Lib>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

#[derive(Default, Deserialize)]
struct Lib {
    name: Option<String>,
    path: Option<String>,
}

/// The lib target that Cargo builds: name = `[lib] name`, else `[package] name` with `-` as `_`;
/// path = `[lib] path`, else "src/lib.rs". Ok(None) when there is no `[package]` (a workspace
/// manifest).
///
/// Err(message) on invalid TOML.
pub(super) fn cargo_lib(text: &str) -> Result<Option<CargoLib>, String> {
    // Only three keys, because a full manifest type rejects valid manifests that it does not
    // know, such as `resolver = "3"`.
    let manifest: Manifest = parse(text)?;
    let Some(package) = manifest.package else {
        return Ok(None);
    };
    let lib = manifest.lib.unwrap_or_default();
    Ok(Some(CargoLib {
        name: lib.name.unwrap_or_else(|| package.name.replace('-', "_")),
        path: lib.path.unwrap_or_else(|| "src/lib.rs".to_string()),
    }))
}

/// `text` without a byte order mark, as `T`. The error is one line: `line N: message`.
fn parse<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    toml::from_str(text).map_err(|e| {
        let message = e.message().trim().replace('\n', "; ");
        match e.span() {
            Some(span) => {
                let before = text.get(..span.start).unwrap_or(text);
                let line = before.matches('\n').count() + 1;
                format!("line {line}: {message}")
            }
            None => message,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programs(text: &str) -> Result<Vec<(String, String, String)>, String> {
        let programs = anchor_programs(text)?.into_iter();
        Ok(programs.map(|p| (p.cluster, p.program, p.id)).collect())
    }

    fn entry(cluster: &str, program: &str, id: &str) -> (String, String, String) {
        (cluster.into(), program.into(), id.into())
    }

    fn lib(text: &str) -> Result<Option<(String, String)>, String> {
        Ok(cargo_lib(text)?.map(|lib| (lib.name, lib.path)))
    }

    #[test]
    fn anchor_programs_reads_program_tables() {
        let text = "\u{feff}[programs.localnet]\nvault = \"B\"\namm = \"A\" # a comment\n\n[programs.devnet]\namm = \"C\"\n";
        assert_eq!(
            programs(text),
            Ok(vec![
                entry("devnet", "amm", "C"),
                entry("localnet", "amm", "A"),
                entry("localnet", "vault", "B"),
            ])
        );
    }

    #[test]
    fn anchor_programs_reads_root_dotted_keys() {
        let text = "programs.localnet.amm = \"A\"\n\n[provider]\ncluster = \"Localnet\"\n";
        assert_eq!(programs(text), Ok(vec![entry("localnet", "amm", "A")]));
    }

    #[test]
    fn anchor_programs_reads_root_inline_tables() {
        let text = "programs = { localnet = { amm = \"A\" } }\n";
        assert_eq!(programs(text), Ok(vec![entry("localnet", "amm", "A")]));
    }

    #[test]
    fn anchor_programs_reads_the_address_form() {
        let text = "[programs.localnet]\n\"my-vault\" = { address = \"B\", idl = \"target/idl/vault.json\" }\n";
        assert_eq!(programs(text), Ok(vec![entry("localnet", "my-vault", "B")]));
    }

    #[test]
    fn anchor_programs_rejects_an_entry_without_an_id() {
        let err = anchor_programs("[programs.devnet]\namm = { path = \"x.so\" }\n");
        assert!(err.is_err_and(|e| e.contains("amm in [programs.devnet]")));
    }

    #[test]
    fn anchor_programs_ignores_other_tables() {
        let text = "[features]\namm = \"not-a-program\"\n\n[[test.genesis]]\naddress = \"C\"\nprogram = \"x.so\"\n";
        assert_eq!(programs(text), Ok(Vec::new()));
    }

    #[test]
    fn cargo_lib_reads_the_lib_path() {
        let text =
            "[package]\nname = \"amm\"\n\n[lib]\nname = \"amm\"\npath = \"src/program.rs\"\n";
        assert_eq!(lib(text), Ok(Some(("amm".into(), "src/program.rs".into()))));
    }

    #[test]
    fn cargo_lib_reads_root_dotted_keys() {
        let text = "lib.name = \"amm\"\nlib.crate-type = [\"cdylib\", \"lib\"]\n\n[package]\nname = \"amm-program\"\n";
        assert_eq!(lib(text), Ok(Some(("amm".into(), "src/lib.rs".into()))));
    }

    #[test]
    fn cargo_lib_name_defaults_to_the_package_name_with_underscores() {
        let text = "[package]\nname = \"my-amm\"\nresolver = \"3\"\n";
        assert_eq!(lib(text), Ok(Some(("my_amm".into(), "src/lib.rs".into()))));
    }

    #[test]
    fn cargo_lib_of_a_workspace_manifest_is_none() {
        let text = "[workspace]\nmembers = [\"programs/*\"]\n";
        assert_eq!(lib(text), Ok(None));
    }
}
