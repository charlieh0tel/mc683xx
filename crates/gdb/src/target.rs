//! What the stub debugs: a CPU32 and what lies around it.
//!
//! [`Target`] is the one trait [`serve`](crate::serve) takes.  A target
//! is one of two kinds, and the trait's defaults are for the first:
//!
//! - **Emulated**: the bare CPU ([`Machine`]), the chip ([`Mc68331`]),
//!   a board ([`Board`]), or a host's run loop around one (the Python
//!   bindings, a runner that serves its I/O between quanta).  It gives
//!   the CPU's controls ([`Target::with_cpu`]), so the stub's
//!   breakpoints, watchpoints and steps are the core's hooks, and it
//!   runs only as the stub runs it, a quantum at a time
//!   ([`Target::run_for`]).
//! - **Under BDM control**: [`Bdm`] over any [`Cpu32Target`], a pod's
//!   on silicon or the emulated chip's.  It runs on its own once
//!   resumed; a software breakpoint is BGND written into memory and a
//!   step is the target's own.
//!
//! The chip's impl is the `mc68331` feature's and the board's the
//! `board` feature's, both on by default.
//!
//! [`Bdm`]: crate::bdm::Bdm
//! [`Cpu32Target`]: mc683xx_target::Cpu32Target
#![cfg_attr(
    feature = "mc68331",
    doc = "[`Mc68331`]: mc683xx_mc68331::chip::Mc68331"
)]
#![cfg_attr(
    not(feature = "mc68331"),
    doc = "[`Mc68331`]: https://docs.rs/mc683xx-mc68331"
)]
#![cfg_attr(feature = "board", doc = "[`Board`]: mc683xx_board::board::Board")]
#![cfg_attr(
    not(feature = "board"),
    doc = "[`Board`]: https://docs.rs/mc683xx-board"
)]

use std::convert::Infallible;

#[cfg(feature = "board")]
use mc683xx_board::condition::Conditions;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::bare::End;
use mc683xx_cpu32::cpu::bare::Limits;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::flat::Machine;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;

use crate::arch::Location;
use crate::arch::get;
use crate::arch::set;

#[cfg(feature = "board")]
mod board;
#[cfg(feature = "mc68331")]
mod chip;

/// How a quantum of a run ended.
///
/// More ends may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::target::TargetRun;
///
/// fn ran_on(ran: TargetRun) -> bool {
///     match ran {
///         TargetRun::Quantum | TargetRun::Hook => true,
///         TargetRun::Halted | TargetRun::Requested | TargetRun::Fault => false,
///         TargetRun::Condition | TargetRun::Background(_) => false,
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetRun {
    /// It ran its quantum: the cycles passed, executing or, the CPU
    /// stopped by `stop` or `lpstop`, waiting.  A target under BDM
    /// control is still running.
    Quantum,
    /// A hook stopped it: the stub's, or the host's own.
    Hook,
    /// The CPU halted (a double bus fault), or is in background mode
    /// with no BDM debugger to resume it; only a reset restarts it.
    Halted,
    /// The host or a peripheral asked it to stop.
    Requested,
    /// A fault stopped it rather than being taken (the bare CPU's
    /// [`Cpu::set_stop_on_fault`]), or an intercepted exception did
    /// ([`Cpu::set_intercepted`]).
    ///
    /// [`Cpu::set_stop_on_fault`]: mc683xx_cpu32::cpu::Cpu::set_stop_on_fault
    /// [`Cpu::set_intercepted`]: mc683xx_cpu32::cpu::Cpu::set_intercepted
    Fault,
    /// A stop condition held ([`Target::conditions`]).
    Condition,
    /// A target under BDM control entered background mode, for the
    /// entry given.
    Background(Entry),
}

impl From<Stop> for TargetRun {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Budget | Stop::Stopped => Self::Quantum,
            Stop::Halted(_) | Stop::Background(_) => Self::Halted,
            Stop::Hook(_) => Self::Hook,
            _ => Self::Requested,
        }
    }
}

/// Why a register or memory access failed.
///
/// More reasons may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::target::AccessError;
///
/// fn target<E>(error: AccessError<E>) -> Option<E> {
///     match error {
///         AccessError::Target(error) => Some(error),
///         AccessError::Unserved(_) | AccessError::Unreachable => None,
///     }
/// }
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AccessError<E> {
    /// The byte at this address is not memory the target serves, in
    /// the address space asked.
    #[error("{0} is not memory the target serves")]
    Unserved(Address),
    /// The target gives no access to its registers: neither its CPU's
    /// controls nor register methods of its own.
    #[error("the target's registers cannot be reached")]
    Unreachable,
    /// The target's own error.
    #[error(transparent)]
    Target(E),
}

impl<E> AccessError<E> {
    /// The same error, the target's own changed by `change`: a host
    /// that wraps a target, its error with it.
    pub fn map<F>(self, change: impl FnOnce(E) -> F) -> AccessError<F> {
        match self {
            Self::Unserved(address) => AccessError::Unserved(address),
            Self::Unreachable => AccessError::Unreachable,
            Self::Target(error) => AccessError::Target(change(error)),
        }
    }
}

/// What the stub debugs.
///
/// An emulated target implements [`with_cpu`](Self::with_cpu),
/// [`read_memory`](Self::read_memory), [`write_memory`](Self::write_memory)
/// and [`run_for`](Self::run_for), and [`reset`](Self::reset) if it
/// can; the rest have its defaults.  A target under BDM control is
/// [`Bdm`](crate::bdm::Bdm), which implements them all.
///
/// An emulated target's memory is reached without bus cycles, so a
/// debugger's look or change never has a peripheral's side effects.
pub trait Target {
    /// What ends a session: an error a run cannot go on from (a host
    /// peripheral's, a pod's lost link).
    type Error;

    /// Calls `call` with the CPU's controls, its registers and hooks to
    /// change and the rest to read: `Some` of what it returns for an
    /// emulated target.  With them, the stub's breakpoints, watchpoints
    /// and steps are hooks, and the register methods' defaults reach
    /// the registers through them.  The default, for a target under BDM
    /// control, is `None`.
    #[expect(
        unused_variables,
        reason = "the default ignores its argument, and the name documents it"
    )]
    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        None
    }

    /// A data or address register.  By default through
    /// [`with_cpu`](Self::with_cpu).
    ///
    /// # Errors
    ///
    /// [`AccessError::Unreachable`] by default for a target without
    /// the CPU's controls; the target's error for one under BDM
    /// control.
    fn read_register(&mut self, register: Register) -> Result<u32, AccessError<Self::Error>> {
        self.with_cpu(|cpu| get(&cpu.registers, Location::Register(register)))
            .ok_or(AccessError::Unreachable)
    }

    /// Writes a data or address register.  By default through
    /// [`with_cpu`](Self::with_cpu).
    ///
    /// # Errors
    ///
    /// As [`read_register`](Self::read_register).
    fn write_register(
        &mut self,
        register: Register,
        value: u32,
    ) -> Result<(), AccessError<Self::Error>> {
        self.with_cpu(|cpu| set(cpu.registers_mut(), Location::Register(register), value))
            .ok_or(AccessError::Unreachable)
    }

    /// A system register, as BDM names them (CPU32RM 7.2.8): the PC is
    /// RPC.  By default through [`with_cpu`](Self::with_cpu), where
    /// PCC, ATEMP and FAR read zero.
    ///
    /// # Errors
    ///
    /// As [`read_register`](Self::read_register).
    fn read_system_register(
        &mut self,
        register: SystemRegister,
    ) -> Result<u32, AccessError<Self::Error>> {
        self.with_cpu(|cpu| get(&cpu.registers, Location::System(register)))
            .ok_or(AccessError::Unreachable)
    }

    /// Writes a system register.  By default through
    /// [`with_cpu`](Self::with_cpu): SR's low word, SFC's and DFC's low
    /// three bits, and PCC, ATEMP and FAR not at all.
    ///
    /// # Errors
    ///
    /// As [`read_register`](Self::read_register).
    fn write_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), AccessError<Self::Error>> {
        self.with_cpu(|cpu| set(cpu.registers_mut(), Location::System(register), value))
            .ok_or(AccessError::Unreachable)
    }

    /// Fills `data` from `address` up, in the address space
    /// `function_code` names, as far as it can: the bytes filled, from
    /// the start.  An emulated target reads with no bus cycle and no
    /// side effect, a byte only a cycle could tell (a peripheral's)
    /// ending it; one under BDM control fills all of `data` or fails.
    ///
    /// # Errors
    ///
    /// The target's error, for one under BDM control.
    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<Self::Error>>;

    /// Writes `data` at `address` up, in the address space
    /// `function_code` names.  An emulated target writes with no bus
    /// cycle, into memory the core serves, ROM included, whatever its
    /// permissions.
    ///
    /// # Errors
    ///
    /// [`AccessError::Unserved`] if a byte is not memory the target
    /// serves in that space, nothing then written; the target's error,
    /// for one under BDM control.
    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<Self::Error>>;

    /// Sets the target going, as gdb continues it.  The default, for
    /// an emulated target, does nothing: it runs as the stub runs it.
    ///
    /// # Errors
    ///
    /// The target's error.
    fn resume(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Runs an emulated target for about `cycles` system clocks, or
    /// until a hook, a halt or a request stops it.  `hit` says whether
    /// one of the stub's own hooks has asked to stop: a run loop that
    /// goes on past hook stops (one that calls the host's hooks between
    /// core runs) ends the quantum when it is true.
    ///
    /// For a target under BDM control, a look at it, with a short wait
    /// if it is still running: [`TargetRun::Background`] once it has
    /// stopped, else [`TargetRun::Quantum`].
    ///
    /// # Errors
    ///
    /// The target's error, one a run cannot go on from.
    fn run_for(&mut self, cycles: Cycles, hit: &dyn Fn() -> bool)
    -> Result<TargetRun, Self::Error>;

    /// Stops a running target, for gdb's Ctrl-C: how it stopped.  The
    /// default, for an emulated target, which is stopped already
    /// between the stub's quanta, is [`TargetRun::Requested`].
    ///
    /// # Errors
    ///
    /// The target's error.
    fn halt(&mut self) -> Result<TargetRun, Self::Error> {
        Ok(TargetRun::Requested)
    }

    /// Steps the target by its own means: `Some` of how it stopped.
    /// The default, `None`, has the stub step it with a hook, which
    /// needs [`with_cpu`](Self::with_cpu).
    ///
    /// # Errors
    ///
    /// The target's error.
    fn step(&mut self) -> Result<Option<TargetRun>, Self::Error> {
        Ok(None)
    }

    /// Resets the CPU as the target resets (the reset exception on the
    /// bare CPU, an external reset on the chip, a reset halted before
    /// the first instruction over BDM); whether it could.  The default
    /// cannot.
    ///
    /// # Errors
    ///
    /// The target's error.
    fn reset(&mut self) -> Result<bool, Self::Error> {
        Ok(false)
    }

    /// The target's stop conditions, for `monitor stop`
    /// ([docs/gdb.md, Stop conditions][conditions]): a board's.  A run
    /// that stops for one ends [`TargetRun::Condition`].  The default
    /// has none.
    ///
    /// [conditions]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md#stop-conditions
    #[cfg(feature = "board")]
    fn conditions(&mut self) -> Option<&mut dyn Conditions> {
        None
    }
}

/// The bare CPU: one address space, whatever the function code.
impl Target for Machine {
    type Error = Infallible;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        Some(call(&mut CpuControl::new(&mut self.cpu)))
    }

    fn read_memory(
        &mut self,
        address: Address,
        _function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<Infallible>> {
        Ok(peek_bytes(data, |at| {
            let mut byte = [0];
            self.read(address.offset(at), &mut byte)
                .ok()
                .map(|()| byte[0])
        }))
    }

    fn write_memory(
        &mut self,
        address: Address,
        _function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<Infallible>> {
        self.write(address, data)
            .map_err(|unmapped| AccessError::Unserved(Address(unmapped.0)))
    }

    fn run_for(
        &mut self,
        cycles: Cycles,
        _hit: &dyn Fn() -> bool,
    ) -> Result<TargetRun, Infallible> {
        let mut limits = Limits::default();
        limits.cycles = Some(cycles);
        let run = Self::run_limited(self, limits);
        Ok(match run.end {
            End::Stop(stop) => stop.into(),
            End::Until | End::Instructions | End::Steps => TargetRun::Quantum,
            End::Fault(_) | End::Intercepted(_) => TargetRun::Fault,
            _ => TargetRun::Requested,
        })
    }

    fn reset(&mut self) -> Result<bool, Infallible> {
        // A halt reading the vectors is what the next run reports.
        let _ = Self::reset(self);
        Ok(true)
    }
}

/// An infallible target's access error as a fallible one's.
#[cfg(feature = "board")]
pub(crate) fn widen<E>(error: AccessError<Infallible>) -> AccessError<E> {
    error.map(|never| match never {})
}

/// Fills `data` with `byte(offset)` for each offset until one is `None`:
/// the bytes filled.
fn peek_bytes(data: &mut [u8], mut byte: impl FnMut(u32) -> Option<u8>) -> usize {
    for (offset, slot) in data.iter_mut().enumerate() {
        match byte(offset as u32) {
            Some(value) => *slot = value,
            None => return offset,
        }
    }
    data.len()
}
