//! Frequencies: the chip's clocks and the board's.

use std::fmt;

use serde::Deserialize;
use serde::Serialize;

/// Nanohertz in a hertz.
pub const NANOHERTZ_PER_HERTZ: u64 = 1_000_000_000;

/// A frequency in nanohertz: 10^-16 of a 10 MHz oscillator, finer than
/// any oven holds it or EFC step moves it, up to 18 GHz.  The chip's
/// clocks are whole hertz (`from_hz`, `hz`).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Frequency(pub u64);

impl Frequency {
    /// `hz` hertz.
    #[must_use]
    pub const fn from_hz(hz: u64) -> Self {
        Self(hz.saturating_mul(NANOHERTZ_PER_HERTZ))
    }

    /// Whole hertz, rounded down.
    #[must_use]
    pub const fn hz(self) -> u64 {
        self.0 / NANOHERTZ_PER_HERTZ
    }
}

impl From<Frequency> for u64 {
    /// Its nanohertz.
    fn from(frequency: Frequency) -> Self {
        frequency.0
    }
}

/// Serde for a whole-hertz frequency saved as its hertz, as a state
/// field's `#[serde(with = "mc683xx_cpu32::frequency::hertz")]`: the
/// chip's clocks, whose snapshots hold hertz.
///
/// Not part of the semver API: serde plumbing for the workspace's snapshots.
#[doc(hidden)]
pub mod hertz {
    use serde::Deserialize;
    use serde::Deserializer;
    use serde::Serialize;
    use serde::Serializer;

    use super::Frequency;

    /// The frequency's whole hertz.
    ///
    /// # Errors
    ///
    /// The serializer's.
    pub fn serialize<S: Serializer>(
        frequency: &Frequency,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        frequency.hz().serialize(serializer)
    }

    /// A frequency of whole hertz.
    ///
    /// # Errors
    ///
    /// The deserializer's.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Frequency, D::Error> {
        u64::deserialize(deserializer).map(Frequency::from_hz)
    }
}

/// Hertz and their unit, the nanohertz as a fraction if there are any:
/// `16777216 Hz`, `3.000000001 Hz`.
impl fmt::Display for Frequency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fraction = self.0 % NANOHERTZ_PER_HERTZ;
        if fraction == 0 {
            write!(formatter, "{} Hz", self.hz())
        } else {
            let digits = format!("{fraction:09}");
            write!(
                formatter,
                "{}.{} Hz",
                self.hz(),
                digits.trim_end_matches('0')
            )
        }
    }
}

impl From<u64> for Frequency {
    /// `nanohertz` nanohertz.
    fn from(nanohertz: u64) -> Self {
        Self(nanohertz)
    }
}

#[cfg(test)]
mod tests {
    use super::Frequency;

    #[test]
    fn hertz_are_whole_and_saved_as_hertz() {
        let frequency = Frequency::from_hz(16_777_216);
        assert_eq!(frequency.hz(), 16_777_216);
        assert_eq!(Frequency(1_999_999_999).hz(), 1, "rounded down");

        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Saved(#[serde(with = "super::hertz")] Frequency);
        let bytes = postcard::to_allocvec(&Saved(frequency)).unwrap();
        assert_eq!(bytes, postcard::to_allocvec(&16_777_216_u64).unwrap());
        assert_eq!(postcard::from_bytes::<Saved>(&bytes), Ok(Saved(frequency)));
    }

    #[test]
    fn a_frequency_shows_in_hertz() {
        assert_eq!(Frequency::from_hz(16_777_216).to_string(), "16777216 Hz");
        assert_eq!(Frequency(3_000_000_001).to_string(), "3.000000001 Hz");
        assert_eq!(Frequency(2_500_000_000).to_string(), "2.5 Hz");
        assert_eq!(Frequency::from(5), Frequency(5));
    }
}
