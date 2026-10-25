//! System protection (SIMRM 3; MC68331UM 4.2): the software watchdog,
//! the periodic interrupt timer, and SYPCR's bus and halt monitors.
//! The watchdog and the timer count EXTAL periods
//! ([`SystemClock::tick`](super::clock::SystemClock::tick)).

use mc683xx_cpu32::interrupt::InterruptLevel;

/// SYPCR SWE: the software watchdog is enabled (MC68331UM D.3.12).
const SYPCR_SWE: u8 = 1 << 7;
/// SYPCR SWP: the watchdog's clock is prescaled by 512.
const SYPCR_SWP: u8 = 1 << 6;
/// SYPCR SWT, bits 5:4: the watchdog's time-out.
const SYPCR_SWT_SHIFT: u8 = 4;
/// SYPCR SWT, unshifted.
const SYPCR_SWT_MASK: u8 = 0b11;
/// SYPCR HME: the halt monitor resets on a double bus fault.
const SYPCR_HME: u8 = 1 << 3;
/// SYPCR BME: the bus monitor times internal-to-external cycles.
const SYPCR_BME: u8 = 1 << 2;
/// SYPCR BMT, bits 1:0: the bus monitor's period.
const SYPCR_BMT_MASK: u8 = 0b11;
/// The bus monitor's longest period, BMT 00; each step halves it
/// (MC68331UM Table 4-2).
const BUS_MONITOR_CLOCKS: u8 = 64;
/// The watchdog's ratio, as a power of two of EXTAL periods, for SWP
/// and SWT clear; each step of SWT adds two, SWP nine (MC68331UM Table
/// 4-4).
const WATCHDOG_LEAST_POWER: u32 = 9;
/// What a step of SWT adds to the power.
const WATCHDOG_SWT_STEP: u32 = 2;
/// What SWP adds to the power: the 2^9 prescaler.
const WATCHDOG_SWP_POWER: u32 = 9;
/// The two values written to SWSR in turn that service the watchdog
/// (MC68331UM 4.2.10).
const SWSR_FIRST: u8 = 0x55;
/// The second.
const SWSR_SECOND: u8 = 0xaa;

/// PICR's implemented bits, 10:0 (MC68331UM D.3.13).
const PICR_IMPLEMENTED: u16 = 0x07ff;
/// PICR PIRQL, bits 10:8: the timer's interrupt level.
const PICR_PIRQL_SHIFT: u16 = 8;
/// PICR PIRQL, unshifted.
const PICR_PIRQL_MASK: u16 = 0b111;
/// PICR PIV, bits 7:0: the timer's vector.
const PICR_PIV_MASK: u16 = 0xff;
/// PICR out of reset: disabled, the uninitialized interrupt vector
/// $0F (MC68331UM D.3.13).
const PICR_RESET: u16 = 0x000f;
/// PITR's implemented bits, 8:0 (MC68331UM D.3.14).
const PITR_IMPLEMENTED: u16 = 0x01ff;
/// PITR PTP: the timer's clock is prescaled by 512.
const PITR_PTP: u16 = 1 << 8;
/// PITR PITM, bits 7:0: the timer's modulus.
const PITR_PITM_MASK: u16 = 0xff;
/// The prescaler PTP and SWP select (MC68331UM 4.2.10, 4.2.11).
const PRESCALE: u64 = 512;
/// EXTAL periods in a PITCLK period before the prescaler: the divide
/// by four (MC68331UM 4.2.11, Figure 4-3).
const PIT_DIVIDE: u64 = 4;

/// SYPCR out of reset: the watchdog enabled at its shortest time-out,
/// prescaled when MODCLK is low; the monitors off; BMT 64 clocks
/// (SIMRM Table 8-4; MC68331UM Table 4-3).
pub(crate) const fn sypcr_reset(modclk: bool) -> u8 {
    if modclk {
        SYPCR_SWE
    } else {
        SYPCR_SWE | SYPCR_SWP
    }
}

/// PITR out of reset: PTP set when MODCLK is low, the timer off
/// (MC68331UM Table 4-5, D.3.14).
pub(crate) const fn pitr_reset(modclk: bool) -> u16 {
    if modclk { 0 } else { PITR_PTP }
}

/// SYPCR, which a reset leaves writable once (MC68331UM D.3.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProtectionControl {
    /// Its value.
    pub(crate) value: u8,
    /// Whether the write it allows has happened.
    pub(crate) written: bool,
}

impl ProtectionControl {
    /// SYPCR out of reset.
    pub(crate) const fn new(modclk: bool) -> Self {
        Self {
            value: sypcr_reset(modclk),
            written: false,
        }
    }

    /// Whether the halt monitor resets on a double bus fault.
    pub(crate) const fn halt_monitor(self) -> bool {
        self.value & SYPCR_HME != 0
    }

    /// The bus monitor's period for internal-to-external cycles, in
    /// system clocks, if BME enables it.
    pub(crate) const fn bus_monitor(self) -> Option<u8> {
        if self.value & SYPCR_BME != 0 {
            Some(BUS_MONITOR_CLOCKS >> (self.value & SYPCR_BMT_MASK))
        } else {
            None
        }
    }

    /// Whether SWP prescales the watchdog's clock.
    const fn prescaled(self) -> bool {
        self.value & SYPCR_SWP != 0
    }

    /// The watchdog's time-out in EXTAL periods, if SWE enables it.
    fn watchdog_period(self) -> Option<u64> {
        let swt = u32::from(self.value >> SYPCR_SWT_SHIFT & SYPCR_SWT_MASK);
        let prescale = if self.prescaled() {
            WATCHDOG_SWP_POWER
        } else {
            0
        };
        (self.value & SYPCR_SWE != 0)
            .then(|| 1 << (WATCHDOG_LEAST_POWER + WATCHDOG_SWT_STEP * swt + prescale))
    }
}

/// The software watchdog (MC68331UM 4.2.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Watchdog {
    /// The EXTAL period it times out at, while it runs.
    deadline: Option<u64>,
    /// While LPSTOP stops its clock, the periods it had left
    /// (MC68331UM 4.2.12).
    stopped: Option<u64>,
    /// Whether $55 was the last value written to SWSR.
    armed: bool,
}

impl Watchdog {
    /// The watchdog out of reset at EXTAL period `tick`.
    pub(crate) fn new(control: ProtectionControl, tick: u64) -> Self {
        let mut watchdog = Self {
            deadline: None,
            stopped: None,
            armed: false,
        };
        watchdog.restart(control, tick);
        watchdog
    }

    /// The time-out restarts from `tick`, at SYPCR's period: a new
    /// SWT takes effect only at a service (MC68331UM 4.2.10).
    fn restart(&mut self, control: ProtectionControl, tick: u64) {
        self.deadline = control.watchdog_period().map(|period| tick + period);
    }

    /// SYPCR written at `tick`, from `old`: clearing SWE stops the
    /// watchdog.  SWP switches the prescaler in front of the divider
    /// chain (MC68331UM 4.2.10, Figure 4-3), so the count left runs on
    /// at the new SWCLK rate at once; SWT's tap waits for a service.
    pub(crate) fn control(
        &mut self,
        old: ProtectionControl,
        control: ProtectionControl,
        tick: u64,
    ) {
        if control.watchdog_period().is_none() {
            self.deadline = None;
            self.stopped = None;
            return;
        }
        let rescale = |left: u64| match (old.prescaled(), control.prescaled()) {
            (false, true) => left * PRESCALE,
            (true, false) => left.div_ceil(PRESCALE),
            _ => left,
        };
        self.deadline = self
            .deadline
            .map(|deadline| tick + rescale(deadline.saturating_sub(tick)));
        self.stopped = self.stopped.map(rescale);
    }

    /// `value` written to SWSR at `tick`.
    pub(crate) fn service(&mut self, value: u8, control: ProtectionControl, tick: u64) {
        if value == SWSR_SECOND && self.armed && self.deadline.is_some() {
            self.restart(control, tick);
        }
        self.armed = value == SWSR_FIRST;
    }

    /// LPSTOP stops its clock at `tick`.
    pub(crate) fn stop(&mut self, tick: u64) {
        if let Some(deadline) = self.deadline.take() {
            self.stopped = Some(deadline.saturating_sub(tick));
        }
    }

    /// Its clock runs again at `tick`.
    pub(crate) fn start(&mut self, tick: u64) {
        if let Some(left) = self.stopped.take() {
            self.deadline = Some(tick + left);
        }
    }

    /// It timed out: it stays stopped until the reset it asks for.
    pub(crate) const fn expire(&mut self) {
        self.deadline = None;
    }

    /// The EXTAL period it times out at, while it runs.
    pub(crate) const fn deadline(&self) -> Option<u64> {
        self.deadline
    }
}

/// The periodic interrupt timer (MC68331UM 4.2.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PeriodicTimer {
    /// PICR.
    pub(crate) control: u16,
    /// PITR.
    pub(crate) timing: u16,
    /// The EXTAL period the count running reaches zero at.
    end: Option<u64>,
    /// Whether an interrupt is requested and not yet acknowledged.
    pending: bool,
}

impl PeriodicTimer {
    /// The timer out of reset.
    pub(crate) const fn new(modclk: bool) -> Self {
        Self {
            control: PICR_RESET,
            timing: pitr_reset(modclk),
            end: None,
            pending: false,
        }
    }

    /// Whether a snapshot's PICR and PITR hold implemented bits only.
    pub(crate) const fn is_valid(&self) -> bool {
        self.control & !PICR_IMPLEMENTED == 0 && self.timing & !PITR_IMPLEMENTED == 0
    }

    /// A count's EXTAL periods at PITR's modulus and prescaler, if
    /// PITM turns the timer on.
    fn period(&self) -> Option<u64> {
        let modulus = u64::from(self.timing & PITR_PITM_MASK);
        let prescale = if self.timing & PITR_PTP != 0 {
            PRESCALE
        } else {
            1
        };
        (modulus != 0).then_some(modulus * prescale * PIT_DIVIDE)
    }

    /// PICR written.
    pub(crate) fn write_control(&mut self, value: u16) {
        self.control = value & PICR_IMPLEMENTED;
        if self.level() == InterruptLevel::NONE {
            self.pending = false;
        }
    }

    /// PITR written at `tick`: zero stops the timer; a count running
    /// finishes before a new modulus loads (MC68331UM 4.2.11).
    pub(crate) fn write_timing(&mut self, value: u16, tick: u64) {
        self.timing = value & PITR_IMPLEMENTED;
        match self.period() {
            None => self.end = None,
            Some(period) if self.end.is_none() => self.end = Some(tick + period),
            Some(_) => {}
        }
    }

    /// PIRQL's level.
    fn level(&self) -> InterruptLevel {
        InterruptLevel::new((self.control >> PICR_PIRQL_SHIFT & PICR_PIRQL_MASK) as u8)
            .unwrap_or(InterruptLevel::NONE)
    }

    /// Its interrupt request's level, while one is pending.
    pub(crate) fn request(&self) -> Option<InterruptLevel> {
        self.pending.then(|| self.level())
    }

    /// The CPU took its interrupt: PIV, the request cleared.
    pub(crate) fn acknowledge(&mut self) -> u8 {
        self.pending = false;
        (self.control & PICR_PIV_MASK) as u8
    }

    /// The EXTAL period the count running ends at.
    pub(crate) const fn end(&self) -> Option<u64> {
        self.end
    }

    /// Brings it to `tick`: each count that ended asks for an
    /// interrupt at PIRQL, if PIRQL is not 0, and reloads PITM.
    pub(crate) fn advance(&mut self, tick: u64) {
        while let Some(end) = self.end
            && end <= tick
        {
            self.pending |= self.level() != InterruptLevel::NONE;
            self.end = self.period().map(|period| {
                let missed = (tick - end) / period;
                end + (missed + 1) * period
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PeriodicTimer;
    use super::ProtectionControl;
    use super::Watchdog;
    use mc683xx_cpu32::interrupt::InterruptLevel;

    #[test]
    fn the_watchdog_wants_55_then_aa() {
        let control = ProtectionControl::new(true);
        let mut watchdog = Watchdog::new(control, 0);
        assert_eq!(watchdog.deadline(), Some(512));
        watchdog.service(0xaa, control, 100);
        watchdog.service(0x55, control, 100);
        assert_eq!(watchdog.deadline(), Some(512));
        watchdog.service(0xaa, control, 200);
        assert_eq!(watchdog.deadline(), Some(712));
    }

    #[test]
    fn a_new_time_out_waits_for_a_service() {
        let reset = ProtectionControl::new(true);
        let mut watchdog = Watchdog::new(reset, 0);
        // SWE, SWT 11: 2^15
        let control = ProtectionControl {
            value: 0xb0,
            written: true,
        };
        watchdog.control(reset, control, 0);
        assert_eq!(watchdog.deadline(), Some(512));
        watchdog.service(0x55, control, 10);
        watchdog.service(0xaa, control, 10);
        assert_eq!(watchdog.deadline(), Some(10 + (1 << 15)));
        let off = ProtectionControl {
            value: 0,
            ..control
        };
        watchdog.control(control, off, 10);
        assert_eq!(watchdog.deadline(), None);
    }

    /// SWP's prescaler switches SWCLK at once (MC68331UM Figure 4-3):
    /// the count left runs on 512 times slower, or faster.
    #[test]
    fn a_new_prescale_rescales_the_count_left() {
        let reset = ProtectionControl::new(true);
        let mut watchdog = Watchdog::new(reset, 0);
        let prescaled = ProtectionControl {
            value: 0xc0,
            written: true,
        };
        watchdog.control(reset, prescaled, 112);
        assert_eq!(watchdog.deadline(), Some(112 + 400 * 512));
        watchdog.control(prescaled, reset, 112 + 512 + 1);
        assert_eq!(watchdog.deadline(), Some(112 + 512 + 1 + 399));
    }

    #[test]
    fn the_bus_monitor_period_halves_with_bmt() {
        let control = ProtectionControl {
            value: 0x04,
            written: true,
        };
        assert_eq!(control.bus_monitor(), Some(64));
        let control = ProtectionControl {
            value: 0x07,
            written: true,
        };
        assert_eq!(control.bus_monitor(), Some(8));
        assert_eq!(ProtectionControl::new(true).bus_monitor(), None);
    }

    #[test]
    fn the_timer_requests_at_pirql_and_reloads() {
        let mut timer = PeriodicTimer::new(true);
        timer.write_control(0x0542);
        timer.write_timing(0x0008, 0);
        assert_eq!(timer.end(), Some(32));
        timer.advance(31);
        assert_eq!(timer.request(), None);
        timer.advance(32);
        assert_eq!(timer.request(), InterruptLevel::new(5));
        assert_eq!(timer.acknowledge(), 0x42);
        assert_eq!(timer.request(), None);
        assert_eq!(timer.end(), Some(64));
    }

    #[test]
    fn a_new_modulus_loads_when_the_count_ends() {
        let mut timer = PeriodicTimer::new(true);
        timer.write_timing(0x0001, 0);
        timer.write_timing(0x0102, 1);
        assert_eq!(timer.end(), Some(4));
        timer.advance(4);
        assert_eq!(timer.end(), Some(4 + 2 * 512 * 4));
        assert_eq!(timer.request(), None, "PIRQL 0 asks for nothing");
        timer.write_timing(0, 5);
        assert_eq!(timer.end(), None);
    }
}
