//! A CPU32 (683xx) under debug control, as the emulator and a BDM pod
//! on silicon both present it: its addresses, operand sizes and
//! function codes.  `no_std`.

#![no_std]
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

#[cfg(test)]
extern crate std;

pub mod address;
pub mod function_code;
pub mod size;
