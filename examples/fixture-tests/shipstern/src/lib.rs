//! The pump_fun parser that the tests in `tests/samples.rs` check.

pub mod parser {
    shipstern_proc_macro::include_shipstern_parser!("../../../fixtures/live/pump_fun/idl.json");
}
