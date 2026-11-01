//! Where smartclock's `third_party/firmware`, which `SMARTCLOCK_FIRMWARE`
//! names, keeps the images, their EEPROM dumps and their decompiled
//! sources (github.com/charlieh0tel/smartclock,
//! `third_party/firmware/README.md`).

use std::path::Path;
use std::path::PathBuf;

/// The variable naming smartclock's `third_party/firmware`.
pub const FIRMWARE: &str = "SMARTCLOCK_FIRMWARE";

/// The directory holding `<image>.bin` and `eeprom/`.
#[must_use]
pub fn images(firmware: &Path) -> PathBuf {
    firmware.join("images")
}

/// The directory holding `image`'s `<image>.c` and
/// `<image>.conventions.jsonl`.
#[must_use]
pub fn decompiled(firmware: &Path, image: &str) -> PathBuf {
    firmware.join("decompiled").join(image)
}
