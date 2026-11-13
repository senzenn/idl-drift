//! One test per file in `tests/fixtures/`, from `idl-verify --export-fixtures
//! --fixture-format carbon`.

use carbon_pumpfun_decoder::PumpfunDecoder;

idl_verify_fixtures::sample_tests! {
    carbon,
    dir: "tests/fixtures",
    accounts: PumpfunDecoder,
    instructions: PumpfunDecoder,
}
