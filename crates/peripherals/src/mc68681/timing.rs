//! The DUART's clock and character format: board time in X1/CLK ticks,
//! the baud rate generator, and a character's bits on the line.

use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::frequency::NANOHERTZ_PER_HERTZ;

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
use super::registers::SIXTEENTHS;

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

/// What a character's parity bit is (MC68681 4.2.1.4, 4.2.1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Parity {
    /// None.
    None,
    /// With parity, odd if true.
    With(bool),
    /// Forced to this level.
    Force(bool),
    /// Multidrop: the address/data bit, an address if true (3.4).
    Multidrop(bool),
}

/// A character's format, from MR1 and MR2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Format {
    /// Data bits, 5 to 8 (4.2.1.6).
    pub(super) data_bits: u8,
    /// Its parity bit.
    pub(super) parity: Parity,
    /// The transmitted stop bits, in sixteenths of a bit (4.2.2.4).
    pub(super) stop: u64,
}

/// What a receiver makes of a character's bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Decoded {
    /// The data bits, the unused high bits zero (3.2).
    pub(super) data: u8,
    /// A parity error, or in multidrop mode the address/data bit
    /// (4.2.9.3).
    pub(super) parity_bit_set: bool,
    /// Whether the character is an address character (multidrop mode
    /// only).
    pub(super) address: bool,
}

impl Format {
    /// The format MR1 `mr1` and MR2 `mr2` give.
    pub(super) const fn of(mr1: u8, mr2: u8) -> Self {
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
        Self {
            data_bits,
            parity,
            stop,
        }
    }

    /// Whether it has a parity (or address/data) bit.
    const fn parity_bits(self) -> u64 {
        match self.parity {
            Parity::None => 0,
            _ => 1,
        }
    }

    /// The data bits' mask.
    const fn mask(self) -> u16 {
        (1 << self.data_bits) - 1
    }

    /// Sixteenths of a bit from the start bit to the center of the first
    /// stop bit, where the receiver checks it (4.2.2.4).
    pub(super) const fn to_stop_center(self) -> u64 {
        SIXTEENTHS * (1 + self.data_bits as u64 + self.parity_bits()) + SIXTEENTHS / 2
    }

    /// Sixteenths of a bit in the transmitted character, start bit to
    /// the end of its stop bits.
    pub(super) const fn length(self) -> u64 {
        SIXTEENTHS * (1 + self.data_bits as u64 + self.parity_bits()) + self.stop
    }

    /// The character's bits on the line, LSB first from bit 0, the
    /// parity bit after the data (3.1).
    pub(super) const fn encode(self, data: u8) -> u16 {
        let bits = data as u16 & self.mask();
        let parity = match self.parity {
            Parity::None => return bits,
            Parity::With(odd) => (bits.count_ones() % 2 == 1) != odd,
            Parity::Force(high) | Parity::Multidrop(high) => high,
        };
        bits | (parity as u16) << self.data_bits
    }

    /// What a receiver of this format makes of `bits` on the line.
    pub(super) const fn decode(self, bits: u16) -> Decoded {
        let data = bits & self.mask();
        let parity = bits >> self.data_bits & 1 != 0;
        let (parity_bit_set, address) = match self.parity {
            Parity::None => (false, false),
            Parity::With(odd) => (((data.count_ones() % 2 == 1) != parity) != odd, false),
            Parity::Force(high) => (parity != high, false),
            Parity::Multidrop(_) => (parity, parity),
        };
        Decoded {
            data: data as u8,
            parity_bit_set,
            address,
        }
    }

    /// Whether this is multidrop mode.
    pub(super) const fn multidrop(self) -> bool {
        matches!(self.parity, Parity::Multidrop(_))
    }
}
