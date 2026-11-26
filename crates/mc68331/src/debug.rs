//! The chip under debug control, as a BDM debugger drives silicon:
//! [`Cpu32Target`] (CPU32RM 7.2.8).
//!
//! Every call but [`Cpu32Target::halted`] runs between the chip's runs,
//! so between instructions.  Memory goes through [`Mc68331::bus_read`]
//! and [`Mc68331::bus_write`]: real cycles, with their side effects, as
//! BDM's READ and WRITE make them.  [`Mc68331::bdm_exchange`] is the
//! serial interface itself ([`serial`]); each of the other calls is a
//! command of its own, so one leaves the interface between commands,
//! dropping a command in progress there.

pub(crate) mod serial;

use mc683xx_target::Address;
use mc683xx_target::Cpu32Target;
use mc683xx_target::Entry;
use mc683xx_target::FunctionCode;
use mc683xx_target::Register;
use mc683xx_target::Size;
use mc683xx_target::SystemRegister;
use mc683xx_target::Unsupported;

use mc683xx_cpu32::Cycles;
use mc683xx_cpu32::Limits;
use mc683xx_cpu32::Stop;
use mc683xx_cpu32::registers::BackgroundOnly;
use mc683xx_cpu32::registers::status;

use crate::chip::Mc68331;
use crate::host::BusCycleError;

/// The most system clocks a step runs: far more than any instruction
/// with its exceptions takes, so a step that ends without stopping (a
/// reset in it, which disables background mode) ends.
const STEP_BUDGET: Cycles = Cycles::from_clocks(1 << 20);

/// Why a debugger's call on the chip failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TargetError {
    /// The host stopped a step before it completed (a hook, a fault
    /// stop, or its stop request): the CPU is in background mode where
    /// it stopped.
    #[error("the host stopped the step before it completed")]
    Stopped,
    /// The call needs background mode, and the CPU is not in it.
    #[error("the CPU is not in background mode")]
    NotHalted,
    /// Background mode was not enabled at the last reset, so the CPU
    /// cannot enter it (CPU32RM 7.2.1).
    #[error("background mode is not enabled")]
    NotEnabled,
    /// A memory access's bus cycle failed.
    #[error(transparent)]
    Access(#[from] BusCycleError),
    /// A memory call's length is not a whole number of its size.
    #[error("{length} bytes are not a whole number of {}-byte accesses", size.bytes())]
    Length {
        /// The bytes asked for.
        length: usize,
        /// The access size.
        size: Size,
    },
    /// The chip does not support the call.
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
}

impl Mc68331 {
    /// BKPT asserted between runs, so at an instruction boundary
    /// (CPU32RM 7.2.2.1, Table 7-1).  With background mode enabled the
    /// CPU enters it at once, as [`Cpu32Target::halt`] makes it, and
    /// this is true.  Disabled, a hardware breakpoint is left pending
    /// for the next run ([`Cpu::set_breakpoint_pending`]), and this is
    /// false: its breakpoint acknowledge cycle, in CPU space at
    /// $00001E, reaches the selects and the bus peripherals; one that
    /// ends it with DSACK has the breakpoint ignored, and with none it
    /// ends in a bus error, which takes vector 12 (CPU32RM 6.2.6;
    /// MC68331UM 4.5.4.1.2).  A halted CPU ignores it, enabled or not,
    /// nothing left pending, and this is false: only RESET restarts it
    /// (CPU32RM 6.2.2).
    ///
    /// [`Cpu::set_breakpoint_pending`]: mc683xx_cpu32::Cpu::set_breakpoint_pending
    pub fn assert_breakpoint(&mut self) -> bool {
        let entered = self.cpu.assert_breakpoint();
        if entered {
            self.board.wake(self.now);
            self.sync_freeze();
        }
        entered
    }

    /// BKPT asserted with background mode enabled, as
    /// [`assert_breakpoint`](Self::assert_breakpoint); disabled,
    /// nothing, and false.  Whether the CPU is in background mode.
    fn breakpoint(&mut self) -> bool {
        self.cpu.bdm_enabled() && self.assert_breakpoint()
    }

    /// An error unless the CPU is in background mode.
    fn in_background(&self) -> Result<(), TargetError> {
        self.cpu
            .background()
            .map(|_| ())
            .ok_or(TargetError::NotHalted)
    }

    /// An error unless the CPU is in background mode; then the serial
    /// interface is left between commands, as a debugger's command
    /// through it would leave it.
    fn command(&mut self) -> Result<(), TargetError> {
        self.in_background()?;
        self.serial.discard();
        Ok(())
    }

    /// GO (CPU32RM 7.2.8.12): background mode left, FREEZE with it.
    fn resume(&mut self) {
        self.cpu.resume();
        self.sync_freeze();
    }

    /// A system register, zero-extended (CPU32RM 7.2.8.6): the
    /// programming model's, or one of background mode's own (7.2.5).
    fn system_register(&self, register: SystemRegister) -> u32 {
        let cpu = &self.cpu;
        cpu.registers
            .system_register(register)
            .unwrap_or_else(|| match register {
                SystemRegister::Pcc => cpu.pcc().0,
                SystemRegister::Atemp => cpu.atemp(),
                SystemRegister::Far => cpu.far().0,
                // RPC, where GO resumes: the PC
                _ => cpu.registers.pc.0,
            })
    }

    /// Sets a system register as WSREG does: a register narrower than
    /// 32 bits takes the low bits, and a write to FAR is ignored
    /// (CPU32RM 7.2.8.7).
    fn set_system_register(&mut self, register: SystemRegister, value: u32) {
        let cpu = &mut self.cpu;
        if let Err(BackgroundOnly(register)) = cpu.registers.set_system_register(register, value) {
            match register {
                SystemRegister::Rpc => cpu.registers.pc = Address(value),
                SystemRegister::Pcc => cpu.set_pcc(Address(value)),
                SystemRegister::Atemp => cpu.set_atemp(value),
                // FAR, read only
                _ => {}
            }
        }
    }

    /// `data`'s accesses of `size` from `address`: each one's address
    /// and bytes, or an error if `data` is not whole accesses.
    fn accesses(
        address: Address,
        size: Size,
        length: usize,
    ) -> Result<impl Iterator<Item = (Address, usize)>, TargetError> {
        let bytes = size.bytes() as usize;
        if !length.is_multiple_of(bytes) {
            return Err(TargetError::Length { length, size });
        }
        Ok((0..length)
            .step_by(bytes)
            .map(move |offset| (address.offset(offset as u32), offset)))
    }
}

impl Mc68331 {
    /// CALL (CPU32RM 7.2.8.13): RPC stacked at the active SP, in its
    /// data space, then execution from `address`.  A fault stacking
    /// leaves the CPU in background mode with SP as it was.
    ///
    /// # Errors
    /// The CPU is not in background mode, or stacking RPC faulted.
    pub fn call(&mut self, address: Address) -> Result<(), TargetError> {
        let supervisor = self.read_system_register(SystemRegister::Sr)? & u32::from(status::S) != 0;
        let space = if supervisor {
            FunctionCode::SupervisorData
        } else {
            FunctionCode::UserData
        };
        let sp = self.read_register(Register::A7)?.wrapping_sub(4);
        let rpc = self.read_system_register(SystemRegister::Rpc)?;
        self.write_memory(Address(sp), Size::Long, space, &rpc.to_be_bytes())?;
        self.write_register(Register::A7, sp)?;
        self.write_system_register(SystemRegister::Rpc, address.0)?;
        self.go()
    }
}

impl Cpu32Target for Mc68331 {
    type Error = TargetError;

    fn reset_halted(&mut self) -> Result<Entry, TargetError> {
        self.reset_with_breakpoint();
        self.cpu.background().ok_or(TargetError::NotEnabled)
    }

    fn halt(&mut self) -> Result<Entry, TargetError> {
        if !self.breakpoint() {
            return Err(TargetError::NotEnabled);
        }
        self.cpu.background().ok_or(TargetError::NotEnabled)
    }

    fn halted(&mut self) -> Result<Option<Entry>, TargetError> {
        Ok(self.cpu.background())
    }

    fn go(&mut self) -> Result<(), TargetError> {
        self.command()?;
        self.resume();
        Ok(())
    }

    /// One step of [`Limits::steps`], the board's time moving with it,
    /// then a breakpoint, as BKPT asserted at the next boundary would
    /// make (CPU32RM 7.2.2.1): PCC the instruction executed, if one was.
    /// A stepped STOP or LPSTOP leaves the CPU running, as T1 does
    /// (CPU32RM 6.2.10).
    fn step(&mut self) -> Result<Entry, TargetError> {
        self.command()?;
        self.resume();
        let mut limits = Limits::default();
        limits.steps = Some(1);
        let start = self.now;
        let completed = loop {
            let ran = self.run(STEP_BUDGET, Some(&mut limits));
            if ran.limited {
                break true;
            }
            // a sync ends a run early, the step not done: it goes on
            if ran.run.stop != Stop::Budget || self.now.since(start) >= STEP_BUDGET {
                break false;
            }
        };
        // a stop the host asked for in the step's last quantum, left
        // pending by the stop that ended it or still waiting: the step's
        let stopped = self.board.take_stop();
        let completed = if completed && self.board.sim.low_power().is_some() {
            // a stepped LPSTOP: the modules awake, then the interrupts
            // pending at its end, unless a hook or a fault stops them
            self.board.wake(self.now);
            limits.steps = Some(0);
            let ran = self.run(STEP_BUDGET, Some(&mut limits));
            ran.limited || ran.run.stop == Stop::Budget
        } else {
            completed
        };
        // a BGND or a double bus fault entered background mode itself
        let entered = self.cpu.background();
        self.breakpoint();
        match (
            self.cpu.background(),
            (entered.is_some() || completed) && !stopped,
        ) {
            (Some(entry), true) => Ok(entry),
            (Some(_), false) => Err(TargetError::Stopped),
            // a reset in the step
            (None, _) => Err(TargetError::NotEnabled),
        }
    }

    fn read_register(&mut self, register: Register) -> Result<u32, TargetError> {
        self.command()?;
        Ok(self.cpu.registers.register(register))
    }

    fn write_register(&mut self, register: Register, value: u32) -> Result<(), TargetError> {
        self.command()?;
        self.cpu.registers.set_register(register, value);
        Ok(())
    }

    fn read_system_register(&mut self, register: SystemRegister) -> Result<u32, TargetError> {
        self.command()?;
        Ok(self.system_register(register))
    }

    /// As WSREG: a register narrower than 32 bits takes the low bits,
    /// and a write to FAR is ignored (CPU32RM 7.2.8.7).
    fn write_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), TargetError> {
        self.command()?;
        self.set_system_register(register, value);
        Ok(())
    }

    fn read_memory(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<(), TargetError> {
        self.command()?;
        let bytes = size.bytes() as usize;
        for (at, offset) in Self::accesses(address, size, data.len())? {
            let value = self
                .bus_read(at, size, function_code)
                .map_err(TargetError::Access)?;
            data[offset..offset + bytes].copy_from_slice(&value.to_be_bytes()[4 - bytes..]);
        }
        Ok(())
    }

    fn write_memory(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), TargetError> {
        self.command()?;
        let bytes = size.bytes() as usize;
        for (at, offset) in Self::accesses(address, size, data.len())? {
            let value = data[offset..offset + bytes]
                .iter()
                .fold(0, |value, &byte| value << 8 | u32::from(byte));
            self.bus_write(at, size, function_code, value)
                .map_err(TargetError::Access)?;
        }
        Ok(())
    }
}
