//! Pacing a board's run against the host's wall clock, for a front end:
//! host-side only.
//!
//! The wall clock here decides only how long the host sleeps between
//! quanta of board time; it never reaches the board, the chip or a
//! peripheral, so a run is the same whatever its pacing ([PLAN.md,
//! Boards, Front ends][plan-boards]).  This module is the one place the
//! board crate reads host time.
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::fmt;
use std::num::NonZeroU64;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;

use crate::control::BoardControl;
use crate::time::PICOSECONDS_PER_SECOND;
use crate::time::Time;

/// Board time a paced run goes between looks at the wall clock and the
/// [`BoardControl`]: 5 ms, about 84,000 clocks of a 16.78 MHz chip.
pub const QUANTUM: NonZeroU64 = NonZeroU64::new(5_000_000_000).expect("not zero");

/// The longest the host sleeps before it looks at the control again,
/// for a stop or a change of pacing (and, in Python, for Ctrl-C).
const SLICE: Duration = Duration::from_millis(20);

/// How fast a paced run's board time may go against wall time.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum Pacing {
    /// As fast as the host runs it.
    #[default]
    Unpaced,
    /// A board second a wall second.
    RealTime,
    /// This many board seconds a wall second: 2.0 twice real time,
    /// 0.5 half.  Finite and above 0.
    Scaled(f64),
}

impl Pacing {
    /// Board seconds a wall second, [`None`] unpaced.
    #[must_use]
    pub const fn factor(self) -> Option<f64> {
        match self {
            Self::Unpaced => None,
            Self::RealTime => Some(1.0),
            Self::Scaled(factor) => Some(factor),
        }
    }

    /// It, if its factor is finite and above 0.
    ///
    /// # Errors
    ///
    /// [`PacingError::Factor`] for any other factor.
    pub fn checked(self) -> Result<Self, PacingError> {
        match self {
            Self::Scaled(factor) if !(factor.is_finite() && factor > 0.0) => {
                Err(PacingError::Factor(factor))
            }
            other => Ok(other),
        }
    }
}

/// [`Pacing::Unpaced`]'s name.
const UNPACED: &str = "unpaced";
/// [`Pacing::RealTime`]'s name.
const REAL_TIME: &str = "realtime";

impl fmt::Display for Pacing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unpaced => formatter.write_str(UNPACED),
            Self::RealTime => formatter.write_str(REAL_TIME),
            Self::Scaled(factor) => write!(formatter, "{factor}"),
        }
    }
}

impl FromStr for Pacing {
    type Err = ParsePacingError;

    /// `unpaced`, `realtime`, or a factor, board seconds a wall second.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            UNPACED => Ok(Self::Unpaced),
            REAL_TIME => Ok(Self::RealTime),
            _ => {
                let factor: f64 = text
                    .parse()
                    .map_err(|_| ParsePacingError::Unknown(text.to_owned()))?;
                Self::Scaled(factor)
                    .checked()
                    .map_err(ParsePacingError::Factor)
            }
        }
    }
}

/// A pacing that cannot be kept.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum PacingError {
    /// A factor not finite, or not above 0.
    #[error("pacing factor {0}: give a finite number of board seconds a wall second, above 0")]
    Factor(f64),
}

/// Text that names no pacing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ParsePacingError {
    /// Neither a name nor a number.
    #[error("no pacing {0:?}: give unpaced, realtime or a factor")]
    Unknown(String),
    /// A number that is not a factor.
    #[error(transparent)]
    Factor(PacingError),
}

/// Keeps a run's board time from going ahead of wall time times its
/// pacing's factor: told the board time before each quantum, it says how
/// long to wait.
///
/// A run that falls behind (a slow host) is not held
/// back, and goes as fast as it can until it catches up.  Unpaced, it
/// never waits, and never reads the clock.  A host with a clock of its
/// own that cannot sleep (a browser) runs the board in frames
/// ([`Board::run_frame`](crate::board::Board::run_frame)) instead.
#[derive(Debug, Default)]
pub struct Pacer {
    /// The host's clock's reading that wall times count from: its first
    /// read.
    origin: Option<Instant>,
    /// Where the pacing started, if paced.
    anchor: Option<Anchor>,
}

/// A paced run's start: wall time and board time together, at a factor.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    /// Wall time, from the pacer's origin.
    wall: Duration,
    /// Board time.
    board: Time,
    /// Board seconds a wall second.
    factor: f64,
}

impl Pacer {
    /// A pacer that starts at its first look.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How long to wait before running past `now` under `pacing`, zero
    /// if not ahead.  The first look, a change of factor and a board
    /// time behind the last (a restore) start the pacing again from
    /// `now`.  It reads the host's clock ([`Instant`]) only when paced.
    pub fn delay(&mut self, now: Time, pacing: Pacing) -> Duration {
        let wall = match pacing.factor() {
            Some(_) => self.origin.get_or_insert_with(Instant::now).elapsed(),
            None => Duration::ZERO,
        };
        self.delay_at(now, pacing, wall)
    }

    /// `delay`, the wall clock reading `wall`.
    fn delay_at(&mut self, now: Time, pacing: Pacing, wall: Duration) -> Duration {
        let Some(factor) = pacing.factor() else {
            self.anchor = None;
            return Duration::ZERO;
        };
        let Some(anchor) = self.anchored(now, factor, wall) else {
            return Duration::ZERO;
        };
        let board = (now.0 - anchor.board.0) as f64 / PICOSECONDS_PER_SECOND as f64;
        Duration::try_from_secs_f64(board / factor)
            .ok()
            .and_then(|elapsed| anchor.wall.checked_add(elapsed))
            .map_or(Duration::MAX, |due| due.saturating_sub(wall))
    }

    /// The board time a run at `now` may go to by the host's wall clock
    /// reading `wall` under `pacing`, [`None`] unpaced (no bound): for a
    /// host with a clock of its own that cannot sleep (a browser's
    /// `performance.now()`), which runs the board to it a slice at a
    /// time rather than waiting.  `wall` is that clock's reading from
    /// any origin the host keeps for the pacer's life, never going back;
    /// a pacer asked this is never asked [`delay`](Self::delay), which
    /// reads [`Instant`].  The pacing starts again from `now` and `wall`
    /// as [`delay`](Self::delay)'s does, the time then `now`.  A host
    /// far behind (a page hidden a while) gets a time far ahead, and
    /// bounds each slice itself; to drop the time it missed rather than
    /// catch it up, it [`restart`](Self::restart)s from where the bound
    /// left it.  Crate-private: its wall clock is the host's, from the
    /// host's origin, where [`delay`](Self::delay) reads [`Instant`] from
    /// the pacer's first look, so a host must not mix the two; the board
    /// keeps a pacer of its own for its frames.
    pub(crate) fn due(&mut self, now: Time, pacing: Pacing, wall: Duration) -> Option<Time> {
        let Some(factor) = pacing.factor() else {
            self.anchor = None;
            return None;
        };
        let Some(anchor) = self.anchored(now, factor, wall) else {
            return Some(now);
        };
        let elapsed = wall.saturating_sub(anchor.wall).as_secs_f64();
        #[expect(
            clippy::cast_sign_loss,
            reason = "not negative, and the cast saturates"
        )]
        let board = (elapsed * factor * PICOSECONDS_PER_SECOND as f64) as u64;
        Some(Time(anchor.board.0.saturating_add(board)).max(now))
    }

    /// Starts the pacing again under `pacing` from `now` and the host's
    /// clock reading `wall`, as [`due`](Self::due) takes it: for a host
    /// that ran less than was due (a frame's bound), so later looks pace
    /// from where it got to and the time it missed is dropped.
    pub(crate) fn restart(&mut self, now: Time, pacing: Pacing, wall: Duration) {
        match pacing.factor() {
            Some(factor) => self.start(now, factor, wall),
            None => self.anchor = None,
        }
    }

    /// The pacing's start at `factor`, or [`None`] if it starts again
    /// here: at the first look, a change of factor, or a board time
    /// behind the start (a restore).
    fn anchored(&mut self, now: Time, factor: f64, wall: Duration) -> Option<Anchor> {
        match self.anchor {
            Some(anchor) if anchor.factor == factor && anchor.board <= now => Some(anchor),
            _ => {
                self.start(now, factor, wall);
                None
            }
        }
    }

    /// Starts the pacing at `factor` from `now` and `wall`.
    const fn start(&mut self, now: Time, factor: f64, wall: Duration) {
        self.anchor = Some(Anchor {
            wall,
            board: now,
            factor,
        });
    }

    /// Waits until the board at `now` is no longer ahead under
    /// `control`'s pacing, or until a stop is asked through it
    /// ([`BoardControl::sleep`], which wakes for either).  Unpaced, it
    /// returns at once, reading neither the clock nor sleeping.
    pub fn wait(&mut self, now: Time, control: &BoardControl) {
        let slept = self.wait_with(now, control, |duration| {
            control.sleep(duration);
            ControlFlow::Continue(())
        });
        debug_assert!(slept.is_continue());
    }

    /// [`wait`](Self::wait), in sleeps of at most 20 ms, each the
    /// host's `sleep(duration)`, so a stop or a change of pacing is
    /// seen within one: the Python bindings release the GIL in it and
    /// look for Ctrl-C after.  `sleep` returning `Break` ends the wait,
    /// and gives `Break`.
    pub fn wait_with(
        &mut self,
        now: Time,
        control: &BoardControl,
        mut sleep: impl FnMut(Duration) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        loop {
            if control.stopping() {
                return ControlFlow::Continue(());
            }
            let delay = self.delay(now, control.pacing());
            if delay.is_zero() {
                return ControlFlow::Continue(());
            }
            sleep(delay.min(SLICE))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A millisecond in picoseconds.
    const MILLISECOND: u64 = PICOSECONDS_PER_SECOND / 1000;

    #[test]
    fn a_pacing_reads_as_it_prints() {
        for pacing in [Pacing::Unpaced, Pacing::RealTime, Pacing::Scaled(2.5)] {
            assert_eq!(pacing.to_string().parse(), Ok(pacing));
        }
        assert_eq!("0.5".parse(), Ok(Pacing::Scaled(0.5)));
        assert_eq!(
            "fast".parse::<Pacing>(),
            Err(ParsePacingError::Unknown("fast".to_owned()))
        );
        for factor in ["0", "-1", "inf", "NaN"] {
            assert!(
                matches!(
                    factor.parse::<Pacing>(),
                    Err(ParsePacingError::Factor(PacingError::Factor(_)))
                ),
                "{factor}"
            );
        }
    }

    #[test]
    fn a_pacer_holds_board_time_to_wall_time_times_the_factor() {
        let start = Duration::ZERO;
        let mut pacer = Pacer::new();
        let twice = Pacing::Scaled(2.0);
        assert_eq!(pacer.delay_at(Time(0), twice, start), Duration::ZERO);
        // 10 ms of board time is 5 ms of wall time at twice real time
        let at = |millis| start + Duration::from_millis(millis);
        let ten = Time(10 * MILLISECOND);
        assert_eq!(pacer.delay_at(ten, twice, at(1)), Duration::from_millis(4));
        assert_eq!(pacer.delay_at(ten, twice, at(5)), Duration::ZERO);
        assert_eq!(pacer.delay_at(ten, twice, at(9)), Duration::ZERO, "behind");
        // a new factor starts again from where the board is
        assert_eq!(pacer.delay_at(ten, Pacing::RealTime, at(9)), Duration::ZERO);
        let twenty = Time(20 * MILLISECOND);
        assert_eq!(
            pacer.delay_at(twenty, Pacing::RealTime, at(10)),
            Duration::from_millis(9)
        );
        // unpaced never waits; paced again starts again
        assert_eq!(
            pacer.delay_at(twenty, Pacing::Unpaced, at(10)),
            Duration::ZERO
        );
        assert_eq!(
            pacer.delay_at(twenty, Pacing::RealTime, at(10)),
            Duration::ZERO
        );
        // board time behind the start (a restore) starts again
        assert_eq!(
            pacer.delay_at(ten, Pacing::RealTime, at(10)),
            Duration::ZERO
        );
        assert_eq!(
            pacer.delay_at(twenty, Pacing::RealTime, at(10)),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn an_unpaced_pacer_never_reads_the_clock() {
        let mut pacer = Pacer::new();
        assert_eq!(pacer.delay(Time(0), Pacing::Unpaced), Duration::ZERO);
        assert_eq!(
            pacer.delay(Time(MILLISECOND), Pacing::Unpaced),
            Duration::ZERO
        );
        assert!(pacer.origin.is_none());
    }

    #[test]
    fn a_host_clock_says_how_far_the_board_may_go() {
        let mut pacer = Pacer::new();
        let twice = Pacing::Scaled(2.0);
        let wall = |millis| Duration::from_millis(millis);
        let board = |millis| Time(millis * MILLISECOND);
        assert_eq!(pacer.due(board(0), Pacing::Unpaced, wall(0)), None);
        // the first look starts the pacing: nothing is due yet
        assert_eq!(pacer.due(board(0), twice, wall(100)), Some(board(0)));
        // 5 ms of wall time is 10 ms of board time at twice real time
        assert_eq!(pacer.due(board(0), twice, wall(105)), Some(board(10)));
        assert_eq!(pacer.due(board(10), twice, wall(105)), Some(board(10)));
        assert_eq!(
            pacer.due(board(12), twice, wall(105)),
            Some(board(12)),
            "ahead"
        );
        // a new factor starts again from where the board is
        assert_eq!(
            pacer.due(board(12), Pacing::RealTime, wall(110)),
            Some(board(12))
        );
        assert_eq!(
            pacer.due(board(12), Pacing::RealTime, wall(130)),
            Some(board(32))
        );
        // board time behind the start (a restore) starts again
        assert_eq!(
            pacer.due(board(1), Pacing::RealTime, wall(140)),
            Some(board(1))
        );
        assert_eq!(
            pacer.due(board(1), Pacing::RealTime, wall(150)),
            Some(board(11))
        );
        // the clock going back is no time at all
        assert_eq!(
            pacer.due(board(11), Pacing::RealTime, wall(0)),
            Some(board(11))
        );
        assert!(pacer.origin.is_none());
    }

    #[test]
    fn a_restart_drops_the_time_missed() {
        let mut pacer = Pacer::new();
        let wall = |millis| Duration::from_millis(millis);
        let board = |millis| Time(millis * MILLISECOND);
        pacer.due(board(0), Pacing::RealTime, wall(0));
        assert_eq!(
            pacer.due(board(0), Pacing::RealTime, wall(1000)),
            Some(board(1000))
        );
        // the host ran 100 ms of it, and drops the rest
        pacer.restart(board(100), Pacing::RealTime, wall(1000));
        assert_eq!(
            pacer.due(board(100), Pacing::RealTime, wall(1010)),
            Some(board(110))
        );
    }

    #[test]
    fn a_tiny_factor_waits_without_overflow() {
        let start = Duration::ZERO;
        let mut pacer = Pacer::new();
        let crawl = Pacing::Scaled(f64::MIN_POSITIVE);
        pacer.delay_at(Time(0), crawl, start);
        assert_eq!(pacer.delay_at(Time(1), crawl, start), Duration::MAX);
    }
}
