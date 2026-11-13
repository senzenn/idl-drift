# idl-drift reference

`idl-drift` compares two versions of a Solana program IDL. It puts each change in a tier, by its effect on a parser that you generated from the old IDL. `source` checks the program id of an Anchor workspace. `stale` and `watch` find a program upgrade with no IDL update, and a new on-chain IDL.

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
| `--json` | `watch` | Print each event as one line of JSON. |
| `--markdown` | `diff`, `source`, `stale` | Print the report as GitHub Markdown. See [Markdown output](#markdown-output). |
| `--fail-on <TIER>` | `diff`, `source` | The lowest tier that sets exit code 1: `breaking` (default), `dangerous`, `additive` or `cosmetic`. |
| `--allow <FILE>` | `diff` | Accept the changes that FILE lists. See [Allow file](#allow-file). |
| `--write-allow <FILE>` | `diff` | Write an allow file that accepts each change at or above `--fail-on`. See [Baseline](#baseline). |
| `--prune-allow` | `diff` | Needs `--write-allow`. Also remove the `--allow` entries that match no change. |
| `--suggest-version` | `diff` | Also print the version bump that a crate generated from OLD needs. See [Version advice](#version-advice). |
| `--onchain` | `diff` | Read OLD as a program id, and use the IDL that the program publishes on chain. See [On-chain IDL](#on-chain-idl). |
| `--onchain` | `source` | Also check the chain. See [Chain check](#chain-check). |
| `--rpc <URL>` | `diff`, `source`, `stale`, `watch` | The JSON-RPC URL. `diff` and `source` accept it only with `--onchain`. |
| `--source <SOURCE>` | `diff` | Needs `--onchain`. The IDL account to read: `metadata` or `anchor`. |
| `--source <SOURCE>` | `stale` | Count only this IDL account: `metadata` or `anchor`. |
| `--layout` | `inspect` | Also print each zero-copy type field by field. See [--layout](#--layout). |
| `--for <FRAMEWORK>` | `inspect` | Also list what the code generator of `shipstern` or `carbon` rejects, changes or drops. Repeat it for more frameworks. See [Framework checks](#framework-checks). |
| `--idl <PATH>` | `source` | One more IDL to check, such as a published copy outside `target/idl`. Repeat it for more IDLs. |
| `--programs <FILE>` | `watch` | The programs to watch, with the local IDL of each. See [watch](#watch). |
| `--state <FILE>` | `watch` | The slots that the last check saw, kept between runs. |
| `--once` | `watch` | Check one time and exit, for cron and CI. |
| `--interval <SECS>` | `watch` | Seconds between two checks. Default: 60. |
| `--webhook <URL>` | `watch` | POST each change event as JSON to this URL, with a 10 s limit. |
| `--save-onchain <DIR>` | `watch` | Write the JSON text of each on-chain IDL that a change event reads to `DIR/<program>-<source>.json`. |

- Do not use `--json` with `--markdown`, or `--once` with `--interval`. `watch` has no `--markdown`.
- `IDL_DRIFT_RPC_URL` sets `--rpc`, and `IDL_DRIFT_WEBHOOK_URL` sets `--webhook`. Use the env vars. `ps` and the shell history show a URL on the command line, and a webhook URL can hold a secret.
- Colors are on only when stdout is a terminal. Set `NO_COLOR` to turn them off.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | `diff`, `source`: no change or finding at or above `--fail-on`. `inspect`: each field resolved. `stale`: each on-chain IDL is fresh. `watch --once`: each program was checked. |
| `1` | `diff`, `source`: at least one change or finding at or above `--fail-on`. `inspect`: see [inspect](#inspect). `stale`: at least one program has a stale IDL or no on-chain IDL. |
| `2` | idl-drift could not run. A file cannot be read or parsed, an option is wrong, or the on-chain IDL or program account cannot be fetched or read. Each command section adds its own cases. |

A reader that closes the pipe early, such as `head`, does not change the exit code.

### On-chain IDL

`diff --onchain <PROGRAM> <NEW>` compares the IDL that the program publishes on chain with NEW. Use it before an upgrade. idl-drift reads two IDL accounts:

- `metadata`: the canonical `idl` account of the Program Metadata program. Only the upgrade authority of the program can write it. It must hold the IDL as UTF-8 JSON text, with no compression. idl-drift does not read an IDL that this account keeps at a URL or in another account.
- `anchor`: the Anchor IDL account that `anchor idl init` writes. Its data is zlib.

When the program has both accounts, the run stops (exit 2) and asks for `--source`, because the two IDLs can differ. A legacy IDL with no address gets the program id as its address. The report starts with the source account, for example `on-chain IDL: Anchor IDL account <ACCOUNT>`.

## Input formats

The two files can use different formats:

- Anchor IDL, 0.30 spec. `metadata.spec` must be `0.1.0`. Another spec stops the run (exit 2).
- Legacy Anchor IDL (before 0.30). idl-drift calculates each discriminator as Anchor does: the first 8 bytes of `sha256("global:<snake_case name>")`, `sha256("account:<Type>")` or `sha256("event:<Type>")`. `<Type>` is the Rust type name: a lowercase first letter becomes uppercase, and a snake_case name with no uppercase letter becomes PascalCase.
- Codama `rootNode` JSON, spec 1.x: the format that Shipstern's `include_shipstern_parser!` reads.
- Shank IDL. See [Limitations](#limitations).

idl-drift also accepts tuple struct fields written as bare types, the pre-0.30 `{"kind": "alias"}` type, and `"serialization": {"custom": ...}`, which has an unknown layout. Two types, accounts or events with the same name stop the run (exit 2), for example with "type `Pool` is defined more than once" (or "account" or "event" in place of "type"). As in Shipstern, two cases pass. The first is a type with the name and the body of an account or event. When the bodies differ, the error names the account or event. The second is a Codama account and a defined type with the same name. idl-drift then uses the account body.

## How idl-drift compares two IDLs

A generated parser reads bytes by position and routes them by discriminator. idl-drift compares the same way:

- Instructions, account types, events and defined types match by name.
- Instruction accounts, instruction arguments, struct fields and enum variants match by position. Composite account groups are flattened. When an account name repeats, group members get the group path as a prefix: `source.owner` in one format, `sourceOwner` across formats.
- A layout change counts only on a reachable type. Reachable types are account, event and instruction argument types, and each type that these reach through fields, variant payloads and aliases. Both IDLs must reach the type. On another type, a layout change is Cosmetic.
- Three pairs compare as the same type, because the bytes are the same. They are `Vec<u8>` and `bytes`, a struct link and the same struct inline, and a non-generic alias link and its target. Links to two structs with different names stay different.
- Arguments that two files of one format nest differently, with the same field types in the same order, compare field by field. A change inside a params type keeps its type and field name.
- Across formats, names compare in camelCase (`deposit_vault` matches `depositVault`), and a tuple struct matches a tuple alias. A struct argument compares field by field, and an alias argument as its target. idl-drift inlines an argument only where its type differs from the other side. So a renamed field keeps its name, and a change inside a struct that both sides link shows on the type. It writes each use of a generic struct or alias as its body, with the arguments filled in. It writes each `repr(C)` gap of a zero-copy type as a `[u8; n]` field named `padding_before_<field>` or `padding_end`, when the Codama type has these fields.

### Zero-copy types

Only accounts are cast from memory. Instruction arguments and events are always Borsh. For each zero-copy account, idl-drift compares the field offsets and array sizes under the old and the new `serialization` and `repr`, as Shipstern's converter does. A change to the item size of an array is Breaking, also in the last field. Changes that cancel out are Cosmetic, for example `packed` removed from an account and from each type in it.

### Reserved bytes

A program can give the bytes of a reserved array to new fields, so that the account keeps its size. A parser built from the new IDL then reads the old bytes as the new fields. This is safe only when these bytes are zero in existing data. Existing data means the accounts on chain, and the old instructions and events that an indexer reads again. idl-drift reports one Dangerous `reserved_bytes_used` change in place of the Breaking field changes. Its subject is the reserved field (`Pool#1`), and its message gives the bytes of each new field. All of these must be true:

- The old struct has a `[u8; N]` field with a reserved name: `reserved`, `padding`, `pad`, `unused` or `spare`. The match ignores ASCII case, leading `_`, and a trailing number with or without one `_`. So `_reserved`, `padding_0` and `_padding1` match, but `reserves`, `reserved_fee` and `padding_before_x` do not.
- At that position, the new struct has one or more new fields, and can have new reserved `[u8; M]` arrays for the other bytes. The old struct does not use the new names.
- Each field in that place has a fixed Borsh size, and the sizes add up to N. So a new `Option` field does not count.
- Each other field keeps its name, its type and its position. New fields in two reserved arrays of one struct get the usual field changes.
- When a zero-copy account holds the struct in either IDL, the `repr(C)` layout keeps its size, its alignment and the other offsets. No padding opens inside the N bytes.

To check the bytes on live accounts, run [`idl-verify --upgrade-impact --compare-idl`](idl-verify.md#upgrade-impact).

### Discriminator dispatch

- A Codama IDL can list more than one discriminator, and all must match. idl-drift compares each one. A change to a later one is Breaking. Shipstern dispatches on the first discriminator only, so the collision rules use only the first one.
- Instructions that share a discriminator are told apart by account count, most accounts first.
- An argument or field with `defaultValueStrategy: "omitted"` stays in the layout, because its bytes stay in the data. idl-drift leaves out only an omitted field that the discriminator bytes at the start hold.
- A `shortU16` number discriminator below 0x80 is 1 byte, as Shipstern reads it. idl-drift does not read a larger one, because Shipstern cannot build a parser for it.
- idl-drift reads Codama's `optionalAccountStrategy` but does not compare it, because Shipstern ignores it.

## Tiers

| Tier | Meaning | Default effect |
|---|---|---|
| Breaking | The old parser misreads bytes or cannot route them. | Exit code 1. |
| Dangerous | Positions and tags stay the same, but the parser must grow, or generated names change. Review the change. | Exit code 0, or 1 with `--fail-on dangerous`. |
| Additive | New surface. Old parsers do not change. | Exit code 0. |
| Cosmetic | No byte that a parser reads changes. | Exit code 0. |

### Rule names

Each change has a `kind`: the rule that found it. Rule names are stable. An [allow file](#allow-file) matches a change on `kind` and `subject`, not on the message text.

| `kind` | Tier | Change |
|---|---|---|
| `program_address_changed` | Breaking | The program address changes. The parser drops each update from the new address. |
| `program_renamed` | Dangerous | The program name changes, so the generated module name changes. |
| `instruction_added`, `account_type_added`, `event_added` | Additive | The new IDL adds an instruction, an account type or an event type. |
| `instruction_removed`, `account_type_removed`, `event_removed` | Breaking | The new IDL removes an instruction, an account type or an event type. `account_type_removed` is Dangerous when the type stays in `types`: Anchor 1.x lists only the accounts that the program owns. |
| `instruction_duplicate` | Breaking | Two instructions have the same name. |
| `instruction_name_collision` | Breaking | Two instruction names differ only by ASCII case. Shipstern generates one helper for both. |
| `discriminator_reused` | Breaking | A new instruction uses the discriminator or the size discriminator of an existing one. Old parsers route it to the existing one. |
| `discriminator_changed`, `account_discriminator_changed`, `event_discriminator_changed` | Breaking | An instruction, account or event discriminator changes. This includes its length (for example in an Anchor to Quasar port), a Codama size discriminator and a later Codama discriminator. For an event, it also includes the offset and a later constant in the chain. |
| `discriminator_offset_changed` | Breaking | The offset of an instruction discriminator changes. |
| `account_discriminator_offset_changed` | Breaking | The start of the account fields moves. See the notes below. |
| `event_envelope_changed`, `event_payload_offset_changed` | Breaking | The self-CPI event envelope discriminator or the payload offset changes, for example from Anchor's 8-byte event tag to the 1-byte tag of many Pinocchio programs. |
| `account_moved`, `account_removed` | Breaking | An instruction account moves, or the new IDL removes one from the middle of the list. |
| `account_added`, `optional_account_removed` | Dangerous | The new IDL adds an account at the end of the list, or removes an optional account from the end. |
| `account_flags_changed` | Dangerous | The `writable`, `signer` or `optional` flag of an instruction account changes. |
| `arg_added`, `arg_removed`, `arg_changed`, `arg_moved` | Breaking | The new IDL adds, removes, retypes or moves an instruction argument. |
| `account_renamed`, `arg_renamed` | Dangerous | An instruction account or argument gets a new name in place. |
| `collision` | Breaking | Two instructions have the same discriminator (or size discriminator) and account count. Two account types have the same discriminator or size discriminator: the parser decodes all of them as the first. Two event types have the same discriminator: Shipstern does not build the parser. |
| `collision_preexisting` | Dangerous | The collision is in both versions. |
| `dispatch_order_changed` | Breaking or Dangerous | The account counts under a shared discriminator change. Breaking when an old parser routes an instruction to another one, else Dangerous. |
| `type_kind_changed`, `field_added`, `field_removed`, `field_changed`, `field_moved`, `variant_inserted`, `variant_removed`, `variant_moved`, `variant_tag_changed`, `variant_payload_changed`, `enum_tag_changed`, `alias_changed` | Breaking (R) | The kind of a type changes, for example from struct to enum. A field is added, removed, retyped or moved. A variant is inserted before the end, removed or moved. A variant tag value, a variant payload, the enum tag width or an alias target changes. |
| `field_renamed`, `variant_renamed`, `variant_appended` | Dangerous (R) | A field or variant gets a new name in place, or the new IDL adds a variant at the end. |
| `reserved_bytes_used` | Dangerous (R) | New fields take the bytes of a reserved array. See [Reserved bytes](#reserved-bytes). |
| `type_removed`, `type_undefined` | Breaking | The new IDL removes a type that a parser still reads, or an account or event has no type in `types`. |
| `layout_changed` | Breaking, Dangerous or Cosmetic | `serialization` or `repr` changes. Breaking when a zero-copy account field moves, or idl-drift cannot calculate the layout (for example, the account holds an enum). Dangerous when no field moves but the zero-copy size changes (trailing padding). Else Cosmetic. This includes a type that only Borsh reads, and the `serialization` of a nested type, which no zero-copy read uses. |
| `error_removed`, `error_renumbered`, `error_code_reused` | Dangerous | The new IDL removes or renumbers an error, or gives an old code to a new error name. An indexer names the `Custom(code)` of a failed transaction from the IDL errors. |

(R): this tier applies only on a reachable type. On another type, the change is Cosmetic.

- A rename in place keeps the type and the position, and neither name is at another position on the other side. Else idl-drift reports a move. `field_renamed` also covers a field of a variant payload, and a payload that changes from named fields to a tuple of the same types.
- `account_discriminator_offset_changed`: a Codama number constant discriminator is also the first field, but a bytes constant comes before the fields. So a change from one to the other moves each field. A renamed, added or removed field does not hide the move. A change that only adds or removes the field of the number constant moves no other field.
- `dispatch_order_changed`: the old parser takes the highest old count at or below the number of accounts that only one instruction has.

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

`--allow <FILE>` lists the changes that you accept. An allowed change does not set exit code 1, but it stays in the report. The human report marks it `[ALLOWED]`. The JSON report adds `"allowed": true`. The Markdown report puts it in a `<details>` block.

- Each line holds one entry: a rule name (`kind`), a space, and the `subject`. Both must match the change exactly. There are no wildcards. idl-drift ignores blank lines.
- A `#` at the start of a line or after a space starts a comment to the end of the line. Write the reason there. The `#` in `Pool#1` is part of the subject, because no space comes before it.
- An entry that matches no change gives a warning on stderr with its line number. JSON lists it in `unused_allows`. The exit code does not change. Remove the entry after the release.
- A line with no subject, or an unknown rule name, stops the run with exit 2. So a typo cannot let a change through.

```text
# Accepted for the v2 upgrade.
field_changed Pool#1   # feeBps is now u32. All indexers read the new IDL.
account_added swap#2   # The new oracle account.
```

### Baseline

`--write-allow <FILE>` writes an allow file that accepts each change at or above `--fail-on` that is not allowed yet. Use it to start a CI gate on a repository with many IDLs, or to accept the drift that a parser crate already handles.

- FILE gets one `<rule> <subject>` line for each change, in report order (Breaking first), with no duplicates. A new file starts with a comment of two lines.
- To update an existing FILE, give the same path to `--allow`. The run keeps each line and comment, and adds the new entries at the end. Any other existing file stops the run with exit 2, and the file does not change. With `--allow A --write-allow B`, B starts with the lines of A.
- `--prune-allow` also removes the entries that match no change, with their comments on the same line. Their warnings end with `--prune-allow removed it`, and JSON lists them in `pruned_allows`.
- FILE must be in a directory that exists and that idl-drift can write to. For a symlink, this is the directory of its target. The run checks this before it reads the IDLs or the chain. idl-drift writes a temp file in the same directory, and a rename replaces FILE. A symlink stays, and its target gets the text. FILE keeps its permissions.
- The report, `--suggest-version` and the exit code are then the ones that `--allow FILE` gives, so the run exits 0. A second `--allow FILE --write-allow FILE` run does not change FILE.
- A subject with a line break (`\n`, `\r`, U+000B, U+000C, U+0085, U+2028, U+2029), a `#` after a space, or a space at one end cannot go in the file. A warning names the change, with hidden characters as `\u{..}`, and the change still sets exit code 1.

```bash
idl-drift diff parser/idl.json idls/amm.json --write-allow parser/drift.allow   # one time, then commit the file
idl-drift diff parser/idl.json idls/amm.json --allow parser/drift.allow         # in CI: each new change fails the job
```

After a review, run `--allow FILE --write-allow FILE` to add the new changes. Add `--prune-allow` to remove the entries that no longer match. Do not give `--write-allow` in the CI gate: it accepts each change, so the gate always passes.

## Version advice

`--suggest-version` adds the version bump that a crate generated from OLD needs, such as a Shipstern parser crate or a Carbon decoder crate. The highest tier of all the changes sets it:

| Highest tier | Bump | x.y.z, x of 1 or more | 0.y.z |
|---|---|---|---|
| Breaking, Dangerous or Additive | `major` | (x+1).0.0 | 0.(y+1).0 |
| Cosmetic | `patch` | x.y.(z+1) | 0.y.(z+1) |
| No change | `none` | no new version | no new version |

- Dangerous changes keep the positions and tags, but the generated code must grow, or its Rust names change. So code that uses the crate can fail to compile.
- Each Additive rule adds a variant to the instruction, account or event enum of the crate. Shipstern parsers and Carbon decoders do not mark these enums `#[non_exhaustive]`, so a `match` in user code fails to compile. So idl-drift gives no `minor` bump.
- A Cosmetic change can alter a generated type that no parser reads. Check the generated code if your crate exports such types.
- Cargo reads the first part that is not zero as the major part, and each 0.0.z as incompatible.
- Allowed changes count too, so a baseline does not lower the bump. The bump does not change the exit code.
- The human report adds `suggested version bump: major (a 0.y.z crate bumps y)` before the exit code. The Markdown report adds a `Suggested version bump` line. JSON adds `suggested_bump`.

## JSON output

`diff --json` prints one object:

| Key | Content |
|---|---|
| `changes` | Each change, with `severity`, `kind`, `subject` and `message`. An allowed change also has `"allowed": true`. |
| `breaking`, `dangerous`, `additive`, `cosmetic` | The messages of each tier, with no allowed changes. |
| `counts` | The number of changes in each tier, with no allowed changes. So `counts` and the tier lists agree with the exit code. |
| `unused_allows` | Only with `--allow` or `--write-allow`: the entries that match no change, each with `line`, `rule` and `subject`. |
| `pruned_allows` | Only with `--prune-allow`: the entries that the run removed, with the same keys. |
| `onchain` | Only with `--onchain`: `source` (`metadata` or `anchor`) and `account`. |
| `suggested_bump` | Only with `--suggest-version`: `major`, `patch` or `none`. |
| `exit_code` | The same value as the process exit code. |

When `diff` or `source` exits 2, `--json` prints only `{"error": "<message>", "exit_code": 2}`. The message also goes to stderr.

## Markdown output

`diff --markdown` prints GitHub Markdown for a job summary (`$GITHUB_STEP_SUMMARY`) or a pull request comment. It holds a heading with the two inputs, the number of changes in each tier and the exit code. Then it holds a table of the changes (tier, rule, subject, message), and the allowed changes in a `<details>` block. idl-drift escapes IDL names and messages, so an IDL cannot add HTML, links or table rows. On exit 2, stdout holds one line, `**idl-drift could not run (exit code 2):** <message>`, and the message also goes to stderr.

## inspect

`inspect` lists the fields whose byte layout idl-drift cannot resolve, and each type that the IDL uses but does not define. `diff` cannot classify a change to such a field, and `idl-verify` cannot decode it. Run `inspect` on a new IDL before you trust a `diff` of it. Each check prints a count line, for example `unmapped (Generic) fields: 1`, and then one line for each item:

| Line | Item | Sets exit code 1 |
|---|---|---|
| `UNMAPPED:` | A field that idl-drift cannot resolve, for example `UNMAPPED: ix:ping.n -> Generic("hiddenPrefixTypeNode")`. An array length that names no const parameter of its type, such as `{"generic": "N"}` in a type with no const `N`, is one too. | Yes |
| undefined types | An account or event with no type in `types`. Shipstern and Codama need that type to read it. `diff` reports it as `type_undefined`. | Yes |
| `SHARED:` | A padded zero-copy type that Borsh also reads. A `bytemuckunsafe` account holds it at `repr(C)` offsets with implicit padding. An instruction argument, an event or a Borsh account reads it with no padding. One type definition cannot describe both reads. Shipstern's converter rejects this as `PaddedBorshType`. | Yes |
| `UNKNOWN LAYOUT:` | A `bytemuck` or `bytemuckunsafe` account whose field offsets idl-drift cannot compute, with the reason. A parser casts it from memory, so a wrong offset misreads each later field. An example is an enum field: the IDL does not keep the enum `repr`, so the tag size is not known. Shipstern rejects it as `UnknownLayout`, and idl-verify gives a `layout` warning. | Yes |
| `SHIPSTERN:` | What Shipstern's parser cannot use, because it selects an instruction or account type by its first discriminator only. Instructions that share the first discriminator and the account count: Shipstern rejects each as ambiguous. Codama account types whose first discriminator Shipstern cannot read, so it skips them. Such a discriminator is a field that is not fixed-size bytes with a bytes default value, or a constant that is not an unsigned number or bytes. | Only with `--for shipstern` |

idl-drift cannot resolve these Codama nodes, and `inspect` lists them:

- hidden prefixes and suffixes, offsets and sentinels;
- inline enums;
- a size prefix or a fixed-size wrapper around a type that is not a string or bytes;
- a fixed count on a map or set;
- a length, count or option prefix that is a float or a wrapped number, and an enum size that is a wrapped number;
- a zeroable option with a custom zero value.

idl-drift reads these Codama nodes in a simpler form. `inspect` does not list them, and `diff` does not report a change to the parts that idl-drift ignores:

- A `booleanTypeNode` is 1 byte, whatever its `size`.
- idl-drift drops a wrapper inside an `amountTypeNode`, `dateTimeTypeNode` or `solAmountTypeNode`, and reads only the number inside it.
- A size-prefixed or remainder string with `base16`, `base58` or `base64` encoding is read as UTF-8 text. Its length is correct, but idl-verify gives a `decode` error on bytes that are not valid UTF-8.

idl-drift resolves inline structs, big-endian numbers, `shortU16`, any integer length prefix, remainder lengths, and tagged, zeroable and remainder options.

### Framework checks

`inspect --for shipstern` and `inspect --for carbon` list, before you generate a parser, the parts of the IDL that the generated parser rejects, reads wrong or drops. Without `--for`, the output and the exit code do not change. Each framework prints its lines once, in the order of the options, after the `SHIPSTERN:` lines of `inspect`.

For an Anchor IDL, `--for shipstern` predicts the errors of Shipstern's Anchor converter (codama-from-anchor). One such error stops `include_shipstern_parser!` with `compile_error!`. Each `SHIPSTERN:` line names the item, the reason and the converter error in brackets, for example `SHIPSTERN: type BankOperationalState: is in zero-copy memory, but its layout cannot be computed (UnknownLayout)`. The checks are `UnsupportedSpec`, a missing `discriminator` (`Json`), `NotAnchor`, `MissingAddress`, `LegacyUnsupported`, a missing legacy `version` (`Legacy`), `UnknownLayout`, `PaddedBorshType`, `TypeMissing`, `EmptyDiscriminator`, `TypeNotStruct`, `InvalidName`, `DiscriminatorArgument`, `UnrecognizedType`, `UndefinedType`, `GenericArgsMissing`, `GenericTypeMissing`, `GenericEnum`, `GenericArgMissing`, `InvalidArrayLength`, `RecursionLimit`, `TooLarge`, `VariableSizeCOption`, `FixedTypeTooLarge`, `NameCollision`, `AmbiguousDiscriminator`, `DuplicateDiscriminator` and `ConflictingFlattenedArguments`.

- `inspect` expands generic types with their arguments, as the converter does. It counts type nodes for `TooLarge`, but the converter counts JSON nodes. So an IDL near the limit of 100000 nodes can pass here and fail there. Sizes stop at 128 levels of nesting.
- It does not predict other `Json` errors (a JSON shape that the converter cannot read), other `Legacy` errors (Anchor's own upgrade of a legacy IDL), or `InlineEnum`. So exit code 0 does not prove that the macro compiles.
- An IDL that idl-drift cannot read exits 2 before the check.
- The lines come in the order of the converter checks. The converter stops at its first error, so it reports only the first line.

For a Codama IDL, Shipstern uses the JSON as it is, so no converter line comes. `--for shipstern` predicts where the macro (Shipstern 0.13.0) panics on the tag of a defined enum, with the rule `EnumTag`:

- a variant with an explicit discriminator that is not its position
- a tag size in a wrapper node, such as `preOffsetTypeNode`
- a `shortU16` tag with more than 128 variants
- an `f32` or `f64` tag

Shipstern 0.12 does not have these checks. It reads each such tag as a `u8`, so its parser reads wrong values. A `SHIPSTERN:` line of either kind sets exit code 1 with `--for shipstern`.

`--for carbon` lists each place where carbon-cli 2.0.0 does not follow the IDL. A Codama IDL goes through `carbon-cli --standard codama`. Another IDL goes through the Anchor path, which calls `@codama/nodes-from-anchor` 1.2.1.

- `CARBON:` the generated decoder reads bytes wrong or drops data, or carbon-cli cannot generate a decoder that compiles. These lines set exit code 1. Example: `CARBON: event adminSetCreatorEvent: not a supported event-CPI shape, so carbon-cli skips every event`.
- `CARBON NOTE:` the generated code changes, but the decoded values stay the same, or carbon-cli needs an option. These lines do not change the exit code.

The `CARBON:` checks:

- Options: a `COption`, or an option with another tag or a fixed size, read as a `u8` tag; zeroable and remainder options.
- Layout: a zero-copy account with `repr(C)` padding, read as Borsh; big-endian numbers; `shortU16`; a bool wider than one byte.
- Lengths and wrappers: strings, bytes and collections whose length is not a `u32` prefix; fixed-size wrappers around a type of another size; hidden prefixes and suffixes, offsets and sentinels.
- Names: a legacy event whose name is not its Rust type name; two instruction accounts with one `snake_case` name (account groups add no prefix).
- Order: an account, instruction or event with the same check as one that the decoder tries first, when that one needs no more accounts than the later instruction sends.
- Discriminators: the accounts of a Shank IDL, which get no discriminator; field discriminators, which get no check; no discriminator when the program has two items of the kind; skipped bytes that are not the removed discriminator field, when fields are left to read; discriminator values that carbon-cli cannot turn into bytes.
- Types: a spec 0.1.0 type with `{"generic": ...}` or an alias of kind `type`, which nodes-from-anchor cannot read; an array length that is not a number, which gives Rust that does not compile.
- Enums and structs: inline enums; enum tags that are not `u8` or not the variant position; an inline struct in an enum variant, as an instruction argument, or inside an option, a collection or a tuple (except the item of an array argument, which carbon-cli names).
- Events: an event that carbon-cli cannot frame, which makes it skip every event.

The `CARBON NOTE:` checks: no program id, an inline struct field, a fixed-size wrapper around a type of the same size, a tuple of one bool, no discriminator on the only item of its kind, and discriminators that the decoder does not check.

The `CARBON` checks come from the TypeScript source of carbon-cli and of nodes-from-anchor. They are not checked against carbon-cli output. They do not cover other shapes that nodes-from-anchor cannot read, or the legacy (pre-0.30) Anchor path past its events and Shank accounts.

### --layout

`--layout` prints each `bytemuck` and `bytemuckunsafe` type field by field. For each field, it gives the offset, size and alignment under SBF `repr(C)` rules. It also gives each implicit gap, and the zero-copy size next to the Borsh size. The `padding` total also counts the gaps inside nested structs, once for each array item, and a `padding inside` line names each field that holds them. The alignment rules are in [idl-verify.md](idl-verify.md#zero-copy-layout).

```text
AdaptorAddReceipt (bytemuckunsafe repr(C)): repr(C) size 144, Borsh size 137, padding 7
  offset  size  align  field
      65     1      1  bump
      66     7      1  _padding0
      80     8      8  last_updated_epoch
  gap at 73: 7 bytes before `last_updated_epoch`
```

An IDL whose converter removed `serialization` lists no zero-copy types. Check this first when a parser reads too few bytes.

## source

`source` checks that an Anchor workspace gives each program one program id. Anchor keeps the id in more than one place. A new keypair or a deploy to another cluster can change one copy and not the others. The program rejects each instruction when its id is not the `declare_id!` (`DeclaredProgramIdMismatch`). Clients built from an IDL send to the IDL address. ROOT (default `.`) is the directory that holds `Anchor.toml`. `source` reads the program id from four places, the legs:

| Leg | JSON `leg` | Where |
|---|---|---|
| `declare_id!` | `declare_id` | The crate root of each `programs/*/` crate: the `[lib] path` in its `Cargo.toml`, else `src/lib.rs`. Also the file of a root `mod <name>;` that the crate root takes `ID` from, such as `src/id.rs`. The program name is the `[lib] name`, else the `[package] name` with `_` for `-`. |
| Anchor.toml | `anchor_toml` | Each program of each cluster under `programs`: `name = "<ID>"` or `name = { address = "<ID>" }`. Each TOML form counts: `[programs.<cluster>]` tables, dotted keys and inline tables. |
| IDL address | `idl_address` | Each `target/idl/*.json`, and each `--idl` file. Anchor: the id is `address`, else `metadata.address`, and the name is `metadata.name`, else `name`. Codama: `program.publicKey` and `program.name`. |
| On chain | `onchain` | Only with `--onchain`. See [Chain check](#chain-check). |

- `declare_id!` is the reference, because the program accepts only the id that it was built with. Each other leg must match it. With no `declare_id!`, the IDL address must match one Anchor.toml id of the program. With neither, the IDL addresses must match each other.
- Program names match in `snake_case`. So the Codama name `orderEngine` and the key `order-engine` are the crate `order_engine`. `a_mm` and `am_m` stay two programs, and so do `pool_2` and `pool2`.
- An IDL can have a name that no lib has. Then the IDL gets the one crate whose lib name or root `#[program] mod` name matches, with no `_` before a digit. So `limitOrder2` is the crate `limit_order_2`. When two crates fit, the IDL keeps its name.
- The first IDL id key that is present gives the id. An empty or non-string value is `invalid_id`, and `source` does not read the next key. A file given again with `--idl` counts one time.
- `source` reads the IDL name and address from the raw JSON, not with the IDL model of `diff`. So an IDL of any format gives its id, also an IDL that `diff` cannot load or an on-chain IDL. An on-chain IDL needs no name.
- `source` reads only the program id from the source code. It does not build or parse the program. A missing file is not an error, and a crate root that does not exist gives no leg.

A `declare_id!` counts when rustc compiles it into the `ID` of the crate:

- At the crate root.
- Directly in a branch block of `cfg_if! { .. }` or `cfg_if::cfg_if! { .. }`, not in a `mod` or `fn` in it. A `mod` or `use` in a branch block counts as one at the crate root. A `declare_id!` in another macro does not count.
- In a root module that the crate root imports with `use <module>::ID` or `use <module>::{ID, ..}`, `pub` or not. A nested module, or a renamed import such as `use keys::ID as KEY`, does not count. A root `mod <module>;` counts the same way, from `<module>.rs` or `<module>/mod.rs`.
- In a module that the crate root imports with `use <module>::*`, only when no crate root `declare_id!` or `use` of `ID` is always there. One under `#[cfg(..)]` or in a `cfg_if!` branch is not always there. A `cfg_if!` chain that ends in `else`, with an `ID` in each branch, is always there.
- Not in a comment, a string, a function or another module, such as `mod jupiter { declare_id!(..) }` for a CPI target. `source` does not evaluate `#[cfg(..)]`, so a `declare_id!` under a cfg that is never true still counts.

`source` compares a program only when it has enough valid ids. With a valid `declare_id!`, it needs one more valid id from any leg, the chain included. Else it needs a valid IDL address and a valid Anchor.toml id, or two valid IDL addresses. Two Anchor.toml clusters alone do not count. These cases stop the run (exit 2):

- No program is compared and no finding is above Cosmetic: there is nothing to compare. For example, ROOT has only a library crate.
- A symlink that points out of ROOT, a path that is not a regular file, a `[lib] path` out of ROOT, or a file over 16 MiB. An `--idl` file can be outside ROOT, but the other rules apply to it.
- A `Cargo.toml` that is not valid TOML or has no `[package]`. An `Anchor.toml` that is not valid TOML, or a `programs` entry of another shape.
- An IDL file that is not JSON, or a local IDL with no name.

### Findings

A Breaking finding means that a tool or a client uses a wrong program id. A Dangerous finding means a weaker check, or a client that cannot read the IDL from chain. A Cosmetic finding means an id that cannot be checked. `source` has no Additive findings and no allow file.

| `kind` | Tier | Cause |
|---|---|---|
| `anchor_toml_mismatch` | Breaking | The id in `[programs.<cluster>]` is not the `declare_id!`. Anchor's tools use that id on that cluster. For example, `anchor test` loads the program at the `[programs.localnet]` id, and the program rejects each instruction. |
| `idl_address_mismatch` | Breaking | The IDL address is not the `declare_id!`. With no `declare_id!`, it is not an Anchor.toml id. With neither, it is not the address of the first IDL of the program. |
| `invalid_id` | Breaking | An id is not base58 of 32 bytes. This includes an IDL `address` that is empty or not a string. |
| `onchain_missing` | Breaking | With `--onchain`: no `declare_id!` value of the program is a deployed program. Each value has no account, or an account that is not executable. |
| `onchain_mismatch` | Breaking | With `--onchain`: the on-chain IDL gives another address, or no address that can be read. An IDL with neither an address nor a name has no address that can be read. |
| `onchain_idl_missing` | Dangerous | With `--onchain`: the program is deployed, but it has no on-chain IDL account. A client cannot read its IDL from chain. |
| `declare_id_duplicate` | Dangerous | The crate root has more than one `declare_id!` value, for example one for each feature. An id that matches any value passes, so a wrong id for one build is not found. Use `--fail-on dangerous` to fail on it. More than 16 values is Breaking, and the chain check skips them. |
| `declare_id_missing` | Cosmetic | A program that Anchor.toml or an IDL names has no `declare_id!` at its crate root. A crate that nothing names is a library, and a program with no crate in `programs/` has its source elsewhere: neither is a finding. |
| `idl_address_missing` | Cosmetic | An IDL has no address. |

- A mismatch message names the `declare_id!` value. With more than one value, it says "one of the N declare_id! values", and `declare_id_duplicate` lists them.
- The `subject` is the program (`amm`), the program and the cluster (`amm@mainnet`), the program and the chain (`amm@chain`), or, for `invalid_id`, the file (`target/idl/amm.json`, `Anchor.toml:mainnet`).

### Chain check

`--onchain` checks each distinct valid `declare_id!` value of each program. A program can pick its id with `cfg`, one id for each cluster. So `source` needs only one deployed value on the cluster of the RPC URL.

1. At least one value must be an executable program account. Else `source` gives one `onchain_missing` for the program.
2. For each deployed value, each IDL that the program publishes (see [On-chain IDL](#on-chain-idl)) must give the same `address`. `source` checks both accounts when both exist. With neither, it gives `onchain_idl_missing`. A legacy IDL with no address gets the program id, so it always matches.

Each on-chain IDL is an `onchain` leg in the report, for example `{"program": "amm", "leg": "onchain", "source": "anchor", "account": "<IDL ACCOUNT>", "id": "<ADDRESS>"}`. These stop the run (exit 2): a failed RPC request, an RPC answer with no account `value` or no `executable` flag, and an IDL account that cannot be read. A CI job with no RPC URL can run `source` without `--onchain`. It still checks `declare_id!`, Anchor.toml and the IDLs.

### Output of source

`source` exits 0 when no finding is at or above `--fail-on`, 1 when at least one is, and 2 when it cannot run. The human report prints one line for each id, `<program>  <place>  <id>`, then the findings, Breaking first, then the counts.
`--json` prints one object with the keys `root` (ROOT as given), `ids`, `findings`, `counts` (the number of findings in each tier) and `exit_code`. Each `ids` entry has `program`, `leg` and `id`. A leg from a file also has `path`, an `anchor_toml` leg has `cluster`, and an `onchain` leg has `source` and `account`. An empty `id` is a file that holds none. Each finding has `severity`, `kind`, `program`, `subject` and `message`. `--markdown` prints a heading with ROOT, the counts, the exit code, a table of the ids and a table of the findings.

## stale

`stale` tells whether the IDL that a program publishes on chain is older than the program code, for example `idl-drift stale <PROGRAM>...`. A program upgrade often comes with no IDL update. A parser built from the on-chain IDL then reads the new data with the old layout. For each program, `stale` reads two slots:

- The slot of the last deploy. The upgradeable loader keeps it in the ProgramData account, and loader v4 at the start of the program account.
- The slot of the last write to each IDL account. It is the slot of the newest successful transaction in the newest 100 that `getSignaturesForAddress` gives for the account. A failed transaction wrote nothing.

An IDL account is stale when a deploy came after its last write. A write in the slot of the deploy is fresh.

| Status | JSON `status` | Meaning |
|---|---|---|
| stale | `stale` | At least one IDL account that counts is stale. |
| no on-chain IDL | `no_idl` | The program has no IDL account that counts. |
| fresh | `fresh` | No IDL account that counts is stale. |
| error | `error` | The program could not be checked. Causes: no account at the address, a loader that keeps no deploy slot (loader v1 or v2, or a closed upgradeable program), no successful transaction in the newest 100 of an IDL account, or a failed request. |

- With no `--source`, both IDL accounts count, because a reader of the old one gets the old IDL. A program that moved to the Program Metadata IDL can keep an old Anchor IDL account. Give `--source metadata` to count only the account that your parser reads.
- A later transaction that only names the IDL account, such as a lamport transfer, makes a stale IDL look fresh.
- Stale means only that no transaction named the IDL account after the deploy. The deploy can keep the interface, so a stale IDL can still be correct. Run `idl-verify` on live data to know.

### Extend check

`ExtendProgram` also sets the slot that the upgradeable loader keeps, with no new code. So when an IDL write is older than that slot, `stale` reads what wrote the slot. It lists the successful transactions that name the ProgramData account, from that slot down to the oldest IDL write. It reads the loader instructions in each one, also the inner ones of a multisig.

| JSON `extend_check` | Meaning |
|---|---|
| `{"result": "upgrade"}` | An `Upgrade` or a `DeployWithMaxDataLen` wrote the slot. |
| `{"result": "extend", "upgrade_slot": null}` | An `ExtendProgram` wrote the slot, and no upgrade came after the IDL write. The IDL is fresh. |
| `{"result": "extend", "upgrade_slot": <SLOT>}` | An `ExtendProgram` wrote the slot. An IDL account is stale when its last write is before `<SLOT>`, the newest upgrade. |
| `{"result": "unknown"}` | The check stopped at its limit, `getTransaction` gave no transaction (a pruned one), or the history of the ProgramData account ended above the IDL write. The slot can be an extend, and the status is stale. |

- The check reads at most 3 pages of 1000 signatures and 10 transactions, and 2 more requests to start the list at the slot. A fresh program costs 4 RPC requests. A stale program with one IDL account costs about 8.
- The human report adds `(an ExtendProgram; ...)` or `(can be an ExtendProgram)` after the deploy slot. Loader v4 has no extend. `extend_check` is not in the report when the check did not run.

### Output of stale

`stale` exits 0 when each program is fresh. It exits 1 when at least one program is stale or has no on-chain IDL. It exits 2 when at least one program could not be checked, or an option is wrong. A program that fails does not stop the others. The human report gives the deploy slot, and the slot and the status of each IDL account.
`--json` prints one object with the keys `programs`, `counts` and `exit_code`. `programs` holds one object for each program, in the order given. Each has `program` and `status`, and then `deploy_slot`, `extend_check` and `idls`, or `error`. Each `idls` entry has `source`, `account`, `slot` and `status`. `counts` holds the number of programs for each status: `fresh`, `stale`, `no_idl` and `error`. `--markdown` prints the counts, the exit code, a table with one row for each IDL account, and the programs that could not be checked.

## watch

`watch` checks a list of programs for a new deploy or a new on-chain IDL. On each change, it diffs the on-chain IDL with the local IDL that your parser uses, and prints one event. For example: `idl-drift watch --programs programs.txt --state state.json --once --json`.

The programs file has one `<program> <local IDL path> [metadata|anchor]` per line, for example `dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH idls/drift.json anchor`. The third field picks the IDL account: the IDL that the line diffs, and the only account that its stale check counts. A program with both accounts needs it. `#` starts a comment. A relative path is relative to the directory of the programs file. A path cannot hold a space.

Each check reads the deploy slot and the IDL write slots of each line, as [stale](#stale) does. When one of them is not the one in the state, `watch` does these steps:

1. It runs the [extend check](#extend-check) when it applies, and reads the on-chain IDL, if the program has one.
2. It diffs the local IDL (old) with the on-chain IDL (new), with the rules of `diff`.
3. It prints one change event. With `--webhook`, it also POSTs the event as JSON.
4. It keeps the new slots in the state. With `--state`, it writes the state file in one step after each check.

The first check of a line has no state, so it prints an event for the line. A failed RPC request, a local IDL that cannot be read, or a failed POST keeps the old slots for the line. The next check then tries again, so the webhook can get the same event two times. A change event, on one line with `--json`:

```json
{"event": "change", "program": "<PROGRAM>", "idl": "idls/pump.json", "idl_path": "<DIR>/idls/pump.json",
 "previous": {"deploy_slot": 458921440, "idls": [{"source": "anchor", "account": "<ACCOUNT>", "slot": 446492745}]},
 "deploy_slot": 458921441, "extend_check": {"result": "upgrade"}, "status": "stale",
 "idls": [{"source": "anchor", "account": "<ACCOUNT>", "slot": 446492745, "status": "stale"}],
 "onchain": {"source": "anchor", "account": "<ACCOUNT>", "saved": "<DIR>/<PROGRAM>-anchor.json"},
 "diff": {"changes": [], "counts": {"breaking": 0, "dangerous": 0, "additive": 0, "cosmetic": 0}, "highest": null}}
```

- `previous` is null on the first check. `onchain` and `diff` are null when the program has no on-chain IDL. `onchain.saved` is there only with `--save-onchain`.
- `diff.changes` has the shape of `changes` in [JSON output](#json-output). `diff.highest` is the highest tier, or null.
- `deploy_slot`, `extend_check`, `status` and `idls` are as in the JSON output of [stale](#output-of-stale).
- A line that fails prints `error: <program> <idl>: <message>` on stderr. With `--json`, it also prints `{"event": "error", "program", "idl", "idl_path", "error"}` on stdout. Only change events go to the webhook.

`watch --once` exits 0 when each line was checked. It exits 2 when a line or the webhook failed, or the programs file, the state file or an option is wrong. Without `--once`, it runs until you stop it. The state file has `version` 1 and a list of `programs`, each with `program`, `idl` (as the programs file gives it), `deploy_slot` and `idls`. Lines that are no longer in the programs file drop out of it.

## Library

The `idl_drift` crate exposes the same functions as the CLI. An example is in the [crate README](../crates/idl-drift/README.md). `idl_drift::model` holds the IDL model that both tools use, and `idl_drift::layout` calculates zero-copy layouts. `idl_drift::diff::reserved_uses` lists the [reserved bytes](#reserved-bytes) that new fields take, with the byte range of each field. `idl_drift::inspect` holds the functions behind `inspect`. `idl_drift::source` holds `discover`, `discover_with_idls`, `read_idl`, `check` and `check_onchain`. `idl_drift::stale` holds `check`, `observe` and `deploy_slot`, behind `stale` and `watch`. Use `discover_with_idls` for IDLs outside `target/idl`, so that an IDL name finds its crate as in the CLI. Before you take a `source` report as a pass, check `SourceReport::nothing_to_compare`, as the CLI does. The programs file and the state file of `watch` are CLI formats, not library API.

## Limitations

- Shank IDLs (`metadata.origin: "shank"`): idl-drift reads each instruction tag from `discriminant`. Shank accounts have no discriminator, so idl-drift does not find a change in how a program tells its accounts apart. A Shank `vec` with a `size` does not load (exit 2).
- `source` reads only the Anchor layout: `programs/*/` and `target/idl/` under ROOT. For another layout, give each IDL with `--idl`. It finds a `declare_id!` only in the crate root of a `programs/*/` crate and in the files of its root `mod <name>;` items. It does not read a `mod` with `#[path = ..]`. It does not read `#[cfg_attr(.., program)]` as the `#[program] mod`, so an IDL with that module name does not find its crate.
- `diff` compares IDLs only, also with `--onchain`. It cannot see a program that was upgraded on chain with no IDL update. Use [idl-verify](idl-verify.md) to check an IDL against real data.
- `diff` does not report a new type in `types`. It reports a removed type only when a parser still reads it.
- idl-drift cannot read inside some Codama nodes. A change inside one is Breaking on a reachable type, with a message that idl-drift cannot read the encoding. idl-drift does not report a change only to its docs. Both node lists are in [inspect](#inspect).
- Collisions use the first discriminator only, as in Shipstern, also when a later discriminator tells the items apart.
- Collisions need equal bytes. Shipstern compares only as many bytes as a discriminator has. So an account type with discriminator `01` before a type with `0102` at the same offset also decodes the data of the second type. idl-drift does not report this case.
- Legacy instruction names with an acronym: Anchor 0.29 writes the function `set_a_b` as `setAB` and hashes `global:set_a_b`, as Shipstern does. idl-drift hashes `global:set_ab`, so its discriminator is wrong.
- Codama spec 2.x renames the `name` key of each node to `identifier`. idl-drift stops with "Codama spec 2.0.0 is not supported; only spec 1.x can be read" (exit 2).
- A Quasar IDL uses another schema: a type definition has a top-level `kind`, and a field can have a `codec`. An IDL with a top-level `spec`, such as `quasar-idl/1.0.0`, stops `diff` and `inspect` with "IDL spec quasar-idl/1.0.0 is not supported; only Anchor, Shank and Codama IDLs can be read" (exit 2). `source` still reads its `address`.
