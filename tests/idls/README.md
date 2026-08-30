# Real Codama IDL fixtures (Shipstern)

Place Yellowstone Shipstern Codama `rootNode` JSON here for regression tests.
These files are **committed when present** so `cargo test` needs no network.

## Required for `limit_order_v1_to_v2_is_breaking`

| File | Raw URL |
|------|---------|
| `limit_order_v1.json` | https://raw.githubusercontent.com/solana-rpc/shipstern/main/tests/idls/limit_order_v1.json |
| `limit_order_v2.json` | https://raw.githubusercontent.com/solana-rpc/shipstern/main/tests/idls/limit_order_v2.json |

Blob (browsable): https://github.com/solana-rpc/shipstern/blob/main/tests/idls/limit_order_v1.json  
(and `…/limit_order_v2.json`)

```bash
# from repo root (manual; not run by CI)
curl -fsSL -o tests/idls/limit_order_v1.json \
  https://raw.githubusercontent.com/solana-rpc/shipstern/main/tests/idls/limit_order_v1.json
curl -fsSL -o tests/idls/limit_order_v2.json \
  https://raw.githubusercontent.com/solana-rpc/shipstern/main/tests/idls/limit_order_v2.json
```

Or copy from a local Shipstern checkout: `cp /path/to/shipstern/tests/idls/limit_order_v{1,2}.json tests/idls/`
