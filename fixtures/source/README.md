# Source fixtures

Small Anchor workspaces for `idl-drift source`, written for these tests. The ids are base58 of
32 made-up bytes, not deployed programs.

- `consistent/`: `declare_id!`, both `Anchor.toml` clusters and the IDL give `amm` the same id. A `//` comment holds an old `declare_id!`.
- `drift/`: `[programs.mainnet]` and the IDL each give `amm` another id than `declare_id!`, and `programs/vault/src/lib.rs` has no `declare_id!`, so the `vault` id in `Anchor.toml` cannot be checked.
