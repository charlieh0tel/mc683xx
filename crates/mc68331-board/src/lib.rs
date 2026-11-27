//! Boards around a 683xx chip: memories, peripherals, decoders and nets
//! around the chip, run on board time.  The chip is a [`Chip`]; the
//! MC68331 is [`mc68331`] (the default feature `mc68331`).
//!
//! A board file says what is wired where and how the chip is
//! configured, never what a peripheral does: peripherals are of types a
//! host registers.  The design is in [PLAN.md, Boards][plan],
//! the file format in [`docs/board-files.md`][board-files].
//!
//! The API a host's peripheral models target:
//!
//! - [`Registry`]: peripheral types by name.
//!   [`register(name, ports, constructor)`][register] adds one: its
//!   declared ports ([`PortSpec`]: a name, a [`Kind`] and a
//!   [`Direction`]), and a constructor from a [`Context`] (the peripheral's
//!   name, its [`Parameters`] from the file, and the run's
//!   [`Environment`]: the seed and the work directory).
//! - [`Peripheral`]: what a constructor builds.  Every method has a
//!   default, so a peripheral implements only the sides it is on: the
//!   bus (`read`, `write`, `wait_states`, `wants_horizon`, through an
//!   attachment, which a [`PeripheralAccess`] names by index with its
//!   [`Role`]), the IRQ pin
//!   (`interrupt_request`), RESET (`reset`), the QSPI (`transfer`,
//!   `select`, `set_qspi_clock`, `qspi_deferrable`), its net ports (`input`,
//!   and [`Drive`] for its outputs), its own time (`next_event`,
//!   `advance`), its snapshot (`save`, `restore`) and what a front end
//!   shows of it (`inspect`).
//! - [`adapter`]: a chip-level model ([`MappedPeripheral`] and [`Timed`], or
//!   [`SpiPeripheral`]) as a peripheral, [`MappedAdapter`] or
//!   [`SpiAdapter`], in the board's snapshot if it implements
//!   [`Save`].
//! - [`spi`]: a QSPI peripheral's SPI edges: its datasheet's edges
//!   ([`Edges`]) and shift logic ([`Slave`]), run in the transfer's mode
//!   ([`exchange`]).
//! - [`Signal`]: what a net carries: `Level` on `logic` and `edge` nets,
//!   `Clock` on `clock` nets ([`Clock`]: exact phase, frequency changes
//!   at a time, edge counting and edge times), `Serial` frames, `Analog`
//!   values.
//! - [`Time`] and [`Span`]: board time and its spans, integer picoseconds.
//!
//! What a front end (a TUI, a GUI) uses, outside the board file:
//!
//! - [`Observer`]: a read-only watcher of a net, attached by name
//!   ([`Board::observe`]), told each change at its board time.
//! - [`Inspection`]: a peripheral's state as a tree of named fields
//!   ([`Peripheral::inspect`], [`Board::inspect`]).
//! - [`BoardControl`]: a thread-safe handle ([`Board::control`]) by
//!   which another thread (a GUI's event loop) stops the run, queues
//!   inputs to externals and sets the pacing, all taken at the run's
//!   sync points.
//! - [`Pacing`]: how fast a paced run ([`Board::run_until_paced`]) goes
//!   against wall time, host-side only, and [`Pacer`], its wait, for a
//!   host's own loop; [`Board::run_frame`] runs a frame of the board
//!   time due by a host's own clock (a browser's).
//!
//! # Examples
//!
//! A board of one ROM, zero-filled: its reset vectors are zero, so the
//! CPU runs `ORI.B #0,D0` from address 0 until the time runs out.
//!
//! ```
//! use std::path::Path;
//!
//! use mc683xx_board::Board;
//! use mc683xx_board::Description;
//! use mc683xx_board::Environment;
//! use mc683xx_board::Options;
//! use mc683xx_board::registry::Registry;
//! use mc683xx_board::time::Time;
//! use mc683xx_cpu32::Stop;
//! use mc683xx_mc68331::Mc68331;
//!
//! let text = r#"
//! version = 1
//!
//! [chip]
//! type = "mc68331"
//!
//! [[memory]]
//! name = "rom"
//! size = 0x800
//! attach = [{ select = "CSBOOT" }]
//! "#;
//! let description = Description::parse(text, Path::new("."), &Options::default())?;
//! let registry = Registry::with_builtins();
//! let mut board = Board::<Mc68331>::load(&description, &registry, &Environment::default())?;
//! let deadline = Time::from_picos(1_000_000);
//! let run = board.run_until(deadline)?;
//! assert_eq!(run.stop, Stop::Budget);
//! assert_eq!(run.now, deadline);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [plan]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards
//! [board-files]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md
//! [register]: registry::Registry::register
//! [`Registry`]: registry::Registry
//! [`PortSpec`]: registry::PortSpec
//! [`Kind`]: signal::Kind
//! [`Direction`]: signal::Direction
//! [`Context`]: registry::Context
//! [`Parameters`]: registry::Parameters
//! [`Peripheral`]: peripheral::Peripheral
//! [`PeripheralAccess`]: peripheral::PeripheralAccess
//! [`Role`]: peripheral::Role
//! [`Drive`]: peripheral::Drive
//! [`MappedPeripheral`]: mc683xx_cpu32::peripheral::MappedPeripheral
//! [`Timed`]: mc683xx_modules::peripheral::Timed
//! [`SpiPeripheral`]: mc683xx_modules::peripheral::SpiPeripheral
//! [`MappedAdapter`]: adapter::MappedAdapter
//! [`SpiAdapter`]: adapter::SpiAdapter
//! [`Save`]: mc683xx_cpu32::state::Save
//! [`Edges`]: spi::Edges
//! [`Slave`]: spi::Slave
//! [`exchange`]: spi::exchange
//! [`Signal`]: signal::Signal
//! [`Clock`]: time::Clock
//! [`Time`]: time::Time
//! [`Span`]: time::Span
//! [`Observer`]: observe::Observer
//! [`Inspection`]: inspect::Inspection
//! [`Peripheral::inspect`]: peripheral::Peripheral::inspect
//! [`Pacing`]: pacing::Pacing
//! [`Pacer`]: pacing::Pacer
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod adapter;
mod board;
pub mod builtin;
mod chip;
pub mod condition;
mod control;
mod description;
mod endpoint;
mod environment;
pub mod inspect;
#[cfg(feature = "mc68331")]
pub mod mc68331;
pub mod observe;
pub mod pacing;
pub mod peripheral;
pub mod registry;
mod schema;
pub mod signal;
pub mod spi;
pub mod state;
pub mod time;
pub mod trace;
mod wiring;

pub use board::Board;
pub use board::BoardRun;
pub use board::BoardStopRequest;
pub use board::BoardTracing;
pub use board::External;
pub use board::LookupError;
pub use board::NoMemory;
pub use board::NoNet;
pub use board::NoPeripheral;
pub use board::NoReceiver;
pub use board::RunError;
pub use board::SendError;
pub use chip::Chip;
#[doc(hidden)]
pub use chip::ChipBuilder;
pub use chip::ChipPin;
#[doc(hidden)]
pub use chip::MemoryPlace;
#[doc(hidden)]
pub use chip::MemoryPlan;
#[doc(hidden)]
pub use chip::Requests;
pub use control::BoardControl;
pub use description::Description;
pub use description::EditError;
pub use description::LoadError;
pub use description::Options;
pub use description::ParseSettingError;
pub use description::Setting;
pub use description::SyntaxError;
pub use environment::Environment;
