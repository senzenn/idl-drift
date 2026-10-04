# map_value_widen

A reachable account field is a Borsh map. Widening its value type from `u64` to `u128`
changes the size of every entry.

Expected: Breaking (`balances: Map<pubkey, u64> -> Map<pubkey, u128>`), exit 1.
