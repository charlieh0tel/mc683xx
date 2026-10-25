//! The SIM and QSM shared by the 683xx parts (SIMRM, QSMRM), on the
//! cpu32 core's bus, and the interface the host's peripherals plug
//! into.
//!
//! # Examples
//!
//! The SIM out of reset: its clock and its decode.
//!
//! ```
//! use mc683xx_cpu32::cycles::Cycles;
//! use mc683xx_cpu32::frequency::Frequency;
//! use mc683xx_modules::chip_select::ChipSelect;
//! use mc683xx_modules::sim::Configuration;
//! use mc683xx_modules::sim::Sim;
//!
//! // DATA[15:0] high, the 32.768 kHz crystal.
//! let sim = Sim::new(Configuration::default(), Cycles::ZERO);
//! // SYNCR out of reset: $3F00, 32.768 kHz x 4 (Y + 1) with Y = 63
//! // (MC68331UM 4.3.2).
//! assert_eq!(sim.clock().frequency(), Frequency::from_hz(32_768 * 4 * 64));
//! // Out of reset only CSBOOT decodes, from address 0 (SIMRM 7.9.3).
//! let selects = sim.chip_selects().at(0x400);
//! assert!(selects.contains(ChipSelect::Boot.index()));
//! ```
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod chip_select;
pub mod module;
pub mod peripheral;
pub mod pins;
pub mod qsm;
pub mod sim;
