//! What a Python host can get wrong, before it becomes a Python
//! exception.

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::flat::MapError;
use mc683xx_cpu32::flat::Unmapped;
use mc683xx_cpu32::interrupt::NoIrqPin;
use mc683xx_cpu32::memory::MemoryError;
use mc683xx_mc68331::chip::BuildError;
use mc683xx_mc68331::host::AccessError;
use mc683xx_modules::sim::clock::ClockSourceError;

/// A host's mistake the bindings refuse.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A chip-select pin name that is none of `CSBOOT`, `CS0`-`CS10`.
    #[error("no chip select {0:?}")]
    Select(String),
    /// A GPT input pin name that is none of `PGP0`-`PGP7`, `PAI`,
    /// `PCLK`.
    #[error("no GPT input pin {0:?}")]
    Input(String),
    /// An SCI frame wider than nine data bits (QSMRM 5.2.4).
    #[error("{0:#x} does not fit an SCI frame")]
    Frame(u16),
    /// A QSPI slave shift of more than 16 bits (QSMRM Table 4-2), or of
    /// a word wider than its bits.
    #[error("{mosi:#x} in {bits} bits is no QSPI shift")]
    Shift {
        /// The word.
        mosi: u16,
        /// Its bits.
        bits: u8,
    },
    /// A register name that is not in the programming model.
    #[error("no register {0:?}")]
    Register(String),
    /// A value wider than the register it is written to.
    #[error("{value:#x} does not fit {register}")]
    Width {
        /// The register's name.
        register: &'static str,
        /// The value.
        value: u32,
    },
    /// An interrupt level that is not an IRQ pin's, 1 to 7.
    #[error(transparent)]
    Level(#[from] NoIrqPin),
    /// The clock given both as a reference and as an external clock.
    #[error("give reference_hz or external_hz, not both")]
    Clock,
    /// A chip clock not of whole hertz.
    #[error(transparent)]
    Frequency(#[from] ClockSourceError),
    /// A builder used after `build`.
    #[error("the chip is already built")]
    Built,
    /// A memory or peripheral number the builder never returned.
    #[error("no {kind} {index}")]
    Unknown {
        /// "memory" or "peripheral".
        kind: &'static str,
        /// The number.
        index: usize,
    },
    /// Bytes outside a memory.
    #[error("{length:#x} bytes at {offset:#x}: outside the memory's {size:#x}")]
    Range {
        /// Where they start.
        offset: usize,
        /// How many.
        length: usize,
        /// The memory's length.
        size: usize,
    },
    /// A host read of more bytes than 32 address lines reach.
    #[error("{0:#x} bytes: more than the address space")]
    Length(usize),
    /// A run whose end is past the clock's range.
    #[error("a budget of {0} clocks runs past the end of time")]
    Budget(u64),
    /// A host cycle's size in bytes that is not 1, 2 or 4.
    #[error("no {0}-byte cycle: give 1, 2 or 4")]
    CycleSize(u32),
    /// A port width in bits that is not 8 or 16.
    #[error("no {0}-bit port: give 8 or 16")]
    PortWidth(u8),
    /// A function code wider than FC[2:0].
    #[error("no function code {0}: FC[2:0] is 0 to 7")]
    FunctionCode(u8),
    /// Page permissions that are not `r`, `w` and `x`, each at most
    /// once.
    #[error("no permissions {0:?}: give r, w and x, each at most once")]
    Permissions(String),
    /// The chip refused the wiring.
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The core refused a range of pages.
    #[error(transparent)]
    Memory(#[from] MemoryError),
    /// A host cycle or peek gave no value.
    #[error(transparent)]
    Access(#[from] AccessError),
    /// The bare CPU refused a map or a peripheral.
    #[error(transparent)]
    Map(#[from] MapError),
    /// Host access to bytes no map covers.
    #[error(transparent)]
    Unmapped(#[from] Unmapped),
    /// The chip could not save, or refused a snapshot.
    #[error(transparent)]
    State(#[from] StateError),
}
