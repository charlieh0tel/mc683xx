//! What a Python host can get wrong, before it becomes a Python
//! exception.

use std::time::TryFromFloatSecsError;

use mc683xx_board::board::NetError;
use mc683xx_board::board::ReceiveError;
use mc683xx_board::board::RunError;
use mc683xx_board::board::SendError;
use mc683xx_board::description::LoadError;
use mc683xx_board::pacing::PacingError;
use mc683xx_board::pacing::ParsePacingError;
use mc683xx_board::registry::RegistryError;
use mc683xx_board::signal::ParseKindError;
use mc683xx_cpu32::cpu::bare::TakeExceptionError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::flat::MapError;
use mc683xx_cpu32::interrupt::NoInterruptLevel;
use mc683xx_cpu32::interrupt::NoIrqPin;
use mc683xx_cpu32::memory::MemoryError;
use mc683xx_cpu32::memory::Unserved;
use mc683xx_mc68331::chip::BuildError;
use mc683xx_mc68331::debug::TargetError;
use mc683xx_mc68331::host::BusCycleError;
use mc683xx_mc68331::port::NoInputsError;
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
    /// An interrupt mask that is not a level, 0 to 7.
    #[error(transparent)]
    Mask(#[from] NoInterruptLevel),
    /// An exception the CPU could not take for the host.
    #[error(transparent)]
    Take(#[from] TakeExceptionError),
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
    Access(#[from] BusCycleError),
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
    /// A peripheral type the registry refused.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// A value a run-time endpoint could not drive.
    #[error(transparent)]
    Send(#[from] SendError),
    /// A run-time endpoint no net drives.
    #[error(transparent)]
    Receive(#[from] ReceiveError),
    /// A net the board does not have.
    #[error(transparent)]
    Net(#[from] NetError),
    /// A net type that is none of a board file's.
    #[error(transparent)]
    Kind(#[from] ParseKindError),
    /// A pacing that does not parse.
    #[error(transparent)]
    ParsePacing(#[from] ParsePacingError),
    /// A pacing factor not finite and above 0.
    #[error(transparent)]
    Pacing(#[from] PacingError),
    /// Pins driven on a port with no inputs.
    #[error(transparent)]
    Inputs(#[from] NoInputsError),
    /// A switch interval that is no duration.
    #[error(transparent)]
    Interval(#[from] TryFromFloatSecsError),
    /// A board memory the board does not have.
    #[error("no memory {0:?}")]
    BoardMemory(String),
    /// A port index past a Python peripheral's ports.
    #[error("no port {0}")]
    Port(usize),
    /// A port index that is none of a Python peripheral's analog inputs.
    #[error("no analog input {0}")]
    AnalogInput(usize),
    /// A board time in seconds that is no span of board time.
    #[error("{0}: give a finite number of seconds, 0 or more, within board time")]
    Seconds(f64),
    /// Both or neither of seconds and picoseconds.
    #[error("give seconds or picoseconds")]
    SecondsOrPicoseconds,
    /// A host's wall clock reading that is no duration.
    #[error("wall {0}: give a finite number of seconds, 0 or more")]
    Wall(f64),
    /// A paced run's quantum of zero.
    #[error("give a quantum of 1 ps or more")]
    Quantum,
    /// A Python inspection's groups nested too deep, at the field named.
    #[error("{0}: inspected groups nest deeper than {INSPECTION_DEPTH}")]
    Nesting(String),
    /// A `dsack_port` answer that is none of 8, 16 and `None`.
    #[error("no {0}-bit port: give 8, 16 or None")]
    DsackPort(u8),
    /// A Python peripheral's read wider than its cycle.
    #[error("{value:#x} does not fit a {bytes}-byte cycle")]
    ReadWidth {
        /// The value.
        value: u32,
        /// The cycle's bytes.
        bytes: u32,
    },
    /// A QSPI peripheral's bits shifted back wider than the transfer.
    #[error("{value:#x} does not fit a {bits}-bit transfer")]
    ShiftedIn {
        /// The bits.
        value: u16,
        /// The transfer's bits.
        bits: u8,
    },
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

/// How deep a Python inspection's groups may nest: past it, a mapping
/// that holds itself would recurse without end.
pub(crate) const INSPECTION_DEPTH: usize = 32;

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
