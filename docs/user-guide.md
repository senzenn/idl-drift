# User guide

This guide starts from your job, not from the tools. Find the case that matches your work,
and follow its steps. The [README](../README.md#install) tells how to install both tools.

| You maintain | Go to |
|---|---|
| A parser, decoder or indexer that keeps its own copy of an IDL | [Keep your IDL copy correct](#keep-your-idl-copy-correct) |
| An SDK or a client that bundles an IDL | [Check a bundled IDL](#check-a-bundled-idl) |
| A Solana program | [Release a program without breaking users](#release-a-program-without-breaking-users) |
| A Codama IDL made from an Anchor IDL | [Check a converted IDL](#check-a-converted-idl) |

## Keep your IDL copy correct

**The problem.** A generated parser reads bytes at fixed positions. When the program
changes, a parser built from an old IDL copy reads the wrong bytes, and nothing fails at
once. Real cases:

- A Carbon decoder returned `None` for about 2 days after Pump added a field
  ([carbon#194](https://github.com/sevenlabs-hq/carbon/issues/194)).
- A Pump `buy` sent without its last optional argument fails in an Anchor decoder that
  uses the published IDL. The issue has been open since 2025-11-10
  ([pump-public-docs#15](https://github.com/pump-fun/pump-public-docs/issues/15)).
- Shipstern parsers are generated from the caller's own copy of the Codama IDL
  ([shipstern#108](https://github.com/solana-rpc/shipstern/issues/108)). So the copy is
  yours to keep correct.

**Step 1. Compare your copy with the IDL on chain.**

```bash
export IDL_DRIFT_RPC_URL=<your RPC URL>
idl-drift diff --onchain <PROGRAM_ID> idls/my_program.json
```

A Breaking change means that your copy and the published IDL disagree on bytes that a
parser reads. Check which one is current: in a check of 36 widely used mainnet programs that
publish an IDL, 22 had an on-chain IDL that was older than the deployed code. When the
program publishes two IDLs, use `--source metadata` or `--source anchor` to pick one.

**Step 2. Check your copy against live data.** This step does not need a correct on-chain
IDL.

```bash
export IDL_VERIFY_RPC_URL=<your RPC URL>
idl-verify --idl idls/my_program.json
```

A real finding from mainnet, on 2026-10-16, with the pump AMM IDL from its repository:

```text
error: instruction buy_exact_quote_in: fails to decode in 2 of 5 samples: `track_volume._0` at offset 24: needs 1 bytes, 0 left
```

Live transactions leave out the last argument, and the program accepts them. A parser that
reads every argument from the IDL fails on those transactions.

To test your parser itself, not only its IDL, add `--decoder`. The
[decoder examples](../examples/decoders/README.md) do this for a Shipstern parser and a Carbon
decoder.

**Step 3. Run both checks every night.** Use the nightly workflow in
[ci.md](ci.md#example-workflows). A new finding then reaches you before your users report it.

**When a check fails:**

1. Get the current IDL from the program team, or from the on-chain IDL when it is current.
2. Regenerate your parser.
3. For a finding that you accept, such as a field you do not read, add a line to an allow
   file (see [idl-verify](idl-verify.md#allow-file)).

## Check a bundled IDL

**The problem.** An SDK ships with a copy of the IDL. The program changes, and the SDK
fails until a new SDK version comes out. Real cases:

- The marginfi SDK fails on 7 of 188 live banks, because its IDL does not have a new enum
  variant ([mrgn-ts#1139](https://github.com/mrgnlabs/mrgn-ts/issues/1139)).
- A Meteora DLMM SDK marked an account read-only that the program writes, so swap
  simulation failed on some pools ([hummingbot/gateway#639](https://github.com/hummingbot/gateway/issues/639)).

**The check.** Run idl-verify with the bundled IDL against live data, every night:

```bash
idl-verify --idl node_modules/<sdk>/idl/my_program.json --program <PROGRAM_ID>
```

An enum value that the IDL does not have is a `decode` error that names the field and the
offset. An account that the program writes, but the IDL marks read-only, is an
`account_flags` finding.

## Release a program without breaking users

**The problem.** Your release changes the bytes that clients, indexers and SDKs read. They
find out when their decoders fail.

**Step 1. Gate each pull request.** The GitHub Action compares the IDL in the pull request
with its version on the base branch (see [ci.md](ci.md)). Or run it by hand:

```bash
idl-drift diff old/my_program.json target/idl/my_program.json
```

Breaking changes fail the step. Use `--fail-on dangerous` to also stop on changes that break
some parsers, such as a new account on an instruction.

**Step 2. Measure the effect on live accounts before you deploy.**

```bash
idl-verify --idl target/idl/my_program.json --upgrade-impact
```

The report counts the accounts that break or need a realloc, and the extra rent:

```text
  Vault                      5   0        0       0        5      8  40640
extra rent for the 5 accounts that need a realloc: 203200 lamports, ...
```

**Step 3. Publish the new IDL on chain after the deploy.** Explorers and indexers decode with
the on-chain IDL. A stale one gives them stale results.

## Check a converted IDL

**The problem.** A conversion can change the layout. `npx codama convert` removes the
`repr(C)` padding from a voltr_vault account type, so each field after the gap is read 7
bytes too early. A converter also gave Raydium AMM v4 an 8-byte discriminator where the
program uses 1 byte ([codama#979](https://github.com/codama-idl/codama/issues/979)).

**The check.**

```bash
idl-drift diff anchor.json codama.json
idl-verify --idl codama.json --samples samples.json
```

```text
error: account adaptorAddReceipt: 7 of 152 bytes unused in 5 of 5 samples
```

## What the tools do not check

- Code that the IDL does not describe, such as a fee address in the program.
- Instructions and accounts that nobody used in the sampled transactions.
- Accounts of a Shank program: a Shank IDL does not map its accounts to types.

The full lists are in [idl-drift](idl-drift.md#limitations) and
[idl-verify](idl-verify.md#limitations).
