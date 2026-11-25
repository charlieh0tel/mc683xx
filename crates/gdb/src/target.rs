//! What the stub debugs: a CPU32 and what lies around it.
//!
//! [`Target`] is the one trait [`serve`](crate::serve) takes.  A target
//! is one of two kinds, and the trait's defaults are for the first:
//!
//! - **Emulated**: the bare CPU ([`Machine`]), the chip ([`Mc68331`]),
//!   a board ([`Board`]), or a host's run loop around one (the Python
//!   bindings, a runner that serves its I/O between quanta).  It gives
//!   the CPU's controls ([`Target::cpu_scope`]), so the stub's
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
#![cfg_attr(feature = "mc68331", doc = "[`Mc68331`]: mc683xx_mc68331::Mc68331")]
#![cfg_attr(
    not(feature = "mc68331"),
    doc = "[`Mc68331`]: https://docs.rs/mc683xx-mc68331"
)]
#![cfg_attr(feature = "board", doc = "[`Board`]: mc683xx_board::Board")]
#![cfg_attr(
    not(feature = "board"),
    doc = "[`Board`]: https://docs.rs/mc683xx-board"
)]

use std::convert::Infallible;

#[cfg(feature = "board")]
use mc683xx_board::condition::Conditions;
use mc683xx_cpu32::CpuControl;
use mc683xx_cpu32::Cycles;
use mc683xx_cpu32::End;
use mc683xx_cpu32::Limits;
use mc683xx_cpu32::Stop;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::flat::Machine;
use mc683xx_target::Address;
use mc683xx_target::Entry;
use mc683xx_target::FunctionCode;
use mc683xx_target::Register;
use mc683xx_target::SystemRegister;

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
/// use mc683xx_gdb::TargetRun;
///
/// fn ran_on(ran: TargetRun) -> bool {
///     match ran {
///         TargetRun::Quantum | TargetRun::Hook => true,
///         TargetRun::Halted | TargetRun::Requested | TargetRun::Fault => false,
///         TargetRun::Condition | TargetRun::Background(_) => false,
///         TargetRun::SelfLoop => false,
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
    /// A fault stopped it rather than being taken
    /// ([`Cpu::set_stop_on_fault`]), or, on the bare CPU, an exception
    /// its host handles did ([`Cpu::add_intercept_hook`]).
    ///
    /// [`Cpu::set_stop_on_fault`]: mc683xx_cpu32::Cpu::set_stop_on_fault
    /// [`Cpu::add_intercept_hook`]: mc683xx_cpu32::Cpu::add_intercept_hook
    Fault,
    /// On the bare CPU, an exception it was about to take stopped it
    /// ([`Cpu::set_stop_on_exception`]).
    ///
    /// [`Cpu::set_stop_on_exception`]: mc683xx_cpu32::Cpu::set_stop_on_exception
    Exception {
        /// The exception's vector.
        vector: Vector,
    },
    /// A stop condition held ([`Target::conditions`]).
    Condition,
    /// A target under BDM control entered background mode, for the
    /// entry given.
    Background(Entry),
    /// A self-loop ended it, the host having set
    /// [`Cpu::set_stop_at_self_loop`]: PC at the loop.
    ///
    /// [`Cpu::set_stop_at_self_loop`]: mc683xx_cpu32::Cpu::set_stop_at_self_loop
    SelfLoop,
}

impl From<Stop> for TargetRun {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Budget | Stop::Stopped => Self::Quantum,
            Stop::Halted(_) | Stop::Background(_) => Self::Halted,
            Stop::Hook(_) => Self::Hook,
            Stop::Fault => Self::Fault,
            Stop::SelfLoop => Self::SelfLoop,
            _ => Self::Requested,
        }
    }
}

/// Whether a [`Target::reset`] reset the target.
///
/// More answers may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::TargetReset;
///
/// fn reset(answer: TargetReset) -> bool {
///     match answer {
///         TargetReset::Done => true,
///         TargetReset::Unsupported => false,
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetReset {
    /// The target reset.
    Done,
    /// The target cannot reset.
    Unsupported,
}

/// Why a register or memory access failed.
///
/// More reasons may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::AccessError;
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
    #[error("no memory at {0}")]
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

/// A target's own step ([`Target::native_step`]).
pub trait NativeStep {
    /// The target's error.
    type Error: std::error::Error;

    /// One step: how it stopped, [`TargetRun::Background`] for a
    /// target under BDM control.
    ///
    /// # Errors
    ///
    /// The target's error.
    fn step(&mut self) -> Result<TargetRun, Self::Error>;
}

/// What the stub debugs.
///
/// An emulated target implements [`cpu_scope`](Self::cpu_scope),
/// [`read_memory`](Self::read_memory), [`write_memory`](Self::write_memory)
/// and [`run_for`](Self::run_for), and [`reset`](Self::reset) if it
/// can; the rest have its defaults, and it steps by the stub's hook.  A target under BDM control is
/// [`Bdm`](crate::bdm::Bdm), which implements them all.
///
/// An emulated target's memory is reached without bus cycles, so a
/// debugger's look or change never has a peripheral's side effects.
pub trait Target {
    /// What ends a session: an error a run cannot go on from (a host
    /// peripheral's, a pod's lost link).
    type Error: std::error::Error;

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
    fn cpu_scope<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        None
    }

    /// A data or address register.  By default through
    /// [`cpu_scope`](Self::cpu_scope).
    ///
    /// # Errors
    ///
    /// [`AccessError::Unreachable`] by default for a target without
    /// the CPU's controls; the target's error for one under BDM
    /// control.
    fn read_register(&mut self, register: Register) -> Result<u32, AccessError<Self::Error>> {
        self.cpu_scope(|cpu| get(&cpu.registers, Location::Register(register)))
            .ok_or(AccessError::Unreachable)
    }

    /// Writes a data or address register.  By default through
    /// [`cpu_scope`](Self::cpu_scope).
    ///
    /// # Errors
    ///
    /// As [`read_register`](Self::read_register).
    fn write_register(
        &mut self,
        register: Register,
        value: u32,
    ) -> Result<(), AccessError<Self::Error>> {
        self.cpu_scope(|cpu| set(cpu.registers_mut(), Location::Register(register), value))
            .ok_or(AccessError::Unreachable)
    }

    /// A system register, as BDM names them (CPU32RM 7.2.8): the PC is
    /// RPC.  By default through [`cpu_scope`](Self::cpu_scope), where
    /// PCC, ATEMP and FAR read zero.
    ///
    /// # Errors
    ///
    /// As [`read_register`](Self::read_register).
    fn read_system_register(
        &mut self,
        register: SystemRegister,
    ) -> Result<u32, AccessError<Self::Error>> {
        self.cpu_scope(|cpu| get(&cpu.registers, Location::System(register)))
            .ok_or(AccessError::Unreachable)
    }

    /// Writes a system register.  By default through
    /// [`cpu_scope`](Self::cpu_scope): SR's low word, SFC's and DFC's low
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
        self.cpu_scope(|cpu| set(cpu.registers_mut(), Location::System(register), value))
            .ok_or(AccessError::Unreachable)
    }

    /// Fills `data` from `address` up, in the address space
    /// `function_code` names, as far as it can: the bytes filled, from
    /// the start.  An emulated target reads with no bus cycle and no
    /// side effect, a peripheral's bytes by its
    /// [`MappedPeripheral::peek`], a byte only a cycle could tell (one a
    /// peripheral declines) ending it; one under BDM control fills all
    /// of `data` or fails.
    ///
    /// [`MappedPeripheral::peek`]: mc683xx_cpu32::peripheral::MappedPeripheral::peek
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
    /// permissions, and a peripheral's bytes by its
    /// [`MappedPeripheral::poke`].
    ///
    /// # Errors
    ///
    /// [`AccessError::Unserved`] if a byte is neither memory the target
    /// serves in that space nor a peripheral's that takes it; nothing
    /// is then written, but for the bytes before one a peripheral
    /// declined.  The target's error, for one under BDM control.
    ///
    /// [`MappedPeripheral::poke`]: mc683xx_cpu32::peripheral::MappedPeripheral::poke
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

    /// Runs an emulated target for about `budget` system clocks, or
    /// until a hook, a halt or a request stops it; or for none, as
    /// [`TargetRun::Quantum`], if the host holds it back (to pace it
    /// against wall time), the stub then looking for gdb's bytes.  `hit` says whether
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
    fn run_for(&mut self, budget: Cycles, hit: &dyn Fn() -> bool)
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

    /// The target's own step, for a target that steps by its own means
    /// (one under BDM control).  Without one, the default, the stub
    /// steps the target with a hook, which needs
    /// [`cpu_scope`](Self::cpu_scope); a target with neither cannot step,
    /// and gdb is offered none.
    fn native_step(&mut self) -> Option<&mut dyn NativeStep<Error = Self::Error>> {
        None
    }

    /// Resets the CPU as the target resets (the reset exception on the
    /// bare CPU, an external reset on the chip, a reset halted before
    /// the first instruction over BDM); whether it could.  The default
    /// cannot, [`TargetReset::Unsupported`].
    ///
    /// # Errors
    ///
    /// The target's error.
    fn reset(&mut self) -> Result<TargetReset, Self::Error> {
        Ok(TargetReset::Unsupported)
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

/// The bare CPU: one address space for memory, whatever the function
/// code, which a peripheral's peek or poke sees.
impl Target for Machine {
    type Error = Infallible;

    fn cpu_scope<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        Some(call(&mut CpuControl::new(&mut self.cpu)))
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<Infallible>> {
        Ok(match self.peek_bytes(address, function_code, data) {
            Ok(()) => data.len(),
            Err(error) => error.address().0.wrapping_sub(address.0) as usize,
        })
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<Infallible>> {
        self.poke_bytes(address, function_code, data)
            .map_err(|error| AccessError::Unserved(error.address()))
    }

    fn run_for(
        &mut self,
        budget: Cycles,
        _hit: &dyn Fn() -> bool,
    ) -> Result<TargetRun, Infallible> {
        let mut limits = Limits::default();
        limits.cycles = Some(budget);
        let run = Self::run_limited(self, limits);
        Ok(match run.end {
            End::Stop(stop) => stop.into(),
            End::Reached | End::Instructions | End::Steps => TargetRun::Quantum,
            End::Fault(_) | End::Intercepted(_) => TargetRun::Fault,
            End::Exception { vector, .. } => TargetRun::Exception { vector },
            End::SelfLoop { .. } => TargetRun::SelfLoop,
            _ => TargetRun::Requested,
        })
    }

    fn reset(&mut self) -> Result<TargetReset, Infallible> {
        // A halt reading the vectors is what the next run reports.
        Self::reset(self);
        Ok(TargetReset::Done)
    }
}

/// An infallible target's access error as a fallible one's.
#[cfg(feature = "board")]
pub(crate) fn widen<E>(error: AccessError<Infallible>) -> AccessError<E> {
    error.map(|never| match never {})
}
