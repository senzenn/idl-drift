# idl-verify-shipstern

Run a generated Shipstern or Vixen parser as the decoder under test of
[`idl-verify --decoder`](https://github.com/senzenn/idl-drift/blob/main/docs/decoder-protocol.md).
It supports Shipstern 0.9 to 0.12 (`include_shipstern_parser!`) and Vixen 0.6 to 0.7
(`include_vixen_parser!`). idl-verify decodes each sample from the IDL, and compares the value with
the value of your parser.

## Use

In the crate that holds your parser, run `cargo add idl-verify-shipstern`. Then add one binary,
`src/bin/verify-decoder.rs`:

```rust
fn main() -> std::io::Result<()> {
    idl_verify_shipstern::serve!(shipstern, my_crate::my_program::AccountParser, my_crate::my_program::InstructionParser)
}
```

```bash
cargo build --bin verify-decoder
idl-verify --idl idl.json --samples samples.json --decoder target/debug/verify-decoder
```

- Vixen: write `vixen` in place of `shipstern`.
- An IDL with no instructions: leave out the instruction parser.
- Events: turn on the `program-events` feature of `shipstern-proc-macro` (or
  `yellowstone-vixen-proc-macro`). Give `my_crate::my_program::resolve_event_default` as a fourth
  argument. Without it, idl-verify skips the events with a note.
- A binary crate, or Vixen (its generated module is private): call the include macro in the binary
  file. Then name the module directly.
- Your crate needs `shipstern-core` (or `yellowstone-vixen-core`) as a direct dependency, because
  the macro names it. This crate has no Shipstern or Vixen dependency. So a crates.io, git or
  vendored copy of each of these versions works.
- After you change the IDL file, touch the file that calls `include_shipstern_parser!`. Cargo does
  not see the IDL change, so it keeps the old parser.

## Details

`serve!` prints each value with `{:?}` and reads that text as JSON, because each generated type
derives `Debug`. For a print form that the reader does not know, the reply gives the text near that
point. A parser error or a panic gives an `error` reply. idl-verify reports it when the IDL decodes
the same bytes. Events go to the parser one at a time. So the code in your `InstructionParser` that
finds events in a transaction (log lines, the self-CPI envelope) does not run. `serve!` runs no
async runtime. It polls each parse one time, and a parse that waits gives an `error` reply.
Generated parsers do not wait.
