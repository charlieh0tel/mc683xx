//! The GPT's prescaler and the counters its taps clock (GPTRM 5.1):
//! counted from the system clock, not stepped, so a counter's value at
//! any time and the time it reaches any value are a division away.

use mc683xx_cpu32::cycles::Cycles;

/// The span of the prescaler, nine stages (GPTRM 5.1).
pub(super) const PRESCALER_SPAN: u64 = 1 << 9;

/// The nine-stage divider of the system clock (GPTRM 5.1), its count
/// unwrapped.  STOP and STOPP stop it; INCP steps it (GPTRM 9.4); a
/// write while FREEZE is asserted sets it (GPTRM 5.1, 9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Prescaler {
    /// The system clocks and INCP steps it has counted when it last
    /// started or stopped.
    base: u64,
    /// When it started, while it runs.
    since: Option<Cycles>,
    /// What writes added to the counted, less than its span.
    phase: u64,
}

impl Prescaler {
    /// The prescaler from zero, running from `now`.
    pub(super) const fn new(now: Cycles) -> Self {
        Self {
            base: 0,
            since: Some(now),
            phase: 0,
        }
    }

    /// The system clocks and INCP steps it has counted by `now`.
    const fn counted(&self, now: Cycles) -> u64 {
        match self.since {
            Some(since) => self.base + since.until(now).0,
            None => self.base,
        }
    }

    /// Its count at `now`.
    pub(super) const fn count(&self, now: Cycles) -> u64 {
        self.counted(now) + self.phase
    }

    /// Its stages set at `now` to `value`'s low nine bits.
    pub(super) const fn set(&mut self, value: u64, now: Cycles) {
        self.phase = value.wrapping_sub(self.counted(now)) % PRESCALER_SPAN;
    }

    /// It stops at `now`.
    pub(super) const fn stop(&mut self, now: Cycles) {
        self.base = self.counted(now);
        self.since = None;
    }

    /// It runs again from `now`.
    pub(super) const fn start(&mut self, now: Cycles) {
        if self.since.is_none() {
            self.since = Some(now);
        }
    }

    /// INCP: one count while it is stopped.
    pub(super) const fn step(&mut self) {
        if self.since.is_none() {
            self.base += 1;
        }
    }

    /// Whether a snapshot's prescaler is one a run brought to `now`: it
    /// started no later, and counted no more than the system clocks
    /// since, INCP's steps among them, its writes' phase within its
    /// span.
    pub(super) const fn is_valid(&self, now: Cycles) -> bool {
        if self.phase >= PRESCALER_SPAN {
            return false;
        }
        match self.since {
            Some(since) => since.0 <= now.0 && self.base <= since.0,
            None => self.base <= now.0,
        }
    }

    /// When its count reaches `count`, if it runs.
    pub(super) const fn when(&self, count: u64) -> Option<Cycles> {
        match self.since {
            Some(since) => Some(Cycles(
                since.0 + count.saturating_sub(self.base + self.phase),
            )),
            None => None,
        }
    }
}

/// A sixteen-bit counter one of the prescaler's taps clocks, or PCLK:
/// TCNT or PWMCNT (GPTRM 5.1), its count unwrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Counter {
    /// The clocks it has counted when last synchronized.
    ticks: u64,
    /// The prescaler's count then.
    at: u64,
    /// The tap's divisor of the system clock, or `None` for PCLK.
    divisor: Option<u64>,
    /// What writes added to the counted, modulo its span (GPTRM 9.3).
    offset: u16,
}

impl Counter {
    /// The counter from zero at the prescaler's `count`, on the tap of
    /// `divisor`.
    pub(super) const fn new(divisor: Option<u64>, count: u64) -> Self {
        Self {
            ticks: 0,
            at: count,
            divisor,
            offset: 0,
        }
    }

    /// The clocks it has counted by the prescaler's `count`: one for
    /// each time the tap divides it anew.
    const fn counted(&self, count: u64) -> u64 {
        match self.divisor {
            Some(divisor) => self.ticks + count / divisor - self.at / divisor,
            None => self.ticks,
        }
    }

    /// Its count at the prescaler's `count`.
    pub(super) const fn ticks(&self, count: u64) -> u64 {
        self.counted(count) + self.offset as u64
    }

    /// Whether a snapshot's counter is one a run brought to the
    /// prescaler's `count` at `now`: synchronized no later, and counted
    /// no more than the system clocks to `now`.
    pub(super) const fn is_valid(&self, count: u64, now: Cycles) -> bool {
        self.at <= count && self.ticks <= now.0 && self.counted(count) <= now.0
    }

    /// Synchronized at the prescaler's `count`, its count kept, as the
    /// prescaler moves to `to`: the tap clocks it next where `to`
    /// counts to a multiple of the divisor.
    pub(super) const fn rebase(&mut self, count: u64, to: u64) {
        self.ticks = self.counted(count);
        self.at = to;
    }

    /// A new tap from the prescaler's `count`.
    pub(super) const fn set_divisor(&mut self, divisor: Option<u64>, count: u64) {
        self.rebase(count, count);
        self.divisor = divisor;
    }

    /// Its count set to `value` at the prescaler's `count`, the tap's
    /// phase kept.
    pub(super) const fn set(&mut self, value: u16, count: u64) {
        self.rebase(count, count);
        self.offset = value.wrapping_sub(self.ticks as u16);
    }

    /// A rising edge on PCLK.
    pub(super) const fn pclk(&mut self) {
        if self.divisor.is_none() {
            self.ticks += 1;
        }
    }

    /// The prescaler's count at which it reaches `ticks`, if a tap
    /// clocks it.
    pub(super) const fn count_at(&self, ticks: u64) -> Option<u64> {
        match self.divisor {
            Some(divisor) => Some(
                divisor
                    * (ticks.saturating_sub(self.ticks + self.offset as u64) + self.at / divisor),
            ),
            None => None,
        }
    }

    /// The tap's divisor of the system clock, or `None` for PCLK.
    pub(super) const fn divisor(&self) -> Option<u64> {
        self.divisor
    }

    /// Whether PCLK clocks it.
    pub(super) const fn on_pclk(&self) -> bool {
        self.divisor.is_none()
    }

    /// Whether its clock is high at the prescaler's `count`, as CPROUT
    /// and PPROUT drive it out: the period of the tap, high one system
    /// clock, the count in which the tap clocks it (GPTRM 5.1).  `None`
    /// for PCLK.
    pub(super) const fn clock_high(&self, count: u64) -> Option<bool> {
        match self.divisor {
            Some(divisor) => Some(count.is_multiple_of(divisor)),
            None => None,
        }
    }

    /// The first prescaler count after `count` at which its clock
    /// changes level: the next count after a high one, else the next
    /// the tap clocks it at (every divisor is at least 2).  `None` for
    /// PCLK.
    pub(super) const fn next_clock_edge(&self, count: u64) -> Option<u64> {
        match self.divisor {
            Some(divisor) if count.is_multiple_of(divisor) => Some(count + 1),
            Some(divisor) => Some((count / divisor + 1) * divisor),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Counter;
    use super::PRESCALER_SPAN;
    use super::Prescaler;
    use mc683xx_cpu32::cycles::Cycles;

    #[test]
    fn a_counter_counts_its_tap_across_a_change() {
        let prescaler = Prescaler::new(Cycles(100));
        let count = prescaler.count(Cycles(100 + 37));
        let mut counter = Counter::new(Some(4), 0);
        assert_eq!(counter.ticks(count), 9);
        counter.set_divisor(Some(16), count);
        assert_eq!(counter.ticks(count + 10), 9);
        assert_eq!(counter.ticks(count + 11), 10, "at 48");
        assert_eq!(counter.count_at(11), Some(64));
        assert_eq!(prescaler.when(64), Some(Cycles(164)));
    }

    #[test]
    fn a_counters_clock_is_high_one_count_a_tick() {
        let counter = Counter::new(Some(8), 3);
        assert_eq!(counter.clock_high(16), Some(true));
        assert_eq!(counter.clock_high(17), Some(false));
        assert_eq!(counter.next_clock_edge(16), Some(17));
        assert_eq!(counter.next_clock_edge(17), Some(24));
        assert_eq!(counter.next_clock_edge(23), Some(24));
        let pclk = Counter::new(None, 0);
        assert!(pclk.on_pclk());
        assert_eq!(pclk.clock_high(16), None);
        assert_eq!(pclk.next_clock_edge(16), None);
    }

    #[test]
    fn a_written_counter_keeps_its_taps_phase() {
        let mut counter = Counter::new(Some(4), 0);
        counter.set(0xffff, 10);
        assert_eq!(counter.ticks(11), 0xffff);
        assert_eq!(counter.ticks(12), 0x1_0000);
        assert_eq!(counter.count_at(0x1_0001), Some(16));
        assert!(counter.is_valid(12, Cycles(12)));
    }

    #[test]
    fn a_written_prescaler_keeps_its_phase_within_its_span() {
        let mut prescaler = Prescaler::new(Cycles::ZERO);
        prescaler.set(0xfffd, Cycles(1000));
        let count = prescaler.count(Cycles(1000));
        assert_eq!(count % PRESCALER_SPAN, 0x1fd);
        assert_eq!(prescaler.when(count + 3), Some(Cycles(1003)));
        assert!(prescaler.is_valid(Cycles(1000)));
        prescaler.phase = PRESCALER_SPAN;
        assert!(!prescaler.is_valid(Cycles(1000)));
    }

    #[test]
    fn a_stopped_prescaler_steps() {
        let mut prescaler = Prescaler::new(Cycles::ZERO);
        prescaler.stop(Cycles(5));
        prescaler.step();
        assert_eq!(prescaler.count(Cycles(100)), 6);
        assert_eq!(prescaler.when(7), None);
        prescaler.start(Cycles(100));
        assert_eq!(prescaler.when(7), Some(Cycles(101)));
    }
}
