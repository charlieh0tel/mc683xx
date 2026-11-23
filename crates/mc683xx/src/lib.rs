//! The SIM and QSM shared by the 683xx parts (SIMRM, QSMRM), on the
//! CPU32 core's bus, and the interface the host's peripherals plug
//! into.
//!
//! # Examples
//!
//! The modules are a chip's, built with it (`mc683xx-mc68331`'s
//! `Builder`), not on their own.  A host names their pins and says
//! how the SIM comes out of reset.
//!
//! ```
//! use mc683xx_cpu32::frequency::Frequency;
//! use mc683xx_modules::chip_select::ChipSelect;
//! use mc683xx_modules::sim::Configuration;
//! use mc683xx_modules::sim::clock::ClockSource;
//!
//! // A chip select by its pin's name, in any case (MC68331UM 4.8).
//! let select: ChipSelect = "cs3".parse()?;
//! assert_eq!(select.to_string(), "CS3");
//! // DATA[15:0] high, the clock synthesized from the 32.768 kHz
//! // crystal (MC68331UM 4.3): the default.
//! let reference = Frequency::from_hz(32_768);
//! let configuration = Configuration::new(0xffff, ClockSource::synthesizer(reference)?);
//! assert_eq!(configuration, Configuration::default());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod chip_select;
#[doc(hidden)]
pub mod module;
pub mod peripheral;
pub mod pins;
pub mod qsm;
pub mod sim;
