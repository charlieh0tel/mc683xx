//! What a bare host asks of the core beyond what the chip does.
//!
//! Runs limited by an address or an instruction count, exceptions the
//! host answers before the CPU stacks anything, faults that stop the
//! run instead of being taken, and, for parity runs against other
//! emulators only, deviations from the CPU32 ([PLAN.md, Bare CPU]).
//! None of it costs a run that does not ask for it.
//!
//! [PLAN.md, Bare CPU]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#bare-cpu

use std::fmt;
use std::mem;

use super::Cpu;
use super::Halt;
use super::Idle;
use super::Stop;
use super::hooks::Invalid;
use super::hooks::Resume;
use crate::bus::Bus;
use crate::cycles::Cycles;
use crate::exception::CycleKind;
use crate::exception::Exception;
use crate::exception::Fault;
use crate::exception::Vector;
use crate::instructions::Flow;
use crate::interrupt::Interrupts;
use crate::registers::Registers;
use crate::registers::status;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

#[cfg(test)]
mod tests;

/// What ends a [`Cpu::run_limited`] besides what ends a [`Cpu::run_for`].
///
/// It may gain fields: build one from [`Limits::default()`] and set
/// those it needs (`let mut x = Limits::default(); x.cycles = ...;`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// A clock budget; [`None`], none.
    pub cycles: Option<Cycles>,
    /// The most instructions to run.
    pub instructions: Option<u64>,
    /// The most steps to take: instructions run, each with the
    /// exceptions it raised, interrupts taken, and a trace a fault stop
    /// left pending, taken as the run starts.  The run ends before
    /// the next instruction once it has taken them, interrupts pending
    /// there taken first, as T1 tracing stops (CPU32RM 6.2.10: the
    /// trace before a pending interrupt): one step is an interrupt
    /// pending at the start, or an instruction and then any interrupt
    /// pending at its end, either way before the handler's first
    /// instruction.  Zero takes only the interrupts pending.
    pub steps: Option<u64>,
    /// An address: the run ends when PC reaches it at an instruction
    /// boundary, before an interrupt is taken or a hook called there.
    pub until: Option<Address>,
}

/// Why a fault that stopped the run was one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FaultCause {
    /// A bus error: nothing answered (an unmapped address) or a peripheral
    /// refused it.
    BusError,
    /// Its page's permissions do not allow it.
    Protected,
    /// A word or long at an odd address, or an odd PC: an address
    /// error (CPU32RM 6.2.3).
    AddressError,
}

/// `bus error`, `protected` or `address error`.
impl fmt::Display for FaultCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BusError => "bus error",
            Self::Protected => "protected",
            Self::AddressError => "address error",
        })
    }
}

/// A fault that stopped the run ([`Cpu::set_stop_on_fault`]): nothing
/// stacked.  A fault in an instruction, its own exception's stacking
/// included, rolls the registers back to its start.  A fault stacking
/// an exception taken after an instruction completed (a trace, a
/// hardware breakpoint, an interrupt) keeps that instruction, the
/// registers as the exception's processing began, and leaves the
/// exception pending: the next run takes the trace or breakpoint
/// first, and samples the interrupt again.  A stopped CPU stays
/// stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FaultStop {
    /// Fetch, read or write.
    pub cycle: CycleKind,
    /// Why.
    pub cause: FaultCause,
    /// The cycle's address, all 32 bits.
    pub address: Address,
    /// Byte, word, or long when the bus took a long in one access.
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// The registers as the instruction, or the exception processing
    /// after one, began, as the CPU holds them again.
    pub registers: Registers,
}

/// The cause, then the cycle: `bus error: write of 2 bytes at
/// 0x90000000, fc 5, pc 0x00001000`.
impl fmt::Display for FaultStop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} of {} bytes at {}, fc {}, pc {}",
            self.cause,
            self.cycle,
            self.size.bytes(),
            self.address,
            self.function_code,
            Address(self.registers.pc)
        )
    }
}

/// An exception an instruction raised that the host intercepts
/// ([`Cpu::set_intercepted`]): nothing stacked, PC at the instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Intercepted {
    /// Its vector.
    pub vector: Vector,
    /// The instruction's address, where PC is.
    pub pc: Address,
}

/// How a limited run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum End {
    /// As [`Cpu::run_for`] ends.
    Stop(Stop),
    /// PC reached [`Limits::until`].
    Until,
    /// It ran [`Limits::instructions`].
    Instructions,
    /// It took [`Limits::steps`].
    Steps,
    /// A fault, with [`Cpu::set_stop_on_fault`].
    Fault(FaultStop),
    /// An intercepted exception.
    Intercepted(Intercepted),
}

/// A limited run's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LimitedRun {
    /// The cycles it took.  With no budget, a CPU that `stop` stopped
    /// takes none waiting.
    pub cycles: Cycles,
    /// Why it ended.
    pub end: End,
    /// The steps it took, as [`Limits::steps`] counts them.
    pub steps: u64,
}

/// Deviations from the CPU32, for parity runs against an emulator that
/// has them; never the CPU's behavior.
///
/// It may gain fields: build one from [`Deviations::default()`] and set
/// those it needs (`let mut x = Deviations::default(); x.misaligned = ...;`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Deviations {
    /// A word or long at an odd data address is run as byte cycles, not
    /// an address error.  Fetches stay strict.
    pub misaligned: bool,
    /// T1 and T0 trace nothing.
    pub untraced: bool,
}

/// The vectors, as a bit set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VectorSet([u64; 4]);

impl VectorSet {
    /// Whether `vector` is in it.
    pub(crate) const fn contains(self, vector: Vector) -> bool {
        self.0[(vector.0 >> 6) as usize] & 1 << (vector.0 & 63) != 0
    }

    /// Adds or removes `vector`.
    const fn set(&mut self, vector: Vector, member: bool) {
        let bit = 1 << (vector.0 & 63);
        let word = &mut self.0[(vector.0 >> 6) as usize];
        *word = if member { *word | bit } else { *word & !bit };
    }

    /// Whether it is empty.
    const fn is_empty(self) -> bool {
        self.0[0] | self.0[1] | self.0[2] | self.0[3] == 0
    }
}

/// The bare host's settings, what a stop for them carries, and what
/// the run loop looks at only between runs, kept out of its way.
#[derive(Debug, Default)]
pub(crate) struct Bare {
    /// Stop on a fault rather than take it.
    pub(crate) stop_on_fault: bool,
    /// The vectors intercepted.
    pub(crate) intercepted: VectorSet,
    /// An intercepted exception taken natively once: by the instruction
    /// at this address, when the instruction count is this.
    pub(crate) native_once: Option<(Address, u64)>,
    /// The deviations.
    pub(crate) deviations: Deviations,
    /// The end a fault or an intercept set, for `run_limited`.
    pub(crate) end: Option<End>,
    /// The last invalid access's reason, for a fault's cause.
    pub(crate) last_invalid: Option<Invalid>,
    /// The interrupts taken since the last limited run began, and the
    /// traces a fault stop left pending: steps of their own.
    pub(crate) exceptions_stepped: u64,
    /// Whether a hardware breakpoint is pending, BKPT asserted with
    /// background mode disabled, until it is acknowledged (CPU32RM
    /// 6.2.6).  Here, out of the run loop's way, as it is looked at
    /// only as a run starts.
    pub(crate) breakpoint_pending: bool,
    /// A trace whose frame a fault stopped the stacking of: the traced
    /// instruction's address it stacks, until the next run takes it
    /// (CPU32RM 6.2.10).  Here as [`breakpoint_pending`] is.
    ///
    /// [`breakpoint_pending`]: Self::breakpoint_pending
    pub(crate) trace_pending: Option<Address>,
}

impl Cpu {
    /// Runs as [`run_for`](Self::run_for) does, but also until PC reaches
    /// `limits.until`, `limits.instructions` have run, a fault stops it
    /// (`set_stop_on_fault`) or an exception is intercepted
    /// (`set_intercepted`).  With no cycle budget it runs until one of
    /// those or a stop.
    pub fn run_limited(
        &mut self,
        bus: &mut dyn Bus,
        interrupts: &mut dyn Interrupts,
        now: Cycles,
        limits: Limits,
    ) -> LimitedRun {
        if self.registers.pc != self.left_pc.0 {
            self.block_start = true;
        }
        self.bare.end = None;
        let budget = limits.cycles.unwrap_or(Cycles(u64::MAX - now.0));
        let first = self.instructions;
        self.bare.exceptions_stepped = 0;
        let run = self.run_from_boundary(bus, interrupts, now, budget, Some(limits));
        self.left_pc = Address(self.registers.pc);
        let end = self.bare.end.clone().unwrap_or(End::Stop(run.stop));
        LimitedRun {
            cycles: run.cycles,
            end,
            steps: self.instructions - first + self.bare.exceptions_stepped,
        }
    }

    /// Forgets that a hook stopped the last run at PC, so the next run
    /// calls the code and block hooks there again.
    pub const fn clear_resume(&mut self) {
        self.resume = None;
    }

    /// The registers as the last instruction began, or the trace,
    /// hardware breakpoint or interrupt processing after it, while they
    /// are kept: while a read, write or invalid access hook is
    /// installed or faults stop the run.  A fault stop holds them again
    /// ([`FaultStop`]).
    #[must_use]
    pub const fn instruction_registers(&self) -> &Registers {
        &self.began
    }

    /// Whether a fault stops a run ([`End::Fault`], [`Stop::Fault`])
    /// instead of being taken as a bus or address error exception: off
    /// at first.  A fault in exception processing stops it too, rather
    /// than halting the CPU in a double bus fault.
    pub fn set_stop_on_fault(&mut self, stop: bool) {
        self.bare.stop_on_fault = stop;
        self.update_capture();
    }

    /// Whether `vector`, raised by an instruction (a trap, an illegal
    /// instruction, a divide by zero, but not a fault, a trace or an
    /// interrupt), ends a limited run before anything is stacked
    /// ([`End::Intercepted`]): its (An)+ and -(An) undone and PC at the
    /// instruction.  None at first.
    pub fn set_intercepted(&mut self, vector: Vector, intercepted: bool) {
        self.bare.intercepted.set(vector, intercepted);
    }

    /// The fault the last run stopped for ([`Stop::Fault`],
    /// [`End::Fault`]), until the next run starts.
    #[must_use]
    pub const fn fault_stop(&self) -> Option<&FaultStop> {
        match &self.bare.end {
            Some(End::Fault(fault)) => Some(fault),
            _ => None,
        }
    }

    /// Whether a fault stops a run.
    #[must_use]
    pub const fn stops_on_fault(&self) -> bool {
        self.bare.stop_on_fault
    }

    /// Why the CPU halted, while it stays halted.
    #[must_use]
    pub const fn halted(&self) -> Option<Halt> {
        match self.idle {
            Some(idle) => idle.halt(),
            None => None,
        }
    }

    /// The entry source while the CPU is in background mode (CPU32RM
    /// 7.2.3).
    #[must_use]
    pub const fn background(&self) -> Option<Entry> {
        match self.idle {
            Some(Idle::Background(entry)) => Some(entry),
            _ => None,
        }
    }

    /// BKPT acknowledged between instructions: with background mode
    /// enabled the CPU enters it for a breakpoint, PCC the last
    /// instruction executed, and this is true (CPU32RM 7.2.2.1).
    /// Disabled, nothing happens and this is false: BKPT then asks for
    /// a hardware breakpoint exception, which
    /// [`set_breakpoint_pending`](Self::set_breakpoint_pending) leaves
    /// for the next run.  In background mode already, true with nothing
    /// changed.
    pub fn breakpoint(&mut self) -> bool {
        if self.background().is_some() {
            return true;
        }
        if !self.bdm_enabled || self.idle.is_some() {
            return false;
        }
        let pcc = self.instruction_pc;
        self.enter_background(Entry::Breakpoint, pcc);
        true
    }

    /// Whether a hardware breakpoint is pending.
    #[must_use]
    pub const fn breakpoint_pending(&self) -> bool {
        self.bare.breakpoint_pending
    }

    /// Leaves a hardware breakpoint pending, as BKPT asserted with
    /// background mode disabled does, or withdraws one not yet
    /// acknowledged.  Breakpoints are left pending, not taken at once
    /// (CPU32RM 6.2.6): the next run acknowledges it before anything
    /// else, at the boundary where the last run ended, after that
    /// instruction's trace and before an interrupt (Table 6-2), with a
    /// breakpoint acknowledge cycle in CPU space at $00001E, BKPT #7
    /// with T set (5.3.1.1).  DSACK ending it ignores the breakpoint;
    /// BERR takes vector 12 with a six-word frame (6.4.2).  A halted CPU
    /// keeps it pending, and a stopped one until the interrupt that
    /// ends STOP or LPSTOP, after whose exception processing it is
    /// acknowledged (PLAN.md, Background debug mode).  Reset drops it.
    /// With background mode enabled the next run enters it, as
    /// [`breakpoint`](Self::breakpoint).
    pub const fn set_breakpoint_pending(&mut self, pending: bool) {
        self.bare.breakpoint_pending = pending;
    }

    /// The traced instruction's address of a trace pending: one whose
    /// frame a fault stopped the stacking of ([`set_stop_on_fault`],
    /// PLAN.md, Faults).
    ///
    /// [`set_stop_on_fault`]: Self::set_stop_on_fault
    #[must_use]
    pub const fn trace_pending(&self) -> Option<Address> {
        self.bare.trace_pending
    }

    /// Leaves a trace of the instruction at `address` pending, or
    /// withdraws one ([`None`]).  The next run takes it before anything
    /// else, at the boundary where the last run ended, before a pending
    /// breakpoint (CPU32RM 6.1.5, Table 6-2): a six-word frame with PC
    /// and `address` (6.2.10), a step of its own.  A halted CPU, or one
    /// in background mode, keeps it pending.  Reset drops it.
    pub const fn set_trace_pending(&mut self, address: Option<Address>) {
        self.bare.trace_pending = address;
    }

    /// Leaves background mode to execute from RPC, the PC, as GO does:
    /// the pipeline flushed (CPU32RM 7.2.6, 7.2.8.12).  Nothing out of
    /// it.  Whatever exception processing preceded the entry is over: a
    /// fault on the first fetch is an exception of its own, not a
    /// double bus fault.
    pub fn resume(&mut self) {
        if self.background().is_some() {
            self.idle = None;
            self.block_start = true;
            self.fault_window = false;
        }
    }

    /// PCC: the first word of the last instruction executed before
    /// background mode (CPU32RM 7.2.5.3).
    #[must_use]
    pub const fn pcc(&self) -> Address {
        self.pcc
    }

    /// Writes PCC, as WSREG can (CPU32RM 7.2.8.7).
    pub const fn set_pcc(&mut self, pcc: Address) {
        self.pcc = pcc;
    }

    /// ATEMP: the entry source on entering background mode, then BDM's
    /// temporary (CPU32RM 7.2.3).
    #[must_use]
    pub const fn atemp(&self) -> u32 {
        self.atemp
    }

    /// Writes ATEMP, as WSREG can.
    pub const fn set_atemp(&mut self, atemp: u32) {
        self.atemp = atemp;
    }

    /// FAR: the address of the last bus or address error's cycle
    /// (CPU32RM 7.2.5.1; PLAN.md, Background debug mode).  Read only.
    #[must_use]
    pub const fn far(&self) -> Address {
        self.far
    }

    /// Whether background mode is enabled.
    #[must_use]
    pub const fn bdm_enabled(&self) -> bool {
        self.bdm_enabled
    }

    /// Enables or disables background mode, as BKPT's level at RESET's
    /// rising edge does (CPU32RM 7.2.1); the chip sets it at each reset
    /// from its BKPT input.  A bare CPU's host sets it before running.
    pub const fn set_bdm_enabled(&mut self, enabled: bool) {
        self.bdm_enabled = enabled;
    }

    /// The stop a run ends in at once while the CPU executes nothing,
    /// or [`None`] while it runs.
    #[must_use]
    pub const fn idle(&self) -> Option<Stop> {
        match self.idle {
            Some(idle) => Some(idle.stop()),
            None => None,
        }
    }

    /// Whether any vector is intercepted.
    #[must_use]
    pub const fn intercepts(&self) -> bool {
        !self.bare.intercepted.is_empty()
    }

    /// Lets the instruction at PC take an intercepted exception
    /// natively, once, the next time it runs.
    pub const fn take_natively_once(&mut self) {
        self.bare.native_once = Some((Address(self.registers.pc), self.instructions));
    }

    /// Whether a long operand is one access: offered to the bus whole
    /// ([`Bus::read_long`], `write_long`) before it is split into word
    /// cycles, and shown to the read and write hooks once, as size 4
    /// at its address, whatever cycles served it; a long that faults
    /// is not shown, its cycles before the fault included.  Off at
    /// first, as on the CPU32's 16-bit bus.
    pub const fn set_long_bus(&mut self, whole: bool) {
        self.long_bus = whole;
    }

    /// Sets the deviations from the CPU32: none at first.
    pub const fn set_deviations(&mut self, deviations: Deviations) {
        self.bare.deviations = deviations;
        self.trace_mask = if deviations.untraced {
            0
        } else {
            status::T1 | status::T0
        };
    }

    /// The deviations from the CPU32.
    #[must_use]
    pub const fn deviations(&self) -> Deviations {
        self.bare.deviations
    }

    /// Whether the registers are kept as each instruction begins.
    pub(crate) fn update_capture(&mut self) {
        self.capture = self.bare.stop_on_fault || self.hooks.want_registers();
    }

    /// The end the bare host's settings make of an instruction that
    /// raised `step` and `released`, if they make one: the registers
    /// restored, nothing taken.
    #[cold]
    #[inline(never)]
    pub(super) fn bare_end(
        &mut self,
        step: Result<Flow, Exception>,
        released: Option<Fault>,
        instruction_pc: Address,
    ) -> Option<End> {
        let fault = match (step, released) {
            (_, Some(fault)) | (Err(Exception::Fault(fault)), None) => Some(fault),
            _ => None,
        };
        if let Some(fault) = fault
            && self.bare.stop_on_fault
        {
            return Some(self.fault_end(fault));
        }
        let Err(exception @ (Exception::Short { .. } | Exception::Long { .. })) = step else {
            return None;
        };
        let vector = exception.vector();
        let native = self.bare.native_once.take() == Some((instruction_pc, self.instructions));
        if native || vector == Vector::TRACE || !self.bare.intercepted.contains(vector) {
            return None;
        }
        mem::take(&mut self.altered).undo(&mut self.registers);
        self.registers.pc = instruction_pc.0;
        self.resume = Some((instruction_pc, Resume::Code));
        Some(End::Intercepted(Intercepted {
            vector,
            pc: instruction_pc,
        }))
    }

    /// A fault's stop: the registers back as captured, as the
    /// instruction or the exception processing after one began.
    pub(crate) fn fault_end(&mut self, fault: Fault) -> End {
        let cause = if fault.vector == Vector::ADDRESS_ERROR {
            FaultCause::AddressError
        } else {
            match self.bare.last_invalid {
                Some(Invalid::Protected) => FaultCause::Protected,
                _ => FaultCause::BusError,
            }
        };
        self.registers.clone_from(&self.began);
        self.released = None;
        self.movem = None;
        self.resume = Some((Address(self.registers.pc), Resume::Code));
        End::Fault(FaultStop {
            cycle: fault.cycle,
            cause,
            address: fault.address,
            size: if fault.long { Size::Long } else { fault.size },
            function_code: fault.function_code,
            registers: self.registers.clone(),
        })
    }

    /// Ends the run at `end` once this instruction ends: a fault's
    /// [`Stop::Fault`] before any other stop asked for in it.
    pub(crate) fn end_with(&mut self, end: End) {
        if matches!(end, End::Fault(_)) {
            self.stop_request = Some(Stop::Fault);
        } else {
            self.stop_request.get_or_insert(Stop::Requested);
        }
        self.bare.end = Some(end);
    }
}
