# Decoder examples

Two small programs that run a generated parser as the decoder under test of `idl-verify --decoder`
(see [decoder-protocol.md](../../docs/decoder-protocol.md)). Each program is one `serve!` line of an
adapter crate. Both decode pump_fun (`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`) with
`fixtures/live/pump_fun/idl.json`. Each is a separate Cargo project, outside the idl-drift workspace.

| Directory | Parser | Versions | Adapter |
|---|---|---|---|
| `shipstern/` | Built with `include_shipstern_parser!` from the IDL | Shipstern `=0.12.0` | [idl-verify-shipstern](../../crates/idl-verify-shipstern/README.md) |
| `carbon/` | `carbon-pumpfun-decoder` | Carbon `=2.0.0` | [idl-verify-carbon](../../crates/idl-verify-carbon/README.md) |

## Run

From the repository root (for Carbon, write `carbon` in place of each `shipstern`):

```bash
cargo build --release -p idl-verify
cargo build --manifest-path examples/decoders/shipstern/Cargo.toml
target/release/idl-verify --idl fixtures/live/pump_fun/idl.json \
  --samples fixtures/live/pump_fun/samples-2026-10-16.json \
  --decoder examples/decoders/shipstern/target/debug/shipstern-decoder
```

Both examples print `reference: 12 of 12 accounts and 27 of 27 instructions match, 20 of 20 events
match`. The run exits with code 1, because the report also has 8 errors. These errors are about the
IDL, not the parser: the program changed after its authors wrote this IDL. Only findings of the
`reference` check are about the parser.

To check live data, set `IDL_VERIFY_RPC_URL` and leave out `--samples`. A live run on 2026-10-16
gave the same values for Shipstern, and found two gaps in `carbon-pumpfun-decoder` 2.0.0. It reads
`CollectCreatorFeeEvent` and `DistributeCreatorFeesEvent` without their last field, `quote_mint`.
It does not decode `collect_creator_fee_v2` and `distribute_creator_fees_v2`.

## Use your own program

- Shipstern: in `src/main.rs`, change the IDL path in `include_shipstern_parser!`. Change
  `parser::pump` to the module that the macro makes. The IDL gives its name.
- Carbon: in `Cargo.toml`, replace `carbon-pumpfun-decoder` with the Carbon decoder of your
  program, with its `serde` feature. In `src/main.rs`, replace `PumpfunDecoder` and `CpiEvent` with
  the types of that crate.

The adapter READMEs give the other forms of `serve!`, for example Vixen, or a decoder with no
events. `.github/workflows/examples.yml` builds both examples and fails on any `reference` finding.
It runs on changes to the examples, the fixtures or the crates, and once a week.
