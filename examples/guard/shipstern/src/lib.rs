//! A Shipstern parser for pump_fun. The tests wrap it in a guard, as a runtime would:
//!
//! ```ignore
//! let pump = Arc::new(Guard::from_idl(IDL)?);
//! Pipeline::new(guard!(shipstern, parser::pump::AccountParser, pump.clone()), handlers)
//! ```

pub mod parser {
    shipstern_proc_macro::include_shipstern_parser!("../../../fixtures/live/pump_fun/idl.json");
}

pub const IDL: &str = include_str!("../../../../fixtures/live/pump_fun/idl.json");
