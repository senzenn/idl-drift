# Manual diff fixtures

Small old and new IDL pairs for `idl-drift diff`. Each pair shows one rule.
`integration/run-all.sh` runs each pair and checks the exit code.

The number prefix gives the expected result: `0x` exits 0, `1x` exits 1 (Breaking), and
`9x` is a malformed file that exits 2. `17_disc_reencoded` is the exception: it exits 0.

| Pair | Change | Exit |
|---|---|---|
| `00_identical` | No change. | 0 |
| `01_default_omission` | One file leaves out default values that the other writes. | 0 |
| `02_new_instruction` | A new instruction (Additive). | 0 |
| `03_enum_append` | An enum variant added at the end (Dangerous). Exits 1 with `--fail-on dangerous`. | 0 |
| `10_field_widen` | A field of a reachable type widens from `u16` to `u32`. | 1 |
| `11_enum_mid_insert` | An enum variant inserted before the end, so later tags change. | 1 |
| `12_arg_added` | A new instruction argument. | 1 |
| `13_account_reorder` | Two instruction accounts change places. | 1 |
| `14_disc_len_change` | The discriminator length changes from 8 bytes to 1. | 1 |
| `15_alias_retarget` | A type alias changes from `u64` to `u32`. | 1 |
| `16_collision_introduced` | A new instruction uses an existing discriminator, with the same account count. | 1 |
| `17_disc_reencoded` | The same Codama discriminator bytes, written in base16 and then in base58. | 0 |
| `90_broken_json_old.json` | Not valid JSON. | 2 |
| `91_missing_field_old.json` | A required field is missing. | 2 |

Each file is a minimal IDL written for these tests.
