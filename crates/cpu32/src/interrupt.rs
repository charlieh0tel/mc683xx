//! Interrupt requests and their acknowledgment.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde::de::Unexpected;

use crate::cycles::Cycle;

use crate::cycles::Cycles;
use crate::exception::Vector;

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
    const fn new(level: u8) -> Option<Self> {
        if level <= Self::NONMASKABLE.0 {
            Some(Self(level))
        } else {
            None
        }
    }

    /// The level, 0 to 7, where `u8::from` cannot be called: a const
    /// context.
    ///
    /// Not part of the semver API: for the modules' and the chip's
    /// const fns; a host converts with `u8::from`.
    #[doc(hidden)]
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The level in the low three bits of `value`, as SR's I\[2:0\]
    /// holds one; the rest are ignored.
    pub(crate) const fn from_low_bits(value: u32) -> Self {
        Self(value as u8 & Self::NONMASKABLE.0)
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
    const fn new(level: u8) -> Option<Self> {
        match InterruptLevel::new(level) {
            Some(level) if level.0 != InterruptLevel::NONE.0 => Some(Self(level)),
            _ => None,
        }
    }

    /// The level, 1 to 7, where `u8::from` cannot be called: a const
    /// context.
    ///
    /// Not part of the semver API: for the modules' and the chip's
    /// const fns; a host converts with `u8::from`.
    #[doc(hidden)]
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.0
    }
}

/// The prefix of an interrupt level's name, `level 0` to `level 7`, as
/// CPU32RM Table 6-1 names them (`Level 7 Interrupt Autovector`).
const LEVEL: &str = "level ";
/// The prefix of an IRQ pin's name, `IRQ1` to `IRQ7` (SIMRM 6.3).
const IRQ: &str = "IRQ";

/// The number after `prefix` in `text`, `prefix` in any case, if it is
/// one digit.
fn digit_after(text: &str, prefix: &str) -> Option<u8> {
    let digit = text
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .and_then(|_| text.get(prefix.len()..))?;
    match digit.as_bytes() {
        [digit @ b'0'..=b'9'] => Some(digit - b'0'),
        _ => None,
    }
}

/// Its name: `level 7`.
impl fmt::Display for InterruptLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{LEVEL}{}", self.0)
    }
}

/// The level named so, as `Display` writes it, in any case: `level 7`,
/// `Level 7`.
impl FromStr for InterruptLevel {
    type Err = ParseInterruptLevelError;

    fn from_str(name: &str) -> Result<Self, ParseInterruptLevelError> {
        digit_after(name, LEVEL)
            .and_then(Self::new)
            .ok_or_else(|| ParseInterruptLevelError(name.to_owned()))
    }
}

/// Text that names no interrupt level.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no interrupt level {0:?}")]
pub struct ParseInterruptLevelError(String);

/// Its pin's name: `IRQ7`.
impl fmt::Display for IrqPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{IRQ}{}", self.get())
    }
}

/// The pin named so, as `Display` writes it, in any case: `IRQ7`,
/// `irq7`.
impl FromStr for IrqPin {
    type Err = ParseIrqPinError;

    fn from_str(name: &str) -> Result<Self, ParseIrqPinError> {
        digit_after(name, IRQ)
            .and_then(Self::new)
            .ok_or_else(|| ParseIrqPinError(name.to_owned()))
    }
}

/// Text that names no IRQ pin.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no IRQ pin {0:?}")]
pub struct ParseIrqPinError(String);

impl TryFrom<u8> for InterruptLevel {
    type Error = NoInterruptLevel;

    fn try_from(level: u8) -> Result<Self, NoInterruptLevel> {
        Self::new(level).ok_or(NoInterruptLevel(level))
    }
}

impl From<InterruptLevel> for u8 {
    fn from(level: InterruptLevel) -> Self {
        level.0
    }
}

/// A value past the levels: above 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no interrupt level {0}")]
pub struct NoInterruptLevel(pub u8);

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

impl From<IrqPin> for u8 {
    fn from(pin: IrqPin) -> Self {
        pin.get()
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
    Vector(Vector),
    /// AVEC: the autovector for the level, 25 to 31.
    Autovector,
    /// A bus error: the spurious interrupt vector, 24.
    Spurious,
}

/// An interrupt acknowledge cycle: how it ended and the clocks it took
/// beyond the timing tables' two-clock bus cycle (CPU32RM 8.1.5), its
/// wait states, which the interrupt's cost does not include.
///
/// Built with [`Acknowledged::new`] (or from an [`Acknowledge`]), so a
/// field added later gets a default and no host's code changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Acknowledged {
    /// How it ended.
    pub answer: Acknowledge,
    /// Its clocks beyond two.
    pub stall: Cycles,
}

impl Acknowledged {
    /// An acknowledge that ended as `answer`, with no wait states.
    #[must_use]
    pub const fn new(answer: Acknowledge) -> Self {
        Self {
            answer,
            stall: Cycles::ZERO,
        }
    }

    /// This acknowledge, taking `stall` clocks beyond two.
    #[must_use]
    pub const fn stalled(self, stall: Cycles) -> Self {
        Self { stall, ..self }
    }
}

impl From<Acknowledge> for Acknowledged {
    /// An acknowledge with no wait states.
    fn from(answer: Acknowledge) -> Self {
        Self::new(answer)
    }
}

/// The CPU's interrupt inputs: the level on its IPL lines and the
/// acknowledge cycle that answers a request.  The chip implements it,
/// arbitrating among its modules and external sources (SIMRM 6).
pub trait Interrupts {
    /// The highest level requested now.
    fn level(&self) -> InterruptLevel;
    /// The acknowledge cycle for `level`, at `now`.
    fn acknowledge(&mut self, level: InterruptLevel, now: Cycle) -> Acknowledged;
    /// The interrupt just acknowledged at `level` was not taken: a
    /// fault stopped its exception processing
    /// ([`Cpu::set_stop_on_fault`]), and the next run samples the
    /// request again.  A source whose acknowledge cleared its request
    /// asks again, so the stop leaves it as it was; nothing, by default.
    ///
    /// [`Cpu::set_stop_on_fault`]: crate::cpu::Cpu::set_stop_on_fault
    fn withdrawn(&mut self, _level: InterruptLevel) {}
}

#[cfg(test)]
mod tests {
    use super::InterruptLevel;
    use super::IrqPin;
    use super::NoInterruptLevel;
    use super::NoIrqPin;
    use super::ParseInterruptLevelError;
    use super::ParseIrqPinError;

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
        assert_eq!(InterruptLevel::try_from(7), Ok(InterruptLevel::NONMASKABLE));
        assert_eq!(InterruptLevel::try_from(8), Err(NoInterruptLevel(8)));
        assert_eq!(u8::from(InterruptLevel::NONMASKABLE), 7);
    }

    #[test]
    fn irq_pins_are_levels_1_to_7() {
        assert_eq!(IrqPin::new(0), None);
        assert_eq!(IrqPin::new(1).map(IrqPin::get), Some(1));
        assert_eq!(IrqPin::new(7), Some(IrqPin::IRQ7));
        assert_eq!(IrqPin::try_from(8), Err(NoIrqPin(8)));
        assert_eq!(u8::from(IrqPin::IRQ7), 7);
        assert_eq!(IrqPin::try_from(InterruptLevel::NONE), Err(NoIrqPin(0)));
        assert_eq!(
            InterruptLevel::from(IrqPin::IRQ7),
            InterruptLevel::NONMASKABLE
        );
        assert_eq!(postcard::from_bytes::<IrqPin>(&[7]), Ok(IrqPin::IRQ7));
        assert!(postcard::from_bytes::<IrqPin>(&[0]).is_err());
    }

    #[test]
    fn a_level_is_named_as_the_manual_names_it() {
        assert_eq!(InterruptLevel::NONMASKABLE.to_string(), "level 7");
        for level in 0..=7 {
            let level = InterruptLevel::new(level).expect("a level");
            assert_eq!(level.to_string().parse(), Ok(level));
        }
        assert_eq!("LEVEL 3".parse(), Ok(InterruptLevel::from_low_bits(3)));
        for text in ["level 8", "level", "level 10", "level  1", "7", "levels 1"] {
            assert_eq!(
                text.parse::<InterruptLevel>(),
                Err(ParseInterruptLevelError(text.to_owned()))
            );
        }
    }

    #[test]
    fn an_irq_pin_is_named_as_the_manual_names_it() {
        assert_eq!(IrqPin::IRQ7.to_string(), "IRQ7");
        for level in 1..=7 {
            let pin = IrqPin::new(level).expect("a pin");
            assert_eq!(pin.to_string().parse(), Ok(pin));
        }
        assert_eq!("irq7".parse(), Ok(IrqPin::IRQ7));
        for text in ["IRQ0", "IRQ8", "IRQ", "IRQ17", "7", "IRQ 7"] {
            assert_eq!(
                text.parse::<IrqPin>(),
                Err(ParseIrqPinError(text.to_owned()))
            );
        }
    }
}
