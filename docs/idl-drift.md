# idl-drift reference

`idl-drift` compares two versions of a Solana program IDL. It puts each change in a tier,
by its effect on a parser that was generated from the old IDL.

## Commands

```text
idl-drift diff [--json | --markdown] [--fail-on <TIER>] [--allow <FILE>] <OLD> <NEW>
idl-drift diff --onchain [--rpc <URL>] [--source <SOURCE>] [--json | --markdown] [--fail-on <TIER>] [--allow <FILE>] <PROGRAM> <NEW>
idl-drift inspect [--layout] <IDL>
```

| Option | Command | Meaning |
|---|---|---|
| `--json` | `diff` | Print the report as JSON. See [JSON output](#json-output). |
| `--markdown` | `diff` | Print the report as GitHub Markdown. It cannot be combined with `--json`. See [Markdown output](#markdown-output). |
| `--fail-on <TIER>` | `diff` | The lowest tier that sets exit code 1. Values: `breaking` (default), `dangerous`, `additive`, `cosmetic`. |
| `--allow <FILE>` | `diff` | Changes that you accept. They do not set exit code 1. See [Allow file](#allow-file). |
| `--onchain` | `diff` | Read the first argument as a program id, and use the IDL that the program publishes on chain as the old IDL. See [On-chain IDL](#on-chain-idl). |
| `--rpc <URL>` | `diff` | The JSON-RPC URL for `--onchain`. Use the `IDL_DRIFT_RPC_URL` environment variable in its place: `ps` and the shell history show a URL on the command line. |
| `--source <SOURCE>` | `diff` | With `--onchain`, the account to read: `metadata` (the Program Metadata program) or `anchor` (the Anchor IDL account). |
| `--layout` | `inspect` | Also print each zero-copy type field by field, with its gaps and both sizes. |

Colors are on only when stdout is a terminal. Set `NO_COLOR` to turn them off.

### On-chain IDL

`idl-drift diff --onchain <PROGRAM> <NEW>` compares the IDL that the program publishes on
chain with NEW. Use it before an upgrade, to see what changes for parsers that were built
from the deployed IDL.

```bash
export IDL_DRIFT_RPC_URL=https://api.mainnet-beta.solana.com
idl-drift diff --onchain 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P target/idl/pump.json
```

idl-drift reads two accounts:

- The canonical `idl` account of the Program Metadata program, which only the program's
  upgrade authority can write. The account must hold the IDL as UTF-8 JSON text, with no
  compression, gzip or zlib.
- The Anchor IDL account, which `anchor idl init` writes. The data is zlib.

If the program has both accounts, idl-drift stops (exit 2) and asks for `--source`, because
the two IDLs can differ. A legacy IDL with no address gets the program id as its address.

The report starts with the account that the IDL came from, for example
`on-chain IDL: Anchor IDL account <ACCOUNT>`. The JSON report adds
`"onchain": {"source": "anchor", "account": "<ACCOUNT>"}`.

A Program Metadata account that stores the IDL at a URL or in another account is not read.

## Input formats

Each file can be one of these formats. The two files do not have to use the same format.

- Anchor IDL, 0.30 spec (with `address`, `metadata` and discriminators). `metadata.spec`
  must be `0.1.0`, the only spec that Anchor reads. Another spec stops the run (exit 2).
- Legacy Anchor IDL (before 0.30). These IDLs have no discriminators. idl-drift calculates
  them from the names, as Anchor does: the first 8 bytes of
  `sha256("global:<snake_case name>")`, `sha256("account:<Name>")` or `sha256("event:<Name>")`.
- Codama `rootNode` JSON, the format that Shipstern's `include_shipstern_parser!` reads.

idl-drift also accepts three Anchor shapes that are not in the spec structs: tuple struct
fields written as bare types, the pre-0.30 `{"kind": "alias"}` type, and
`"serialization": {"custom": ...}`. A custom serialization has an unknown layout.

## How idl-drift compares two IDLs

A generated parser reads bytes by position and routes them by discriminator. idl-drift
compares the two IDLs the same way:

- Instructions, account types, events and defined types match by name.
- Inside them, items compare by position: instruction accounts (composite groups are
  flattened), instruction arguments, struct fields and enum variants. When an account name
  repeats, group members get their group path as a prefix: `source.owner` between two IDLs
  of one format, `sourceOwner` (as Codama writes it) across formats.
- A layout change counts only on a **reachable** type: an account type, an event type, a
  type that an instruction argument names, and each type that these reach through fields,
  variant payloads and aliases. A layout change on any other type is Cosmetic.

When the two files use different formats, idl-drift first does these steps:

- It compares names in camelCase, as Codama writes them. So `deposit_vault` and
  `depositVault` are the same instruction.
- It compares an instruction argument that holds a struct, linked or inline, field by
  field at each depth, and an argument that links an alias as the alias target. Codama
  inlines a type that has only one use, and Borsh writes a struct as its fields, so the
  bytes are the same.
- It writes each use of a generic struct or alias as its body with the arguments filled in,
  as Codama converters do.
- It compares a tuple struct and a tuple alias as the same type.

### Zero-copy types

`serialization` and `repr` changes follow Shipstern's converter. Only accounts are cast from
memory. Instruction arguments and events are always Borsh. idl-drift lays out each zero-copy
account two times, with the old and with the new `serialization` and `repr`, and compares
the field offsets. Thus changes that cancel out are Cosmetic, for example a converter that
removes `packed` from an account and from each type in it.

### Discriminator dispatch

A Codama IDL can list more than one discriminator, and the Codama spec says that all of
them must match. idl-drift compares each one: a change to a later discriminator is
`discriminator_changed` (`account_discriminator_changed` for an account type), and Breaking.
Shipstern dispatches on the first discriminator only, so the collision rules below use only
the first one. An argument or field with `defaultValueStrategy: "omitted"` stays in the
layout, because its bytes stay in the data. Only an omitted field that the discriminator
bytes at the start hold is left out.

Codama's `optionalAccountStrategy` is read but not compared, because Shipstern ignores it.

Two rules follow Shipstern's dispatch:

- Instructions that share a discriminator are told apart by account count, most accounts
  first.
- A self-CPI event envelope that changes length moves the start of the payload. An example
  is a port from Anchor's 8-byte event tag to the 1-byte tag that many Pinocchio programs use.

## Tiers

| Tier | Meaning | Default effect |
|---|---|---|
| **Breaking** | The old parser misreads bytes or cannot route them. | Exit code 1. |
| **Dangerous** | Existing positions and tags stay the same, but the parser must grow, or generated names change. Review the change. | Exit code 0, or 1 with `--fail-on dangerous`. |
| **Additive** | New surface. Old parsers are not affected. | Exit code 0. |
| **Cosmetic** | No byte that a parser reads changes. | Exit code 0. |

### Breaking

- The program address changed. The parser drops each update from the new address.
- An instruction was removed.
- Two instructions have the same name, or names that differ only by ASCII case. Shipstern
  generates one helper for both.
- A new instruction uses the discriminator or the size discriminator of an existing
  instruction. Old parsers route it to the existing instruction.
- An instruction, account or event discriminator changed. This includes a change in its
  length (for example an Anchor to Quasar port), a Codama size discriminator, an event
  discriminator offset, and a later constant in an event discriminator chain.
- The start of an account's fields moved. A Codama number constant discriminator is also
  the first field, and a bytes constant comes before the fields, so a change from one to
  the other moves each field (`account_discriminator_offset_changed`).
- An instruction account moved, or an account was removed from the middle of the list.
- An instruction argument was added, removed, retyped or moved.
- Two instructions have the same discriminator, or the same Codama size discriminator, and
  the same account count.
- Two account types have the same size discriminator. The parser decodes all of them as the
  first one.
- An account type or event type was removed.
- An event envelope discriminator or the event payload offset changed.
- The kind of a type changed, for example from struct to enum.
- On a reachable type: a field was added, removed, retyped or moved; an enum variant was
  inserted before the end, removed or moved; a variant tag value or payload changed; the
  enum tag width changed; an alias target changed; or the type was removed while a parser
  still reads it.
- `serialization` or `repr` changed and a field of a zero-copy account moves, for example
  when a converter removes `bytemuckunsafe` from a padded type. The same applies when the
  layout of a zero-copy account cannot be calculated, for example when it holds an enum.

### Dangerous

- The program name changed. The generated module name changes.
- An enum variant was added at the end.
- An account was added at the end of an instruction's account list.
- An optional account was removed from the end of the list.
- The `writable`, `signer` or `optional` flag of an instruction account changed.
- A field, argument, account or variant was renamed in place: the same type, the same
  position, and neither name is used at another position on the other side. Otherwise
  idl-drift reports a move.
- A discriminator collision exists in both versions.
- The set of account counts under a shared discriminator changed. This changes the
  dispatch order.
- `serialization` or `repr` changed, no field moves, but the zero-copy size changes
  (trailing padding).
- An error was removed or renumbered, or a new error name took an old code. An indexer
  names the `Custom(code)` of a failed transaction from the IDL's errors.

### Additive

- A new instruction, account type or event type.

### Cosmetic

- A field, variant, enum tag or alias change on a type that no parser reads.
- A `serialization` or `repr` change that moves no zero-copy field. This includes a type
  that is only read as Borsh, and a nested type's own `serialization`, which no zero-copy
  read uses.

## Rule names

Each change has a `kind`: the rule that found it. Rule names are stable. An
[allow file](#allow-file) matches a change on `kind` and `subject`, not on the message text,
which can change.

| Area | `kind` | Tier |
|---|---|---|
| Program | `program_address_changed` | Breaking |
| | `program_renamed` | Dangerous |
| Instructions | `instruction_removed`, `instruction_duplicate`, `instruction_name_collision`, `discriminator_reused`, `discriminator_changed`, `discriminator_offset_changed`, `collision` | Breaking |
| | `collision_preexisting`, `dispatch_order_changed` | Dangerous |
| | `instruction_added` | Additive |
| Instruction accounts | `account_moved`, `account_removed` | Breaking |
| | `account_added`, `optional_account_removed`, `account_renamed`, `account_flags_changed` | Dangerous |
| Instruction arguments | `arg_added`, `arg_removed`, `arg_changed`, `arg_moved` | Breaking |
| | `arg_renamed` | Dangerous |
| Account types | `account_type_removed`, `account_discriminator_changed`, `account_discriminator_offset_changed` | Breaking |
| | `account_type_removed` when the type stays in `types`. Anchor 1.x lists only the accounts that the program owns. | Dangerous |
| | `account_type_added` | Additive |
| Events | `event_removed`, `event_discriminator_changed`, `event_envelope_changed`, `event_payload_offset_changed` | Breaking |
| | `event_added` | Additive |
| Defined types | `type_kind_changed`, `type_removed`, `type_undefined` | Breaking |
| | `field_added`, `field_removed`, `field_changed`, `field_moved`, `variant_inserted`, `variant_removed`, `variant_moved`, `variant_tag_changed`, `variant_payload_changed`, `enum_tag_changed`, `alias_changed` | Breaking on a reachable type, else Cosmetic |
| | `field_renamed`, `variant_renamed`, `variant_appended` | Dangerous on a reachable type, else Cosmetic |
| | `layout_changed` | Breaking, Dangerous or Cosmetic. See [Zero-copy types](#zero-copy-types). |
| Errors | `error_removed`, `error_renumbered`, `error_code_reused` | Dangerous |

The `subject` names the item. A positional item has `#<position>`, counted from 0:

| Subject | Meaning |
|---|---|
| `program` | The program address or name. |
| `swap` | The instruction, type or event `swap`. |
| `swap#2` | Account or argument 2 of instruction `swap`. |
| `size:4` | A collision on a Codama size discriminator of 4 bytes. |
| `Pool#1` | Field or variant 1 of type `Pool`. |

## Allow file

`--allow <FILE>` lists the changes that you accept. An allowed change does not set exit
code 1, but it stays in the report. The human report marks it `[ALLOWED]`, the JSON report
adds `"allowed": true` to it, and the Markdown report puts it in a `<details>` block.

Each line of the file holds one entry: a rule name (`kind`), a space, and the `subject`.
Both must match the change exactly. There are no wildcards. A `#` at the start of a line or
after a space starts a comment, to the end of the line. Use the comment for the reason.
Blank lines are ignored.

```text
# Accepted for the v2 upgrade.
field_changed Pool#1   # feeBps is now u32. All indexers read the new IDL.
account_added swap#2   # The new oracle account.
```

The `#` in `Pool#1` is part of the subject, because no space comes before it.

- An entry that matches no change gives a warning on stderr with its line number. The JSON
  report lists it in `unused_allows`. It does not change the exit code. Remove the entry
  after the release.
- A line with no subject, or a rule name that idl-drift does not have, stops the run with
  exit 2. Thus a typo cannot let a change through.

## Exit codes

The exit codes are in the [README](../README.md#exit-codes). A reader that closes
the pipe early, such as `head`, does not change the exit code.

## JSON output

`idl-drift diff --json` prints one object with these keys:

| Key | Content |
|---|---|
| `changes` | Each change, with `severity`, `kind`, `subject` and `message`. An allowed change also has `"allowed": true`. |
| `breaking`, `dangerous`, `additive`, `cosmetic` | The messages of each tier, other than allowed changes. |
| `counts` | The number of changes in each tier, other than allowed changes. |
| `unused_allows` | Only with `--allow`: the entries that match no change, each with `line`, `rule` and `subject`. |
| `exit_code` | The same value as the process exit code. |

`counts` and the tier lists do not include allowed changes, so they agree with the exit
code. `changes` holds all of them.

One entry of `changes` for the `amm_old.json` and `amm_new.json` pair in
`crates/idl-drift/tests/fixtures/`:

```json
{
  "severity": "breaking",
  "kind": "field_changed",
  "subject": "Pool#1",
  "message": "FIELD changed (Pool#1) via reachable type: feeBps: u16 -> u32 (Borsh layout)"
}
```

When the run exits 2, stdout holds only `{"error": "<message>", "exit_code": 2}`. The
message also goes to stderr.

## Markdown output

`idl-drift diff --markdown` prints GitHub Markdown for a job summary (`$GITHUB_STEP_SUMMARY`)
or a pull request comment: a heading with the two inputs, the number of changes in each
tier, the exit code, a table of the changes (tier, rule, subject, message), and the allowed
changes in a `<details>` block. The [GitHub Action](ci.md) writes it to the job summary.

IDL names and messages are escaped, so an IDL cannot add HTML, links or table rows. When the
run exits 2, stdout holds one line, `**idl-drift could not run (exit code 2):** <message>`.
The message also goes to stderr.

## inspect

`inspect` lists the fields whose byte layout idl-drift cannot resolve, and each type that
the IDL uses but does not define. A change to such a field cannot be classified, and
`idl-verify` cannot decode it. Run `inspect` on a new IDL
before you trust a `diff` of it.

idl-drift cannot resolve these Codama nodes, and `inspect` lists them:

- hidden prefixes and suffixes, offsets and sentinels;
- inline enums;
- a size prefix around a type that is not a string or bytes;
- a fixed-size wrapper around a type that is not a string or bytes;
- a fixed count on a map or set;
- a length, count or option prefix that is a float or a wrapped number, and an enum size
  that is a wrapped number;
- a zeroable option with a custom zero value.

idl-drift reads these Codama nodes in a simpler form. `inspect` does not list them, and
`diff` does not report a change to the parts that idl-drift ignores:

- A `booleanTypeNode` is read as 1 byte, whatever its `size`.
- A wrapper inside an `amountTypeNode`, `dateTimeTypeNode` or `solAmountTypeNode` is
  dropped. Only the number inside it is read.
- A size-prefixed or remainder string with `base16`, `base58` or `base64` encoding is read as
  UTF-8 text. Its length is correct, but idl-verify reports a `decode` error when the bytes
  are not valid UTF-8.

idl-drift can resolve inline structs, big-endian numbers, `shortU16`, any integer length
prefix, remainder lengths, and tagged, zeroable and remainder options.

A Codama IDL with a `hiddenPrefixTypeNode` on argument `n` of instruction `ping` gives:

```text
instructions: 1  accounts: 0  events: 1  types: 1
unmapped (Generic) fields: 1
UNMAPPED: ix:ping.n -> Generic("hiddenPrefixTypeNode")
undefined types: 0
padded zero-copy types also read as Borsh: 0
instructions that Shipstern cannot tell apart: 0
account types that Shipstern skips: 0
```

### Padded zero-copy types that Borsh also reads

`inspect` also lists, on a `SHARED:` line, each padded zero-copy type that Borsh also
reads. A type inside a `bytemuckunsafe` account sits at `repr(C)` offsets, with implicit
padding. When an instruction argument, an event or a Borsh account reaches the same type,
Borsh reads it without the padding. One type definition cannot describe both reads.
Shipstern's converter rejects this case as `PaddedBorshType`.

### Shipstern warnings

`inspect` also lists, on `SHIPSTERN:` lines, what Shipstern's generated parser cannot use.
Shipstern selects an instruction or an account type by its first discriminator only.

- Instructions that share the first discriminator and the account count. Codama can tell them
  apart by a later discriminator, but Shipstern rejects each of them as ambiguous.
- Codama account types whose first discriminator Shipstern cannot read, so it skips them: a
  field discriminator that is not fixed-size bytes with a bytes default value, or a constant
  that is not an unsigned number or bytes.

These lines do not change the exit code, because other parsers read these IDLs.

### --layout

`--layout` prints each `bytemuck` and `bytemuckunsafe` type field by field: the offset, size
and alignment of each field under SBF `repr(C)` rules, each implicit gap, and the zero-copy
size next to the Borsh size. The alignment rules are in
[idl-verify.md](idl-verify.md#zero-copy-layout).

```bash
idl-drift inspect --layout fixtures/live/voltr_vault/idl.json
```

```text
instructions: 28  accounts: 6  events: 26  types: 44
unmapped (Generic) fields: 0
undefined types: 0
padded zero-copy types also read as Borsh: 0
instructions that Shipstern cannot tell apart: 0
account types that Shipstern skips: 0
zero-copy types: 13

AdaptorAddReceipt (bytemuckunsafe repr(C)): repr(C) size 144, Borsh size 137, padding 7
  offset  size  align  field
       0    32      1  vault
      32    32      1  adaptor_program
      64     1      1  version
      65     1      1  bump
      66     7      1  _padding0
      80     8      8  last_updated_epoch
      88    56      1  _reserved
  gap at 73: 7 bytes before `last_updated_epoch`
...
```

An IDL whose converter removed `serialization` lists no zero-copy types. Check this first
when a parser reads too few bytes.

## Library

The `idl_drift` crate exposes the same functions as the CLI. An example is in the
[crate README](../crates/idl-drift/README.md). `idl_drift::model` holds the IDL model that
both tools use. `idl_drift::layout` calculates zero-copy layouts. `idl_drift::inspect` holds
the functions behind `inspect`.

## Limitations

- **Shank IDLs: instructions only.** A Shank IDL (`metadata.origin: "shank"`, for example
  from Metaplex or Phoenix) keeps each instruction tag in `discriminant`, and idl-drift reads
  it. Shank accounts have no discriminator, so a change in how a program tells its accounts
  apart is not found. A Shank `vec` with a `size` does not load (exit 2).
- **IDL against IDL only.** idl-drift compares IDLs, also with `--onchain`. It cannot see a
  program that was upgraded on chain with no IDL update. Use [idl-verify](idl-verify.md) to
  check an IDL against real data.
- **New types in `types` are not reported.** A type removed from `types` is reported only
  when a parser still reads it.
- **Unresolved Codama nodes.** idl-drift cannot read inside some Codama nodes. A change
  inside one is Breaking on a reachable type, with a message that idl-drift cannot read the
  encoding. A change only to its docs is not reported. Run `inspect` to see these fields.
  idl-drift reads some other nodes in a simpler form, and does not report them. Both lists
  are in [inspect](#inspect).
- **Collisions use the first discriminator only.** Instructions that share the first
  discriminator are reported as a collision, as in Shipstern, also when a later
  discriminator tells them apart. In the Token-2022 IDL these collisions are real for a
  Shipstern parser.
- **Codama spec 1.x only.** Codama spec 2.x, which is still in development, renames the
  `name` key of each node to `identifier`. idl-drift stops with "Codama spec 2.0.0 is not
  supported; only spec 1.x can be read" (exit 2).
