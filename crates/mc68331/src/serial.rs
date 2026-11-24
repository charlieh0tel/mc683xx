//! What this chip's BDM serial interface sends beyond CPU32RM Table
//! 7-3's responses, which are in `mc683xx_target::serial`
//! ([`Mc68331::bdm_exchange`](crate::Mc68331::bdm_exchange) is one
//! transfer).

use mc683xx_target::serial::STATUS;

/// A transfer's 16 data bits.
pub(crate) const DATA: u32 = 0xFFFF;

/// What this chip's transfer reads while the CPU is not in background
/// mode.  DSO is then IPIPE (CPU32RM 7.2.6), which the chip does not
/// model, so the line reads high; silicon's reads vary.  Not a
/// response of CPU32RM Table 7-3, so not in `mc683xx_target::serial`.
pub const NOT_IN_BACKGROUND: u32 = STATUS | DATA;
