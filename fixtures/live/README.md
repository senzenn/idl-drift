# Live mainnet fixtures

Real account and instruction data, captured from Solana mainnet by the Shipstern parity harness (`solana-rpc/shipstern#338`) before 2026-10-04. Used to test `idl-verify` offline, with no RPC.

| Dir | Program | Samples |
|---|---|---|
| `voltr_vault/` | `vVoLTRjQmtFpiYoegx285Ze4gsLJ8ZxgFKVcuvmG1a8` | 16 accounts (Vault, StrategyInitReceipt, Protocol, AdaptorAddReceipt), 20 instructions, 25 log events (7 types) |
| `jupiter_vaults/` | `jupo974WCqAUMD3RtpayTap1me7StQCWtEkBbDb6Ba3` | 20 accounts (7 types), 20 instructions |
| `pump_fun/` | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | 16 events only, 8 log and 8 cpi (3 types) |

Files:

- `idl.json`: the program's Anchor IDL (spec 0.1.0).
- `samples.json`: `{ programId, name, transactions, accounts: [{ address, type, data }], instructions: [{ signature, data, accounts }] }`. `data` is base64. `type` is the harness's own label; don't trust it, classify by discriminator.
- `voltr_vault/idl.js-codama.json`: the same IDL converted by `npx codama convert` (`@codama/nodes-from-anchor` 1.5.6). It drops the implicit `repr(C)` padding of the `zero_copy(unsafe)` type `AdaptorAddReceipt`, so a parser built from it reads 145 of 152 bytes. Use it as the known-bad case: `idl-verify` must flag it. Limits: these samples can only show the bug through bytes used (7 left over). `last_updated_epoch`, the field after the gap, is 0 in all 5 samples, so a value comparison can't show the misread.
- Events were added on 2026-10-14 with `idl-verify --save-samples`. voltr's are clean. `pump_fun/idl.json` is the on-chain IDL from `fixtures/real/`; its `TradeEvent` is 24 bytes shorter than what the program writes, in all 10 samples, log and cpi alike. Use it as the known-bad event case.
- `reference.json`: `@coral-xyz/anchor`'s decode of every sample, for `idl-verify --reference`. Regenerate with `cd tools/reference && npm install && node reference.mjs ../../fixtures/live/<dir>/idl.json ../../fixtures/live/<dir>/samples.json > ../../fixtures/live/<dir>/reference.json`.

The IDLs belong to the authors of their programs. The license of this repository does not cover them.
