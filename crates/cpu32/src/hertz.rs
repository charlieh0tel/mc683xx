//! Serde for a whole-hertz frequency saved as its hertz, as a state
//! field's `#[serde(with = "mc683xx_cpu32::hertz")]`: the
//! chip's clocks, whose snapshots hold hertz.
//!
//! Not part of the semver API: serde plumbing for the workspace's snapshots.

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

use crate::frequency::Frequency;

/// The frequency's whole hertz.
///
/// # Errors
///
/// The serializer's.
pub fn serialize<S: Serializer>(frequency: &Frequency, serializer: S) -> Result<S::Ok, S::Error> {
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
