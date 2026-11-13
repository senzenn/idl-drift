# Roadmap

This page lists planned work. The order is not a commitment. The limitations of each tool
are in its reference page: [idl-drift](docs/idl-drift.md#limitations) and
[idl-verify](docs/idl-verify.md#limitations).

## Planned

- **Publish to crates.io.** Publish `idl-drift` first, because `idl-verify` depends on it by
  version. Then `cargo install idl-drift` and `cargo install idl-verify` will work.
- **Value sanity checks.** Flag decoded values that cannot be true, such as a fee above
  100%. These find a field read with the wrong type at the correct width, which byte counts
  cannot find.
- **Health report.** Run `idl-verify` over many programs on a schedule and publish one
  report.
