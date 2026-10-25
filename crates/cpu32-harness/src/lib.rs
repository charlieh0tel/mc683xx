//! Instruction-level tests of the CPU32 core: SingleStepTests' 68000
//! cases (`single_step`) and, with the `musashi` feature, a differential
//! fuzzer against Musashi's 68020 (`differential`); and the smartclock
//! firmware corpus (`firmware`, `libc`, `functions`), the image's own
//! functions called on known inputs, and with the `musashi` or
//! `unicorn` feature against those oracles (`oracles`).  All run whatever the core
//! implements and count the rest as unimplemented.  Test-only; never
//! published.

pub mod decode;
pub mod differences;
#[cfg(feature = "musashi")]
pub mod differential;
pub mod extension;
pub mod families;
pub mod firmware;
pub mod functions;
pub mod libc;
pub mod machine;
pub mod oracles;
pub mod single_step;
