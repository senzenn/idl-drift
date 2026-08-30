# idl-drift

Semantic drift detection for Solana program IDLs. Diffs two IDL versions and
flags the changes that would make a generated parser silently misread bytes.
Shipstern-compatible.

## The problem

Solana programs get upgraded. Their IDLs change. Parsers generated from the
old IDL keep running.

Yellowstone Shipstern generates account, instruction, and event parsers from a
Codama IDL (`include_shipstern_parser!`). The generated code bakes in assumptions
about the IDL it was built from:

- **accounts are read positionally** - `ix.accounts[0]`, `[1]`, `[2]`
- **`check_min_accounts_req(len, N)`** gates every instruction on a fixed count
- **discriminator tag-dispatch** routes bytes to the right variant
- **Borsh field widths** are fixed at codegen time
- **enum variants are tag-indexed** by declaration order

None of that is re-checked at runtime. So when the IDL drifts - a `u16` widens
to `u32`, a variant is inserted mid-enum, an 8-byte Anchor event envelope
becomes a 1-byte Pinocchio one, an account is inserted at position 3 - the
stale parser still compiles and still runs. It just decodes the wrong bytes
into the right-looking struct. No panic, no error, no alert. Corrupt data
downstream.

This is the failure class described in
[solana-rpc/shipstern#108](https://github.com/solana-rpc/shipstern/issues/108).

`idl-drift` catches it before the merge. It diffs two IDL documents and
classifies every change by whether a Shipstern-generated parser would misread data
after it, then exits nonzero if any change is parser-breaking.

## How it works

Three stages, in `src/`:

**1. Parse and normalize** (`model.rs`, `codama_convert.rs`)

`Idl::from_json` sniffs the input shape:

- `kind: "rootNode"` goes through `codama-nodes` (pinned to `=0.9.1`, the same
  crate and version Shipstern's `include_shipstern_parser!` deserializes), then
  `codama_convert::from_codama` flattens the node graph into the internal
  model. Discriminator bytes are decoded out of base16/base58/base64
  `bytesValueNode` encodings, so the same discriminator written two ways
  compares equal instead of producing a false Breaking.
- Anything else is treated as Anchor / Solana IDL-spec JSON.
  `normalize_legacy_anchor` coerces older `anchor idl fetch` output into the
  current spec shape: top-level `name`/`version` into `metadata`, `isMut` and
  `isSigner` into `writable`/`signer`, `publicKey` into `pubkey`,
  `{"defined": "Foo"}` into the object form, inline account and event field
  lists promoted into `types[]`, bare tuple-struct field lists given `_0`,
  `_1` names.

Both paths land in the same flat model. Every optional field is
`#[serde(default)]` and the structs derive `PartialEq`, so an IDL that omits
`writable: false` and one that writes it explicitly compare **equal** rather
than producing spurious diffs.

**2. Reachability** (`diff::reachable_types`)

Roots are every account type name, every event type name, and every `Defined`
reference in an instruction argument (including generic arguments). That set
is then closed transitively through the fields of reached structs.

Layout changes to types outside this set are downgraded to Cosmetic - they are
dead weight in the IDL and no parser reads them. See
[Limitations](#limitations) for where this traversal currently stops short.

**3. Classify** (`diff.rs`)

Positional comparison throughout, because that is what the generated parsers
do. Instructions match by name; within an instruction, accounts are flattened
through composite groups and then compared by index, and args are compared by
index. Types match by name; struct fields and enum variants compare by index.

Two rules model Shipstern's dispatch specifically:

- **Event envelopes.** Shipstern's `cpi_event_discriminator` and
  `cpi_event_payload_offset` describe the self-CPI wrap around an event
  payload. An envelope length change (Anchor's 8-byte event-instruction tag
  to Pinocchio's 1-byte) moves where payload decoding starts.
- **Discriminator collisions.** Shipstern's `InstructionParser` disambiguates
  instructions sharing a discriminator by flattened account count, most
  accounts first. Same discriminator plus same account count is unresolvable
  without a hand-written `InstructionResolver`.

## Install

```bash
cargo install --path .
```

Or run in place with `cargo run -- <args>`.

## Usage

### `diff`

```bash
idl-drift diff old.json new.json
idl-drift diff old.json new.json --json
```

Accepts Codama `RootNode` JSON (the shape Shipstern loads) or Anchor /
Solana-IDL-spec JSON, in either position. The two files do not have to be the
same format.

Human output groups findings by tier and ends with a summary line. `--json`
emits the same findings as four string arrays plus counts and the exit code,
for CI to parse.

**Exit codes** (oasdiff-style CI contract):

| Code | Meaning |
|------|---------|
| `0`  | No breaking changes |
| `1`  | At least one `Breaking` change |
| `2`  | I/O or parse error |

Wire into CI:

```bash
idl-drift diff idl/baseline.json idl/current.json
```

### `inspect`

```bash
idl-drift inspect idl.json
```

Parses one IDL and reports Codama shapes that fell through to
`Type::Generic` - fields whose byte layout the converter could not resolve.
Those are blind spots: a change to an unmapped field cannot be classified
correctly. Use it to check that a new IDL is fully understood before trusting
a `diff` result on it.

Scans every instruction argument, plus the fields of every reachable type.

| Code | Meaning |
|------|---------|
| `0`  | Every scanned field resolved to a concrete layout type |
| `1`  | At least one unmapped (`Generic`) field |
| `2`  | I/O or parse error |

## Severity tiers

| Tier | Meaning | Rules |
|------|---------|-------|
| **Breaking** | Parser will misread bytes or fail to route. Fail the build. | Instruction removed; duplicate instruction name; instruction/account/event discriminator changed or length-changed (scheme swap); account changed at a fixed position; account removed mid-list; arg added, removed, or changed; unresolvable discriminator collision (same disc, same account count); account or event type removed; event envelope discriminator changed or length-changed; event envelope payload offset changed; type kind changed; and, on a reachable type: field added/removed/changed (`u16`->`u32`), enum variant inserted mid-list or removed or renamed at an index, type-alias target changed (`Amount=u64`->`u32`), serialization mode changed (bytemuck adds padding borsh lacks) |
| **Dangerous** | Existing tags and positions intact, but parsers must grow. Review. | Enum variant *appended* (new tag only); new account appended to an instruction (`check_min_accounts_req` expects more); optional account removed from the tail; pre-existing discriminator collision carried forward; disambiguation order changed (account-count set shifted under a shared discriminator) |
| **Additive** | Purely additive surface; old parsers unaffected. | New instruction; new account type; new event type |
| **Cosmetic** | No parser impact (unreachable / dead types). | Field, variant, alias, or serialization change on a type not reachable from any instruction arg, account, or event |

Only `Breaking` affects the exit code.

## idl-drift vs. `deserialize_checked`

These are complementary halves of the same chain, not competitors
([shipstern#108](https://github.com/solana-rpc/shipstern/issues/108)).
Shipstern ships a first-party `deserialize_checked` that alerts at **runtime**
when a live IDL no longer matches the compiled parser. `idl-drift` is the
**pre-merge** half: offline, no RPC, fail CI before a breaking IDL lands.

| | **idl-drift** | **`deserialize_checked` (Shipstern)** |
|---|---|---|
| When | CI / pre-merge | Runtime / post-deploy |
| Input | Two local IDL JSON files | Live IDL vs compiled parser |
| Network | Offline | Needs the IDL source at decode time |
| Failure mode | Exit 1 - block the merge | Alert on a stale-IDL transaction |

## Repo layout

| Path | What it is |
|---|---|
| `src/model.rs` | Flat IDL model, shape sniffing, legacy-Anchor normalization |
| `src/codama_convert.rs` | Codama `RootNode` to flat model |
| `src/diff.rs` | Reachability + all classification rules |
| `src/inspect.rs` | `Type::Generic` coverage scan |
| `src/main.rs` | CLI, output formatting, exit codes |
| `tests/diff_tests.rs` | 26 unit tests, one or more per rule |
| `fixtures/manual/` | 12 old/new pairs (one per rule) + 2 malformed-input fixtures |
| `fixtures/real/`, `tests/idls/` | Real on-chain and Codama IDLs, incl. a true limit_order v1 -> v2 pair |
| `integration/scenarios/` | 6 end-to-end scenarios with expected exit codes and a `NOTES.md` each |
| `integration/run-all.sh` | Runs those scenarios and asserts the exit codes |
| `shipstern-integration/` | Scaffolding to run against Shipstern's own IDL corpus (JSON gitignored, no network in scripts) |
| `proto/` | The awk prototype and its fixtures. See [Oracle](#oracle) |
| `scripts/` | `anchor idl fetch` helper, real-IDL inspect driver, manual-fixture driver |

## Testing

```bash
cargo test                    # 26 unit tests
bash integration/run-all.sh   # 6 scenarios, asserts exit codes
bash scripts/manual-test.sh   # 12 manual fixture pairs
```

## Limitations

**Declared-IDL drift only.** `idl-drift` compares two IDL documents. It cannot
detect IDL-vs-deployed-bytecode divergence.

A program can be upgraded on-chain without updating its IDL account. The
validator (Agave) executes bytecode, not IDLs - so a parser built from a stale
IDL will happily decode live transactions against a different layout, and this
tool will not see it.

File-first (diff two local JSON files) is intentional: some real-world IDLs
aren't on-chain or public - they're built from program source - so an RPC-only
tool would miss part of a maintainer's fleet (shipstern#108).

**Known gaps in reachability.** The transitive closure walks struct fields
only. Three cases currently under-report:

1. A type referenced **only through an enum variant payload** is not marked
   reachable. A real Borsh width change inside it is reported Cosmetic, exit 0.
2. A type referenced **only through a type-alias target** is likewise not
   marked reachable, with the same result.
3. Types **added to or removed from `types[]`** are not reported at all.
   Deleting a type that a reachable field still references leaves a dangling
   `Defined` ref and produces a clean exit 0.

Until these are closed, treat a clean `diff` on an IDL that uses enum payloads
or alias indirection as necessary but not sufficient.

**Pair with other checks.** Transaction-replay validation (fixtures or
historical txs against the generated parser) and Shipstern's `deserialize_checked`
cover the runtime and bytecode classes of failure this tool cannot reach.

## Oracle

Classification rules were ported from the awk prototype in `proto/`
(`proto/diff.awk` plus 59 `.facts` fixture files). That suite is the reference
oracle; the Rust engine is expected to match it case-for-case.

## License

MIT OR Apache-2.0
# idl-drift
