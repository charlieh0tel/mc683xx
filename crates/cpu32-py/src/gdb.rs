//! `serve_gdb` on `Machine`, `Mc68331` and `Board` (docs/python.md,
//! Debugging with gdb; docs/gdb.md): a Python harness hands the target
//! to gdb and gets it back when gdb detaches.  The Python peripherals,
//! peripherals and hooks run as in any run; their exceptions end the session
//! and are raised, as is Ctrl-C, while the stub waits for gdb or between
//! quanta.

use std::fmt::Display;

use mc683xx_board::condition::Conditions;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_gdb::SessionError;
use mc683xx_gdb::connection;
use mc683xx_gdb::connection::PollError;
use mc683xx_gdb::connection::Polled;
use mc683xx_gdb::connection::Waiter;
use mc683xx_gdb::target::AccessError;
use mc683xx_gdb::target::Target;
use mc683xx_gdb::target::TargetRun;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::PyErr;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::exceptions::PyRuntimeError;
use pyo3::pyclass;
use pyo3::types::PyAnyMethods;

use crate::mapped::Pending;

/// Where `serve_gdb` waits for gdb unless told: gdb's usual port, on
/// this host only.
pub(crate) const ADDRESS: &str = "127.0.0.1:1234";
/// How a gdb session ended, as `serve_gdb` returns it.
#[pyclass(
    module = "mc683xx",
    eq,
    eq_int,
    hash,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Ending {
    /// gdb detached: the target may run on.
    Detached,
    /// gdb killed the target: the host should stop running it.
    Killed,
}

/// A Rust target whose Python peripherals' exceptions wait in `pending`:
/// raised when the quantum they stopped ends.
pub(crate) struct Checked<'a, T> {
    /// The target.
    pub(crate) target: &'a mut T,
    /// Where its Python objects' exceptions wait.
    pub(crate) pending: &'a Pending,
}

impl<T> Target for Checked<'_, T>
where
    T: Target,
    T::Error: Display,
{
    type Error = PyErr;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        self.target.with_cpu(call)
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<PyErr>> {
        self.target
            .read_memory(address, function_code, data)
            .map_err(|error| error.map(raised_from))
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<PyErr>> {
        self.target
            .write_memory(address, function_code, data)
            .map_err(|error| error.map(raised_from))
    }

    fn run_for(&mut self, cycles: Cycles, hit: &dyn Fn() -> bool) -> PyResult<TargetRun> {
        let ran = self.target.run_for(cycles, hit);
        self.pending.check()?;
        let ran = ran.map_err(raised_from)?;
        Python::attach(|py| py.check_signals())?;
        Ok(ran)
    }

    /// A reset, its Python peripherals' exceptions raised as a run's
    /// are.
    fn reset(&mut self) -> PyResult<bool> {
        let reset = self.target.reset();
        self.pending.check()?;
        let reset = reset.map_err(raised_from)?;
        Python::attach(|py| py.check_signals())?;
        Ok(reset)
    }

    fn conditions(&mut self) -> Option<&mut dyn Conditions> {
        self.target.conditions()
    }
}

/// A Rust target's error as Python's.
fn raised_from(error: impl Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

/// The Python host as the stub waits for gdb: other threads run during
/// each wait (the GIL released; an in-process client among them), and
/// Python's signal handlers between waits, so Ctrl-C ends the session.
pub(crate) struct Signals<'py>(Python<'py>);

impl Waiter for Signals<'_> {
    type Error = PyErr;

    fn wait<R: Send>(&mut self, wait: impl FnOnce() -> R + Send) -> R {
        self.0.detach(wait)
    }

    fn idle(&mut self) -> PyResult<()> {
        self.0.check_signals()
    }
}

/// gdb's connection, for `serve`.
pub(crate) type Connection<'py> = Polled<Signals<'py>>;

/// A polled connection's error as Python's.
fn raised(error: PollError<PyErr>) -> PyErr {
    match error {
        PollError::Connection(error) => error.into(),
        PollError::Host(error) => error,
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Waits for gdb at `address`, calls `ready` with the port it listens
/// on, and takes gdb's connection.  The target is not borrowed yet,
/// so `ready` may look at it.
pub(crate) fn accept<'py>(
    py: Python<'py>,
    address: &str,
    ready: Option<&Bound<'py, PyAny>>,
) -> PyResult<Connection<'py>> {
    connection::accept_polled(
        address,
        |at| ready.map_or(Ok(()), |ready| ready.call1((at.port(),)).map(drop)),
        Signals(py),
    )
    .map_err(raised)
}

/// Serves gdb on `connection` until it detaches or kills `target`.
/// An exception, Ctrl-C's among them, ends the session, the stub's
/// breakpoints and watchpoints removed, and is raised.
pub(crate) fn serve<T: Target<Error = PyErr>>(
    target: &mut T,
    connection: Connection<'_>,
) -> PyResult<Ending> {
    match mc683xx_gdb::serve(target, connection) {
        Ok(mc683xx_gdb::Ending::Detached) => Ok(Ending::Detached),
        Ok(mc683xx_gdb::Ending::Killed) => Ok(Ending::Killed),
        Err(SessionError::Target(error)) => Err(error),
        Err(SessionError::Connection(error)) => Err(raised(error)),
        Err(SessionError::Protocol(text)) => Err(PyRuntimeError::new_err(text)),
        Ok(ending) => Err(PyRuntimeError::new_err(format!("{ending:?}"))),
        Err(error) => Err(PyRuntimeError::new_err(error.to_string())),
    }
}
