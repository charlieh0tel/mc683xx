//! A board's control handle and pacing in Python (Rust's
//! `control::BoardControl` and `pacing::Pacing`): a handle another
//! Python thread may hold, and a paced run that releases the GIL while
//! it sleeps.

use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::mem;
use std::num::NonZeroU64;
use std::ops::ControlFlow;

use mc683xx_board::board::Board;

use mc683xx_board::board::BoardRun;
use mc683xx_board::control::BoardControl;
use mc683xx_board::error::RunError;
use mc683xx_board::pacing::Pacer;
use mc683xx_board::pacing::Pacing;
use mc683xx_board::pacing::ParsePacingError;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::ParseKindError;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::PyErr;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::exceptions::PyValueError;
use pyo3::pyclass;
use pyo3::pymethods;

use crate::board::signal;

/// How fast a paced run's board time may go against wall time (Rust's
/// `pacing::Pacing`).
#[pyclass(name = "Pacing", module = "mc683xx", frozen, eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PacingView(pub(crate) Pacing);

#[pymethods]
impl PacingView {
    /// As fast as the host runs it, the default.
    #[classattr]
    const UNPACED: Self = Self(Pacing::Unpaced);

    /// A board second a wall second.
    #[classattr]
    const REAL_TIME: Self = Self(Pacing::RealTime);

    /// `unpaced`, `realtime` or a factor, as text.
    #[new]
    fn new(text: &str) -> PyResult<Self> {
        text.parse()
            .map(Self)
            .map_err(|error: ParsePacingError| PyValueError::new_err(error.to_string()))
    }

    /// `factor` board seconds a wall second: 2.0 twice real time, 0.5
    /// half.  A factor not finite and above 0 raises `ValueError`.
    #[staticmethod]
    fn scaled(factor: f64) -> PyResult<Self> {
        Pacing::Scaled(factor)
            .checked()
            .map(Self)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// Board seconds a wall second, `None` unpaced.
    #[getter]
    const fn factor(&self) -> Option<f64> {
        self.0.factor()
    }

    /// As `==` compares: the variant and the factor.
    fn __hash__(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        mem::discriminant(&self.0).hash(&mut hasher);
        self.0.factor().map(f64::to_bits).hash(&mut hasher);
        hasher.finish()
    }

    fn __str__(&self) -> String {
        self.0.to_string()
    }

    fn __repr__(&self) -> String {
        match self.0 {
            Pacing::RealTime => "Pacing.REAL_TIME".to_owned(),
            Pacing::Scaled(factor) => format!("Pacing.scaled({factor:?})"),
            _ => "Pacing.UNPACED".to_owned(),
        }
    }
}

/// A board's control handle (Rust's `Board::control`): usable from any
/// Python thread while the board runs on its own.  A stop ends the run
/// at its next sync point (`Stop.Requested()`); inputs are driven there,
/// in the order queued; the pacing applies from the next quantum.
#[pyclass(name = "BoardControl", module = "mc683xx", frozen)]
#[derive(Debug, Clone)]
pub(crate) struct BoardControlView(pub(crate) BoardControl);

#[pymethods]
impl BoardControlView {
    /// Asks the run to stop at its next sync point.
    fn stop(&self) {
        self.0.stop();
    }

    /// Whether a stop was asked that no run has taken yet.
    #[getter]
    fn stopping(&self) -> bool {
        self.0.stopping()
    }

    /// Queues `value` for the run-time endpoint `name`, as
    /// `Board.send` drives it, at the run's next sync point; `kind` is
    /// the net's type (`serial` by default).  An endpoint that drives no
    /// net, or none of `kind`, raises `ValueError`, queuing nothing.
    #[pyo3(signature = (name, value, *, kind = "serial"))]
    fn send(&self, name: &str, value: &Bound<'_, PyAny>, kind: &str) -> PyResult<()> {
        let kind: Kind = kind
            .parse()
            .map_err(|error: ParseKindError| PyValueError::new_err(error.to_string()))?;
        self.0
            .send(name, signal(kind, value)?)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// Paces the board's runs from their next quantum.
    fn set_pacing(&self, pacing: PacingView) -> PyResult<()> {
        self.0
            .set_pacing(pacing.0)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// The pacing, `Pacing.UNPACED` until set.
    #[getter]
    fn pacing(&self) -> PacingView {
        PacingView(self.0.pacing())
    }

    fn __repr__(&self) -> String {
        format!("BoardControl(pacing={})", self.0.pacing())
    }
}

/// Runs `board` to `until` in quanta of `quantum` picoseconds, paced by
/// its control (Rust's `Board::run_until_paced`): the GIL released
/// before each quantum, paced or not, and while it sleeps between
/// quanta, so another thread (a UI's `stop` or `send`) runs, and
/// Python's signal handlers run between quanta and sleeps, so Ctrl-C
/// raises `KeyboardInterrupt`, given as the `Err` of the outer result.
/// A run that reaches `until` waits once more after its last quantum.
/// The GIL stays held while a quantum runs: the board holds Python
/// objects and is not `Send`.
pub(crate) fn run_until_paced(
    py: Python<'_>,
    board: &mut Board<Mc68331>,
    until: Time,
    quantum: NonZeroU64,
) -> Result<Result<BoardRun, RunError>, PyErr> {
    let control = board.control();
    let mut pacer = Pacer::new();
    let mut raised = None;
    let mut signals = || match py.check_signals() {
        Ok(()) => ControlFlow::Continue(()),
        Err(error) => {
            raised = Some(error);
            ControlFlow::Break(())
        }
    };
    let mut wait = |now: Time| {
        signals()?;
        pacer.wait_with(now, &control, |duration| {
            py.detach(|| control.sleep(duration));
            signals()
        })
    };
    let outcome = board.run_until_quantized(until, quantum, |board| {
        // a waiting thread's turn, an unpaced run never sleeping
        py.detach(|| ());
        wait(board.now())
    });
    // the last quantum waited out, as Rust's does
    if outcome.is_ok() && board.now() >= until {
        let _ = wait(board.now());
    }
    raised.map_or(Ok(outcome), Err)
}
