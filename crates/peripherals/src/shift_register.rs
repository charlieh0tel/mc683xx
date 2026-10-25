//! A serial-in shift register, as the serial peripherals on the QSPI hold
//! one.

use mc683xx_board::spi::Slave;
use serde::Deserialize;
use serde::Serialize;

/// A serial-in shift register of 1 to 64 stages, shifting toward its
/// most significant: the bit that leaves it is the one that went in as
/// many bits before as it has stages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ShiftRegister {
    /// Its stages, the last bit in at bit 0.
    bits: u64,
}

impl ShiftRegister {
    /// A register holding `bits`, the last bit in at bit 0.
    pub(crate) const fn holding(bits: u64) -> Self {
        Self { bits }
    }

    /// Its `width` stages as a serial slave whose output is the last
    /// stage, for `spi::exchange`.
    pub(crate) const fn stages(&mut self, width: u32) -> Stages<'_> {
        Stages {
            register: self,
            width,
        }
    }

    /// Its stages, the last bit in at bit 0.
    pub(crate) const fn bits(self) -> u64 {
        self.bits
    }

    /// Clears every stage.
    pub(crate) const fn clear(&mut self) {
        self.bits = 0;
    }
}

/// A register's stages as a serial slave: each sample edge shifts MOSI
/// in, and MISO is the last stage.
#[derive(Debug)]
pub(crate) struct Stages<'a> {
    /// The register.
    register: &'a mut ShiftRegister,
    /// Its stages, 1 to 64.
    width: u32,
}

impl Slave for Stages<'_> {
    fn sample(&mut self, mosi: bool) {
        let shifted = self.register.bits << 1 | u64::from(mosi);
        self.register.bits = shifted & (u64::MAX >> (u64::BITS - self.width));
    }

    fn miso(&self) -> Option<bool> {
        Some(self.register.bits >> (self.width - 1) & 1 != 0)
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_board::spi::Slave;

    use super::ShiftRegister;

    /// Shifts the low `count` bits of `word` into `width` stages, most
    /// significant first, and gives the bits that left, the first most
    /// significant.
    fn shift(register: &mut ShiftRegister, width: u32, word: u64, count: u32) -> u64 {
        let mut stages = register.stages(width);
        (0..count).rev().fold(0, |out, bit| {
            let leaving = stages.miso().unwrap();
            stages.sample(word >> bit & 1 != 0);
            out << 1 | u64::from(leaving)
        })
    }

    #[test]
    fn a_register_gives_back_what_went_in_its_width_before() {
        let mut register = ShiftRegister::default();
        assert_eq!(shift(&mut register, 16, 0x1234, 16), 0);
        assert_eq!(shift(&mut register, 16, 0xab, 8), 0x12);
        assert_eq!(register.bits(), 0x34ab);
        assert_eq!(shift(&mut register, 16, 0xffff, 16), 0x34ab);
    }

    #[test]
    fn bits_past_a_short_register_come_straight_through() {
        let mut register = ShiftRegister::default();
        assert_eq!(
            shift(&mut register, 6, 0x0fff, 12),
            0x003f,
            "six old zeros, six ones"
        );
        assert_eq!(register.bits(), 0x3f);
    }

    #[test]
    fn a_wide_register_holds_every_bit() {
        let mut register = ShiftRegister::default();
        for word in [0xabc, 0xdef, 0x123] {
            assert_eq!(shift(&mut register, 36, word, 12), 0);
        }
        assert_eq!(register.bits(), 0xa_bcde_f123);
        assert_eq!(shift(&mut register, 36, 0, 12), 0xabc);
        register.clear();
        assert_eq!(register.bits(), 0);
    }
}
