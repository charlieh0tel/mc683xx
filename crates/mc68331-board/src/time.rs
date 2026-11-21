//! Board time and clocks.
//!
//! Board time is integer picoseconds, so edges between peripherals keep
//! their sub-cycle phase and a run is the same on every host; a clock's
//! phase is carried exactly through any number of frequency changes
//! ([PLAN.md, Boards][plan-boards]).
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::collections::VecDeque;
use std::fmt;
use std::iter::Sum;
use std::ops::Add;
use std::ops::AddAssign;
use std::ops::Div;
use std::ops::Mul;
use std::ops::Rem;
use std::ops::Sub;
use std::ops::SubAssign;

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::frequency::NANOHERTZ_PER_HERTZ;
use serde::Deserialize;
use serde::Serialize;

/// Picoseconds in a second.
pub const PICOSECONDS_PER_SECOND: u64 = 1_000_000_000_000;
/// A clock's phase units in a cycle.  A nanohertz for a picosecond is
/// 10^-21 cycle, so phase advances by whole units, exactly.
pub const PHASE_UNITS: u128 = PICOSECONDS_PER_SECOND as u128 * NANOHERTZ_PER_HERTZ as u128;

/// A time on the board: picoseconds since power-on, 213 days of them.
///
/// Its arithmetic is with [`Span`]s: `+` and `-` (and `+=`, `-=`) move
/// it, saturating at power-on and the last time; `-` of two times is the
/// span between them, zero if the second is later (`since`); `/` and `%`
/// by a period are the whole periods since power-on and the span into
/// the current one (`now / PERIOD`), and panic on a zero period, as
/// integers do.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Time(pub u64);

/// The picoseconds and their unit: `1000 ps`.
impl fmt::Display for Time {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ps", self.0)
    }
}

impl From<u64> for Time {
    fn from(picoseconds: u64) -> Self {
        Self(picoseconds)
    }
}

impl From<Time> for u64 {
    fn from(time: Time) -> Self {
        time.0
    }
}

impl Time {
    /// Power-on.
    pub const ZERO: Self = Self(0);
    /// The last time there is.
    pub const MAX: Self = Self(u64::MAX);

    /// `seconds` seconds, or the last time if that is past it.
    #[must_use]
    pub const fn from_seconds(seconds: u64) -> Self {
        Self(Span::from_seconds(seconds).0)
    }

    /// The span from `earlier` to this, zero if it is later.
    #[must_use]
    pub const fn since(self, earlier: Self) -> Span {
        Span(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Span> for Time {
    type Output = Self;

    /// `span` later, or the last time.
    fn add(self, span: Span) -> Self {
        Self(self.0.saturating_add(span.0))
    }
}

impl Sub<Span> for Time {
    type Output = Self;

    /// `span` earlier, or power-on.
    fn sub(self, span: Span) -> Self {
        Self(self.0.saturating_sub(span.0))
    }
}

impl AddAssign<Span> for Time {
    /// `span` later, or the last time.
    fn add_assign(&mut self, span: Span) {
        *self = *self + span;
    }
}

impl SubAssign<Span> for Time {
    /// `span` earlier, or power-on.
    fn sub_assign(&mut self, span: Span) {
        *self = *self - span;
    }
}

impl Sub for Time {
    type Output = Span;

    /// The span from `earlier` to this, zero if it is later.
    fn sub(self, earlier: Self) -> Span {
        self.since(earlier)
    }
}

impl Div<Span> for Time {
    type Output = u64;

    /// The whole periods of `period` since power-on.
    fn div(self, period: Span) -> u64 {
        self.0 / period.0
    }
}

impl Rem<Span> for Time {
    type Output = Span;

    /// The span into the current period of `period`.
    fn rem(self, period: Span) -> Span {
        Span(self.0 % period.0)
    }
}

/// A span of board time: whole picoseconds, up to the last [`Time`].
///
/// Made from a count of a unit (`Span::from_micros(5)`) or from seconds,
/// and read back as either.  `+`, `-` and `*` by a count saturate, at
/// zero and the longest span, as [`Time`]'s arithmetic does; `/` by a
/// count rounds down, `/` by a span is the whole spans in it and `%` what
/// is left, and these panic on zero, as integers do.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Span(u64);

/// Picoseconds in a nanosecond.
const PICOSECONDS_PER_NANOSECOND: u64 = 1_000;
/// Picoseconds in a microsecond.
const PICOSECONDS_PER_MICROSECOND: u64 = 1_000_000;
/// Picoseconds in a millisecond.
const PICOSECONDS_PER_MILLISECOND: u64 = 1_000_000_000;

impl Span {
    /// No time.
    pub const ZERO: Self = Self(0);
    /// The longest span: power-on to the last time.
    pub const MAX: Self = Self(u64::MAX);

    /// `picoseconds` picoseconds.
    #[must_use]
    pub const fn from_picos(picoseconds: u64) -> Self {
        Self(picoseconds)
    }

    /// `nanoseconds` nanoseconds, or the longest span.
    #[must_use]
    pub const fn from_nanos(nanoseconds: u64) -> Self {
        Self(nanoseconds.saturating_mul(PICOSECONDS_PER_NANOSECOND))
    }

    /// `microseconds` microseconds, or the longest span.
    #[must_use]
    pub const fn from_micros(microseconds: u64) -> Self {
        Self(microseconds.saturating_mul(PICOSECONDS_PER_MICROSECOND))
    }

    /// `milliseconds` milliseconds, or the longest span.
    #[must_use]
    pub const fn from_millis(milliseconds: u64) -> Self {
        Self(milliseconds.saturating_mul(PICOSECONDS_PER_MILLISECOND))
    }

    /// `seconds` seconds, or the longest span.
    #[must_use]
    pub const fn from_seconds(seconds: u64) -> Self {
        Self(seconds.saturating_mul(PICOSECONDS_PER_SECOND))
    }

    /// `seconds` seconds, rounded down to a picosecond: zero for a
    /// negative number or a NaN, the longest span past it.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the cast saturates, and takes a NaN to zero"
    )]
    pub fn from_seconds_f64(seconds: f64) -> Self {
        Self((seconds * PICOSECONDS_PER_SECOND as f64) as u64)
    }

    /// Its picoseconds.
    #[must_use]
    pub const fn picos(self) -> u64 {
        self.0
    }

    /// Its seconds, to an `f64`'s precision.
    #[must_use]
    pub fn seconds_f64(self) -> f64 {
        self.0 as f64 / PICOSECONDS_PER_SECOND as f64
    }

    /// Whether it is no time.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

/// The picoseconds and their unit: `1000 ps`.
impl fmt::Display for Span {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ps", self.0)
    }
}

impl From<u64> for Span {
    fn from(picoseconds: u64) -> Self {
        Self(picoseconds)
    }
}

impl From<Span> for u64 {
    fn from(span: Span) -> Self {
        span.0
    }
}

impl Add for Span {
    type Output = Self;

    /// Both spans, or the longest span.
    fn add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl Sub for Span {
    type Output = Self;

    /// `other` shorter, or zero.
    fn sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl AddAssign for Span {
    /// Both spans, or the longest span.
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl SubAssign for Span {
    /// `other` shorter, or zero.
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

impl Mul<u64> for Span {
    type Output = Self;

    /// `count` of it, or the longest span.
    fn mul(self, count: u64) -> Self {
        Self(self.0.saturating_mul(count))
    }
}

impl Mul<Span> for u64 {
    type Output = Span;

    /// This many of `span`, or the longest span.
    fn mul(self, span: Span) -> Span {
        span * self
    }
}

impl Div<u64> for Span {
    type Output = Self;

    /// A `count`th of it, rounded down to a picosecond.
    fn div(self, count: u64) -> Self {
        Self(self.0 / count)
    }
}

impl Div for Span {
    type Output = u64;

    /// The whole `period`s in it.
    fn div(self, period: Self) -> u64 {
        self.0 / period.0
    }
}

impl Rem for Span {
    type Output = Self;

    /// What is left of it after its whole `period`s.
    fn rem(self, period: Self) -> Self {
        Self(self.0 % period.0)
    }
}

impl Sum for Span {
    /// All of them, or the longest span.
    fn sum<I: Iterator<Item = Self>>(spans: I) -> Self {
        spans.fold(Self::ZERO, Add::add)
    }
}

/// A clock's phase: the cycles since its origin, `PHASE_UNITS` to a
/// cycle.  A rising edge is a whole number of cycles.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Phase(pub u128);

impl Phase {
    /// Whole cycles: the rising edges at or before it.
    #[must_use]
    pub const fn cycles(self) -> u128 {
        self.0 / PHASE_UNITS
    }

    /// The part of a cycle past the last rising edge, in
    /// `PHASE_UNITS`.
    #[must_use]
    pub const fn fraction(self) -> u128 {
        self.0 % PHASE_UNITS
    }
}

/// A clock as a clock net carries it: from `at` on it runs at
/// `frequency`, and its phase at `at` is `phase`.
///
/// A source that changes frequency (an oscillator following its control
/// voltage) drives a new [`Clock`] from the time of the change, made by
/// `retuned`, which carries the phase across exactly: no rounding
/// accumulates however often it changes.  Times are rounded to the
/// picosecond only where an edge's time is asked for, up, to the first
/// picosecond at or after it.  Times before `at` extrapolate back at
/// `frequency`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Clock {
    /// When this frequency took effect.
    pub at: Time,
    /// The frequency from then on.
    pub frequency: Frequency,
    /// The phase then.
    pub phase: Phase,
}

impl Clock {
    /// A clock of `frequency` with a rising edge at `edge`.
    #[must_use]
    pub const fn new(frequency: Frequency, edge: Time) -> Self {
        Self::with_phase(frequency, edge, Phase(0))
    }

    /// A clock of `frequency` whose phase at `at` is `phase`.
    #[must_use]
    pub const fn with_phase(frequency: Frequency, at: Time, phase: Phase) -> Self {
        Self {
            at,
            frequency,
            phase,
        }
    }

    /// Its phase at `time`.
    #[must_use]
    pub const fn phase_at(&self, time: Time) -> Phase {
        let frequency = self.frequency.0 as u128;
        if time.0 >= self.at.0 {
            Phase(self.phase.0 + frequency * (time.0 - self.at.0) as u128)
        } else {
            Phase(
                self.phase
                    .0
                    .saturating_sub(frequency * (self.at.0 - time.0) as u128),
            )
        }
    }

    /// Its rising edges after `from` and at or before `to`.
    #[must_use]
    pub const fn edges(&self, from: Time, to: Time) -> u128 {
        self.phase_at(to)
            .cycles()
            .saturating_sub(self.phase_at(from).cycles())
    }

    /// The `n`th rising edge after `from` (the first is `n` 1), if it
    /// has one: none at zero frequency, or past the last time.
    #[must_use]
    pub fn edge_after(&self, from: Time, n: u64) -> Option<Time> {
        self.edge(self.phase_at(from).cycles() + u128::from(n))
    }

    /// The time of rising edge `number`, where its phase is `number`
    /// whole cycles, if it has one.
    #[must_use]
    pub fn edge(&self, number: u128) -> Option<Time> {
        self.when(Phase(number.checked_mul(PHASE_UNITS)?))
    }

    /// The first picosecond at which its phase is at least `phase`.
    #[must_use]
    pub fn when(&self, phase: Phase) -> Option<Time> {
        let frequency = u128::from(self.frequency.0);
        if frequency == 0 {
            return None;
        }
        if phase.0 <= self.phase.0 {
            let back = (self.phase.0 - phase.0) / frequency;
            return Some(self.at - Span(u64::try_from(back).unwrap_or(u64::MAX)));
        }
        let ahead = u64::try_from((phase.0 - self.phase.0).div_ceil(frequency)).ok()?;
        self.at.0.checked_add(ahead).map(Time)
    }

    /// This clock from `at` on at `frequency`, its phase carried across.
    #[must_use]
    pub const fn retuned(&self, at: Time, frequency: Frequency) -> Self {
        Self {
            at,
            frequency,
            phase: self.phase_at(at),
        }
    }
}

/// A clock net's history, as a sink keeps it: each frequency it has run
/// at from its time, so that a query reaching back across a change
/// integrates each stretch at its own frequency, exactly.
///
/// A sink pushes each [`Clock`] its input receives (a source drives one at
/// every change) and queries when it needs to: edges are counted, not
/// delivered.  Segments older than the history's span, but the one that
/// covers its start, are dropped; a query before the oldest extrapolates
/// back from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockHistory {
    /// The segments, oldest first, each from its `at`.
    segments: VecDeque<Clock>,
    /// The history kept behind the newest segment.
    span: Span,
}

impl ClockHistory {
    /// An empty history keeping `span`.
    #[must_use]
    pub const fn new(span: Span) -> Self {
        Self {
            segments: VecDeque::new(),
            span,
        }
    }

    /// The clock from `clock.at` on.  A clock from before the newest
    /// segment replaces the segments it overlaps.
    pub fn push(&mut self, clock: Clock) {
        while self
            .segments
            .back()
            .is_some_and(|newest| newest.at >= clock.at)
        {
            self.segments.pop_back();
        }
        self.segments.push_back(clock);
        let horizon = clock.at - self.span;
        while self.segments.len() > 1 && self.segments[1].at <= horizon {
            self.segments.pop_front();
        }
    }

    /// The clock now: the newest segment.
    #[must_use]
    pub fn current(&self) -> Option<Clock> {
        self.segments.back().copied()
    }

    /// The segment that holds `time`, and the time the next begins.
    fn segment(&self, time: Time) -> Option<(usize, &Clock)> {
        let index = self
            .segments
            .iter()
            .rposition(|segment| segment.at <= time)
            .unwrap_or(0);
        self.segments.get(index).map(|segment| (index, segment))
    }

    /// The phase at `time`.
    #[must_use]
    pub fn phase_at(&self, time: Time) -> Option<Phase> {
        self.segment(time)
            .map(|(_, segment)| segment.phase_at(time))
    }

    /// Rising edges after `from` and at or before `to`.
    #[must_use]
    pub fn edges(&self, from: Time, to: Time) -> Option<u128> {
        Some(
            self.phase_at(to)?
                .cycles()
                .saturating_sub(self.phase_at(from)?.cycles()),
        )
    }

    /// The time of rising edge `number`, the first picosecond at or
    /// after it, found in the segment it falls in.
    #[must_use]
    pub fn edge(&self, number: u128) -> Option<Time> {
        let target = Phase(number.checked_mul(PHASE_UNITS)?);
        let index = self
            .segments
            .iter()
            .rposition(|segment| segment.phase <= target)
            .unwrap_or(0);
        self.segments.get(index)?.when(target)
    }

    /// The `n`th rising edge after `from` (the first is `n` 1).
    #[must_use]
    pub fn edge_after(&self, from: Time, n: u64) -> Option<Time> {
        self.edge(self.phase_at(from)?.cycles() + u128::from(n))
    }

    /// From `time` to the next rising edge after it, rounded up to a
    /// picosecond, and exactly, in the cycle's `PHASE_UNITS` still to
    /// run: the interpolator's measure.
    #[must_use]
    pub fn to_next_edge(&self, time: Time) -> Option<(Span, u128)> {
        let phase = self.phase_at(time)?;
        let next = self.edge(phase.cycles() + 1)?;
        Some((next.since(time), PHASE_UNITS - phase.fraction()))
    }
}

/// The chip's system clock on board time: from `cycles`, which fell at
/// `time`, each clock lasts 1 / `hz` seconds.
///
/// The board moves it only
/// where the frequency changes (SYNCR, a reset), right after the
/// instruction that changed it (the chip's run ends there,
/// [`ChipBuilder::build`]), so between changes one base maps every
/// time and the mapping never drifts.  The clock moves (an MC68331 runs
/// at 8.388608 MHz from the 32.768 kHz reference until SYNCR is written,
/// MC68331UM 4.3), so a peripheral that converts must not keep a base it was given
/// before: it takes [`Drive::system_clock`] at each call, or keeps what
/// [`Peripheral::system_clock`] tells it at each change.
///
/// The rounding rule, the scheduled input rule's ([PLAN.md,
/// Peripherals][plan-peripherals]): toward the chip a time rounds up, to
/// the first clock at or after it (`cycles`); toward the board a clock
/// rounds down, to the last picosecond at or before it (`time`).  So
/// `cycles(time(c))` is `c`.
///
/// [`ChipBuilder::build`]: crate::chip::ChipBuilder::build
/// [`Drive::system_clock`]: crate::peripheral::Drive::system_clock
/// [`Peripheral::system_clock`]: crate::peripheral::Peripheral::system_clock
/// [plan-peripherals]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#peripherals
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeBase {
    /// The system clock it starts at.
    cycles: Cycles,
    /// Its time.
    time: Time,
    /// The system clock's frequency from then, saved as its hertz.
    #[serde(with = "mc683xx_cpu32::frequency::hertz")]
    frequency: Frequency,
}

impl TimeBase {
    /// A base: system clock `cycles` falls at `time`, and the clock runs
    /// at `frequency`, whole hertz, from there.
    #[must_use]
    pub const fn new(cycles: Cycles, time: Time, frequency: Frequency) -> Self {
        Self {
            cycles,
            time,
            frequency,
        }
    }

    /// The system clock's frequency.
    #[must_use]
    pub const fn frequency(&self) -> Frequency {
        self.frequency
    }

    /// The time `at` falls at, rounded down, before the base too.
    #[must_use]
    pub fn time(&self, at: Cycles) -> Time {
        let picoseconds = |clocks: u64| u128::from(clocks) * u128::from(PICOSECONDS_PER_SECOND);
        let hz = u128::from(self.frequency.hz().max(1));
        let whole = |picoseconds: u128| Span(u64::try_from(picoseconds).unwrap_or(u64::MAX));
        if at >= self.cycles {
            self.time + whole(picoseconds(at.0 - self.cycles.0) / hz)
        } else {
            self.time - whole(picoseconds(self.cycles.0 - at.0).div_ceil(hz))
        }
    }

    /// The first system clock at or after `at`, before the base too.
    #[must_use]
    pub fn cycles(&self, at: Time) -> Cycles {
        let scale = |span: Span| u128::from(span.0) * u128::from(self.frequency.hz());
        let whole = |clocks: u128| u64::try_from(clocks).unwrap_or(u64::MAX);
        if at >= self.time {
            let clocks = scale(at.since(self.time)).div_ceil(u128::from(PICOSECONDS_PER_SECOND));
            Cycles(self.cycles.0.saturating_add(whole(clocks)))
        } else {
            let clocks = scale(self.time.since(at)) / u128::from(PICOSECONDS_PER_SECOND);
            Cycles(self.cycles.0.saturating_sub(whole(clocks)))
        }
    }

    /// The base from `at` on, at `frequency`: itself if the frequency
    /// has not changed.
    pub(crate) fn moved(&self, at: Cycles, frequency: Frequency) -> Self {
        if frequency == self.frequency {
            return *self;
        }
        Self {
            cycles: at,
            time: self.time(at),
            frequency,
        }
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::cycles::Cycles;

    use super::Clock;
    use super::ClockHistory;
    use super::PHASE_UNITS;
    use super::Span;
    use super::Time;
    use super::TimeBase;
    use mc683xx_cpu32::frequency::Frequency;

    /// Ten megahertz, an OCXO's.
    const TEN_MHZ: Frequency = Frequency::from_hz(10_000_000);

    /// `picoseconds` picoseconds.
    const fn ps(picoseconds: u64) -> Span {
        Span::from_picos(picoseconds)
    }

    #[test]
    fn time_moves_by_spans_and_divides_into_periods() {
        let mut time = Time(2_500);
        time += ps(500);
        assert_eq!(time, Time(3_000));
        time -= ps(4_000);
        assert_eq!(time, Time::ZERO, "saturating at power-on");
        assert_eq!(Time::MAX + ps(1), Time::MAX);
        assert_eq!(Time(3_000) - Time(1_000), ps(2_000));
        assert_eq!(Time(1_000) - Time(3_000), Span::ZERO);
        assert_eq!(Time(2_500) / ps(1_000), 2);
        assert_eq!(Time(2_500) % ps(1_000), ps(500));
    }

    #[test]
    fn spans_come_in_units_and_scale() {
        assert_eq!(Span::from_nanos(3), ps(3_000));
        assert_eq!(Span::from_micros(3), Span::from_nanos(3_000));
        assert_eq!(Span::from_millis(3), Span::from_micros(3_000));
        assert_eq!(Span::from_seconds(3), Span::from_millis(3_000));
        assert_eq!(Span::from_seconds_f64(0.25), Span::from_millis(250));
        assert_eq!(Span::from_seconds_f64(-1.0), Span::ZERO);
        assert_eq!(Span::from_seconds_f64(f64::NAN), Span::ZERO);
        assert_eq!(Span::from_seconds_f64(1e300), Span::MAX);
        assert_eq!(Span::from_seconds(u64::MAX), Span::MAX);
        assert!((Span::from_millis(1_500).seconds_f64() - 1.5).abs() < f64::EPSILON);
        assert_eq!(ps(2) + ps(3), ps(5));
        assert_eq!(ps(2) - ps(3), Span::ZERO, "saturating at zero");
        assert_eq!(Span::MAX + ps(1), Span::MAX);
        assert_eq!(ps(7) * 3, ps(21));
        assert_eq!(Span::MAX * 2, Span::MAX);
        assert_eq!(3 * ps(7), ps(21));
        assert_eq!(ps(7) / 2, ps(3));
        assert_eq!(ps(7) / ps(2), 3);
        assert_eq!(ps(7) % ps(2), ps(1));
        assert_eq!([ps(1), ps(2), ps(3)].into_iter().sum::<Span>(), ps(6));
        assert_eq!(Span::from_micros(5).to_string(), "5000000 ps");
        assert_eq!(Span::from(9_u64).picos(), 9);
    }

    #[test]
    fn edges_are_counted_exactly_over_long_spans() {
        let clock = Clock::new(TEN_MHZ, Time(123));
        let day = Time::from_seconds(86_400);
        assert_eq!(clock.edges(Time(123), day + ps(123)), 864_000_000_000);
        assert_eq!(clock.edge_after(Time(123), 1), Some(Time(100_123)));
        assert_eq!(clock.edge_after(Time(124), 10), Some(Time(1_000_123)));
    }

    #[test]
    fn a_retune_carries_the_phase_without_rounding() {
        // 3 Hz has no whole-picosecond period: a third of a second.
        let third = Clock::new(Frequency::from_hz(3), Time::ZERO);
        let mut clock = third;
        for step in 1..=3_000 {
            clock = clock.retuned(Time(step * 1_000_000_000), Frequency::from_hz(3));
        }
        assert_eq!(clock.phase_at(Time::from_seconds(3)).0, 9 * PHASE_UNITS);
        assert_eq!(
            clock.edge_after(Time::ZERO, 1),
            third.edge_after(Time::ZERO, 1)
        );
        assert_eq!(clock.edge_after(Time::ZERO, 1), Some(Time(333_333_333_334)));
    }

    #[test]
    fn the_phase_at_a_time_holds_its_fraction() {
        let clock = Clock::new(TEN_MHZ, Time::ZERO);
        let phase = clock.phase_at(Time(25_000));
        assert_eq!(phase.cycles(), 0);
        assert_eq!(phase.fraction(), PHASE_UNITS / 4);
        let faster = clock.retuned(Time(25_000), Frequency::from_hz(20_000_000));
        assert_eq!(faster.edge_after(Time(25_000), 1), Some(Time(62_500)));
    }

    #[test]
    fn a_history_integrates_across_its_changes() {
        // 10 MHz, then 20 MHz from 1 us: 10 edges, then 20 a us.
        let mut history = ClockHistory::new(Span::from_seconds(64));
        let clock = Clock::new(TEN_MHZ, Time::ZERO);
        history.push(clock);
        let faster = clock.retuned(Time(1_000_000), Frequency::from_hz(20_000_000));
        history.push(faster);
        assert_eq!(history.edges(Time::ZERO, Time(2_000_000)), Some(30));
        assert_eq!(history.edge_after(Time(500_000), 5), Some(Time(1_000_000)));
        assert_eq!(history.edge_after(Time(500_000), 6), Some(Time(1_050_000)));
        assert_eq!(
            history.edge(3),
            Some(Time(300_000)),
            "found in the old segment"
        );
        assert_eq!(
            history.to_next_edge(Time(1_010_000)),
            Some((ps(40_000), PHASE_UNITS * 4 / 5))
        );
        // A push from the past replaces what it overlaps.
        history.push(clock.retuned(Time(1_000_000), TEN_MHZ));
        assert_eq!(history.edges(Time::ZERO, Time(2_000_000)), Some(20));
    }

    #[test]
    fn a_history_keeps_its_span() {
        let mut history = ClockHistory::new(ps(10));
        let mut clock = Clock::new(TEN_MHZ, Time::ZERO);
        for at in 1..=100 {
            clock = clock.retuned(Time(at), TEN_MHZ);
            history.push(clock);
        }
        assert!(history.segments.len() <= 11);
        assert_eq!(history.current(), Some(clock));
    }

    /// Days of a 10 MHz oscillator stepped every second, as an EFC
    /// steers one, against the exact sum of each second's cycles.
    #[test]
    fn days_of_retunes_accumulate_no_rounding() {
        const DAYS: u64 = 3;
        let seconds = DAYS * 86_400;
        let mut history = ClockHistory::new(Span::from_seconds(64));
        let mut clock = Clock::new(TEN_MHZ, Time(7));
        history.push(clock);
        // The exact phase, in nanohertz-seconds: cycles x 10^9.
        let mut exact: u128 = 0;
        let mut frequency = TEN_MHZ;
        for second in 1..=seconds {
            exact += u128::from(frequency.0);
            // A step of 6.25e-13, a DAC count's, up and down in turn,
            // drifting: 6.25 uHz is 6250 nHz.
            let step = 6_250 * (second % 7);
            frequency = Frequency(TEN_MHZ.0 + step - 6_250 * 3);
            clock = clock.retuned(Time::from_seconds(second) + ps(7), frequency);
            history.push(clock);
        }
        let end = Time::from_seconds(seconds) + ps(7);
        let phase = history.phase_at(end).unwrap();
        // Phase units are 10^-21 cycle; a nanohertz-second is 10^-9.
        assert_eq!(phase.0, exact * 1_000_000_000_000);
        // The next edge after the end, exactly: the remaining fraction
        // at the last frequency, rounded up to a picosecond.
        let remaining = PHASE_UNITS - phase.fraction();
        let picoseconds = remaining.div_ceil(u128::from(frequency.0));
        assert_eq!(
            history.to_next_edge(end),
            Some((ps(u64::try_from(picoseconds).unwrap()), remaining))
        );
        // Ten million edges on, within the last segment.
        let edge = history.edge_after(end, 10_000_000).unwrap();
        let target = (phase.cycles() + 10_000_000) * PHASE_UNITS;
        assert_eq!(
            u128::from(edge.0 - end.0),
            (target - phase.0).div_ceil(u128::from(frequency.0))
        );
    }

    #[test]
    fn a_stopped_clock_has_no_edges() {
        let clock = Clock::new(Frequency(0), Time(5));
        assert_eq!(clock.edge_after(Time(5), 1), None);
        assert_eq!(clock.edges(Time(0), Time::MAX), 0);
    }

    #[test]
    fn chip_times_round_up_to_a_clock() {
        let base = TimeBase::new(Cycles(10), Time(1_000), Frequency::from_hz(16_777_216));
        assert_eq!(base.time(Cycles(11)), Time(1_000 + 59_604));
        assert_eq!(base.cycles(Time(1_000)), Cycles(10));
        assert_eq!(base.cycles(Time(1_001)), Cycles(11));
        assert_eq!(base.cycles(Time(1_000 + 59_605)), Cycles(12));
        assert_eq!(base.cycles(base.time(Cycles(500))), Cycles(500));
        let moved = base.moved(Cycles(20), Frequency::from_hz(8_388_608));
        assert_eq!(moved.time, base.time(Cycles(20)));
        assert_eq!(moved.cycles(moved.time + ps(1)), Cycles(21));
        assert_eq!(
            base.moved(Cycles(20), Frequency::from_hz(16_777_216)),
            base,
            "no drift"
        );
    }

    #[test]
    fn clocks_before_the_base_round_down_and_back() {
        let base = TimeBase::new(
            Cycles(1_000),
            Time(100_000_000),
            Frequency::from_hz(16_777_216),
        );
        assert_eq!(base.time(Cycles(999)), Time(100_000_000 - 59_605));
        for clocks in (0..1_000).chain(1_000..1_100) {
            let cycles = Cycles(clocks);
            assert_eq!(base.cycles(base.time(cycles)), cycles, "{clocks}");
        }
    }
}
