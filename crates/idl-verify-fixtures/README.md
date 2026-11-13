# idl-verify-fixtures

`sample_tests!` makes one `#[test]` for each fixture file that
[`idl-verify --export-fixtures`](https://github.com/senzenn/idl-drift/blob/main/docs/idl-verify.md#fixture-export)
writes. The test runs a Shipstern parser or a Carbon decoder on the file. A sample that the
parser cannot decode fails by its file name in `cargo test`. The tests read no network.

## Shipstern

```bash
idl-verify --idl idl.json --samples samples.json \
  --export-fixtures fixtures --fixture-format shipstern
```

```toml
[dev-dependencies]
shipstern-core = "0.12"
shipstern-mock = "0.12"
idl-verify-fixtures = "0.4"
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

An account test passes when the account parser returns `Ok`. A `<signature>_tx.json` test
passes when no instruction gives an error other than `Filtered`, and at least one gives `Ok`.
The tests read the embedded file with `shipstern-mock`, so the mock never fetches it.

## Carbon

```bash
idl-verify --idl idl.json --samples samples.json \
  --export-fixtures tests/fixtures --fixture-format carbon
```

```toml
[dev-dependencies]
carbon-core = "2"
carbon-pumpfun-decoder = "2"
carbon-test-utils = "2"
serde_json = "1"
idl-verify-fixtures = "0.4"
```

```rust
// tests/samples.rs
idl_verify_fixtures::sample_tests! {
    carbon,
    dir: "tests/fixtures",
    accounts: carbon_pumpfun_decoder::PumpfunDecoder,
    instructions: carbon_pumpfun_decoder::PumpfunDecoder,
}
```

A test passes when `decode_account` or `decode_instruction` returns `Some`. The tests read
the embedded file as a `carbon-test-utils` `TestAccount` or `TestInstruction`, with
`serde_json`.

## Keys

| Key | Value |
|---|---|
| `dir` | The fixture directory, from the crate root. Other files in it are ignored. A missing directory, one with no fixture file, a fixture name that is not UTF-8 and a link that leads nowhere are compile errors. |
| `accounts`, `instructions` | The parser or decoder value for each kind of file. Give at least one. A file whose kind has no parser is a compile error. |
| `expected` | Optional. A directory with `<file stem>.txt` for each fixture. The test compares the `{:#?}` output of the decoded value with that file. Run the tests with `IDL_VERIFY_FIXTURES_BLESS=1` to write the files. The test reads this file at run time, from the `CARGO_MANIFEST_DIR` that cargo sets. |

The generated code names the crates of the framework by their usual names, so the test crate
needs them as dependencies: `shipstern-core` and `shipstern-mock`, or `carbon-core`,
`carbon-test-utils` and `serde_json`. A renamed dependency (`package = ...` in `Cargo.toml`)
does not work. This crate has no Shipstern, Carbon or Solana dependency.

Each test embeds its fixture file, so the test binary reads no fixture at run time.

## Test names

Each test is named after its file, without `.json`. A character outside `[A-Za-z0-9_]`
becomes `_`, and a name that starts with a digit gets a `_` in front. If two files give the
same name, the second gets `_2`. For example:

```text
test _4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf_account ... ok
test E98AtWDa8whooJTw8nbbfz4oifvTcK7VxfaLb2uV2uEG_account ... FAILED
```

## New files

Each test embeds its file with `include_bytes!`, so cargo builds the tests again when a file
changes. A proc macro cannot watch a directory on stable Rust, so cargo does not see a new
file. After you add a file, touch the file that calls the macro, or add this `build.rs`:

```rust
fn main() {
    println!("cargo:rerun-if-changed=fixtures");
}
```

## License

MIT or Apache-2.0.
