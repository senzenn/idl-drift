# Real on-chain Anchor IDLs

Fetched from mainnet with `anchor idl fetch`. `integration/run-all.sh` runs
`idl-drift inspect` on each one and expects each field to resolve.

| File | Program | Format |
|---|---|---|
| `limit_order_v1_onchain.json` | `jupoNjAxXgZ4rjzxzPMP4oxduvQsQtZzyknqvzYNrNu` | legacy Anchor (before 0.30) |
| `limit_order_v2_onchain.json` | `j1o2qRpjcyUwEvwtcfhEQefh773ZgjxcVRry7LDqg5X` | Anchor 0.30 spec |
| `pump_fun_onchain.json` | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | Anchor 0.30 spec |

The `idl-drift` tests use `limit_order_v1_onchain.json` to check the discriminators that
idl-drift calculates for a legacy IDL.

To add one (needs the Anchor CLI and mainnet RPC access):

```bash
./scripts/fetch-idl.sh <PROGRAM_ID> <name>
```

The script writes the program id into `address`, because legacy IDLs do not have it.

The IDLs belong to the authors of their programs. See [License](../../README.md#license).
