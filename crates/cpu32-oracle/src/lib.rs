//! Test oracles for the CPU32 core: the register state and the memory
//! an instruction test compares, a call a firmware test runs, a seeded
//! generator, and, with the `musashi` and `unicorn` features, Musashi
//! and Unicorn in 68020 mode as black boxes.  Test-only; never
//! published.

pub mod call;
pub mod memory;
#[cfg(feature = "musashi")]
pub mod musashi;
pub mod random;
pub mod state;
#[cfg(feature = "unicorn")]
pub mod unicorn;
