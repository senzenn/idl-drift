# User guide

Find the case that matches your work, and do its steps. The README tells how to
[install](../README.md#install) the tools and lists the [exit codes](../README.md#exit-codes).

| You maintain | Go to |
|---|---|
| A parser, decoder or indexer that keeps its own copy of an IDL | [Keep your IDL copy correct](#keep-your-idl-copy-correct) |
| An SDK or a client that bundles an IDL | [Check a bundled IDL](#check-a-bundled-idl) |
| A Solana program | [Release a program without breaking users](#release-a-program-without-breaking-users) |
| A Codama IDL made from an Anchor IDL | [Check a converted IDL](#check-a-converted-idl) |
| An indexer in production, with Shipstern or Carbon | [Runtime guard](#runtime-guard) |

## Keep your IDL copy correct

A generated parser reads bytes at fixed positions. When the program changes, a parser built
from an old IDL copy reads the wrong bytes, but nothing fails immediately. For example, a Carbon
decoder returned `None` for about 2 days after Pump added a field
([carbon#194](https://github.com/sevenlabs-hq/carbon/issues/194)).

1. Compare your copy with the IDL on chain. A Breaking change means that the two IDLs disagree
   on bytes that a parser reads. Find out which IDL is current, because an on-chain IDL can be
   older than the deployed code. When the program publishes two IDLs, select one with
   `--source metadata` or `--source anchor`.
2. Check your copy against live data. This step does not need a correct on-chain IDL. To also
   test your parser, add `--decoder`. The [decoder examples](../examples/decoders/README.md)
   do this for a Shipstern parser and a Carbon decoder.
3. Run both checks each night with the nightly workflow in [ci.md](ci.md#example-workflows).

```bash
export IDL_DRIFT_RPC_URL=<your RPC URL>
export IDL_VERIFY_RPC_URL=<your RPC URL>
idl-drift diff --onchain <PROGRAM_ID> idls/my_program.json   # step 1
idl-verify --idl idls/my_program.json                         # step 2
```

When a check fails, get the current IDL from the program team, or from the chain when the
on-chain IDL is current. Then generate your parser again. To accept a finding, such as a field
that you do not read, add it to an [allow file](idl-verify.md#allow-file).

## Runtime guard

The nightly check runs one time each day. Between two runs, a program upgrade can change the
data, and the indexer does not tell you. A Carbon decoder returns `None` for data that it cannot
decode, and the pipeline drops it with no count and no log. The decoder also accepts extra bytes
at the end of an account. Shipstern counts parse errors for all programs together, so the count
does not show which program changed.

1. Wrap the parser or decoder with the `idl-drift-guard` crate. The guard counts each update
   by program, kind and outcome. It reads the IDL, so it also counts the extra bytes that a
   decoder accepts with no error.

   ```rust
   use std::sync::Arc;
   use std::time::Duration;
   use idl_drift_guard::{guard, Guard, Window};

   let pump = Arc::new(
       Guard::from_idl(include_str!("../idls/pump.json"))?
           .alert_after(5, Window::Time(Duration::from_secs(60)), |alert| eprintln!("{alert}")),
   );
   // Shipstern 0.12 to 0.13 (`vixen` for Vixen 0.7):
   Pipeline::new(guard!(shipstern, pump::AccountParser, pump.clone()), handlers);
   // Carbon 2.0:
   pipeline.account(guard!(carbon, PumpfunDecoder, pump.clone()), processor);
   ```

   The wrapper returns each value unchanged. Use one guard for each program. For an IDL with
   no address, or a deploy at a different address, use `Guard::from_idl_for(program_id, json)`.
   The alert callback runs inside the decode call. It must not panic or block, so send the
   alert to a channel.
2. Export the counters. Serve `idl_drift_guard::prometheus([&*pump])` on your metrics
   endpoint. Alert on `idl_drift_guard_updates_total` with an outcome other than `decoded` and
   `filtered`, or use the `alert_after` callback.

The first alert gives the type, length, slot and address of the first miss. Carbon decoders do
not see the slot or the address. [examples/guard](../examples/guard) has working examples. The
[crate README](../crates/idl-drift-guard/README.md) lists each outcome and the limits.

## Check a bundled IDL

An SDK ships with a copy of the IDL. When the program changes, the SDK fails until a new SDK
version comes out. For example, the marginfi SDK fails on some live banks, because its IDL does
not have a new enum variant ([mrgn-ts#1139](https://github.com/mrgnlabs/mrgn-ts/issues/1139)).

Each night, run idl-verify with the bundled IDL against live data:

```bash
idl-verify --idl node_modules/<sdk>/idl/my_program.json --program <PROGRAM_ID>
```

An enum value that the IDL does not have gives a `decode` error with the field and the offset.
An account that the program writes, but the IDL marks read-only, gives an `account_flags` finding.

## Release a program without breaking users

Your release can change the bytes that clients, indexers and SDKs read. They find out when
their decoders fail.

1. Check each pull request with the GitHub Action (see [ci.md](ci.md)), or run the check
   manually. The Action compares the IDL in the pull request with its version on the base
   branch. Breaking changes fail the check. To also stop on changes that break some parsers,
   such as a new account on an instruction, add `--fail-on dangerous`.
2. Before the deploy, measure the effect on live accounts. The report counts the accounts that
   break or need a realloc, and gives the extra rent. See
   [upgrade impact](idl-verify.md#upgrade-impact).
3. After the deploy, publish the new IDL on chain. Explorers and indexers decode with the
   on-chain IDL, so a stale one gives them stale results.

```bash
idl-drift diff old/my_program.json target/idl/my_program.json      # step 1
idl-verify --idl target/idl/my_program.json --upgrade-impact        # step 2
```

## Check a converted IDL

A conversion can change the layout. For example, `npx codama convert` removes the `repr(C)`
padding from a voltr_vault account type. Then each field after the gap is read 7 bytes too
early. Compare the two IDLs, then check the converted IDL against samples:

```bash
idl-drift diff anchor.json codama.json
idl-verify --idl codama.json --samples samples.json
```

For voltr_vault, idl-verify reports `7 of 152 bytes unused` for the account `adaptorAddReceipt`.

## What the tools do not check

- Code that the IDL does not describe, such as a fee address in the program.
- Instructions and accounts that no sampled transaction used.
- Accounts of a Shank program: a Shank IDL does not map its accounts to types.

The full lists are in [idl-drift](idl-drift.md#limitations) and
[idl-verify](idl-verify.md#limitations).
