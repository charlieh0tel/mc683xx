//! Transfers in each SPI mode, for the QSPI peripherals' tests.

use mc683xx_board::peripheral::Transfer;
use mc683xx_board::spi::Mode;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_modules::peripheral::QspiClock;

/// A frame's first transfer in SPI mode `number`, of no bits
/// (`Transfer::shifting` gives it its word).
pub(crate) fn transfer(number: u8) -> Transfer {
    let mode = Mode::numbered(number);
    let clock = QspiClock {
        polarity: mode.polarity,
        phase: mode.phase,
        period: Cycles(8),
        system_clock: Frequency::from_hz(16_777_216),
    };
    Transfer::new(0, 0, 0, 0, 0, clock, Time::ZERO)
}
