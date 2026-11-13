//! The code that `sample_tests!` writes.
//!
//! Locals use `Span::mixed_site()`, so the user's parser expression never sees them. Each test
//! embeds its file, so the test binary reads no fixture at run time.

use std::ffi::OsStr;
use std::path::Path;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote_spanned};
use syn::{Expr, Ident, LitStr};

use crate::input::Input;
use crate::scan::{scan, Fixture, Framework, Kind};

/// The tests for the fixtures that `input` names, with paths from `manifest_dir`.
pub(crate) fn sample_tests(
    input: TokenStream,
    manifest_dir: Option<&OsStr>,
) -> syn::Result<TokenStream> {
    let input: Input = syn::parse2(input)?;
    let root = Path::new(manifest_dir.ok_or_else(|| {
        syn::Error::new(
            Span::call_site(),
            "sample_tests!: CARGO_MANIFEST_DIR is not set; build the tests with cargo",
        )
    })?);
    let dir = &input.dir;
    let fixtures = scan(&root.join(dir.value()), input.framework).map_err(|error| {
        syn::Error::new(
            dir.span(),
            format!("sample_tests!: dir {:?}: {error}", dir.value()),
        )
    })?;
    let mut tests = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        let (parser, key) = match fixture.kind {
            Kind::Account => (&input.accounts, "accounts"),
            Kind::Transaction | Kind::Instruction => (&input.instructions, "instructions"),
        };
        let Some(parser) = parser else {
            return Err(syn::Error::new(
                dir.span(),
                format!(
                    "sample_tests!: {} needs `{key}: <parser>`, because it ends in `{}`",
                    fixture.file,
                    fixture.kind.suffix()
                ),
            ));
        };
        tests.push(test(&input, fixture, parser)?);
    }
    Ok(tests.into_iter().collect())
}

fn test(input: &Input, fixture: &Fixture, parser: &Expr) -> syn::Result<TokenStream> {
    let name = format_ident!("{}", fixture.test);
    let file = &fixture.file;
    let path = fixture.path.to_str().ok_or_else(|| {
        syn::Error::new(
            input.dir.span(),
            format!(
                "sample_tests!: the path {} is not UTF-8",
                fixture.path.display()
            ),
        )
    })?;
    let site = Span::mixed_site();
    // Only `expected` needs `Debug` on the decoded value.
    let (value, check) = match &input.expected {
        Some(dir) => (Ident::new("value", site), check(dir, &fixture.stem)),
        None => (Ident::new("_value", site), TokenStream::new()),
    };
    let body = match input.framework {
        Framework::Shipstern => shipstern(fixture.kind, parser, &value),
        Framework::Carbon => carbon(fixture.kind, parser, &value),
    };
    Ok(quote_spanned! {site=>
        #[::core::prelude::v1::test]
        #[allow(non_snake_case)]
        fn #name() {
            let file: &str = #file;
            let bytes: &[u8] = ::std::include_bytes!(#path);
            #body
            #check
        }
    })
}

/// Reads the embedded file with `shipstern-mock` and runs the parser on it.
fn shipstern(kind: Kind, parser: &Expr, value: &Ident) -> TokenStream {
    let site = Span::mixed_site();
    match kind {
        Kind::Account => quote_spanned! {site=>
            let parser = #parser;
            let account = match ::shipstern_mock::read_account_fixture(bytes) {
                ::std::result::Result::Ok(::shipstern_mock::FixtureData::Account(account)) => account,
                ::std::result::Result::Ok(other) => {
                    ::std::panic!("{}: not an account fixture: {:?}", file, other)
                }
                ::std::result::Result::Err(error) => {
                    ::std::panic!("{}: shipstern-mock cannot read it: {}", file, error)
                }
            };
            let parsed = ::shipstern_mock::futures::executor::block_on(
                ::shipstern_core::Parser::parse(&parser, &account),
            );
            let #value = match parsed {
                ::std::result::Result::Ok(value) => value,
                ::std::result::Result::Err(error) => {
                    ::std::panic!("{}: the parser does not decode the account: {}", file, error)
                }
            };
        },
        Kind::Transaction | Kind::Instruction => quote_spanned! {site=>
            let parser = #parser;
            let fixture = match ::shipstern_mock::read_instructions_fixture(bytes) {
                ::std::result::Result::Ok(::shipstern_mock::FixtureData::Instructions(fixture)) => fixture,
                ::std::result::Result::Ok(other) => {
                    ::std::panic!("{}: not a transaction fixture: {:?}", file, other)
                }
                ::std::result::Result::Err(error) => {
                    ::std::panic!("{}: shipstern-mock cannot read it: {}", file, error)
                }
            };
            let mut values = ::std::vec::Vec::new();
            for ix in &fixture.instructions {
                let parsed = ::shipstern_mock::futures::executor::block_on(
                    ::shipstern_core::Parser::parse(&parser, &ix.into()),
                );
                match parsed {
                    ::std::result::Result::Ok(value) => values.push(value),
                    ::std::result::Result::Err(::shipstern_core::ParseError::Filtered) => {}
                    ::std::result::Result::Err(error) => ::std::panic!(
                        "{}: the parser does not decode the instruction at {:?}: {}",
                        file, ix.ix_index, error
                    ),
                }
            }
            ::std::assert!(
                !values.is_empty(),
                "{}: the parser filters all {} instructions and decodes none",
                file,
                fixture.instructions.len()
            );
            let #value = values;
        },
    }
}

/// Reads the embedded file as a `carbon-test-utils` type and runs the decoder on it.
/// `read_account` and `read_instruction` read a path at run time, so they are not used.
fn carbon(kind: Kind, parser: &Expr, value: &Ident) -> TokenStream {
    let site = Span::mixed_site();
    match kind {
        Kind::Account => quote_spanned! {site=>
            let decoder = #parser;
            let account = match ::serde_json::from_slice::<::carbon_test_utils::TestAccount>(bytes) {
                ::std::result::Result::Ok(account) => account.into(),
                ::std::result::Result::Err(error) => {
                    ::std::panic!("{}: not a carbon-test-utils TestAccount: {}", file, error)
                }
            };
            let #value = ::carbon_core::account::AccountDecoder::decode_account(&decoder, &account)
                .unwrap_or_else(|| ::std::panic!(
                    "{}: the decoder does not decode the account ({} bytes, first bytes {:02x?})",
                    file,
                    account.data.len(),
                    account.data.get(..8).unwrap_or(&account.data)
                ));
        },
        Kind::Transaction | Kind::Instruction => quote_spanned! {site=>
            let decoder = #parser;
            let instruction = match ::serde_json::from_slice::<::carbon_test_utils::TestInstruction>(bytes) {
                ::std::result::Result::Ok(instruction) => instruction.into(),
                ::std::result::Result::Err(error) => {
                    ::std::panic!("{}: not a carbon-test-utils TestInstruction: {}", file, error)
                }
            };
            let #value = ::carbon_core::instruction::InstructionDecoder::decode_instruction(
                &decoder,
                &instruction,
            )
            .unwrap_or_else(|| ::std::panic!(
                "{}: the decoder does not decode the instruction (first bytes {:02x?})",
                file,
                instruction.data.get(..8).unwrap_or(&instruction.data)
            ));
        },
    }
}

/// Compares the `{:#?}` output of `value` with `<dir>/<stem>.txt`, or writes that file when
/// `IDL_VERIFY_FIXTURES_BLESS=1`. The path starts at the run-time `CARGO_MANIFEST_DIR`, which
/// cargo sets, else at the current directory. So a moved crate still finds its files.
fn check(dir: &LitStr, stem: &str) -> TokenStream {
    let txt = format!("{stem}.txt");
    quote_spanned! {Span::mixed_site()=>
        let actual = ::std::format!("{:#?}\n", value);
        let path = ::std::path::PathBuf::from(
            ::std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default(),
        )
        .join(#dir)
        .join(#txt);
        let bless = ::std::env::var("IDL_VERIFY_FIXTURES_BLESS");
        if bless.as_deref() == ::std::result::Result::Ok("1") {
            if let ::std::option::Option::Some(parent) = path.parent() {
                ::std::fs::create_dir_all(parent).unwrap_or_else(|error| {
                    ::std::panic!("{}: cannot create {}: {}", file, parent.display(), error)
                });
            }
            ::std::fs::write(&path, &actual).unwrap_or_else(|error| {
                ::std::panic!("{}: cannot write {}: {}", file, path.display(), error)
            });
        } else {
            let wanted = ::std::fs::read_to_string(&path).unwrap_or_else(|error| ::std::panic!(
                "{}: cannot read {}: {}. Run the tests with IDL_VERIFY_FIXTURES_BLESS=1 to write it",
                file, path.display(), error
            ));
            ::std::assert!(
                wanted.replace("\r\n", "\n") == actual,
                "{}: the decoded value is not the one in {}. Run the tests with \
                 IDL_VERIFY_FIXTURES_BLESS=1 to write it.\n--- expected\n{}--- actual\n{}",
                file, path.display(), wanted, actual
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use quote::quote;

    use super::*;

    struct Crate(PathBuf);

    impl Crate {
        fn new(name: &str, files: &[&str]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "idl-verify-fixtures-expand-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("fixtures")).unwrap();
            for file in files {
                std::fs::write(root.join("fixtures").join(file), "{}").unwrap();
            }
            Self(root)
        }

        fn expand(&self, input: TokenStream) -> Result<String, String> {
            sample_tests(input, Some(self.0.as_os_str()))
                .map(|tokens| tokens.to_string())
                .map_err(|error| error.to_string())
        }
    }

    impl Drop for Crate {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const ADDR: &str = "4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf_account.json";
    const SIG: &str = "5abc_tx.json";

    #[test]
    fn a_missing_directory_is_a_compile_error_that_names_it() {
        let krate = Crate::new("missing", &[]);
        let error = krate
            .expand(quote!(carbon, dir: "tests/fixtures", accounts: D))
            .unwrap_err();
        let path = krate.0.join("tests/fixtures");
        assert_eq!(
            error,
            format!(
                "sample_tests!: dir \"tests/fixtures\": the directory {} does not exist",
                path.display()
            )
        );
    }

    #[test]
    fn a_file_with_no_parser_for_it_is_a_compile_error() {
        let krate = Crate::new("no-parser", &[ADDR, SIG]);
        let error = krate
            .expand(quote!(shipstern, dir: "fixtures", accounts: A))
            .unwrap_err();
        assert!(error.contains("5abc_tx.json"), "{error}");
        assert!(error.contains("`instructions: <parser>`"), "{error}");
    }

    #[test]
    fn bad_arguments_are_compile_errors() {
        let krate = Crate::new("args", &[ADDR]);
        let cases = [
            (
                quote!(vixen, dir: "fixtures", accounts: A),
                "expected `shipstern` or `carbon`",
            ),
            (quote!(carbon, accounts: A), "missing `dir"),
            (quote!(carbon, dir: "fixtures"), "give `accounts: <parser>`"),
            (
                quote!(carbon, dir: "fixtures", accounts: A, accounts: B),
                "`accounts` is given twice",
            ),
            (
                quote!(carbon, dir: "fixtures", decoder: A),
                "unknown key `decoder`",
            ),
        ];
        for (input, message) in cases {
            let error = krate.expand(input).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        let error = sample_tests(quote!(carbon, dir: "fixtures", accounts: A), None).unwrap_err();
        assert!(error.to_string().contains("CARGO_MANIFEST_DIR"), "{error}");
    }
}
