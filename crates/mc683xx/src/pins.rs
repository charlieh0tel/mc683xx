//! What a port drives on its pins, shared by every module's ports.

use serde::Deserialize;
use serde::Serialize;

/// A port's outputs: the pins it drives and their levels, never a level
/// on a pin it does not drive.  Its bytes in a snapshot are the two
/// masks', `driven` first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "Masks")]
pub struct PortLevels {
    /// The pins it drives, a mask.
    driven: u8,
    /// Their levels; zero on the pins it does not drive.
    levels: u8,
}

/// [`PortLevels`] as a snapshot holds it, masked when it is read back.
#[derive(Deserialize)]
#[serde(rename = "PortLevels")]
struct Masks {
    /// The pins driven.
    driven: u8,
    /// Their levels.
    levels: u8,
}

impl From<Masks> for PortLevels {
    fn from(masks: Masks) -> Self {
        Self::new(masks.driven, masks.levels)
    }
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

    /// The pins it drives, a mask.
    #[must_use]
    pub const fn driven(self) -> u8 {
        self.driven
    }

    /// The levels of the pins it drives; zero on the others.
    #[must_use]
    pub const fn levels(self) -> u8 {
        self.levels
    }

    /// The level on each pin, `undriven` on the pins not driven.
    #[must_use]
    pub const fn pin_levels(self, undriven: u8) -> u8 {
        self.levels | undriven & !self.driven
    }
}

/// A port's registers and the levels the board drives on its pins, in
/// one form for every module's ports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct PortState {
    /// The data latch: what the firmware last wrote to the port's data
    /// register.
    pub latch: u8,
    /// The data direction register: a set bit makes the pin an output.
    pub direction: u8,
    /// The pin assignment: a set bit gives the pin its module function
    /// rather than general-purpose I/O.
    pub assignment: u8,
    /// The levels the board drives on the pins.
    pub pins: u8,
}

impl PortState {
    /// A port's data latch, data direction, pin assignment and the
    /// levels the board drives on its pins.
    ///
    /// Not part of the semver API: a host receives a port's state
    /// (`Mc68331::port`) and reads its fields; the modules build it.
    #[doc(hidden)]
    #[must_use]
    pub const fn new(latch: u8, direction: u8, assignment: u8, pins: u8) -> Self {
        Self {
            latch,
            direction,
            assignment,
            pins,
        }
    }
}

/// The levels the board drives on a port's pins, `pins`, with those in
/// `mask` driven to their levels in `value` instead.
///
/// Not part of the semver API: plumbing the modules' masked pin setters
/// share.
#[doc(hidden)]
#[must_use]
pub const fn merge_pins(pins: u8, value: u8, mask: u8) -> u8 {
    pins & !mask | value & mask
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
