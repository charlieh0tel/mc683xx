//! The DUART's clock and character format: board time in X1/CLK ticks,
//! the baud rate generator, and the format its mode registers give.

use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::frequency::NANOHERTZ_PER_HERTZ;

use crate::character::Format;
use crate::character::Parity;
use crate::character::SIXTEENTHS;

use super::registers::BRG_SET_1;
use super::registers::BRG_SET_2;
use super::registers::MIN_DATA_BITS;
use super::registers::MR1_BITS_MASK;
use super::registers::MR1_PARITY_MASK;
use super::registers::MR1_PARITY_SHIFT;
use super::registers::MR1_PARITY_TYPE;
use super::registers::MR2_STOP_MASK;
use super::registers::PARITY_FORCE;
use super::registers::PARITY_MULTIDROP;
use super::registers::PARITY_WITH;

/// The clock on X1/CLK, which every rate and the counter/timer derive
/// from (MC68681 1.2): board time to its ticks and back.  Tick `n` is
/// at the first picosecond at or after `n` periods from power-on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Crystal {
    /// Its frequency, hertz.
    pub(super) hz: u64,
}

impl Crystal {
    /// The last tick at or before `time`.
    pub(super) fn tick(self, time: Time) -> u64 {
        (u128::from(time.0) * u128::from(self.hz) / u128::from(PICOSECONDS_PER_SECOND)) as u64
    }

    /// When tick `tick` is.
    pub(super) fn time(self, tick: u64) -> Time {
        let scaled = u128::from(tick) * u128::from(PICOSECONDS_PER_SECOND);
        Time(scaled.div_ceil(u128::from(self.hz)) as u64)
    }

    /// The picoseconds `ticks` ticks take, rounded down.
    pub(super) fn span(self, ticks: u64) -> u64 {
        (u128::from(ticks) * u128::from(PICOSECONDS_PER_SECOND) / u128::from(self.hz)) as u64
    }

    /// The bit rate of a 16X clock of X1/CLK divided by `divisor`.
    pub(super) fn baud(self, divisor: u64) -> Frequency {
        let nanohertz = u128::from(self.hz) * u128::from(NANOHERTZ_PER_HERTZ)
            / u128::from(SIXTEENTHS * divisor);
        Frequency(nanohertz as u64)
    }
}

/// The baud rate generator's divisor for clock select `code` in the set
/// ACR\[7\] picks, or `None` for the timer and the external clocks
/// (Table 4-5, CSRA).
pub(super) fn generator(code: u8, set_2: bool) -> Option<u64> {
    let rates = if set_2 { &BRG_SET_2 } else { &BRG_SET_1 };
    rates.get(usize::from(code)).copied()
}

/// The format MR1 `mr1` and MR2 `mr2` give (MC68681 4.2.1.4-4.2.1.6,
/// 4.2.2.4).
pub(super) const fn format_of(mr1: u8, mr2: u8) -> Format {
    let data_bits = MIN_DATA_BITS + (mr1 & MR1_BITS_MASK);
    let high = mr1 & MR1_PARITY_TYPE != 0;
    let parity = match mr1 >> MR1_PARITY_SHIFT & MR1_PARITY_MASK {
        PARITY_WITH => Parity::With(high),
        PARITY_FORCE => Parity::Force(high),
        PARITY_MULTIDROP => Parity::Multidrop(high),
        _ => Parity::None,
    };
    // MR2[3:0]: 9/16 to 1 bit by sixteenths for codes 0 to 7 (1 1/16
    // to 1 1/2 with five data bits), 1 9/16 to 2 for 8 to 15 (MC68681
    // Table 4-5, MR2A)
    let code = (mr2 & MR2_STOP_MASK) as u64;
    let stop = if code < 8 {
        9 + code + if data_bits == MIN_DATA_BITS { 8 } else { 0 }
    } else {
        17 + code
    };
    Format {
        data_bits,
        parity,
        stop,
    }
}
