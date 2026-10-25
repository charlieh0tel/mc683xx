//! A GDB remote stub for the CPU32: the bare CPU, the chip or a board.
//!
//! gdb's remote serial protocol, through the [`gdbstub`] crate, over a
//! [`target::Target`] ([PLAN.md, Debugger]).  [`docs/gdb.md`] is its
//! guide, whose Rust block the `examples` crate runs as a doctest.
//!
//! It costs a run nothing until gdb is attached: breakpoints,
//! watchpoints and steps are the core's hooks, installed while gdb
//! wants them, and the target runs only inside [`stub::serve`].
//!
//! # Examples
//!
//! A bare CPU served over an in-memory connection, as gdb would ask
//! it why it stopped and then detach:
//!
//! ```
//! use std::collections::VecDeque;
//! use std::io;
//!
//! use gdbstub::conn::Connection;
//! use gdbstub::conn::ConnectionExt;
//! use mc683xx_cpu32::flat::ADDRESS_BITS;
//! use mc683xx_cpu32::flat::Machine;
//! use mc683xx_gdb::stub::Ending;
//! use mc683xx_gdb::stub::serve;
//!
//! /// gdb's end of the connection: its packets, and the stub's replies.
//! struct Wire {
//!     /// What gdb sends.
//!     input: VecDeque<u8>,
//!     /// What the stub sent.
//!     output: Vec<u8>,
//! }
//!
//! impl Connection for Wire {
//!     type Error = io::Error;
//!
//!     fn write(&mut self, byte: u8) -> io::Result<()> {
//!         self.output.push(byte);
//!         Ok(())
//!     }
//!
//!     fn flush(&mut self) -> io::Result<()> {
//!         Ok(())
//!     }
//! }
//!
//! impl ConnectionExt for Wire {
//!     fn read(&mut self) -> io::Result<u8> {
//!         self.input
//!             .pop_front()
//!             .ok_or_else(|| io::ErrorKind::UnexpectedEof.into())
//!     }
//!
//!     fn peek(&mut self) -> io::Result<Option<u8>> {
//!         Ok(self.input.front().copied())
//!     }
//! }
//!
//! let mut machine = Machine::new(ADDRESS_BITS)?;
//! // `?`, why it stopped, then `D`, detach: `$` each body `#` its sum.
//! let mut wire = Wire {
//!     input: b"$?#3f$D#44".iter().copied().collect(),
//!     output: Vec::new(),
//! };
//! let ending = serve(
//!     &mut machine,
//!     &mut wire as &mut dyn ConnectionExt<Error = io::Error>,
//! )?;
//! assert_eq!(ending, Ending::Detached);
//! assert!(wire.output.starts_with(b"+$T05"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [PLAN.md, Debugger]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#debugger
//! [`docs/gdb.md`]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

mod arch;
pub mod stub;
pub mod target;
