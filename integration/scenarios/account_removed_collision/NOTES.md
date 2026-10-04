# account_removed_collision

`place` and `cancel` share a discriminator. In v1, `place` has 3 accounts and `cancel` 2,
so Shipstern's parser tells them apart by account count. v2 drops `oracle` from `place`, so
both have the same discriminator and the same count, and only a hand-written resolver could
route them.

Expected: Breaking (`unresolvable collision`, plus the removed account), exit 1.
