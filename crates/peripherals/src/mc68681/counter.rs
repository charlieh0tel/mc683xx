//! The 16-bit counter/timer (MC68681 3.5, 4.2.13.2, 4.2.15.5,
//! 4.2.17, 4.2.18), counted in closed form from X1/CLK ticks: a fast
//! timer costs nothing until something looks at it.

use mc683xx_cpu32::cpu::state::StateError;
use serde::Deserialize;
use serde::Serialize;

use super::registers::COUNTER_TURN;
use super::registers::PRELOAD_MINIMUM;

/// The counter/timer's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Counter {
    /// CTUR:CTLR, the preload.
    pub(super) preload: u16,
    /// Whether it runs: a counter between its start and stop commands, a
    /// timer from its first start command on, or the MC68681's from
    /// RESET.
    pub(super) running: bool,
    /// The X1/CLK tick its current count started at.
    anchor: u64,
    /// Source clocks from `anchor` to its next terminal count.
    count: u32,
    /// Terminal counts since the timer's last start command: ISR\[3\] is set
    /// at every second one (3.5.2).
    terminal_counts: u64,
    /// The counter/timer output, high when true: the timer's square wave,
    /// or the counter's, low from terminal count to its stop command
    /// (4.2.11.5).
    pub(super) output: bool,
    /// ISR\[3\], counter/timer ready.
    pub(super) ready: bool,
    /// The count a stopped counter holds.
    stopped: u16,
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            preload: 0,
            running: false,
            anchor: 0,
            count: 0,
            terminal_counts: 0,
            output: true,
            ready: false,
            stopped: 0,
        }
    }
}

/// How the counter/timer counts now: its mode and its source clock's
/// period in X1/CLK ticks, `None` for a source the model does not have
/// (IP2, or a transmitter on an external clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Source {
    /// Timer mode (ACR\[6\]) rather than counter mode.
    pub(super) timer: bool,
    /// X1/CLK ticks a source clock.
    pub(super) period: Option<u64>,
}

impl Counter {
    /// The preload as a count: zero is a full turn of the counter, and
    /// one, below the datasheets' minimum of 2, counts as 2.
    fn loaded(&self) -> u32 {
        match u32::from(self.preload) {
            0 => COUNTER_TURN,
            preload => preload.max(PRELOAD_MINIMUM),
        }
    }

    /// Refuses a count past a full turn of the counter.
    pub(super) fn check(&self) -> Result<(), StateError> {
        if self.count <= COUNTER_TURN {
            Ok(())
        } else {
            Err(StateError::invalid("a DUART count past a full turn"))
        }
    }

    /// RESET (2.4): stopped, the output high, ISR\[3\] clear.
    pub(super) fn reset(&mut self) {
        *self = Self {
            preload: self.preload,
            ..Self::default()
        };
    }

    /// The MC68681's timer out of RESET at tick `now`: running on its
    /// preload, its output high (2.4, 3.5).
    pub(super) fn run_from(&mut self, now: u64) {
        self.running = true;
        self.anchor = now;
        self.count = self.loaded();
    }

    /// The tick of the next terminal count, if it counts.
    fn next_terminal(&self, source: Source) -> Option<u64> {
        let period = source.period?;
        self.running
            .then(|| self.anchor + u64::from(self.count) * period)
    }

    /// Brings it to tick `now`.
    pub(super) fn catch_up(&mut self, now: u64, source: Source) {
        let Some(period) = source.period else {
            return;
        };
        let Some(terminal) = self.next_terminal(source).filter(|&at| at <= now) else {
            return;
        };
        if !source.timer {
            // the counter: ready and its output low at terminal count; it
            // rolls over to $FFFF and counts on (3.5.1)
            self.ready = true;
            self.output = false;
            self.anchor = terminal;
            self.count = COUNTER_TURN;
            let turns = (now - terminal) / (u64::from(COUNTER_TURN) * period);
            self.anchor += turns * u64::from(COUNTER_TURN) * period;
            return;
        }
        // the timer: each terminal count inverts the output and reloads,
        // the preload written since taking effect there; every second sets
        // ISR[3] (3.5.2)
        self.count = self.loaded();
        let half = u64::from(self.count) * period;
        let after = (now - terminal) / half;
        let counts = 1 + after;
        let first = self.terminal_counts + 1;
        self.terminal_counts += counts;
        if !counts.is_multiple_of(2) {
            self.output = !self.output;
        }
        // an even count among first..=first + after
        if counts >= 2 || first.is_multiple_of(2) {
            self.ready = true;
        }
        self.anchor = terminal + after * half;
    }

    /// The value the count registers read at tick `now` (4.2.17).
    pub(super) fn value(&self, now: u64, source: Source) -> u16 {
        if !self.running {
            return self.stopped;
        }
        let Some(period) = source.period else {
            return self.stopped;
        };
        let elapsed = (now - self.anchor) / period;
        // Taken mod 2^16 below, so a wrapping reinterpretation is exact.
        let remaining = i64::from(self.count) - elapsed.cast_signed();
        remaining.rem_euclid(i64::from(COUNTER_TURN)) as u16
    }

    /// The start counter command at tick `now`: a counter loads its
    /// preload and counts down; a timer ends its cycle, inverts its
    /// output and starts a new one (3.5.1, 3.5.2).
    pub(super) fn start(&mut self, now: u64, source: Source) {
        self.catch_up(now, source);
        if source.timer {
            self.output = !self.output;
            self.terminal_counts = 0;
        }
        self.running = true;
        self.anchor = now;
        self.count = self.loaded();
    }

    /// The stop counter command at tick `now`: ISR\[3\] clear, and a
    /// counter stopped, its output high again; a timer runs on.
    pub(super) fn stop(&mut self, now: u64, source: Source) {
        self.catch_up(now, source);
        self.ready = false;
        if !source.timer {
            self.stopped = self.value(now, source);
            self.running = false;
            self.output = true;
        }
    }

    /// The counter or timer's source changes at tick `now` (ACR, or a
    /// transmitter's clock select): it goes on from the count it has,
    /// on the new source.  The datasheets say to change it only while
    /// the counter/timer is unused (4.2.13.2).
    pub(super) fn rebase(&mut self, now: u64, old: Source) {
        self.catch_up(now, old);
        if self.running {
            let value = u32::from(self.value(now, old));
            self.count = if value == 0 { COUNTER_TURN } else { value };
            self.anchor = now;
        }
    }

    /// The tick it next changes on its own: the counter's terminal
    /// count, until it reaches it; the timer's, when its output shows on
    /// a pin (`every`), or else the one that sets ISR\[3\] while it is clear
    /// and `ready_wanted`.
    pub(super) fn next_event(
        &self,
        source: Source,
        every: bool,
        ready_wanted: bool,
    ) -> Option<u64> {
        let terminal = self.next_terminal(source)?;
        if !source.timer {
            return (!self.ready || self.output).then_some(terminal);
        }
        if every {
            return Some(terminal);
        }
        if self.ready || !ready_wanted {
            return None;
        }
        if (self.terminal_counts + 1).is_multiple_of(2) {
            Some(terminal)
        } else {
            // the period is known: `next_terminal` had one
            let period = source.period?;
            Some(terminal + u64::from(self.loaded()) * period)
        }
    }

    /// The timer's output as a 16X clock: its period in X1/CLK ticks,
    /// twice the preload in source clocks, while it runs on X1/CLK
    /// (3.5.2).
    pub(super) fn clock_divisor(&self, source: Source) -> Option<u64> {
        let period = source.period?;
        (source.timer && self.running).then(|| 2 * u64::from(self.count) * period)
    }
}

#[cfg(test)]
mod tests {
    use super::COUNTER_TURN;
    use super::Counter;

    #[test]
    fn a_check_refuses_a_count_past_a_full_turn() {
        let full = Counter {
            count: COUNTER_TURN,
            ..Counter::default()
        };
        assert!(full.check().is_ok());
        let past = Counter {
            count: COUNTER_TURN + 1,
            ..Counter::default()
        };
        assert!(past.check().is_err());
    }
}
