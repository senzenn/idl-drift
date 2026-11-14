//! One `#[test]` per fixture file that `idl-verify --export-fixtures` writes, for a Shipstern
//! parser or a Carbon decoder. A sample that the parser cannot decode fails by its file name in
//! `cargo test`, and the tests read no network.
//!
//! ```ignore
//! idl_verify_fixtures::sample_tests! {
//!     shipstern,
//!     dir: "fixtures",
//!     accounts: my_parser::pump::AccountParser,
//!     instructions: my_parser::pump::InstructionParser,
//! }
//! ```
//!
//! See [`sample_tests!`] for the Carbon form and the keys.

use proc_macro::TokenStream;

mod expand;
mod input;
mod scan;

/// One `#[test]` per fixture file in a directory.
///
/// ```ignore
/// idl_verify_fixtures::sample_tests! {
///     carbon,
///     dir: "tests/fixtures",
///     accounts: carbon_pumpfun_decoder::PumpfunDecoder,
///     instructions: carbon_pumpfun_decoder::PumpfunDecoder,
/// }
/// ```
///
/// The first word is the framework:
///
/// | Framework | Files | Test passes when |
/// |---|---|---|
/// | `shipstern` | `*_account.json`, `*_tx.json` (`shipstern-mock` 0.12 or 0.13) | the account parser returns `Ok`; for a tx file, no instruction gives an error other than `Filtered`, and at least one gives `Ok` |
/// | `carbon` | `*_account.json`, `*_instruction.json` (`carbon-test-utils` 2.0) | `decode_account` or `decode_instruction` returns `Some` |
///
/// Keys:
///
/// - `dir: "<path>"`: the fixture directory, from the crate root (`CARGO_MANIFEST_DIR`).
///   Other files in it are ignored. A missing directory, one with no fixture file, a fixture
///   name that is not UTF-8 and a link that leads nowhere are compile errors.
/// - `accounts: <expr>`, `instructions: <expr>`: the parser or decoder value for each kind of
///   file. Give at least one. A file whose kind has no parser is a compile error.
/// - `expected: "<path>"` (optional): a directory with `<file stem>.txt` for each fixture. The
///   test compares the `{:#?}` output of the decoded value with that file. Run the tests with
///   `IDL_VERIFY_FIXTURES_BLESS=1` to write the files. The test reads the file at run time,
///   from the `CARGO_MANIFEST_DIR` that cargo sets, else from the current directory.
///
/// Each test is named after its file: characters outside `[A-Za-z0-9_]` become `_`, a name
/// that starts with a digit gets a `_` in front, and a name that is already used gets `_2`.
///
/// The generated code names the user's crates by their usual names, so the test crate needs
/// them as dependencies: `shipstern-core` and `shipstern-mock` for `shipstern`, and
/// `carbon-core`, `carbon-test-utils` and `serde_json` for `carbon`. A renamed dependency
/// (`package = ...` in `Cargo.toml`) does not work.
///
/// Each test embeds its file with `include_bytes!`, so the test binary reads no fixture at run
/// time, and cargo builds the tests again when a file changes. A proc macro cannot watch a directory on stable Rust: after you add a file,
/// touch the file that calls the macro.
#[proc_macro]
pub fn sample_tests(input: TokenStream) -> TokenStream {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR");
    expand::sample_tests(input.into(), manifest_dir.as_deref())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
