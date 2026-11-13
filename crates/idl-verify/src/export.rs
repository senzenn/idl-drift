//! Fixture export: samples as the test fixture files of a parser framework, so that its
//! tests run on real data with no network.
//!
//! A sample keeps no lamports, rent epoch or executable flag. Each account file gets
//! lamports 0, rent epoch 0 and `executable: false`, and the program as its owner, because
//! both frameworks check the owner. Each file name is a base58 key or signature, so a
//! sample can never name a path outside the export directory.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::ops::Range;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::rpc::{InnerGroup, RecordedInstruction, TransactionRecord};
use crate::samples::{AccountSample, InstructionSample, Samples};

/// The framework whose fixture files an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureFormat {
    /// `shipstern-mock` 0.12: `<address>_account.json`, and `<signature>_tx.json` for each
    /// transaction record.
    Shipstern,
    /// `carbon-test-utils` 2.0: `<address>_account.json` (`TestAccount`) and
    /// `<signature>_<outer>_<inner>_instruction.json` (`TestInstruction`). A sample with no
    /// position gets a digest of the file in place of `<outer>_<inner>`.
    Carbon,
}

impl std::fmt::Display for FixtureFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Shipstern => "shipstern",
            Self::Carbon => "carbon",
        })
    }
}

/// What an export wrote, and the samples that got no file.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct ExportSummary {
    /// Account files.
    pub accounts: usize,
    /// Carbon instruction files.
    pub instructions: usize,
    /// Shipstern transaction files.
    pub transactions: usize,
    /// Why a sample got no file, one line for each.
    pub skipped: Vec<String>,
}

impl ExportSummary {
    /// All files written.
    #[must_use]
    pub fn files(&self) -> usize {
        self.accounts + self.instructions + self.transactions
    }
}

/// Fills one file: the function writes the contents into the writer that it gets.
pub type FillFile<'f> = dyn FnMut(&mut dyn Write) -> io::Result<()> + 'f;

/// Stores one file: its name in the export directory, and the function that fills it.
pub type WriteFile<'w> = dyn FnMut(&str, &mut FillFile<'_>) -> Result<(), Error> + 'w;

/// Writes the fixture files of `samples` and `records` in `format` through `write`, with
/// `program_id` as the owner of each account. Each file is serialized straight into its
/// writer. Shipstern gets account files and one file per record; Carbon gets account and
/// instruction files. `summary` counts the files and lists the samples that got none, also
/// when an error stops the export.
///
/// # Errors
///
/// When `program_id` is not a base58 key, or `write` fails.
pub fn write_fixtures(
    format: FixtureFormat,
    program_id: &str,
    samples: &Samples,
    records: &[TransactionRecord],
    write: &mut WriteFile<'_>,
    summary: &mut ExportSummary,
) -> Result<(), Error> {
    let program = key_bytes(program_id).ok_or_else(|| {
        Error::Usage(format!(
            "fixture export: the program id `{program_id}` is not a base58 key, and each account file needs it as the owner"
        ))
    })?;
    let mut out = Output {
        write,
        digests: HashMap::new(),
        summary,
    };
    for (index, account) in samples.accounts.iter().enumerate() {
        let subject = format!("accounts[{index}]");
        let Some(pubkey) = key_bytes(&account.address) else {
            out.summary.skipped.push(format!(
                "{subject}: `{}` is not a base58 address, so the file has no name",
                account.address
            ));
            continue;
        };
        let name = format!("{}_account.json", account.address);
        let added = match format {
            FixtureFormat::Shipstern => {
                let file = shipstern_account(account, pubkey, program);
                out.add(&subject, name, &file, Style::Compact)?
            }
            FixtureFormat::Carbon => {
                let file = carbon_account(account, program_id);
                out.add(&subject, name, &file, Style::Pretty)?
            }
        };
        if added {
            out.summary.accounts += 1;
        }
    }
    match format {
        FixtureFormat::Shipstern => {
            for record in records {
                let subject = format!("transaction `{}`", record.signature);
                if !is_signature(&record.signature) {
                    let why = "not a base58 signature, so the file has no name";
                    out.summary.skipped.push(format!("{subject}: {why}"));
                    continue;
                }
                let programs: Vec<&str> =
                    record.instructions.iter().map(|ix| &*ix.program).collect();
                let logs = logs_by_top_level(&record.log_messages, &programs);
                match shipstern_tx(program_id, record, &logs) {
                    Ok(file) => {
                        let name = format!("{}_tx.json", record.signature);
                        if out.add(&subject, name, &file, Style::Compact)? {
                            out.summary.transactions += 1;
                        }
                    }
                    Err(why) => out.summary.skipped.push(format!("{subject}: {why}")),
                }
            }
        }
        FixtureFormat::Carbon => carbon_instructions(&mut out, program_id, &samples.instructions)?,
    }
    Ok(())
}

/// Writes files, and keeps a digest of each one to catch two samples with one name.
struct Output<'o, 'w> {
    write: &'o mut WriteFile<'w>,
    digests: HashMap<String, [u8; 32]>,
    summary: &'o mut ExportSummary,
}

impl Output<'_, '_> {
    /// Writes a file, unless an earlier sample gave the same name. Returns whether it wrote.
    fn add(
        &mut self,
        subject: &str,
        name: String,
        file: &impl Serialize,
        style: Style,
    ) -> Result<bool, Error> {
        match self.digests.entry(name) {
            Entry::Vacant(entry) => {
                let mut digest = [0; 32];
                (self.write)(entry.key(), &mut |out| {
                    digest = emit(out, file, style)?;
                    Ok(())
                })?;
                entry.insert(digest);
                Ok(true)
            }
            Entry::Occupied(entry) => {
                if emit(io::sink(), file, style).ok() != Some(*entry.get()) {
                    let name = entry.key();
                    self.summary.skipped.push(format!(
                        "{subject}: an earlier sample with other data gives {name}; the first is kept"
                    ));
                }
                Ok(false)
            }
        }
    }
}

/// `shipstern-mock` 0.12 `AccountInfo`. A `Pubkey` serializes as its 32 bytes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShipsternAccount<'a> {
    data: &'a [u8],
    pubkey: [u8; 32],
    executable: bool,
    lamports: u64,
    owner: [u8; 32],
    rent_epoch: u64,
    space: usize,
}

/// `shipstern-mock` 0.12 `SerializableTransactionFixture`.
#[derive(Serialize)]
struct ShipsternTx<'a> {
    instructions: Vec<ShipsternInstruction<'a>>,
    log_messages: &'a [String],
}

/// `shipstern-mock` 0.12 `SerializableInstructionUpdate`.
#[derive(Serialize)]
struct ShipsternInstruction<'a> {
    ix_index: [usize; 2],
    program: [u8; 32],
    accounts: Vec<[u8; 32]>,
    data: &'a [u8],
    inner: Vec<ShipsternInstruction<'a>>,
    log_messages: &'a [String],
}

/// `carbon-test-utils` 2.0 `TestAccount`.
#[derive(Serialize)]
struct CarbonAccount<'a> {
    data: String,
    executable: bool,
    lamports: u64,
    owner: &'a str,
    rent_epoch: u64,
}

/// `carbon-test-utils` 2.0 `TestInstruction`.
#[derive(Serialize)]
struct CarbonInstruction<'a> {
    program_id: &'a str,
    accounts: Vec<CarbonAccountMeta<'a>>,
    data: String,
}

#[derive(Serialize)]
struct CarbonAccountMeta<'a> {
    pubkey: &'a str,
    is_signer: bool,
    is_writable: bool,
}

/// The mock writes compact JSON, so an exported file looks like a file that it fetches.
fn shipstern_account(
    account: &AccountSample,
    pubkey: [u8; 32],
    owner: [u8; 32],
) -> ShipsternAccount<'_> {
    ShipsternAccount {
        data: &account.data,
        pubkey,
        executable: false,
        lamports: 0,
        owner,
        rent_epoch: 0,
        space: account.data.len(),
    }
}

/// The runtime nests CPIs at most 9 deep (SIMD-0268); a deeper tree is hostile, and the
/// recursion that builds it must stay small.
const MAX_CPI_DEPTH: usize = 16;

/// Each call to the program, listed as `shipstern-mock` 0.12 lists them: the top-level calls,
/// then the CPIs as the meta lists them.
/// `logs` holds the log lines of each top-level instruction.
fn shipstern_tx<'r>(
    program_id: &str,
    record: &'r TransactionRecord,
    logs: &'r [Vec<String>],
) -> Result<ShipsternTx<'r>, String> {
    let logs_of = |outer: usize| logs.get(outer).map_or(&[][..], Vec::as_slice);
    let mut instructions = Vec::new();
    for (i, ix) in record.instructions.iter().enumerate() {
        if ix.program == program_id {
            let inner = top_level_cpis(record, i)?;
            instructions.push(entry([i, 0], ix, inner, logs_of(i))?);
        }
    }
    for group in &record.inner {
        for (j, ix) in group.instructions.iter().enumerate() {
            if ix.program == program_id {
                let inner = cpi_tree(group, j + 1..cpis_end(group, j), 0)?;
                instructions.push(entry([group.index, j], ix, inner, logs_of(group.index))?);
            }
        }
    }
    Ok(ShipsternTx {
        instructions,
        log_messages: &record.log_messages,
    })
}

fn entry<'r>(
    ix_index: [usize; 2],
    ix: &'r RecordedInstruction,
    inner: Vec<ShipsternInstruction<'r>>,
    log_messages: &'r [String],
) -> Result<ShipsternInstruction<'r>, String> {
    let key = |text: &str| key_bytes(text).ok_or_else(|| format!("the key `{text}` is not base58"));
    Ok(ShipsternInstruction {
        ix_index,
        program: key(&ix.program)?,
        accounts: ix
            .accounts
            .iter()
            .map(|k| key(k))
            .collect::<Result<_, _>>()?,
        data: &ix.data,
        inner,
        log_messages,
    })
}

/// The CPIs of the top-level instruction at `index`, nested as shipstern-core 0.12 nests them
/// for a parser: an `emit_cpi!` event can be only there, with no `Program data:` line.
fn top_level_cpis(
    record: &TransactionRecord,
    index: usize,
) -> Result<Vec<ShipsternInstruction<'_>>, String> {
    let mut out = Vec::new();
    for group in record.inner.iter().filter(|g| g.index == index) {
        out.extend(cpi_tree(group, 0..group.instructions.len(), 0)?);
    }
    Ok(out)
}

/// The instructions in `range` of `group`, each with the CPIs it makes in `inner`.
fn cpi_tree(
    group: &InnerGroup,
    range: Range<usize>,
    depth: usize,
) -> Result<Vec<ShipsternInstruction<'_>>, String> {
    if depth > MAX_CPI_DEPTH {
        return Err(format!(
            "CPIs nest more than {MAX_CPI_DEPTH} deep, which the runtime does not allow"
        ));
    }
    let mut out = Vec::new();
    let mut j = range.start;
    while j < range.end {
        let Some(ix) = group.instructions.get(j) else {
            break;
        };
        let end = cpis_end(group, j).min(range.end);
        let inner = cpi_tree(group, j + 1..end, depth + 1)?;
        out.push(entry([group.index, j], ix, inner, &[])?);
        j = end;
    }
    Ok(out)
}

/// The end of the CPIs that the instruction at `j` makes: the run of later instructions with
/// a greater stack height. An instruction with no stack height makes none that can be known.
fn cpis_end(group: &InnerGroup, j: usize) -> usize {
    let items = &group.instructions;
    let Some(height) = items.get(j).and_then(|ix| ix.stack_height) else {
        return j + 1;
    };
    let deeper = |ix: &RecordedInstruction| ix.stack_height.is_some_and(|h| h > height);
    let mut after = items.iter().skip(j + 1);
    after
        .position(|ix| !deeper(ix))
        .map_or(items.len(), |n| j + 1 + n)
}

fn carbon_account<'a>(account: &AccountSample, owner: &'a str) -> CarbonAccount<'a> {
    CarbonAccount {
        data: STANDARD.encode(&account.data),
        executable: false,
        lamports: 0,
        owner,
        rent_epoch: 0,
    }
}

/// One file per instruction sample, named by its place in the transaction. A sample from an
/// older samples file has no place, so a digest of the file names it. Either way one name
/// always holds the same bytes. A sample with no flags gets `false` for each key.
fn carbon_instructions(
    out: &mut Output<'_, '_>,
    program_id: &str,
    instructions: &[InstructionSample],
) -> Result<(), Error> {
    for (index, ix) in instructions.iter().enumerate() {
        let subject = format!("instructions[{index}]");
        if !is_signature(&ix.signature) {
            out.summary.skipped.push(format!(
                "{subject}: `{}` is not a base58 signature, so the file has no name",
                ix.signature
            ));
            continue;
        }
        if let Some(bad) = ix.accounts.iter().find(|k| key_bytes(k).is_none()) {
            out.summary.skipped.push(format!(
                "{subject}: the account key `{bad}` is not base58, and carbon-test-utils parses each key"
            ));
            continue;
        }
        let (signer, writable) = ix.flags.as_ref().map_or((&[][..], &[][..]), |f| {
            (f.signer.as_slice(), f.writable.as_slice())
        });
        let accounts = ix
            .accounts
            .iter()
            .enumerate()
            .map(|(i, pubkey)| CarbonAccountMeta {
                pubkey,
                is_signer: signer.get(i) == Some(&true),
                is_writable: writable.get(i) == Some(&true),
            })
            .collect();
        let file = CarbonInstruction {
            program_id,
            accounts,
            data: hex(&ix.data),
        };
        let place = match ix.position {
            Some([outer, inner]) => format!("{outer}_{inner}"),
            None => hex(&emit(io::sink(), &file, Style::Pretty).unwrap_or_default()[..8]),
        };
        let name = format!("{}_{place}_instruction.json", ix.signature);
        if out.add(&subject, name, &file, Style::Pretty)? {
            out.summary.instructions += 1;
        }
    }
    Ok(())
}

/// Shipstern files are compact JSON, as the mock writes them; Carbon files are pretty JSON.
#[derive(Clone, Copy)]
enum Style {
    Compact,
    Pretty,
}

/// Passes bytes on, and keeps a SHA-256 of them.
struct Hashing<W> {
    inner: W,
    sha: Sha256,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.sha.update(buf.get(..n).unwrap_or_default());
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Serializes `value` into `out` with no copy of the whole file in memory, and returns the
/// SHA-256 of the bytes.
fn emit(out: impl Write, value: &impl Serialize, style: Style) -> io::Result<[u8; 32]> {
    let mut out = Hashing {
        inner: out,
        sha: Sha256::new(),
    };
    match style {
        Style::Compact => serde_json::to_writer(&mut out, value)?,
        Style::Pretty => {
            serde_json::to_writer_pretty(&mut out, value)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(out.sha.finalize().into())
}

fn hex(data: &[u8]) -> String {
    let mut out = String::new();
    for b in data {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn key_bytes(text: &str) -> Option<[u8; 32]> {
    bs58::decode(text).into_vec().ok()?.try_into().ok()
}

fn is_signature(text: &str) -> bool {
    bs58::decode(text).into_vec().is_ok_and(|b| b.len() == 64)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Invoke,
    Close,
    Other,
}

/// The shape of a log line, as shipstern-core 0.12 `classify_log_line` reads it.
fn line_kind(line: &str) -> LineKind {
    let Some((second, tail)) = line
        .strip_prefix("Program ")
        .and_then(|rest| rest.split_once(' '))
    else {
        return LineKind::Other;
    };
    if matches!(second, "log:" | "data:" | "return:") {
        return LineKind::Other;
    }
    let depth = tail
        .strip_prefix("invoke [")
        .and_then(|after| after.split(']').next());
    if depth.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())) {
        LineKind::Invoke
    } else if tail == "success" || tail.starts_with("failed:") {
        LineKind::Close
    } else {
        LineKind::Other
    }
}

/// The log lines of each `invoke [1]` frame, to the line that closes it, as shipstern-core
/// `split_logs_by_outer_ix` cuts them.
fn top_level_frames(logs: &[String]) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut depth = 0u32;
    for line in logs {
        let kind = line_kind(line);
        if depth == 0 {
            if kind == LineKind::Invoke && line.contains(" invoke [1]") {
                groups.push(vec![line.clone()]);
                depth = 1;
            }
            continue;
        }
        match kind {
            LineKind::Invoke => depth = depth.saturating_add(1),
            LineKind::Close => depth -= 1,
            LineKind::Other => {}
        }
        if let Some(group) = groups.last_mut() {
            group.push(line.clone());
        }
    }
    groups
}

/// The log lines of each top-level instruction, whose programs are `programs`. A precompile
/// writes no line, so a frame goes only to the instruction whose program opened it, as
/// Shipstern does after #377. `shipstern-mock` 0.12 gives frame `i` to instruction `i`.
fn logs_by_top_level(logs: &[String], programs: &[&str]) -> Vec<Vec<String>> {
    let mut frames = top_level_frames(logs).into_iter().peekable();
    programs
        .iter()
        .map(|program| {
            let invoke = format!("Program {program} invoke [1]");
            match frames.peek().and_then(|frame| frame.first()) {
                Some(first) if *first == invoke => frames.next().unwrap_or_default(),
                _ => Vec::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::KeyFlags;

    fn key(n: u8) -> String {
        bs58::encode([n; 32]).into_string()
    }

    fn sig(n: u8) -> String {
        bs58::encode([n; 64]).into_string()
    }

    /// `[n; 32]` as JSON, the way a `Pubkey` serializes.
    fn bytes(n: u8) -> String {
        format!("[{}]", vec![n.to_string(); 32].join(","))
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(ToString::to_string).collect()
    }

    /// One account of program `key(9)`, and one instruction with flags.
    fn samples() -> Samples {
        Samples {
            program_id: Some(key(9)),
            accounts: vec![AccountSample {
                address: key(1),
                data: vec![7, 0, 255],
            }],
            instructions: vec![InstructionSample {
                signature: sig(2),
                data: vec![0xab, 1],
                accounts: vec![key(3), key(4)],
                flags: Some(KeyFlags {
                    signer: vec![true, false],
                    writable: vec![true, true],
                    cpi: true,
                }),
                position: Some([1, 2]),
            }],
            events: Vec::new(),
        }
    }

    /// The files of one export, by name.
    fn export(
        format: FixtureFormat,
        samples: &Samples,
        records: &[TransactionRecord],
    ) -> (std::collections::BTreeMap<String, String>, ExportSummary) {
        let mut files = std::collections::BTreeMap::new();
        let mut write = |name: &str, fill: &mut FillFile<'_>| {
            let mut bytes = Vec::new();
            fill(&mut bytes).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(
                files.insert(name.to_string(), text).is_none(),
                "{name} twice"
            );
            Ok(())
        };
        let mut summary = ExportSummary::default();
        write_fixtures(format, &key(9), samples, records, &mut write, &mut summary).unwrap();
        (files, summary)
    }

    #[test]
    fn a_shipstern_account_file_is_the_mock_account_info() {
        let (files, summary) = export(FixtureFormat::Shipstern, &samples(), &[]);
        // A samples file keeps no whole transaction, so only the account has a file.
        assert_eq!((summary.accounts, summary.transactions), (1, 0));
        let expected = format!(
            r#"{{"data":[7,0,255],"pubkey":{},"executable":false,"lamports":0,"owner":{},"rentEpoch":0,"space":3}}"#,
            bytes(1),
            bytes(9)
        );
        assert_eq!(files[&format!("{}_account.json", key(1))], expected);
        assert_eq!(files.len(), 1);
    }

    fn recorded(program: u8, accounts: &[u8], data: u8, height: u32) -> RecordedInstruction {
        RecordedInstruction {
            program: key(program),
            accounts: accounts.iter().map(|&n| key(n)).collect(),
            data: vec![data],
            stack_height: Some(height),
        }
    }

    #[test]
    fn a_shipstern_tx_file_nests_cpis_and_skips_the_logs_of_a_precompile() {
        // Program 9 at index 1, after a precompile (5) that writes no log line, then 6.
        // Its CPIs: program 7, which calls 9, then a self-CPI of 9.
        let p = key(9);
        let logs = vec![
            format!("Program {p} invoke [1]"),
            format!("Program {} invoke [2]", key(7)),
            format!("Program {p} invoke [3]"),
            format!("Program {p} success"),
            format!("Program {} success", key(7)),
            format!("Program {p} invoke [2]"),
            format!("Program {p} success"),
            format!("Program {p} success"),
            format!("Program {} invoke [1]", key(6)),
            format!("Program {} success", key(6)),
        ];
        let record = TransactionRecord {
            signature: sig(2),
            instructions: vec![
                recorded(5, &[], 0, 1),
                recorded(9, &[3], 1, 1),
                recorded(6, &[], 0, 1),
            ],
            inner: vec![InnerGroup {
                index: 1,
                instructions: vec![
                    recorded(7, &[], 2, 2),
                    recorded(9, &[3], 3, 3),
                    recorded(9, &[4], 4, 2),
                ],
            }],
            log_messages: logs.clone(),
        };
        let (files, summary) = export(FixtureFormat::Shipstern, &Samples::default(), &[record]);
        assert_eq!(summary.transactions, 1);
        let ix = |index: &str,
                  program: u8,
                  account: Option<u8>,
                  data: u8,
                  inner: &str,
                  logs: &str| {
            let accounts = account.map_or("[]".to_string(), |n| format!("[{}]", bytes(n)));
            format!(
                r#"{{"ix_index":{index},"program":{},"accounts":{accounts},"data":[{data}],"inner":[{inner}],"log_messages":{logs}}}"#,
                bytes(program)
            )
        };
        let json = |lines: &[String]| serde_json::to_string(lines).unwrap();
        // The frame of program 9: lines 0 to 7. The CPIs carry no lines of their own.
        let own = json(&logs[..8]);
        let cpi_tree = [
            ix(
                "[1,0]",
                7,
                None,
                2,
                &ix("[1,1]", 9, Some(3), 3, "", "[]"),
                "[]",
            ),
            ix("[1,2]", 9, Some(4), 4, "", "[]"),
        ];
        let calls = [
            ix("[1,0]", 9, Some(3), 1, &cpi_tree.join(","), &own),
            ix("[1,1]", 9, Some(3), 3, "", &own),
            ix("[1,2]", 9, Some(4), 4, "", &own),
        ];
        let expected = format!(
            r#"{{"instructions":[{}],"log_messages":{}}}"#,
            calls.join(","),
            json(&logs)
        );
        assert_eq!(files[&format!("{}_tx.json", sig(2))], expected);
    }

    #[test]
    fn a_log_frame_goes_only_to_the_instruction_whose_program_opened_it() {
        // Shipstern #377: Ed25519 at index 2 writes no line, so index 3 keeps its own frame.
        let logs = lines(&[
            "Program CB invoke [1]",
            "Program CB success",
            "Program CB invoke [1]",
            "Program CB success",
            "Program F invoke [1]",
            "Program log: Instruction: Commit",
            "Program F success",
        ]);
        let frames = logs_by_top_level(&logs, &["CB", "CB", "Ed25519", "F"]);
        let sizes: Vec<usize> = frames.iter().map(Vec::len).collect();
        assert_eq!(sizes, [2, 2, 0, 3]);
        assert_eq!(frames[3][0], "Program F invoke [1]");
    }

    #[test]
    fn carbon_files_are_test_account_and_test_instruction() {
        let (files, summary) = export(FixtureFormat::Carbon, &samples(), &[]);
        assert_eq!((summary.accounts, summary.instructions), (1, 1));
        let account = format!(
            "{{\n  \"data\": \"BwD/\",\n  \"executable\": false,\n  \"lamports\": 0,\n  \"owner\": \"{}\",\n  \"rent_epoch\": 0\n}}\n",
            key(9)
        );
        assert_eq!(files[&format!("{}_account.json", key(1))], account);
        let meta = |pubkey: String, signer: bool, writable: bool| {
            format!("    {{\n      \"pubkey\": \"{pubkey}\",\n      \"is_signer\": {signer},\n      \"is_writable\": {writable}\n    }}")
        };
        let instruction = format!(
            "{{\n  \"program_id\": \"{}\",\n  \"accounts\": [\n{},\n{}\n  ],\n  \"data\": \"ab01\"\n}}\n",
            key(9),
            meta(key(3), true, true),
            meta(key(4), false, true)
        );
        // Named by its place in the transaction: CPI 2 of top-level instruction 1.
        assert_eq!(
            files[&format!("{}_1_2_instruction.json", sig(2))],
            instruction
        );
    }

    #[test]
    fn carbon_names_stay_with_the_instruction_across_exports() {
        // An older samples file has no position: the name holds a digest of the file.
        let mut s = samples();
        s.instructions[0].position = None;
        let (first, _) = export(FixtureFormat::Carbon, &s, &[]);
        let mut second_sample = s.instructions[0].clone();
        second_sample.data = vec![5];
        s.instructions.insert(0, second_sample);
        let (both, _) = export(FixtureFormat::Carbon, &s, &[]);
        // The first export's file keeps its name and bytes when another sample comes first.
        assert!(first
            .iter()
            .all(|(name, text)| both.get(name) == Some(text)));
        assert_eq!(both.len(), 3);
    }

    #[test]
    fn samples_with_no_safe_file_name_or_a_second_body_are_skipped() {
        let mut s = samples();
        let account = |address: &str, data: Vec<u8>| AccountSample {
            address: address.to_string(),
            data,
        };
        s.accounts.push(account("../evil", vec![1]));
        s.accounts.push(account(&key(1), vec![8]));
        s.accounts.push(account(&key(1), vec![7, 0, 255]));
        let ix = |signature: &str, accounts: Vec<String>| InstructionSample {
            signature: signature.to_string(),
            data: vec![5],
            accounts,
            flags: None,
            position: Some([0, 0]),
        };
        s.instructions.push(ix("", Vec::new()));
        s.instructions
            .push(ix(&sig(5), vec!["bad/key".to_string()]));
        let (files, summary) = export(FixtureFormat::Carbon, &s, &[]);
        assert_eq!(files.len(), 2);
        let starts: Vec<&str> = summary.skipped.iter().map(|l| &l[..15]).collect();
        assert_eq!(
            starts,
            [
                "accounts[1]: `.",
                "accounts[2]: an",
                "instructions[1]",
                "instructions[2]"
            ]
        );
    }

    #[test]
    fn hostile_records_are_skipped() {
        // Stack heights deeper than the runtime allows, and a key that is not base58.
        let deep = (2..40).map(|h| recorded(7, &[], 0, h)).collect();
        let mut bad_key = recorded(9, &[], 0, 1);
        bad_key.accounts.push("bad/key".into());
        let record = |n: u8, instructions, inner| TransactionRecord {
            signature: sig(n),
            instructions,
            inner,
            log_messages: Vec::new(),
        };
        let records = [
            record(
                2,
                vec![recorded(9, &[], 0, 1)],
                vec![InnerGroup {
                    index: 0,
                    instructions: deep,
                }],
            ),
            record(3, vec![bad_key], Vec::new()),
        ];
        let (files, summary) = export(FixtureFormat::Shipstern, &Samples::default(), &records);
        assert!(files.is_empty());
        assert_eq!(summary.skipped.len(), 2, "{:?}", summary.skipped);
    }

    #[test]
    fn a_program_id_that_is_not_a_key_stops_the_export() {
        for format in [FixtureFormat::Shipstern, FixtureFormat::Carbon] {
            let mut write = |_: &str, _: &mut FillFile<'_>| Ok(());
            let mut summary = ExportSummary::default();
            let err =
                write_fixtures(format, "P", &samples(), &[], &mut write, &mut summary).unwrap_err();
            assert!(matches!(err, Error::Usage(_)), "{err}");
            assert!(err.to_string().contains("`P` is not a base58 key"), "{err}");
        }
    }

    #[test]
    fn frames_split_as_shipstern_core_splits_them() {
        let logs = lines(&[
            "outside any frame",
            "Program A invoke [1]",
            "Program log: Program B invoke [1]",
            "Program A success",
            "between frames",
            "Program B invoke [1]",
            "Program B failed: custom program error: 0x1",
        ]);
        assert_eq!(
            top_level_frames(&logs),
            [
                lines(&[
                    "Program A invoke [1]",
                    "Program log: Program B invoke [1]",
                    "Program A success"
                ]),
                lines(&[
                    "Program B invoke [1]",
                    "Program B failed: custom program error: 0x1"
                ]),
            ]
        );
    }
}
