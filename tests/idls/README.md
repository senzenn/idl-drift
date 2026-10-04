# Codama IDL pair for `limit_order_v1_to_v2_is_breaking`

Copied from the Shipstern repo, `tests/idls/`:

| File | Program |
|---|---|
| `limit_order_v1.json` | `jupoNjAxXgZ4rjzxzPMP4oxduvQsQtZzyknqvzYNrNu` |
| `limit_order_v2.json` | `j1o2qRpjcyUwEvwtcfhEQefh773ZgjxcVRry7LDqg5X` |

v2 is a successor program rather than an in-place upgrade, but it is the largest real Codama
pair available, so the test uses it to check that the diff stays stable and Breaking.
