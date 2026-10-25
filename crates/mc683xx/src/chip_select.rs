//! The SIM's chip-select pins, which the host wires its peripherals to.

use std::fmt;
use std::str::FromStr;

use mc683xx_cpu32::peripheral::Select;

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
            Self::Numbered(number) => number.get() as usize + 1,
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

    /// The select a cycle carries ([`BusCycle::select`]), if it is one of
    /// these: the core numbers a select by its [`index`](Self::index).
    ///
    /// [`BusCycle::select`]: mc683xx_cpu32::peripheral::BusCycle::select
    #[must_use]
    pub fn from_select(select: Select) -> Option<Self> {
        Self::from_index(usize::from(u8::from(select)))
    }
}

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
pub struct ParseChipSelectError(pub String);

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
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number <= Self::LAST {
            Some(Self(number))
        } else {
            None
        }
    }

    /// Its number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::ChipSelect;
    use super::Number;

    #[test]
    fn eleven_numbered_selects() {
        assert!(Number::new(10).is_some());
        assert!(Number::new(11).is_none());
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
    fn indices_go_both_ways() {
        for index in 0..ChipSelect::COUNT {
            assert_eq!(ChipSelect::from_index(index).unwrap().index(), index);
        }
        assert_eq!(ChipSelect::from_index(ChipSelect::COUNT), None);
    }
}
