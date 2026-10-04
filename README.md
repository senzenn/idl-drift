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
```

Either file can be Anchor JSON (0.30 spec or legacy) or Codama `rootNode` JSON; the two
don't have to match. Each format names things its own way (`deposit_vault`, `depositVault`),
so when they differ, names are compared in camelCase, as Codama writes them. Struct-typed
instruction args are compared field by field then too, since Codama inlines them. Legacy
Anchor IDLs leave discriminators out, so they are computed from the names, the way Anchor
does. The human report lists changes by tier and ends with a summary;
`--json` gives the same changes as four arrays, with counts and the exit code.

For example, `idl-drift diff tests/fixtures/amm_old.json tests/fixtures/amm_new.json`:

```
  [BREAKING] FIELD changed (Pool#1) via reachable type: feeBps: u16 -> u32 (Borsh layout)
  [DANGEROUS] new account on instruction (swap#2): shifts count/positions -> check_min_accounts_req expects more
  [ADDITIVE] new instruction: collectFees
summary: 1 breaking, 1 dangerous, 1 additive, 0 cosmetic
exit code: 1
```

| Exit code | Meaning |
|---|---|
| `0` | No Breaking change |
| `1` | At least one Breaking change |
| `2` | A file could not be read or parsed |

In CI:

```bash
idl-drift diff idl/baseline.json idl/current.json
```

### `inspect`

```bash
idl-drift inspect idl.json
```

Lists the fields whose byte layout `idl-drift` could not resolve, such as inline structs or
Codama encodings with no Borsh equivalent (big-endian numbers, non-`u32` length prefixes,
zeroable options). A change to such a field can't be classified, and `idl-verify` can't
decode it, so run this before trusting a `diff` on a new IDL. Exit code 1 means at least
one unresolved field.

## What counts as breaking

Instructions and types match by name. Within them, accounts (with composite groups
flattened), args, struct fields and enum variants compare by position, because that is how
generated parsers read them. Layout changes only count on types a parser reads: account
types, event types, types named in instruction args, and everything those reference
through fields, variant payloads and aliases.

| Tier | Meaning | Rules |
|---|---|---|
| **Breaking** | The parser misreads bytes or can't route them. Fails the build. | Instruction removed; duplicate instruction name; instruction, account or event discriminator changed (or its length, as in an Anchor to Quasar swap); account changed at a fixed position; account removed mid-list; arg added, removed or changed; unresolvable discriminator collision (same discriminator and account count); account or event type removed; event envelope discriminator or payload offset changed; type kind changed; and, on a reachable type: field added, removed or changed, enum variant inserted mid-list, removed or renamed, variant payload changed, alias target changed, `serialization` or `repr` changed so that a field of a zero-copy type moves, including a nested type's `repr` (such as a converter dropping `bytemuckunsafe`) |
| **Dangerous** | Existing positions and tags hold, but the parser must grow. Review. | Enum variant appended; account appended to an instruction; optional account removed from the tail; discriminator collision carried over; the set of account counts under a shared discriminator changed; `serialization` or `repr` changed with no field moving but the zero-copy size changing (trailing padding) |
| **Additive** | New surface; old parsers are unaffected. | New instruction, account type or event type |
| **Cosmetic** | No byte a parser reads changes. | Any of the layout changes above on an unreachable type; `serialization` or `repr` changed with no zero-copy field moving, or on a type only ever read as Borsh |

Only Breaking sets the exit code.

Two rules follow Shipstern's dispatch: instructions that share a discriminator are told
apart by account count, most first, so the same discriminator and count can't be resolved;
and a self-CPI event envelope that changes length (Anchor's 8-byte tag to Pinocchio's
1-byte one) moves where the payload starts.

## Limitations

- **IDL against IDL only.** `idl-drift` can't see a program upgraded on chain without an IDL
  update. `idl-verify` covers that case by checking the IDL against real data.
- **Types added to or removed from `types` are not reported.** A dangling reference left by
  a removal goes unnoticed.

## Repo layout

| Path | What it is |
|---|---|
| `src/model.rs` | The flat IDL model, and legacy Anchor normalization |
| `src/codama_convert.rs` | Codama `rootNode` to the flat model |
| `src/diff.rs` | Reachability and every classification rule |
| `src/inspect.rs` | The unresolved-field scan |
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

MIT OR Apache-2.0
