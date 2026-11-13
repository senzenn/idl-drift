# idl-verify reference

`idl-verify` checks a Solana program IDL against real account, instruction and event data.
It finds an IDL that a parser decodes with no error, but from the wrong bytes. It classifies
each sample by discriminator, decodes it from the IDL alone (no generated code), and reports
the data that does not fit.

## Usage

```text
idl-verify [OPTIONS] --idl <PATH>
idl-verify timeline [OPTIONS] --idl <PATH>...
```

The second form lists the deploys of a program (see [Timeline](#timeline)).

The Codama conversion of voltr_vault, run from the repository root:

```bash
idl-verify --idl fixtures/live/voltr_vault/idl.js-codama.json \
           --samples fixtures/live/voltr_vault/samples.json
```

```text
voltrVault 0.2.0  vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8

accounts: 16 sampled, 16 classified
  type                 samples  size  used  layout
  adaptorAddReceipt          5   152   145  borsh
  protocol                   1   110   110  borsh
  strategyInitReceipt        5   192   192  borsh
  vault                      5   928   928  borsh

instructions: 20 sampled, 20 classified
  name              samples  accounts  idl accounts  data
  depositStrategy        13     15-25            14  30-91
  depositVault            6        13            13  16
  withdrawStrategy        1        16            14  42

events: 25 sampled, 25 classified
  name                          samples  log  cpi          size  used
  depositStrategyEvent                5    5    0           312  312
  ...

not sampled: 2 of 6 account types, 25 of 28 instructions, 19 of 26 events

error: account adaptorAddReceipt: 7 of 152 bytes unused in 5 of 5 samples
  hint: 152 bytes is exactly the repr(C) layout (8 + 144), which pads 7 bytes before `lastUpdatedEpoch`. The IDL lays this type out as Borsh, without the padding. A decoder built from it reads `lastUpdatedEpoch` and every later field at the wrong offset. Keep the Anchor IDL's `serialization` and `repr`, or add the padding as explicit fields.
  example: AsfkxMdVYjMnr2fdTBMUXhq81hgi2hbENXCy9WhUQF7u
note: instruction depositStrategy: up to 11 accounts passed beyond the 14 the IDL lists (remaining accounts), in 13 of 13 samples
note: instruction withdrawStrategy: up to 2 accounts passed beyond the 14 the IDL lists (remaining accounts), in 1 of 1 sample

summary: 1 error, 0 warnings, 2 notes
```

The same samples against the original Anchor `idl.json` give no error. Its
`serialization: bytemuckunsafe` and `repr` let idl-verify lay out the type as the program
does.

In the tables:

- `size` is the data length of the samples. `used` is the number of those bytes that the
  IDL reads. Both include the discriminator.
- For instructions, `accounts` is the number of accounts that the samples pass, and
  `idl accounts` is the number that the IDL lists.
- For events, `log` and `cpi` count the samples from each [source](#events).

## Options

| Option | Meaning |
|---|---|
| `--idl <PATH>` | The program IDL: Anchor (0.30 spec or legacy) or Codama `rootNode` JSON. Required. |
| `--samples <PATH>` | Captured samples. See [the samples file](#the-samples-file). |
| `--rpc <URL>` | Take samples from a live cluster over JSON-RPC. The env var `IDL_VERIFY_RPC_URL` sets the same value. Use the env var: `ps` and shell history show a URL on the command line. |
| `--program <ID>` | With `--rpc`: the program to sample. Default: the IDL's `address`. |
| `--max-tx <N>` | With `--rpc`: read at most N successful transactions. Default: 40. |
| `--stall <N>` | With `--rpc`: stop after N successful transactions in a row bring no new instruction or event kind (a transaction that the RPC no longer keeps counts too), or after N pages of signatures in a row hold only failed transactions. Default: 40. A transaction that is read counts toward both limits, so with the defaults `--max-tx` usually stops first. A transaction that the RPC answers with `null` counts only toward N, so N such answers in a row stop the run before `--max-tx` does. |
| `--save-samples <PATH>` | With `--rpc`: write the live samples to this file, in the samples format. |
| `--export-fixtures <DIR>` | Write the samples to this directory as test fixtures of a parser framework. Needs `--fixture-format`. See [fixture export](#fixture-export). |
| `--fixture-format <FORMAT>` | The format of `--export-fixtures`: `shipstern` (`shipstern-mock` files) or `carbon` (`carbon-test-utils` files). |
| `--reference <PATH>` | Compare values with the output of the reference decoder. See [reference decoder](#reference-decoder). |
| `--decoder <CMD>` | Compare values with a decoder under test. See [decoder-protocol.md](decoder-protocol.md). |
| `--compare-idl <PATH>` | Compare values with a second IDL of the same program. See [compare two IDLs](#compare-two-idls). |
| `--upgrade-impact` | Read `--idl` as the IDL of a program upgrade, and put each sampled account in a class: ok, shorter, needs a realloc or broken. See [upgrade impact](#upgrade-impact). |
| `--min-coverage <PERCENT>` | Fail when less than this percent of the IDL's account types, instructions or events have a sample. |
| `--fail-on <SEVERITY>` | The lowest severity that sets exit code 1: `error` (default) or `warning`. |
| `--allow <FILE>` | Findings that you accept. They do not set exit code 1. See [Allow file](#allow-file). |
| `--write-allow <FILE>` | Write an allow file that accepts each finding at or above `--fail-on`. See [Baseline](#baseline). |
| `--prune-allow` | With `--write-allow`: also remove the entries of the `--allow` file that match no finding. The JSON report lists them in `pruned_allows`. |
| `--json` | Print the report as JSON. See [JSON output](#json-output). |
| `--markdown` | Print the report as GitHub Markdown. See [Markdown output](#markdown-output). |

Rules for the options:

- Give `--samples` or `--rpc` (or set `IDL_VERIFY_RPC_URL`). When both are set, `--samples`
  wins, and idl-verify does not sample the cluster. `--max-tx` or `--stall` with `--samples`
  is a usage error (exit 2). Their default values do not count.
- `--save-samples` saves only live samples. With `--samples` it is a usage error (exit 2).
- `--export-fixtures` and `--fixture-format` go together. They work with `--samples` and
  with `--rpc`.
- `--reference`, `--decoder` and `--compare-idl` cannot be combined. `--upgrade-impact`
  cannot be combined with `--reference` or `--decoder`.
- `--json` and `--markdown` cannot be combined.
- Colors are on only when stdout is a terminal. Set `NO_COLOR` to turn them off.

## Exit codes

The exit codes are in the [README](../README.md#exit-codes). With the default
`--fail-on error`, a run that exits 0 can still report warnings and notes. A run never
passes on data that it did not check: it exits 2.

## Allow file

`--allow <FILE>` lists the findings that you accept. An allowed finding does not set exit
code 1, but it stays in the report. The human report marks it `[ALLOWED]`, the JSON report
adds `"allowed": true` to it, and the Markdown report puts it in a `<details>` block.

Each line of the file holds one entry: a check name (the `check` of the finding), a space,
and the `subject`, such as `account Vault`. Both must match the finding exactly. There are
no wildcards. A `#` at the start of a line or after a space starts a comment, to the end of
the line. Use the comment for the reason. Blank lines are ignored.

```text
# Known and accepted.
bytes_used account adaptorAddReceipt   # repr(C) padding. Our parser reads the Anchor IDL.
program_id program                     # We sample a devnet deployment.
```

- An entry that matches no finding gives a warning on stderr with its line number. The JSON
  report lists it in `unused_allows`. It does not change the exit code.
- A line with no subject, or a check name that idl-verify does not have, stops the run with
  exit 2. Thus a typo cannot let a finding through.

### Baseline

`--write-allow <FILE>` writes an allow file with one `<check> <subject>` line for each
finding at or above `--fail-on` that is not allowed yet, in the order of the JSON report.
The rules are the same as for
[`idl-drift diff --write-allow`](idl-drift.md#baseline): FILE must be new, or the `--allow`
file, which keeps each line and comment. `--prune-allow` removes the entries that match no
finding. The write is atomic, and the report and the exit code are then the ones that
`--allow FILE` gives.

```bash
idl-verify --idl idl.json --samples samples.json --write-allow verify.allow
idl-verify --idl idl.json --samples samples.json --allow verify.allow   # exit 0
```

Use it with `--samples`: a live run can sample other transactions, and then find other
findings.

## Checks

Each finding names its check, a subject (such as `account Vault` or `instruction deposit`), a
message, usually a hint, and one affected address or signature that you can look up on an
explorer.

| Check (`--json` name) | Severity | What it means |
|---|---|---|
| `discriminator` | error or note | A sample's discriminator matches nothing in the IDL. The program uses an account type, instruction or event that the IDL does not have, or the IDL has no discriminators. Event samples against an IDL with no `events` give one error. For legacy Anchor IDLs, idl-verify calculates the discriminators from the names, as Anchor does. An instruction with a Codama size discriminator matches only data of that length, at any position of the discriminator list. An account type with no discriminator matches by exact data length: the IDL's `size` (or Codama size discriminator), else the type's fixed size. Note: an account type picked only by size, with no discriminator and no Codama size discriminator. A parser that selects accounts only by a discriminator, such as Shipstern's, cannot decode it. Note: an account that the program owns and that holds no data. It has no discriminator, so no IDL can describe it, and it is not checked. Note: with an Anchor IDL, Anchor's IDL instruction (`0x40f4bc78a7e9690a`) and `IdlAccount` (`0x184662bf3a907b9e`). Each Anchor program built without the `no-idl` feature has them, and an Anchor IDL lists neither, so they are not checked. |
| `missing_type` | error | An account has no entry in `types`, so it cannot be decoded. |
| `decode` | error | The data does not fit the IDL: it is too short, or it has an invalid bool, option tag, enum tag or UTF-8 string. A NaN float is an error in a Borsh type, because Borsh refuses it, but not in a zero-copy type. A vec, set or map of a type with no data (such as an empty struct) is also an error, because Borsh refuses it. A type tree that needs more than 65536 nodes, plus 64 for each byte of data, is an error too, because an IDL can be hostile. The message names the field and offset. When each failing account is shorter than the layout, the hint says so: these accounts can be older than an upgrade. |
| `bytes_used` | error, warning or note | The IDL reads fewer bytes than the data holds. See [unused bytes](#unused-bytes). For instructions, any byte after the arguments is an error: instruction data has no spare room, so the IDL is missing an argument or declares one too narrow. For events, bytes after the payload are an error when a byte is not zero, else a note. When each sample has the same trailing bytes, the hint names an N-byte missing field, and an `Option` shape when it fits. |
| `envelope` | error | A self-CPI event sample does not start with the expected envelope: the IDL's envelope, else Anchor's event tag. |
| `layout` | error or warning | Error: a `bytemuck` (Pod) type whose calculated layout has padding, also inside a nested struct. Pod forbids padding, so the IDL's `repr` or fields are wrong (usually a missing `packed: true`). Warning: a zero-copy type whose layout the IDL does not fix (an option, vec, enum or generic inside it, `repr(Rust)` without `packed`, or `repr(Rust, packed)` with a bool inside it). idl-verify reports it as not verifiable, and does not guess. |
| `size_distribution` | note | A fixed-size account type has more than one size, such as accounts created before an upgrade. |
| `coverage` | error | Only with `--min-coverage`: less than the given percent of the IDL's account types, instructions or events have a sample. One finding for each kind that is short. |
| `account_count` | error or note | Error: samples pass fewer accounts than the IDL lists. The message names the missing accounts and their fixed addresses. A parser that checks the account count rejects each such instruction. Note: only optional accounts are missing at the tail, or a Codama instruction with `optionalAccountStrategy: "omitted"` leaves out absent optional accounts. Note: samples pass more accounts than the IDL lists (remaining accounts). |
| `fixed_address` | error, warning or note | An account that the IDL gives an address (a program or sysvar) holds another key. Error: an Anchor `address` holds another key, so the IDL's account order does not match the program. A Codama `publicKeyValueNode` default fills the account only when the caller gives no key, so the program can accept another key. Note: another key is at the position of a Codama default. Warning: the default key is at another position, so the accounts likely moved. An absent optional account passed as the program id is accepted. An account fixed to the IDL's own address accepts the sampled program id, because a program runs only at its own id. |
| `account_flags` | error | An account that the IDL marks `writable` gets a key that the transaction makes read-only, or an account that the IDL marks `signer` gets a key that does not sign the transaction. The IDL's account order probably does not match the program, or an earlier account is missing. Only samples with flags are checked (see [the samples file](#the-samples-file)). A key with more privileges than the IDL asks for is accepted, because the transaction flags cover each instruction in it. The signer check uses only top-level instructions, because a CPI can sign for a PDA. A Codama `isSigner: "either"` account and an absent optional account are accepted. |
| `pda` | error or note | An account that the IDL derives from seeds (Anchor `pda`, Codama `pdaValueNode` default) gets a key that its seeds do not give. Error: the derived address is passed at another position in the same instruction, so the IDL's account order does not match the program, or an earlier account is missing. Note: the derived address is not in the instruction, so the seeds may read data that the IDL does not show. A sample is skipped when a seed cannot be resolved: a path into account data (`pool.mint`) or an absent optional account. A seed path into a struct argument (`params.id`) reads the decoded argument. When the IDL names a program for the seeds, the sampled program id is also tried, because the IDL can come from another deployment. |
| `program_id` | warning | The samples' `programId` is not the IDL's `address`. |
| `reference` | error, warning or note | Error: decoded values do not agree with the other decoder. The other side failed: an error for the decoder under test or the old IDL, a warning for the reference decoder. Note: a type was skipped, or the decoder under test skipped a sample kind that it does not decode. See [compare with another decoder](#compare-with-another-decoder). |
| `upgrade_impact` | error, warning or note | Only with `--upgrade-impact`. Error: an account breaks under the new IDL. Warning: an account needs a realloc before the new program can read it. Note: an account holds bytes after the data that the new IDL reads. See [upgrade impact](#upgrade-impact). |

### Unused bytes

When an account has more bytes than the IDL reads, idl-verify tries to explain them:

- **The size is the `repr(C)` size.** The IDL lays out as Borsh a type that the program
  casts as `repr(C)`, and drops its implicit padding. Fields after the first gap are
  misread. Error, with the location of the padding. Padding only after the last field
  moves no field, so when the bytes after the data are zero, the rules below apply to it.
  When they are not zero but the same bytes in each sample, the hint names a missing field.
- **The size is the used size rounded up to 8, on a Borsh type.** This is a common pattern
  of spare allocation, but not proof. Warning: compare with the program source.
- **Extra bytes that are all zero on a fixed-size Borsh type.** This is usually spare
  allocation from Anchor's `space`, but a missing field that is zero in each sample looks
  the same. Warning.
- **Any other size on a fixed-size type, or any extra bytes on a zero-copy type.** The IDL
  can be missing a field. Error.
- **A variable-size type** (strings, vecs, options) usually has an allocation at its maximum
  size, so trailing zeros are a note. Trailing bytes that are not zero are a warning: the
  IDL can be missing trailing fields.

### Zero-copy layout

Anchor `zero_copy` accounts (`serialization: bytemuck` or `bytemuckunsafe`) are cast from
account bytes, not Borsh-decoded. So idl-verify reads their fields at `repr(C)` offsets,
with SBF alignment rules:

- `u128` and `i128` align to 8. (On `x86_64` they align to 16 since Rust 1.77.)
- A pubkey is `[u8; 32]` and aligns to 1.
- `packed: true` aligns each field to 1. `align: N` increases the struct's alignment.
- A struct nested in a zero-copy type keeps its own `repr` and padding.
- A zero-copy type with no `repr` in the IDL is read as `repr(C)`. This is the default of
  Anchor's `#[zero_copy]`.
- `repr(Rust, packed)` is the default of `#[zero_copy(unsafe)]`, and Anchor writes it into
  the IDL. Its fields are read in IDL order with no padding. Rust does not promise this
  order, but rustc 1.85 to 1.97 keep it when no field holds a bool. They move a field that
  holds a bool, so such a type gets a `layout` warning: idl-verify reports it as not
  verifiable.
- `repr(Rust)` without `packed` does not fix the field order. A type with it gets the same
  `layout` warning.

`idl-drift inspect --layout` prints these layouts. See
[idl-drift.md](idl-drift.md#--layout).

## The samples file

```json
{
  "programId": "vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8",
  "accounts": [
    { "address": "HXtk15EA5pBg3rSKxBm8sWPExScPkTknSRp37fXNHgNA", "data": "0wjoKwKYdXc..." }
  ],
  "instructions": [
    { "signature": "5uXzA4qK...", "data": "9lI54oPe/fk...", "accounts": ["ST999VUT...", "4sycXz9X..."],
      "signer": [false, true], "writable": [true, false], "cpi": false, "position": [2, 0] }
  ],
  "events": [
    { "signature": "5uXzA4qK...", "source": "log", "data": "eKpmsOmLc5I..." }
  ]
}
```

| Key | Content |
|---|---|
| `accounts[].data` | The account's full data, base64, as `getAccountInfo` returns it with `"encoding": "base64"`. |
| `instructions[].data` | The data of one instruction, base64. `getTransaction` returns it in base58, so encode it again. |
| `instructions[].accounts` | The instruction's account keys, in order. Resolve them through the transaction's account keys and, for v0 transactions, its loaded addresses. |
| `instructions[].signer`, `instructions[].writable`, `instructions[].cpi` | Optional. One `signer` and one `writable` flag for each key in `accounts`. `signer` is true when the key signs the transaction. `writable` is true when the message header (or the writable list of loaded addresses) makes the key writable. `cpi` is true for an inner instruction. Give all three or none. Without them, the `account_flags` check skips the sample. |
| `instructions[].position` | Optional. `[top-level index, inner index]` in the transaction. The inner index is 0 for a top-level instruction, else the place of the CPI in that instruction's list, from 1. Live sampling writes it. [Fixture export](#fixture-export) names Carbon files with it. A value of another shape is ignored, with one warning. |
| `events[]` | Optional. What the program emitted. `"source": "log"` is a `Program data:` line, base64-decoded, from the program's own invoke frame. `"source": "cpi"` is the data of a self-CPI event instruction, with the envelope. |
| `programId`, `address`, `signature` | Optional, but they make the report useful. |

Include only instructions to the program. Inner (CPI) instructions count too. idl-verify
ignores other keys, such as a `type` label: it classifies samples by discriminator only.
`fixtures/live/*/samples.json` are real examples from mainnet.

## Live sampling

`--rpc <URL>` (or `IDL_VERIFY_RPC_URL`) samples the program over JSON-RPC. The RPC must
serve `getAccountInfo`, `getSignaturesForAddress`, `getTransaction` and
`getMultipleAccounts`, and with `--upgrade-impact` also `getMinimumBalanceForRentExemption`.

- **Last deploy.** For a loader-v3 or loader-v4 program, idl-verify reads the slot of the
  last deploy. A transaction in that slot or earlier, failed or not, ran the old code, so
  sampling stops there.

- **Signatures.** From `getSignaturesForAddress`, 1000 at a time, back through history.
  idl-verify skips failed transactions, because they can hold data that the program
  rejected. It also skips a transaction that the RPC no longer keeps (`null`).
- **Transactions.** From each transaction (`getTransaction`, versions 0 and 1):
  - each instruction that calls the program, top level and CPI, with keys resolved through
    address lookup tables, and the signer and writable flag of each key;
  - each self-CPI event, as a `cpi` event. A self-CPI event is an instruction to the program
    that starts with the IDL's envelope, else Anchor's event tag, as in Shipstern;
  - each `Program data:` log line written while the program is the innermost running frame,
    as a `log` event. A line from a program that it calls is not credited to it.
- **Limits.** At most 5 samples for each instruction discriminator, each unknown prefix and
  each event kind.
- **Stop.** Sampling stops when each IDL instruction has 5 samples, when `--stall`
  transactions in a row bring no new instruction or event kind (a `null` one counts too),
  when `--stall` pages of signatures in a row hold only failed transactions, after
  `--max-tx` successful transactions, at the last deploy, or when the program has no older
  transactions. The report gives the number of transactions read and the reason it stopped.
- **Accounts.** The accounts that those instructions pass and the program owns, from
  `getMultipleAccounts`, at most 5 for each account type. idl-verify first reads only the
  owner, the length and the first bytes of each account. Then it reads all the data of the
  accounts it keeps, in requests under the 32 MiB answer limit. So a large account that it
  does not keep, such as program code, is never downloaded. idl-verify keeps accounts with
  an unknown discriminator, so the `discriminator` check can report them.

A run exits 2 when it finds nothing of the program, the URL is empty, the program address
is not base58, or a request fails. So a typo does not look like a quiet program. idl-verify
tries each request up to 5 times after a rate limit (HTTP 429), a server error, a lost
connection or a cut-off answer. After a 429 it waits for the `Retry-After` time, up to 60 s,
else 10 s. Each try waits up to 10 s for the connection and up to 2 min for the whole
answer. A try that times out, also while it reads an error answer, is tried again only one
time. So a host that does not accept the connection fails the request after 20.5 s (2 x
10 s, and 0.5 s between the tries), and a request that times out fails after 240.5 s (2 x
2 min + 0.5 s). The longest wait for one request is 14 min: 5 tries of up to 2 min each,
and a 60 s `Retry-After` between them.
`idl-drift diff --onchain` uses the same rules. The public mainnet RPC reads old transactions
slowly: on a program with little traffic, a run of 40 transactions took 28 min. Use a lower
`--max-tx` or a private RPC there.

idl-verify never prints the URL, because providers put the API key in it.

A run that samples only a few instruction kinds still passes: what it checked is correct.
To fail such a run, set `--min-coverage` (see the `coverage` check). It works with
`--samples` too.

## Compare with another decoder

Byte counts cannot find a field read with the wrong type at the correct width, such as an
`i64` read as a `u64`. For that, idl-verify can compare each decoded value with another
decoder. There are three sources. Use one at a time. Field names compare without case and
without underscores.

### Reference decoder

`tools/reference` decodes the samples with `@coral-xyz/anchor`'s `BorshCoder`:

```bash
cd tools/reference && npm install
node reference.mjs path/to/idl.json path/to/samples.json > reference.json
idl-verify --idl path/to/idl.json --samples path/to/samples.json --reference reference.json
```

The reference must come from the same samples file. idl-verify refuses a reference whose
entries do not align with the samples.

`@coral-xyz/anchor` ignores `serialization` and `repr`. It reads zero-copy types as Borsh,
and misreads each field after implicit padding. So idl-verify skips types with padding (with
a note) and does not report them as mismatches. On `fixtures/live`, idl-verify and Anchor
agree on all 31 comparable accounts and all 40 instructions. The voltr_vault run gives:

```text
reference: 11 of 11 accounts and 20 of 20 instructions match (5 samples skipped)
note: account AdaptorAddReceipt: not compared with the reference decoder, which ignores the 7 bytes of repr(C) padding in zero-copy types
```

### Decoder under test

`--decoder "<command>"` runs your own decoder as a child process, such as a binary built
from Shipstern's `include_shipstern_parser!` or a Carbon decoder. A generated parser reads
zero-copy padding, so padded types are compared, not skipped. The protocol is in
[decoder-protocol.md](decoder-protocol.md).

### Compare two IDLs

`--compare-idl <old.json>` decodes the same samples with a second IDL of the program, such
as the legacy Anchor IDL before a move to 0.30, or a Codama conversion. It compares the
values as `--reference` does. A sample that the old IDL classifies by another discriminator,
or by none, is an error that names both. Events are compared too: each IDL removes its own
self-CPI envelope, and a self-CPI event without the old IDL's envelope is an error.

```bash
idl-verify --idl fixtures/live/voltr_vault/idl.js-codama.json \
           --samples fixtures/live/voltr_vault/samples.json \
           --compare-idl fixtures/live/voltr_vault/idl.json
```

```text
reference: 16 of 16 accounts and 20 of 20 instructions match, 25 of 25 events match
```

Equal values do not prove equal layouts. voltr's Codama IDL drops 7 bytes of padding in
`AdaptorAddReceipt`, but each captured sample holds zeros after the gap. The run above still
fails on the `bytes_used` error of the new IDL. Run idl-verify on the old IDL alone to see
its unused bytes.

## Fixture export

`--export-fixtures <DIR> --fixture-format <FORMAT>` writes the samples to DIR as the test
fixtures of a parser framework. Then the tests of a Shipstern parser or a Carbon decoder run
on real chain data with no network. The run still checks the samples, and the findings set
the exit code as before. The export itself exits 2 when it writes no file, when the program
id is not a base58 key, or when a file cannot be written. On stderr, idl-verify gives the
number of files and each sample that got no file, also when a write error stops the run.
It serializes each file straight to disk, so the export adds no copy of a file in memory.
Only a Carbon account file holds its base64 data in memory while it is written.

| Format | Files |
|---|---|
| `shipstern` | `<address>_account.json` for each account sample. With `--rpc`, also `<signature>_tx.json` for each transaction that gave a kept sample. The format of `shipstern-mock` 0.12, for `account_fixture!` and `tx_fixture!`. |
| `carbon` | `<address>_account.json` (`TestAccount`) for each account sample, and `<signature>_<outer>_<inner>_instruction.json` (`TestInstruction`) for each instruction sample. `<outer>` is the top-level index in the transaction. `<inner>` is 0 for a top-level instruction, else the place of the CPI in that instruction's list, from 1. The format of `carbon-test-utils` 2.0, for `read_account` and `read_instruction`. |

- **Fixed values.** A sample keeps no lamports, rent epoch or executable flag. Each account
  file gets lamports 0, rent epoch 0 and `executable: false`. The owner is the program
  (`--program`, else the samples' `programId`, else the IDL's `address`), because both
  frameworks skip an account of another owner.
- **Shipstern from `--samples`: account files only.** A tx file holds each call to the
  program in the transaction, with its position, its CPIs and the log. A samples file does
  not keep them, and the mock reads a file that exists and does not fetch it, so a part of a
  transaction would change what a test sees. A live run keeps each transaction that gave a
  kept sample.
- **Shipstern tx files are correct where the mock's own fetch is not.** The calls are in
  the order of `shipstern-mock` 0.12: the top-level calls first, then the CPIs. Each call
  gets the log lines of its top-level instruction. A log frame goes only to the
  instruction whose program opened it, because a precompile (for example Ed25519) writes no
  line. This is the rule of Shipstern after #377; the 0.12 mock gives frame `i` to
  instruction `i`, so after a precompile each call gets the lines of another instruction.
  The `inner` list of each call holds the CPIs that it makes, nested by stack height as the
  Shipstern runtime nests them. The 0.12 mock writes it empty, so an event that only
  `emit_cpi!` writes, with no `Program data:` line, is lost. An instruction with no stack
  height (older transactions) gets no CPIs. A self-CPI event is also a call of its own. A
  parser from `include_shipstern_parser!` returns `Filtered` for it, and with the
  `program-events` feature reads the event from the `inner` list of its caller and from the
  log lines.
- **Carbon instruction keys** get the `signer` and `writable` flags of the sample. A sample
  with no flags gets `false` for each key. Events get no file, because an event sample
  keeps no accounts.
- **Same bytes each time.** Shipstern files are compact JSON, as the mock writes them.
  Carbon files are pretty JSON. One name always holds the same bytes, so a second export
  into the same DIR never changes a file to another instruction. A samples file from an
  older idl-verify has no `position`, and a `position` of another shape is ignored with
  one warning. Such Carbon instruction files are named
  `<signature>_<digest>_instruction.json`, where `<digest>` is the first 16 hex digits of
  the SHA-256 of the file. A file of the same name is replaced. Other files in DIR stay.
- **Names.** Each file is named after a base58 key or signature, so a hostile samples file
  cannot write outside DIR. A sample whose address, signature or account key is not base58
  gets no file, with a warning. A transaction whose CPIs nest deeper than the runtime
  allows gets no file either.

Shipstern, from the pump_fun samples:

```bash
idl-verify --idl fixtures/live/pump_fun/idl.json \
           --samples fixtures/live/pump_fun/samples-2026-10-16.json \
           --export-fixtures my-parser/fixtures --fixture-format shipstern
```

```rust
// my-parser/tests/pump.rs. The mock reads ./fixtures, from the crate root.
// Dependencies: those of examples/decoders/shipstern, shipstern-mock, tokio "macros".
use shipstern_core::Parser;

shipstern_proc_macro::include_shipstern_parser!("idl.json");

#[tokio::test]
async fn a_real_bonding_curve_decodes() {
    // account_fixture! panics when the parser cannot decode the account.
    let _curve = shipstern_mock::account_fixture!(
        "E98AtWDa8whooJTw8nbbfz4oifvTcK7VxfaLb2uV2uEG",
        &pump::AccountParser
    );
}
```

The mock fetches a missing file from `RPC_ENDPOINT` (devnet by default) and writes it. In CI,
set `RPC_ENDPOINT=http://127.0.0.1:9`, so that a missing file fails the test.

Carbon, from the same samples:

```bash
idl-verify --idl fixtures/live/pump_fun/idl.json \
           --samples fixtures/live/pump_fun/samples-2026-10-16.json \
           --export-fixtures tests/fixtures --fixture-format carbon
```

```rust
use carbon_core::account::AccountDecoder;
use carbon_pumpfun_decoder::PumpfunDecoder;

#[test]
fn a_real_bonding_curve_decodes() {
    let path = "tests/fixtures/E98AtWDa8whooJTw8nbbfz4oifvTcK7VxfaLb2uV2uEG_account.json";
    let account = carbon_test_utils::read_account(path).unwrap();
    assert!(PumpfunDecoder.decode_account(&account).is_some());
}
```

With these samples, `carbon-pumpfun-decoder` 2.0.0 decodes all 12 accounts and 27 of the 41
instructions. The other 14 have the 3 discriminators that the `discriminator` check reports:
neither the IDL nor the decoder has them.

### One test per file

The [idl-verify-fixtures](../crates/idl-verify-fixtures/README.md) crate writes these tests
for you. Its `sample_tests!` macro reads the export directory at compile time and makes one
`#[test]` per file, named after the file. A sample that the parser cannot decode then fails
by name in `cargo test`:

```rust
// tests/samples.rs. Dev-dependencies: shipstern-core, shipstern-mock, idl-verify-fixtures.
idl_verify_fixtures::sample_tests! {
    shipstern,
    dir: "fixtures",
    accounts: my_parser::pump::AccountParser,
    instructions: my_parser::pump::InstructionParser,
}
```

```rust
// tests/samples.rs. Dev-dependencies: carbon-core, carbon-test-utils, serde_json,
// idl-verify-fixtures.
idl_verify_fixtures::sample_tests! {
    carbon,
    dir: "tests/fixtures",
    accounts: carbon_pumpfun_decoder::PumpfunDecoder,
    instructions: carbon_pumpfun_decoder::PumpfunDecoder,
}
```

- **Pass rule.** An account test passes when the parser returns `Ok` or the decoder returns
  `Some`. A Carbon instruction test passes on `Some`. A Shipstern tx test passes when no
  instruction gives an error other than `Filtered`, and at least one gives `Ok`.
- **No network.** Each test embeds its file. A Shipstern test reads it with `shipstern-mock`
  and never calls `load_fixture`, so the mock never fetches. A Carbon test reads it as a
  `carbon-test-utils` `TestAccount` or `TestInstruction`.
- **Pinned values.** With `expected: "<dir>"`, each test also compares the `{:#?}` output of
  the decoded value with `<dir>/<file stem>.txt`, from the run-time `CARGO_MANIFEST_DIR`.
  `IDL_VERIFY_FIXTURES_BLESS=1` writes the files.
- **New files need a build.** Each test embeds its file with `include_bytes!`, so a changed
  file builds the tests again. A proc macro cannot watch a directory on stable Rust: after a
  new export, touch the test file, or add a `build.rs` that prints
  `cargo:rerun-if-changed=fixtures`.

`examples/fixture-tests/` runs both forms on the pump_fun export.

## Upgrade impact

Before you deploy a program upgrade, `--upgrade-impact` shows how the new IDL reads the
program's accounts. Give the new IDL with `--idl`. idl-verify decodes each sampled
account with it and puts the account in one class:

| Class | Rule |
|---|---|
| ok | The account decodes, and the new IDL reads each byte. |
| shorter | The account decodes, and bytes are left over. The new type can be smaller, or the account has spare room. The account is not broken. |
| realloc | A read goes past the end of the data, and the account decodes when zero bytes are added at the end. A field or a fixed array of the new layout can start or end past the data. The realloc size is the smallest number of zero bytes that makes it decode. For a fixed-size or zero-copy type, this is the new size minus the account size. For a variable-size type, it is the bytes that the new trailing fields need. |
| broken | The discriminator matches no account type of the new IDL. Or the data does not decode, and zero bytes do not fix it, such as an invalid enum tag or bool. Or a length prefix that the new IDL reads from the existing bytes points past the data: a live account holds complete data, so the new IDL reads those bytes another way. (The prefix of a new trailing field comes from the zero bytes, so it is 0.) Or, with `--compare-idl`, a field that both IDLs name decodes to another value, or a new field takes bytes of an old reserved array that are not zero. |

Give the current IDL with `--compare-idl`. idl-verify then decodes each account with both
IDLs and compares the fields that both IDLs name, with the same comparison as
[compare two IDLs](#compare-two-idls). A field that only one IDL has is not a difference,
also in a struct inside an option, an enum variant, a tuple or a map.
Lists compare on the items that both IDLs read. Without `--compare-idl`, a field that
changes its type, or that moves, can show as ok, shorter or realloc.

With `--compare-idl`, idl-verify also checks the
[reserved bytes](idl-drift.md#reserved-bytes) that new fields take: the cases where
`idl-drift diff` reports `reserved_bytes_used`. idl-verify reads the reserved array in the
old decode of each account, from the old account type that matched its discriminator, so a
renamed account type is checked too. It finds the array also in a struct inside a field, a
list, an option, a map, a tuple, an enum variant or a generic argument. When a byte that a
new field takes is not zero, the account is broken, and the error names the field, the
bytes and the sample count:

```text
error: account Pool: breaks under the new IDL in 2 of 5 samples: `Pool.b` takes bytes 0..8 of the old `reserved`, and `reserved[0]` is 5, not 0
```

When idl-verify cannot find the reserved array in the old decode, the account is broken
too, because its bytes are not checked. The reserved array is not compared with the new,
shorter array of the same name, because the two hold other bytes. Bytes that stay reserved
can hold any value. Without
`--compare-idl`, idl-verify does not know which bytes were reserved, so it does not check
them.

The extra rent of an account that needs a realloc is the rent-exempt minimum of the new
size minus the minimum of the current size. A live run gets the minimums from the cluster
with `getMinimumBalanceForRentExemption`, one request for each size. With `--samples`,
idl-verify sends no request. It uses `(128 + size) * 5080` lamports, as Agave's
`Rent::minimum_balance` does with a rate of 5080 lamports per byte. 5080 is the mainnet
rate since the SIMD-0437 feature `set_lamports_per_byte_to_5080` activated at slot
446256000. A later SIMD-0437 step can lower it. A live run gets the exact current value
from the cluster. The output names the source of the rent.

A broken account is an `upgrade_impact` error, an account that needs a realloc is a
warning, and a shorter account is a note. So with the default `--fail-on error`, a run where
accounts only need a realloc exits 0. `--fail-on warning` fails it.

The new voltr IDL below adds a `u64` at the end of the zero-copy `Vault`, narrows
`StrategyInitReceipt.position_value` from `u64` to `u32`, and makes
`AdaptorAddReceipt._reserved` 8 bytes shorter:

```bash
idl-verify --idl voltr_new.json --upgrade-impact \
           --samples fixtures/live/voltr_vault/samples.json \
           --compare-idl fixtures/live/voltr_vault/idl.json
```

```text
upgrade impact under the new IDL (sample counts, not all accounts of the program):
  type                 sampled  ok  shorter  broken  realloc  bytes  rent each
  AdaptorAddReceipt          5   0        5       0        0      -  -
  Protocol                   1   1        0       0        0      -  -
  StrategyInitReceipt        5   2        0       3        0      -  -
  Vault                      5   0        0       0        5      8  40640
  total                     16   3        5       3        5
extra rent for the 5 accounts that need a realloc: 203200 lamports, estimated offline as (128 + size) * 5080 lamports
  5080 lamports per byte is the mainnet rate since the SIMD-0437 feature set_lamports_per_byte_to_5080. A later SIMD-0437 step can lower it. A live run (--rpc) gets the exact value from the cluster.

error: account StrategyInitReceipt: breaks under the new IDL in 3 of 5 samples: `position_value` reads 3670056934 under the new IDL and 415986917350 under the old IDL
warning: account Vault: needs a realloc of 8 bytes under the new IDL in 5 of 5 samples
note: account AdaptorAddReceipt: 8 bytes after the data that the new IDL reads, in 5 of 5 samples
```

The `u32` keeps its offset in the `repr(C)` layout, so without `--compare-idl` all 5
`StrategyInitReceipt` accounts are ok. The 2 ok accounts hold a value that fits in 32 bits.

The run checks accounts only. The instructions and events in the samples are for the
current program, so idl-verify does not check them against the new IDL, and the report's
`instructions` and `events` are empty. A run with no account data exits 2, and so does a
run with a Shank IDL, because idl-verify does not check Shank accounts. The classes
replace the `decode`, `bytes_used`, `size_distribution` and `reference` checks on the
accounts. The `discriminator`, `missing_type` and `layout` checks still run. When the new
IDL has another address, such as a new deployment, give the sampled program with
`--program`. The `program_id` warning then says that the two addresses differ.

Limits:

- **The counts are of the sample.** Live sampling takes the accounts that recent
  transactions pass, at most 5 of each account type. idl-verify does not count all the
  accounts of a type, because many RPCs refuse `getProgramAccounts` for a large program.
- **New bytes are zero.** idl-verify decodes a realloc with zero bytes at the end. If the
  new program writes other values when it reallocates, the classes do not show it.
- **An IDL with no account discriminators** matches accounts by size. An account of the old
  size then matches no type, and is broken.
- **A type that idl-verify cannot decode has no row.** It gets its usual `missing_type` or
  `layout` finding.

## Timeline

`idl-verify timeline` lists the deploys of a program and tells which of your IDL files
decodes the transactions of each slot range between two deploys. A backfill (for example
with Jetstreamer) then decodes each range with the IDL of the code that ran in it.

```text
idl-verify timeline --idl <PATH> [--idl <PATH>]... [--program <ID>] [--rpc <URL>]
                    [--per-range <N>] [--max-tx <N>] [--max-scan <N>] [--json]
```

- **Deploys.** The scan lists the signatures of the upgrade authority, which signs each
  deploy. It reads (at most `--max-scan`) only the ones that the `ProgramData` account lists
  too, because the authority also signs each buffer write. A deploy is a
  `DeployWithMaxDataLen` or `Upgrade` of this program, also as a CPI (Squads).
- **Other cases.** An extend or a set-authority keeps the code. After a set-authority to the
  listed key, the scan goes on with the key before it. When one page of the authority needs
  more than 5 pages of `ProgramData` signatures, the scan reads each authority transaction
  from then on.
  An immutable program: the scan lists `ProgramData`. Loader v4: a `Deploy` of the program
  account. Another loader cannot change the code: one range.
- **End of the scan.** At the deploy that made the program, the list is complete from
  there. A close and a new deploy at the same address (SIMD-0432) hides the life before it.
  The scan also stops when the RPC has no older signatures, after `--max-scan` transactions,
  or after 10 pages of 1000 signatures. Then the range from `?` can span several deploys,
  and the report says so. A public RPC can keep less history than the chain.
- **Ranges.** Agave runs new code from the slot after its deploy. So a range runs from the
  slot after one deploy to the slot of the next one. For each range, idl-verify reads the
  successful program transactions, newest first (`before` and `until` are the two deploy
  signatures), until `--per-range` of them call the program or log its events (default 10),
  or after `--max-tx` reads (default 40) or 10 pages.
- **Fit.** Each IDL decodes the samples of each transaction, as a check does. An IDL fits a
  transaction when the check finds no error. It fits a range when it fits every sampled
  transaction. A range with samples that no IDL fits is a finding. A range with no sample
  is not.

The table gives, for each range, the samples that each IDL decodes, and the IDLs that decode
all of them (`fits`). Under each range that no IDL fits, a line per IDL gives its first error
finding and the transaction, so you can tell an IDL to correct from the wrong IDL. stderr
gets one line when the scan ends and one per range. `--json` gives the same as one object:
`deploys`, then `ranges` with `first_slot`, `last_slot` (`null` for now),
`deploy_signature`, `samples`, `unreadable`, `best` (the IDLs that decode the most) and a
`results` entry per IDL (`fit`: `all`, `some`, `none` or `no_sample`; `first_error`), and
`deploy_scan` with the listed accounts and why the scan stopped (`first_deploy`,
`no_older_signatures`, `scan_limit`, `fixed_code`).

Exit codes: 0 when each range with samples has an IDL that decodes all of them, 1 when a
range has samples that no single IDL decodes, 2 when the run cannot run or no range has a
sample.

pump.fun on the public mainnet RPC on 2026-11-12, with the IDL of `fixtures/live` and the
older Shipstern test IDL:

```bash
idl-verify timeline --program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P \
  --idl fixtures/live/pump_fun/idl.json --idl fixtures/shipstern/pump_fun.json \
  --max-scan 4 --per-range 5
```

```text
program 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
3 deploys found in 4 transactions of 7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8
the scan stopped at --max-scan transactions before the first deploy, so the range from ? can span several deploys
IDL 1  fixtures/live/pump_fun/idl.json
IDL 2  fixtures/shipstern/pump_fun.json

from slot  to slot    samples  IDL 1  IDL 2  fits  deploy
?          456494698  5        1/5    0/5    none  -
456494699  458888140  5        1/5    0/5    none  2fAUGTBfLUViHGAq2hZKXHsvVXC6Lv1km3Pf6SLMrouyteWBSJifxwqoA7Srfs4seXh6c7kSsKuN7dmfJrRE9FFG
458888141  458921440  5        3/5    2/5    none  23sYjeCDLtM9MHe45mZZGeZuqh7fvzhGimND36T3uU7TT1D6xEXLt7QmwALNk91WVXTk9aTHAhzv5QNDe8m6qyez
458921441  now        5        2/5    0/5    none  5HtoG6baGa55zvKasGqp4yYLS6RDuKH17Vrz5QJPmQnwQ5xEfdfeTwLfVZHK15zUvXihBejEi2WwQXXos3YrfnDW

error: slots ? to 456494698: no IDL decodes all 5 samples; IDL 1 decodes the most, 1; the range can span several deploys
  IDL 1: discriminator instructions: 0x07051dc4f5176550 matches no instruction discriminator in the IDL, in 1 sample; transaction 4uGdzdByUaMzA2rtsGdG7MAZkorjorABQdoQpmwRHhfFznmEx54Z8wYTzG3t3YbFJrTy54McNH378UquWBfjZQ2w
  IDL 2: discriminator instructions: 0x07051dc4f5176550 matches no instruction discriminator in the IDL, in 1 sample; transaction 4uGdzdByUaMzA2rtsGdG7MAZkorjorABQdoQpmwRHhfFznmEx54Z8wYTzG3t3YbFJrTy54McNH378UquWBfjZQ2w
error: slots 456494699 to 458888140: no IDL decodes all 5 samples; IDL 1 decodes the most, 1
  IDL 1: bytes_used event TradeEvent: 24 bytes after the payload in 2 of 2 samples; transaction 5YoNGammBXRU3vbrmSU49Ho2vasWdfZSUY1vbT3NHMbtn9GUJkyVAxnt9L7W9cVy44Rqiee1CSt2Eh7jatQL3faW
  IDL 2: bytes_used event tradeEvent: 100 bytes after the payload in 2 of 2 samples; transaction 5YoNGammBXRU3vbrmSU49Ho2vasWdfZSUY1vbT3NHMbtn9GUJkyVAxnt9L7W9cVy44Rqiee1CSt2Eh7jatQL3faW
error: slots 458888141 to 458921440: no IDL decodes all 5 samples; IDL 1 decodes the most, 3
  IDL 1: bytes_used event TradeEvent: 32 bytes after the payload in 2 of 2 samples; transaction 5rgMsiqtT42zLa9oKAWqhBKqAyLF3CLvzStnjQTjRtANW8uJ87WwqRNpqSDTmDQ2CcEEhmuPYfnVomqGZJVQXV6G
  IDL 2: bytes_used instruction createV2: 1 byte after the arguments in 1 of 1 sample; transaction cf9DPygfQ1NyDcBc1SiqnwhthL5y2PM7LM676KnDpSM4yFpYEnrX2zDuBLEYejhaHgdqPDthiHpNxkHRBLdKF3g
error: slots 458921441 to now: no IDL decodes all 5 samples; IDL 1 decodes the most, 2
  IDL 1: bytes_used event TradeEvent: 32 bytes after the payload in 2 of 2 samples; transaction 25HfYCCTToo3TmqMC5hyapyv4eS9mFitu7g8FMcezE6uK24WpvE69hmwiXss94b9sfmXeL4ansohZyFNBnUqoqJX
  IDL 2: bytes_used instruction buy: 1 byte after the arguments in 1 of 1 sample; transaction 4cudtUDJNDEGRhn4L5qjzZuDvtg8Mo7poNsU7M2quT4szDndvNcpQgzvwWbQX1fsVycV8GkWYRW5iHVykiGYRd66
summary: 4 ranges; 4 with samples that no single IDL decodes
```

The 4 transactions read are the newest of the authority that the `ProgramData` account
lists too. The newest upgrade is in slot 458921440, the last write slot of the `ProgramData`
account. Neither IDL fits a range: the `fixtures/live` IDL is 24 to 32 bytes short on
`TradeEvent` and does not know an instruction of the oldest range, so other IDL versions are needed. With
`--max-scan 30`, an earlier build found 5 upgrades from slot 452654932 on.

## Events

A `log` event is `[discriminator][payload]`. A `cpi` event first loses its envelope: the
IDL's envelope, else Anchor's `EVENT_IX_TAG` (`e445a52e51cb9a1d`), because an Anchor IDL
cannot describe one. idl-verify classifies the rest by event discriminator and decodes it as
the `types` entry with the event's name. Sizes and offsets count from the discriminator.

Codama has no node for an envelope. idl-verify reads the envelope of a Codama IDL by a
Shipstern convention, not by Codama behavior: in an event whose discriminators are all
constants, the constant at offset 0 is the envelope tag, and the next one starts the event.
Codama reads each of these constants as a discriminator of the event.

`fixtures/live/pump_fun` shows a real case. pump.fun emits each event both as a log line and
as a self-CPI. Its `TradeEvent` holds 24 bytes that its on-chain IDL does not describe, in
each sample:

```text
error: event TradeEvent: 24 bytes after the payload in 10 of 10 samples
  hint: the IDL may be missing trailing fields, or declares one narrower than the program writes
```

## JSON output

`--json` prints the full report as one object.

| Key | Content |
|---|---|
| `program` | `name`, `version` and `address` from the IDL. |
| `samples_program_id` | The `programId` of the samples, or `null`. |
| `account_coverage`, `instruction_coverage`, `event_coverage` | `sampled`, `classified`, and `not_sampled` (the names with no sample). |
| `accounts` | One entry for each account type: `name`, `samples`, `layout`, `sizes` (a list of `size` and `count`), `bytes_used` (`min`, `max`), and `expected_size`, the size that the IDL gives a fixed-size type. Sizes include the discriminator. `bytes_used` is `null` when no sample decoded. `expected_size` is `null` for a variable-size type. |
| `instructions` | One entry for each instruction: `name`, `samples`, `idl_accounts`, `sample_accounts` (`min`, `max`), `data_sizes` (`min`, `max`). |
| `events` | One entry for each event: `name`, `samples`, `sources` (`log`, `cpi`), `sizes`, `bytes_used`. Sizes include the discriminator and do not include a self-CPI envelope. |
| `reference` | Only after a comparison: `accounts_compared`, `accounts_matched`, `instructions_compared`, `instructions_matched`, `events_compared`, `events_matched`, `skipped`. |
| `sampling` | Only after a live run: `transactions` (successful transactions read) and `stop_reason`. |
| `upgrade_impact` | Only with `--upgrade-impact`. `rent_source` (`rpc` or `formula`), `lamports_per_byte` (the offline rate, or `null` for `rpc`), `types`, `unknown` (accounts that the `discriminator` check reports as errors, counted as broken) and `totals`. Each entry of `types` has `name`, `sampled`, `ok`, `shorter` (`count`, `bytes`), `realloc` (`count`, `bytes`, `rent_each`, `rent_total`) and `broken` (`count`, and `reasons`, each with `reason`, `count`, `detail` and `example`). `bytes` and `rent_each` are `min` and `max`, or `null` when `count` is 0. `totals` has `sampled`, `ok`, `shorter`, `realloc`, `broken` and `rent_total`. The counts are of the sample. Rent is in lamports. In this mode, `instructions` and `events` are empty. |
| `findings` | Each finding: `severity`, `check`, `subject`, `message`, and when present `hint` and `example`. An allowed finding also has `"allowed": true`. |
| `unused_allows` | Only with `--allow` or `--write-allow`: the entries that match no finding, each with `line`, `rule` and `subject`. |
| `pruned_allows` | Only with `--prune-allow`: the entries that the run removed from the allow file, with the same keys. |
| `counts` | `errors`, `warnings`, `notes`, other than allowed findings, so that they agree with the exit code. |
| `exit_code` | The same value as the process exit code. |

When the run exits 2, stdout holds only `{"error": "<message>", "exit_code": 2}`. The
message also goes to stderr.

## Markdown output

`--markdown` prints GitHub Markdown for a job summary (`$GITHUB_STEP_SUMMARY`) or a pull
request comment: a heading with the IDL and the samples, the program, the number of findings
of each severity, the exit code, a table of the findings (severity, check, subject, message,
hint), and the allowed findings in a `<details>` block. With `--upgrade-impact`, the upgrade
impact table and the extra rent come before the findings. The [GitHub Action](ci.md) writes
it to the job summary.

IDL names and messages are escaped, so an IDL cannot add HTML, links or table rows. When the
run exits 2, stdout holds one line, `**idl-verify could not run (exit code 2):** <message>`.
The message also goes to stderr.

## Library

An example is in the [crate README](../crates/idl-verify/README.md). `idl_verify::decode` is
a runtime decoder over the IDL model. `idl_verify::rpc` takes samples from a cluster.
`idl_verify::layout` re-exports the `repr(C)` layout calculator from `idl_drift`.

## Limitations

- **Shank IDLs: instructions only.** A Shank IDL (`metadata.origin: "shank"`, for example
  from Metaplex or Phoenix) keeps each instruction tag in `discriminant`, and idl-verify
  reads it. A Shank program tells its account types apart by a field, such as a `key` enum,
  that the IDL does not map to each type. So idl-verify does not check Shank accounts, and
  gives one note. With `--upgrade-impact`, which checks accounts only, a Shank IDL exits 2.
  A Shank `vec` with a `size` does not load (exit 2).
- **Live sampling sees recent activity only.** idl-verify does not check instructions and
  events that the sampled transactions do not use. The report lists them under "not
  sampled". Increase `--max-tx` and `--stall` to look further back. Set `--min-coverage` to
  fail a run that samples too little.
- **The timeline samples a few transactions per range.** An IDL that fits each sample can
  still miss an instruction that the samples do not use, and the samples come from the end
  of each range. A deploy that the scan cannot read (`null`) or that a migrated program made
  under its old loader is not listed; the report counts the unread transactions.
- **A narrowed last field can hide.** When the bytes that a narrower type does not read are
  zero, such as the high half of a small timestamp, they look like spare room: a note, not
  an error.
- **A narrowed field in a variable-size account is only a warning.** It shows as bytes that
  are not zero after the decoded data. Bytes alone cannot show if a field is too narrow or
  if the program added fields after the IDL was written. A parser that ignores trailing
  bytes, as Shipstern's account parser does, gives wrong values with no error. Use
  `--fail-on warning` in CI, or `--compare-idl <old IDL>` to see the changed values.
- **Some swapped accounts look correct.** `account_flags` and `pda` find a swap only when a
  key lands where the IDL asks for a privilege that the key does not have, or where seeds
  derive another key. Two accounts with the same flags and no usable seeds can change
  places with no finding.
- **A field dropped before zero reserved bytes is only a warning.** Many accounts end in
  reserved bytes that are zero. If the IDL drops a field before them, the extra bytes are
  zero, so idl-verify reports spare allocation. Use `--fail-on warning` in CI, or
  `--reference` or `--decoder` to compare the values.
- **The reference decoder reads the same IDL.** A reference mismatch means that one of the
  two decoders misreads the bytes. It cannot prove that the IDL is correct.
- **Codama nodes that idl-drift cannot read.** idl-verify uses the IDL model of idl-drift.
  A field that idl-drift cannot resolve cannot be decoded, and some nodes are read in a
  simpler form. Both lists are in [idl-drift.md](idl-drift.md#inspect).
- **idl-verify follows the Codama spec where Shipstern does not.** A sample matches only if
  every discriminator matches, and the arguments start after all of them. Omitted arguments
  and fields stay in the layout. Shipstern 0.12 dispatches on the first discriminator and
  drops omitted arguments, so it reads Token-2022 extension arguments one byte early. A
  sample that idl-verify accepts can still be misread by a Shipstern parser. Use
  `--decoder` to compare with your parser.
- **Omitted optional accounts.** With `optionalAccountStrategy: "omitted"`, a sample that
  passes fewer accounts than the IDL lists leaves out absent optional accounts. idl-verify
  maps the keys to IDL positions only where the position is certain, and skips the other
  accounts in `fixed_address`, `account_flags` and `pda`.
- **A Codama IDL cannot fix an address.** Codama has only a default key, so a Codama IDL
  converted from Anchor loses each `address` constraint. Another key at that position is a
  note, not an error. Verify the Anchor IDL to get the error.
- **Codama spec 1.x only.** Codama spec 2.x, which is still in development, renames the
  `name` key of each node to `identifier`. idl-verify stops with "Codama spec 2.0.0 is not
  supported; only spec 1.x can be read" (exit 2).
- **No Quasar IDLs.** A Quasar IDL uses another schema: a type definition has a top-level
  `kind`, and a field can have a `codec`. An IDL with a top-level `spec`, for example
  `quasar-idl/1.0.0`, stops idl-verify with
  "IDL spec quasar-idl/1.0.0 is not supported; only Anchor, Shank and Codama IDLs can be read"
  (exit 2).
- **Bool decoding is Borsh-strict.** A bool byte other than 0 or 1 is a `decode` error, as
  in Shipstern, which decodes with Borsh. The Codama spec says that any value other than 1
  is false, so a Codama client can accept data that idl-verify rejects.
