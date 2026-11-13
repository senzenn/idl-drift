//! A Carbon decoder as the decoder under test for `idl-verify --decoder`.

fn main() -> std::io::Result<()> {
    idl_verify_carbon::serve!(
        carbon_pumpfun_decoder::PumpfunDecoder,
        events = carbon_pumpfun_decoder::instructions::CpiEvent
    )
}
