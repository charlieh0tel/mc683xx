//! What can be wrong with a board file, or with loading it.

use std::io;
use std::path::PathBuf;

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_modules::sim::clock::ClockSourceError;

use crate::registry::ParameterError;
use crate::signal::Kind;
use crate::time::Time;

/// Why a board file was refused, each naming what in the file is wrong.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The file could not be read.
    #[error("{}: {error}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// Why.
        error: io::Error,
    },
    /// The TOML does not parse, or a key or value is not the schema's.
    #[error(transparent)]
    Syntax(SyntaxError),
    /// A version other than this build's, or none.
    #[error("board file version {0}: this build reads version 1")]
    Version(String),
    /// A name used twice.
    #[error("{0:?} is named twice")]
    Duplicate(String),
    /// A name that cannot name a peripheral: empty, holding a `.`, or `chip`
    /// or `external`, which name endpoints.
    #[error("{0:?} cannot be a name")]
    Name(String),
    /// A chip-select pin name that is none of CSBOOT, CS0 to CS10.
    #[error("{owner}: no chip select {pin:?}")]
    Pin {
        /// What names it.
        owner: String,
        /// The name.
        pin: String,
    },
    /// Two users of one select, or of one decoder output, that do not
    /// split it.
    #[error("{point}: {first} and {second} both take its cycles")]
    Shared {
        /// The select or decoder output.
        point: String,
        /// One user.
        first: String,
        /// The other.
        second: String,
    },
    /// A memory's image that could not be read.
    #[error("{memory}: image {}: {error}", path.display())]
    Image {
        /// The memory.
        memory: String,
        /// Its path.
        path: PathBuf,
        /// Why.
        error: io::Error,
    },
    /// A memory's size that is no whole number of 2-Kbyte pages, or is
    /// less than its image.
    #[error("{memory}: {reason}")]
    Size {
        /// The memory.
        memory: String,
        /// What is wrong.
        reason: String,
    },
    /// A fixed memory's base off a page boundary, putting it past
    /// ADDR\[23:0\], or overlapping another's.
    #[error("{memory}: {reason}")]
    Base {
        /// The memory.
        memory: String,
        /// What is wrong.
        reason: String,
    },
    /// An attachment that is not one of the forms, or that its owner
    /// cannot take.
    #[error("{owner}: attachment {index}: {reason}")]
    Attachment {
        /// Its memory or peripheral.
        owner: String,
        /// Its index.
        index: usize,
        /// What is wrong.
        reason: String,
    },
    /// A decoder whose inputs, outputs or aliases are wrong.
    #[error("decoder {decoder}: {reason}")]
    Decoder {
        /// The decoder.
        decoder: String,
        /// What is wrong.
        reason: String,
    },
    /// An interrupt level that is no IRQ pin's, 1 to 7.
    #[error("{owner}: no IRQ pin at level {level}")]
    Level {
        /// What names it.
        owner: String,
        /// The level.
        level: u8,
    },
    /// An option `[board] options` does not declare.
    #[error("no board option {0:?}")]
    Option(String),
    /// Parameters set for a peripheral the file does not name.
    #[error("parameters set for {0:?}, which the board file does not name")]
    Peripheral(String),
    /// A parameter setting not of the form `PERIPHERAL.KEY=VALUE`.
    #[error("{0:?}: give PERIPHERAL.KEY=VALUE")]
    Setting(String),
    /// Both a reference and an external clock.
    #[error("chip: give reference_hz or external_hz, not both")]
    Clock,
    /// A clock frequency the chip cannot have.
    #[error("chip: {error}")]
    Frequency {
        /// Why.
        error: ClockSourceError,
    },
    /// A peripheral type the registry does not know.
    #[error("{peripheral}: no peripheral type {kind:?}")]
    Type {
        /// The peripheral.
        peripheral: String,
        /// The type.
        kind: String,
    },
    /// A parameter the peripheral's constructor refused.
    #[error("{peripheral}: {error}")]
    Parameter {
        /// The peripheral.
        peripheral: String,
        /// Why.
        error: ParameterError,
    },
    /// A constructor that gave two ports one name.
    #[error("{peripheral}: port {port:?} declared twice")]
    Port {
        /// The peripheral.
        peripheral: String,
        /// The port.
        port: String,
    },
    /// An endpoint that does not parse, or names nothing on the board.
    #[error("net {net}: no endpoint {endpoint:?}")]
    Endpoint {
        /// The net.
        net: String,
        /// The endpoint.
        endpoint: String,
    },
    /// An endpoint of another type than its net's.
    #[error("net {net} ({kind}): {endpoint} is {found}")]
    Kind {
        /// The net.
        net: String,
        /// The net's type.
        kind: Kind,
        /// The endpoint.
        endpoint: String,
        /// What the endpoint is.
        found: Kind,
    },
    /// An endpoint on the wrong side: a driver that cannot drive, a sink
    /// that cannot be driven.
    #[error("net {net}: {endpoint} cannot {what}")]
    Direction {
        /// The net.
        net: String,
        /// The endpoint.
        endpoint: String,
        /// "drive" or "be driven".
        what: &'static str,
    },
    /// Several drivers on a net that is not wired, a wired net that is
    /// not logic, or an endpoint on two nets in one role.
    #[error("net {net}: {reason}")]
    Net {
        /// The net.
        net: String,
        /// What is wrong.
        reason: String,
    },
    /// A file with no `[chip] type`: every board file names its chip.
    #[error("chip: give its type, `[chip] type = \"...\"`")]
    NoChip,
    /// A `[chip] type` other than the chip the board is loaded with.
    #[error("chip type {found:?}: this board is loaded as {expected:?}")]
    ChipType {
        /// The file's.
        found: String,
        /// The chip's.
        expected: &'static str,
    },
    /// The chip refused the wiring: its own error.
    #[error(transparent)]
    Build(Box<dyn std::error::Error + Send + Sync>),
    /// A snapshot that does not restore into this board.
    #[error(transparent)]
    State(#[from] StateError),
}

/// Why a board file's TOML does not parse, or does not fit the schema:
/// its line, column and what is wrong, as the message says.  Opaque:
/// the parser is not part of the API.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SyntaxError(toml::de::Error);

impl SyntaxError {
    /// `error`, as a load's.
    pub(crate) const fn from_toml(error: toml::de::Error) -> LoadError {
        LoadError::Syntax(Self(error))
    }
}

/// Why a run could not go on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// A peripheral whose `advance` left its `next_event` at or before the
    /// time it was advanced to, `repeats` times running: it breaks
    /// [`Peripheral::advance`]'s contract, and the board would never get past
    /// `at`.
    ///
    /// [`Peripheral::advance`]: crate::peripheral::Peripheral::advance
    #[error("{peripheral}: advanced to {} ps {repeats} times, its next event never past it", at.0)]
    Stalled {
        /// The peripheral.
        peripheral: String,
        /// The time it was advanced to.
        at: Time,
        /// The advances.
        repeats: u32,
    },
}

/// Why a run-time endpoint could not drive a value ([`Board::send`]).
///
/// [`Board::send`]: crate::board::Board::send
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SendError {
    /// The board has no `external.name` that drives a net.
    #[error("external.{0} drives no net")]
    Endpoint(String),
    /// It drives only nets of `kinds`, none of which carries the value.
    #[error("external.{endpoint} drives {} nets only, which do not carry the value", kinds.iter().map(ToString::to_string).collect::<Vec<_>>().join(" and "))]
    Kind {
        /// The endpoint's name.
        endpoint: String,
        /// The types of the nets it drives.
        kinds: Vec<Kind>,
    },
}

/// Why [`Board::take_received`] took nothing.
///
/// [`Board::take_received`]: crate::board::Board::take_received
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReceiveError {
    /// The board has no `external.name` that a net drives.
    #[error("no net drives external.{0}")]
    Endpoint(String),
}

/// Why a net could not be observed or read ([`Board::observe`],
/// [`Board::net_value`]).
///
/// [`Board::observe`]: crate::board::Board::observe
/// [`Board::net_value`]: crate::board::Board::net_value
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    /// The board has no net of that name.
    #[error("no net {0:?}")]
    Unknown(String),
}
