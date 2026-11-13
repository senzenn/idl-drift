# Fixture test examples

Two small crates that run a parser on the fixture files of `idl-verify --export-fixtures`, with
one `#[test]` per file. Each test file is one `sample_tests!` call of
[idl-verify-fixtures](../../crates/idl-verify-fixtures/README.md).

| Directory | Parser | Versions | Files |
|---|---|---|---|
| `shipstern/` | Built with `include_shipstern_parser!` from the IDL | Shipstern `=0.12.0` | 13 in `fixtures/`, with their `Debug` output in `expected/` |
| `carbon/` | `carbon-pumpfun-decoder` | Carbon `=2.0.0` | 39 in `tests/fixtures/` |

Both use pump_fun (`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`), the IDL
`fixtures/live/pump_fun/idl.json` and the samples `fixtures/live/pump_fun/samples-2026-10-16.json`.
Each is a separate Cargo project, outside the idl-drift workspace.

## Run

From the repository root:

```bash
RPC_ENDPOINT=http://127.0.0.1:9 cargo test --manifest-path examples/fixture-tests/shipstern/Cargo.toml
RPC_ENDPOINT=http://127.0.0.1:9 cargo test --manifest-path examples/fixture-tests/carbon/Cargo.toml
```

The output has one line per file, for example:

```text
test _4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf_account ... ok
```

Each test embeds its file and never calls `shipstern-mock`'s `load_fixture`, so no test can
fetch. `RPC_ENDPOINT` points at a closed port only as a guard.

## The files

The files come from these commands, with `--fixture-format shipstern` and
`--fixture-format carbon`:

```bash
cargo build --release -p idl-verify
target/release/idl-verify --idl fixtures/live/pump_fun/idl.json \
  --samples fixtures/live/pump_fun/samples-2026-10-16.json \
  --export-fixtures examples/fixture-tests/carbon/tests/fixtures --fixture-format carbon
```

- **Shipstern: 12 account files and 1 tx file.** A samples file keeps no whole transaction, so
  the export writes no `<signature>_tx.json`. The one tx file is made by hand from the bytes
  of the first top-level instruction sample (`5DARg...`, a `sell_v2`). It has only that call,
  at `ix_index` `[0, 0]`, and no log lines, because a samples file keeps neither the position
  nor the log. It makes sure that the tx test compiles and runs. A live export (`--rpc`)
  writes the whole file.
- **Carbon: 39 of the 53 files.** The other 14 instruction samples have the 3 discriminators
  that the `discriminator` check of `idl-verify` reports. Neither the IDL nor
  `carbon-pumpfun-decoder` 2.0.0 has them, so their tests fail. They are deleted after the
  export, so that the example passes.

After you change the parser or the samples, write the `Debug` files of the Shipstern example
again with `IDL_VERIFY_FIXTURES_BLESS=1`.

## CI

The `fixture-tests` job of `.github/workflows/examples.yml` exports the files again and fails
when a checked-in file is not the same. The hand-made tx file is not compared. Then it runs
both crates with `RPC_ENDPOINT=http://127.0.0.1:9`.
