//! Conditions that stop a board's run, for a debugger: an emulator's
//! alone, as silicon has no such watch ([docs/gdb.md, Stop
//! conditions][gdb-conditions]).
//!
//! A condition watches a net ([`Condition::Net`]) or is one a
//! peripheral offers ([`Peripheral::conditions`]).  Armed
//! ([`Conditions::arm`]), it stays armed until disarmed; each time it
//! holds, the run stops after the instruction running, as a
//! peripheral's [`Drive::stop`] stops it, and [`Conditions::fired`]
//! names it.  Armed conditions are outside the snapshot: a restore
//! keeps them armed.
//!
//! [gdb-conditions]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md#stop-conditions
//! [`Peripheral::conditions`]: crate::peripheral::Peripheral::conditions
//! [`Drive::stop`]: crate::peripheral::Drive::stop

use std::fmt;
use std::rc::Weak;
use std::str::FromStr;

use crate::error::ConditionError;
use crate::observe::Observer;
use crate::signal::Signal;
use crate::time::Time;
use crate::wiring::Shared;

/// A condition a peripheral offers, as `monitor stop conditions` lists
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionInfo {
    /// Its name, the word that arms it.
    pub name: &'static str,
    /// Its arguments, as a usage line shows them (`CHANNEL [BYTE]`).
    pub arguments: &'static str,
    /// What it stops for.
    pub help: &'static str,
}

/// A peripheral's own handle on a condition it armed
/// ([`Peripheral::arm`]), given back to fire and disarm it.
///
/// [`Peripheral::arm`]: crate::peripheral::Peripheral::arm
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConditionToken(pub u32);

impl From<u32> for ConditionToken {
    fn from(token: u32) -> Self {
        Self(token)
    }
}

impl From<ConditionToken> for u32 {
    fn from(token: ConditionToken) -> Self {
        token.0
    }
}

/// Why a peripheral would not arm a condition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ArmError {
    /// It offers no condition of that name.
    #[error("no condition {0:?}")]
    Unknown(String),
    /// The arguments are not the condition's: why.
    #[error("{0}")]
    Arguments(String),
}

/// What on a net a condition stops for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NetChange {
    /// A logic net going high from low (or from no value yet).
    Rise,
    /// A logic net going low from high (or from no value yet).
    Fall,
    /// Any change: a logic net's level that differs from the last, or
    /// any other net's every change.
    Change,
    /// A logic net driven high, whatever it was.
    High,
    /// A logic net driven low, whatever it was.
    Low,
}

impl NetChange {
    /// Every change, in the order `monitor stop conditions` lists them.
    pub const ALL: [Self; 5] = [Self::Rise, Self::Fall, Self::Change, Self::High, Self::Low];

    /// Its word.
    const fn name(self) -> &'static str {
        match self {
            Self::Rise => "rise",
            Self::Fall => "fall",
            Self::Change => "change",
            Self::High => "high",
            Self::Low => "low",
        }
    }

    /// Whether only a logic net, one of levels, has it.
    #[must_use]
    pub const fn levels_only(self) -> bool {
        !matches!(self, Self::Change)
    }

    /// Whether the net, `last` its level before (if it had one and it
    /// is a level), holds the condition as it carries `signal`.
    fn holds(self, last: Option<bool>, signal: Signal) -> bool {
        let Signal::Level(high) = signal else {
            return self == Self::Change;
        };
        match self {
            Self::Rise => high && last != Some(true),
            Self::Fall => !high && last != Some(false),
            Self::Change => last != Some(high),
            Self::High => high,
            Self::Low => !high,
        }
    }
}

/// Its word: `rise`, `fall`, `change`, `high`, `low`.
impl fmt::Display for NetChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl FromStr for NetChange {
    type Err = ParseNetChangeError;

    fn from_str(name: &str) -> Result<Self, ParseNetChangeError> {
        Self::ALL
            .into_iter()
            .find(|change| change.name() == name)
            .ok_or_else(|| ParseNetChangeError(name.to_owned()))
    }
}

/// Text that names no net change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no net change {0:?}; one of rise, fall, change, high, low")]
pub struct ParseNetChangeError(pub String);

/// What a condition stops for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Condition {
    /// A net's change.
    Net {
        /// The net.
        net: String,
        /// What on it.
        change: NetChange,
    },
    /// One of a peripheral's own conditions.
    Peripheral {
        /// The peripheral, by its name in the board file.
        peripheral: String,
        /// The condition's name ([`ConditionInfo::name`]).
        condition: String,
        /// Its arguments, as words.
        arguments: Vec<String>,
    },
}

/// The words that arm it: `net RESET fall`, `peripheral duart tx a
/// 0x0d`.
impl fmt::Display for Condition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net { net, change } => write!(formatter, "{NET} {net} {change}"),
            Self::Peripheral {
                peripheral,
                condition,
                arguments,
            } => {
                write!(formatter, "{PERIPHERAL} {peripheral} {condition}")?;
                arguments
                    .iter()
                    .try_for_each(|argument| write!(formatter, " {argument}"))
            }
        }
    }
}

/// The word of a net's condition.
const NET: &str = "net";
/// The word of a peripheral's condition.
const PERIPHERAL: &str = "peripheral";

/// The condition its words name, separated by white space.
impl FromStr for Condition {
    type Err = ParseConditionError;

    fn from_str(text: &str) -> Result<Self, ParseConditionError> {
        let words: Vec<&str> = text.split_whitespace().collect();
        match words.as_slice() {
            [NET, net, change] => Ok(Self::Net {
                net: (*net).to_owned(),
                change: change.parse()?,
            }),
            [NET, ..] => Err(ParseConditionError::Net),
            [PERIPHERAL, peripheral, condition, arguments @ ..] => Ok(Self::Peripheral {
                peripheral: (*peripheral).to_owned(),
                condition: (*condition).to_owned(),
                arguments: arguments.iter().map(|&word| word.to_owned()).collect(),
            }),
            [PERIPHERAL, ..] => Err(ParseConditionError::Peripheral),
            [source, ..] => Err(ParseConditionError::Source((*source).to_owned())),
            [] => Err(ParseConditionError::Empty),
        }
    }
}

/// Text that names no condition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseConditionError {
    /// Nothing.
    #[error("no condition: `net NET CHANGE` or `peripheral NAME CONDITION [ARGUMENTS]`")]
    Empty,
    /// A first word neither `net` nor `peripheral`.
    #[error("no condition source {0:?}: `net` or `peripheral`")]
    Source(String),
    /// A net's condition of other than three words.
    #[error("a net's condition is `net NET CHANGE`")]
    Net,
    /// A peripheral's condition without its name.
    #[error("a peripheral's condition is `peripheral NAME CONDITION [ARGUMENTS]`")]
    Peripheral,
    /// A net's change no change names.
    #[error(transparent)]
    Change(#[from] ParseNetChangeError),
}

/// An armed condition's number, from 1, never reused on its board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConditionId(pub u64);

impl From<u64> for ConditionId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<ConditionId> for u64 {
    fn from(id: ConditionId) -> Self {
        id.0
    }
}

/// Its number.
impl fmt::Display for ConditionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// What a board with conditions does for a debugger: arms and disarms
/// them, and says which fired.  A [`Board`] is one.
///
/// [`Board`]: crate::board::Board
pub trait Conditions {
    /// Arms `condition`: its number.
    ///
    /// # Errors
    ///
    /// [`ConditionError`] for a net or peripheral the board has none
    /// of, a level's change on a net of another kind, or a condition
    /// the peripheral refuses.
    fn arm(&mut self, condition: Condition) -> Result<ConditionId, ConditionError>;

    /// Disarms condition `id`; whether it was armed.
    fn disarm(&mut self, id: ConditionId) -> bool;

    /// The armed conditions, in the order armed.
    fn armed(&self) -> Vec<(ConditionId, Condition)>;

    /// The conditions that fired in the last run, in the order they
    /// fired, each once: a run that stopped for one names it here until
    /// the next run begins, disarmed since or not.
    fn fired(&self) -> Vec<ConditionId>;

    /// The peripherals that offer conditions, in file order: each one's
    /// name and conditions.
    fn offered(&self) -> Vec<(String, &'static [ConditionInfo])>;
}

/// A condition that held, as the board records it until it names the
/// condition ([`Conditions::fired`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Firing {
    /// A peripheral's, by its index and token, while it is armed.
    Peripheral(usize, ConditionToken),
    /// By its id: a net's, or a peripheral's disarmed since it held, so
    /// a token the peripheral gives out again does not take it.
    Named(ConditionId),
}

/// A net's condition, as the observer on the net that watches it.
pub(crate) struct NetWatch {
    /// The condition.
    pub(crate) id: ConditionId,
    /// What on the net.
    pub(crate) change: NetChange,
    /// The net's level as last carried, if it has one and it is a
    /// level.
    pub(crate) last: Option<bool>,
    /// Where it records a firing and asks the stop.
    pub(crate) shared: Weak<Shared>,
}

/// The level a value is, if it is one.
pub(crate) const fn level(signal: Option<Signal>) -> Option<bool> {
    match signal {
        Some(Signal::Level(high)) => Some(high),
        _ => None,
    }
}

impl Observer for NetWatch {
    fn changed(&mut self, _net: &str, _at: Time, signal: Signal) {
        let holds = self.change.holds(self.last, signal);
        self.last = level(Some(signal));
        if holds && let Some(shared) = self.shared.upgrade() {
            shared.fired.borrow_mut().push(Firing::Named(self.id));
            shared.request_stop();
        }
    }

    fn restored(&mut self, _net: &str, _at: Time, signal: Option<Signal>) {
        self.last = level(signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_round_trip_through_their_words() {
        for text in [
            "net RESET fall",
            "peripheral duart tx a 0x0d",
            "peripheral duart rx b",
        ] {
            let condition: Condition = text.parse().unwrap();
            assert_eq!(condition.to_string(), text);
        }
    }

    #[test]
    fn malformed_conditions_are_refused() {
        assert_eq!("".parse::<Condition>(), Err(ParseConditionError::Empty));
        assert_eq!(
            "pin X high".parse::<Condition>(),
            Err(ParseConditionError::Source("pin".to_owned()))
        );
        assert_eq!(
            "net RESET".parse::<Condition>(),
            Err(ParseConditionError::Net)
        );
        assert_eq!(
            "peripheral duart".parse::<Condition>(),
            Err(ParseConditionError::Peripheral)
        );
        assert_eq!(
            "net RESET up".parse::<Condition>(),
            Err(ParseNetChangeError("up".to_owned()).into())
        );
    }

    #[test]
    fn levels_hold_as_their_changes_say() {
        let high = Signal::Level(true);
        let low = Signal::Level(false);
        assert!(NetChange::Rise.holds(Some(false), high));
        assert!(NetChange::Rise.holds(None, high));
        assert!(!NetChange::Rise.holds(Some(true), high));
        assert!(NetChange::Fall.holds(Some(true), low));
        assert!(!NetChange::Fall.holds(Some(true), high));
        assert!(NetChange::Change.holds(Some(true), low));
        assert!(!NetChange::Change.holds(Some(true), high));
        assert!(NetChange::High.holds(Some(true), high));
        assert!(NetChange::Low.holds(Some(false), low));
        assert!(NetChange::Change.holds(None, Signal::Bus(3)));
        assert!(!NetChange::High.holds(None, Signal::Bus(3)));
    }
}
