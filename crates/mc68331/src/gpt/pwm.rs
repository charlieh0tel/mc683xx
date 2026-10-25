//! The GPT's pulse-width modulation unit (GPTRM 6; MC68331UM 7.11):
//! PWMCNT on a prescaler tap, and two outputs whose duty cycles take
//! effect at the end of a period.

use std::fmt;
use std::str::FromStr;

use super::timer::Counter;

/// PWMC PPROUT: PWMA carries PWMCNT's clock (GPTRM 6.3.1).
const PWMC_PPROUT: u8 = 1 << 7;
/// PWMC PPR, bits 6:4: the tap, system clock / 2^(PPR + 1), or PCLK.
const PWMC_PPR_SHIFT: u8 = 4;
/// PWMC PPR, unshifted.
const PWMC_PPR_MASK: u8 = 0b111;
/// PPR's PCLK value (GPTRM Table 6-2).
const PPR_PCLK: u8 = 0b111;
/// PPR 0's divisor, 2.
const PPR_LEAST_DIVISOR: u64 = 2;
/// PWMC SFA; SFB is one bit lower: slow mode, a 32768-count period.
const PWMC_SFA: u8 = 1 << 3;
/// PWMC F1A; F1B is one bit lower: a one on the pin.
const PWMC_F1A: u8 = 1 << 1;
/// CFORC FPWMA, bit 9 of the word; FPWMB is one bit lower: the pin is
/// a discrete output of F1x (GPTRM 3.3.4).
const FPWMA: u8 = 1 << 1;
/// The duty's shift in a fast period's count: bits 7:0 (GPTRM 6.1).
const FAST_SHIFT: u32 = 0;
/// A slow period's: bits 14:7.
const SLOW_SHIFT: u32 = 7;
/// Bits in a duty value and a period's phase.
const DUTY_BITS: u32 = 8;
/// The counts `next_edge` tries: the rest of the period in progress
/// and one whole period on the registers, each changing the level at
/// most at its start and where its phase reaches the duty.
const EDGE_CANDIDATES: usize = 4;

/// One of the two outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Channel {
    /// PWMA.
    A,
    /// PWMB.
    B,
}

impl Channel {
    /// Its name, `A` or `B`, its pin `PWMA` or `PWMB` less `PWM`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The PWM channel [`Channel::name`] names, in any case.
impl FromStr for Channel {
    type Err = ParseChannelError;

    fn from_str(name: &str) -> Result<Self, ParseChannelError> {
        Self::BOTH
            .into_iter()
            .find(|candidate| candidate.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| ParseChannelError(name.to_owned()))
    }
}

/// Text that names no PWM channel.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no PWM channel {0:?}")]
pub struct ParseChannelError(pub String);

impl Channel {
    /// Both, A first.
    pub(super) const BOTH: [Self; 2] = [Self::A, Self::B];

    /// Its index, 0 for A.
    pub(super) const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    /// Its bit among a pair of A and B bits, A the higher.
    const fn bit(self, a: u8) -> u8 {
        match self {
            Self::A => a,
            Self::B => a >> 1,
        }
    }
}

/// What a PWM pin or OC1's carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Output {
    /// A steady level.
    Level(bool),
    /// A waveform high for `high` of every `period` PWMCNT counts, from
    /// the period's start.
    Wave {
        /// Counts in a period: 256 fast, 32768 slow.
        period: u32,
        /// Counts high.
        high: u32,
        /// System clocks a count, or `None` when PCLK clocks PWMCNT.
        clocks: Option<u64>,
    },
    /// A counter's clock, high one system clock a count: PWMCNT's on
    /// PWMA (PPROUT), TCNT's on OC1 (CPROUT).
    Clock {
        /// System clocks a count, or `None` for PCLK.
        clocks: Option<u64>,
    },
}

/// The PWM unit's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Pwm {
    /// PWMC as written.
    pub(super) control: u8,
    /// CFORC's FPWMA and FPWMB.
    pub(super) force: u8,
    /// PWMA and PWMB.
    pub(super) duty: [u8; 2],
    /// PWMABUF and PWMBBUF, the duties of the periods in progress.
    pub(super) buffer: [u8; 2],
    /// F1x as of the period in progress: high all period.
    high: [bool; 2],
    /// The period each output last synchronized in.
    period: [u64; 2],
    /// PWMCNT.
    counter: Counter,
}

impl Pwm {
    /// The PWM unit out of reset at the prescaler's `count`: PWMCNT on
    /// the system clock / 2 (GPTRM 6.1).
    pub(super) const fn new(count: u64) -> Self {
        Self {
            control: 0,
            force: 0,
            duty: [0; 2],
            buffer: [0; 2],
            high: [false; 2],
            period: [0; 2],
            counter: Counter::new(Some(PPR_LEAST_DIVISOR), count),
        }
    }

    /// Whether a snapshot's PWMCNT is on the tap PPR selects.
    pub(super) const fn is_valid(&self) -> bool {
        match (self.counter.divisor(), Self::divisor(self.control)) {
            (Some(clocked), Some(selected)) => clocked == selected,
            (clocked, selected) => clocked.is_none() && selected.is_none(),
        }
    }

    /// PPR's divisor, or `None` for PCLK.
    const fn divisor(control: u8) -> Option<u64> {
        match control >> PWMC_PPR_SHIFT & PWMC_PPR_MASK {
            PPR_PCLK => None,
            ppr => Some(PPR_LEAST_DIVISOR << ppr),
        }
    }

    /// Whether `channel` is in slow mode.
    const fn slow(&self, channel: Channel) -> bool {
        self.control & channel.bit(PWMC_SFA) != 0
    }

    /// The bit of PWMCNT a period's phase starts at.
    const fn shift(&self, channel: Channel) -> u32 {
        if self.slow(channel) {
            SLOW_SHIFT
        } else {
            FAST_SHIFT
        }
    }

    /// PWMCNT at the prescaler's `count`.
    pub(super) const fn count(&self, count: u64) -> u64 {
        self.counter.ticks(count)
    }

    /// `channel`'s period number at PWMCNT `ticks`.
    const fn period_of(&self, channel: Channel, ticks: u64) -> u64 {
        ticks >> (self.shift(channel) + DUTY_BITS)
    }

    /// `channel`'s duty and F1x for the period PWMCNT `ticks` is in: a
    /// period begun since the last synchronization takes the duty
    /// register and the F1 bit
    /// (GPTRM 6.2, 6.3.1).
    const fn effective(&self, channel: Channel, ticks: u64) -> (u8, bool) {
        let index = channel.index();
        if self.period_of(channel, ticks) == self.period[index] {
            (self.buffer[index], self.high[index])
        } else {
            (self.duty[index], self.control & channel.bit(PWMC_F1A) != 0)
        }
    }

    /// Brings the buffers to the prescaler's `count`.
    pub(super) fn sync(&mut self, count: u64) {
        let ticks = self.count(count);
        for channel in Channel::BOTH {
            let (duty, high) = self.effective(channel, ticks);
            let index = channel.index();
            self.buffer[index] = duty;
            self.high[index] = high;
            self.period[index] = self.period_of(channel, ticks);
        }
    }

    /// PWMC written at the prescaler's `count`, synchronized: a new tap
    /// or mode starts its count from here.
    pub(super) fn write_control(&mut self, value: u8, count: u64) {
        self.counter.set_divisor(Self::divisor(value), count);
        self.control = value;
        let ticks = self.count(count);
        for channel in Channel::BOTH {
            self.period[channel.index()] = self.period_of(channel, ticks);
        }
    }

    /// A rising edge on PCLK.
    pub(super) const fn pclk(&mut self) {
        self.counter.pclk();
    }

    /// PWMCNT.
    pub(super) const fn counter(&self) -> Counter {
        self.counter
    }

    /// The prescaler's count at which PWMCNT reaches `ticks`, if a tap
    /// clocks it.
    pub(super) const fn count_at(&self, ticks: u64) -> Option<u64> {
        self.counter.count_at(ticks)
    }

    /// Whether `channel`'s pin is forced to F1x (GPTRM 3.3.4).
    const fn forced(&self, channel: Channel) -> bool {
        self.force & channel.bit(FPWMA) != 0
    }

    /// Whether `channel`'s pin carries PWMCNT's clock (PPROUT; GPTRM
    /// 6.3.1): PWMA unless forced.
    pub(super) const fn clock_out(&self, channel: Channel) -> bool {
        matches!(channel, Channel::A) && self.control & PWMC_PPROUT != 0 && !self.forced(channel)
    }

    /// The level on `channel`'s pin at the prescaler's `count`, the
    /// clock under PPROUT aside, which
    /// [`Gpt::pwm_level`](super::Gpt::pwm_level) gives.
    pub(super) const fn level(&self, channel: Channel, count: u64) -> bool {
        self.level_at(channel, self.count(count))
    }

    /// The level on `channel`'s pin at PWMCNT `ticks`: F1x as a
    /// discrete output, high from the period's start until the phase
    /// reaches the duty (GPTRM 1.5, 6.2).
    pub(super) const fn level_at(&self, channel: Channel, ticks: u64) -> bool {
        if self.forced(channel) {
            return self.control & channel.bit(PWMC_F1A) != 0;
        }
        let (duty, high) = self.effective(channel, ticks);
        let phase = (ticks >> self.shift(channel)) & ((1 << DUTY_BITS) - 1);
        high || phase < duty as u64
    }

    /// The first PWMCNT count after `ticks` at which `channel`'s level
    /// changes while the registers stay as they are: a period's start
    /// or the count its phase reaches the duty (GPTRM 6.2).  `None` if
    /// the level holds.
    pub(super) fn next_edge(&self, channel: Channel, ticks: u64) -> Option<u64> {
        if self.forced(channel) {
            return None;
        }
        let shift = self.shift(channel);
        let span = 1 << (shift + DUTY_BITS);
        let level = self.level_at(channel, ticks);
        let mut at = ticks;
        for _ in 0..EDGE_CANDIDATES {
            let start = at & !(span - 1);
            let (duty, high) = self.effective(channel, at);
            let end = start + (u64::from(duty) << shift);
            at = if !high && end > at { end } else { start + span };
            if self.level_at(channel, at) != level {
                return Some(at);
            }
        }
        None
    }

    /// What `channel`'s pin carries at the prescaler's `count`.
    pub(super) const fn output(&self, channel: Channel, count: u64) -> Output {
        let clocks = Self::divisor(self.control);
        if self.forced(channel) {
            return Output::Level(self.control & channel.bit(PWMC_F1A) != 0);
        }
        if self.clock_out(channel) {
            return Output::Clock { clocks };
        }
        let (duty, high) = self.effective(channel, self.count(count));
        if high || duty == 0 {
            return Output::Level(high);
        }
        let unit = 1 << self.shift(channel);
        Output::Wave {
            period: unit << DUTY_BITS,
            high: unit * duty as u32,
            clocks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Channel;
    use super::ParseChannelError;

    #[test]
    fn channels_print_as_their_letters_and_parse_back_in_any_case() {
        for channel in Channel::BOTH {
            assert_eq!(channel.to_string().parse(), Ok(channel));
        }
        assert_eq!("b".parse(), Ok(Channel::B));
        assert_eq!(
            "?".parse::<Channel>(),
            Err(ParseChannelError("?".to_owned()))
        );
    }
}
