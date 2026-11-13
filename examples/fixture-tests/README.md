# Fixture test examples

Two small crates that run a parser on the fixture files of `idl-verify --export-fixtures`, with one
`#[test]` per file. Each test file is one `sample_tests!` call of
[idl-verify-fixtures](../../crates/idl-verify-fixtures/README.md). Both use pump_fun
(`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`), the IDL `fixtures/live/pump_fun/idl.json` and the
samples `fixtures/live/pump_fun/samples-2026-10-16.json`. Each is a separate Cargo project, outside
the idl-drift workspace.

| Directory | Parser | Versions | Files |
|---|---|---|---|
| `shipstern/` | Built with `include_shipstern_parser!` from the IDL | Shipstern `=0.12.0` | 13 in `fixtures/`, with their `Debug` output in `expected/` |
| `carbon/` | `carbon-pumpfun-decoder` | Carbon `=2.0.0` | 39 in `tests/fixtures/` |

## Run

From the repository root (for Carbon, write `carbon` in place of `shipstern`):

```bash
RPC_ENDPOINT=http://127.0.0.1:9 cargo test --manifest-path examples/fixture-tests/shipstern/Cargo.toml
```

The output has one line per file, for example
`test _4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf_account ... ok`. Each test embeds its file and
never calls the `load_fixture` of `shipstern-mock`, so no test can fetch. `RPC_ENDPOINT` points at
a closed port only as a guard.

## The files

The `fixture-tests` job of `.github/workflows/examples.yml` has the export command. The job exports
the files again, and fails when a checked-in file is not the same. Then it runs both crates.

- Shipstern: 12 account files and 1 tx file. A samples file keeps no whole transaction, so the
  export writes no `<signature>_tx.json`. We made the tx file by hand from the bytes of the first
  top-level instruction sample (`5DARg...`, a `sell_v2`). It has only that call, at `ix_index`
  `[0, 0]`, and no log lines. It makes sure that the tx test compiles and runs. The CI job does not
  compare it. A live export (`--rpc`) writes the whole file.
- Carbon: 39 of the 53 files. The other 14 instruction samples have the 3 discriminators that the
  `discriminator` check of `idl-verify` reports. Neither the IDL nor `carbon-pumpfun-decoder` 2.0.0
  has them, so their tests fail. The example leaves them out, so that it passes.

After you change the parser or the samples, run the Shipstern tests with
`IDL_VERIFY_FIXTURES_BLESS=1`. This writes the `Debug` files again.
