//! The host guide's examples: `docs/host-guide.md` is this crate's
//! documentation, so `cargo test` compiles and runs each of its Rust
//! blocks as a doctest.  `firmware` is the little the examples share:
//! a ROM image and the few instructions their programs need.  `gdb`
//! holds `docs/gdb.md`'s, the same way.  `echo` is the example board's
//! firmware, `boards/echo.bin`.
#![doc = include_str!("../../docs/host-guide.md")]

#[doc = include_str!("../../docs/gdb.md")]
pub mod gdb {}

pub mod echo;
pub mod firmware;
