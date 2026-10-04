# truncated_idl

A partial write or a bad copy can cut a JSON file short. The tool must fail with a parse
error rather than panic, so CI can tell a bad file (exit 2) from a Breaking change (exit 1).

Expected: exit 2, with `error: parse ...` on stderr.
