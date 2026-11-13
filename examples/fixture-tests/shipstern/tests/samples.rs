//! One test per file in `fixtures/`, from `idl-verify --export-fixtures --fixture-format
//! shipstern`. Each test compares the parsed value with its file in `expected/`.

use shipstern_fixture_tests::parser::pump;

idl_verify_fixtures::sample_tests! {
    shipstern,
    dir: "fixtures",
    accounts: pump::AccountParser,
    instructions: pump::InstructionParser,
    expected: "expected",
}
