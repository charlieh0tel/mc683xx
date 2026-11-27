//! Transfers in each SPI mode, for the QSPI peripherals' tests.

use mc683xx_board::peripheral::Transfer;
use mc683xx_board::spi::Mode;
use mc683xx_cpu32::Cycles;
use mc683xx_cpu32::Frequency;
use mc683xx_modules::peripheral::QspiClock;

/// A frame's first transfer in SPI mode `number`, of no bits
/// (`Transfer::shifting` gives it its word).
pub(crate) fn transfer(number: u8) -> Transfer {
    let mode = Mode::try_from(number).unwrap();
    let clock = QspiClock::new(
        mode.polarity,
        mode.phase,
        Cycles::from_clocks(8),
        Frequency::from_hz(16_777_216),
    );
    Transfer::new(0, 0, clock)
}
