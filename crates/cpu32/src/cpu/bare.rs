//! What a bare host asks of the core beyond what the chip does.
//!
//! Runs limited by an address or an instruction count, exceptions the
//! host handles before the CPU stacks anything, exceptions the host has
//! the CPU take, and faults that stop the run instead of being taken
//! ([PLAN.md, Bare CPU]).
//! None of it costs a run that does not ask for it.
//!
//! [PLAN.md, Bare CPU]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#bare-cpu

use std::fmt;
use std::mem;

use serde::Deserialize;
use serde::Serialize;

use super::Core;
use super::Cpu;
use super::Halt;
use super::Idle;
use super::Stop;
use crate::bus::Bus;
use crate::cpu::hooks::Resume;
use crate::cycles::Cycle;
use crate::cycles::Cycles;
use crate::exception::BusCycleKind;
use crate::exception::Exception;
use crate::exception::Fault;
use crate::exception::Vector;
use crate::exception::VectorSet;
use crate::hooks::Invalid;
use crate::instructions::Flow;
use crate::interrupt::InterruptLevel;
use crate::interrupt::Interrupts;
use crate::memory::Space;
use crate::registers::Registers;
use crate::registers::status;
use mc683xx_target::Address;
use mc683xx_target::Entry;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

#[cfg(test)]
mod tests;

/// BRA with an 8-bit displacement of -2: to itself (CPU32RM 4.4, BRA;
/// the displacement is from the opcode's address plus two).
const BRA_SELF_BYTE: u32 = 0x60fe;
/// BRA with a 16-bit displacement in the word that follows.
const BRA_WORD: u32 = 0x6000;
/// BRA with a 32-bit displacement in the long word that follows.
const BRA_LONG: u32 = 0x60ff;
/// The displacement of a branch to itself, -2.
const SELF_DISPLACEMENT: u32 = 0u32.wrapping_sub(2);
/// JMP (xxx).W: the address in the word that follows, sign-extended
/// (CPU32RM 4.4, JMP; 3.4.3).
const JMP_ABSOLUTE_WORD: u32 = 0x4ef8;
/// JMP (xxx).L: the address in the long word that follows.
const JMP_ABSOLUTE_LONG: u32 = 0x4ef9;

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
    /// left pending or a hardware breakpoint pending
    /// ([`Cpu::set_breakpoint_pending`]), taken as the run starts.  The run ends before
    /// the next instruction once it has taken them, interrupts pending
    /// there taken first, as T1 tracing stops (CPU32RM 6.2.10: the
    /// trace before a pending interrupt): one step is an interrupt
    /// pending at the start, or an instruction and then any interrupt
    /// pending at its end, either way before the handler's first
    /// instruction.  Zero takes only the interrupts pending.
    pub steps: Option<u64>,
    /// An address: the run ends when PC reaches it at an instruction
    /// boundary, before an interrupt is taken or a hook called there.
    pub reach: Option<Address>,
    /// Whether the run ends at a self-loop ([`End::SelfLoop`]): an
    /// instruction that branches to itself, BRA with a displacement of
    /// -2 (8, 16 or 32 bits) or JMP (xxx).W or (xxx).L of its own
    /// address (CPU32RM 4.4, BRA, JMP), as firmware idles waiting for
    /// an interrupt.  Looked for at each instruction boundary once no
    /// interrupt is due there (one due is taken first), before the
    /// code and block hooks, with SR's T1 and T0 clear (a traced loop
    /// takes a trace exception each time round); the instruction is
    /// not executed, and is read with no cycle from the core's memory
    /// (one a peripheral serves is not looked at).  DBcc to itself
    /// counts down, so it is not one.
    pub self_loop: bool,
}

/// Why a fault that stopped the run was one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// A fault that stopped the run ([`Cpu::set_stop_on_fault`]): no frame
/// stacked for it.  A fault in an instruction, its own exception's
/// stacking included, rolls the registers back to its start, though a
/// frame's writes before the fault stay written.  A fault stacking
/// an exception taken after an instruction completed (a trace, a
/// hardware breakpoint, an interrupt) keeps that instruction, the
/// registers as the exception's processing began, and leaves the
/// exception pending: the next run takes the trace or breakpoint
/// first, and samples the interrupt again.  A stopped CPU stays
/// stopped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct FaultStop {
    /// Fetch, read or write.
    pub kind: BusCycleKind,
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
/// 0x90000000 in supervisor-data, pc 0x00001000`.
impl fmt::Display for FaultStop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} of {} bytes at {} in {}, pc {}",
            self.cause,
            self.kind,
            self.size.bytes(),
            self.address,
            self.function_code,
            self.registers.pc
        )
    }
}

/// An exception an instruction raised that the host handles
/// ([`Cpu::add_intercept_hook`]): nothing stacked, PC at the
/// instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Intercepted {
    /// Its vector.
    pub vector: Vector,
    /// The instruction's address, where PC is.
    pub pc: Address,
}

/// Why [`Cpu::take_exception`] took nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TakeExceptionError {
    /// The CPU executes nothing: halted, or in background mode, the
    /// stop a run ends in at once.
    #[error("the CPU executes nothing: {0}")]
    Idle(Stop),
    /// Stacking the frame or reading the vector faulted, with
    /// [`Cpu::set_stop_on_fault`]: no exception entered, the registers
    /// as before; what the frame's writes before the fault wrote stays
    /// written.
    #[error("stopped at a fault: {0}")]
    Fault(FaultStop),
}

/// How a limited run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum End {
    /// As [`Cpu::run_for`] ends.
    Stop(Stop),
    /// PC reached [`Limits::reach`].
    Reached,
    /// It ran [`Limits::instructions`].
    Instructions,
    /// It took [`Limits::steps`].
    Steps,
    /// A fault, with [`Cpu::set_stop_on_fault`].
    Fault(FaultStop),
    /// An intercepted exception.
    Intercepted(Intercepted),
    /// An exception the CPU was about to take, with
    /// [`Cpu::set_stop_on_exception`]: nothing stacked.
    Exception {
        /// Its vector.
        vector: Vector,
        /// PC, where its processing begins when the run goes on.
        pc: Address,
    },
    /// PC at a self-loop, with [`Limits::self_loop`]: the loop not
    /// executed.
    SelfLoop {
        /// The loop's address.
        pc: Address,
    },
}

/// A short lowercase name for each way a limited run ends, as
/// [`Stop`]'s: its stop's name, or `reached`, `instructions`, `steps`,
/// `self-loop`, `fault`, `intercepted` or `exception`.  The detail a variant holds
/// (the fault, the exception) is not part of it.
impl fmt::Display for End {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stop(stop) => stop.fmt(formatter),
            Self::Reached => formatter.write_str("reached"),
            Self::Instructions => formatter.write_str("instructions"),
            Self::Steps => formatter.write_str("steps"),
            Self::Fault(_) => formatter.write_str("fault"),
            Self::Intercepted(_) => formatter.write_str("intercepted"),
            Self::Exception { .. } => formatter.write_str("exception"),
            Self::SelfLoop { .. } => formatter.write_str("self-loop"),
        }
    }
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

/// An exception the next run takes once with no stop or intercept for
/// it: the one of `vector` raised with PC at `pc` when the instruction
/// count is `instructions`.  Another exception there, a trace a host
/// withdrew and the trap after it, or a higher interrupt arriving
/// since, is not this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TakeOnce {
    /// The PC it is raised at: its instruction's, or the boundary's.
    pc: Address,
    /// The instruction count it is raised at.
    instructions: u64,
    /// Its vector.
    vector: Vector,
}

/// The bare host's settings, what a stop for them carries, and what
/// the run loop looks at only between runs, kept out of its way.
#[derive(Debug, Default)]
pub(crate) struct Bare {
    /// Stop on a fault rather than take it.
    pub(crate) stop_on_fault: bool,
    /// The vectors whose exceptions stop a run before they are taken.
    pub(crate) stop_on_exception: VectorSet,
    /// The vectors the intercept hooks cover.
    pub(crate) intercepted: VectorSet,
    /// An intercepted exception taken natively once.
    pub(crate) native_once: Option<TakeOnce>,
    /// The exception an exception stop ended the last run at, taken
    /// with no stop once.
    pub(crate) exception_once: Option<TakeOnce>,
    /// The end a fault or an intercept set, for `run_limited`.
    pub(crate) end: Option<End>,
    /// The last invalid access's reason, for a fault's cause.
    pub(crate) last_invalid: Option<Invalid>,
    /// The interrupts taken since the last limited run began, and the
    /// traces and hardware breakpoints pending as it began: steps of
    /// their own.
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
    /// `limits.reach`, `limits.instructions` have run, a fault stops it
    /// (`set_stop_on_fault`) or an intercept hook does
    /// (`add_intercept_hook`).  With no cycle budget it runs until one of
    /// those or a stop.
    pub fn run_limited(
        &mut self,
        bus: &mut dyn Bus,
        interrupts: &mut dyn Interrupts,
        now: Cycle,
        limits: Limits,
    ) -> LimitedRun {
        let budget = limits.cycles.unwrap_or(Cycle::MAX.since(now));
        let first = self.instructions;
        self.bare.exceptions_stepped = 0;
        let run = self.run_from_host(bus, interrupts, now, budget, Some(limits));
        let end = self.bare.end.clone().unwrap_or(End::Stop(run.stop));
        LimitedRun {
            cycles: run.cycles,
            end,
            steps: self.instructions - first + self.bare.exceptions_stepped,
        }
    }

    /// Takes the exception of `vector` now, between instructions, as
    /// the host asks: exception processing as the CPU's own (CPU32RM
    /// 6.1.3), SR copied, S set, T1 and T0 cleared, then a four-word
    /// format $0 frame (6.4.1) with SR and PC, and PC from the vector,
    /// at VBR plus its offset (6.1.1).  With `level`, as interrupt
    /// processing does, the interrupt mask is set to it (6.2.11),
    /// whatever the mask was: the mask does not hold it off.  Level 0,
    /// which no interrupt sets, clears the mask.  It ends
    /// STOP and LPSTOP, as an interrupt does (4.4), and leaves loop
    /// mode.  The exception hooks and [`Bus::exception_entered`] see it
    /// as an exception the CPU takes; a hook's stop has no run to stop,
    /// nor has a stop a peripheral asks for in its cycles.  No clocks
    /// are charged.
    ///
    /// A fault stacking the frame or reading the vector is taken as one
    /// in exception processing is: a type IV bus error (6.3.1.4), or a
    /// double bus fault while a bus or address error is being taken
    /// (6.2.2), as one is when `vector` is 2 or 3, until its handler's
    /// first opcode is fetched.
    ///
    /// # Errors
    ///
    /// [`TakeExceptionError::Idle`] for a halted CPU, or one in
    /// background mode, which takes nothing, and
    /// [`TakeExceptionError::Fault`] for a fault that stops rather than
    /// being taken ([`Cpu::set_stop_on_fault`]): no exception entered,
    /// the registers rolled back, but memory and peripherals keep what
    /// the frame's writes before the fault did to them.
    pub fn take_exception(
        &mut self,
        bus: &mut dyn Bus,
        now: Cycle,
        vector: Vector,
        level: Option<InterruptLevel>,
    ) -> Result<(), TakeExceptionError> {
        if let Some(idle) = self.idle {
            return Err(TakeExceptionError::Idle(idle.stop()));
        }
        self.bare.end = None;
        self.began.clone_from(&self.registers);
        self.leave_loop_mode();
        let resume = self.resume;
        let fault_window = self.fault_window;
        if vector == Vector::BUS_ERROR || vector == Vector::ADDRESS_ERROR {
            self.fault_window = true;
        }
        let pc = self.registers.pc;
        Core {
            cpu: self,
            bus,
            now,
        }
        .process(vector, pc, None, level, false);
        self.stop_request = None;
        if let Some(End::Fault(fault)) = self.bare.end.take() {
            // the instruction at PC not begun: its hooks still to run
            self.resume = resume;
            self.fault_window = fault_window;
            return Err(TakeExceptionError::Fault(fault));
        }
        self.stopped = false;
        Ok(())
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

    /// The fault the last run stopped for ([`Stop::Fault`],
    /// [`End::Fault`]), until the next run starts or a reset.
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

    /// The vectors whose exceptions, about to be taken, end a run before
    /// anything is stacked ([`End::Exception`]): [`VectorSet::ALL`] for
    /// any, [`VectorSet::NONE`], as at first, for none.  It covers an
    /// exception an instruction raises (a trap, an illegal
    /// instruction, a fault), a trace, a hardware breakpoint and an
    /// interrupt, once acknowledged, as its vector is then known; not
    /// a fault in exception processing, nor one
    /// [`take_exception`](Self::take_exception) takes.  An instruction's
    /// exception leaves the instruction undone, PC at it, as a fault
    /// stop does ([`FaultStop`]); a trace or a hardware breakpoint is
    /// left pending, the instruction before it kept, and an interrupt
    /// is withdrawn, to be sampled and acknowledged again.  The next
    /// run takes that exception natively, once, with no stop for it
    /// (the host may instead take one of its own with `take_exception`,
    /// and withdraw a trace left pending with
    /// [`set_trace_pending`](Self::set_trace_pending)); another at that
    /// PC, the trap after a withdrawn trace or a higher interrupt
    /// arriving since, stops the run.  The promise is the stopped
    /// exception's: the next exception processed spends it, whether it
    /// matches or not and whatever this setting is then; setting this
    /// leaves it, and reset drops it.  A snapshot holds it: one saved at
    /// the stop takes the exception once wherever it is restored, this
    /// set before or after.  A fault with
    /// [`set_stop_on_fault`](Self::set_stop_on_fault) is a fault stop
    /// ([`End::Fault`]) instead, and an exception the intercept hooks
    /// cover is theirs ([`add_intercept_hook`](Self::add_intercept_hook)):
    /// one they all decline is taken natively, with no stop.  The stop
    /// is [`run_limited`](Self::run_limited)'s; under
    /// [`run_for`](Self::run_for) it is [`Stop::Requested`], with no
    /// vector.
    pub fn set_stop_on_exception(&mut self, vectors: VectorSet) {
        self.bare.stop_on_exception = vectors;
        self.update_capture();
    }

    /// The vectors whose exceptions stop a run.
    #[must_use]
    pub const fn stops_on_exception(&self) -> VectorSet {
        self.bare.stop_on_exception
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

    /// BKPT asserted between instructions: with background mode
    /// enabled the CPU enters it for a breakpoint, PCC the last
    /// instruction executed, out of STOP or LPSTOP too, and this is true
    /// (CPU32RM 7.2.2.1).  Disabled, BKPT asks for a hardware breakpoint
    /// exception, left pending for the next run as
    /// [`set_breakpoint_pending`](Self::set_breakpoint_pending) leaves
    /// it, and this is false.  In background mode already, true with
    /// nothing changed.  Halted, nothing happens, nothing is left
    /// pending, and this is false, background mode enabled or not: only
    /// RESET restarts a halted processor (CPU32RM 5.1, 6.2.2).
    pub fn assert_breakpoint(&mut self) -> bool {
        match self.idle {
            Some(Idle::Background(_)) => true,
            Some(Idle::Halted(_)) => false,
            None if !self.bdm_enabled => {
                self.bare.breakpoint_pending = true;
                false
            }
            None => {
                let pcc = self.instruction_pc;
                self.enter_background(Entry::Breakpoint, pcc);
                true
            }
        }
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
    /// [`assert_breakpoint`](Self::assert_breakpoint).
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

    /// Lets the instruction at PC take an intercepted exception of
    /// `vector` natively, once, the next time it runs: no intercept
    /// hook is called for it, and it is no exception stop
    /// ([`set_stop_on_exception`](Self::set_stop_on_exception)).  An
    /// exception of another vector is intercepted as before.
    pub const fn take_natively_once(&mut self, vector: Vector) {
        self.bare.native_once = Some(self.take_once(self.registers.pc, vector));
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

    /// Whether the instruction at PC is a self-loop
    /// ([`Limits::self_loop`]), untraced, read from the core's memory
    /// with no cycle.
    #[cold]
    #[inline(never)]
    pub(super) fn at_self_loop(&self) -> bool {
        let pc = self.registers.pc;
        if pc.is_odd() || self.registers.sr() & (status::T1 | status::T0) != 0 {
            return false;
        }
        let space = Space::of(self.registers.program_space());
        let word = |offset: u32, size| self.memory.peek(space, pc.offset(offset), size);
        match word(0, Size::Word) {
            Some(BRA_SELF_BYTE) => true,
            Some(BRA_WORD) => {
                word(2, Size::Word).map(|word| Size::Word.sign_extend(word))
                    == Some(SELF_DISPLACEMENT)
            }
            Some(BRA_LONG) => word(2, Size::Long) == Some(SELF_DISPLACEMENT),
            Some(JMP_ABSOLUTE_WORD) => {
                word(2, Size::Word).map(|word| Size::Word.sign_extend(word)) == Some(pc.0)
            }
            Some(JMP_ABSOLUTE_LONG) => word(2, Size::Long) == Some(pc.0),
            _ => false,
        }
    }

    /// Whether the registers are kept as each instruction begins.
    pub(crate) fn update_capture(&mut self) {
        self.capture = self.bare.stop_on_fault
            || !self.bare.stop_on_exception.is_empty()
            || self.hooks.want_registers();
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
        let vector = match (step, fault) {
            (_, Some(fault)) => fault.vector,
            (Err(exception @ (Exception::Short { .. } | Exception::Long { .. })), None) => {
                exception.vector()
            }
            (_, None) => return None,
        };
        // `rte`'s requeued trace is a trace, after the RTE completed
        if vector == Vector::TRACE {
            return None;
        }
        if self.bare.native_once.take() == Some(self.take_once(instruction_pc, vector)) {
            return None;
        }
        if fault.is_none() && self.bare.intercepted.contains(vector) {
            let intercepted = Intercepted {
                vector,
                pc: instruction_pc,
            };
            if !self.hooks.intercept(&intercepted) {
                return None;
            }
            self.undo_instruction(instruction_pc);
            return Some(End::Intercepted(intercepted));
        }
        if !self.exception_stops_at(instruction_pc, vector) {
            return None;
        }
        if fault.is_some() {
            self.roll_back();
        } else {
            self.undo_instruction(instruction_pc);
        }
        Some(End::Exception {
            vector,
            pc: self.registers.pc,
        })
    }

    /// Whether the exception of `vector`, about to be processed at an
    /// instruction boundary with PC at `pc` (a trace, a hardware
    /// breakpoint, an interrupt), stops the run instead
    /// ([`set_stop_on_exception`](Self::set_stop_on_exception)): if so,
    /// the run ends once this instruction ends.
    pub(crate) fn exception_stops(&mut self, vector: Vector, pc: Address) -> bool {
        if !self.exception_stops_at(pc, vector) {
            return false;
        }
        self.end_with(End::Exception { vector, pc });
        true
    }

    /// Whether an exception of `vector` about to be processed with PC
    /// at `pc` stops the run: its vector stops it, and it is not the one
    /// the last run stopped at, which is taken this once.
    /// The promise is spent by the first exception processed after it,
    /// stopping or not, so none outlives the run on.
    fn exception_stops_at(&mut self, pc: Address, vector: Vector) -> bool {
        let once = self.bare.exception_once.take();
        self.bare.stop_on_exception.contains(vector) && once != Some(self.take_once(pc, vector))
    }

    /// The key of an exception of `vector` raised now with PC at `pc`.
    const fn take_once(&self, pc: Address, vector: Vector) -> TakeOnce {
        TakeOnce {
            pc,
            instructions: self.instructions,
            vector,
        }
    }

    /// What a host's run leaves for the next: after an exception stop,
    /// that exception taken with no stop, once.
    pub(crate) fn after_host_run(&mut self) {
        if let Some(End::Exception { vector, pc }) = self.bare.end {
            self.bare.exception_once = Some(self.take_once(pc, vector));
        }
    }

    /// Undoes the instruction at `instruction_pc` that raised an
    /// exception, for a stop before anything is stacked: its (An)+ and
    /// -(An) undone, PC at it, its code and block hooks not called
    /// again when it reruns.
    fn undo_instruction(&mut self, instruction_pc: Address) {
        mem::take(&mut self.altered).undo(&mut self.registers);
        self.registers.pc = instruction_pc;
        self.resume = Some((instruction_pc, Resume::Code));
    }

    /// Rolls the registers back as captured, as the instruction or the
    /// exception processing after one began, for a fault's stop: its
    /// released write and MOVEM progress dropped, its code and block
    /// hooks not called again when it reruns.
    fn roll_back(&mut self) {
        self.registers.clone_from(&self.began);
        self.released = None;
        self.movem = None;
        self.resume = Some((self.registers.pc, Resume::Code));
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
        self.roll_back();
        End::Fault(FaultStop {
            kind: fault.kind,
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
