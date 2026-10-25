//! The MC68331: the CPU32 core, the SIM and QSM, and its GPT
//! (MC68331UM).
//!
//! The host wires its peripherals and memory to the chip-select pins with a
//! [`chip::Builder`]; the SIM's decode of the firmware's chip-select
//! registers decides which cycles reach them.
//!
//! # Examples
//!
//! A boot ROM on CSBOOT, the one select that decodes out of reset
//! (SIMRM 7.9.3), holding the reset vectors and a `stop`:
//!
//! ```
//! use mc683xx_cpu32::cpu::Stop;
//! use mc683xx_cpu32::cycles::Cycles;
//! use mc683xx_mc68331::chip::BuildError;
//! use mc683xx_mc68331::chip::Builder;
//! use mc683xx_modules::chip_select::ChipSelect;
//! use mc683xx_modules::sim::Configuration;
//!
//! /// Where the program starts.
//! const START: u32 = 0x400;
//! /// STOP #$2700 (CPU32RM 4, STOP).
//! const STOP: [u8; 4] = [0x4e, 0x72, 0x27, 0x00];
//!
//! fn main() -> Result<(), BuildError> {
//!     // One 2-Kbyte page: the initial SSP and PC, then the program.
//!     let mut rom = vec![0; 2048];
//!     rom[0..4].copy_from_slice(&0x800_u32.to_be_bytes());
//!     rom[4..8].copy_from_slice(&START.to_be_bytes());
//!     rom[START as usize..][..STOP.len()].copy_from_slice(&STOP);
//!     // DATA[15:0] high (a 16-bit boot port), the 32.768 kHz crystal.
//!     let mut builder = Builder::new(Configuration::default());
//!     builder.memory(rom, false, &[ChipSelect::Boot])?;
//!     // The power-on reset: SSP and PC from the vectors.
//!     let mut chip = builder.build();
//!     assert_eq!(chip.cpu().registers.pc, START);
//!     let run = chip.run_for(Cycles(10_000));
//!     assert_eq!(run.stop, Stop::Stopped);
//!     assert_eq!(chip.now(), run.cycles);
//!     Ok(())
//! }
//! ```
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

mod board;
pub mod chip;
pub mod event;
pub mod gpt;
pub mod host;
pub mod state;

#[cfg(test)]
mod tests;
