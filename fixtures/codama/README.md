# Codama IDL excerpts

Small excerpts of official Codama IDLs, copied from the
[codama-js](https://github.com/codama-idl/codama) repository,
`packages/dynamic-client/test/programs/idls/`. They keep only the parts that the tests need.

| File | Source | What the tests check |
|---|---|---|
| `token_2022_transfer_fee.json` | `token-2022-idl.json` | Instructions with two omitted discriminator arguments, such as `discriminator` and `transferFeeDiscriminator` |
| `pmp_accounts.json` | `pmp-idl.json` (Program Metadata) | Accounts with an omitted field and no discriminator node |

These files belong to their authors. The license of this repository does not cover them, as
the [README](../../README.md#license) says.
