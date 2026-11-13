# idl-verify-fixtures

`sample_tests!` makes one `#[test]` for each fixture file that
[`idl-verify --export-fixtures`](https://github.com/senzenn/idl-drift/blob/main/docs/idl-verify.md#fixture-export)
writes. The test runs a Shipstern parser or a Carbon decoder on the file. A sample that the
parser cannot decode fails by its file name in `cargo test`. The tests read no network.

## Use

| Framework | Export with | Dev-dependencies, plus `idl-verify-fixtures = "0.4"` |
|---|---|---|
| Shipstern | `--fixture-format shipstern` | `shipstern-core = "0.12"`, `shipstern-mock = "0.12"` |
| Carbon | `--fixture-format carbon` | `carbon-core = "2"`, `carbon-test-utils = "2"`, `serde_json = "1"`, and the decoder crate, for example `carbon-pumpfun-decoder = "2"` |

```bash
idl-verify --idl idl.json --samples samples.json --export-fixtures fixtures --fixture-format shipstern
```

```rust
// tests/samples.rs
idl_verify_fixtures::sample_tests! {
    shipstern,
    dir: "fixtures",
    accounts: my_parser::pump::AccountParser,
    instructions: my_parser::pump::InstructionParser,
}
```

For Carbon, write `carbon` in place of `shipstern`. Give the decoder, for example
`carbon_pumpfun_decoder::PumpfunDecoder`, to `accounts` and to `instructions`.

A Shipstern account test passes when the account parser returns `Ok`. A `<signature>_tx.json`
test passes when no instruction gives an error other than `Filtered`, and at least one gives
`Ok`. A Carbon test passes when `decode_account` or `decode_instruction` returns `Some`.

## Keys

| Key | Value |
|---|---|
| `dir` | The fixture directory, from the crate root. The macro ignores other files in it. A missing directory is a compile error. So is a directory with no fixture file, a fixture name that is not UTF-8, or a link that leads nowhere. |
| `accounts`, `instructions` | The parser or decoder value for each kind of file. Give at least one. A file whose kind has no parser is a compile error. |
| `expected` | Optional. A directory with `<file stem>.txt` for each fixture. The test compares the `{:#?}` output of the decoded value with that file. Run the tests with `IDL_VERIFY_FIXTURES_BLESS=1` to write the files. The test reads this file at run time, from the `CARGO_MANIFEST_DIR` that cargo sets. |

## Notes

- The generated code names the framework crates by their usual names, so the test crate needs
  them as dependencies. A renamed dependency (`package = ...` in `Cargo.toml`) does not work.
  This crate has no Shipstern, Carbon or Solana dependency.
- A Shipstern test reads its file with `shipstern-mock`, and the mock never fetches it. A Carbon
  test reads its file as a `carbon-test-utils` `TestAccount` or `TestInstruction`, with
  `serde_json`.
- Each test has the name of its file, without `.json`. A character outside `[A-Za-z0-9_]`
  becomes `_`. A name that starts with a digit gets a `_` in front. If two files give the same
  name, the second gets `_2`. For example: `test _4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf_account ... ok`.
- Each test embeds its file with `include_bytes!`. So the test binary reads no fixture at run
  time, and cargo builds the tests again when a file changes. A proc macro cannot watch a
  directory on stable Rust, so cargo does not see a new file. After you add a file, touch the
  file that calls the macro, or add a `build.rs` with
  `fn main() { println!("cargo:rerun-if-changed=fixtures"); }`.

## License

MIT or Apache-2.0.
