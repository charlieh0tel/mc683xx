//! Loading images and running init scripts on a CPU32 under debug
//! control, over any [`Cpu32Target`](mc683xx_target::Cpu32Target): a
//! BDM pod's on silicon, or the emulated chip's.
//!
//! - [`image`]: an [`Image`](image::Image), from S-records
//!   ([`srecord`]), an ELF32 big-endian executable ([`elf`]) or raw
//!   binary.
//! - [`load`]: [`Image::load`](image::Image::load) writes it in words,
//!   reads it back and compares, and sets RPC from its entry point.
//! - [`script`]: an init script, one command a line, that writes
//!   memory and registers and checks or polls memory, as a board's
//!   reset code would before a load: chip selects, the watchdog, the
//!   clock.
//!
//! The target is halted, in background mode, for both.

// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod elf;
pub mod image;
pub mod load;
pub mod script;
pub mod srecord;
