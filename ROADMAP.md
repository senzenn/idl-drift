# Roadmap

This page lists planned work. The order is not a commitment. The limitations of each tool
are in its reference page: [idl-drift](docs/idl-drift.md#limitations) and
[idl-verify](docs/idl-verify.md#limitations).

## Planned

- **Publish to crates.io.** Publish `idl-drift` first, because `idl-verify` depends on it by
  version. Then `cargo install idl-drift` and `cargo install idl-verify` will work.
- **GitHub Action.** An `action.yml` that installs and runs both tools in one step, in place
  of the example workflows in `examples/github-actions/`.
- **`idl-drift diff --onchain <program>`.** Get the deployed IDL (from the Anchor IDL account
  or the Program Metadata program) and compare it with a local IDL.
- **Upgrade impact.** Before a deploy, decode each live account of the program with the new
  IDL. Count the accounts that fail to decode or need a realloc, and estimate the rent for
  the extra bytes.
- **Value sanity checks.** Flag decoded values that cannot be true, such as a fee above
  100%. These find a field read with the wrong type at the correct width, which byte counts
  cannot find.
- **Fixture export.** Write captured samples as test fixtures for generated parsers, such
  as `shipstern-mock` or Carbon decoder tests.
- **Health report.** Run `idl-verify` over many programs on a schedule and publish one
  report.
