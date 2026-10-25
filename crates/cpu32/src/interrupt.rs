//! Interrupt requests and their acknowledgment.

use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde::de::Unexpected;

use crate::cycles::Cycles;

/// An interrupt priority level: 0, none, to 7, nonmaskable (CPU32RM
/// 6.2.11).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct InterruptLevel(u8);

/// A snapshot's level, refused above 7.
impl<'de> Deserialize<'de> for InterruptLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let level = u8::deserialize(deserializer)?;
        Self::new(level).ok_or_else(|| {
            D::Error::invalid_value(
                Unexpected::Unsigned(level.into()),
                &"an interrupt level, 0 to 7",
            )
        })
    }
}

impl InterruptLevel {
    /// No request.
    pub const NONE: Self = Self(0);
    /// Level 7, taken on a transition whatever the mask (CPU32RM
    /// 6.2.11).
    pub const NONMASKABLE: Self = Self(7);

    /// Level `level`, if it is one: 0 to 7.
    #[must_use]
    pub const fn new(level: u8) -> Option<Self> {
        if level <= Self::NONMASKABLE.0 {
            Some(Self(level))
        } else {
            None
        }
    }

    /// The level, 0 to 7.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// An interrupt request line: one of the levels a request can have, 1
/// to 7, as the IRQ\[7:1\] pins carry them (SIMRM 6.3).  Level 0 is no
/// request, so no pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct IrqPin(InterruptLevel);

/// A snapshot's pin, refused at 0 or above 7.
impl<'de> Deserialize<'de> for IrqPin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let level = u8::deserialize(deserializer)?;
        Self::new(level).ok_or_else(|| {
            D::Error::invalid_value(Unexpected::Unsigned(level.into()), &"an IRQ pin, 1 to 7")
        })
    }
}

impl IrqPin {
    /// IRQ7, nonmaskable.
    pub const IRQ7: Self = Self(InterruptLevel::NONMASKABLE);

    /// IRQ`level`'s pin, if there is one: 1 to 7.
    #[must_use]
    pub const fn new(level: u8) -> Option<Self> {
        match InterruptLevel::new(level) {
            Some(level) if level.get() != InterruptLevel::NONE.get() => Some(Self(level)),
            _ => None,
        }
    }

    /// The level it requests.
    #[must_use]
    pub const fn level(self) -> InterruptLevel {
        self.0
    }

    /// The level, 1 to 7.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }
}

impl From<IrqPin> for InterruptLevel {
    fn from(pin: IrqPin) -> Self {
        pin.0
    }
}

impl TryFrom<InterruptLevel> for IrqPin {
    type Error = NoIrqPin;

    fn try_from(level: InterruptLevel) -> Result<Self, NoIrqPin> {
        Self::new(level.get()).ok_or(NoIrqPin(level.get()))
    }
}

impl TryFrom<u8> for IrqPin {
    type Error = NoIrqPin;

    fn try_from(level: u8) -> Result<Self, NoIrqPin> {
        Self::new(level).ok_or(NoIrqPin(level))
    }
}

/// A level no IRQ pin carries: 0, or past 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no IRQ pin at level {0}")]
pub struct NoIrqPin(pub u8);

/// How an interrupt acknowledge cycle ended (CPU32RM 6.2.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Acknowledge {
    /// A vector number from the peripheral.
    Vector(u8),
    /// AVEC: the autovector for the level, 25 to 31.
    Autovector,
    /// A bus error: the spurious interrupt vector, 24.
    Spurious,
}

/// An interrupt acknowledge cycle: how it ended and the clocks it took
/// beyond the timing tables' two-clock bus cycle (CPU32RM 8.1.5), its
/// wait states, which the interrupt's cost does not include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Acknowledged {
    /// How it ended.
    pub answer: Acknowledge,
    /// Its clocks beyond two.
    pub stall: Cycles,
}

impl From<Acknowledge> for Acknowledged {
    /// An acknowledge with no wait states.
    fn from(answer: Acknowledge) -> Self {
        Self {
            answer,
            stall: Cycles::ZERO,
        }
    }
}

/// The CPU's interrupt inputs: the level on its IPL lines and the
/// acknowledge cycle that answers a request.  The chip implements it,
/// arbitrating among its modules and external sources (SIMRM 6).
pub trait Interrupts {
    /// The highest level requested now.
    fn level(&self) -> InterruptLevel;
    /// The acknowledge cycle for `level`, at `now`.
    fn acknowledge(&mut self, level: InterruptLevel, now: Cycles) -> Acknowledged;
}

#[cfg(test)]
mod tests {
    use super::InterruptLevel;
    use super::IrqPin;
    use super::NoIrqPin;

    #[test]
    fn a_snapshot_level_stops_at_7() {
        assert_eq!(
            postcard::from_bytes::<InterruptLevel>(&[7]),
            Ok(InterruptLevel::NONMASKABLE)
        );
        assert!(postcard::from_bytes::<InterruptLevel>(&[8]).is_err());
    }

    #[test]
    fn levels_stop_at_7() {
        assert_eq!(InterruptLevel::new(7), Some(InterruptLevel::NONMASKABLE));
        assert_eq!(InterruptLevel::new(8), None);
    }

    #[test]
    fn irq_pins_are_levels_1_to_7() {
        assert_eq!(IrqPin::new(0), None);
        assert_eq!(IrqPin::new(1).map(IrqPin::get), Some(1));
        assert_eq!(IrqPin::new(7), Some(IrqPin::IRQ7));
        assert_eq!(IrqPin::try_from(8), Err(NoIrqPin(8)));
        assert_eq!(IrqPin::try_from(InterruptLevel::NONE), Err(NoIrqPin(0)));
        assert_eq!(
            InterruptLevel::from(IrqPin::IRQ7),
            InterruptLevel::NONMASKABLE
        );
        assert_eq!(postcard::from_bytes::<IrqPin>(&[7]), Ok(IrqPin::IRQ7));
        assert!(postcard::from_bytes::<IrqPin>(&[0]).is_err());
    }
}
