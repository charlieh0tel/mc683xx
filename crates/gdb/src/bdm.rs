//! A CPU32 under BDM control as the stub's [`Target`]: a BDM pod on
//! silicon, or the emulated chip, through [`Cpu32Target`].
//!
//! It runs on its own between a resume and its next entry into
//! background mode; meanwhile the stub looks in turn at it and at gdb's
//! connection, for its Ctrl-C.  A single step is [`Cpu32Target::step`],
//! which may stop past more than one instruction or at a handler; gdb
//! reads where from PC.  A software breakpoint is BGND written over
//! the instruction (CPU32RM 4.4, BGND), so it needs memory the target
//! can write; there are no hardware breakpoints or watchpoints.
//!
//! The target must run on its own once resumed: silicon does, and an
//! emulated chip does only if its host runs it, in its
//! [`Cpu32Target::halted`] for one.

use std::thread;
use std::time::Duration;

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use crate::target::AccessError;
use crate::target::NativeStep;
use crate::target::Target;
use crate::target::TargetReset;
use crate::target::TargetRun;

/// How long a look at a target still running waits before the stub
/// looks at gdb again.
const POLL_TICK: Duration = Duration::from_millis(1);

/// A [`Cpu32Target`] as the stub's [`Target`], borrowed for a session:
/// `serve(&mut Bdm::new(&mut pod), connection)`.
#[derive(Debug)]
pub struct Bdm<'t, T> {
    /// The target.
    target: &'t mut T,
}

impl<'t, T: Cpu32Target> Bdm<'t, T> {
    /// `target` as the stub's [`Target`].
    #[must_use]
    pub fn new(target: &'t mut T) -> Self {
        Self { target }
    }

    /// The size to move `length` bytes at `address` in: words if both
    /// are even, else bytes.
    fn size(address: Address, length: usize) -> Size {
        if address.0.is_multiple_of(2) && length.is_multiple_of(2) {
            Size::Word
        } else {
            Size::Byte
        }
    }
}

impl<T: Cpu32Target> Target for Bdm<'_, T> {
    type Error = T::Error;

    fn read_register(&mut self, register: Register) -> Result<u32, AccessError<T::Error>> {
        self.target
            .read_register(register)
            .map_err(AccessError::Target)
    }

    fn write_register(
        &mut self,
        register: Register,
        value: u32,
    ) -> Result<(), AccessError<T::Error>> {
        self.target
            .write_register(register, value)
            .map_err(AccessError::Target)
    }

    fn read_system_register(
        &mut self,
        register: SystemRegister,
    ) -> Result<u32, AccessError<T::Error>> {
        self.target
            .read_system_register(register)
            .map_err(AccessError::Target)
    }

    fn write_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), AccessError<T::Error>> {
        self.target
            .write_system_register(register, value)
            .map_err(AccessError::Target)
    }

    /// Real bus cycles: words when the address and length are even,
    /// else bytes.
    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<T::Error>> {
        let size = Self::size(address, data.len());
        self.target
            .read_memory(address, size, function_code, data)
            .map_err(AccessError::Target)?;
        Ok(data.len())
    }

    /// Real bus cycles, sized as reads are.
    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<T::Error>> {
        let size = Self::size(address, data.len());
        self.target
            .write_memory(address, size, function_code, data)
            .map_err(AccessError::Target)
    }

    fn resume(&mut self) -> Result<(), T::Error> {
        self.target.go()
    }

    fn run_for(&mut self, _cycles: Cycles, _hit: &dyn Fn() -> bool) -> Result<TargetRun, T::Error> {
        if let Some(entry) = self.target.halted()? {
            return Ok(TargetRun::Background(entry));
        }
        thread::sleep(POLL_TICK);
        Ok(TargetRun::Quantum)
    }

    fn halt(&mut self) -> Result<TargetRun, T::Error> {
        self.target.halt().map(TargetRun::Background)
    }

    fn native_step(&mut self) -> Option<&mut dyn NativeStep<Error = T::Error>> {
        Some(self)
    }

    fn reset(&mut self) -> Result<TargetReset, T::Error> {
        self.target.reset_halted().map(|_| TargetReset::Done)
    }
}

impl<T: Cpu32Target> NativeStep for Bdm<'_, T> {
    type Error = T::Error;

    fn step(&mut self) -> Result<TargetRun, T::Error> {
        self.target.step().map(TargetRun::Background)
    }
}
