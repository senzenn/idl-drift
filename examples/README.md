# Examples

`github-actions/` holds the CI workflows for both tools. Setup, secrets and results are in
[docs/ci.md](../docs/ci.md).

`decoders/` holds a Shipstern parser and a Carbon decoder as decoders under test for
`idl-verify --decoder`. See [decoders/README.md](decoders/README.md).

`guard/` holds the same parser and decoder, each wrapped in an `idl-drift-guard` guard, with
tests that feed the live pump_fun samples, and a compile test for Vixen 0.7. See
[crates/idl-drift-guard](../crates/idl-drift-guard/README.md).

`fixture-tests/` runs the same parser and decoder on exported fixture files, one test per
file. See [fixture-tests/README.md](fixture-tests/README.md).
