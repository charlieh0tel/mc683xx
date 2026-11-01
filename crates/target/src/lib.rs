//! A CPU32 (683xx) under debug control, as the emulator and a BDM pod
//! on silicon both present it: [`Cpu32Target`], and the registers,
//! entry sources, addresses, operand sizes and function codes it takes.
//! `no_std`.

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
pub mod entry;
pub mod function_code;
pub mod register;
pub mod size;

use address::Address;
use entry::Entry;
use function_code::FunctionCode;
use register::Register;
use register::SystemRegister;
use size::Size;

/// What a debugger needs of a CPU32 with BDM enabled (CPU32RM 7.2):
/// stopping it in background mode, its registers and memory there, and
/// resuming it.  Register and memory calls need the target halted.
///
/// Each memory call names its address space.  An implementation that
/// reaches it through SFC or DFC, as BDM's READ and WRITE do (CPU32RM
/// 7.2.8.8), restores the program's SFC and DFC before resuming, and
/// [`read_system_register`](Cpu32Target::read_system_register) and
/// [`write_system_register`](Cpu32Target::write_system_register) of
/// SFC and DFC reach the program's values, never the ones a memory
/// call left: a write is what resuming restores.
///
/// ATEMP holds the entry source only until the first command after
/// entry overwrites it (CPU32RM 7.2.3), so an implementation reads it
/// first and keeps the [`Entry`] for [`halt`](Cpu32Target::halt) and
/// [`halted`](Cpu32Target::halted) to report until it resumes.  A
/// reset forgets whatever an implementation kept.
pub trait Cpu32Target {
    /// What went wrong: the link, a bus error, or the target's own.
    type Error: core::error::Error;

    /// Resets the target with BDM enabled and stops it in background
    /// mode before its first instruction (CPU32RM 7.2.1).
    ///
    /// # Errors
    /// The target did not enter background mode.
    fn reset_halted(&mut self) -> Result<Entry, Self::Error>;

    /// Stops a running target in background mode, or reports why a
    /// halted one is halted.  A halt whose ATEMP Table 7-2 does not
    /// list is still a halt: [`Entry::Unknown`], not an error.
    ///
    /// # Errors
    /// The target did not enter background mode: among other causes,
    /// BDM was not enabled at its last reset.
    fn halt(&mut self) -> Result<Entry, Self::Error>;

    /// Why the target is in background mode, or `None` while it runs.
    /// An emulated target may run on while this is called.
    ///
    /// # Errors
    /// The state could not be read.
    fn halted(&mut self) -> Result<Option<Entry>, Self::Error>;

    /// Resumes execution at RPC.
    ///
    /// # Errors
    /// The command did not complete.
    fn go(&mut self) -> Result<(), Self::Error>;

    /// Reads a data or address register.
    ///
    /// # Errors
    /// The read did not complete.
    fn read_register(&mut self, register: Register) -> Result<u32, Self::Error>;

    /// Writes a data or address register, all 32 bits.
    ///
    /// # Errors
    /// The write did not complete.
    fn write_register(&mut self, register: Register, value: u32) -> Result<(), Self::Error>;

    /// Reads a system register.
    ///
    /// # Errors
    /// The read did not complete.
    fn read_system_register(&mut self, register: SystemRegister) -> Result<u32, Self::Error>;

    /// Writes a system register.
    ///
    /// # Errors
    /// The write did not complete.
    fn write_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), Self::Error>;

    /// Reads `data.len()` bytes from `address` in `function_code`'s
    /// space, in `size` accesses; the bytes are in address order, so
    /// values are big-endian.
    ///
    /// # Errors
    /// A bus or address error, or a length that is not a multiple of
    /// `size`.
    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &mut [u8],
    ) -> Result<(), Self::Error>;

    /// Writes `data` to `address` in `function_code`'s space, in `size`
    /// accesses, bytes in address order.
    ///
    /// # Errors
    /// A bus or address error, or a length that is not a multiple of
    /// `size`.
    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &[u8],
    ) -> Result<(), Self::Error>;
}
