//! What a port drives on its pins, shared by every module's ports.

use serde::Deserialize;
use serde::Serialize;

/// A port's outputs: the pins it drives and their levels.  Its bytes
/// in a snapshot are the two masks', `driven` first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortLevels {
    /// The pins it drives, a mask.
    pub driven: u8,
    /// Their levels; zero on the pins it does not drive.
    pub levels: u8,
}

impl PortLevels {
    /// `driven`'s pins at `levels`, the levels of the pins not driven
    /// cleared.
    #[must_use]
    pub const fn new(driven: u8, levels: u8) -> Self {
        Self {
            driven,
            levels: levels & driven,
        }
    }

    /// The level on each pin, `undriven` on the pins not driven.
    #[must_use]
    pub const fn pin_levels(self, undriven: u8) -> u8 {
        self.levels & self.driven | undriven & !self.driven
    }
}

#[cfg(test)]
mod tests {
    use super::PortLevels;

    #[test]
    fn undriven_pins_read_zero_and_take_others() {
        let levels = PortLevels::new(0x0f, 0xff);
        assert_eq!(levels.levels, 0x0f);
        assert_eq!(levels.pin_levels(0xa0), 0xaf);
    }
}
