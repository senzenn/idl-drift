//! `idl-drift` diffs two Solana program IDLs and classifies every change as Breaking,
//! Dangerous, Additive or Cosmetic by whether a parser generated from the old IDL would
//! misread data after it.
//!
//! ```no_run
//! use idl_drift::{diff, model::Idl};
//!
//! let old = Idl::from_json(&std::fs::read_to_string("old.json")?)?;
//! let new = Idl::from_json(&std::fs::read_to_string("new.json")?)?;
//! let report = diff(&old, &new);
//! std::process::exit(report.exit_code());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
#![allow(clippy::missing_errors_doc, clippy::module_name_repetitions)]

pub mod allow;
pub mod codama_convert;
pub mod diff;
pub mod inspect;
pub mod layout;
pub mod markdown;
pub mod model;
pub mod onchain;
mod reach;
#[cfg(feature = "rpc")]
pub mod rpc;
pub mod shipstern;

pub use diff::{diff, Change, ChangeKind, Report, Severity};
pub use inspect::{coverage, layouts, CoverageReport, TypeLayout, UnmappedField};
