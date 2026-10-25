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
//!   bus (`read`, `write`, `wait_states`, through an attachment, which a
//!   [`PeripheralAccess`] names by index with its [`Role`]), the IRQ pin
//!   (`interrupt_request`), RESET (`reset`), the QSPI (`transfer`,
//!   `select`, `qspi_clock`, `qspi_deferrable`), its net ports (`input`,
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
//! - [`Time`]: board time, integer picoseconds.
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
//!   host's own loop.
//!
//! # Examples
//!
//! A board of one ROM, zero-filled: its reset vectors are zero, so the
//! CPU runs `ORI.B #0,D0` from address 0 until the time runs out.
//!
//! ```
//! use std::path::Path;
//!
//! use mc683xx_board::board::Board;
//! use mc683xx_board::description::Description;
//! use mc683xx_board::description::Options;
//! use mc683xx_board::registry::Environment;
//! use mc683xx_board::registry::Registry;
//! use mc683xx_board::time::Time;
//! use mc683xx_cpu32::cpu::Stop;
//! use mc683xx_mc68331::chip::Mc68331;
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
//! let mut board = Board::<Mc68331>::load(&description, &registry, Environment::default())?;
//! let until = Time(1_000_000);
//! let run = board.run_until(until)?;
//! assert_eq!(run.stop, Stop::Budget);
//! assert_eq!(run.now, until);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [plan]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards
//! [board-files]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md
//! [register]: registry::Registry::register
//! [`Chip`]: chip::Chip
//! [`Registry`]: registry::Registry
//! [`PortSpec`]: registry::PortSpec
//! [`Kind`]: signal::Kind
//! [`Direction`]: signal::Direction
//! [`Context`]: registry::Context
//! [`Parameters`]: registry::Parameters
//! [`Environment`]: registry::Environment
//! [`Peripheral`]: peripheral::Peripheral
//! [`PeripheralAccess`]: peripheral::PeripheralAccess
//! [`Role`]: peripheral::Role
//! [`Drive`]: peripheral::Drive
//! [`MappedPeripheral`]: mc683xx_cpu32::peripheral::MappedPeripheral
//! [`Timed`]: mc683xx_modules::peripheral::Timed
//! [`SpiPeripheral`]: mc683xx_modules::peripheral::SpiPeripheral
//! [`MappedAdapter`]: adapter::MappedAdapter
//! [`SpiAdapter`]: adapter::SpiAdapter
//! [`Save`]: mc683xx_cpu32::cpu::state::Save
//! [`Edges`]: spi::Edges
//! [`Slave`]: spi::Slave
//! [`exchange`]: spi::exchange
//! [`Signal`]: signal::Signal
//! [`Clock`]: time::Clock
//! [`Time`]: time::Time
//! [`Observer`]: observe::Observer
//! [`Board::observe`]: board::Board::observe
//! [`Inspection`]: inspect::Inspection
//! [`Peripheral::inspect`]: peripheral::Peripheral::inspect
//! [`Board::inspect`]: board::Board::inspect
//! [`BoardControl`]: control::BoardControl
//! [`Board::control`]: board::Board::control
//! [`Pacing`]: pacing::Pacing
//! [`Board::run_until_paced`]: board::Board::run_until_paced
//! [`Pacer`]: pacing::Pacer
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

pub mod adapter;
pub mod board;
pub mod builtin;
pub mod chip;
pub mod control;
pub mod description;
pub mod endpoint;
pub mod error;
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
pub mod time;
pub mod trace;
mod wiring;
