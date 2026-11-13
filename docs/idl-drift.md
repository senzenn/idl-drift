# idl-drift reference

`idl-drift` compares two versions of a Solana program IDL. It puts each change in a tier,
by its effect on a parser that was generated from the old IDL. `idl-drift source` checks
the program id of an Anchor workspace. `idl-drift stale` and `idl-drift watch` check the
chain for a program upgrade with no IDL update, and for a new on-chain IDL.

## Commands

```text
idl-drift diff [--json | --markdown] [--fail-on <TIER>] [--allow <FILE>] [--write-allow <FILE> [--prune-allow]] [--suggest-version] <OLD> <NEW>
idl-drift diff --onchain [--rpc <URL>] [--source <SOURCE>] [--json | --markdown] [--fail-on <TIER>] [--allow <FILE>] [--write-allow <FILE> [--prune-allow]] [--suggest-version] <PROGRAM> <NEW>
idl-drift inspect [--layout] [--for <FRAMEWORK>]... <IDL>
idl-drift source [--idl <PATH>]... [--onchain [--rpc <URL>]] [--json | --markdown] [--fail-on <TIER>] [<ROOT>]
idl-drift stale [--rpc <URL>] [--source <SOURCE>] [--json | --markdown] <PROGRAM>...
idl-drift watch --programs <FILE> [--state <FILE>] [--once | --interval <SECS>] [--webhook <URL>] [--rpc <URL>] [--json] [--save-onchain <DIR>]
```

| Option | Command | Meaning |
|---|---|---|
| `--json` | `diff`, `source`, `stale` | Print the report as JSON. See [JSON output](#json-output). |
| `--json` | `watch` | Print each event as one line of JSON. See [watch](#watch). |
| `--markdown` | `diff`, `source`, `stale` | Print the report as GitHub Markdown. It cannot be combined with `--json`. See [Markdown output](#markdown-output). |
| `--fail-on <TIER>` | `diff`, `source` | The lowest tier that sets exit code 1. Values: `breaking` (default), `dangerous`, `additive`, `cosmetic`. |
| `--allow <FILE>` | `diff` | Changes that you accept. They do not set exit code 1. See [Allow file](#allow-file). |
| `--write-allow <FILE>` | `diff` | Write an allow file that accepts each change at or above `--fail-on`. See [Baseline](#baseline). |
| `--prune-allow` | `diff` | With `--write-allow`: also remove the entries of the `--allow` file that match no change. See [Baseline](#baseline). |
| `--suggest-version` | `diff` | Also print the version bump that a crate generated from OLD needs. See [Version advice](#version-advice). |
| `--onchain` | `diff` | Read the first argument as a program id, and use the IDL that the program publishes on chain as the old IDL. See [On-chain IDL](#on-chain-idl). |
| `--onchain` | `source` | Also check the chain. See [Chain check](#chain-check). |
| `--rpc <URL>` | `diff`, `source`, `stale`, `watch` | The JSON-RPC URL for `--onchain`, `stale` and `watch`. Use the `IDL_DRIFT_RPC_URL` environment variable in its place: `ps` and the shell history show a URL on the command line. |
| `--source <SOURCE>` | `diff` | With `--onchain`, the account to read: `metadata` (the Program Metadata program) or `anchor` (the Anchor IDL account). |
| `--source <SOURCE>` | `stale` | Count only this IDL account: `metadata` or `anchor`. See [stale](#stale). |
| `--layout` | `inspect` | Also print each zero-copy type field by field, with its gaps and both sizes. |
| `--for <FRAMEWORK>` | `inspect` | Also list what the code generator of the framework rejects, changes or drops: `shipstern` or `carbon`. Give it again for the other framework. See [Framework checks](#framework-checks). |
| `--idl <PATH>` | `source` | One more IDL to check, such as a published copy outside `target/idl`. Give it again for more IDLs. |
| `--programs <FILE>` | `watch` | The programs to watch, with the local IDL of each. See [watch](#watch). |
| `--state <FILE>` | `watch` | The slots that the last check saw, kept between runs. |
| `--once` | `watch` | Check one time and exit, for cron and CI. |
| `--interval <SECS>` | `watch` | Seconds between two checks. Default: 60. |
| `--webhook <URL>` | `watch` | POST each change event as JSON to this URL. Use the `IDL_DRIFT_WEBHOOK_URL` environment variable in its place when the URL holds a secret. |
| `--save-onchain <DIR>` | `watch` | Write the JSON text of each on-chain IDL that a change event reads to `DIR/<program>-<source>.json`. |

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
  `sha256("global:<snake_case name>")`, `sha256("account:<Type>")` or `sha256("event:<Type>")`.
  `<Type>` is the Rust type name, as in Shipstern. A name with a lowercase first letter,
  such as Pyth's `priceUpdateV2`, gets an uppercase first letter, and a snake_case name with
  no uppercase letter becomes PascalCase.
- Codama `rootNode` JSON, the format that Shipstern's `include_shipstern_parser!` reads.

idl-drift also accepts three Anchor shapes that are not in the spec structs: tuple struct
fields written as bare types, the pre-0.30 `{"kind": "alias"}` type, and
`"serialization": {"custom": ...}`. A custom serialization has an unknown layout.

Two types, two accounts or two events with the same name stop the run (exit 2) with
"type `Pool` is defined more than once", or `account` or `event` for those. Shipstern and
Codama read the first one or reject the IDL. Two cases are accepted, as in Shipstern:

- Two types with the name of an account or event and with the same body, such as a legacy
  account body and a type of the same name. When the bodies differ, the error names the
  account or event.
- In a Codama IDL, an account and a defined type of the same name. The account body is used.

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
  variant payloads and aliases. Both IDLs must reach the type, because an old parser reads
  only what the old IDL reaches. A layout change on any other type is Cosmetic.

When the two files use different formats, idl-drift first does these steps:

- It compares names in camelCase, as Codama writes them. So `deposit_vault` and
  `depositVault` are the same instruction.
- It compares an instruction argument that holds a struct, linked or inline, field by
  field, and an argument that links an alias as the alias target. Codama inlines a type
  that has only one use, Shipstern's converter inlines it one level, and Borsh writes a
  struct as its fields, so the bytes are the same. The arguments pair one by one, and an
  argument is inlined only where its type differs from the other side. So a renamed field
  keeps its name, and a struct that both sides link stays a link: a change inside it shows
  on the type.
- It writes each `repr(C)` gap of a zero-copy type as a `[u8; n]` field, named
  `padding_before_<field>` or `padding_end`, when the Codama type has these fields.
  Shipstern's converter writes the gaps this way, so Borsh reads the same bytes.
- It writes each use of a generic struct or alias as its body with the arguments filled in,
  as Codama converters do.
- It compares a tuple struct and a tuple alias as the same type.

For every pair of files, idl-drift also does these steps, because the bytes are the same:

- It compares `Vec<u8>` and `bytes` as the same type. Anchor and Shank write a Rust
  `Vec<u8>` as `bytes`, and codama-rs writes it as a u8 array with a u32 prefix.
- It compares a link to a struct in a field, a variant payload, an alias or an instruction
  argument with the same struct written inline, as the same type. Codama inlines a type that
  has only one use. Two links to structs with different names stay different.
- It compares a link to a non-generic alias in the same places as the alias target, because
  Shipstern writes the target in the generated type. Thus links to two aliases of `u64` are
  the same type.
- When the two files of one format nest the arguments of an instruction differently, but
  hold the same field types in the same order, it compares the arguments field by field.
  For example, Codama links a params type after it gets a second use. Other arguments stay
  as written, so a change inside a params type keeps its type and field name.

### Zero-copy types

`serialization` and `repr` changes follow Shipstern's converter. Only accounts are cast from
memory. Instruction arguments and events are always Borsh. idl-drift lays out each zero-copy
account two times, with the old and with the new `serialization` and `repr`, and compares
the field offsets and the array sizes. An array whose item size changes moves each later
item, so it is Breaking also when the array is the last field. Changes that cancel out are
Cosmetic, for example a converter that removes `packed` from an account and from each type
in it.

### Reserved bytes

Many programs add a field with the bytes of a reserved array, so that the account keeps its
size. A parser built from the new IDL then reads the old bytes as the new field, so the
upgrade is safe only if these bytes are zero in existing data: the accounts on chain, or
the old instructions and events that an indexer reads again. idl-drift reports this as one
Dangerous `reserved_bytes_used` change, with the subject of the reserved field (`Pool#1`)
and the bytes that each new field takes, in place of the Breaking field changes. All of
these must be true:

- The old struct has a `[u8; N]` field with a reserved name: `reserved`, `padding`, `pad`,
  `unused` or `spare`, in any ASCII case, after leading `_` and a trailing number (with or
  without one `_`) are removed. So `_reserved`, `padding_0` and `_padding1` match, and
  `reserves`, `reserved_fee` and `padding_before_x` do not.
- At its position, the new struct has one or more new fields, and can have new reserved
  `[u8; M]` arrays that keep the other bytes. A new field has a name that the old struct
  does not use.
- Each field in its place has a fixed Borsh size, and the sizes add up to N. So a new
  `Option` field does not count.
- Every other field keeps its name, its type and its position.
- When a zero-copy account holds the struct, in either IDL, the `repr(C)` layout keeps
  its size, its alignment and the offset of each other field, and no padding opens inside
  the N bytes.

Otherwise the struct gets the usual field changes, for example new fields that do not fit,
that move a later field, or a struct with another change. New fields in two reserved
arrays of one struct also get the usual field changes. To check the bytes on live accounts,
run [`idl-verify --upgrade-impact --compare-idl`](idl-verify.md#upgrade-impact).

### Discriminator dispatch

A Codama IDL can list more than one discriminator, and the Codama spec says that all of
them must match. idl-drift compares each one: a change to a later discriminator is
`discriminator_changed` (`account_discriminator_changed` for an account type), and Breaking.
Shipstern dispatches on the first discriminator only, so the collision rules below use only
the first one. An argument or field with `defaultValueStrategy: "omitted"` stays in the
layout, because its bytes stay in the data. Only an omitted field that the discriminator
bytes at the start hold is left out. A `shortU16` number discriminator below 0x80 is 1 byte,
as Shipstern reads it. idl-drift does not read a larger one, because Shipstern cannot build a
parser for it.

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
  the other moves each field (`account_discriminator_offset_changed`). A renamed, added or
  removed field does not hide the move. When the change only removes the field of the number
  constant, or only adds it, no other field moves.
- An instruction account moved, or an account was removed from the middle of the list.
- An instruction argument was added, removed, retyped or moved.
- Two instructions have the same discriminator, or the same Codama size discriminator, and
  the same account count.
- An instruction that shares a discriminator gets an account count that an old parser
  routes to another instruction (`dispatch_order_changed`). The old parser takes the
  highest old count at or below the number of accounts that only one instruction has.
- Two account types have the same discriminator or the same size discriminator. The parser
  decodes all of them as the first one.
- Two event types have the same discriminator. Shipstern does not build the parser.
- An account type or event type was removed.
- An event envelope discriminator or the event payload offset changed.
- On a reachable type: the kind of the type changed, for example from struct to enum; a
  field was added, removed, retyped or moved; an enum variant was inserted before the end,
  removed or moved; a variant tag value or payload changed; the enum tag width changed; an
  alias target changed; or the type was removed while a parser still reads it.
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
  idl-drift reports a move. This includes a field of an enum variant payload, and a payload
  that changes from named fields to a tuple of the same types (`field_renamed`).
- A discriminator collision exists in both versions.
- The set of account counts under a shared discriminator changed, but an old parser still
  routes each instruction to itself. This changes the dispatch order.
- `serialization` or `repr` changed, no field moves, but the zero-copy size changes
  (trailing padding).
- New fields take the bytes of a reserved array (`reserved_bytes_used`). See
  [Reserved bytes](#reserved-bytes).
- An error was removed or renumbered, or a new error name took an old code. An indexer
  names the `Custom(code)` of a failed transaction from the IDL's errors.

### Additive

- A new instruction, account type or event type.

### Cosmetic

- A field, variant, enum tag, alias or kind change on a type that no parser reads.
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
| | `collision_preexisting` | Dangerous |
| | `dispatch_order_changed` | Breaking when an old parser routes an instruction to another one, else Dangerous |
| | `instruction_added` | Additive |
| Instruction accounts | `account_moved`, `account_removed` | Breaking |
| | `account_added`, `optional_account_removed`, `account_renamed`, `account_flags_changed` | Dangerous |
| Instruction arguments | `arg_added`, `arg_removed`, `arg_changed`, `arg_moved` | Breaking |
| | `arg_renamed` | Dangerous |
| Account types | `account_type_removed`, `account_discriminator_changed`, `account_discriminator_offset_changed`, `collision` | Breaking |
| | `collision_preexisting` | Dangerous |
| | `account_type_removed` when the type stays in `types`. Anchor 1.x lists only the accounts that the program owns. | Dangerous |
| | `account_type_added` | Additive |
| Events | `event_removed`, `event_discriminator_changed`, `event_envelope_changed`, `event_payload_offset_changed`, `collision` | Breaking |
| | `collision_preexisting` | Dangerous |
| | `event_added` | Additive |
| Defined types | `type_removed`, `type_undefined` | Breaking |
| | `type_kind_changed`, `field_added`, `field_removed`, `field_changed`, `field_moved`, `variant_inserted`, `variant_removed`, `variant_moved`, `variant_tag_changed`, `variant_payload_changed`, `enum_tag_changed`, `alias_changed` | Breaking on a reachable type, else Cosmetic |
| | `field_renamed`, `variant_renamed`, `variant_appended`, `reserved_bytes_used` | Dangerous on a reachable type, else Cosmetic |
| | `layout_changed` | Breaking, Dangerous or Cosmetic. See [Zero-copy types](#zero-copy-types). |
| Errors | `error_removed`, `error_renumbered`, `error_code_reused` | Dangerous |

The `subject` names the item. A positional item has `#<position>`, counted from 0:

| Subject | Meaning |
|---|---|
| `program` | The program address or name. |
| `swap` | The instruction, type or event `swap`. |
| `swap#2` | Account or argument 2 of instruction `swap`. |
| `size:4` | A collision on a Codama size discriminator of 4 bytes. |
| `account:01` | A collision on the account discriminator `01`. `event:01` is the same for events. |
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

### Baseline

`--write-allow <FILE>` writes an allow file that accepts each change at or above `--fail-on`
that is not allowed yet. Use it to start a CI gate on a repository with many IDLs, or to
accept the drift that a parser crate already handles.

- FILE gets one `<rule> <subject>` line for each change, in report order (Breaking first),
  with no duplicates. A new file starts with a comment of two lines.
- If FILE exists, give the same path with `--allow` too. The run then keeps each line and
  comment of the file, and adds the new entries at the end. An entry that matches no change
  stays, and gets the usual warning. Any other existing file stops the run with exit 2, and
  the file does not change. With `--allow A --write-allow B`, B starts with the lines of A.
- `--prune-allow` also removes the entries that match no change, with their comments on the
  same line. Their warnings end with `--prune-allow removed it`, and the JSON report lists
  them in `pruned_allows`.
- FILE must be in a directory that exists and that idl-drift can write to. For a symlink,
  this is the directory of its target. The run checks this with a temp file before it reads
  the IDLs or the chain.
- The write goes to a new temp file in the same directory, and a rename replaces FILE. Thus
  a reader never sees a part of the file. When FILE is a symlink, its target gets the text
  and the link stays. The new file keeps the permissions of the old one.
- The report, `--suggest-version` and the exit code are then the ones that `--allow FILE`
  gives, so the run exits 0. The JSON report has `unused_allows`.
- An allow file cannot hold a subject with a line break (`\n`, `\r`, U+000B, U+000C, U+0085,
  U+2028 or U+2029), a `#` after a space, or a space at one end. Such a change is not written:
  a warning names it, with hidden characters as `\u{..}`, and it still sets exit code 1.
- Then `idl-drift diff --allow FILE` with the same inputs and the same `--fail-on` exits 0.
  It warns only about the entries that matched no change before. A second
  `--allow FILE --write-allow FILE` run does not change FILE.

A CI gate for many IDLs, each with the IDL that its parser crate was generated from:

```bash
# One time: accept the drift that the parsers handle now, and commit the allow files.
for idl in idls/*.json; do
  name=$(basename "$idl" .json)
  idl-drift diff "parsers/$name/idl.json" "$idl" --write-allow "parsers/$name/drift.allow"
done

# In CI: each new change fails the job.
for idl in idls/*.json; do
  name=$(basename "$idl" .json)
  idl-drift diff "parsers/$name/idl.json" "$idl" --allow "parsers/$name/drift.allow" || fail=1
done
exit "${fail:-0}"
```

After a review, run `--allow FILE --write-allow FILE` to add the new changes. Add
`--prune-allow` to remove the entries that no longer match. Do not give `--write-allow` in
the CI gate: it accepts each change, so the gate always passes.

## Version advice

`--suggest-version` adds the version bump that a crate generated from OLD needs, such as a
Shipstern parser crate or a Carbon decoder crate. The bump comes from the highest tier of
all the changes:

| Highest tier | Bump | x.y.z, x of 1 or more | 0.y.z |
|---|---|---|---|
| Breaking | `major` | (x+1).0.0 | 0.(y+1).0 |
| Dangerous | `major` | (x+1).0.0 | 0.(y+1).0 |
| Additive | `major` | (x+1).0.0 | 0.(y+1).0 |
| Cosmetic | `patch` | x.y.(z+1) | 0.y.(z+1) |
| No change | `none` | no new version | no new version |

- Breaking changes alter the bytes that the crate decodes. Dangerous changes keep the
  positions and tags, but the generated code must grow (a new enum variant or account) or
  its Rust names change (a renamed field or program). Thus code that uses the crate can
  fail to compile.
- The Additive rules are `instruction_added`, `account_type_added` and `event_added`. Each
  adds a variant to the instruction, account or event enum of the crate. Shipstern parsers
  and Carbon decoders do not mark these enums `#[non_exhaustive]`, so a `match` on the enum
  in user code fails to compile. No rule adds an item without a new variant, so idl-drift
  gives no `minor` bump. A later version can add values.
- A Cosmetic change alters no byte that a parser reads. It can still alter a generated type
  that no parser reads. Check the generated code if your crate exports such types.
- Cargo reads the first part that is not zero as the major part. For 0.y.z, y is the
  major part. Cargo reads each 0.0.z as incompatible.
- Allowed changes count too: an allow entry sets only the exit code, and the generated
  crate changes all the same. Thus a baseline does not lower the bump.
- The bump does not change the exit code.

The human report adds `suggested version bump: major (a 0.y.z crate bumps y)` before the
exit code, and the Markdown report adds a `Suggested version bump` line. In a script, read
the JSON key:

```bash
idl-drift diff old.json new.json --json --suggest-version | jq -r .suggested_bump
```

## Exit codes

The exit codes are in the [README](../README.md#exit-codes). A reader that closes
the pipe early, such as `head`, does not change the exit code. With `inspect --for`, a
`SHIPSTERN:` or `CARBON:` line of that framework also sets exit code 1; a `CARBON NOTE:` line
does not.

## JSON output

`idl-drift diff --json` prints one object with these keys:

| Key | Content |
|---|---|
| `changes` | Each change, with `severity`, `kind`, `subject` and `message`. An allowed change also has `"allowed": true`. |
| `breaking`, `dangerous`, `additive`, `cosmetic` | The messages of each tier, other than allowed changes. |
| `counts` | The number of changes in each tier, other than allowed changes. |
| `unused_allows` | Only with `--allow` or `--write-allow`: the entries that match no change, each with `line`, `rule` and `subject`. |
| `pruned_allows` | Only with `--prune-allow`: the entries that the run removed from the allow file, with the same keys. |
| `suggested_bump` | Only with `--suggest-version`: `major`, `patch` or `none`. See [Version advice](#version-advice). |
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

An account or event that has no type in `types` is an undefined type, because Shipstern and
Codama need that type to read it. `diff` reports it as `type_undefined`. An array length that
names no const parameter of its type, such as `{"generic": "N"}` in a type without a const
`N`, is an unresolved field.

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
zero-copy accounts with an unknown layout: 0
instructions that Shipstern cannot tell apart: 0
account types that Shipstern skips: 0
```

### Padded zero-copy types that Borsh also reads

`inspect` also lists, on a `SHARED:` line, each padded zero-copy type that Borsh also
reads. A type inside a `bytemuckunsafe` account sits at `repr(C)` offsets, with implicit
padding. When an instruction argument, an event or a Borsh account reaches the same type,
Borsh reads it without the padding. One type definition cannot describe both reads.
Shipstern's converter rejects this case as `PaddedBorshType`.

### Zero-copy accounts with an unknown layout

`inspect` also lists, on an `UNKNOWN LAYOUT:` line, each `bytemuck` or `bytemuckunsafe`
account whose field offsets idl-drift cannot compute, with the reason. A parser casts such an
account from memory, so a wrong offset misreads each field after it. An example is an enum
field: the IDL does not keep the enum `repr`, so the size of the tag is not known.

```text
UNKNOWN LAYOUT: Bank: `config.operational_state`: `BankOperationalState` is an enum, which has no defined zero-copy layout
```

Shipstern's converter rejects these IDLs as `UnknownLayout`, and `idl-verify` gives a
`layout` warning: not verifiable. These lines set exit code 1.

### Shipstern warnings

`inspect` also lists, on `SHIPSTERN:` lines, what Shipstern's generated parser cannot use.
Shipstern selects an instruction or an account type by its first discriminator only.

- Instructions that share the first discriminator and the account count. Codama can tell them
  apart by a later discriminator, but Shipstern rejects each of them as ambiguous.
- Codama account types whose first discriminator Shipstern cannot read, so it skips them: a
  field discriminator that is not fixed-size bytes with a bytes default value, or a constant
  that is not an unsigned number or bytes.

These lines do not change the exit code, because other parsers read these IDLs. With
`--for shipstern`, they set exit code 1.

### Framework checks

`inspect --for shipstern` and `inspect --for carbon` tell you, before you generate a parser,
which parts of the IDL the generated parser rejects, reads wrong or drops. Without `--for`,
the output and the exit code do not change. Each framework prints its lines once, in the
order of the options, after the Shipstern warnings.

`--for shipstern` predicts errors of Shipstern's Anchor converter (codama-from-anchor, at
Shipstern 84ad2363). One such error stops `include_shipstern_parser!` with `compile_error!`.
Each line names the item, the reason, and the converter error in brackets:

```text
parts that include_shipstern_parser! rejects: 6
SHIPSTERN: type BankOperationalState: is in zero-copy memory, but its layout cannot be computed (UnknownLayout)
...
```

The checks are `UnsupportedSpec`, a missing `discriminator` (`Json`), `NotAnchor`,
`MissingAddress`, `LegacyUnsupported`, a missing legacy `version` (`Legacy`),
`UnknownLayout`, `PaddedBorshType`, `TypeMissing`, `EmptyDiscriminator`, `TypeNotStruct`,
`InvalidName`, `DiscriminatorArgument`, `UnrecognizedType`, `UndefinedType`,
`GenericArgsMissing`, `GenericTypeMissing`, `GenericEnum`, `GenericArgMissing`,
`InvalidArrayLength`, `RecursionLimit`, `TooLarge`, `VariableSizeCOption`,
`FixedTypeTooLarge`, `NameCollision`, `AmbiguousDiscriminator`, `DuplicateDiscriminator` and
`ConflictingFlattenedArguments`. `inspect` expands generic types with their arguments, as
the converter does, and counts type nodes to find `TooLarge`. The converter counts JSON
nodes, so its count is higher, and an IDL near the limit of 100000 nodes can pass here and
fail there. Sizes stop at 128 levels of nesting, as in the converter.

It does not predict other `Json` errors (a JSON shape that the converter cannot read), other
`Legacy` errors (Anchor's own upgrade of a legacy IDL), or `InlineEnum`. An IDL that
idl-drift cannot read exits 2 before the check. So exit code 0 does not prove that the macro
compiles.

Shipstern uses Codama JSON as it is, so a Codama IDL gives no converter line. For a Codama
IDL, `--for shipstern` predicts the panics of the macro (Shipstern 0.13.0) on the tag of a
defined enum, with the rule `EnumTag`:

- a variant with an explicit discriminator that is not its position
- a tag size in a wrapper node, such as `preOffsetTypeNode`
- a `shortU16` tag with more than 128 variants
- an `f32` or `f64` tag

Shipstern 0.12 does not have these checks. It reads each such tag as a `u8`, so its parser
reads wrong values.

The lines come in the order that the converter checks. The converter stops at its first
error, so it reports only the first line. A `SHIPSTERN:` line of either kind sets exit code 1
with `--for shipstern`.

`--for carbon` lists each place where carbon-cli does not follow the IDL. A Codama IDL goes
through `carbon-cli --standard codama`; any other IDL goes through the Anchor path. There are
two kinds of lines:

- `CARBON:` the generated decoder reads bytes wrong or drops data, or carbon-cli cannot
  generate a decoder that compiles. These lines set exit code 1.
- `CARBON NOTE:` the generated code changes, but the decoded values stay the same, or
  carbon-cli needs an option. These lines do not change the exit code.

```text
parts that carbon-cli misreads, drops or cannot generate: 1
CARBON: event adminSetCreatorEvent: not a supported event-CPI shape, so carbon-cli skips every event
carbon-cli changes that keep the decoded values: 1
CARBON NOTE: program: no base58 program id in `address` or `metadata.address`, so carbon-cli stops unless --program-id is given
```

The `CARBON:` checks: a `COption`, or an option with another tag or a fixed size, read as a
`u8` tag; a zero-copy account with `repr(C)` padding, read as Borsh; a legacy event whose name
is not its Rust type name; two instruction accounts with one `snake_case` name (account groups
add no prefix); an account, instruction or event with the same check as one that the decoder
tries first, when that one needs no more accounts than the later instruction sends; a spec
0.1.0 type with `{"generic": ...}` or an alias of kind `type`, which nodes-from-anchor cannot
read; an array length that is not a number, which gives Rust that does not compile; the
accounts of a Shank IDL, which get no discriminator; big-endian numbers; `shortU16`; a bool
wider than one byte; strings, bytes and collections whose length is not a `u32` prefix;
fixed-size wrappers around a type of another size; zeroable and remainder options; hidden
prefixes and suffixes, offsets and sentinels; inline enums; an inline struct in an enum
variant, as an instruction argument, or inside an option, a collection or a tuple (except the
item of an array argument, which carbon-cli names); enum tags that are not `u8` or not the variant
position; field discriminators, which get no check; no discriminator when the program has two
items of the kind; skipped bytes that are not the removed discriminator field, when fields
are left to read; discriminator values that carbon-cli cannot turn into bytes; and events that
carbon-cli cannot frame, which make it skip every event.

The `CARBON NOTE:` checks: no program id, an inline struct field, a fixed-size wrapper
around a type of the same size, a tuple of one bool, no discriminator on the only item of its
kind, and discriminators that the decoder does not check.

The `CARBON` lines follow carbon-cli at commit a64db3c (2.0.0) and the Anchor converter that
it calls, `@codama/nodes-from-anchor` 1.2.1, from their TypeScript source. They are not checked
against carbon-cli output. Not checked: other shapes that nodes-from-anchor cannot read, and
the legacy (pre-0.30) Anchor path past its events and Shank accounts.

### --layout

`--layout` prints each `bytemuck` and `bytemuckunsafe` type field by field: the offset, size
and alignment of each field under SBF `repr(C)` rules, each implicit gap, and the zero-copy
size next to the Borsh size. The `padding` total also counts the gaps inside nested structs,
once for each array item, and a `padding inside` line names each field that holds them. The
alignment rules are in [idl-verify.md](idl-verify.md#zero-copy-layout).

```bash
idl-drift inspect --layout fixtures/live/voltr_vault/idl.json
```

```text
instructions: 28  accounts: 6  events: 26  types: 44
unmapped (Generic) fields: 0
undefined types: 0
padded zero-copy types also read as Borsh: 0
zero-copy accounts with an unknown layout: 0
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

## source

`source` checks that an Anchor workspace gives each program one program id. Anchor keeps
the id in more than one place, and a new keypair or a deploy to another cluster can change
one copy and not the others. Anchor's generated entrypoint compares the program id with
`declare_id!` and rejects each instruction when they differ (`DeclaredProgramIdMismatch`).
Clients built from an IDL send to the address in the IDL.

```bash
idl-drift source                                   # the workspace in the current directory
idl-drift source path/to/workspace --idl idls/amm.json
```

ROOT is the directory that holds `Anchor.toml`. `source` reads the program id from four
places, the *legs*:

| Leg | JSON `leg` | Where |
|---|---|---|
| `declare_id!` | `declare_id` | The crate root of each `programs/*/` crate: the `[lib] path` in its `Cargo.toml`, else `src/lib.rs`. Also the file of a root `mod <name>;` that the crate root takes `ID` from, such as `src/id.rs`. A crate root that does not exist gives no leg. The program name is the `[lib] name`, else the `[package] name` with `_` for `-`. A `Cargo.toml` that is not valid TOML or has no `[package]` stops the run (exit 2). |
| Anchor.toml | `anchor_toml` | Each program of each cluster under `programs`: `name = "<ID>"` or `name = { address = "<ID>" }`. Every TOML form counts: `[programs.<cluster>]` tables, dotted keys such as `programs.localnet.amm = "<ID>"`, and inline tables. An entry of another shape, or a file that is not valid TOML, stops the run (exit 2). |
| IDL address | `idl_address` | Each `target/idl/*.json`, and each IDL given with `--idl`. For Anchor, the id is `address`, else `metadata.address` (legacy Anchor), and the program name is `metadata.name`, else the top-level `name`. Anchor writes the name of the `#[program] mod` there. The next paragraph says how an IDL name finds its crate. A file given again with `--idl` is read one time. For Codama (`rootNode`), only `program.publicKey` and `program.name` count, as in the Codama tools. The first id key that is there gives the id: when its value is empty or not a string, the finding is `invalid_id`, and `source` does not read the next key. |
| On chain | `onchain` | Only with `--onchain`. See [Chain check](#chain-check). |

`declare_id!` is the reference, because the program accepts only the id that it was built
with. Each other leg is compared with it. When a program has no `declare_id!`, its IDL
address must match one of its Anchor.toml ids. With neither, its IDL addresses must match
each other. Program names match in `snake_case`, so the Codama name `orderEngine` and the key
`order-engine` are the crate `order_engine`. The crates `a_mm` and `am_m` stay two programs,
and so do `pool_2` and `pool2`. When no lib has the name of an IDL, the IDL gets the lib name
of the one crate whose lib name or root `#[program] mod` name it is, with no `_` before a
digit. So the Codama name `limitOrder2` is the crate `limit_order_2`, and an Anchor IDL named
`amm_program` is the crate with `#[program] mod amm_program`. When two crates fit, the IDL
keeps its name.

A `declare_id!` counts when rustc compiles it into the crate's `ID`:

- At the crate root.
- Directly in a branch block of `cfg_if! { .. }` (also `cfg_if::cfg_if! { .. }`), not in a
  `mod` or `fn` in it. Another macro can drop or change its input, so a `declare_id!` in it,
  as in `name! { .. }`, `name!(..)` or `name![..]`, does not count. A `mod` or a `use` in a
  branch block counts as one at the crate root.
- In a module at the crate root that the crate root imports with `use <module>::ID` or
  `use <module>::{ID, ..}`, `pub` or not. A nested module, or a renamed import such as
  `use keys::ID as KEY`, does not count. A root `mod <module>;` counts in the same way: its
  file is `<module>.rs` or `<module>/mod.rs` next to the crate root. Anchor's `keys sync` also
  writes the id in `src/id.rs`.
- In a module that the crate root imports with `use <module>::*`, only when no `declare_id!`
  at the crate root and no `use` that names `ID` is always there. One with a `#[cfg(..)]`, or
  one in a `cfg_if!` branch, is not always there. A `cfg_if!` chain that ends in `else` and
  has such an `ID` in each branch is always there. As in rustc, the `ID` that the crate defines
  or names shadows the glob, so the glob counts only when a cfg can remove that `ID`.

One in a comment, a string, a function or another module (such as
`mod jupiter { declare_id!(..) }` for a CPI target) does not count. `source` does not
evaluate `#[cfg(..)]`, so a `declare_id!` under a cfg that is never true still counts.

`source` reads only the program id from the source code. It does not build or parse the
program. A missing file is not an error, because a CI job often has only some of the
files. A program is compared when it has a valid `declare_id!` and one more valid id (the
chain included), or, with no valid `declare_id!`, a valid IDL address and a valid
Anchor.toml id, or, with neither, two valid IDL addresses. Two Anchor.toml clusters alone do
not count, because no rule compares one cluster with another. When no program is compared and
no finding is above Cosmetic, `source` has nothing to compare, and the run stops (exit 2). For
example, ROOT holds no program id, only a library crate, or only Anchor.toml and a `lib.rs`
with no `declare_id!`.

`source` reads only regular files under ROOT. A symlink that points out of ROOT, a path
that is not a regular file, a `[lib] path` that resolves out of ROOT, or a file over 16 MiB
stops the run (exit 2). An IDL given with `--idl` can be outside ROOT, but the same file
type and size rules apply to it.

`source` reads the IDL name and address from the raw JSON, not with the IDL model of
`diff`. So an IDL of any format gives its id, also an IDL that `diff` cannot load. This is
also true for the on-chain IDL with `--onchain`. A file that is not JSON, or an IDL with no
name, stops the run (exit 2). An on-chain IDL needs no name: its address is enough.

### Findings

**Tiers for source.** A Breaking finding means that a tool or a client uses a wrong program
id. A Dangerous finding means that the check is weaker, or that a client cannot read the
IDL from chain. A Cosmetic finding means that an id cannot be checked. `source` has no
Additive findings.

| `kind` | Tier | Why |
|---|---|---|
| `anchor_toml_mismatch` | Breaking | The id in `[programs.<cluster>]` is not the `declare_id!`. Anchor's tools use that id on that cluster. For example, `anchor test` loads the program at the `[programs.localnet]` id, where the program rejects each instruction. |
| `idl_address_mismatch` | Breaking | The IDL address is not the `declare_id!`. With no `declare_id!`, it is not an Anchor.toml id, or with neither, it is not the address of the first IDL of the program. Clients and indexers built from the IDL use the wrong program. |
| `invalid_id` | Breaking | An id is not base58 of 32 bytes. This includes an IDL `address` that is empty or not a string. |
| `onchain_missing` | Breaking | With `--onchain`: no `declare_id!` value of the program is a deployed program. Each value has no account, or an account that is not executable. |
| `onchain_mismatch` | Breaking | With `--onchain`: the IDL that the program publishes on chain gives another address, or no address that can be read. A legacy IDL has no address key, so the program is its address. An IDL with neither an address nor a name has no address that can be read. |
| `onchain_idl_missing` | Dangerous | With `--onchain`: the program is deployed, but it has no on-chain IDL account. A client cannot read its IDL from chain. |
| `declare_id_duplicate` | Dangerous | The crate root has more than one `declare_id!` value, so the program picks its id with `cfg`, for example one for each feature. An id that matches any one of the values passes, so a wrong id for one build is not found. This finding lists the values. Use `--fail-on dangerous` to fail on it. More than 16 values is Breaking, and the chain check skips them, because a `cfg` set needs far fewer. |
| `declare_id_missing` | Cosmetic | The `lib.rs` of a program that Anchor.toml or an IDL names has no `declare_id!` at the crate root, so its other ids cannot be checked. A crate that nothing else names is a library, and a program with no crate in `programs/` has its source elsewhere. Neither one is a finding. |
| `idl_address_missing` | Cosmetic | An IDL has no address, so its id cannot be checked. |

A mismatch message names the `declare_id!` value. When the program has more than one value,
the message says "one of the N declare_id! values", and `declare_id_duplicate` lists them.

The `subject` is the program (`amm`), the program and the cluster (`amm@mainnet`), the
program and the chain (`amm@chain`), or, for `invalid_id`, the file
(`target/idl/amm.json`, `Anchor.toml:mainnet`).

### Chain check

`--onchain` checks each distinct valid `declare_id!` value of each program. A program that
picks its id with `cfg` can have one value for each cluster, so `source` needs only one
deployed value on the cluster of the RPC URL.

1. At least one value must be an executable program account. If no value is, `source`
   gives one `onchain_missing` for the program.
2. For each value that is a deployed program, the IDL that the program publishes, in its
   Program Metadata account or its Anchor IDL account (see [On-chain IDL](#on-chain-idl)),
   must give the same `address`. When the program has both accounts, `source` checks both.
   When it has neither, `source` gives `onchain_idl_missing`. A legacy IDL with no address
   gets the program id, so it always matches.

Each IDL read from chain is in the report as an `onchain` leg. In the JSON report, this leg
has `source` (`metadata` or `anchor`), the IDL account (`account`), and the address in the
IDL (`id`):

```json
{"program": "amm", "leg": "onchain", "source": "anchor", "account": "<IDL ACCOUNT>", "id": "<ADDRESS>"}
```

A failed RPC request, an RPC answer with no account `value` or no `executable` flag, or an
IDL account that cannot be read stops the run (exit 2). Give the URL in
`IDL_DRIFT_RPC_URL` or with `--rpc`, as for `diff --onchain`.

A CI job with no RPC URL can run `source` without `--onchain`. It still checks
`declare_id!`, Anchor.toml and the IDLs.

### Exit codes and output

`source` exits 0 when no finding is at or above `--fail-on` (default `breaking`), 1 when
at least one is, and 2 when it cannot run: ROOT is not a directory or has nothing to compare,
a file cannot be read or parsed, a file is outside ROOT, not a regular file or over 16 MiB,
an option is wrong, or an RPC request failed.

The human report prints one line for each id, `<program>  <place>  <id>`, then the
findings, Breaking first, then the counts:

```text
amm  [programs.localnet] in Anchor.toml  29dv8e1WcjL4w6a7HDaHbUfXrF12yiJiVcKQ1qgeT3rF
vault  [programs.localnet] in Anchor.toml  6k78AbasGMFFrhG95Pj6jQbqkVt7FQMhVgemxJovWKR6
amm  [programs.mainnet] in Anchor.toml  4SsxzsqrisRmgB5N7NfXVEuU9e9sssebVUyhzmD1rNpH
amm  declare_id! in programs/amm/src/lib.rs  29dv8e1WcjL4w6a7HDaHbUfXrF12yiJiVcKQ1qgeT3rF
vault  declare_id! in programs/vault/src/lib.rs  (none)
amm  address in target/idl/amm.json  5bVzRzm2mwUd3iKzXTD9S82SJLjJKxKXzRJrzDyCZ3JJ
  [BREAKING] amm@mainnet: [programs.mainnet] amm is 4SsxzsqrisRmgB5N7NfXVEuU9e9sssebVUyhzmD1rNpH, ...
  [BREAKING] amm: target/idl/amm.json address is 5bVzRzm2mwUd3iKzXTD9S82SJLjJKxKXzRJrzDyCZ3JJ, ...
  [COSMETIC] vault: no declare_id! at the crate root of programs/vault/src/lib.rs: the ids of vault cannot be checked
summary: 2 breaking, 0 dangerous, 0 additive, 1 cosmetic
exit code: 1
```

`--json` prints one object with these keys:

| Key | Content |
|---|---|
| `root` | ROOT as given. |
| `ids` | Each id read, with `program`, `leg` and `id`. `leg` is `declare_id`, `anchor_toml`, `idl_address` or `onchain`. A leg from a file has `path`. An `anchor_toml` leg also has `cluster`. An `onchain` leg has `source` and `account`. An empty `id` is a file that holds none: a `lib.rs` with no `declare_id!`, or an IDL with no address. |
| `findings` | Each finding, with `severity`, `kind`, `program`, `subject` and `message`. |
| `counts` | The number of findings in each tier. |
| `exit_code` | The same value as the process exit code. |

On exit 2, stdout holds only `{"error": "<message>", "exit_code": 2}`, as for `diff`.
`--markdown` prints a heading with ROOT, the counts, the exit code, a table of the ids and a
table of the findings. `source` has no allow file.

## stale

`stale` tells whether the IDL that a program publishes on chain is older than the program's
code. A program upgrade often comes with no IDL update, and a parser built from the on-chain
IDL then reads the new data with the old layout.

```bash
export IDL_DRIFT_RPC_URL=https://api.mainnet-beta.solana.com
idl-drift stale 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH
```

For each program, `stale` reads two slots:

- The slot of the last deploy. The upgradeable loader keeps it in the ProgramData account,
  and loader v4 at the start of the program account.
- The slot of the last write to each IDL account (see [On-chain IDL](#on-chain-idl)): the
  slot of the newest successful transaction among the newest 100 that
  `getSignaturesForAddress` gives for the account. A failed transaction wrote nothing.

An IDL account is stale when a deploy came after its last write. A write in the slot of the
deploy counts as fresh.

| Status | JSON `status` | Meaning |
|---|---|---|
| stale | `stale` | At least one IDL account that counts is stale. |
| no on-chain IDL | `no_idl` | The program has no IDL account that counts. |
| fresh | `fresh` | No IDL account that counts is stale. |
| error | `error` | The program could not be checked: no account at the address, a loader that keeps no deploy slot (loader v1 or v2, or a closed upgradeable program), no successful transaction among the newest 100 of an IDL account, or a failed request. |

With no `--source`, both IDL accounts count, because a reader of the old one gets the old IDL.
A program that moved to the Program Metadata IDL can keep an old Anchor IDL account: give
`--source metadata` to count only the account that your parser reads. The report gives the
slot and the status of each account.

```text
no on-chain IDL  675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8  deploy slot 445763204
stale  dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH  deploy slot 429731225; Program Metadata account 7DuBKBbixzCJEFgvAxpt7MCUuSwuY854iYJ4BLpzPEVt last written in slot 429731225 (fresh); Anchor IDL account 8BKPjRu7Hvd6Y2J67EdLx9MsrZFQwEMVwYg7NeVbAPF8 last written in slot 384474599 (stale)
summary: 1 stale, 0 fresh, 1 no on-chain IDL, 0 errors
exit code: 1
```

### Extend check

`ExtendProgram` also sets the slot that the upgradeable loader keeps, with no new code. So
when an IDL write is older than that slot, `stale` reads what wrote the slot. It lists the
successful transactions that name the ProgramData account, from that slot down to the oldest
IDL write, and reads the loader instructions in each one, also the inner ones of a multisig.

| JSON `extend_check` | Meaning |
|---|---|
| `{"result": "upgrade"}` | An `Upgrade` or a `DeployWithMaxDataLen` wrote the slot. |
| `{"result": "extend", "upgrade_slot": null}` | An `ExtendProgram` wrote the slot, and no upgrade came after the IDL write. The IDL is fresh. |
| `{"result": "extend", "upgrade_slot": <SLOT>}` | An `ExtendProgram` wrote the slot. An IDL account is stale when its last write is before `<SLOT>`, the newest upgrade. |
| `{"result": "unknown"}` | The check stopped at its limit, `getTransaction` gave no transaction (a pruned one), or the history of the ProgramData account ended above the IDL write. The slot can be an extend, and the status is stale. |

Other programs can name a ProgramData account in many transactions, so the check reads at
most 3 pages of 1000 signatures and 10 transactions, and 2 more requests to start the list
at the slot. The human report adds `(an ExtendProgram; ...)` or `(can be an ExtendProgram)`
after the deploy slot. Loader v4 has no extend, and `extend_check` is not in the report when
the check did not run.

On the public mainnet RPC, a stale program with one IDL account costs about 8 requests,
130 KB and 5 to 8 s (pump.fun and voltr_vault on 2026-11-12). A fresh program needs no extend
check, so it costs 4 requests.

### Exit codes and output of stale

`stale` exits 0 when each program is fresh, 1 when at least one is stale or has no on-chain
IDL, and 2 when at least one could not be checked or an option is wrong. A program that
fails does not stop the others. `--json` prints one object:

| Key | Content |
|---|---|
| `programs` | One object per program, in the order given: `program`, `status`, and `deploy_slot`, `extend_check` and `idls` (each with `source`, `account`, `slot` and `status`), or `error`. |
| `counts` | The number of programs per status: `fresh`, `stale`, `no_idl` and `error`. |
| `exit_code` | The same value as the process exit code. |

`--markdown` prints the counts, the exit code, a table with one row per IDL account, and the
programs that could not be checked.

The newest successful transaction that names an IDL account counts as its last write. A
later transaction that only names the account, such as a lamport transfer to it, makes a
stale IDL look fresh. A stale result means only that no transaction named the IDL account
after the deploy. The deploy can keep the interface, so a stale IDL can still be correct:
run `idl-verify` on live data to know.

## watch

`watch` checks a list of programs for a new deploy or a new on-chain IDL. On each change it
diffs the on-chain IDL with the local IDL that your parser uses, and prints one event.

```bash
idl-drift watch --programs programs.txt --state state.json --once --json
idl-drift watch --programs programs.txt --interval 300 --webhook "$HOOK_URL"
```

The programs file has one `<program> <local IDL path>` per line. A third field, `metadata`
or `anchor`, picks the IDL account: it is the IDL that the line diffs, and the only account
that its stale check counts. A program with both accounts needs it. `#` starts a comment. A
relative path is relative to the directory of the programs file. A path cannot hold a space.

```text
# program                                      local IDL               source
6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P    idls/pump.json
dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH    idls/drift.json         anchor
```

Each check reads the deploy slot and the IDL write slots of each line, as [stale](#stale)
does. When one of them is not the one in the state, `watch`:

1. Runs the [extend check](#extend-check) when it applies, and reads the on-chain IDL,
   unless the program has none.
2. Diffs the local IDL (old) with the on-chain IDL (new), with the rules of `diff`.
3. Prints one change event. With `--webhook`, it also POSTs the event as JSON, with a 10 s
   limit.
4. Keeps the new slots in the state. With `--state`, it writes the state file in one step
   after each check.

The first check of a line has no state, so it prints an event for the line. A failed RPC
request, a local IDL that cannot be read, or a failed POST keeps the old slots for the line,
so the next check tries again. The webhook can get the same event two times.

A change event, on one line with `--json`:

```json
{"event": "change", "program": "<PROGRAM>", "idl": "idls/pump.json", "idl_path": "<DIR>/idls/pump.json",
 "previous": {"deploy_slot": 458921440, "idls": [{"source": "anchor", "account": "<ACCOUNT>", "slot": 446492745}]},
 "deploy_slot": 458921441, "extend_check": {"result": "upgrade"}, "status": "stale",
 "idls": [{"source": "anchor", "account": "<ACCOUNT>", "slot": 446492745, "status": "stale"}],
 "onchain": {"source": "anchor", "account": "<ACCOUNT>", "saved": "<DIR>/<PROGRAM>-anchor.json"},
 "diff": {"changes": [], "counts": {"breaking": 0, "dangerous": 0, "additive": 0, "cosmetic": 0}, "highest": null}}
```

`previous` is null on the first check. `onchain` and `diff` are null when the program has no
on-chain IDL. `diff.changes` has the shape of `changes` in [JSON output](#json-output), and
`diff.highest` is the highest tier, or null. `deploy_slot`, `extend_check`, `status` and
`idls` are as in the JSON output of [stale](#exit-codes-and-output-of-stale). `onchain.saved`
is there only with `--save-onchain`. A line that fails prints `error: <program> <idl>: <message>` on stderr, and
with `--json` also `{"event": "error", "program", "idl", "idl_path", "error"}` on stdout.
Only change events go to the webhook.

`watch --once` exits 0 when each line was checked, and 2 when a line or the webhook failed,
or the programs file, the state file or an option is wrong. Without `--once`, it runs until
it is stopped. The state file has `version` 1, and a list of `programs`, each with
`program`, `idl` (as the programs file gives it), `deploy_slot` and `idls`. Lines that are
no longer in the programs file drop out of it.

## Library

The `idl_drift` crate exposes the same functions as the CLI. An example is in the
[crate README](../crates/idl-drift/README.md). `idl_drift::model` holds the IDL model that
both tools use. `idl_drift::layout` calculates zero-copy layouts.
`idl_drift::diff::reserved_uses` lists the [reserved bytes](#reserved-bytes) that new fields
take, with the byte range of each field. `idl_drift::inspect` holds
the functions behind `inspect`. `idl_drift::source` holds the functions behind `source`:
`discover`, `discover_with_idls`, `read_idl`, `check` and `check_onchain`.
`idl_drift::stale` holds `check`, `observe` and `deploy_slot` behind `stale` and `watch`.
The programs file and the state file of `watch` are CLI formats, not library API. Use
`discover_with_idls` for IDLs outside `target/idl`, so that an IDL name finds its crate as
the CLI does. Before a caller takes a
report as a pass, it must check `SourceReport::nothing_to_compare`, as the CLI does.

## Limitations

- **Shank IDLs: instructions only.** A Shank IDL (`metadata.origin: "shank"`, for example
  from Metaplex or Phoenix) keeps each instruction tag in `discriminant`, and idl-drift reads
  it. Shank accounts have no discriminator, so a change in how a program tells its accounts
  apart is not found. A Shank `vec` with a `size` does not load (exit 2).
- **source: the Anchor layout only.** `source` reads `programs/*/` and `target/idl/` under
  ROOT. For another layout, give each IDL with `--idl`. It does not find a `declare_id!`
  outside the crate root of a `programs/*/` crate and the files of its root `mod <name>;`
  items. It does not read a `mod` with `#[path = ..]`. It does not read
  `#[cfg_attr(.., program)]` as the `#[program] mod`, so an IDL with that module name does not
  find its crate.
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
- **Collisions use the first discriminator only.** Instructions, account types or event
  types that share the first discriminator are reported as a collision, as in Shipstern, also
  when a later discriminator tells them apart. In the Token-2022 IDL these collisions are real
  for a Shipstern parser.
- **Collisions need equal bytes.** Shipstern compares only as many bytes as a discriminator
  has. Thus an account type with discriminator `01` before a type with `0102` at the same
  offset also decodes the data of the second type. idl-drift does not report this case.
- **Legacy instruction names with an acronym.** Anchor 0.29 writes the function `set_a_b` as
  `setAB` and hashes `global:set_a_b`, as Shipstern does. idl-drift hashes `global:set_ab`,
  so its discriminator is wrong.
- **Codama spec 1.x only.** Codama spec 2.x, which is still in development, renames the
  `name` key of each node to `identifier`. idl-drift stops with "Codama spec 2.0.0 is not
  supported; only spec 1.x can be read" (exit 2).
- **No Quasar IDLs.** A Quasar IDL uses another schema: a type definition has a top-level
  `kind`, and a field can have a `codec`. An IDL with a top-level `spec`, for example
  `quasar-idl/1.0.0`, stops `diff` and `inspect` with
  "IDL spec quasar-idl/1.0.0 is not supported; only Anchor, Shank and Codama IDLs can be read"
  (exit 2). `source` still reads its `address`.
