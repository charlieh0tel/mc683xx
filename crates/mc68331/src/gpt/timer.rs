//! The GPT's prescaler and the counters its taps clock (GPTRM 5.1):
//! counted from the system clock, not stepped, so a counter's value at
//! any time and the time it reaches any value are a division away.

use mc683xx_cpu32::cycles::Cycles;

/// The nine-stage divider of the system clock (GPTRM 5.1), its count
/// unwrapped.  STOP and STOPP stop it; INCP steps it (GPTRM 9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Prescaler {
    /// Its count when it last started or stopped.
    base: u64,
    /// When it started, while it runs.
    since: Option<Cycles>,
}

impl Prescaler {
    /// The prescaler from zero, running from `now`.
    pub(super) const fn new(now: Cycles) -> Self {
        Self {
            base: 0,
            since: Some(now),
        }
    }

    /// Its count at `now`.
    pub(super) const fn count(&self, now: Cycles) -> u64 {
        match self.since {
            Some(since) => self.base + since.until(now).0,
            None => self.base,
        }
    }

    /// It stops at `now`.
    pub(super) const fn stop(&mut self, now: Cycles) {
        self.base = self.count(now);
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

    /// When its count reaches `count`, if it runs.
    pub(super) const fn when(&self, count: u64) -> Option<Cycles> {
        match self.since {
            Some(since) => Some(Cycles(since.0 + count.saturating_sub(self.base))),
            None => None,
        }
    }
}

/// A counter one of the prescaler's taps clocks, or PCLK: TCNT or
/// PWMCNT (GPTRM 5.1), its count unwrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Counter {
    /// Its count when last synchronized.
    ticks: u64,
    /// The prescaler's count then.
    at: u64,
    /// The tap's divisor of the system clock, or `None` for PCLK.
    divisor: Option<u64>,
}

impl Counter {
    /// The counter from zero at the prescaler's `count`, on the tap of
    /// `divisor`.
    pub(super) const fn new(divisor: Option<u64>, count: u64) -> Self {
        Self {
            ticks: 0,
            at: count,
            divisor,
        }
    }

    /// Its count at the prescaler's `count`: one for each time the tap
    /// divides it anew.
    pub(super) const fn ticks(&self, count: u64) -> u64 {
        match self.divisor {
            Some(divisor) => self.ticks + count / divisor - self.at / divisor,
            None => self.ticks,
        }
    }

    /// A new tap from the prescaler's `count`.
    pub(super) const fn set_divisor(&mut self, divisor: Option<u64>, count: u64) {
        self.ticks = self.ticks(count);
        self.at = count;
        self.divisor = divisor;
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
            Some(divisor) => Some(divisor * (ticks.saturating_sub(self.ticks) + self.at / divisor)),
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
