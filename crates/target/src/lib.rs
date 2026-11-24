//! A CPU32 (683xx) under debug control, as the emulator and a BDM pod
//! on silicon both present it: [`Cpu32Target`], and the registers,
//! entry sources, addresses, operand sizes and function codes it takes,
//! and the number syntax a person types for them ([`integer`]).
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
pub mod integer;
pub mod register;
pub mod serial;
pub mod size;

use address::Address;
use entry::Entry;
use function_code::FunctionCode;
use register::Register;
use register::SystemRegister;
use size::Size;

/// A [`Cpu32Target`] call the implementation does not support: the
/// error a method added after 1.0 returns by default.  Built with
/// [`Unsupported::new`], so a field added later gets a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{what} is not supported")]
#[non_exhaustive]
pub struct Unsupported {
    /// What is not supported: the call, or what it needs.
    pub what: &'static str,
}

impl Unsupported {
    /// `what` is not supported.
    #[must_use]
    pub const fn new(what: &'static str) -> Self {
        Self { what }
    }
}

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
///
/// A method added to the trait after 1.0 comes with a default body
/// that fails with [`Unsupported`], so an implementation written
/// before it still builds, and fails that call cleanly.
pub trait Cpu32Target {
    /// What went wrong: the link, a bus error, or the target's own.
    /// `'static`, so a host keeps it as a cause, and from
    /// [`Unsupported`], for a call the implementation lacks.
    type Error: core::error::Error + From<Unsupported> + 'static;

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

    /// Executes from RPC and stops in background mode again where T1
    /// tracing would trap (CPU32RM 6.2.10), before any handler's first
    /// instruction: an interrupt pending at the start, taken; or one
    /// instruction, with the exceptions it raised (the program's own
    /// trace among them), then an interrupt pending at its end, taken,
    /// as the trace comes before it.  Either way, one of a higher level
    /// pending after an interrupt is taken too.  So a step is not always one
    /// instruction: a caller reads RPC to see where it stopped.  A
    /// stepped STOP or LPSTOP does not stop the CPU.  PCC is
    /// unspecified after it.
    ///
    /// The entry is [`Entry::Breakpoint`] for an ordinary stop,
    /// [`Entry::Background`] if the instruction was BGND, and
    /// [`Entry::DoubleBusFault`] for a double bus fault.  An
    /// implementation may need configuration first (a pod's scratch
    /// RAM for its exception table and stub).
    ///
    /// Implementation-defined, as a pod stepping by T1 through an
    /// exception table of its own cannot match them: an RTE of a
    /// format $C frame, which is not traced; a read or write of VBR;
    /// MOVE from SR; a vector fetch that faults; SSW TR in a fault's
    /// frame.
    ///
    /// # Errors
    /// The target is not halted, cannot step as configured, or did not
    /// stop where a step ends.
    fn step(&mut self) -> Result<Entry, Self::Error>;

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
    /// `size`.  After an error `data`'s contents are unspecified: some,
    /// none or all of the accesses before the failing one may have
    /// filled their bytes.
    fn read_memory(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<(), Self::Error>;

    /// Writes `data` to `address` in `function_code`'s space, in `size`
    /// accesses, bytes in address order.
    ///
    /// # Errors
    /// A bus or address error, or a length that is not a multiple of
    /// `size`.  After a bus or address error, the accesses before the
    /// failing one may have been made.
    fn write_memory(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::Unsupported;

    #[test]
    fn unsupported_names_what() {
        assert_eq!(
            Unsupported::new("CALL").to_string(),
            "CALL is not supported"
        );
    }
}
