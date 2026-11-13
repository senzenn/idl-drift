//! `diff --suggest-version` and `diff --write-allow`: version advice and baseline mode.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const AMM_OLD: &str = "tests/fixtures/amm_old.json";
const AMM_NEW: &str = "tests/fixtures/amm_new.json";

fn drift(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_idl-drift"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// An empty temp dir for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("idl-drift-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn suggest_version_prints_the_bump_in_each_format() {
    let plain = drift(&["diff", AMM_OLD, AMM_NEW, "--json"]);
    assert!(json(&plain).get("suggested_bump").is_none());

    // An allow file sets only the exit code: the crate still needs the bump.
    let dir = temp_dir("bump");
    let allow = dir.join("allow.txt");
    let all = "field_changed Pool#1\naccount_added swap#2\ninstruction_added collectFees\n";
    std::fs::write(&allow, all).unwrap();
    let args = [
        "diff",
        AMM_OLD,
        AMM_NEW,
        "--suggest-version",
        "--allow",
        arg(&allow),
    ];
    let out = drift(&[&args[..], &["--json"]].concat());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(json(&out)["suggested_bump"], "major");

    let human = String::from_utf8(drift(&args).stdout).unwrap();
    assert!(
        human.contains("\nsuggested version bump: major (a 0.y.z crate bumps y)\nexit code: 0\n"),
        "{human}"
    );
    let md = String::from_utf8(drift(&[&args[..], &["--markdown"]].concat()).stdout).unwrap();
    assert!(
        md.contains("\nSuggested version bump: `major` (a 0.y.z crate bumps y).\n"),
        "{md}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_allow_writes_a_baseline_that_the_next_run_passes_with() {
    let dir = temp_dir("write-allow");
    let file = dir.join("idl.allow");
    let out = drift(&[
        "diff",
        AMM_OLD,
        AMM_NEW,
        "--fail-on",
        "dangerous",
        "--write-allow",
        arg(&file),
        "--json",
    ]);
    // The report is the one that --allow FILE gives after the write.
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report = json(&out);
    assert_eq!(report["exit_code"], 0);
    assert_eq!(report["unused_allows"], serde_json::json!([]));
    let text = std::fs::read_to_string(&file).unwrap();
    let entries: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    // Report order, Breaking first. The Additive change is below --fail-on.
    assert_eq!(entries, ["field_changed Pool#1", "account_added swap#2"]);
    assert!(text.starts_with("# "), "{text}");
    // The rename leaves no temp file.
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["idl.allow"]);

    let out = drift(&[
        "diff",
        AMM_OLD,
        AMM_NEW,
        "--fail-on",
        "dangerous",
        "--allow",
        arg(&file),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(json(&out)["unused_allows"], serde_json::json!([]));
    assert!(!stderr(&out).contains("warning"), "{}", stderr(&out));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_allow_keeps_unused_entries_unless_prune_allow() {
    let dir = temp_dir("write-allow-existing");
    let file = dir.join("idl.allow");
    let old =
        "# reviewed\nfield_changed Pool#1  # feeBps grew on purpose\narg_added swap#9  # v2\n";
    std::fs::write(&file, old).unwrap();

    // An existing file that the run does not read: refuse, and keep it.
    let out = drift(&["diff", AMM_OLD, AMM_NEW, "--write-allow", arg(&file)]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--allow"), "{}", stderr(&out));
    let other = dir.join("other.allow");
    std::fs::write(&other, "").unwrap();
    let out = drift(&[
        "diff",
        AMM_OLD,
        AMM_NEW,
        "--allow",
        arg(&other),
        "--write-allow",
        arg(&file),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), old);
    let out = drift(&[
        "diff",
        AMM_OLD,
        AMM_NEW,
        "--allow",
        arg(&file),
        "--prune-allow",
    ]);
    assert_eq!(out.status.code(), Some(2));

    // The same file: keep each line and comment, warn about the unused entry, add the rest.
    let update = [
        "diff",
        AMM_OLD,
        AMM_NEW,
        "--fail-on",
        "additive",
        "--json",
        "--allow",
        arg(&file),
        "--write-allow",
        arg(&file),
    ];
    let out = drift(&update);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).contains("line 3: no change matches `arg_added swap#9`"));
    let unused = serde_json::json!([{"line": 3, "rule": "arg_added", "subject": "swap#9"}]);
    let report = json(&out);
    assert_eq!(report["unused_allows"], unused);
    assert!(report.get("pruned_allows").is_none());
    let kept = format!("{old}account_added swap#2\ninstruction_added collectFees\n");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), kept);
    // A second run changes nothing.
    assert_eq!(drift(&update).status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), kept);

    let out = drift(&[&update[..], &["--prune-allow"]].concat());
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out)
            .contains("line 3: no change matches `arg_added swap#9`; --prune-allow removed it"),
        "{}",
        stderr(&out)
    );
    let report = json(&out);
    assert_eq!(report["pruned_allows"], unused);
    assert_eq!(report["unused_allows"], serde_json::json!([]));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# reviewed\nfield_changed Pool#1  # feeBps grew on purpose\n\
         account_added swap#2\ninstruction_added collectFees\n"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn write_allow_updates_the_target_of_a_symlink_and_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("write-allow-link");
    let (real, link) = (dir.join("real.allow"), dir.join("link.allow"));
    std::fs::write(&real, "field_changed Pool#1\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink("real.allow", &link).unwrap();
    let out = drift(&[
        "diff",
        AMM_OLD,
        AMM_NEW,
        "--fail-on",
        "dangerous",
        "--allow",
        arg(&link),
        "--write-allow",
        arg(&link),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "field_changed Pool#1\naccount_added swap#2\n"
    );
    let mode = std::fs::metadata(&real).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o640);
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["link.allow", "real.allow"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_allow_checks_the_directory_before_any_rpc_request() {
    let dir = temp_dir("write-allow-dir");
    let file = dir.join("missing").join("idl.allow");
    let out = Command::new(env!("CARGO_BIN_EXE_idl-drift"))
        .args([
            "diff",
            "--onchain",
            "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
            AMM_NEW,
        ])
        .args(["--write-allow", arg(&file)])
        .env("IDL_DRIFT_RPC_URL", "http://127.0.0.1:1/")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = stderr(&out);
    assert!(
        text.contains("directory") && !text.contains("RPC"),
        "{text}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn write_allow_checks_write_access_before_any_rpc_request() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("write-allow-access");
    let locked = dir.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let real = locked.join("real.allow");
    std::fs::write(&real, "").unwrap();
    let (link, new) = (dir.join("link.allow"), locked.join("new.allow"));
    std::os::unix::fs::symlink(&real, &link).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root can write anyway, so the check has nothing to find.
    if std::fs::write(locked.join("probe"), "").is_err() {
        // A new file in the locked directory, and the locked target of a symlink.
        let cases = [
            vec!["--write-allow", arg(&new)],
            vec!["--allow", arg(&link), "--write-allow", arg(&link)],
        ];
        for flags in cases {
            let out = Command::new(env!("CARGO_BIN_EXE_idl-drift"))
                .args([
                    "diff",
                    "--onchain",
                    "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
                    AMM_NEW,
                ])
                .args(&flags)
                .env("IDL_DRIFT_RPC_URL", "http://127.0.0.1:1/")
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(2));
            let text = stderr(&out);
            assert!(
                text.contains("cannot write") && !text.contains("RPC"),
                "{text}"
            );
        }
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_allow_subjects_with_spaces_and_hash_read_back_the_same() {
    let dir = temp_dir("write-allow-subjects");
    let (old, new) = (dir.join("old.json"), dir.join("new.json"));
    std::fs::write(
        &old,
        r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.1.0"},"instructions":[
        {"name":"swap all","discriminator":[1,1,1,1,1,1,1,1],"accounts":[],"args":[]},
        {"name":"fee#1","discriminator":[2,2,2,2,2,2,2,2],"accounts":[],"args":[]},
        {"name":"bad #name","discriminator":[3,3,3,3,3,3,3,3],"accounts":[],"args":[]}]}"#,
    )
    .unwrap();
    std::fs::write(
        &new,
        r#"{"address":"A","metadata":{"name":"t","version":"1","spec":"0.1.0"},"instructions":[]}"#,
    )
    .unwrap();
    let file = dir.join("idl.allow");
    let out = drift(&[
        "diff",
        arg(&old),
        arg(&new),
        "--write-allow",
        arg(&file),
        "--json",
    ]);
    // The reader takes ` #name` as a comment, so the file cannot hold that subject.
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("`instruction_removed bad #name`"),
        "{}",
        stderr(&out)
    );
    let text = std::fs::read_to_string(&file).unwrap();
    let entries: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(
        entries,
        ["instruction_removed fee#1", "instruction_removed swap all"]
    );

    let out = drift(&[
        "diff",
        arg(&old),
        arg(&new),
        "--allow",
        arg(&file),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let report = json(&out);
    assert_eq!(report["unused_allows"], serde_json::json!([]));
    let allowed: Vec<&str> = report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["allowed"] == true)
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(allowed, ["fee#1", "swap all"]);
    std::fs::remove_dir_all(&dir).unwrap();
}
