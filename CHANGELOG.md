# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - Unreleased

The first release of `idl-drift` and `idl-verify`.

### Added

#### idl-drift

- `idl-drift diff OLD NEW` puts each change in one of four tiers: Breaking, Dangerous,
  Additive or Cosmetic. It reads Anchor (0.30 spec and legacy) and Codama `rootNode` IDLs,
  and the two files can use different formats.
- Layout changes count only on types that a parser reads. Zero-copy `serialization` and
  `repr` changes are judged by field offsets. Discriminator collisions are judged by account
  count, as Shipstern dispatches them.
- `--fail-on <tier>`, and `--json` with a stable rule name (`kind`) and `subject` for each
  change.
- `idl-drift inspect IDL` lists unresolved fields and padded zero-copy types that Borsh also
  reads. `--layout` prints each zero-copy type field by field.

#### idl-verify

- `idl-verify --idl IDL --samples SAMPLES` checks an IDL against captured account,
  instruction and event data.
- Live sampling with `--rpc` or `IDL_VERIFY_RPC_URL`, controlled by `--program`, `--max-tx`
  and `--stall`. `--save-samples` saves a live run to run again offline.
- Checks for discriminators, missing types, decode failures, unused bytes, zero-copy
  layouts, size distribution, account counts, fixed addresses, account flags, event
  envelopes, program id and coverage, with hints for unused bytes.
- Value comparison with `--reference` (`@coral-xyz/anchor`), `--decoder` (a decoder under
  test) or `--compare-idl` (a second IDL).
- `--min-coverage`, `--fail-on warning` and `--json`.

#### Both tools

- One exit code contract: 0 when clean, 1 on a finding at or above `--fail-on`, 2 when the
  tool cannot run. With `--json`, an exit 2 prints `{"error": "...", "exit_code": 2}`.
- GitHub Actions examples: `idl-drift` on pull requests and a nightly `idl-verify` run.
