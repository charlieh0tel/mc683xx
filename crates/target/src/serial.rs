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
