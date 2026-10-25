//! Stock chips for MC68331 boards, registered by part number for board
//! files to name.
//!
//! Models of commercial peripherals, each from its maker's datasheet
//! ([PLAN.md, Stock peripherals]); [`docs/peripherals.md`] is their
//! guide.  [`register`] adds them to a [`Registry`] and [`names`] lists
//! them.  Board-specific logic stays with the consumer.
//!
//! # Examples
//!
//! The stock types in a registry, and a board file that names one: a
//! 74x374 latch on CS1 driving a lamp from Q4, low from power-on.
//!
//! ```
//! use std::path::Path;
//!
//! use mc683xx_board::board::Board;
//! use mc683xx_mc68331::chip::Mc68331;
//! use mc683xx_board::description::Description;
//! use mc683xx_board::description::Options;
//! use mc683xx_board::registry::Environment;
//! use mc683xx_board::registry::Registry;
//! use mc683xx_board::signal::Signal;
//! use mc683xx_board::time::Time;
//!
//! let mut registry = Registry::with_builtins();
//! mc683xx_peripherals::register(&mut registry)?;
//! assert!(mc683xx_peripherals::names().any(|name| name == "74x374"));
//!
//! let text = r#"
//! version = 1
//!
//! [chip]
//! type = "mc68331"
//! [[peripheral]]
//! name = "u12"
//! type = "74x374"
//! attach = [{ select = "CS1", role = "write" }]
//! reset = []
//! [[net]]
//! name = "lamp"
//! type = "logic"
//! from = "u12.Q4"
//! to = ["external.lamp"]
//! "#;
//! let description = Description::parse(text, Path::new("."), &Options::default())?;
//! let mut board = Board::<Mc68331>::load(&description, &registry, Environment::default())?;
//! board.run_until(Time(1))?;
//! assert_eq!(board.take_received("lamp").unwrap(), [(Time::ZERO, Signal::Level(false))]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [PLAN.md, Stock peripherals]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#stock-peripherals
//! [`docs/peripherals.md`]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

mod ad569;
mod adc0838;
pub mod at28c64b;
mod buffer;
mod dac;
mod latch;
mod mc144110;
mod mc68681;
mod noise;
mod octal;
mod shift_register;
mod sipo;
#[cfg(test)]
mod spi_mode;

use mc683xx_board::registry::Registry;
use mc683xx_board::registry::RegistryError;

/// The stock types: a name and its registration.
type Stock = (&'static str, fn(&mut Registry) -> Result<(), RegistryError>);

/// Every stock type, by its registered name.
const STOCK: &[Stock] = &[
    (ad569::NAME, ad569::register),
    (adc0838::NAME, adc0838::register),
    (at28c64b::NAME, at28c64b::register),
    (buffer::NAME, buffer::register),
    (latch::NAME_373, latch::register_373),
    (latch::NAME_374, latch::register_374),
    (mc144110::NAME, mc144110::register),
    (mc68681::MC68681, mc68681::register_mc68681),
    (mc68681::SCC68692, mc68681::register_scc68692),
    (sipo::NAME, sipo::register),
];

/// Registers each stock type `registry` does not already have, so a
/// host's own type of the same name takes precedence.
///
/// # Errors
///
/// If a type declares a port twice.
pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    for (name, register) in STOCK {
        if registry.get(name).is_none() {
            register(registry)?;
        }
    }
    Ok(())
}

/// The stock types' names.
pub fn names() -> impl Iterator<Item = &'static str> {
    STOCK.iter().map(|(name, _)| *name)
}
