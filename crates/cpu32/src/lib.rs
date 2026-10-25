//! The Motorola CPU32 core (CPU32RM): its instructions, exceptions and
//! bus cycles, and nothing of any chip or board.  A chip builds the
//! core's page tables from its address decode and answers the rest of
//! the bus and the interrupt inputs.
//!
//! # Examples
//!
//! A bare CPU32 on flat memory ([`flat::Machine`]) takes the reset
//! exception and runs MOVEQ #42,D0:
//!
//! ```
//! use mc683xx_cpu32::address::Address;
//! use mc683xx_cpu32::cpu::bare::Limits;
//! use mc683xx_cpu32::flat::ADDRESS_BITS;
//! use mc683xx_cpu32::flat::Machine;
//! use mc683xx_cpu32::memory::PAGE_BYTES;
//!
//! // The reset vectors, SSP then PC (CPU32RM 6.2.1), and the code.
//! let mut image = vec![0; PAGE_BYTES as usize];
//! image[0..4].copy_from_slice(&0x800_u32.to_be_bytes());
//! image[4..8].copy_from_slice(&0x400_u32.to_be_bytes());
//! image[0x400..0x402].copy_from_slice(&0x702a_u16.to_be_bytes());
//!
//! let mut machine = Machine::new(ADDRESS_BITS)?;
//! machine.map(Address(0), PAGE_BYTES, &image)?;
//! machine.reset().expect("the vectors are mapped");
//! let mut limits = Limits::default();
//! limits.instructions = Some(1);
//! machine.run_limited(limits);
//! assert_eq!(machine.cpu.registers.data[0], 42);
//! assert_eq!(machine.cpu.registers.pc, 0x402);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod address;
pub mod bus;
pub mod cpu;
pub mod cycles;
mod effective_address;
pub mod exception;
mod flags;
pub mod flat;
pub mod frequency;
pub mod function_code;
mod instructions;
pub mod interrupt;
pub mod memory;
pub mod peripheral;
pub mod registers;
pub mod size;
mod timing;
