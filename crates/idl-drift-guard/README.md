# idl-drift-guard

Count the decode misses of a live [Shipstern](https://github.com/solana-rpc/shipstern) parser or
[Carbon](https://github.com/sevenlabs-hq/carbon) decoder per program. Get an alert at the first bad
data after a program upgrade. A Shipstern runtime counts parse errors for all programs together,
and a Carbon pipeline drops a decode miss in silence. Both accept extra bytes at the end of an
account that grew. The guard reads the IDL, so it sees each of these cases.

## Use

Run `cargo add idl-drift-guard`, and make one guard per program:

```rust
use std::sync::Arc;
use idl_drift_guard::{guard, Guard, Window};

let pump = Arc::new(
    Guard::from_idl(include_str!("../idls/pump.json"))?
        .alert_after(5, Window::Time(std::time::Duration::from_secs(60)), |alert| eprintln!("{alert}")),
);
```

Then wrap each parser or decoder of the program with the same guard, in place of the bare one:

| Framework | One line |
|---|---|
| Shipstern 0.12 | `Pipeline::new(guard!(shipstern, pump::AccountParser, pump.clone()), handlers)` |
| Vixen 0.7 | `Pipeline::new(guard!(vixen, pump::InstructionParser, pump.clone()), handlers)` |
| Carbon 2.0 | `.account(guard!(carbon, PumpfunDecoder, pump.clone()), processor)` |

The wrapper returns each value and each error unchanged. A Shipstern or Vixen wrapper keeps the
parser's `id()` and prefilter. A Carbon wrapper is an `AccountDecoder` and an `InstructionDecoder`,
as far as the decoder is one. The macro names the framework crates, so your crate needs them as
direct dependencies: `shipstern-core` (or `yellowstone-vixen-core`), or `carbon-core`,
`solana-account` and `solana-instruction`. This crate has no framework dependency.

| Call | What it does |
|---|---|
| `Guard::from_idl(json)` | Makes a guard for the program at the IDL's address. |
| `Guard::from_idl_for(program_id, json)` | The same, for an IDL with no address, or a deploy at another address (devnet, a fork). |
| `Guard::new(program_id, name)` | Makes a guard with no IDL. It counts only what the decoder says. |
| `alert_after(n, window, callback)` | Calls the callback with the first miss of a window, when the window holds `n` misses. A window is a time span (`Window::Time`) or a number of updates (`Window::Updates`), from the first miss. The guard alerts once per window. The callback runs inside the decode call, so it must not panic and must not block. Send the alert to a channel. |
| `expect_len(Kind::Account, "Name", len)` | Sets the real length of an account that the program allocates with spare bytes. Without it, the account counts as `extra_bytes`. |
| `expect_min_len(Kind::Instruction, "buy", 25)` | Accepts each length from 25 up, for data whose length varies at the end. Live pump `buy` instructions have 25 or 26 bytes. |
| `guard.snapshot()` | Returns the counts. |
| `idl_drift_guard::prometheus([&*pump])` | Returns the counts as Prometheus text: `idl_drift_guard_updates_total` and `idl_drift_guard_alerts_total`, with the labels `program` and `name` (and `kind`, `outcome` on the updates). It sums guards with the same program and name. |
| `guard.program_mismatch()` | Returns the first other program that the decoder is for: the parser's `program_id()`, or a program for which the decoder returned a value. With a wrong program id, each update counts as `filtered`. The guard then writes one warning on stderr. |

## Counters

Each update gets one outcome, per program and per kind (`account`, `instruction`, `event`):

| Outcome | Meaning |
|---|---|
| `decoded` | The decoder returned a value, and the length fits the IDL (or the IDL fixes none). |
| `filtered` | The update is for another program, or the parser skipped it. |
| `unknown_discriminator` | No type of the IDL has this discriminator. |
| `decode_error` | The decoder failed on data whose discriminator and length fit a type. |
| `short_data` | The decoder failed, and the data is shorter than the type. |
| `extra_bytes` | The decoder returned a value, but bytes follow the type. |
| `size_mismatch` | The decoder returned a value from data shorter than the type. Or no type has this length where the IDL selects types by length. |

The IDL fixes a length with a size discriminator, the Borsh size, or the `repr(C)` layout size of a
zero-copy account. A string, a vec, an option or an enum with variants of different sizes fixes no
length. For such a type, the guard sees `short_data` only before its body, and never `extra_bytes`.

## Limits

- Carbon decoders do not see the slot or the account address. So a Carbon alert gives the type, the
  length and the first bytes of the miss, but no slot and no key.
- Shipstern parses self-CPI events with the outer instruction, so its event updates count as
  `filtered`. A Carbon decoder gets them as instructions, and the guard counts them as `event`.
- Vixen 0.6 is not supported. With `program-events`, its instruction parsers give `Err(Filtered)`
  for an unknown discriminator or short data. When the transaction logs an event, they give `Ok`
  with no instruction. So the guard counts no miss. Vixen 0.7 returns the error.
