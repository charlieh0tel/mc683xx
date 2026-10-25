//! What the stub debugs: a CPU32 and what lies around it.
//!
//! [`Target`] is a small trait the bare CPU ([`Machine`]), the chip
//! ([`Mc68331`]) and a board ([`Board`]) implement.  A host with a run
//! loop of its own (the Python bindings, a runner that serves its I/O
//! between quanta) implements it too, usually by delegating to one of
//! these.  The chip's is the `mc68331` feature's and the board's the
//! `board` feature's, both on by default.
//!
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

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::bare::End;
use mc683xx_cpu32::cpu::bare::Limits;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::flat::Machine;

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
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetRun {
    /// It ran its quantum: the cycles passed, executing or, the CPU
    /// stopped by `stop` or `lpstop`, waiting.
    Quantum,
    /// A hook stopped it: the stub's, or the host's own.
    Hook,
    /// The CPU halted (a double bus fault); only a reset restarts it.
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
}

impl From<Stop> for TargetRun {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Budget | Stop::Stopped => Self::Quantum,
            Stop::Halted(_) => Self::Halted,
            Stop::Hook(_) => Self::Hook,
            _ => Self::Requested,
        }
    }
}

/// A byte the stub cannot write: not memory the core serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not memory the core serves")]
pub struct Unwritable(pub Address);

/// What the stub debugs.
///
/// Memory is reached without bus cycles, so a debugger's look or
/// change never has a peripheral's side effects.
pub trait Target {
    /// What ends a session: an error a run cannot go on from (a host
    /// peripheral's).
    type Error;

    /// Calls `call` with the CPU's controls: its registers and hooks to
    /// change, the rest to read.
    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> R;

    /// Fills `data` from `address` up, in supervisor data space, with no
    /// bus cycle and no side effect, as far as it can: the bytes filled,
    /// from the start.  A byte only a cycle could tell (a peripheral's)
    /// ends it.
    fn peek(&mut self, address: Address, data: &mut [u8]) -> usize;

    /// Writes `data` at `address` up, with no bus cycle: memory the core
    /// serves, ROM included, whatever its permissions.
    ///
    /// # Errors
    ///
    /// [`Unwritable`] if a byte is not memory the core serves; then
    /// nothing is written.
    fn poke(&mut self, address: Address, data: &[u8]) -> Result<(), Unwritable>;

    /// Runs for about `cycles` system clocks, or until a hook, a halt or
    /// a request stops it.  `hit` says whether one of the stub's own
    /// hooks has asked to stop: a run loop that goes on past hook stops
    /// (one that calls the host's hooks between core runs) ends the
    /// quantum when it is true.
    ///
    /// # Errors
    ///
    /// The target's error, one a run cannot go on from.
    fn run_for(&mut self, cycles: Cycles, hit: &dyn Fn() -> bool)
    -> Result<TargetRun, Self::Error>;

    /// Resets the CPU as the target resets (the reset exception on the
    /// bare CPU, an external reset on the chip); whether it could.  The
    /// default cannot.
    fn reset(&mut self) -> bool {
        false
    }
}

impl Target for Machine {
    type Error = Infallible;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> R {
        call(&mut CpuControl::new(&mut self.cpu))
    }

    fn peek(&mut self, address: Address, data: &mut [u8]) -> usize {
        peek_bytes(data, |at| {
            let mut byte = [0];
            self.read(address.offset(at), &mut byte)
                .ok()
                .map(|()| byte[0])
        })
    }

    fn poke(&mut self, address: Address, data: &[u8]) -> Result<(), Unwritable> {
        self.write(address, data)
            .map_err(|unmapped| Unwritable(Address(unmapped.0)))
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
            End::Until | End::Instructions => TargetRun::Quantum,
            End::Fault(_) | End::Intercepted(_) => TargetRun::Fault,
            _ => TargetRun::Requested,
        })
    }

    fn reset(&mut self) -> bool {
        // A halt reading the vectors is what the next run reports.
        let _ = Self::reset(self);
        true
    }
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
