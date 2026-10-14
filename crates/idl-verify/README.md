# idl-verify

Checks a Solana program IDL against real account and instruction data.

`idl-drift diff` catches an IDL that changed between two versions. `idl-verify` catches an
IDL that is wrong today: one that a parser decodes without any error, reading the wrong
bytes. It classifies every sample by discriminator, decodes it from the IDL alone (no
generated code), and reports what does not fit.

The bug it was built for: `npx codama convert` drops the implicit `repr(C)` padding of
Anchor `zero_copy(unsafe)` types. A parser generated from the converted IDL reads 145 of
the 152 bytes of every voltr_vault `AdaptorAddReceipt`, and every field after the gap
comes out shifted. Nothing errors.

## Install

```bash
cargo install --path crates/idl-verify
```

Or run it from the repo with `cargo run -p idl-verify -- <args>`.

## Usage

```bash
idl-verify --idl idl.json --samples samples.json
idl-verify --idl idl.json --samples samples.json --json
idl-verify --idl idl.json --samples samples.json --reference reference.json
IDL_VERIFY_RPC_URL=<url> idl-verify --idl idl.json
IDL_VERIFY_RPC_URL=<url> idl-verify --idl idl.json --program <program id>
```

`--idl` takes an Anchor IDL (0.30+ spec, with discriminators) or a Codama `rootNode` file.
`--samples` is captured data, described [below](#the-samples-file). `--rpc` or
`IDL_VERIFY_RPC_URL` takes samples from a live cluster instead, described in
[live sampling](#live-sampling). Prefer the env var: `ps` and shell history show a URL given
on the command line. `--samples` wins when both are set.

Run on the Codama conversion of voltr_vault, from the repo root:

```bash
idl-verify --idl fixtures/live/voltr_vault/idl.js-codama.json \
           --samples fixtures/live/voltr_vault/samples.json
```

```
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

not sampled: 2 of 6 account types, 25 of 28 instructions

error: account adaptorAddReceipt: 7 of 152 bytes unused in 5 of 5 samples
  hint: 152 bytes is exactly the repr(C) layout (8 + 144), which pads 7 bytes before
  `lastUpdatedEpoch`. The IDL lays this type out as Borsh, without the padding. A decoder
  built from it reads `lastUpdatedEpoch` and every later field at the wrong offset. Keep
  the Anchor IDL's `serialization` and `repr`, or add the padding as explicit fields.
  example: AsfkxMdVYjMnr2fdTBMUXhq81hgi2hbENXCy9WhUQF7u
note: instruction depositStrategy: up to 11 accounts passed beyond the 14 the IDL lists
  (remaining accounts), in 13 of 13 samples
...
summary: 1 error, 0 warnings, 2 notes
```

The same samples against the original Anchor `idl.json` come back clean: its
`serialization: bytemuckunsafe` and `repr` let idl-verify lay the type out as the program
does.

In the tables, `size` is the observed data length and `used` is how many of those bytes
the IDL accounts for, both including the discriminator. For instructions, `accounts` is
how many accounts the samples pass and `idl accounts` how many the IDL lists.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | No errors. Warnings and notes may be present. |
| `1` | At least one error. |
| `2` | A file could not be read or parsed. |

The same contract as `idl-drift diff`, so CI can gate on it:

```yaml
- run: idl-verify --idl idl/program.json --samples tests/samples.json
```

## Checks

Every finding names its check, a subject (`account Vault`, `instruction deposit`), a
message, usually a hint, and one affected address or signature you can look up on an
explorer.

| Check (`--json` name) | Severity | What it means |
|---|---|---|
| `discriminator` | error | A sample's discriminator matches nothing in the IDL: the program uses an account type or instruction the IDL lacks, or the IDL has no discriminators at all. For legacy (pre-0.30) Anchor IDLs, which leave them out, they are computed from the names the way Anchor does. An account type with no discriminator is matched by exact data length: the IDL's `size` (or Codama size discriminator), else the type's fixed size. |
| `missing_type` | error | An account has no entry in `types`, so it cannot be decoded. |
| `decode` | error | The data does not fit the IDL: too short, an invalid bool, option tag, enum tag or UTF-8 string. The message names the field and offset. When every failing account is shorter than the layout, the hint says so: those accounts may predate an upgrade. |
| `bytes_used` | error, warning or note | The IDL accounts for fewer bytes than the data holds. See [unused bytes](#unused-bytes). For instructions, any trailing byte is an error: instruction data has no slack, so the IDL is missing or narrowing an argument. |
| `layout` | error or warning | Error: a `bytemuck` (Pod) type whose computed layout has padding, which Pod forbids, so the IDL's `repr` or fields are wrong (usually a missing `packed: true`). Warning: a zero-copy type whose layout the IDL does not pin down (an option, vec, enum, generic or `repr(Rust)` inside it); it is reported as unverifiable instead of guessed. |
| `size_distribution` | note | A fixed-size account type comes in several sizes, such as accounts created before an upgrade. |
| `account_count` | error, warning or note | Error: samples pass fewer accounts than the IDL lists, naming the missing accounts and their fixed addresses. A parser that checks the account count rejects every such instruction. Warning: only optional accounts are missing. Note: samples pass more accounts than the IDL lists (remaining accounts). |
| `fixed_address` | error | An account the IDL pins to an address (a program or sysvar) holds another key. The IDL's account order does not match the program. An absent optional account passed as the program id is accepted. |
| `program_id` | warning | The samples' `programId` is not the IDL's `address`. |
| `reference` | error, warning or note | Decoded values disagree with the reference decoder (error), the reference failed (warning), or a type was skipped (note). See [reference comparison](#reference-comparison). |

### Unused bytes

When an account has more bytes than the IDL uses, idl-verify tries to explain them:

- **The size matches the `repr(C)` layout.** The IDL lays out as Borsh a type the program
  casts as `repr(C)`, dropping its implicit padding. Fields after the first gap are
  misread. Error, with the padding's location.
- **The size is the used size rounded up to 8.** Likely spare allocation. Warning: confirm
  against the program source.
- **Anything else on a fixed-size type.** The IDL may be missing a field. Error.
- **A variable-size type** (strings, vecs, options) is normally allocated at its maximum
  size, so trailing zeros are a note. Non-zero trailing bytes are a warning: the IDL may be
  missing trailing fields.

### Zero-copy layout

Anchor `zero_copy` accounts (`serialization: bytemuck` or `bytemuckunsafe`) are cast from
account bytes, not Borsh-decoded, so idl-verify reads their fields at `repr(C)` offsets
with SBF alignment rules:

- `u128` and `i128` align to 8 (on `x86_64` they align to 16 since Rust 1.77);
- a pubkey is `[u8; 32]` and aligns to 1;
- `packed: true` aligns every field to 1; `align: N` raises the struct's alignment;
- a struct nested in a zero-copy type keeps its own `repr` and padding;
- a zero-copy type with no `repr` in the IDL is treated as `repr(C)`, which Anchor's
  `zero_copy` attribute applies.

## The samples file

```json
{
  "programId": "vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8",
  "accounts": [
    { "address": "HXtk15EA5pBg3rSKxBm8sWPExScPkTknSRp37fXNHgNA", "data": "0wjoKwKYdXc..." }
  ],
  "instructions": [
    { "signature": "5uXzA4qK...", "data": "9lI54oPe/fk...", "accounts": ["ST999VUT...", "4sycXz9X..."] }
  ]
}
```

- `accounts[].data`: the account's full data, base64, as `getAccountInfo` returns it with
  `"encoding": "base64"`.
- `instructions[].data`: one instruction's data, base64. `getTransaction` returns it in
  base58; re-encode it.
- `instructions[].accounts`: that instruction's account keys, in order, resolved through the
  transaction's account keys and, for v0 transactions, its loaded addresses.
- Include only instructions addressed to the program; inner (CPI) instructions count too.
- `programId`, `address` and `signature` are optional but make the report useful. Other
  keys, such as a `type` label, are ignored: samples are classified by discriminator only.

`fixtures/live/*/samples.json` are real examples, captured from mainnet.

## Live sampling

`--rpc <url>` samples the program (`--program`, or the IDL's `address`) over JSON-RPC:

- The 40 most recent signatures, from `getSignaturesForAddress`. Failed transactions are
  skipped, since they can carry data the program rejected.
- From each transaction (`getTransaction`, versions 0 and 1): every instruction that calls
  the program, top level and CPI, with keys resolved through address lookup tables. Anchor
  self-CPI events are skipped. At most 20 instructions are kept.
- The accounts those instructions pass that the program owns (`getMultipleAccounts`), at
  most 5 per account type. Accounts with an unknown discriminator are kept, so the
  `discriminator` check can report them.

A failed request exits 2: a run never passes on samples it could not fetch. Rate limits
and server errors are retried 5 times. The URL is never printed, since providers put the
API key in it.

## Reference comparison

Byte counts can't catch a field read with the wrong type at the right width, such as an
`i64` read as a `u64`. For that, idl-verify can compare every decoded value with an
independent decoder, `@coral-xyz/anchor`'s `BorshCoder`:

```bash
cd tools/reference && npm install
node reference.mjs path/to/idl.json path/to/samples.json > reference.json
idl-verify --idl path/to/idl.json --samples path/to/samples.json --reference reference.json
```

The reference must come from the same samples file; idl-verify refuses one whose entries
don't line up. Field names compare case-insensitively and without underscores.

`@coral-xyz/anchor` ignores `serialization` and `repr`, so it reads zero-copy types as
Borsh and misreads every field after implicit padding. Types with padding are skipped
(with a note) rather than reported as mismatches. On `fixtures/live`, idl-verify and anchor
agree on all 31 comparable accounts and all 40 instructions.

## Decoder under test

The same comparison runs against any decoder that speaks a one-line protocol, such as a
binary built from Shipstern's `include_shipstern_parser!` or a Carbon decoder:

```bash
idl-verify --idl idl.json --samples samples.json --decoder "./target/release/my-decoder spl_governance"
```

idl-verify writes one JSON line per sample to the child's stdin, accounts first:
`{"kind":"account","program":"<base58>","address":"...","data":"<base64>"}` then
`{"kind":"instruction","program":"<base58>","signature":"...","data":"<base64>","accounts":["<base58>"]}`.
The child answers one line per sample, in order: `{"status":"ok","value":...}` or
`{"status":"error","error":"..."}`. An instruction value is `{"name":"...","data":{...}}`.
Values may use serde's plain shapes: bare numbers, unit variants as names, tuples as arrays.
A generated parser reads zero-copy padding, so padded types are compared, not skipped.

## JSON output

`--json` prints the whole report: `program`, `samples_program_id`, `account_coverage` and
`instruction_coverage` (`sampled`, `classified`, `not_sampled`), per-type `accounts` and
`instructions` summaries, `reference` when compared, `findings`, `counts` (`errors`,
`warnings`, `notes`) and `exit_code`. Each finding has `severity`, `check`, `subject`,
`message`, and optionally `hint` and `example`.

## Library

```rust
use idl_drift::model::Idl;
use idl_verify::{verify, Samples};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let idl = Idl::from_json(&std::fs::read_to_string("idl.json")?)?;
    let samples = Samples::from_json(&std::fs::read_to_string("samples.json")?)?;
    let report = verify(&idl, &samples, None);
    for finding in &report.findings {
        println!("{:?} {}: {}", finding.severity, finding.subject, finding.message);
    }
    std::process::exit(i32::from(report.exit_code()));
}
```

`idl_verify::decode::Decoder` and `idl_verify::layout::TypeIndex` are public too: a runtime
decoder over the IDL model and the `repr(C)` layout calculator.

## Limitations

- **Live sampling sees recent activity only.** Account types and instructions that the last
  40 transactions don't touch are not sampled; the report lists them under "not sampled".
- **Events are not checked.** Neither `Program data:` logs nor self-CPI events.
- **Spare allocation is a heuristic.** "Rounded up to 8" is a common allocation pattern,
  not proof; it is a warning for that reason.
- **The reference decoder reads the same IDL.** A reference mismatch means one of the two
  decoders misreads the bytes; it cannot prove the IDL itself right.
