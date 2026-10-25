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
/// never waits.
#[derive(Debug, Default)]
pub struct Pacer {
    /// Where the pacing started, if paced.
    anchor: Option<Anchor>,
}

/// A paced run's start: wall time and board time together, at a factor.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    /// Wall time.
    wall: Instant,
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
    /// `now`.
    pub fn delay(&mut self, now: Time, pacing: Pacing) -> Duration {
        self.delay_at(now, pacing, Instant::now())
    }

    /// `delay`, the wall clock reading `wall`.
    fn delay_at(&mut self, now: Time, pacing: Pacing, wall: Instant) -> Duration {
        let Some(factor) = pacing.factor() else {
            self.anchor = None;
            return Duration::ZERO;
        };
        let anchor = match self.anchor {
            Some(anchor) if anchor.factor == factor && anchor.board <= now => anchor,
            _ => {
                self.anchor = Some(Anchor {
                    wall,
                    board: now,
                    factor,
                });
                return Duration::ZERO;
            }
        };
        let board = (now.0 - anchor.board.0) as f64 / PICOSECONDS_PER_SECOND as f64;
        Duration::try_from_secs_f64(board / factor)
            .ok()
            .and_then(|elapsed| anchor.wall.checked_add(elapsed))
            .map_or(Duration::MAX, |due| due.saturating_duration_since(wall))
    }

    /// Waits until the board at `now` is no longer ahead under
    /// `control`'s pacing, or until a stop is asked through it
    /// ([`BoardControl::sleep`], which wakes for either).
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
        let start = Instant::now();
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
    fn a_tiny_factor_waits_without_overflow() {
        let start = Instant::now();
        let mut pacer = Pacer::new();
        let crawl = Pacing::Scaled(f64::MIN_POSITIVE);
        pacer.delay_at(Time(0), crawl, start);
        assert_eq!(pacer.delay_at(Time(1), crawl, start), Duration::MAX);
    }
}
