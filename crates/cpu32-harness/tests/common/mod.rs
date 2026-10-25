//! The Z3805A's wiring around the chip, shared by the whole-image boot
//! tests (github.com/charlieh0tel/smartclock,
//! `docs/firmware/restart.md`, "The chip selects"): the flash read on
//! CSBOOT and CS1 and written on CS6 and CS7, the RAM read on CS0 and
//! written a byte lane each on CS2 and CS3, and every other select on
//! one host peripheral.

use std::any::Any;

use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::MemoryId;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::peripheral::Timed;

/// The variable naming the firmware directory, smartclock's
/// `third_party/firmware`.
pub(crate) const FIRMWARE: &str = "SMARTCLOCK_FIRMWARE";
/// Bytes of flash: four 128 K x 8 parts (restart.md).
const FLASH_BYTES: usize = 0x8_0000;
/// Bytes of RAM the selects reach (restart.md, "The RAM").
const RAM_BYTES: usize = 0x1_0000;
/// The pins the host peripheral stands on: every select but the flash's
/// reads and the RAM's.
const PERIPHERAL_PINS: [u8; 7] = [4, 5, 6, 7, 8, 9, 10];

/// CS`number`.
fn select(number: u8) -> ChipSelect {
    ChipSelect::Numbered(Number::new(number).expect("CS0 to CS10"))
}

/// The board wired as the Z3805A's, with `image` in the flash and
/// `peripheral` on `PERIPHERAL_PINS`, before its power-on reset; with the
/// peripheral's and the RAM's handles.
pub(crate) fn board<D: MappedPeripheral + Timed + Save + Any>(
    image: Vec<u8>,
    peripheral: D,
) -> (Builder, PeripheralId, MemoryId) {
    let mut flash = image;
    flash.resize(FLASH_BYTES, 0xff);
    let mut builder = Builder::default();
    builder
        .memory(flash, false, &[ChipSelect::Boot, select(1)])
        .expect("flash");
    let ram = builder
        .memory(vec![0; RAM_BYTES], true, &[select(0), select(2), select(3)])
        .expect("RAM");
    let pins: Vec<ChipSelect> = PERIPHERAL_PINS
        .iter()
        .map(|&number| select(number))
        .collect();
    let peripheral = builder.peripheral(peripheral, &pins).expect("peripheral");
    (builder, peripheral, ram)
}
