//! A board's control handle and pacing in Python (Rust's
//! `control::BoardControl` and `pacing::Pacing`): a handle another
//! Python thread may hold, and a paced run that releases the GIL while
//! it sleeps.

use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::mem;
use std::ops::ControlFlow;
use std::time::Duration;
use std::time::Instant;

use mc683xx_board::board::Board;

use mc683xx_board::board::BoardRun;
use mc683xx_board::control::BoardControl;
use mc683xx_board::error::RunError;
use mc683xx_board::pacing::Pacer;
use mc683xx_board::pacing::Pacing;
use mc683xx_board::pacing::ParsePacingError;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::ParseKindError;
use mc683xx_board::time::NonZeroSpan;
use mc683xx_board::time::Span;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::PyErr;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::exceptions::PyValueError;
use pyo3::marker::Ungil;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;

use crate::board::signal;
use crate::python::repr;

/// How fast a paced run's board time may go against wall time (Rust's
/// `pacing::Pacing`), a class per variant.
#[pyclass(name = "Pacing", module = "mc683xx", frozen, eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PacingView {
    /// As fast as the host runs it, the default.
    Unpaced(),
    /// A board second a wall second.
    RealTime(),
    /// `factor` board seconds a wall second: 2.0 twice real time, 0.5
    /// half.  Finite and above 0, or a run or `set_pacing` given it
    /// raises `ValueError`.
    Scaled {
        /// Board seconds a wall second.
        factor: f64,
    },
}

impl PacingView {
    /// Rust's pacing, its factor unchecked.
    const fn unchecked(self) -> Pacing {
        match self {
            Self::Unpaced() => Pacing::Unpaced,
            Self::RealTime() => Pacing::RealTime,
            Self::Scaled { factor } => Pacing::Scaled(factor),
        }
    }

    /// Rust's pacing; a factor not finite and above 0 raises
    /// `ValueError`.
    pub(crate) fn pacing(self) -> PyResult<Pacing> {
        self.unchecked()
            .checked()
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }
}

impl From<Pacing> for PacingView {
    fn from(pacing: Pacing) -> Self {
        match pacing {
            Pacing::Unpaced => Self::Unpaced(),
            Pacing::RealTime => Self::RealTime(),
            Pacing::Scaled(factor) => Self::Scaled { factor },
            // a pacing the bindings do not know, by its factor
            other => other
                .factor()
                .map_or(Self::Unpaced(), |factor| Self::Scaled { factor }),
        }
    }
}

#[pymethods]
impl PacingView {
    /// `unpaced`, `realtime` or a factor, as text (Rust's `FromStr`, the
    /// runner's `--pace`).
    #[staticmethod]
    fn parse(text: &str) -> PyResult<Self> {
        text.parse()
            .map(|pacing: Pacing| pacing.into())
            .map_err(|error: ParsePacingError| PyValueError::new_err(error.to_string()))
    }

    /// Board seconds a wall second, `None` unpaced (Rust's
    /// `Pacing::factor`).
    #[getter]
    const fn factor(&self) -> Option<f64> {
        self.unchecked().factor()
    }

    /// As `==` compares: the variant and the factor.
    fn __hash__(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        mem::discriminant(self).hash(&mut hasher);
        self.factor().map(f64::to_bits).hash(&mut hasher);
        hasher.finish()
    }

    /// Its text, as `parse` takes it.
    fn __str__(&self) -> String {
        self.unchecked().to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(match self {
            Self::Unpaced() => "Pacing.Unpaced()".to_owned(),
            Self::RealTime() => "Pacing.RealTime()".to_owned(),
            Self::Scaled { factor } => format!("Pacing.Scaled({})", repr(py, *factor)?),
        })
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
            .set_pacing(pacing.pacing()?)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// The pacing, `Pacing.Unpaced()` until set.
    #[getter]
    fn pacing(&self) -> PacingView {
        self.0.pacing().into()
    }

    fn __repr__(&self) -> String {
        format!("BoardControl(pacing={})", self.0.pacing())
    }
}

/// Runs `board` to `until` in quanta of `quantum`, paced by
/// its control (Rust's `Board::run_until_paced`): the GIL released
/// between quanta, paced or not, once held two switch intervals
/// ([`Turns`]), and while it sleeps between quanta, so another thread
/// (a UI's `stop` or `send`) runs, and
/// Python's signal handlers run between quanta and sleeps, so Ctrl-C
/// raises `KeyboardInterrupt`, given as the `Err` of the outer result.
/// A run that reaches `until` waits once more after its last quantum.
/// The GIL stays held while a quantum runs: the board holds Python
/// objects and is not `Send`.
pub(crate) fn run_until_paced(
    py: Python<'_>,
    board: &mut Board<Mc68331>,
    until: Time,
    quantum: NonZeroSpan,
) -> Result<Result<BoardRun, RunError>, PyErr> {
    let control = board.control();
    let mut pacer = Pacer::new();
    let mut turns = Turns::new(py)?;
    let mut pace = |turns: &mut Turns<'_>, now: Time| {
        pacer.wait_with(now, &control, |duration| {
            turns.detach(|| control.sleep(duration));
            turns.signals()
        })
    };
    let outcome = board.run_until_quantized(until, quantum, |board| {
        turns.turn()?;
        pace(&mut turns, board.now())
    });
    // the last quantum waited out, as Rust's does
    if outcome.is_ok() && board.now() >= until && turns.signals().is_continue() {
        let _ = pace(&mut turns, board.now());
    }
    turns.ended(outcome)
}

/// Runs a frame of `board` (Rust's `Board::run_frame`): to the board
/// time due by `wall` under its control's pacing, but no more than
/// `bound`, the GIL released between quanta and Python's
/// signal handlers run, as [`run_until_paced`] does, but never sleeping.
pub(crate) fn run_frame(
    py: Python<'_>,
    board: &mut Board<Mc68331>,
    wall: Duration,
    bound: Span,
) -> Result<Result<BoardRun, RunError>, PyErr> {
    let mut turns = Turns::new(py)?;
    let outcome = board.run_frame_with(wall, bound, |_| turns.turn());
    turns.ended(outcome)
}

/// What a run in Python does between its quanta: another thread's turn
/// at the GIL, and Python's signal handlers, the first exception one
/// raises kept to end the run with.
///
/// A thread waiting for the GIL asks its holder to drop it only when a
/// whole switch interval (`sys.getswitchinterval()`) passes with no
/// other thread taking it, and each release wakes it to wait a whole
/// interval again; a release finds the ask and hands the GIL over,
/// waiting until the other thread holds it.  A release followed at once
/// by the take back is no handoff, the waiting thread rarely winning
/// that race: released every quantum, the GIL was never asked for, and
/// another thread starved for as long as the run.  So a turn releases
/// it only once twice the interval has passed since the last, and the
/// thread waiting gets it within about two intervals.  The interval is
/// read again each time the GIL comes back: another thread can set it
/// only while it holds the GIL, so a change during the run applies from
/// the next release on.
struct Turns<'py> {
    /// The GIL, held.
    py: Python<'py>,
    /// `sys.getswitchinterval`.
    switch_interval: Bound<'py, PyAny>,
    /// What a signal handler, or reading the switch interval, raised.
    raised: Option<PyErr>,
    /// When a turn last released the GIL, or the run started.
    released: Instant,
    /// How long the GIL is held between releases: twice the switch
    /// interval.
    held: Duration,
}

impl<'py> Turns<'py> {
    /// Turns under `py`, nothing raised, from now.
    fn new(py: Python<'py>) -> PyResult<Self> {
        let switch_interval = py.import("sys")?.getattr("getswitchinterval")?;
        let held = held(&switch_interval)?;
        Ok(Self {
            py,
            switch_interval,
            raised: None,
            released: Instant::now(),
            held,
        })
    }

    /// Runs `released` with the GIL released, another thread's turn,
    /// then reads the switch interval again, which that thread may have
    /// set; what reading it raises is kept, and ends the run at the
    /// next signal check.
    fn detach<T: Ungil>(&mut self, released: impl Ungil + FnOnce() -> T) -> T {
        let value = self.py.detach(released);
        match held(&self.switch_interval) {
            Ok(held) => self.held = held,
            Err(error) => {
                self.raised.get_or_insert(error);
            }
        }
        value
    }

    /// Python's signal handlers run: `Break` if one raised, or if
    /// something raised already.
    fn signals(&mut self) -> ControlFlow<()> {
        if self.raised.is_some() {
            return ControlFlow::Break(());
        }
        match self.py.check_signals() {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => {
                self.raised = Some(error);
                ControlFlow::Break(())
            }
        }
    }

    /// A waiting thread's turn, if the GIL has been held long enough (a
    /// run that never sleeps releases it here alone), then the signal
    /// handlers.
    fn turn(&mut self) -> ControlFlow<()> {
        if self.released.elapsed() >= self.held {
            self.detach(|| ());
            self.released = Instant::now();
        }
        self.signals()
    }

    /// `outcome`, or what a signal handler raised.
    fn ended<T>(self, outcome: T) -> Result<T, PyErr> {
        self.raised.map_or(Ok(outcome), Err)
    }
}

/// How long a run holds the GIL between releases: twice the switch
/// interval `switch_interval` (`sys.getswitchinterval`) gives.
fn held(switch_interval: &Bound<'_, PyAny>) -> PyResult<Duration> {
    let interval: f64 = switch_interval.call0()?.extract()?;
    Duration::try_from_secs_f64(2.0 * interval)
        .map_err(|error| PyValueError::new_err(error.to_string()))
}
