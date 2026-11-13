# idl-drift

Find the Solana IDL changes that make parsers, indexers and clients read the wrong bytes.

A parser that is generated from an IDL reads each field at a fixed byte offset. When the IDL
does not match the program, the parser reads wrong values and gives no error. This is common.
In a check of widely used mainnet programs on 2026-10-15, 22 of 36 on-chain IDLs were older
than the deployed code.

There are two tools:

- `idl-drift` compares two versions of an IDL. It puts each change in a tier: Breaking,
  Dangerous, Additive or Cosmetic.
- `idl-verify` checks one IDL against real account, instruction and event data. It finds the
  bytes that the IDL does not read, also when the decoded values look correct.

Both tools read Anchor IDLs (0.30 spec and legacy), Codama IDLs (spec 1.x) and Shank IDLs.
Rule names, check names, JSON keys and exit codes are stable.

## Install

You need Rust 1.91 or newer.

```bash
cargo install --locked idl-drift idl-verify
```

Binaries for Linux and macOS (x86_64 and arm64) are on the
[releases page](https://github.com/senzenn/idl-drift/releases).

## Use

### Compare two versions of an IDL

```bash
idl-drift diff old.json new.json
```

```text
  [BREAKING] FIELD changed (Pool#1) via reachable type: feeBps: u16 -> u32 (Borsh layout)
  [DANGEROUS] new account on instruction (swap#2): a parser built from the new IDL reads it by index, ...
  [ADDITIVE] new instruction: collectFees
summary: 1 breaking, 1 dangerous, 1 additive, 0 cosmetic
```

The command exits with code 1 on a Breaking change. Add `--fail-on dangerous` to also fail on
a Dangerous change. Add `--json` or `--markdown` to get the report for a script or a job summary.

### Compare your IDL with the IDL on chain

```bash
export IDL_DRIFT_RPC_URL=<your RPC URL>
idl-drift diff --onchain <PROGRAM_ID> target/idl/my_program.json
```

Put the RPC URL in the environment variable. `ps` and the shell history show a URL that you
give on the command line.

### Check an IDL against live data

```bash
export IDL_VERIFY_RPC_URL=<your RPC URL>
idl-verify --idl idl.json --save-samples samples.json
```

```text
error: account adaptorAddReceipt: 7 of 152 bytes unused in 5 of 5 samples
```

idl-verify reads live data from the program at the IDL's address. To run the same check again
offline, use `--samples samples.json`.

### Check that the tools can read each field

```bash
idl-drift inspect idl.json
```

Each `UNMAPPED:`, `UNDEFINED:` or `UNKNOWN LAYOUT:` line names a part of the IDL that the tools
cannot check. Add `--for shipstern` or `--for carbon` to also list what the code generator of
that framework rejects, changes or drops.

### Measure the upgrade impact before a deploy

```bash
idl-verify --idl new.json --upgrade-impact
```

This reads the live accounts of the program with the new IDL. It counts the accounts that
break or need a realloc, and the extra rent.

### More

| Command | What it does |
|---|---|
| `idl-drift source [ROOT]` | Checks that `declare_id!`, `Anchor.toml` and the IDL give one program id. |
| `idl-drift stale <PROGRAM>...` | Tells if the on-chain IDL is older than the last deploy of the program. |
| `idl-drift watch --programs <FILE>` | Reports each new deploy or on-chain IDL, with a diff against your local IDL. |
| `idl-verify --decoder <CMD>` | Compares the values of your own parser with idl-verify. See [Adapters](https://github.com/senzenn/idl-drift/blob/main/docs/decoder-protocol.md#adapters). |

The `idl-drift-guard` crate counts IDL problems in production. See
[Runtime guard](https://github.com/senzenn/idl-drift/blob/main/docs/user-guide.md#runtime-guard).
Both tools are also libraries: see [idl-drift](https://docs.rs/idl-drift) and
[idl-verify](https://docs.rs/idl-verify) on docs.rs.

## Use in CI

The GitHub Action runs either tool in one step and writes the report to the job summary.

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
- uses: senzenn/idl-drift@v0.4.0
  with:
    idl: idls/my_program.json
```

On a pull request, the Action compares the IDL with its version on the base branch. See
[docs/ci.md](https://github.com/senzenn/idl-drift/blob/main/docs/ci.md) for the inputs, the
allow file and a nightly `idl-verify` job.

## Exit codes

Both tools use the same exit codes.

| Code | Meaning |
|---|---|
| `0` | No finding at or above `--fail-on`. |
| `1` | At least one finding at or above `--fail-on`. For `inspect`: a part that the tools cannot check, or a problem of the `--for` framework. |
| `2` | The tool could not run. For example: a file cannot be read, an option is wrong, an RPC request failed, or nothing was sampled. |

## Limits

- The tools read IDLs, not program source code. They do not find a change that the IDL does
  not show. `idl-drift source` reads only the program id from the source.
- Live sampling sees recent transactions only. It does not check instructions and account
  types that nobody used in that time.
- For Shank IDLs, the tools check instructions only. Shank accounts have no discriminator.

On 2026-10-16, Shipstern 0.12.0 parsers and the Carbon `carbon-pump-swap-decoder` 2.0.0 gave
the same values as idl-verify on live mainnet data.

## Documentation

| Page | Content |
|---|---|
| [User guide](https://github.com/senzenn/idl-drift/blob/main/docs/user-guide.md) | The steps for each use case. |
| [idl-drift reference](https://github.com/senzenn/idl-drift/blob/main/docs/idl-drift.md) | Commands, tiers, rules, allow file, JSON format, limits. |
| [idl-verify reference](https://github.com/senzenn/idl-drift/blob/main/docs/idl-verify.md) | Options, checks, samples file, upgrade impact, limits. |
| [CI](https://github.com/senzenn/idl-drift/blob/main/docs/ci.md) | The GitHub Action. |
| [Decoder protocol](https://github.com/senzenn/idl-drift/blob/main/docs/decoder-protocol.md) | Test your own parser with `--decoder`. |
| [Contributing](https://github.com/senzenn/idl-drift/blob/main/CONTRIBUTING.md), [Architecture](https://github.com/senzenn/idl-drift/blob/main/docs/architecture.md) | Setup, checks and the parts of the code. |
| [Changelog](https://github.com/senzenn/idl-drift/blob/main/CHANGELOG.md), [Security](https://github.com/senzenn/idl-drift/blob/main/SECURITY.md) | Releases, and how to report a security problem. |

## License

The code is licensed under either of
[Apache-2.0](https://github.com/senzenn/idl-drift/blob/main/LICENSE-APACHE) or
[MIT](https://github.com/senzenn/idl-drift/blob/main/LICENSE-MIT), at your option.

The IDLs and samples in `fixtures/` and in the test fixture directories belong to the authors
of their programs. This license does not cover them, and the published crates do not include
them.
