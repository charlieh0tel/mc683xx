//! What the host supplies around the chip beside its mapped
//! peripherals (`mc683xx_cpu32::peripheral`): the time a peripheral or a
//! module keeps, and the QSPI's peripherals.

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;

/// Anything with its own time: a host peripheral, a module, a model that
/// runs beside the CPU.
///
/// The chip ends a quantum at the earliest next event, so a peripheral sees
/// time move only between quanta.  By default it has no time of its
/// own.
pub trait Timed {
    /// When it next changes on its own, if it will.
    fn next_event(&self) -> Option<Cycles> {
        None
    }
    /// Brings it to `now`.
    fn advance(&mut self, now: Cycles) {
        let _ = now;
    }
}

/// SCK's level between transfers (QSMRM 4.3.1, CPOL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ClockPolarity {
    /// CPOL 0: SCK idles low.
    IdleLow,
    /// CPOL 1: SCK idles high.
    IdleHigh,
}

/// Which SCK edges change and capture the data (QSMRM 4.3.1, CPHA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ClockPhase {
    /// CPHA 0: data is captured on the leading edge and changed on the
    /// following one.
    CaptureLeading,
    /// CPHA 1, out of reset: data is changed on the leading edge and
    /// captured on the following one.
    ChangeLeading,
}

/// The QSPI's serial clock as SPCR0 and the system clock set it
/// (QSMRM 4.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct QspiClock {
    /// CPOL.
    pub polarity: ClockPolarity,
    /// CPHA.
    pub phase: ClockPhase,
    /// System clocks in an SCK period, 2 x SPBR (QSMRM equation 4-1).
    pub period: Cycles,
    /// The system clock's frequency (MC68331UM 4.3).
    #[serde(with = "mc683xx_cpu32::frequency::hertz")]
    pub system_clock: Frequency,
}

impl QspiClock {
    /// SCK's frequency, the system clock / (2 x SPBR) (QSMRM equation
    /// 4-1), rounded down.
    #[must_use]
    pub const fn baud(self) -> Frequency {
        Frequency(self.system_clock.0 / self.period.0)
    }
}

/// MISO when no peripheral drives it, for a transfer of `bits`: all
/// ones, the level this crate and `mc683xx-board` read on it.
#[must_use]
pub const fn floating_miso(bits: u8) -> u16 {
    if bits as u32 >= u16::BITS {
        u16::MAX
    } else {
        (1 << bits) - 1
    }
}

/// A QSPI transfer, one queue entry's, as a peripheral sees it.  Built
/// with [`QspiTransfer::new`], so a field added later gets a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct QspiTransfer {
    /// The queue entry, 0 to 15: its index in the QSPI RAM (QSMRM
    /// 4.3.6).
    pub entry: u8,
    /// The levels on PCS\[3:0\], the entry's code on the pins the QSPI
    /// drives (QSMRM 4.3.6.3).
    pub chip_selects: u8,
    /// The bits shifted out, right-aligned.
    pub out: u16,
    /// How many: 8 to 16 (QSMRM Table 4-2).
    pub bits: u8,
    /// The serial clock it is shifted with: its mode (CPOL, CPHA) and
    /// SCK period.
    pub clock: QspiClock,
    /// When it ends.
    pub now: Cycles,
}

impl QspiTransfer {
    /// Queue entry `entry`'s transfer of `bits` of `out` with
    /// `chip_selects` on PCS\[3:0\], shifted with `clock`, ending at
    /// `now`.
    #[must_use]
    pub const fn new(
        entry: u8,
        chip_selects: u8,
        out: u16,
        bits: u8,
        clock: QspiClock,
        now: Cycles,
    ) -> Self {
        Self {
            entry,
            chip_selects,
            out,
            bits,
            clock,
            now,
        }
    }
}

/// A peripheral on the QSPI (QSMRM 4).
///
/// It sees each transfer ([`QspiTransfer`]) with its queue entry, the
/// levels on PCS\[3:0\], and its serial clock; whether the board uses
/// the four as selects or decodes them to a peripheral number is the
/// board's.  The chip holds one: it is the whole board's QSPI side, so
/// it sees every transfer and every change of the PCS levels, whatever
/// they select.  On a board of `mc683xx-board` peripherals, the board's
/// PCS decode is this peripheral, and each peripheral sees only its own
/// select (`Peripheral::select`).
pub trait SpiPeripheral {
    /// Shifts the transfer's `bits` of `out` to the peripheral its
    /// `chip_selects` select, most significant bit first, in its clock's
    /// mode, and returns the bits shifted back, right-aligned (QSMRM
    /// 4.3.6).
    fn transfer(&mut self, transfer: QspiTransfer) -> u16;

    /// RESET reaches it at `now`, as the chip's QSPI reset scope says
    /// (`Builder::qspi_reset_scope`; every reset by default, SIMRM 8.2).
    /// By default nothing changes.
    fn reset(&mut self, now: Cycles) {
        let _ = now;
    }

    /// The levels on PCS\[3:0\] change at `now`: a queue entry drives its
    /// code, or the pins return to PORTQS between transfers (QSMRM
    /// 4.3.6.3, CONT).  Every change, whatever it selects: a peripheral
    /// framed by its select sees the frame start and end here, as the
    /// levels it decodes go to its code and away.  Levels that do not
    /// change are not told.  By default nothing changes.
    fn chip_selects(&mut self, levels: u8, now: Cycles) {
        let _ = (levels, now);
    }

    /// The serial clock for the transfers from `now` on: told at the
    /// start of the first queue entry, and of any entry after SPCR0's
    /// CPOL, CPHA or SPBR, or the system clock, changed.  A peripheral
    /// that checks its SPI mode or its fastest SCK does it here.  By
    /// default nothing changes.
    fn clock(&mut self, clock: QspiClock, now: Cycles) {
        let _ = (clock, now);
    }

    /// Whether its calls may come late.  By default, no: a run's quantum
    /// ends at each call, so the calls come in time with the CPU's
    /// cycles to the board's other peripherals, and an interrupt or stop
    /// the peripheral asks for in one is seen at once.  If it may, the
    /// queue runs on between the points where the CPU can see it (its
    /// interrupt, a read or write of the QSM, the run's end), and the
    /// calls come in a batch there, each in order with its own `now`.
    /// For a peripheral that shares no state with the board's other
    /// peripherals and asks for no interrupt or stop.
    fn deferrable(&self) -> bool {
        false
    }
}
