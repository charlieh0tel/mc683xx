//! The clock synthesizer (SIMRM 4; MC68331UM 4.3) and board time.
//!
//! Board time is counted in system clocks ([`Cycles`]).  The periodic
//! interrupt timer and the software watchdog count EXTAL periods
//! instead (MC68331UM 4.2.10, 4.2.11), and with the synthesizer a
//! whole number of system clocks, 4(Y + 1)2^(2W + X), falls in each
//! (MC68331UM 4.3.2).  The clock keeps the first EXTAL period on the
//! current multiplier and the system clock it begins at, so EXTAL time
//! stays exact across SYNCR writes.

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::frequency::NANOHERTZ_PER_HERTZ;

/// SYNCR (MC68331UM D.3.3): W, the VCO's divide by four.
const SYNCR_W: u16 = 1 << 15;
/// SYNCR X: the divide by two outside the loop.
const SYNCR_X: u16 = 1 << 14;
/// SYNCR Y, the modulus counter's initial value, bits 13:8.
const SYNCR_Y_SHIFT: u16 = 8;
/// SYNCR Y, unshifted.
const SYNCR_Y_MASK: u16 = 0x3f;
/// SYNCR EDIV: ECLK is the system clock / 16, else / 8.
const SYNCR_EDIV: u16 = 1 << 7;
/// SYNCR SLOCK: the synthesizer has locked, or the clock is external.
const SYNCR_SLOCK: u16 = 1 << 3;
/// SYNCR RSTEN: loss of reference resets the MCU.
const SYNCR_RSTEN: u16 = 1 << 2;
/// SYNCR STSIM: the SIM clock runs from the VCO during LPSTOP.
const SYNCR_STSIM: u16 = 1 << 1;
/// SYNCR STEXT: CLKOUT runs during LPSTOP.
const SYNCR_STEXT: u16 = 1;
/// The bits a write sets; SLIMP and SLOCK are status.
const SYNCR_WRITABLE: u16 = SYNCR_W
    | SYNCR_X
    | SYNCR_Y_MASK << SYNCR_Y_SHIFT
    | SYNCR_EDIV
    | SYNCR_RSTEN
    | SYNCR_STSIM
    | SYNCR_STEXT;
/// SYNCR out of reset: a modulus-64 count, W and X clear (MC68331UM
/// 4.3.2, D.3.3).
const SYNCR_RESET: u16 = 0x3f00;
/// The synthesizer's fixed factor: F = Fref x 4(Y + 1) x 2^(2W + X)
/// (MC68331UM 4.3.2).
const SYNTHESIZER_FACTOR: u64 = 4;
/// ECLK's divisors of the system clock, EDIV clear and set (MC68331UM
/// 4.3.3, D.3.3).
const ECLK_DIVISOR: u64 = 8;
/// ECLK's divisor with EDIV set.
const ECLK_DIVISOR_EDIV: u64 = 16;
/// The 68331's reference crystal (MC68331UM 4.3.1, Figure 4-5).
pub const REFERENCE: Frequency = Frequency::from_hz(32_768);

/// The lowest reference the synthesizer takes undivided (SIMRM 4.4.1).
pub const REFERENCE_MIN: Frequency = Frequency::from_hz(25_000);
/// The highest (SIMRM 4.4.1).
pub const REFERENCE_MAX: Frequency = Frequency::from_hz(50_000);

/// A clock source the chip cannot have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClockSourceError {
    /// A frequency that is not whole hertz: the chip's clocks are, as
    /// its snapshot saves them.
    #[error("a chip clock of {0}: give whole hertz")]
    FractionalHertz(Frequency),
    /// A synthesizer reference outside 25 to 50 kHz, the range it
    /// multiplies undivided (SIMRM 4.4.1).  The 3.2 to 6.4 MHz range
    /// it divides by 128 (SIMRM 4.4.2) is not modeled: the 68331's
    /// standard reference is 32.768 kHz (MC68331UM 4.3).
    #[error("a reference crystal of {0}: give 25 to 50 kHz (SIMRM 4.4.1)")]
    ReferenceRange(Frequency),
}

/// `frequency`, if it is whole hertz.
const fn whole_hertz(frequency: Frequency) -> Result<Frequency, ClockSourceError> {
    if frequency.0.is_multiple_of(NANOHERTZ_PER_HERTZ) {
        Ok(frequency)
    } else {
        Err(ClockSourceError::FractionalHertz(frequency))
    }
}

/// Where the system clock comes from, as MODCLK selects at reset
/// (MC68331UM 4.3.1).  Its frequencies are whole hertz: build one with
/// [`ClockSource::synthesizer`] or [`ClockSource::external`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClockSource {
    /// MODCLK high: the synthesizer multiplies the reference on EXTAL,
    /// of `reference` (25 to 50 kHz, SIMRM 4.4.1), as SYNCR sets.
    #[non_exhaustive]
    Synthesizer {
        /// The reference's frequency, whole hertz.
        #[serde(with = "mc683xx_cpu32::frequency::hertz")]
        reference: Frequency,
    },
    /// MODCLK low: EXTAL is the system clock, of `frequency`; SYNCR has no
    /// effect.
    #[non_exhaustive]
    External {
        /// The clock's frequency, whole hertz.
        #[serde(with = "mc683xx_cpu32::frequency::hertz")]
        frequency: Frequency,
    },
}

impl Default for ClockSource {
    fn default() -> Self {
        Self::Synthesizer {
            reference: REFERENCE,
        }
    }
}

impl ClockSource {
    /// The synthesizer from a `reference` crystal.
    ///
    /// # Errors
    ///
    /// [`ClockSourceError::FractionalHertz`] if `reference` is not whole
    /// hertz, [`ClockSourceError::ReferenceRange`] if it is outside
    /// [`REFERENCE_MIN`] to [`REFERENCE_MAX`].
    pub const fn synthesizer(reference: Frequency) -> Result<Self, ClockSourceError> {
        if reference.0 < REFERENCE_MIN.0 || reference.0 > REFERENCE_MAX.0 {
            return Err(ClockSourceError::ReferenceRange(reference));
        }
        match whole_hertz(reference) {
            Ok(reference) => Ok(Self::Synthesizer { reference }),
            Err(error) => Err(error),
        }
    }

    /// An external clock of `frequency`.
    ///
    /// # Errors
    ///
    /// [`ClockSourceError::FractionalHertz`] if `frequency` is not whole
    /// hertz.
    pub const fn external(frequency: Frequency) -> Result<Self, ClockSourceError> {
        match whole_hertz(frequency) {
            Ok(frequency) => Ok(Self::External { frequency }),
            Err(error) => Err(error),
        }
    }

    /// MODCLK's level at reset.
    #[must_use]
    pub const fn modclk(self) -> bool {
        matches!(self, Self::Synthesizer { .. })
    }
}

/// The system clock: its source, SYNCR, and the EXTAL period and
/// system clock it last changed at.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SystemClock {
    /// The source MODCLK chose.
    source: ClockSource,
    /// SYNCR's writable bits.
    syncr: u16,
    /// System clocks in an EXTAL period.
    clocks_per_tick: u64,
    /// The system clock at which EXTAL period `anchor_tick` begins, on
    /// the current multiplier: the end of the period running at the
    /// last change, or the change itself if it came at a period's
    /// start.  Before it, period `anchor_tick - 1` runs at the old one.
    anchor: Cycles,
    /// The first EXTAL period on the current multiplier.
    anchor_tick: u64,
}

impl SystemClock {
    /// The clock from `source`, out of reset at `now`.
    pub(crate) fn new(source: ClockSource, now: Cycles) -> Self {
        let mut clock = Self {
            source,
            syncr: SYNCR_RESET,
            clocks_per_tick: 1,
            anchor: now,
            anchor_tick: 0,
        };
        clock.change(now);
        clock
    }

    /// SYNCR as read: always locked, never in limp mode.
    #[must_use]
    pub const fn syncr(&self) -> u16 {
        self.syncr | SYNCR_SLOCK
    }

    /// SYNCR written at `now`.
    pub(crate) fn write_syncr(&mut self, value: u16, now: Cycles) {
        self.syncr = value & SYNCR_WRITABLE;
        self.change(now);
    }

    /// The source MODCLK chose.
    #[must_use]
    pub const fn source(&self) -> ClockSource {
        self.source
    }

    /// System clocks in an EXTAL period.
    #[must_use]
    pub const fn clocks_per_tick(&self) -> u64 {
        self.clocks_per_tick
    }

    /// The system clock's frequency.
    #[must_use]
    pub const fn frequency(&self) -> Frequency {
        match self.source {
            ClockSource::Synthesizer { reference } => {
                Frequency::from_hz(reference.hz() * self.clocks_per_tick)
            }
            ClockSource::External { frequency } => frequency,
        }
    }

    /// System clocks in an ECLK period.
    #[must_use]
    pub const fn eclk_divisor(&self) -> u64 {
        if self.syncr & SYNCR_EDIV != 0 {
            ECLK_DIVISOR_EDIV
        } else {
            ECLK_DIVISOR
        }
    }

    /// The EXTAL periods begun by `at`, counting from zero: before the
    /// anchor, the period the last change left running.
    #[must_use]
    pub const fn tick(&self, at: Cycles) -> u64 {
        if at.0 < self.anchor.0 {
            return self.anchor_tick.saturating_sub(1);
        }
        self.anchor_tick + (at.0 - self.anchor.0) / self.clocks_per_tick
    }

    /// The system clock EXTAL period `tick` begins at, on the current
    /// multiplier; a period before the anchor's is the anchor's.
    #[must_use]
    pub const fn at(&self, tick: u64) -> Cycles {
        Cycles(self.anchor.0 + tick.saturating_sub(self.anchor_tick) * self.clocks_per_tick)
    }

    /// The multiplier changes at `now`.  EXTAL is the fixed reference
    /// (MC68331UM 4.3.2), so the EXTAL period running finishes at its
    /// own length in time: what is left of it, in old clocks, scales to
    /// new clocks, rounded up (PLAN.md, Clock synthesizer).  The new
    /// multiplier counts from its end.
    fn change(&mut self, now: Cycles) {
        let (end, end_tick) = if now < self.anchor {
            (self.anchor, self.anchor_tick)
        } else {
            let tick = self.tick(now);
            let begun = self.at(tick);
            if begun < now {
                (begun + Cycles(self.clocks_per_tick), tick + 1)
            } else {
                (now, tick)
            }
        };
        let old = self.clocks_per_tick;
        self.clocks_per_tick = self.multiplier();
        let left = (end.0 - now.0).saturating_mul(self.clocks_per_tick);
        self.anchor = Cycles(now.0 + left.div_ceil(old));
        self.anchor_tick = end_tick;
    }

    /// System clocks in an EXTAL period by the source and SYNCR.
    const fn multiplier(&self) -> u64 {
        match self.source {
            ClockSource::Synthesizer { .. } => {
                let y = (self.syncr >> SYNCR_Y_SHIFT & SYNCR_Y_MASK) as u64;
                let w = (self.syncr & SYNCR_W != 0) as u32;
                let x = (self.syncr & SYNCR_X != 0) as u32;
                (SYNTHESIZER_FACTOR * (y + 1)) << (2 * w + x)
            }
            ClockSource::External { .. } => 1,
        }
    }

    /// Sets the multiplier, whatever SYNCR gives, as no run can.
    #[cfg(test)]
    pub(crate) const fn set_multiplier_unchecked(&mut self, clocks_per_tick: u64) {
        self.clocks_per_tick = clocks_per_tick;
    }

    /// Whether a snapshot's clock is one a run could leave: SYNCR's
    /// writable bits only, and the multiplier they give.
    pub(crate) const fn is_valid(&self) -> bool {
        self.syncr & !SYNCR_WRITABLE == 0 && self.clocks_per_tick == self.multiplier()
    }
}

#[cfg(test)]
mod tests {
    use super::ClockSource;
    use super::ClockSourceError;
    use super::REFERENCE_MAX;
    use super::REFERENCE_MIN;
    use super::SystemClock;
    use mc683xx_cpu32::cpu::state::decode;
    use mc683xx_cpu32::cpu::state::encode;
    use mc683xx_cpu32::cycles::Cycles;
    use mc683xx_cpu32::frequency::Frequency;

    #[test]
    fn reset_is_256_times_the_reference() {
        let clock = SystemClock::new(ClockSource::default(), Cycles::ZERO);
        assert_eq!(clock.clocks_per_tick(), 256);
        assert_eq!(clock.frequency().hz(), 8_388_608);
        assert_eq!(clock.syncr(), 0x3f08);
    }

    #[test]
    fn w_x_and_y_set_the_frequency() {
        let mut clock = SystemClock::new(ClockSource::default(), Cycles::ZERO);
        // W, X, Y 15: 4 x 16 x 8 (MC68331UM Table 4-7)
        clock.write_syncr(0xcf00, Cycles::ZERO);
        assert_eq!(clock.frequency().hz(), 16_777_216);
        clock.write_syncr(0x0000, Cycles::ZERO);
        assert_eq!(clock.clocks_per_tick(), 4);
    }

    #[test]
    fn extal_time_survives_a_change() {
        let mut clock = SystemClock::new(ClockSource::default(), Cycles::ZERO);
        // 10.5 periods at 256 clocks; the half left is 256 clocks at 512
        let now = Cycles(10 * 256 + 128);
        clock.write_syncr(0xcf00, now);
        assert_eq!(clock.tick(now), 10);
        assert_eq!(clock.at(11), Cycles(now.0 + 256));
        assert_eq!(clock.tick(Cycles(now.0 + 256 + 511)), 11);
        assert_eq!(clock.tick(Cycles(now.0 + 256 + 512)), 12);
    }

    #[test]
    fn a_lower_multiplier_finishes_the_running_period_in_time() {
        let mut clock = SystemClock::new(ClockSource::default(), Cycles::ZERO);
        // half a 256-clock period in, Y = 0: the half left is 2 clocks
        // at 4 a period
        let now = Cycles(128);
        clock.write_syncr(0x0000, now);
        assert_eq!(clock.tick(now), 0);
        assert_eq!(clock.tick(Cycles(129)), 0);
        assert_eq!(clock.tick(Cycles(130)), 1);
        assert_eq!(clock.at(1), Cycles(130));
        assert_eq!(clock.tick(Cycles(130 + 4)), 2);
        // a second change before that end scales what is left again:
        // 1 clock at 4 is 2 at 8
        clock.write_syncr(0x0100, Cycles(129));
        assert_eq!(clock.tick(Cycles(130)), 0);
        assert_eq!(clock.at(1), Cycles(131));
        assert_eq!(clock.at(2), Cycles(131 + 8));
    }

    #[test]
    fn a_higher_multiplier_finishes_the_running_period_in_time() {
        let mut clock = SystemClock::new(ClockSource::default(), Cycles::ZERO);
        clock.write_syncr(0x0000, Cycles::ZERO);
        // half a 4-clock period in, Y = 63: the half left is 128 clocks
        clock.write_syncr(0x3f00, Cycles(2));
        assert_eq!(clock.tick(Cycles(129)), 0);
        assert_eq!(clock.at(1), Cycles(130));
        assert_eq!(clock.at(2), Cycles(130 + 256));
    }

    #[test]
    fn what_is_left_rounds_up_to_a_whole_clock() {
        let mut clock = SystemClock::new(ClockSource::default(), Cycles::ZERO);
        // 255 of 256 clocks left at 4 a period: 3.98 clocks
        clock.write_syncr(0x0000, Cycles(1));
        assert_eq!(clock.at(1), Cycles(1 + 4));
    }

    #[test]
    fn a_change_at_a_period_start_takes_effect_at_once() {
        let mut clock = SystemClock::new(ClockSource::default(), Cycles::ZERO);
        clock.write_syncr(0x0000, Cycles(256));
        assert_eq!(clock.tick(Cycles(256)), 1);
        assert_eq!(clock.tick(Cycles(260)), 2);
    }

    #[test]
    fn a_chip_clock_is_whole_hertz() {
        let fractional = Frequency(16_000_000_500_000_000);
        assert_eq!(
            ClockSource::external(fractional),
            Err(ClockSourceError::FractionalHertz(fractional))
        );
        assert_eq!(
            ClockSource::synthesizer(Frequency(32_768_000_000_001)),
            Err(ClockSourceError::FractionalHertz(Frequency(
                32_768_000_000_001
            )))
        );
        // past u64's nanohertz, hertz saturate to a fraction
        assert!(ClockSource::external(Frequency::from_hz(u64::MAX)).is_err());
        assert_eq!(
            ClockSourceError::FractionalHertz(fractional).to_string(),
            "a chip clock of 16000000.5 Hz: give whole hertz"
        );
    }

    #[test]
    fn a_reference_outside_25_to_50_khz_is_refused() {
        for reference in [REFERENCE_MIN, REFERENCE_MAX] {
            assert!(ClockSource::synthesizer(reference).is_ok());
        }
        for hz in [24_999, 50_001, 4_194_304] {
            let reference = Frequency::from_hz(hz);
            assert_eq!(
                ClockSource::synthesizer(reference),
                Err(ClockSourceError::ReferenceRange(reference))
            );
        }
        assert_eq!(
            ClockSourceError::ReferenceRange(Frequency::from_hz(4_194_304)).to_string(),
            "a reference crystal of 4194304 Hz: give 25 to 50 kHz (SIMRM 4.4.1)"
        );
    }

    #[test]
    fn a_whole_hertz_source_survives_a_snapshot() {
        for source in [
            ClockSource::external(Frequency::from_hz(16_000_001)).unwrap(),
            ClockSource::synthesizer(Frequency::from_hz(32_768)).unwrap(),
        ] {
            let clock = SystemClock::new(source, Cycles::ZERO);
            let saved: SystemClock = decode(1, &encode(1, &clock)).unwrap();
            assert_eq!(saved.source(), source);
            assert_eq!(saved.frequency(), clock.frequency());
        }
    }

    #[test]
    fn an_external_clock_ignores_syncr() {
        let mut clock = SystemClock::new(
            ClockSource::external(Frequency::from_hz(16_000_000)).unwrap(),
            Cycles::ZERO,
        );
        clock.write_syncr(0xcf00, Cycles::ZERO);
        assert_eq!(clock.clocks_per_tick(), 1);
        assert_eq!(clock.frequency().hz(), 16_000_000);
    }
}
