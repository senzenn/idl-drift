# idl-verify-carbon

Run a [Carbon](https://github.com/sevenlabs-hq/carbon) 2.0 decoder as the decoder under test
of [`idl-verify --decoder`](https://github.com/senzenn/idl-drift/blob/main/docs/decoder-protocol.md).
idl-verify decodes each sample from the IDL, and compares the value with the value of your
decoder. The decoder can come from crates.io or from `carbon-cli`.

## Use

In a crate that depends on the decoder crate, turn on the decoder's `serde` feature, run
`cargo add idl-verify-carbon`, and add one binary, `src/bin/verify-decoder.rs`:

```toml
[dependencies]
carbon-core = "2"
carbon-pumpfun-decoder = { version = "2", features = ["serde"] }
solana-account = "4"
solana-instruction = { version = "3", default-features = false }
```

```rust
fn main() -> std::io::Result<()> {
    idl_verify_carbon::serve!(carbon_pumpfun_decoder::PumpfunDecoder, events = carbon_pumpfun_decoder::instructions::CpiEvent)
}
```

```bash
cargo build --bin verify-decoder
idl-verify --idl idl.json --samples samples.json --decoder target/debug/verify-decoder
```

- A decoder crate with no `instructions::CpiEvent`: leave out `events = ..`. idl-verify then
  skips the events with a note.
- Two decoders: `serve!(accounts = a, instructions = i)`, and `events = ..` as a third
  argument if necessary.

The crate needs `carbon-core`, `solana-account` and `solana-instruction` as direct
dependencies, at the versions of the decoder, because the macro names them. This crate has no
Carbon or Solana dependency. `serve!` expands in your crate, against your own crates.

## What the decoder gets

| Sample | Call |
|---|---|
| Account | `decode_account` with the sample's data, owned by the program id. A generated decoder skips an account of another owner. |
| Instruction | `decode_instruction` with the program id, the data, and the account keys as read-only metas that do not sign. |
| Event | `CpiEvent::decode` with Anchor's self-CPI tag, then the event bytes. |

A decoder returns `None` when it does not decode a sample. The reply is then an `error`, so
idl-verify reports each sample that the IDL decodes and the decoder does not. A panic is also
an `error` reply.

## Value shapes

The adapter removes Carbon's serde envelope: `{"type": "<Name>", "data": ..}` for accounts and
instructions, and `{"<Name>": ..}` for events. For an instruction, it sends only the
arguments. It writes an integer above 64 bits as a decimal string.

## Limits

- Carbon's pipeline decodes only self-CPI events. The adapter also sends events from
  `Program data:` log lines to `CpiEvent::decode`, which checks the same layout.
- Values go through JSON text. The default float parser of `serde_json` can change the last
  bit of a float.
