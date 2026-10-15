//! `idl-verify` checks a Solana program IDL against real account and instruction data.
//!
//! [`idl_drift`] compares two IDL versions. This crate catches an IDL that is wrong today:
//! it classifies each sample by discriminator, decodes it from the IDL alone, and reports
//! bytes the IDL leaves unused, data it cannot decode, instructions whose accounts don't
//! match, and values that disagree with a reference decoder.
//!
//! ```no_run
//! use idl_drift::model::Idl;
//! use idl_verify::{verify, Samples};
//!
//! let idl = Idl::from_json(&std::fs::read_to_string("idl.json")?)?;
//! let samples = Samples::from_json(&std::fs::read_to_string("samples.json")?)?;
//! let report = verify(&idl, &samples, None);
//! std::process::exit(i32::from(report.exit_code()));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod decode;
pub mod report;
pub mod rpc;

mod classify;
mod decoder;
mod error;
mod reference;
mod samples;
#[cfg(test)]
mod testing;
mod text;
mod verify;

pub use idl_drift::layout;

pub use decoder::run_decoder;
pub use error::Error;
pub use reference::Reference;
pub use report::{Finding, Report, SamplingSummary, Severity};
pub use samples::{AccountSample, EventSample, EventSource, InstructionSample, KeyFlags, Samples};
pub use verify::verify;
