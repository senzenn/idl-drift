# idl-verify reference

`idl-verify` checks a Solana program IDL against real account, instruction and event data.
It finds an IDL that a parser decodes with no error, but from the wrong bytes.
It classifies each sample by discriminator and decodes it from the IDL alone, with no generated code.

## Usage

```text
idl-verify [OPTIONS] --idl <PATH>
idl-verify timeline [OPTIONS] --idl <PATH>
```

The second form lists the deploys of a program. See [Timeline](#timeline). A run on the Codama conversion of voltr_vault gives this report (shortened):

```text
accounts: 16 sampled, 16 classified
  type                 samples  size  used  layout
  adaptorAddReceipt          5   152   145  borsh
not sampled: 2 of 6 account types, 25 of 28 instructions, 19 of 26 events
error: account adaptorAddReceipt: 7 of 152 bytes unused in 5 of 5 samples
  hint: 152 bytes is exactly the repr(C) layout (8 + 144), which pads 7 bytes before `lastUpdatedEpoch`. ...
  example: AsfkxMdVYjMnr2fdTBMUXhq81hgi2hbENXCy9WhUQF7u
summary: 1 error, 0 warnings, 2 notes
```

In the tables, `size` is the data length of the samples, and `used` is the number of those bytes that the IDL reads. Both include the discriminator.
For instructions, `accounts` is the number of accounts that the samples pass, and `idl accounts` is the number that the IDL lists. For events, `log` and `cpi` count the samples from each [source](#events).

## Options

| Option | Meaning |
|---|---|
| `--idl <PATH>` | Required. The program IDL: Anchor (0.30 spec or legacy) or Codama `rootNode` JSON. |
| `--samples <PATH>` | Captured samples. See [The samples file](#the-samples-file). |
| `--rpc <URL>` | Take samples live over JSON-RPC. Env var: `IDL_VERIFY_RPC_URL`. Use the env var, because `ps` and shell history show the URL. |
| `--program <ID>` | With `--rpc`: the program to sample. Default: the IDL's `address`. |
| `--max-tx <N>` | With `--rpc`: read at most N successful transactions. Default: 40. |
| `--stall <N>` | With `--rpc`: stop when N successful transactions in a row bring no new instruction or event kind. Also stop when N signature pages in a row hold only failed transactions. Default: 40. |
| `--save-samples <PATH>` | With `--rpc`: write the live samples to this file, in the samples format. |
| `--export-fixtures <DIR>` | Write the samples to DIR as parser test fixtures. Needs `--fixture-format`. See [Fixture export](#fixture-export). |
| `--fixture-format <FORMAT>` | `shipstern` (`shipstern-mock` files) or `carbon` (`carbon-test-utils` files). |
| `--reference <PATH>` | Compare values with the output of the [reference decoder](#reference-decoder). |
| `--decoder <CMD>` | Compare values with a decoder under test. See [decoder-protocol.md](decoder-protocol.md). |
| `--compare-idl <PATH>` | Compare values with a second IDL of the same program. See [Compare two IDLs](#compare-two-idls). |
| `--upgrade-impact` | Read `--idl` as the IDL of an upgrade, and put each sampled account in a class. See [Upgrade impact](#upgrade-impact). |
| `--min-coverage <PERCENT>` | Exit 1 when less than PERCENT of the IDL's account types, instructions or events have a sample. Works with `--samples` too. Without it, a run passes on any number of samples. |
| `--fail-on <FAIL_ON>` | The lowest severity that sets exit code 1: `error` (default) or `warning`. |
| `--allow <FILE>` | Accept the findings that FILE lists. See [Allow file](#allow-file). |
| `--write-allow <FILE>` | Write an allow file that accepts each finding at or above `--fail-on`. See [Baseline](#baseline). |
| `--prune-allow` | With `--write-allow`: also remove the `--allow` entries that match no finding. |
| `--json` | Print the report as JSON. See [JSON output](#json-output). |
| `--markdown` | Print the report as GitHub Markdown. See [Markdown output](#markdown-output). |

- Give `--samples` or `--rpc` (or set `IDL_VERIFY_RPC_URL`). If you give both, `--samples` wins and idl-verify does not sample the cluster.
- `--max-tx`, `--stall` or `--save-samples` with `--samples` is a usage error (exit 2). Default values do not count.
- Give `--export-fixtures` and `--fixture-format` together, with `--samples` or `--rpc`.
- Use only one of `--reference`, `--decoder` and `--compare-idl`. Do not use `--upgrade-impact` with `--reference` or `--decoder`.
- Do not use `--json` with `--markdown`. Colors are on only when stdout is a terminal. Set `NO_COLOR` to turn them off.

## Exit codes

The exit codes are in the [README](../README.md#exit-codes). With `--fail-on error`, a run that exits 0 can still report warnings and notes. A run that cannot check the data exits 2. It never passes.

## Allow file

`--allow <FILE>` lists the findings that you accept. An allowed finding does not set exit code 1, but it stays in the report.
The human report marks it `[ALLOWED]`. The JSON report adds `"allowed": true`. The Markdown report puts it in a `<details>` block.

- Each line holds one entry: a check name (the `check` of the finding), a space, and the `subject`, such as `account Vault`.
- Both parts must match the finding exactly, with no wildcards. A `#` at the start of a line or after a space starts a comment. idl-verify ignores blank lines.
- An entry that matches no finding gives a warning on stderr with its line number. JSON lists it in `unused_allows`. The exit code does not change.
- A line with no subject, or an unknown check name, stops the run with exit 2. So a typo cannot let a finding through.

```text
bytes_used account adaptorAddReceipt   # repr(C) padding. Our parser reads the Anchor IDL.
program_id program                     # We sample a devnet deployment.
```

### Baseline

`--write-allow <FILE>` writes one `<check> <subject>` line for each finding at or above `--fail-on` that is not allowed yet.
The lines are in the order of the JSON report. The rules are the same as for [`idl-drift diff --write-allow`](idl-drift.md#baseline):

- FILE must be new, or the `--allow` file. idl-verify keeps each line and comment of the `--allow` file.
- `--prune-allow` removes the entries that match no finding. The JSON report lists them in `pruned_allows`.
- The write is atomic. The report and the exit code are then the ones that `--allow FILE` gives.
- Use it with `--samples`. A live run can sample other transactions, and then find other findings.

## Checks

Each finding has a check name, a subject (such as `account Vault`), a message and usually a hint. It also gives one address or signature to look up on an explorer.

- For a legacy Anchor IDL, idl-verify calculates the discriminators from the names, as Anchor does.
- An instruction with a Codama size discriminator matches only data of that length. The size discriminator can be at any position in the list.
- An account type with no discriminator matches by exact data length. The length is the IDL's `size` (or Codama size discriminator), else the fixed size of the type.

| Check (`--json` name) | Severity | Meaning |
|---|---|---|
| `discriminator` | error or note | Error: a sample's discriminator matches nothing in the IDL, or the IDL has no discriminators. Event samples against an IDL with no `events` give one error. Note: an account type that only its size selects (no discriminator, no Codama size discriminator). A parser that selects by discriminator only, such as Shipstern's, cannot decode it. Note: a program-owned account with no data. It is not checked. Note: with an Anchor IDL, Anchor's IDL instruction (`0x40f4bc78a7e9690a`) and `IdlAccount` (`0x184662bf3a907b9e`). Each Anchor program without the `no-idl` feature has them. They are not checked. |
| `missing_type` | error | An account has no entry in `types`, so idl-verify cannot decode it. |
| `decode` | error | The data does not fit the IDL: it is too short, or holds an invalid bool, option tag, enum tag or UTF-8 string. The message names the field and offset. A NaN float in a Borsh type is also an error, but not in a zero-copy type. So is a vec, set or map of a type with no data. A type tree over 65536 nodes, plus 64 per data byte, is an error too. When each failing account is shorter than the layout, the hint says that they can be older than an upgrade. |
| `bytes_used` | error, warning or note | The IDL reads fewer bytes than the data holds. Accounts: see [Unused bytes](#unused-bytes). Instructions: any byte after the arguments is an error. The IDL is missing an argument or declares one too narrow. Events: trailing bytes are an error if one is not zero, else a note. When each sample has the same trailing bytes, the hint names an N-byte missing field, and an `Option` shape when it fits. |
| `envelope` | error | A self-CPI event sample does not start with the expected envelope: the IDL's envelope, else Anchor's event tag. |
| `layout` | error or warning | Error: a `bytemuck` (Pod) type whose layout has padding, also in a nested struct. The IDL's `repr` or fields are wrong, usually a missing `packed: true`. Warning: the IDL does not fix the layout of a zero-copy type. Causes are an option, vec, enum or generic inside it, `repr(Rust)` without `packed`, or `repr(Rust, packed)` with a bool. idl-verify reports such a type as not verifiable. |
| `size_distribution` | note | A fixed-size account type has more than one size, such as accounts created before an upgrade. |
| `coverage` | error | Only with `--min-coverage`. Less than the given percent of the IDL's account types, instructions or events have a sample. One finding per kind. |
| `account_count` | error or note | Error: samples pass fewer accounts than the IDL lists. The message names the missing accounts and their fixed addresses. A parser that checks the account count rejects such an instruction. Note: only optional accounts are missing at the end, or a Codama instruction with `optionalAccountStrategy: "omitted"` leaves them out. Note: samples pass more accounts than the IDL lists (remaining accounts). |
| `fixed_address` | error, warning or note | An account that the IDL gives an address (a program or sysvar) holds another key. Error: an Anchor `address` holds another key, so the account order is wrong. Note: another key is at the position of a Codama `publicKeyValueNode` default, which the program can accept. Warning: the default key is at another position, so the accounts probably moved. idl-verify accepts the program id for an absent optional account. It accepts the sampled program id where the IDL fixes its own address. |
| `account_flags` | error | An IDL `writable` account gets a read-only key, or an IDL `signer` account gets a key that does not sign the transaction. The account order is probably wrong, or an earlier account is missing. Only samples with flags are checked. idl-verify accepts a key with more privileges than the IDL asks for. The signer check uses top-level instructions only, because a CPI can sign for a PDA. idl-verify accepts a Codama `isSigner: "either"` account and an absent optional account. |
| `pda` | error or note | An account that the IDL derives from seeds (Anchor `pda`, Codama `pdaValueNode` default) gets another key. Error: the derived address is at another position in the same instruction. The account order is wrong, or an earlier account is missing. Note: the derived address is not in the instruction, so the seeds can read data that the IDL does not show. idl-verify skips a sample when a seed cannot resolve: a path into account data (`pool.mint`) or an absent optional account. A seed path into a struct argument (`params.id`) reads the decoded argument. When the IDL names a program for the seeds, idl-verify also tries the sampled program id. |
| `program_id` | warning | The samples' `programId` is not the IDL's `address`. |
| `reference` | error, warning or note | Error: decoded values do not agree with the other decoder. A failure of the other side is an error for the decoder under test or the old IDL, and a warning for the reference decoder. Note: a type was skipped, or the decoder under test skipped a sample kind. See [Compare with another decoder](#compare-with-another-decoder). |
| `upgrade_impact` | error, warning or note | Only with `--upgrade-impact`. Error: an account breaks. Warning: an account needs a realloc. Note: an account has bytes after the data that the new IDL reads. |

### Unused bytes

When an account has more bytes than the IDL reads, idl-verify tries to explain them:

| Case | Severity |
|---|---|
| The size is the `repr(C)` size. The IDL lays out as Borsh a type that the program casts as `repr(C)`, and drops its padding. Fields after the first gap are misread. | error, with the padding location |
| The size is the used size rounded up to 8, on a Borsh type. This is common spare allocation, but not proof. Compare with the program source. | warning |
| The extra bytes are all zero, on a fixed-size Borsh type. This is usually spare allocation from Anchor's `space`, but a field that is always zero looks the same. | warning |
| Any other size on a fixed-size type, or any extra bytes on a zero-copy type. The IDL can be missing a field. | error |
| Trailing zeros on a variable-size type (strings, vecs, options), which usually has an allocation at its maximum size. | note |
| Trailing bytes that are not zero on a variable-size type. The IDL can be missing trailing fields. | warning |

Padding only after the last field moves no field. If the bytes after the data are zero, the other rows apply. If they are not zero but the same in each sample, the hint names a missing field.

### Zero-copy layout

The program casts Anchor `zero_copy` accounts (`serialization: bytemuck` or `bytemuckunsafe`) from the bytes. So idl-verify reads them at `repr(C)` offsets with SBF alignment rules:

- `u128` and `i128` align to 8 (on `x86_64`, to 16). A pubkey is `[u8; 32]` and aligns to 1.
- `packed: true` aligns each field to 1. `align: N` increases the alignment of the struct. A nested struct keeps its own `repr` and padding.
- A zero-copy type with no `repr` in the IDL is read as `repr(C)`, the default of Anchor's `#[zero_copy]`.
- `repr(Rust, packed)` is the default of `#[zero_copy(unsafe)]`, and Anchor writes it into the IDL. idl-verify reads its fields in IDL order with no padding.
- Rust does not promise that order. rustc 1.85 to 1.97 keep it when no field holds a bool, but move a field that holds a bool. Such a type gets a `layout` warning.
- `repr(Rust)` without `packed` does not fix the field order. A type with it gets the same `layout` warning.

`idl-drift inspect --layout` prints these layouts. See [idl-drift.md](idl-drift.md#--layout).

## The samples file

```json
{
  "programId": "vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8",
  "accounts": [{ "address": "HXtk15EA5pBg3rSKxBm8sWPExScPkTknSRp37fXNHgNA", "data": "0wjoKwKYdXc..." }],
  "instructions": [{ "signature": "5uXzA4qK...", "data": "9lI54oPe/fk...", "accounts": ["ST999VUT...", "4sycXz9X..."],
                     "signer": [false, true], "writable": [true, false], "cpi": false, "position": [2, 0] }],
  "events": [{ "signature": "5uXzA4qK...", "source": "log", "data": "eKpmsOmLc5I..." }]
}
```

| Key | Content |
|---|---|
| `accounts[].data` | The full account data in base64, as `getAccountInfo` returns it with `"encoding": "base64"`. |
| `instructions[].data` | The instruction data in base64. `getTransaction` returns base58, so encode it again. |
| `instructions[].accounts` | The account keys, in order. Resolve them through the transaction's account keys and, for v0 transactions, its loaded addresses. |
| `instructions[].signer`, `.writable`, `.cpi` | Optional. Give all three or none. One `signer` and one `writable` flag per key. `signer`: the key signs the transaction. `writable`: the message header or the writable loaded addresses make the key writable. `cpi`: an inner instruction. Without them, `account_flags` skips the sample. |
| `instructions[].position` | Optional. `[top-level index, inner index]`. The inner index is 0 for a top-level instruction, else the place of the CPI in that instruction's list, from 1. Live sampling writes it. Carbon [fixture](#fixture-export) names use it. idl-verify ignores a value of another shape, with one warning. |
| `events[]` | Optional. `"source": "log"`: a base64-decoded `Program data:` line from the program's own invoke frame. `"source": "cpi"`: the data of a self-CPI event instruction, with the envelope. |
| `programId`, `address`, `signature` | Optional, but they make the report useful. |

Include only instructions to the program, inner (CPI) instructions too. idl-verify ignores other keys, such as a `type` label. `fixtures/live/*/samples.json` are real mainnet examples.

## Live sampling

`--rpc` needs `getAccountInfo`, `getSignaturesForAddress`, `getTransaction` and `getMultipleAccounts`. `--upgrade-impact` also needs `getMinimumBalanceForRentExemption`.

- idl-verify reads successful transactions back through history. It skips failed transactions and transactions that the RPC no longer keeps (`null`).
- It takes each instruction to the program (top level and CPI) with its signer and writable flags. It takes self-CPI events as `cpi` events.
- It takes each `Program data:` line as a `log` event when the program is the innermost frame. Lines of a program that it calls do not count.
- It keeps at most 5 samples per instruction discriminator, unknown prefix and event kind. It reads at most 5 program-owned accounts per account type, also with unknown discriminators.
- Sampling stops when each IDL instruction has 5 samples, or at the `--stall` or `--max-tx` limit. It also stops at the last deploy (loader v3 or v4) or at the oldest transaction.
- Each transaction read counts toward both limits, so `--max-tx` usually stops first. A `null` answer counts only toward `--stall`. The report gives the count and the stop reason.
- A run exits 2 when it finds nothing of the program, the URL is empty, the address is not base58, or a request fails.
- idl-verify tries a request up to 5 times after HTTP 429, a server error, a lost connection or a cut-off answer. After a 429, it waits for `Retry-After` (up to 60 s), else 10 s.
- Each try waits up to 10 s to connect and 2 min for the answer. A timeout is tried again only one time. One request waits at most 14 min.
- idl-verify never prints the URL, because providers put the API key in it. `idl-drift diff --onchain` uses the same rules.
- The public mainnet RPC reads old transactions slowly. For a program with little traffic, use a lower `--max-tx` or a private RPC.

## Compare with another decoder

Byte counts cannot find a field read with the wrong type at the correct width, such as an `i64` read as a `u64`. For that, compare each decoded value with another decoder. Use one source at a time. Field names compare without case and without underscores.

`--decoder "<command>"` runs your decoder under test as a child process, such as a binary from Shipstern's `include_shipstern_parser!` or a Carbon decoder. A generated parser reads zero-copy padding, so idl-verify compares padded types too. The protocol is in [decoder-protocol.md](decoder-protocol.md).

### Reference decoder

`tools/reference` decodes the samples with the `BorshCoder` of `@coral-xyz/anchor`. Make the reference from the same samples file. idl-verify refuses a reference whose entries do not align with the samples.

```bash
cd tools/reference && npm install
node reference.mjs path/to/idl.json path/to/samples.json > reference.json
idl-verify --idl path/to/idl.json --samples path/to/samples.json --reference reference.json
```

`@coral-xyz/anchor` ignores `serialization` and `repr`, and misreads zero-copy fields after implicit padding. So idl-verify skips types with padding, with a note.

### Compare two IDLs

`--compare-idl <old.json>` decodes the samples with a second IDL of the program, such as the legacy Anchor IDL before 0.30, or a Codama conversion. It compares values as `--reference` does.

- A sample that the old IDL classifies by another discriminator, or by none, is an error that names both.
- Events compare too. Each IDL removes its own self-CPI envelope. A self-CPI event without the old IDL's envelope is an error.
- Equal values do not prove equal layouts. Dropped padding with zero bytes gives equal values. Run idl-verify on the old IDL alone to see its unused bytes.

## Fixture export

`--export-fixtures <DIR> --fixture-format <FORMAT>` writes the samples to DIR as test fixtures. Shipstern parser or Carbon decoder tests then run on real chain data with no network.

| Format | Files |
|---|---|
| `shipstern` | `<address>_account.json` for each account sample. With `--rpc`, also `<signature>_tx.json` for each transaction that gave a kept sample. The format of `shipstern-mock` 0.12 and 0.13, for `account_fixture!` and `tx_fixture!`. |
| `carbon` | `<address>_account.json` (`TestAccount`) for each account sample. `<signature>_<outer>_<inner>_instruction.json` (`TestInstruction`) for each instruction sample, from [`position`](#the-samples-file). The format of `carbon-test-utils` 2.0, for `read_account` and `read_instruction`. |

- The run still checks the samples, and the findings set the exit code. The export exits 2 when it writes no file, the program id is not base58, or a write fails.
- stderr gives the file count and each sample without a file. A sample with a key or signature that is not base58 gets no file, with a warning.
- Account files get lamports 0, rent epoch 0 and `executable: false`. The owner is `--program`, else the samples' `programId`, else the IDL's `address`.
- From `--samples`, Shipstern gets account files only, because a samples file does not keep whole transactions.
- A Shipstern tx file lists the top-level calls, then the CPIs. Each call gets the log lines of its top-level instruction, and its CPIs in `inner` by stack height.
- Unlike the files that the 0.12 mock fetches itself, these tx files keep events that only `emit_cpi!` writes. Older transactions with no stack height get no CPIs.
- A self-CPI event is also a call. An `include_shipstern_parser!` parser returns `Filtered` for it. With the `program-events` feature, it reads the event from its caller's `inner` list and the log lines.
- Carbon instruction keys get the `signer` and `writable` flags of the sample, else `false`. Events get no Carbon file, because an event sample keeps no accounts.
- A file of the same name is replaced, and other files in DIR stay. A Carbon instruction sample with no valid `position` gets the name `<signature>_<digest>_instruction.json`, with the first 16 hex digits of the file's SHA-256.
- `shipstern_mock::account_fixture!` fetches a missing file from `RPC_ENDPOINT` (devnet by default). In CI, set `RPC_ENDPOINT=http://127.0.0.1:9`, so that a missing file fails.

The `sample_tests!` macro of the [idl-verify-fixtures](../crates/idl-verify-fixtures/README.md) crate reads the export directory at compile time. It makes one `#[test]` per file, named after the file. `examples/fixture-tests/` runs both forms on the pump_fun export.

```rust
// Dev-dependencies: shipstern-core, shipstern-mock, idl-verify-fixtures.
idl_verify_fixtures::sample_tests! { shipstern, dir: "fixtures",
    accounts: my_parser::pump::AccountParser, instructions: my_parser::pump::InstructionParser }
// Dev-dependencies: carbon-core, carbon-test-utils, serde_json, idl-verify-fixtures.
idl_verify_fixtures::sample_tests! { carbon, dir: "tests/fixtures",
    accounts: carbon_pumpfun_decoder::PumpfunDecoder, instructions: carbon_pumpfun_decoder::PumpfunDecoder }
```

- An account test passes on `Ok` from the parser or `Some` from the decoder. A Carbon instruction test passes on `Some`.
- A Shipstern tx test passes when no instruction gives an error other than `Filtered`, and at least one gives `Ok`.
- Each test embeds its file. A Shipstern test never calls `load_fixture`, so the mock never fetches.
- With `expected: "<dir>"`, each test also compares the `{:#?}` output with `<dir>/<file stem>.txt`, from the run-time `CARGO_MANIFEST_DIR`. `IDL_VERIFY_FIXTURES_BLESS=1` writes the files.
- A proc macro cannot watch a directory on stable Rust. After a new export, touch the test file, or add a `build.rs` that prints `cargo:rerun-if-changed=fixtures`.

## Upgrade impact

Before you deploy an upgrade, run with `--upgrade-impact` and the new IDL in `--idl`. idl-verify decodes each sampled account with the new IDL and gives it one class:

| Class | Severity | Rule |
|---|---|---|
| ok | none | The account decodes, and the new IDL reads each byte. |
| shorter | note | The account decodes, and bytes are left over. The type can be smaller, or the account has spare room. |
| realloc | warning | The account decodes only with zero bytes added at the end. The realloc size is the smallest number of zero bytes that works. For a fixed-size or zero-copy type, it is the new size minus the account size. For a variable-size type, it is the bytes that new trailing fields need. |
| broken | error | The discriminator matches no new account type. Or the data does not decode, and zero bytes do not fix it (such as an invalid enum tag or bool). Or a length prefix in the existing bytes points past the data. Or, with `--compare-idl`, a shared field decodes to another value, or a new field takes old reserved bytes that are not zero. A new trailing field reads its length prefix from the zero bytes, so that prefix is 0. |

Add the current IDL with `--compare-idl` to compare the fields that both IDLs name, as [Compare two IDLs](#compare-two-idls) does:

- A field that only one IDL has is not a difference, also inside an option, enum variant, tuple or map. Lists compare on the items that both IDLs read.
- Without `--compare-idl`, a field that changes its type or moves can show as ok, shorter or realloc. Reserved bytes are not checked.
- With it, idl-verify checks the [reserved bytes](idl-drift.md#reserved-bytes) that new fields take (`reserved_bytes_used` in `idl-drift diff`). It finds the reserved array at any depth of the old decode, also for a renamed type.
- A taken reserved byte that is not zero makes the account broken, and the error names the field, the bytes and the sample count. An array that idl-verify cannot find also makes the account broken.

The extra rent of a realloc account is the rent-exempt minimum of the new size minus that of the current size. A live run gets the minimums with `getMinimumBalanceForRentExemption`, one request per size.
With `--samples`, idl-verify uses `(128 + size) * 5080` lamports, as Agave's `Rent::minimum_balance` does. 5080 lamports per byte is the mainnet rate since SIMD-0437. A later SIMD-0437 step can lower it. The output names the rent source.

This new voltr IDL adds a `u64` at the end of the zero-copy `Vault`, and narrows `StrategyInitReceipt.position_value` from `u64` to `u32`. The `u32` keeps its `repr(C)` offset, so without `--compare-idl` all 5 `StrategyInitReceipt` accounts are ok. Output (shortened):

```text
  type                 sampled  ok  shorter  broken  realloc  bytes  rent each
  StrategyInitReceipt        5   2        0       3        0      -  -
  Vault                      5   0        0       0        5      8  40640
  total                     16   3        5       3        5
extra rent for the 5 accounts that need a realloc: 203200 lamports, estimated offline as (128 + size) * 5080 lamports
error: account StrategyInitReceipt: breaks under the new IDL in 3 of 5 samples: `position_value` reads 3670056934 under the new IDL and 415986917350 under the old IDL
warning: account Vault: needs a realloc of 8 bytes under the new IDL in 5 of 5 samples
```

- With `--fail-on error`, a run where accounts only need a realloc exits 0. `--fail-on warning` fails it.
- The run checks accounts only, so `instructions` and `events` are empty. A run with no account data, or with a Shank IDL, exits 2.
- The classes replace `decode`, `bytes_used`, `size_distribution` and `reference` on accounts. `discriminator`, `missing_type` and `layout` still run, and a type that idl-verify cannot decode has no row.
- If the new IDL has another address, give the sampled program with `--program`. The `program_id` warning then says that the addresses differ.
- The counts are of the sample (at most 5 accounts per type), because many RPCs refuse `getProgramAccounts` for a large program.
- A realloc adds zero bytes. If the new program writes other values, the classes do not show it.
- An IDL with no account discriminators matches accounts by size. An account of the old size then matches no type, and is broken.

## Timeline

`idl-verify timeline` lists the deploys of a program. It tells which of your IDLs decodes the transactions of each slot range between two deploys. A backfill (for example with Jetstreamer) can then decode each range with the correct IDL.

| Option | Meaning |
|---|---|
| `--idl <PATH>` | An IDL to try on each range. Give it once for each IDL version. |
| `--program <ID>` | The program. Default: the address of the first `--idl`. |
| `--rpc <URL>` | The JSON-RPC URL. Env var: `IDL_VERIFY_RPC_URL`. |
| `--per-range <N>` | Program transactions to decode in each range. Default: 10. |
| `--max-tx <N>` | Successful transactions to read at most in each range. Default: 40. |
| `--max-scan <N>` | Transactions to read at most to find the deploys. Default: 100. |
| `--json` | Print the result as JSON. |

- The scan reads the transactions of the upgrade authority (and each authority before it) that the `ProgramData` account lists too.
- A deploy is a `DeployWithMaxDataLen` or `Upgrade` of the program, also as a CPI (Squads). Loader v4: a `Deploy` of the program account. An extend or a set-authority keeps the code.
- Immutable program: the scan lists `ProgramData`. A program of another loader cannot change its code, so it has one range.
- The list is complete back to the deploy that made the program. A close and a new deploy at the same address (SIMD-0432) hide the life before it.
- The scan also stops when the RPC has no older signatures, after `--max-scan` transactions, or after 10 pages of 1000 signatures. Then the range from `?` can span several deploys.
- Agave runs new code from the slot after its deploy. So a range runs from the slot after one deploy to the slot of the next.
- For each range, idl-verify reads successful program transactions, newest first, until `--per-range` of them call the program or log its events. It also stops after `--max-tx` reads or 10 pages.
- An IDL fits a range when a check of each sampled transaction finds no error. A range with samples that no IDL fits is a finding. A range with no sample is not.

The table gives the samples that each IDL decodes, and the IDLs that decode all of them (`fits`). Under each range that no IDL fits, each IDL gets one line with its first error and the transaction. stderr gets one line when the scan ends and one per range.

```text
from slot  to slot    samples  IDL 1  IDL 2  fits  deploy
?          456494698  5        1/5    0/5    none  -
error: slots ? to 456494698: no IDL decodes all 5 samples; IDL 1 decodes the most, 1; the range can span several deploys
  IDL 1: discriminator instructions: 0x07051dc4f5176550 matches no instruction discriminator in the IDL, in 1 sample; transaction 4uGdzdBy...
```

`--json` gives one object with `deploys`, `ranges` and `deploy_scan`:

- Each range has `first_slot`, `last_slot` (`null` for now), `deploy_signature`, `samples`, `unreadable`, `best` (the IDLs that decode the most) and `results`.
- `results` has one entry per IDL, with `fit` (`all`, `some`, `none` or `no_sample`) and `first_error`.
- `deploy_scan` has the listed accounts and the stop reason: `first_deploy`, `no_older_signatures`, `scan_limit` or `fixed_code`.

Exit 0: each range with samples has an IDL that decodes all of them. Exit 1: a range has samples that no single IDL decodes. Exit 2: the run cannot run, or no range has a sample.

## Events

A `log` event is `[discriminator][payload]`. A `cpi` event first loses its envelope: the IDL's envelope, else Anchor's `EVENT_IX_TAG` (`e445a52e51cb9a1d`). idl-verify classifies the rest by event discriminator, and decodes it as the `types` entry with the event's name. Sizes and offsets count from the discriminator.

Codama has no envelope node. In a Codama event whose discriminators are all constants, idl-verify reads the constant at offset 0 as the envelope tag. The next constant starts the event. This is a Shipstern convention: Codama itself reads each of these constants as an event discriminator.

## JSON output

`--json` prints the full report as one object. When the run exits 2, stdout holds only `{"error": "<message>", "exit_code": 2}`. The message also goes to stderr.

| Key | Content |
|---|---|
| `program` | `name`, `version` and `address` from the IDL. |
| `samples_program_id` | The `programId` of the samples, or `null`. |
| `account_coverage`, `instruction_coverage`, `event_coverage` | `sampled`, `classified`, and `not_sampled` (the names with no sample). |
| `accounts` | Per account type: `name`, `samples`, `layout`, `sizes` (list of `size` and `count`), `bytes_used` (`min`, `max`) and `expected_size`. `bytes_used` is `null` when no sample decoded. `expected_size` is the IDL size of a fixed-size type, else `null`. Sizes include the discriminator. |
| `instructions` | Per instruction: `name`, `samples`, `idl_accounts`, `sample_accounts` (`min`, `max`), `data_sizes` (`min`, `max`). |
| `events` | Per event: `name`, `samples`, `sources` (`log`, `cpi`), `sizes`, `bytes_used`. Sizes include the discriminator, but not a self-CPI envelope. |
| `reference` | Only after a comparison: `accounts_compared`, `accounts_matched`, `instructions_compared`, `instructions_matched`, `events_compared`, `events_matched`, `skipped`. |
| `sampling` | Only after a live run: `transactions` (successful transactions read) and `stop_reason`. |
| `upgrade_impact` | Only with `--upgrade-impact`. `rent_source` (`rpc` or `formula`), `lamports_per_byte` (the offline rate, or `null` for `rpc`), `types`, `unknown` and `totals`. Each `types` entry has `name`, `sampled`, `ok`, `shorter` (`count`, `bytes`), `realloc` (`count`, `bytes`, `rent_each`, `rent_total`) and `broken` (`count`, `reasons`). Each reason has `reason`, `count`, `detail` and `example`. `bytes` and `rent_each` are `min` and `max`, or `null` when `count` is 0. `unknown` counts the accounts that the `discriminator` check reports as errors, as broken. `totals` has `sampled`, `ok`, `shorter`, `realloc`, `broken` and `rent_total`. Counts are of the sample. Rent is in lamports. |
| `findings` | Each finding: `severity`, `check`, `subject`, `message`, and when present `hint` and `example`. An allowed finding also has `"allowed": true`. |
| `unused_allows` | Only with `--allow` or `--write-allow`: the entries that match no finding, each with `line`, `rule` and `subject`. |
| `pruned_allows` | Only with `--prune-allow`: the entries that the run removed from the allow file, with the same keys. |
| `counts` | `errors`, `warnings`, `notes`. Allowed findings do not count, so the counts agree with the exit code. |
| `exit_code` | The process exit code. |

## Markdown output

`--markdown` prints GitHub Markdown for a job summary (`$GITHUB_STEP_SUMMARY`) or a pull request comment. The [GitHub Action](ci.md) writes it to the job summary.
It starts with a heading: the IDL, the samples, the program, the finding count per severity and the exit code. With `--upgrade-impact`, the upgrade impact table and the extra rent come next.
Then come a findings table (severity, check, subject, message, hint) and the allowed findings in a `<details>` block. IDL names and messages are escaped, so an IDL cannot add HTML, links or table rows.
When the run exits 2, stdout holds one line: `**idl-verify could not run (exit code 2):** <message>`. The message also goes to stderr.

## Library

An example is in the [crate README](../crates/idl-verify/README.md). `idl_verify::decode` is a runtime decoder over the IDL model. `idl_verify::rpc` takes samples from a cluster. `idl_verify::layout` re-exports the `repr(C)` layout calculator from `idl_drift`.

## Limitations

- Shank IDLs: instructions only. idl-verify reads the instruction tag in `discriminant` of a Shank IDL (`metadata.origin: "shank"`). A Shank program tells account types apart by a field, such as a `key` enum, that the IDL does not map. So idl-verify does not check Shank accounts, and gives one note. A Shank `vec` with a `size` does not load (exit 2).
- Live sampling sees recent activity only. The report lists the unused instructions and events under "not sampled". Increase `--max-tx` and `--stall` to look further back, or set `--min-coverage`.
- The timeline decodes a few transactions from the end of each range. An IDL can fit each sample and still miss an unused instruction. The scan does not list a deploy that it cannot read (`null`), or one that a migrated program made under its old loader. The report counts the unread transactions.
- A narrowed last field can hide. If the bytes that the narrower type does not read are zero, they look like spare room: a note, not an error. An example is the high half of a small timestamp.
- A narrowed field in a variable-size account is only a warning, because it looks the same as fields added after the IDL. A parser that ignores trailing bytes, such as Shipstern's account parser, then gives wrong values with no error. Use `--fail-on warning` in CI, or `--compare-idl <old IDL>`.
- Some swapped accounts look correct. `account_flags` and `pda` find a swap only through a missing privilege or a wrong derived key. Two accounts with the same flags and no usable seeds can swap with no finding.
- A field dropped before zero reserved bytes is only a warning, because the extra bytes look like spare allocation. Use `--fail-on warning` in CI, or `--reference` or `--decoder`.
- The reference decoder reads the same IDL. A mismatch shows that one decoder misreads the bytes. It cannot prove that the IDL is correct.
- Codama nodes that idl-drift cannot read. idl-verify uses the idl-drift IDL model. It cannot decode a field that idl-drift cannot resolve, and it reads some nodes in a simpler form. See [idl-drift.md](idl-drift.md#inspect).
- idl-verify follows the Codama spec where Shipstern does not. All discriminators must match, the arguments start after all of them, and omitted arguments and fields stay in the layout. Shipstern 0.12 dispatches on the first discriminator and drops omitted arguments, so it reads Token-2022 extension arguments one byte early. Use `--decoder` to compare with your parser.
- With `optionalAccountStrategy: "omitted"`, idl-verify maps keys to IDL positions only where the position is certain. It skips the other accounts in `fixed_address`, `account_flags` and `pda`.
- A Codama IDL cannot fix an address. A Codama IDL converted from Anchor loses each `address` constraint, so another key there is only a note. Verify the Anchor IDL to get the error.
- Codama spec 1.x only. Spec 2.x renames the `name` key of each node to `identifier`. idl-verify then stops with exit 2: "Codama spec 2.0.0 is not supported; only spec 1.x can be read".
- No Quasar IDLs. An IDL with a top-level `spec`, such as `quasar-idl/1.0.0`, stops with exit 2: "IDL spec quasar-idl/1.0.0 is not supported; only Anchor, Shank and Codama IDLs can be read".
- Bool decoding is Borsh-strict. A bool byte other than 0 or 1 is a `decode` error, as in Shipstern. The Codama spec reads any value other than 1 as false.
