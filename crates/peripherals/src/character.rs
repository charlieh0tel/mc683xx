//! A character's format on an asynchronous serial line, as the UARTs
//! here frame it: a start bit, the data bits LSB first, a parity bit
//! and the stop bits.  Each UART builds a `Format` from its own
//! registers.

/// Sixteenths of a bit in one bit time: the unit of a format's stop
/// bits and of its timing, and the periods of a 16X clock in a bit
/// (MC68681 1.2).
pub(crate) const SIXTEENTHS: u64 = 16;

/// What a character's parity bit is (MC68681 4.2.1.4, 4.2.1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Parity {
    /// None.
    None,
    /// With parity, odd if true.
    With(bool),
    /// Forced to this level.
    Force(bool),
    /// Multidrop: the address/data bit, an address if true (MC68681
    /// 3.4).
    Multidrop(bool),
}

/// A character's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Format {
    /// Data bits, 5 to 8.
    pub(crate) data_bits: u8,
    /// Its parity bit.
    pub(crate) parity: Parity,
    /// The transmitted stop bits, in sixteenths of a bit.
    pub(crate) stop: u64,
}

/// What a receiver makes of a character's bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Decoded {
    /// The data bits, the unused high bits zero.
    pub(crate) data: u8,
    /// A parity error, or in multidrop mode the address/data bit.
    pub(crate) parity_bit_set: bool,
    /// Whether the character is an address character (multidrop mode
    /// only).
    pub(crate) address: bool,
}

impl Format {
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
    /// stop bit, where a receiver checks it.
    pub(crate) const fn to_stop_center(self) -> u64 {
        SIXTEENTHS * (1 + self.data_bits as u64 + self.parity_bits()) + SIXTEENTHS / 2
    }

    /// Sixteenths of a bit in the transmitted character, start bit to
    /// the end of its stop bits.
    pub(crate) const fn length(self) -> u64 {
        SIXTEENTHS * (1 + self.data_bits as u64 + self.parity_bits()) + self.stop
    }

    /// The character's bits on the line, LSB first from bit 0, the
    /// parity bit after the data.
    pub(crate) const fn encode(self, data: u8) -> u16 {
        let bits = data as u16 & self.mask();
        let parity = match self.parity {
            Parity::None => return bits,
            Parity::With(odd) => (bits.count_ones() % 2 == 1) != odd,
            Parity::Force(high) | Parity::Multidrop(high) => high,
        };
        bits | (parity as u16) << self.data_bits
    }

    /// What a receiver of this format makes of `bits` on the line.
    pub(crate) const fn decode(self, bits: u16) -> Decoded {
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
    pub(crate) const fn multidrop(self) -> bool {
        matches!(self.parity, Parity::Multidrop(_))
    }
}
