//! Loading images and running init scripts on a CPU32 under debug
//! control, over any [`Cpu32Target`](mc683xx_target::Cpu32Target): a
//! BDM pod's on silicon, or the emulated chip's.
//!
//! - [`Image`]: an image, from S-records ([`srecord`]), an ELF32
//!   big-endian executable ([`elf`]) or raw binary.
//! - [`Image::load`] writes it in words, reads it back and compares,
//!   and sets RPC from its entry point ([`LoadOptions`],
//!   [`LoadReport`]).
//! - [`script`]: an init script, one command a line, that writes
//!   memory and registers and checks or polls memory, as a board's
//!   reset code would before a load: chip selects, the watchdog, the
//!   clock.
//!
//! The target is halted, in background mode, for both.
//!
//! Inputs are in memory: an ELF file and a raw binary as bytes,
//! S-records and a script as text.  The crate opens no files, so a
//! host reads them itself, and a parse error carries no I/O error: the
//! errors are `Clone` and `Eq`.

// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod elf;
mod image;
mod load;
pub mod script;
pub mod srecord;

pub use image::Image;
pub use image::ImageError;
pub use image::Segment;
pub use load::ImageLoadError;
pub use load::LoadOptions;
pub use load::LoadReport;
pub use load::LoadedSegment;
pub use load::OddEdges;
pub use load::Verification;
