//! Which resets reach a host peripheral: the board's wiring of its reset
//! input.
//!
//! Every reset source asserts the chip's RESET pin (SIMRM 8.1, Table
//! 8-1), but a board need not wire a peripheral to it: a peripheral may see
//! only its own power-on reset, or RESET through logic that some resets
//! do not pass.

use crate::sim::ResetCause;

/// The reset causes a peripheral sees, a set of `ResetCause`s by their
/// RSR bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResetScope(u8);

impl ResetScope {
    /// No reset reaches it.
    pub const NONE: Self = Self(0);
    /// Every reset: a peripheral on the RESET pin, the default.
    pub const ALL: Self = Self::of(&ResetCause::ALL);

    /// The resets `causes`.
    #[must_use]
    pub const fn of(causes: &[ResetCause]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < causes.len() {
            bits |= causes[index].status();
            index += 1;
        }
        Self(bits)
    }

    /// These and `cause`.
    #[must_use]
    pub const fn with(self, cause: ResetCause) -> Self {
        Self(self.0 | cause.status())
    }

    /// These but `cause`.
    #[must_use]
    pub const fn without(self, cause: ResetCause) -> Self {
        Self(self.0 & !cause.status())
    }

    /// Whether `cause` reaches the peripheral.
    #[must_use]
    pub const fn contains(self, cause: ResetCause) -> bool {
        self.0 & cause.status() != 0
    }
}

impl Default for ResetScope {
    fn default() -> Self {
        Self::ALL
    }
}

#[cfg(test)]
mod tests {
    use crate::sim::ResetCause;

    use super::ResetScope;

    #[test]
    fn all_holds_every_cause_and_none_none() {
        for cause in ResetCause::ALL {
            assert!(ResetScope::ALL.contains(cause));
            assert!(!ResetScope::NONE.contains(cause));
        }
        let scope = ResetScope::ALL.without(ResetCause::Watchdog);
        assert!(!scope.contains(ResetCause::Watchdog));
        assert!(scope.contains(ResetCause::External));
        assert_eq!(scope.with(ResetCause::Watchdog), ResetScope::ALL);
        assert_eq!(
            ResetScope::of(&[ResetCause::PowerOn]),
            ResetScope::NONE.with(ResetCause::PowerOn)
        );
    }
}
