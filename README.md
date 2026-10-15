# idl-drift

Two tools that catch a Solana program IDL that would make a generated parser silently
misread bytes:

- **`idl-drift`** diffs two IDL versions and classifies every change by whether a parser
  generated from the old one would misread data after it.
- **[`idl-verify`](crates/idl-verify/)** checks one IDL against real account and
  instruction data: bytes the IDL leaves unused (such as zero-copy padding a converter
  dropped), data it cannot decode, and instructions whose accounts don't match.

Both read Anchor and Codama IDLs, run offline, and share one CI exit-code contract. This
README covers `idl-drift`; [`crates/idl-verify/README.md`](crates/idl-verify/README.md)
covers `idl-verify`.

## Why

Parsers generated from an IDL, such as Shipstern's `include_shipstern_parser!`, bake the
IDL in: accounts are read by position, every instruction must have a minimum account
count, discriminators route the bytes, Borsh field widths are fixed, and enum variants
are numbered by declaration order. None of that is checked at runtime. When the IDL
drifts (a `u16` widens to `u32`, a variant is inserted mid-enum, an account is inserted at
position 3) the old parser still runs and decodes the wrong bytes into a plausible struct,
with no error. `idl-drift` fails CI before that IDL lands.

## Install

```bash
cargo install --path .                    # idl-drift
cargo install --path crates/idl-verify    # idl-verify
```

Or run in place with `cargo run -- <args>` and `cargo run -p idl-verify -- <args>`.

## Usage

### `diff`

```bash
idl-drift diff old.json new.json
idl-drift diff old.json new.json --json
idl-drift diff old.json new.json --fail-on dangerous
```

Either file can be Anchor JSON (0.30 spec or legacy) or Codama `rootNode` JSON; the two
don't have to match. Each format names things its own way (`deposit_vault`, `depositVault`),
so when they differ, names are compared in camelCase, as Codama writes them. Then an
instruction arg that links a struct is compared field by field, at every depth, and an arg
that links an alias is compared as the alias target. Codama inlines only a type with one
use, and Borsh writes a struct as its fields, so the bytes are the same. A tuple struct and
a tuple alias also compare as the same type. Legacy
Anchor IDLs leave discriminators out, so they are computed from the names, the way Anchor
does. The human report lists changes by tier and ends with a summary.
`--json` gives every change with its `severity`, `kind` (the rule, such as
`field_renamed`), `subject` (such as `swap#2` or `Pool#1`) and `message`, plus the four
tier arrays, counts and the exit code. Allow-list a known change in CI by `kind` and
`subject`, not by message text.

For example, `idl-drift diff tests/fixtures/amm_old.json tests/fixtures/amm_new.json`:

```
  [BREAKING] FIELD changed (Pool#1) via reachable type: feeBps: u16 -> u32 (Borsh layout)
  [DANGEROUS] new account on instruction (swap#2): a parser built from the new IDL reads it by index, so it fails on a transaction without it
  [ADDITIVE] new instruction: collectFees
summary: 1 breaking, 1 dangerous, 1 additive, 0 cosmetic
exit code: 1
```

| Exit code | Meaning |
|---|---|
| `0` | No change at or above `--fail-on` (default: Breaking) |
| `1` | At least one change at or above `--fail-on` |
| `2` | A file could not be read or parsed |

With `--json`, a run that exits 2 prints one object to stdout,
`{"error": "<message>", "exit_code": 2}`, and the message to stderr, as `idl-verify` does.
A reader that closes the pipe early, such as `head`, does not change the exit code.

In CI:

```bash
idl-drift diff idl/baseline.json idl/current.json
```

### `inspect`

```bash
idl-drift inspect idl.json
idl-drift inspect idl.json --layout
```

Lists the fields whose byte layout `idl-drift` could not resolve, such as Codama hidden
prefixes, offsets, sentinels, inline enums, or a size prefix around a struct. A change to
such a field can't be classified, and `idl-verify` can't decode it, so run this before
trusting a `diff` on a new IDL. Inline structs, big-endian numbers, `shortU16`, any integer
length prefix, remainder lengths, and tagged, zeroable or remainder options are read.

It also lists padded zero-copy types that Borsh also reads (`SHARED:` lines). A type inside
a `bytemuckunsafe` account sits at `repr(C)` offsets, with implicit padding. When an
instruction argument, an event or a Borsh account reaches the same type, Borsh reads it
without the padding, so one definition cannot describe both reads. Shipstern's converter
rejects this case as `PaddedBorshType`. None of the 170 IDLs in the Shipstern parity corpus
has one: only voltr_vault has padded types in zero-copy memory (`AdaptorAddReceipt`,
`RequestWithdrawVaultReceipt`), and no Borsh read reaches them.

Exit code 1 means at least one unresolved field or one shared padded type.

`--layout` also prints every `bytemuck` and `bytemuckunsafe` type field by field: offset,
size and alignment under SBF `repr(C)`, each implicit gap, and the zero-copy size next to the
Borsh size. For voltr's `AdaptorAddReceipt`: a 7-byte gap before `last_updated_epoch`, 144
bytes against 137. An IDL whose converter dropped `serialization` lists no zero-copy types,
which is the first thing to check when a parser reads too few bytes.

## What counts as breaking

Instructions and types match by name. Within them, accounts (with composite groups
flattened), args, struct fields and enum variants compare by position, because that is how
generated parsers read them. Layout changes only count on types a parser reads: account
types, event types, types named in instruction args, and everything those reference
through fields, variant payloads and aliases.

Three Anchor shapes outside the spec structs are accepted: tuple struct fields written as
bare types, the pre-0.30 `{"kind": "alias"}` type, and `"serialization": {"custom": ...}`,
whose layout is treated as unknown.

| Tier | Meaning | Rules |
|---|---|---|
| **Breaking** | The parser misreads bytes or can't route them. Fails the build. | Program address changed (the parser drops every update from the new address); instruction removed; duplicate instruction name, or two names that differ only by ASCII case (Shipstern gives them one generated helper); instruction, account or event discriminator changed (or its length, as in an Anchor to Quasar swap), including a Codama size discriminator, an event discriminator's offset, and a later constant in an event's discriminator chain; account moved to another position; account removed mid-list; arg added, removed, retyped or moved; unresolvable discriminator collision (same discriminator and account count); account or event type removed; event envelope discriminator or payload offset changed; type kind changed; and, on a reachable type: field added, removed, retyped or moved, enum variant inserted mid-list, removed or moved, variant payload changed, alias target changed, type removed while still referenced, `serialization` or `repr` changed so that a field of a zero-copy account moves (such as a converter dropping `bytemuckunsafe` from a padded type), or on a zero-copy account whose layout can't be computed (such as one that holds an enum) |
| **Dangerous** | Existing positions and tags hold, but the parser must grow or its names change. Review. | Program renamed (the generated module name changes); enum variant appended; account appended to an instruction; optional account removed from the tail; account `writable`, `signer` or `optional` flag changed; a field, arg, account or variant renamed in place (same type, same position, and neither name used elsewhere on the other side, otherwise it moved); discriminator collision carried over; the set of account counts under a shared discriminator changed; `serialization` or `repr` changed with no field moving but the zero-copy size changing (trailing padding) |
| **Additive** | New surface; old parsers are unaffected. | New instruction, account type or event type |
| **Cosmetic** | No byte a parser reads changes. | Any of the layout changes above on an unreachable type; `serialization` or `repr` changed with no zero-copy field moving, on a type only ever read as Borsh, or where no zero-copy layout uses it (a nested type's own `serialization`) |

Only Breaking sets the exit code, unless `--fail-on` lowers the bar (`dangerous`, `additive`
or `cosmetic`).

`serialization` and `repr` changes follow Shipstern's converter: only accounts are cast from
memory, and instruction args and events are always Borsh. Each zero-copy account is read
once with every old `serialization` and `repr` and once with every new one, and the field
offsets are compared. So changes that cancel out, such as a converter that drops `packed`
from an account and from each type inside it, are Cosmetic.

Two rules follow Shipstern's dispatch: instructions that share a discriminator are told
apart by account count, most first, so the same discriminator and count can't be resolved;
and a self-CPI event envelope that changes length (Anchor's 8-byte tag to Pinocchio's
1-byte one) moves where the payload starts.

## Limitations

- **IDL against IDL only.** `idl-drift` can't see a program upgraded on chain without an IDL
  update. `idl-verify` covers that case by checking the IDL against real data.
- **Types added to `types` are not reported**, and a type removed from `types` is only
  reported when something a parser reads still references it.

## Repo layout

| Path | What it is |
|---|---|
| `src/model.rs` | The flat IDL model, and legacy Anchor normalization |
| `src/codama_convert.rs` | Codama `rootNode` to the flat model |
| `src/diff.rs` | Reachability and every classification rule |
| `src/inspect.rs` | The unresolved-field scan, zero-copy layouts and shared padded types |
| `src/main.rs` | The `idl-drift` CLI |
| `crates/idl-verify/` | `idl-verify`: decoder, zero-copy layout, checks and report |
| `tests/diff_tests.rs` | One or more tests per diff rule |
| `fixtures/manual/` | Old and new IDL pairs, one per rule, and two malformed files |
| `fixtures/real/`, `tests/idls/` | Real Anchor and Codama IDLs, including a limit_order v1 to v2 pair |
| `fixtures/live/` | Mainnet account and instruction samples with their IDLs, for `idl-verify` |
| `integration/scenarios/` | End-to-end diff scenarios, each with a `NOTES.md` |
| `integration/run-all.sh` | Runs every scenario, manual pair and fixture through the CLIs and checks exit codes |
| `tools/reference/` | Reference decoder (`@coral-xyz/anchor`) for `idl-verify --reference` |
| `scripts/fetch-idl.sh` | Fetches an on-chain Anchor IDL into `fixtures/real/` |

## Testing

```bash
cargo test --workspace
bash integration/run-all.sh
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

No test needs the network.

## License

The code is licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your
option. The IDLs and samples in `fixtures/`, `tests/idls/` and `shipstern-integration/` belong to their
programs' authors. They are not covered by this license, and the crates do not ship them.
