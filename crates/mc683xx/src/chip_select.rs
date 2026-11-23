//! The SIM's chip-select pins, which the host wires its peripherals to,
//! how a select ends a cycle and the DSACK pins a peripheral drives.

use std::fmt;
use std::str::FromStr;

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::Select;

use crate::sim::chip_selects::DSACK_FAST;

/// Clocks in a bus cycle with no wait states and fast termination
/// (SIMRM 7.5.1, 7.7; MC68331UM 4.5).
const ZERO_WAIT_CLOCKS: u8 = 3;
/// Clocks in a fast-termination cycle.
const FAST_CLOCKS: u8 = 2;

/// One of the SIM's 12 chip-select signals (SIMRM 7; MC68331UM 4.8).
/// Which address, space and cycle asserts each is the firmware's, in
/// CSPARx, CSBARx and CSORx; which peripheral it reaches is the board's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChipSelect {
    /// CSBOOT: decodes the whole space from reset, for the boot ROM
    /// (SIMRM 7.9.3).
    Boot,
    /// CS0 to CS10 (SIMRM 7.9.2).
    Numbered(Number),
}

impl ChipSelect {
    /// How many there are: CSBOOT and CS0 to CS10.
    pub const COUNT: usize = Number::LAST as usize + 2;

    /// Its index among all twelve: CSBOOT 0, CS`n` `n` + 1, the order
    /// of their registers (MC68331UM Table D-3).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Boot => 0,
            Self::Numbered(number) => number.0 as usize + 1,
        }
    }

    /// The select at `index`, if there is one.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Boot),
            _ if index < Self::COUNT => Some(Self::Numbered(Number((index - 1) as u8))),
            _ => None,
        }
    }
}

/// The select a cycle carries ([`BusCycle::select`]), if it is one of
/// these: the core numbers a select by its [`index`](ChipSelect::index).
///
/// [`BusCycle::select`]: mc683xx_cpu32::peripheral::BusCycle::select
impl TryFrom<Select> for ChipSelect {
    type Error = NoChipSelect;

    fn try_from(select: Select) -> Result<Self, NoChipSelect> {
        let index = u8::from(select);
        Self::from_index(usize::from(index)).ok_or(NoChipSelect(index))
    }
}

/// A [`Select`] whose index is none of the twelve chip selects'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no chip select at index {0}")]
pub struct NoChipSelect(pub u8);

/// The select as the core's cycles carry it, by its index.
impl From<ChipSelect> for Select {
    fn from(select: ChipSelect) -> Self {
        Self::from(select.index() as u8)
    }
}

/// Its pin's name in the manual: `CSBOOT`, `CS0` to `CS10`.
impl fmt::Display for ChipSelect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(NAMES[self.index()])
    }
}

/// The select whose pin is the text, in any case.
impl FromStr for ChipSelect {
    type Err = ParseChipSelectError;

    fn from_str(name: &str) -> Result<Self, ParseChipSelectError> {
        NAMES
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(name))
            .and_then(Self::from_index)
            .ok_or_else(|| ParseChipSelectError(name.to_owned()))
    }
}

/// Text that names no chip select: none of CSBOOT, CS0 to CS10.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no chip select {0:?}")]
pub struct ParseChipSelectError(String);

/// The pins' names, by [`ChipSelect::index`].
const NAMES: [&str; ChipSelect::COUNT] = [
    "CSBOOT", "CS0", "CS1", "CS2", "CS3", "CS4", "CS5", "CS6", "CS7", "CS8", "CS9", "CS10",
];

/// A numbered chip select, 0 to 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Number(u8);

impl Number {
    /// The highest, CS10.
    const LAST: u8 = 10;

    /// CS`number`, if there is one.
    const fn new(number: u8) -> Option<Self> {
        if number <= Self::LAST {
            Some(Self(number))
        } else {
            None
        }
    }
}

impl TryFrom<u8> for Number {
    type Error = NoChipSelectNumber;

    fn try_from(number: u8) -> Result<Self, NoChipSelectNumber> {
        Self::new(number).ok_or(NoChipSelectNumber(number))
    }
}

impl From<Number> for u8 {
    fn from(number: Number) -> Self {
        number.0
    }
}

/// A number no chip select has: past CS10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no chip select CS{0}")]
pub struct NoChipSelectNumber(pub u8);

/// How a select would end a cycle it matches (SIMRM 7.5, 7.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Termination {
    /// Internal DSACK after two clocks.
    Fast,
    /// Internal DSACK after this many wait states, 0 to 13.
    WaitStates(u8),
    /// The peripheral asserts DSACK.
    External,
    /// MODE set: the cycle runs on ECLK.
    Eclk,
}

impl Termination {
    /// The most wait states DSACK encodes: the field's values below
    /// fast termination's.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    pub const MOST_WAIT_STATES: u8 = DSACK_FAST as u8 - 1;

    /// The cycle's clocks, if the select ends it: ECLK cycles are
    /// charged an ECLK period and half again, the average wait to
    /// synchronize being half a period (our choice, [PLAN.md]).
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    #[doc(hidden)]
    #[must_use]
    pub const fn clocks(self, eclk_divisor: u64) -> Option<Cycles> {
        match self {
            Self::Fast => Some(Cycles::from_clocks(FAST_CLOCKS as u64)),
            Self::WaitStates(wait) => Some(Cycles::from_clocks((ZERO_WAIT_CLOCKS + wait) as u64)),
            Self::External => None,
            Self::Eclk => Some(Cycles::from_clocks(eclk_divisor + eclk_divisor / 2)),
        }
    }

    /// The clocks of a cycle an external DSACK ends after `wait`
    /// states.
    ///
    /// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
    #[doc(hidden)]
    #[must_use]
    pub const fn external_clocks(wait: Cycles) -> Cycles {
        Cycles::from_clocks(ZERO_WAIT_CLOCKS as u64).saturating_add(wait)
    }
}

/// The DSACK pins a peripheral drives on an externally terminated select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dsack {
    /// DSACK0: an 8-bit port (SIMRM Table 5-2).
    Dsack0,
    /// DSACK1: a 16-bit port.
    Dsack1,
    /// Both: reserved, taken as a 16-bit port.
    Both,
}

impl Dsack {
    /// Whether it drives DSACK0.
    #[must_use]
    pub const fn drives_dsack0(self) -> bool {
        matches!(self, Self::Dsack0 | Self::Both)
    }

    /// Whether it drives DSACK1.
    #[must_use]
    pub const fn drives_dsack1(self) -> bool {
        matches!(self, Self::Dsack1 | Self::Both)
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::peripheral::Select;

    use super::ChipSelect;
    use super::NoChipSelectNumber;
    use super::Number;

    #[test]
    fn eleven_numbered_selects() {
        assert_eq!(Number::try_from(10).map(u8::from), Ok(10));
        assert_eq!(Number::try_from(11), Err(NoChipSelectNumber(11)));
    }

    #[test]
    fn names_go_both_ways_in_any_case() {
        for index in 0..ChipSelect::COUNT {
            let select = ChipSelect::from_index(index).unwrap();
            assert_eq!(select.to_string().parse(), Ok(select));
        }
        assert_eq!("csboot".parse(), Ok(ChipSelect::Boot));
        assert_eq!(
            "CS11".parse::<ChipSelect>(),
            Err(super::ParseChipSelectError("CS11".to_owned()))
        );
    }

    #[test]
    fn a_select_is_a_chip_select_by_its_index() {
        assert_eq!(
            ChipSelect::try_from(Select::from(ChipSelect::Boot)),
            Ok(ChipSelect::Boot)
        );
        assert_eq!(
            ChipSelect::try_from(Select::from(12)),
            Err(super::NoChipSelect(12))
        );
    }

    #[test]
    fn indices_go_both_ways() {
        for index in 0..ChipSelect::COUNT {
            assert_eq!(ChipSelect::from_index(index).unwrap().index(), index);
        }
        assert_eq!(ChipSelect::from_index(ChipSelect::COUNT), None);
    }
}
