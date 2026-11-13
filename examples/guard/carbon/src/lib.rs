//! A Carbon decoder for pump_fun. The tests wrap it in a guard, as a pipeline would:
//!
//! ```ignore
//! let pump = Arc::new(Guard::from_idl(IDL)?);
//! Pipeline::builder().account(guard!(carbon, PumpfunDecoder, pump.clone()), processor)
//! ```

pub const IDL: &str = include_str!("../../../../fixtures/live/pump_fun/idl.json");
