# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Library: `idl_drift::source::SourceReport::nothing_to_compare` is `true` when no program
  was compared and there is no finding. `idl-drift source` stops with exit 2 on such a
  report, and a caller must not take it as a pass.

### Changed

- Library: `idl_drift::source::FindingKind`, `Leg` and `SourceError` are now
  `#[non_exhaustive]`, so a `match` on them needs a `_` arm.
- Library: `SourceReport::compared` is `true` only for a program with a valid `declare_id!`
  and one more valid id, or, with no valid `declare_id!`, a valid IDL address and a valid
  `Anchor.toml` id.
- `idl-drift source` lists the `Anchor.toml` ids by cluster, then program. It lists a
  `declare_id!` value that `lib.rs` repeats only one time.

### Fixed

- `idl-drift source`: a hostile `lib.rs` no longer stalls the `declare_id!` scan.
- `idl-drift source`: a `declare_id!` that rustc does not compile into `ID`, in a macro other
  than `cfg_if!` or in a module glob that another id shadows, no longer counts. A
  `use <module>::{ID, ..}` now counts.
- `idl-drift source` reads the crate root at the Cargo `[lib] path`, and the `programs` of
  `Anchor.toml` in each TOML form.
- `idl-drift source`: two `Anchor.toml` clusters alone no longer count as compared, so the
  run stops with exit 2.
- `idl-drift source`: an IDL file whose `address` is empty or not a string is Breaking
  `invalid_id`. It was Cosmetic.

## [0.2.0] - 2026-10-23

### Added

- `idl-drift source` checks that an Anchor workspace gives each program one id in
  `declare_id!`, `Anchor.toml` and `target/idl`, and with `--onchain`, on chain. The GitHub
  Action runs it with `command: source`.
- `idl-drift inspect` lists, on `SHIPSTERN:` lines, instructions that Shipstern cannot tell
  apart and account types that Shipstern skips. The exit code does not change.
- Library: `idl_drift::markdown::escape_control` writes each control character as an escape.
- Library: `idl_drift::source` holds the program-id check. `discover` reads a workspace,
  `read_idl` reads one more IDL, `check` compares the ids, and `check_onchain` checks the
  chain through an RPC call that the caller gives.
- Library: `idl_drift::shipstern` (`ambiguous_instructions`, `skipped_accounts`) lists what
  Shipstern cannot parse.

### Changed

- Library: `idl_drift::layout::FieldLayout` has a new `padding` field, and
  `idl_verify::decode::DecodeErrorKind` has a new `NaN` variant. Both types are now
  `#[non_exhaustive]`, so code that builds `FieldLayout` or matches every variant must change.
- Library: `idl_drift::layout::LayoutError` has a new `TooLarge` variant and is now
  `#[non_exhaustive]`. A layout call returns it when it needs more than
  `idl_drift::layout::MAX_WORK` (65536) type nodes.
- Library: `idl_drift::model::Instruction` has a new `payload_offset` field, as `NamedDisc`
  has. Code that builds an `Instruction` must set it.
- Library: `idl_drift::layout::StructLayout::padding_len` now counts the padding inside fields
  too.

### Fixed

- idl-drift: a field rename inside an enum variant payload is Dangerous (`field_renamed`), as
  in a struct. It was Breaking. A change from named fields to a tuple of the same types is
  also a rename.
- idl-drift: a struct field, a variant payload or an alias that links a struct now matches
  the same struct written inline. Codama inlines a type that has only one use, so this gave
  a false Breaking `field_changed`.
- idl-drift: a variant inserted before the end is `variant_inserted`, and a variant removed
  before the end is `variant_removed` at its own index. The variants after it are no longer
  reported as `variant_moved` and `variant_appended`. The first variant of an empty enum is
  `variant_appended`.
- A small IDL whose types each hold two of the next type no longer stops `diff` and
  `inspect` for minutes. A layout that needs more than 65536 type nodes is reported as
  unknown. An on-chain IDL can be hostile, so this matters for `diff --onchain`.
- idl-drift: each account discriminator check runs on its own. An allowed
  `account_discriminator_changed` no longer hides an `account_discriminator_offset_changed`.
- A later Codama discriminator on a field that is not omitted no longer moves the start of
  an account body or of the instruction arguments. That field is part of the body, so
  idl-verify read each later field one byte late. Shipstern also starts after the first
  discriminator.
- idl-verify: an instruction matches only when its Codama size discriminator matches too, at
  any position. A program that takes one tag at two data lengths no longer gives a false
  "bytes after the arguments" error.
- A Codama discriminator after a size discriminator now counts in both tools.
- idl-verify: an instruction with only a Codama size discriminator matches by data length.
- idl-drift: a change from a number to a bytes account discriminator is Breaking.
- idl-drift: generic params match by position. A reorder is Breaking, and a rename is no
  change.
- idl-drift: a renamed enum variant with a new tag is Breaking.
- idl-drift: an account rename no longer hides a flag change.
- idl-drift: `Vec<u8>` and `bytes` are the same type.
- idl-drift: args that nest the same fields in another way are no change.
- idl-drift: a layout change on a type that only the new IDL reaches is Cosmetic.
- idl-drift: a legacy Anchor field or seed named `publicKey` keeps its name.
- idl-verify: value compares find a wrong enum variant with capitalized names.
- idl-verify: a NaN in a Borsh type is a `decode` error.
- idl-verify: padding inside a nested zero-copy struct counts.
- idl-verify: Anchor's IDL instruction and `IdlAccount` give a note, not an error.
- idl-verify: `--decoder` accepts the `null` that serde writes for NaN.
- idl-verify: a decoder that keeps running after its last reply is stopped.
- idl-verify: live sampling reads all the data of only the accounts that it keeps.
- RPC: an answer over 32 MiB is not retried, and `"error": null` is not an error.
- RPC: an answer that is not a JSON object is an error, not a panic.
- Control characters in findings, changes, IDL names and errors are escaped on the terminal.
  The GitHub Action writes them as `?` in the job log.

## [0.1.0] - 2026-10-16

The first release of `idl-drift` and `idl-verify`.

### Added

#### idl-drift

- `idl-drift diff OLD NEW` puts each change in one of four tiers: Breaking, Dangerous,
  Additive or Cosmetic. It reads Anchor (0.30 spec and legacy) and Codama `rootNode` IDLs,
  and the two files can use different formats.
- Layout changes count only on types that a parser reads. Zero-copy `serialization` and
  `repr` changes are judged by field offsets. Discriminator collisions, Codama size
  discriminators included, are judged by account count, as Shipstern dispatches them.
- Every Codama discriminator counts, as the spec says, and omitted arguments and fields
  stay in the layout. A change to a later discriminator is Breaking. Codama spec 2.x files
  stop with a clear message.
- Generic types and Codama inline structs compare as the same bytes across formats. A
  change inside a Codama node that idl-drift cannot read is reported, not hidden.
- `--fail-on <tier>`, and `--json` with a stable rule name (`kind`) and `subject` for each
  change.
- `idl-drift inspect IDL` lists unresolved fields, types that the IDL uses but does not
  define, and padded zero-copy types that Borsh also reads. `--layout` prints each zero-copy
  type field by field. A generic parameter that its type declares is not unresolved.
- Shank IDLs: instruction tags come from `discriminant`. Shank maps and sets load.
- Const generic values are read as Codama's Anchor converter and Shipstern read them (JS
  `parseInt`), so `" +0X20zz"` is 32.
- `type_undefined` (Breaking): the new IDL uses a type that it does not define.
- An account that leaves `accounts` while its type stays in `types` is Dangerous, not
  Breaking. Anchor 1.x lists only the accounts that the program owns.
- `idl-drift diff --onchain PROGRAM NEW` uses the IDL that the program publishes on chain
  (Program Metadata or Anchor IDL account) as the old IDL.
- A removed or renumbered error, or an error code with a new name, is Dangerous.
- `repr(Rust, packed)` zero-copy types (Anchor `zero_copy(unsafe)`) have a layout when no
  field holds a bool. An Anchor IDL whose `metadata.spec` is not 0.1.0 stops with exit 2.

#### idl-verify

- `idl-verify --idl IDL --samples SAMPLES` checks an IDL against captured account,
  instruction and event data.
- Live sampling with `--rpc` or `IDL_VERIFY_RPC_URL`, controlled by `--program`, `--max-tx`
  and `--stall`. `--save-samples` saves a live run to run again offline.
- Live sampling stops at the program's last deploy, skips a transaction that the RPC no
  longer keeps, and downloads only the accounts that the program owns.
- Shank accounts give one note, not errors: a Shank IDL does not map its accounts to types.
- An account type that only its size picked, with no discriminator, gives a note: a parser
  that selects accounts only by a discriminator, such as Shipstern's, cannot decode it.
- Value comparison accepts a public key as its 32 bytes, as serde writes a `Pubkey` (Carbon).
- `--upgrade-impact` puts each sampled account in a class under a new IDL: ok, shorter,
  needs a realloc (with the extra rent) or broken. The `upgrade_impact` check reports them.
- Checks for discriminators, missing types, decode failures, unused bytes, zero-copy
  layouts, size distribution, account counts, fixed addresses, account flags, PDA seeds,
  event envelopes, program id and coverage, with hints for unused bytes. All-zero extra
  bytes on a fixed-size Borsh account are a warning, not an error. On a zero-copy
  account, unused bytes stay an error, also at a size rounded up to 8.
- Codama instructions with `optionalAccountStrategy: "omitted"` map the passed accounts to
  IDL positions, so a left-out optional account gives no false finding.
- A Codama default public key is not a constraint. Another key there is a note. An Anchor
  `address` stays an error. PDA seeds that read a field of a struct argument are checked.
- Value comparison with `--reference` (`@coral-xyz/anchor`), `--decoder` (a decoder under
  test) or `--compare-idl` (a second IDL).
- Decoder examples for Shipstern 0.12 and Carbon 2.0 in `examples/decoders/`, with a CI job
  that runs them each week.
- `--min-coverage`, `--fail-on warning` and `--json`.

#### Both tools

- RPC requests are tried again after a lost connection or a cut-off answer. After a 429
  they wait for `Retry-After`, else 10 s.
- One exit code contract: 0 when clean, 1 on a finding at or above `--fail-on`, 2 when the
  tool cannot run. With `--json`, an exit 2 prints `{"error": "...", "exit_code": 2}`.
- GitHub Actions examples: `idl-drift` on pull requests and a nightly `idl-verify` run.
- `--allow <FILE>`: an allow file of `<rule> <subject>` lines. An allowed change or finding
  does not set exit code 1, and stays in the report as allowed. An entry that matches
  nothing gives a warning. A line that cannot be read stops the run with exit 2.
- `--markdown`: a GitHub Markdown report for a job summary or a pull request comment. IDL
  text in it is escaped.
- A GitHub Action (`action.yml`) that installs the tool and runs `diff` or `verify` in one
  step, with the Markdown report in the job summary. The example workflows use it.
- A release workflow that builds both binaries for Linux and macOS, on x86_64 and arm64,
  and publishes them with a sha256 file for each archive.
- The minimum supported Rust version is 1.91, and CI checks it.
