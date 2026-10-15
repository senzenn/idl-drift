# Contributing

## Setup

You need:

- Rust 1.91 or newer to build. This is the `rust-version` in `Cargo.toml`.
- Rust 1.97.0 with `clippy` and `rustfmt` to run the checks, as CI does.
  `rust-toolchain.toml` pins this version, so rustup uses it in the repository.
- Node.js and npm, only to run the reference decoder in `tools/reference`. No test needs it.

```bash
git clone https://github.com/senzenn/idl-drift
cd idl-drift
cargo build --workspace
```

Run the tools from the repository:

```bash
cargo run -p idl-drift -- diff OLD NEW
cargo run -p idl-verify -- --idl IDL --samples SAMPLES
```

## Repository layout

| Path | Content |
|---|---|
| `crates/idl-drift/` | The `idl-drift` crate: IDL model, layouts, diff rules, `inspect`, CLI. |
| `crates/idl-verify/` | The `idl-verify` crate: samples, live sampling, runtime decoder, checks, report, CLI. |
| `fixtures/manual/` | Small old and new IDL pairs, one for each diff rule, and malformed files. |
| `fixtures/real/` | Real on-chain Anchor IDLs. |
| `fixtures/shipstern/` | Codama IDLs from Shipstern's integration tests. |
| `fixtures/live/` | Captured mainnet samples with their IDLs, for `idl-verify`. |
| `integration/scenarios/` | End-to-end diff scenarios. Each has a `NOTES.md`. |
| `integration/run-all.sh` | Runs each scenario, manual pair and fixture through both CLIs, and checks the exit codes. |
| `tools/reference/` | Reference decoder (`@coral-xyz/anchor`) for `idl-verify --reference`. |
| `action.yml`, `scripts/action.sh` | The GitHub Action. See [docs/ci.md](docs/ci.md). |
| `examples/github-actions/` | CI workflows for users that run the action. |
| `.github/workflows/release.yml`, `scripts/package-release.sh` | Build and publish the release binaries on a `v*` tag. |

[docs/architecture.md](docs/architecture.md) describes the parts of each crate.

## Checks

A change must pass each check that CI runs:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash integration/run-all.sh
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

No test needs the network. The RPC tests use a local address that refuses the connection.

Code rules:

- No `unwrap` or `expect` outside tests. Clippy warns on both, and CI makes warnings
  errors. Return a typed error.
- Clippy `pedantic` is on for the workspace.
- Input is untrusted. Use checked arithmetic on sizes and offsets that come from an IDL or
  from sample data. A bad input must give exit 2 with a message, never a panic.
- Keep comments short. Say why, not what.

## Tests

- **One test for each behavior.** Name the test after the behavior, for example
  `codama_conversion_that_drops_padding_is_flagged`.
- **Include a known-bad case.** A test for a rule or a check must show that it fires on bad
  input. Where it matters, also show that it stays silent on the correct input.
- **Change one thing.** A known-bad case changes real or minimal data in one way, and
  asserts that exactly that problem is reported.
- **Assert on the rule, not on the text.** Check the severity and the `kind` (idl-drift) or
  the `check` (idl-verify). Check the message only when the message is the behavior.
- **No near-duplicates.** Do not add a test that another test already covers.
- **Real data stays real.** Do not edit files in `fixtures/live/` or `fixtures/real/`. A test
  can change a copy in memory.

For an end-to-end case, add a pair to `fixtures/manual/` or a scenario to
`integration/scenarios/`, and add a line with the expected exit code to
`integration/run-all.sh`.

## Commits and pull requests

Use [Conventional Commits](https://www.conventionalcommits.org/):
`<type>(<scope>): <description>`. Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`. Common scopes are `diff`,
`inspect`, `layout`, `codama` and `verify`.

A pull request does one thing. Fill in the three headings of the pull request template:
Problem, Summary of Changes and Testing. Add a line to the `[Unreleased]` section of
[CHANGELOG.md](CHANGELOG.md) for each change that users can see.

Do not report a security problem in a public issue. See [SECURITY.md](SECURITY.md).

## Add a diff rule

1. Find how a generated parser reads the bytes that the rule is about. A rule is Breaking
   only when the old parser reads the wrong bytes or cannot route them. See
   [docs/idl-drift.md](docs/idl-drift.md#tiers).
2. Add a variant to `ChangeKind`. Its serialized `snake_case` name is a public, stable rule
   name. Never rename an existing variant.
3. Report the change in the diff rules with a severity, the new `kind`, a `subject` and a
   message. For a change inside a type, follow reachability: a change on a type that no
   parser reads is Cosmetic.
4. Add a test to `crates/idl-drift/tests/` with two IDLs that show the change, and an
   end-to-end case if needed (see [Tests](#tests)).
5. Add the rule to the tier lists and the rule name table in
   [docs/idl-drift.md](docs/idl-drift.md).

## Add a verify check

1. Find a real case: a program, an IDL and a sample that the check must flag. Without a
   real case, explain in the pull request why the check is needed.
2. Add a variant to `Check` in the report part of `idl-verify`. Its serialized name is the
   `check` value in the JSON report. Never rename an existing variant.
3. Implement the check where the other checks run. Give each finding a severity, a subject,
   a message, a hint when the cause is known, and one example address or signature.
4. Add a test with a known-bad sample. When the check uses captured data, add it to the
   `fixtures/live/` tests in `crates/idl-verify/tests/`: change one field of a real sample,
   and assert that the check reports it.
5. Add the check to the checks table in [docs/idl-verify.md](docs/idl-verify.md#checks).
   If it has a limitation, add it to the limitations list.

## Fixtures

The IDLs and samples in `fixtures/` belong to the authors of their programs (see
[License](README.md#license)). Add a fixture only from a public source, and say in the
directory's `README.md` where it came from.

To add a captured samples file, run `idl-verify --save-samples` on a live run. Never commit
an RPC URL or an API key.
