# Pre-regeneration guard (Mode 2)

Companion to [shipstern#108](https://github.com/solana-rpc/shipstern/issues/108).

## Where this fits

Shipstern regenerates parsers from Codama IDLs and refreshes cargo-insta schema
snapshots. **Before** that regeneration, classify IDL drift:

| Verdict | Meaning |
|---------|---------|
| `SAFE TO REGENERATE` (exit 0) | No Breaking findings — regenerating parsers / insta is consistent with a non-breaking IDL move (Additive/Dangerous/Cosmetic only, or identical). |
| `BREAKING — DO NOT REGENERATE: N changes` (exit 1) | Layout/dispatch breaks exist — do **not** blindly regenerate; review, bump consumer expectations, or gate the PR. |
| exit 2 | Parse/I/O failure on an IDL artifact — retry / fix the file. |

This is **shipstern#108 step 3** (diff-and-classify) only. Fetching the IDL and running
`cargo insta` remain outside this script.

## Run

```bash
bash shipstern-integration/pre-regen-guard.sh \
  shipstern-integration/idls/limit_order_v1.json \
  shipstern-integration/idls/limit_order_v2.json
```

Under the hood: `cargo run -q -- diff OLD NEW --json`, then map
`report.exit_code()` to the messages above.
