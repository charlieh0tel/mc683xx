//! What a bare host asks of the core beyond what the chip does.
//!
//! Runs limited by an address or an instruction count, exceptions the
//! host answers before the CPU stacks anything, faults that stop the
//! run instead of being taken, and, for parity runs against other
//! emulators only, deviations from the CPU32 ([PLAN.md, Bare CPU]).
//! None of it costs a run that does not ask for it.
//!
//! [PLAN.md, Bare CPU]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#bare-cpu

use std::mem;

use super::Cpu;
use super::Halt;
use super::Stop;
use super::hooks::Invalid;
use super::hooks::Resume;
use crate::address::Address;
use crate::bus::Bus;
use crate::cycles::Cycles;
use crate::exception::CycleKind;
use crate::exception::Exception;
use crate::exception::Fault;
use crate::exception::Vector;
use crate::function_code::FunctionCode;
use crate::instructions::Flow;
use crate::interrupt::Interrupts;
use crate::registers::Registers;
use crate::registers::status;
use crate::size::Size;

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

/// A fault that stopped the run ([`Cpu::set_stop_on_fault`]): nothing
/// stacked, the registers back as the faulting instruction (or
/// interrupt entry) began.
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
    /// The registers as the instruction began, as the CPU holds them
    /// again.
    pub registers: Registers,
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

/// The bare host's settings and what a stop for them carries.
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
        let run = self.run_instructions(bus, interrupts, now, budget, Some(limits));
        self.left_pc = Address(self.registers.pc);
        let end = self.bare.end.take().unwrap_or(End::Stop(run.stop));
        LimitedRun {
            cycles: run.cycles,
            end,
        }
    }

    /// Forgets that a hook stopped the last run at PC, so the next run
    /// calls the code and block hooks there again.
    pub const fn clear_resume(&mut self) {
        self.resume = None;
    }

    /// The registers as the last instruction, or interrupt entry, began,
    /// while they are kept: while a read, write or invalid access hook
    /// is installed or faults stop the run.
    #[must_use]
    pub const fn instruction_registers(&self) -> &Registers {
        &self.began
    }

    /// Whether a fault stops a limited run ([`End::Fault`]) instead of
    /// being taken as a bus or address error exception: off at first.
    /// A fault in exception processing stops it too, rather than
    /// halting the CPU in a double bus fault.
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

    /// Whether a fault stops a limited run.
    #[must_use]
    pub const fn stops_on_fault(&self) -> bool {
        self.bare.stop_on_fault
    }

    /// Why the CPU halted, while it stays halted.
    #[must_use]
    pub const fn halted(&self) -> Option<Halt> {
        self.halt
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

    /// A fault's stop: the registers back as the instruction began.
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

    /// Ends the run at `end` once this instruction ends.
    pub(crate) fn end_with(&mut self, end: End) {
        self.bare.end = Some(end);
        self.stop_request.get_or_insert(Stop::Requested);
    }
}
