//! What a Python host can get wrong, before it becomes a Python
//! exception.

use mc683xx_board::error::LoadError;
use mc683xx_board::error::RunError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::flat::MapError;
use mc683xx_cpu32::interrupt::NoIrqPin;
use mc683xx_cpu32::memory::MemoryError;
use mc683xx_cpu32::memory::Unserved;
use mc683xx_mc68331::chip::BuildError;
use mc683xx_mc68331::debug::TargetError;
use mc683xx_mc68331::host::AccessError;
use mc683xx_modules::sim::clock::ClockSourceError;

/// A host's mistake the bindings refuse.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A chip-select pin name that is none of `CSBOOT`, `CS0`-`CS10`.
    #[error("no chip select {0:?}")]
    Select(String),
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
        register: String,
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
    /// `ResetCause.UNKNOWN` given as a reset to make or reach.
    #[error("ResetCause.UNKNOWN is no reset to ask for")]
    UnknownResetCause,
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
    /// Host access to bytes that are not memory the core serves.
    #[error(transparent)]
    Unserved(#[from] Unserved),
    /// The chip could not save, or refused a snapshot.
    #[error(transparent)]
    State(#[from] StateError),
    /// The chip refused a debugger's call.
    #[error(transparent)]
    Target(#[from] TargetError),
    /// A board file refused, or not read.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// A board's run that could not go on.
    #[error(transparent)]
    Run(#[from] RunError),
    /// A value the bindings have no Python value for, by its kind and,
    /// where it prints, its Rust name: a variant of a `non_exhaustive`
    /// enum newer than they are, or a run's end with no Python class
    /// (PLAN.md, Python).  A run's end ([`RUN_END`]) raises `RunError`,
    /// any other `NotImplementedError`.
    #[error("a {kind}{} the bindings do not know", named(name.as_deref()))]
    Unmapped {
        /// What it is: `reset cause`, [`RUN_END`].
        kind: &'static str,
        /// Its Rust name, if it has a printed form.
        name: Option<String>,
    },
}

/// A run's end as an [`Error::Unmapped`] names its kind.
pub(crate) const RUN_END: &str = "run end";

impl Error {
    /// A value of `kind` with no Python value, unnamed.
    pub(crate) const fn unmapped(kind: &'static str) -> Self {
        Self::Unmapped { kind, name: None }
    }

    /// A value of `kind` with no Python value, `name` its Rust name.
    pub(crate) fn named(kind: &'static str, name: impl ToString) -> Self {
        Self::Unmapped {
            kind,
            name: Some(name.to_string()),
        }
    }

    /// A run's end with no Python class, `name` its Rust name.
    pub(crate) fn run_end(name: impl ToString) -> Self {
        Self::named(RUN_END, name)
    }
}

/// ` (name)`, or nothing for no name.
fn named(name: Option<&str>) -> String {
    name.map(|name| format!(" ({name})")).unwrap_or_default()
}
