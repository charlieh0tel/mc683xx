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

pub mod serial;

use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::bare::Limits;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::registers::status;

use crate::chip::Mc68331;
use crate::host::AccessError;

/// The most system clocks a step runs: far more than any instruction
/// with its exceptions takes, so a step that ends without stopping (a
/// reset in it, which disables background mode) ends.
const STEP_BUDGET: Cycles = Cycles(1 << 20);

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
    /// A memory access at `address` failed.
    #[error("{address}: {error}")]
    Access {
        /// The access's address.
        address: Address,
        /// Why.
        error: AccessError,
    },
    /// A memory call's length is not a whole number of its size.
    #[error("{length} bytes are not a whole number of {}-byte accesses", size.bytes())]
    Length {
        /// The bytes asked for.
        length: usize,
        /// The access size.
        size: Size,
    },
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
    /// MC68331UM 4.5.4.1.2).
    ///
    /// [`Cpu::set_breakpoint_pending`]: mc683xx_cpu32::cpu::Cpu::set_breakpoint_pending
    pub fn assert_breakpoint(&mut self) -> bool {
        if self.cpu.bdm_enabled() {
            return self.breakpoint();
        }
        self.cpu.set_breakpoint_pending(true);
        false
    }

    /// BKPT acknowledged between instructions, as
    /// [`Cpu::breakpoint`](mc683xx_cpu32::cpu::Cpu::breakpoint): out of
    /// STOP or LPSTOP too, which wakes the modules LPSTOP stopped.
    /// Whether the CPU is in background mode.
    fn breakpoint(&mut self) -> bool {
        let entered = self.cpu.breakpoint();
        if entered {
            self.board.wake(self.now);
            self.sync_freeze();
        }
        entered
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

    /// A data or address register.
    fn register(&self, register: Register) -> u32 {
        let number = usize::from(register.code());
        let registers = &self.cpu.registers;
        match number {
            0..8 => registers.data[number],
            _ => registers.address(number - 8),
        }
    }

    /// Sets a data or address register, all 32 bits.
    fn set_register(&mut self, register: Register, value: u32) {
        let number = usize::from(register.code());
        let registers = &mut self.cpu.registers;
        match number {
            0..8 => registers.data[number] = value,
            _ => registers.set_address(number - 8, value),
        }
    }

    /// A system register, zero-extended (CPU32RM 7.2.8.6).
    fn system_register(&self, register: SystemRegister) -> u32 {
        let cpu = &self.cpu;
        let registers = &cpu.registers;
        match register {
            SystemRegister::Rpc => registers.pc,
            SystemRegister::Pcc => cpu.pcc().0,
            SystemRegister::Atemp => cpu.atemp(),
            SystemRegister::Far => cpu.far().0,
            SystemRegister::Vbr => registers.vbr,
            SystemRegister::Sr => u32::from(registers.sr()),
            SystemRegister::Usp => registers.usp,
            SystemRegister::Ssp => registers.ssp,
            SystemRegister::Sfc => u32::from(registers.sfc.bits()),
            SystemRegister::Dfc => u32::from(registers.dfc.bits()),
        }
    }

    /// Sets a system register as WSREG does: a register narrower than
    /// 32 bits takes the low bits, and a write to FAR is ignored
    /// (CPU32RM 7.2.8.7).
    fn set_system_register(&mut self, register: SystemRegister, value: u32) {
        let cpu = &mut self.cpu;
        match register {
            SystemRegister::Rpc => cpu.registers.pc = value,
            SystemRegister::Pcc => cpu.set_pcc(Address(value)),
            SystemRegister::Atemp => cpu.set_atemp(value),
            SystemRegister::Far => {}
            SystemRegister::Vbr => cpu.registers.vbr = value,
            SystemRegister::Sr => cpu.registers.set_sr(value as u16),
            SystemRegister::Usp => cpu.registers.usp = value,
            SystemRegister::Ssp => cpu.registers.ssp = value,
            SystemRegister::Sfc => cpu.registers.sfc = FunctionCode::new(value),
            SystemRegister::Dfc => cpu.registers.dfc = FunctionCode::new(value),
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
            FunctionCode::SUPERVISOR_DATA
        } else {
            FunctionCode::USER_DATA
        };
        let sp = self.read_register(Register::A7)?.wrapping_sub(4);
        let rpc = self.read_system_register(SystemRegister::Rpc)?;
        self.write_memory(Address(sp), space, Size::Long, &rpc.to_be_bytes())?;
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
            let (run, limited) = self.run(STEP_BUDGET, Some(&mut limits));
            if limited {
                break true;
            }
            // a sync ends a run early, the step not done: it goes on
            if run.stop != Stop::Budget || start.until(self.now) >= STEP_BUDGET {
                break false;
            }
        };
        // a stop the host asked for in the step's last quantum
        let stopped = self.board.take_stop();
        if completed && self.board.sim.low_power().is_some() {
            // a stepped LPSTOP: the modules awake, then the interrupts
            // pending at its end
            self.board.wake(self.now);
            limits.steps = Some(0);
            self.run(STEP_BUDGET, Some(&mut limits));
        }
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
        Ok(self.register(register))
    }

    fn write_register(&mut self, register: Register, value: u32) -> Result<(), TargetError> {
        self.command()?;
        self.set_register(register, value);
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
        function_code: FunctionCode,
        size: Size,
        data: &mut [u8],
    ) -> Result<(), TargetError> {
        self.command()?;
        let bytes = size.bytes() as usize;
        for (at, offset) in Self::accesses(address, size, data.len())? {
            let value = self
                .bus_read(at, size, function_code)
                .map_err(|error| TargetError::Access { address: at, error })?;
            data[offset..offset + bytes].copy_from_slice(&value.to_be_bytes()[4 - bytes..]);
        }
        Ok(())
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &[u8],
    ) -> Result<(), TargetError> {
        self.command()?;
        let bytes = size.bytes() as usize;
        for (at, offset) in Self::accesses(address, size, data.len())? {
            let value = data[offset..offset + bytes]
                .iter()
                .fold(0, |value, &byte| value << 8 | u32::from(byte));
            self.bus_write(at, size, function_code, value)
                .map_err(|error| TargetError::Access { address: at, error })?;
        }
        Ok(())
    }
}
