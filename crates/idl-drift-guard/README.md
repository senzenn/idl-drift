# idl-drift-guard

Count the decode misses of a live [Shipstern](https://github.com/solana-rpc/shipstern) parser
or [Carbon](https://github.com/sevenlabs-hq/carbon) decoder, per program, and get an alert at
the first bad data after a program upgrade.

A Shipstern runtime counts parse errors for all programs together, and a Carbon pipeline
drops a decode miss in silence. Both accept bytes at the end of an account that grew. The guard reads
the IDL, so it sees each of these cases.

## Use

```bash
cargo add idl-drift-guard
```

```rust
use std::sync::Arc;
use idl_drift_guard::{guard, Guard, Window};

let pump = Arc::new(
    Guard::from_idl(include_str!("../idls/pump.json"))?
        .alert_after(5, Window::Time(std::time::Duration::from_secs(60)), |alert| eprintln!("{alert}")),
);
```

Then wrap the parser or decoder, in place of the bare one:

| Framework | One line |
|---|---|
| Shipstern 0.12 | `Pipeline::new(guard!(shipstern, pump::AccountParser, pump.clone()), handlers)` |
| Vixen 0.7 | `Pipeline::new(guard!(vixen, pump::InstructionParser, pump.clone()), handlers)` |
| Carbon 2.0 | `.account(guard!(carbon, PumpfunDecoder, pump.clone()), processor)` |

The wrapper returns each value and each error unchanged. A Shipstern wrapper keeps the parser's
`id()` and prefilter. A Carbon wrapper is an `AccountDecoder` and an `InstructionDecoder`, as
far as the decoder is one.

Use one guard per program, shared by its wrappers. `prometheus` sums guards with the same
program and name, because a scrape with a series twice fails.

`from_idl` takes the program id from the IDL's address. For an IDL with no address, or a
deploy at another address (devnet, a fork), use `Guard::from_idl_for(program_id, json)`. With a
wrong program id, each update counts as `filtered`. So the guard checks it: a Shipstern or
Vixen wrapper compares the parser's `program_id()` when it is made, and each wrapper notes a
value that the decoder returns for another program. The first other program goes to
`guard.program_mismatch()` and to one line on stderr.

The macro names the framework crates, so your crate needs them as direct dependencies:
`shipstern-core` (or `yellowstone-vixen-core`), or `carbon-core`, `solana-account` and
`solana-instruction`. This crate has no framework dependency.

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
| `size_mismatch` | The decoder returned a value from data shorter than the type, or no type has this length where the IDL selects types by length. |

`guard.snapshot()` returns the counts. `idl_drift_guard::prometheus([&*pump])` returns them as
Prometheus text:

```text
idl_drift_guard_updates_total{program="6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",name="pump",kind="account",outcome="extra_bytes"} 12
idl_drift_guard_alerts_total{program="6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",name="pump"} 1
```

`alert_after(n, window, callback)` calls the callback when one window holds `n` misses, with
the first miss of the window. A window is a time span (`Window::Time`) or a number of updates
(`Window::Updates`), from the first miss. It alerts once per window. The callback runs inside
the decode call: it must not panic and must not block. Send the alert to a channel.

```text
pump 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P: 3 decode misses; first: account extra_bytes (Global): 1088 bytes, the IDL fixes 1045, starts with 0xa7e8e8b1c86c727f, slot 100, 4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf
```

## What the guard reads from the IDL

`Guard::from_idl` reads the discriminators and the fixed lengths: a size discriminator, the
Borsh size, or the `repr(C)` layout size of a zero-copy account. A type with a string, a vec,
an option or an enum whose variants differ in size has no fixed length, so the guard sees
`short_data` only before its body and never `extra_bytes`.

An account that the program allocates with spare bytes counts as `extra_bytes`;
`expect_len(Kind::Account, "Name", len)` sets its real length. When the length varies at the
end, for example live pump `buy` instructions with 25 and 26 bytes,
`expect_min_len(Kind::Instruction, "buy", 25)` accepts each length from 25 up.

`Guard::new(program_id, name)` makes a guard with no IDL. It counts only what the decoder says.

## Limits

- Carbon decoders do not see the slot or the account address, so a Carbon alert gives the type,
  the length and the first bytes of the miss, but no slot and no key.
- Shipstern parses self-CPI events with the outer instruction, so its event updates count as
  `filtered`. A Carbon decoder gets them as instructions, and the guard counts them as `event`.
- Vixen 0.6 is not supported. Its instruction parsers with `program-events` call
  `resolve_instruction_default(..).ok()`, so an unknown discriminator or short data gives
  `Err(Filtered)`, or `Ok` with no instruction when the transaction logs an event. The guard
  then counts no miss. Vixen 0.7 returns the error.
