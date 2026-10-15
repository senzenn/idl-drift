# Shipstern Codama IDLs

Codama `rootNode` IDLs copied from the Shipstern repository,
`tests/idls/`. Shipstern's integration tests generate parsers from them with
`include_shipstern_parser!`, so they show the Codama shapes that real parsers read.

| File | Program name | Program |
|---|---|---|
| `chained_cpi_event_envelope.json` | test IDL | no live program; an event envelope chain with three constants |
| `dca.json` | `dca` | `DCA265Vj8a9CEuX1eb1LWRnDT7uK6q1xMipnNyatn23M` |
| `dynamic_bonding_curve.json` | `dynamicBondingCurve` | `dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN` |
| `glow.json` | `margin` | `GLoWMgcn3VbyFKiC2FGMgfKxYSyTJS7uKFwKY2CSkq9X` |
| `invite_escrow.json` | `inviteEscrow` | `inv1tEtSwRMtM44tbvJGNiTxMvDfPVnX9StyqXfDfks` |
| `jupiter_log_scope_regression.json` | `jupiterRegression` | `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` |
| `limit_order_v1.json` | `limitOrder` | `jupoNjAxXgZ4rjzxzPMP4oxduvQsQtZzyknqvzYNrNu` |
| `limit_order_v2.json` | `limitOrder2` | `j1o2qRpjcyUwEvwtcfhEQefh773ZgjxcVRry7LDqg5X` |
| `loopscale.json` | `loopscale` | `1oopBoJG58DgkUVKkEzKgyG9dvRmpgeEm1AVjoHkF78` |
| `metaplex.json` | `tokenMetadata` | `metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s` |
| `okx_labs1.json` | `dexSolana` | `6m2CDdhRgxpH4WjvdzxAYbGxwdGUz5MziiL5jek2kBma` |
| `order_engine.json` | `orderEngine` | `61DFfeTKM7trxYcPQCM78bJ794ddZprZpAwAnLiwTpYH` |
| `perpetuals.json` | `perpetuals` | `PERPHjGBqRHArX4DySjwM6UJHiR3sWAatqfdBS2qQJu` |
| `pump_fun.json` | `pumpFun` | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` |
| `raydium_amm_v4_with_swapv2.json` | `raydiumAmm` | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` |
| `spl_governance.json` | `splGovernance` | `GovER5Lthms3bLBqWub97yVrMmEogzX7xNjdXpPPCVZw` |
| `squads_multisig_program.json` | `squadsMultisigProgram` | `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf` |

## Uses

- `crates/idl-drift/tests/` diffs `limit_order_v1.json` against `limit_order_v2.json`, the
  largest real Codama pair. v2 is a successor program, not an in-place upgrade, so the test
  checks that the diff stays stable and Breaking.
- `integration/run-all.sh` diffs `pump_fun.json` against the on-chain Anchor IDL in
  `fixtures/real/` (exit 1), and the on-chain legacy `limit_order_v1` IDL against
  `limit_order_v1.json` (exit 0: the same program in two formats).

The IDLs belong to the authors of their programs. See [License](../../README.md#license).
