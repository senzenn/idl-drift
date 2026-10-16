# Decoder examples

Two small programs that run a generated parser as the decoder under test of
`idl-verify --decoder` (see [decoder-protocol.md](../../docs/decoder-protocol.md)). idl-verify
sends each sample to the parser and compares each value with its own decode of the same bytes.

| Directory | Parser | Versions |
|---|---|---|
| `shipstern/` | Built with `include_shipstern_parser!` from the IDL | Shipstern `=0.12.0` |
| `carbon/` | `carbon-pumpfun-decoder` | Carbon `=2.0.0` |

Both decode pump_fun (`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`) with
`fixtures/live/pump_fun/idl.json`. Each is a separate Cargo project, outside the idl-drift
workspace.

## Run

From the repository root:

```bash
cargo build --release -p idl-verify
cargo build --manifest-path examples/decoders/shipstern/Cargo.toml
target/release/idl-verify --idl fixtures/live/pump_fun/idl.json \
  --samples fixtures/live/pump_fun/samples-2026-10-16.json \
  --decoder examples/decoders/shipstern/target/debug/shipstern-decoder
```

For Carbon, use `examples/decoders/carbon/Cargo.toml` and
`examples/decoders/carbon/target/debug/carbon-decoder`.

Both examples print this line:

```text
reference: 12 of 12 accounts and 27 of 27 instructions match, 20 of 20 events match
```

The run exits with code 1, because the report also has 8 errors. These errors are about the
IDL, not about the parser: the program changed after this IDL was written. For example, the
IDL reads 115 of the 166 bytes of a `BondingCurve` account. A parser built from this IDL has
the same gaps. Only findings of the `reference` check are about the parser.

To check live data, set `IDL_VERIFY_RPC_URL` and leave out `--samples`. A live run on
2026-10-16 (400 transactions) gave the same values for Shipstern. For Carbon, it found two
gaps in `carbon-pumpfun-decoder` 2.0.0:

- It reads `CollectCreatorFeeEvent` and `DistributeCreatorFeesEvent` without their last
  field, `quote_mint`.
- It does not decode `collect_creator_fee_v2` and `distribute_creator_fees_v2`.

## Use your own program

Shipstern:

1. In `src/main.rs`, change the IDL path in `include_shipstern_parser!`.
2. Change `parser::pump` to the module that the macro makes. Its name comes from the IDL.

Carbon:

1. In `Cargo.toml`, replace `carbon-pumpfun-decoder` with the Carbon decoder of your program.
2. In `src/main.rs`, replace `PumpfunDecoder` and `CpiEvent` with the types of that crate.

## Value shapes

- Shipstern: the generated types implement serde only when the crate that holds them has a
  `serde` feature. This example turns it on by default. The parser writes a value as
  `{"<kind>": {"<Variant>": ...}}`, and instruction and event arguments are under `args`.
- Carbon: an event has no accounts, so the example decodes events with `CpiEvent::decode`,
  after Anchor's event tag. Carbon keeps an Anchor struct argument, such as `params`, as one
  field. The example removes that wrapper.

## CI

`.github/workflows/examples.yml` builds both examples and fails on any `reference` finding. It
runs on changes to the examples, the fixtures or the crates, and once a week, so a Shipstern
or Carbon release that breaks an example shows up within a week.
