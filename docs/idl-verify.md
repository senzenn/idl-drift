# idl-verify reference

`idl-verify` checks a Solana program IDL against real account, instruction and event data.
It finds an IDL that a parser decodes with no error, but from the wrong bytes. It classifies
each sample by discriminator, decodes it from the IDL alone (no generated code), and reports
the data that does not fit.

## Usage

```text
idl-verify [OPTIONS] --idl <PATH>
```

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
| `--stall <N>` | With `--rpc`: stop after N successful transactions in a row bring no new instruction or event kind. Default: 40. It stops a run only when `--max-tx` is larger than N, so with the defaults `--max-tx` stops first. |
| `--save-samples <PATH>` | With `--rpc`: write the live samples to this file, in the samples format. |
| `--reference <PATH>` | Compare values with the output of the reference decoder. See [reference decoder](#reference-decoder). |
| `--decoder <CMD>` | Compare values with a decoder under test. See [decoder-protocol.md](decoder-protocol.md). |
| `--compare-idl <PATH>` | Compare values with a second IDL of the same program. See [compare two IDLs](#compare-two-idls). |
| `--upgrade-impact` | Read `--idl` as the IDL of a program upgrade, and put each sampled account in a class: ok, shorter, needs a realloc or broken. See [upgrade impact](#upgrade-impact). |
| `--min-coverage <PERCENT>` | Fail when less than this percent of the IDL's account types, instructions or events have a sample. |
| `--fail-on <SEVERITY>` | The lowest severity that sets exit code 1: `error` (default) or `warning`. |
| `--allow <FILE>` | Findings that you accept. They do not set exit code 1. See [Allow file](#allow-file). |
| `--json` | Print the report as JSON. See [JSON output](#json-output). |
| `--markdown` | Print the report as GitHub Markdown. See [Markdown output](#markdown-output). |

Rules for the options:

- Give `--samples` or `--rpc` (or set `IDL_VERIFY_RPC_URL`). When both are set, `--samples`
  wins, and idl-verify does not sample the cluster. `--max-tx` or `--stall` with `--samples`
  is a usage error (exit 2). Their default values do not count.
- `--save-samples` saves only live samples. With `--samples` it is a usage error (exit 2).
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

## Checks

Each finding names its check, a subject (such as `account Vault` or `instruction deposit`), a
message, usually a hint, and one affected address or signature that you can look up on an
explorer.

| Check (`--json` name) | Severity | What it means |
|---|---|---|
| `discriminator` | error or note | A sample's discriminator matches nothing in the IDL. The program uses an account type, instruction or event that the IDL does not have, or the IDL has no discriminators. Event samples against an IDL with no `events` give one error. For legacy Anchor IDLs, idl-verify calculates the discriminators from the names, as Anchor does. An instruction with a Codama size discriminator matches only data of that length, at any position of the discriminator list. An account type with no discriminator matches by exact data length: the IDL's `size` (or Codama size discriminator), else the type's fixed size. Note: an account type picked only by size, with no discriminator and no Codama size discriminator. A parser that selects accounts only by a discriminator, such as Shipstern's, cannot decode it. Note: an account that the program owns and that holds no data. It has no discriminator, so no IDL can describe it, and it is not checked. Note: with an Anchor IDL, Anchor's IDL instruction (`0x40f4bc78a7e9690a`) and `IdlAccount` (`0x184662bf3a907b9e`). Each Anchor program built without the `no-idl` feature has them, and an Anchor IDL lists neither, so they are not checked. |
| `missing_type` | error | An account has no entry in `types`, so it cannot be decoded. |
| `decode` | error | The data does not fit the IDL: it is too short, or it has an invalid bool, option tag, enum tag or UTF-8 string. A NaN float is an error in a Borsh type, because Borsh refuses it, but not in a zero-copy type. A vec, set or map of a type with no data (such as an empty struct) is also an error, because Borsh refuses it. The message names the field and offset. When each failing account is shorter than the layout, the hint says so: these accounts can be older than an upgrade. |
| `bytes_used` | error, warning or note | The IDL reads fewer bytes than the data holds. See [unused bytes](#unused-bytes). For instructions, any byte after the arguments is an error: instruction data has no spare room, so the IDL is missing an argument or declares one too narrow. For events, bytes after the payload are an error when a byte is not zero, else a note. When each sample has the same trailing bytes, the hint names an N-byte missing field, and an `Option` shape when it fits. |
| `envelope` | error | A self-CPI event sample does not start with the expected envelope: the IDL's envelope, else Anchor's event tag. |
| `layout` | error or warning | Error: a `bytemuck` (Pod) type whose calculated layout has padding, also inside a nested struct. Pod forbids padding, so the IDL's `repr` or fields are wrong (usually a missing `packed: true`). Warning: a zero-copy type whose layout the IDL does not fix (an option, vec, enum or generic inside it, `repr(Rust)` without `packed`, or `repr(Rust, packed)` with a bool inside it). idl-verify reports it as not verifiable, and does not guess. |
| `size_distribution` | note | A fixed-size account type has more than one size, such as accounts created before an upgrade. |
| `coverage` | error | Only with `--min-coverage`: less than the given percent of the IDL's account types, instructions or events have a sample. One finding for each kind that is short. |
| `account_count` | error or note | Error: samples pass fewer accounts than the IDL lists. The message names the missing accounts and their fixed addresses. A parser that checks the account count rejects each such instruction. Note: only optional accounts are missing at the tail, or a Codama instruction with `optionalAccountStrategy: "omitted"` leaves out absent optional accounts. Note: samples pass more accounts than the IDL lists (remaining accounts). |
| `fixed_address` | error, warning or note | An account that the IDL gives an address (a program or sysvar) holds another key. Error: an Anchor `address` holds another key, so the IDL's account order does not match the program. A Codama `publicKeyValueNode` default fills the account only when the caller gives no key, so the program can accept another key. Note: another key is at the position of a Codama default. Warning: the default key is at another position, so the accounts likely moved. An absent optional account passed as the program id is accepted. |
| `account_flags` | error | An account that the IDL marks `writable` gets a key that the transaction makes read-only, or an account that the IDL marks `signer` gets a key that does not sign the transaction. The IDL's account order probably does not match the program, or an earlier account is missing. Only samples with flags are checked (see [the samples file](#the-samples-file)). A key with more privileges than the IDL asks for is accepted, because the transaction flags cover each instruction in it. The signer check uses only top-level instructions, because a CPI can sign for a PDA. A Codama `isSigner: "either"` account and an absent optional account are accepted. |
| `pda` | error or note | An account that the IDL derives from seeds (Anchor `pda`, Codama `pdaValueNode` default) gets a key that its seeds do not give. Error: the derived address is passed at another position in the same instruction, so the IDL's account order does not match the program, or an earlier account is missing. Note: the derived address is not in the instruction, so the seeds may read data that the IDL does not show. A sample is skipped when a seed cannot be resolved: a path into account data (`pool.mint`) or an absent optional account. A seed path into a struct argument (`params.id`) reads the decoded argument. When the IDL names a program for the seeds, the sampled program id is also tried, because the IDL can come from another deployment. |
| `program_id` | warning | The samples' `programId` is not the IDL's `address`. |
| `reference` | error, warning or note | Error: decoded values do not agree with the other decoder. The other side failed: an error for the decoder under test or the old IDL, a warning for the reference decoder. Note: a type was skipped, or the decoder under test skipped a sample kind that it does not decode. See [compare with another decoder](#compare-with-another-decoder). |
| `upgrade_impact` | error, warning or note | Only with `--upgrade-impact`. Error: an account breaks under the new IDL. Warning: an account needs a realloc before the new program can read it. Note: an account holds bytes after the data that the new IDL reads. See [upgrade impact](#upgrade-impact). |

### Unused bytes

When an account has more bytes than the IDL reads, idl-verify tries to explain them:

- **The size is the `repr(C)` size.** The IDL lays out as Borsh a type that the program
  casts as `repr(C)`, and drops its implicit padding. Fields after the first gap are
  misread. Error, with the location of the padding.
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
      "signer": [false, true], "writable": [true, false], "cpi": false }
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
  last deploy. A transaction in that slot or earlier ran the old code, so sampling stops
  there.

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
  transactions in a row bring no new instruction or event kind, after `--max-tx` successful
  transactions, at the last deploy, or when the program has no older transactions. The
  report gives the number of transactions read and the reason it stopped.
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
else 10 s. `idl-drift diff --onchain` uses the same rules.

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

## Upgrade impact

Before you deploy a program upgrade, `--upgrade-impact` shows how the new IDL reads the
program's accounts. Give the new IDL with `--idl`. idl-verify decodes each sampled
account with it and puts the account in one class:

| Class | Rule |
|---|---|
| ok | The account decodes, and the new IDL reads each byte. |
| shorter | The account decodes, and bytes are left over. The new type can be smaller, or the account has spare room. The account is not broken. |
| realloc | A read goes past the end of the data, and the account decodes when zero bytes are added at the end. A field or a fixed array of the new layout can start or end past the data. The realloc size is the smallest number of zero bytes that makes it decode. For a fixed-size or zero-copy type, this is the new size minus the account size. For a variable-size type, it is the bytes that the new trailing fields need. |
| broken | The discriminator matches no account type of the new IDL. Or the data does not decode, and zero bytes do not fix it, such as an invalid enum tag or bool. Or a length prefix that the new IDL reads from the existing bytes points past the data: a live account holds complete data, so the new IDL reads those bytes another way. (The prefix of a new trailing field comes from the zero bytes, so it is 0.) Or, with `--compare-idl`, a field that both IDLs name decodes to another value. |

Give the current IDL with `--compare-idl`. idl-verify then decodes each account with both
IDLs and compares the fields that both IDLs name, with the same comparison as
[compare two IDLs](#compare-two-idls). A field that only one IDL has is not a difference.
Lists compare on the items that both IDLs read, so a reserved array that gives bytes to a
new field is not a difference when the values stay the same. Without `--compare-idl`, a
field that changes its type, or that moves, can show as ok, shorter or realloc.

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
`instructions` and `events` are empty. A run with no account data exits 2. The classes
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
| `upgrade_impact` | Only with `--upgrade-impact`. `rent_source` (`rpc` or `formula`), `lamports_per_byte` (the offline rate, or `null` for `rpc`), `types`, `unknown` (accounts whose discriminator matches no account type, counted as broken) and `totals`. Each entry of `types` has `name`, `sampled`, `ok`, `shorter` (`count`, `bytes`), `realloc` (`count`, `bytes`, `rent_each`, `rent_total`) and `broken` (`count`, and `reasons`, each with `reason`, `count`, `detail` and `example`). `bytes` and `rent_each` are `min` and `max`, or `null` when `count` is 0. `totals` has `sampled`, `ok`, `shorter`, `realloc`, `broken` and `rent_total`. The counts are of the sample. Rent is in lamports. In this mode, `instructions` and `events` are empty. |
| `findings` | Each finding: `severity`, `check`, `subject`, `message`, and when present `hint` and `example`. An allowed finding also has `"allowed": true`. |
| `unused_allows` | Only with `--allow`: the entries that match no finding, each with `line`, `rule` and `subject`. |
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
  gives one note. A Shank `vec` with a `size` does not load (exit 2).
- **Live sampling sees recent activity only.** idl-verify does not check instructions and
  events that the sampled transactions do not use. The report lists them under "not
  sampled". Increase `--max-tx` and `--stall` to look further back. Set `--min-coverage` to
  fail a run that samples too little.
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
