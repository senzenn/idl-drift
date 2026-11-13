//! A Shipstern parser as the decoder under test for `idl-verify --decoder`.

mod parser {
    shipstern_proc_macro::include_shipstern_parser!("../../../fixtures/live/pump_fun/idl.json");
}

fn main() -> std::io::Result<()> {
    idl_verify_shipstern::serve!(
        shipstern,
        parser::pump::AccountParser,
        parser::pump::InstructionParser,
        parser::pump::resolve_event_default
    )
}
