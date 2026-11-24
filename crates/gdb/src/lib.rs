//! A GDB remote stub for the CPU32: the bare CPU, the chip, a board,
//! or a CPU32 under BDM control.
//!
//! gdb's remote serial protocol, through the [`gdbstub`] crate, over a
//! [`Target`] ([PLAN.md, Debugger]): [`serve`], on a
//! [`connection::Connection`].  An emulated target gives the CPU's
//! controls, so breakpoints, watchpoints and steps are the core's
//! hooks; [`Bdm`] makes any
//! [`Cpu32Target`](mc683xx_target::Cpu32Target), a BDM pod's on
//! silicon among them, a target whose breakpoints are BGND planted in
//! memory.  [`docs/gdb.md`] is its guide, whose Rust block the
//! `examples` crate runs as a doctest.
//!
//! It costs an emulated run nothing until gdb is attached: its hooks
//! are installed while gdb wants them, and the target runs only inside
//! [`serve`].
//!
//! # Examples
//!
//! A bare CPU served over an in-memory connection, as gdb would ask
//! it why it stopped and then detach:
//!
//! ```
//! use std::collections::VecDeque;
//! use std::io;
//!
//! use mc683xx_cpu32::flat::ADDRESS_BITS;
//! use mc683xx_cpu32::flat::Machine;
//! use mc683xx_gdb::Ending;
//! use mc683xx_gdb::connection::Connection;
//! use mc683xx_gdb::serve;
//!
//! /// gdb's end of the connection: its packets, and the stub's replies.
//! struct Wire {
//!     /// What gdb sends.
//!     input: VecDeque<u8>,
//!     /// What the stub sent.
//!     output: Vec<u8>,
//! }
//!
//! impl Connection for Wire {
//!     type Error = io::Error;
//!
//!     fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
//!         self.output.extend_from_slice(bytes);
//!         Ok(())
//!     }
//!
//!     fn flush(&mut self) -> io::Result<()> {
//!         Ok(())
//!     }
//!
//!     fn read(&mut self) -> io::Result<u8> {
//!         self.input
//!             .pop_front()
//!             .ok_or_else(|| io::ErrorKind::UnexpectedEof.into())
//!     }
//!
//!     fn peek(&mut self) -> io::Result<Option<u8>> {
//!         Ok(self.input.front().copied())
//!     }
//! }
//!
//! let mut machine = Machine::new(ADDRESS_BITS)?;
//! // `?`, why it stopped, then `D`, detach: `$` each body `#` its sum.
//! let mut wire = Wire {
//!     input: b"$?#3f$D#44".iter().copied().collect(),
//!     output: Vec::new(),
//! };
//! let ending = serve(&mut machine, &mut wire)?;
//! assert_eq!(ending, Ending::Detached);
//! assert!(wire.output.starts_with(b"+$T05"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [PLAN.md, Debugger]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#debugger
//! [`docs/gdb.md`]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

use std::fmt;
use std::fmt::Display;

use mc683xx_target::Address;

use crate::connection::Connection;

mod arch;
mod bdm;
pub mod connection;
mod protocol;
mod session;
mod target;

pub use bdm::Bdm;
pub use target::AccessError;
pub use target::NativeStep;
pub use target::Target;
pub use target::TargetReset;
pub use target::TargetRun;

/// How a session ended.
///
/// More endings may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::Ending;
///
/// fn detached(ending: Ending) -> bool {
///     match ending {
///         Ending::Detached => true,
///         Ending::Killed | Ending::HungUp => false,
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ending {
    /// gdb detached: the target may run on without it.
    Detached,
    /// gdb killed the target: the host should stop running it.
    Killed,
    /// gdb hung up: it closed the connection without detaching or
    /// killing the target, as it does when it exits or its link is
    /// lost ([`Connection::hung_up`]).  The target is left as a
    /// session that failed leaves it: an emulated one to its host, one
    /// under BDM control halted.
    HungUp,
}

/// `detached`, `killed` or `hung up`.
impl fmt::Display for Ending {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Detached => "detached",
            Self::Killed => "killed",
            Self::HungUp => "hung up",
        })
    }
}

/// Why a session ended other than as gdb asked.
///
/// More reasons may come, so a match needs a wildcard arm:
///
/// ```compile_fail
/// use mc683xx_gdb::SessionError;
///
/// fn target<E, C>(error: SessionError<E, C>) -> Option<E> {
///     match error {
///         SessionError::Target { error } => Some(error),
///         SessionError::Connection { .. } | SessionError::Protocol(_) => None,
///         SessionError::Unrestored(_) | SessionError::Uncleaned { .. } => None,
///     }
/// }
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError<E, C> {
    /// A call on the target failed: its run, or, under BDM control,
    /// halting it or putting it back as it was.
    #[error("target: {error}")]
    Target {
        /// The target's error.
        error: E,
    },
    /// The connection failed: other than by gdb hanging up, which is
    /// [`Ending::HungUp`].
    #[error("connection: {error}")]
    Connection {
        /// The connection's error.
        error: C,
    },
    /// gdb said something the stub could not take.
    #[error("protocol: {0}")]
    Protocol(String),
    /// Breakpoints planted that the stub could not put back, BGND read
    /// back or not read at all, at these addresses; the target is left
    /// halted, not resumed.
    #[error("breakpoints still planted at {}", listed(.0))]
    Unrestored(Vec<Address>),
    /// The session failed, and so did cleaning up after it: halting a
    /// target under BDM control or removing its breakpoints.  The
    /// target is left halted, not resumed.
    #[error("{}; cleaning up: {}", shown(error), shown(cleanup))]
    Uncleaned {
        /// The session's failure: `Target`, `Connection` or `Protocol`.
        error: Box<SessionError<E, C>>,
        /// Cleaning up's: `Target` or `Unrestored`.
        cleanup: Box<SessionError<E, C>>,
    },
}

/// `error` as its message: a nested session error, which thiserror
/// would otherwise bound on its own `Display`, an unprovable cycle.
fn shown<E: Display, C: Display>(error: &SessionError<E, C>) -> String {
    error.to_string()
}

/// `addresses`, comma-separated.
fn listed(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Serves gdb on `connection` over `target` until gdb detaches, kills
/// it or hangs up.
///
/// The target is halted first, so gdb finds it stopped, and stays
/// where it is until gdb continues or steps it.  When the session
/// ends, however it ends, every hook the stub installed is removed, and
/// a target under BDM control is halted and every breakpoint still
/// planted removed.  Detach resumes a target under BDM control and
/// leaves an emulated one to its host; kill leaves either halted, and
/// so does a hang-up a target under BDM control.
///
/// # Errors
///
/// [`SessionError::Target`] if a call on the target fails,
/// [`SessionError::Connection`] if the connection fails other than by
/// gdb hanging up, [`SessionError::Protocol`] if gdb sends what the stub cannot
/// take, and [`SessionError::Unrestored`] if a breakpoint planted
/// cannot be put back; then a detach does not resume the target.  A
/// session that failed and then failed to clean up reports both,
/// [`SessionError::Uncleaned`].
pub fn serve<T: Target, C: Connection>(
    target: &mut T,
    connection: C,
) -> Result<Ending, SessionError<T::Error, C::Error>> {
    session::serve(target, connection)
}

#[cfg(test)]
mod tests {
    use super::Ending;

    #[test]
    fn an_ending_prints_its_name() {
        assert_eq!(Ending::Detached.to_string(), "detached");
        assert_eq!(Ending::Killed.to_string(), "killed");
        assert_eq!(Ending::HungUp.to_string(), "hung up");
    }
}
