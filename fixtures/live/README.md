# Live mainnet fixtures

Real account, instruction and event data from Solana mainnet, with the IDL of each program.
The tests use them to run `idl-verify` offline, with no RPC. The account and instruction
samples came from the Shipstern parity harness (`solana-rpc/shipstern#338`) before
2026-10-04. The events came from `idl-verify --save-samples` on 2026-10-14.

| Directory | Program | Samples |
|---|---|---|
| `voltr_vault/` | `vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8` | 16 accounts (4 types), 20 instructions, 25 log events (7 types) |
| `jupiter_vaults/` | `jupo974WCqAUMD3RtpayTap1me7StQCWtEkBbDb6Ba3` | 20 accounts (7 types), 20 instructions |
| `pump_fun/` | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | 16 events only: 8 log and 8 cpi (3 types) |

## Files

- `idl.json`: the program's Anchor IDL (spec 0.1.0).
- `samples.json`: the samples, in the format in
  [docs/idl-verify.md](../../docs/idl-verify.md#the-samples-file). The extra keys `name`,
  `transactions` and each account's `type` come from the harness. idl-verify ignores them
  and classifies by discriminator.
- `reference.json` (`voltr_vault/` and `jupiter_vaults/` only): `@coral-xyz/anchor`'s decode
  of each sample, for `idl-verify --reference`. To make it again, see
  [reference decoder](../../docs/idl-verify.md#reference-decoder).

## Known-bad cases

- `voltr_vault/idl.js-codama.json` is the same IDL converted by `npx codama convert`
  (`@codama/nodes-from-anchor` 1.5.6). It drops the implicit `repr(C)` padding of the
  `zero_copy(unsafe)` type `AdaptorAddReceipt`, so a parser built from it reads 145 of 152
  bytes. idl-verify must flag it. `last_updated_epoch`, the field after the gap, is 0 in all
  5 samples, so only the unused bytes show the bug, not a value comparison.
- `pump_fun/idl.json` is the on-chain IDL from `fixtures/real/`. Its `TradeEvent` is 24 bytes
  shorter than what the program writes, in all 10 samples, log and cpi. idl-verify must
  flag it.

The IDLs and samples belong to the authors of their programs. See
[License](../../README.md#license).
