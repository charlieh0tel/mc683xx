//! BDM's serial interface's responses: what the CPU shifts out in a
//! 17-bit transfer that is not data (CPU32RM 7.2.7, Table 7-3), for
//! the emulated chip and a pod's host alike.

/// The status/control bit, bit 16 of a transfer (CPU32RM 7.2.7).
pub const STATUS: u32 = 1 << 16;
/// "Not ready with response; come again" (CPU32RM Table 7-3).
pub const NOT_READY: u32 = STATUS;
/// "BERR terminated bus cycle; data invalid" (CPU32RM Table 7-3).
pub const BUS_ERROR: u32 = STATUS | 0x0001;
/// "Illegal command" (CPU32RM Table 7-3).
pub const ILLEGAL: u32 = STATUS | 0xFFFF;
/// "Command complete; status OK" (CPU32RM Table 7-3).
pub const COMPLETE: u32 = 0xFFFF;
/// What the emulated chip's transfer reads while the CPU is not in
/// background mode.  DSO is then IPIPE (CPU32RM 7.2.6), which the
/// chip does not model, so the line reads high; silicon's reads vary.
pub const NOT_IN_BACKGROUND: u32 = STATUS | 0xFFFF;
