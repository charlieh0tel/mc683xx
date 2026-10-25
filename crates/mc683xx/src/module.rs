//! The chip's modules beside the SIM on the intermodule bus (IMB).
//!
//! A part composes its own (the MC68331's QSM and GPT, MC68331UM Table
//! D-1) behind [`Module`], and the chip routes their register cycles
//! and arbitrates their interrupt requests (SIMRM 6.3).

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::size::Size;

use crate::peripheral::Timed;

/// A register word's lower byte, bits 7:0, at the odd address (the IMB
/// is big-endian, MC68331UM 4.4.2).
const LOWER_BYTE: u16 = 0x00ff;
/// Its upper byte, bits 15:8, at the even address.
const UPPER_BYTE: u16 = !LOWER_BYTE;

/// A module's register block in the 4-Kbyte module space (SIMRM 1.1).
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// Its first byte's offset from the module space's base, $YFF000.
    pub offset: u32,
    /// Its length in bytes.
    pub length: u32,
}

impl Block {
    /// Whether `offset`, from the module space's base, is in it.
    #[must_use]
    pub const fn contains(self, offset: u32) -> bool {
        offset >= self.offset && offset - self.offset < self.length
    }
}

/// A register cycle on the IMB.
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterAccess {
    /// The byte offset in the module's block.
    pub offset: u32,
    /// Byte or word.
    pub size: Size,
    /// Whether the CPU is at the supervisor privilege level (the
    /// cycle's FC2).
    pub supervisor: bool,
    /// When it starts.
    pub now: Cycles,
}

impl RegisterAccess {
    /// The even offset of the register word it falls in.
    #[must_use]
    pub const fn word(self) -> u32 {
        self.offset & !1
    }

    /// Whether it carries the word's upper byte, bits 15:8, at the
    /// even address (the IMB is big-endian, MC68331UM 4.4.2).
    #[must_use]
    pub fn upper(self) -> bool {
        self.size != Size::Byte || self.offset & 1 == 0
    }

    /// Whether it carries the word's lower byte, bits 7:0.
    #[must_use]
    pub fn lower(self) -> bool {
        self.size != Size::Byte || self.offset & 1 != 0
    }

    /// `word` as a read cycle returns it, right-aligned.
    #[must_use]
    pub fn read_value(self, word: u16) -> u32 {
        match (self.upper(), self.lower()) {
            (true, false) => u32::from(word >> 8),
            (false, true) => u32::from(word & LOWER_BYTE),
            _ => u32::from(word),
        }
    }

    /// The word a write cycle of `value` makes of `old`: a byte keeps
    /// the other half.
    #[must_use]
    pub fn merge(self, old: u16, value: u32) -> u16 {
        match (self.upper(), self.lower()) {
            (true, false) => old & LOWER_BYTE | (value as u16) << 8,
            (false, true) => old & UPPER_BYTE | value as u16 & LOWER_BYTE,
            _ => value as u16,
        }
    }
}

/// An interrupt request a module makes (SIMRM 6.3).
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptRequest {
    /// Its priority level.
    pub level: InterruptLevel,
    /// The IARB field of the module's configuration register: 0 does
    /// not contend, so the CPU takes a spurious interrupt.
    pub arbitration: u8,
}

/// What a read of a module's register would find, found without the
/// read: a debugger's look, as BDM's memory commands take (CPU32RM 7).
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peek {
    /// The value a read cycle would return now, right-aligned.
    Value(u32),
    /// The module would not acknowledge the cycle, which goes on to
    /// the external bus (`read`'s `None`).
    Unacknowledged,
    /// The module cannot tell without the read itself.
    Unpeekable,
}

/// What seals [`Module`]: only the workspace's chips compose modules,
/// so a module's methods may change in any release.
///
/// Not part of the semver API: plumbing `mc683xx-mc68331` assembles the chip with.
#[doc(hidden)]
pub mod sealed {
    /// Implemented by the workspace's modules alone.
    pub trait Sealed {}
}

/// A module on the IMB.  A cycle the module does not acknowledge
/// (`None`, `false`) goes on to the external bus, as a QSM register
/// cycle from user mode does when SUPV is set (QSMRM 3.3).
///
/// Sealed: a host cannot implement it.  A part composes its own modules;
/// a host reaches them through the chip.  So this does not compile:
///
/// ```compile_fail,E0277
/// use mc683xx_cpu32::cpu::state::StateError;
/// use mc683xx_cpu32::cycles::Cycles;
/// use mc683xx_cpu32::interrupt::InterruptLevel;
/// use mc683xx_modules::module::Block;
/// use mc683xx_modules::module::InterruptRequest;
/// use mc683xx_modules::module::Module;
/// use mc683xx_modules::module::Peek;
/// use mc683xx_modules::module::RegisterAccess;
/// use mc683xx_modules::peripheral::Timed;
///
/// struct Mine;
///
/// impl Timed for Mine {}
///
/// impl Module for Mine {
///     fn block(&self) -> Block {
///         Block { offset: 0, length: 0 }
///     }
///     fn read(&mut self, _: RegisterAccess) -> Option<u32> {
///         None
///     }
///     fn peek(&self, _: RegisterAccess) -> Peek {
///         Peek::Unacknowledged
///     }
///     fn write(&mut self, _: RegisterAccess, _: u32) -> bool {
///         false
///     }
///     fn request(&self) -> Option<InterruptRequest> {
///         None
///     }
///     fn acknowledge(&mut self, _: InterruptLevel, _: Cycles) -> Option<u8> {
///         None
///     }
///     fn reset(&mut self, _: Cycles) {}
///     fn save(&self) -> Result<Vec<u8>, StateError> {
///         Ok(Vec::new())
///     }
///     fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
///         Ok(())
///     }
/// }
/// ```
pub trait Module: Timed + sealed::Sealed {
    /// Its register block.
    fn block(&self) -> Block;
    /// A read cycle; the value right-aligned.
    fn read(&mut self, access: RegisterAccess) -> Option<u32>;
    /// What `read` would return for `access`, without its side effects
    /// (the flags a read arms or clears), the module already brought
    /// to `access.now`.
    fn peek(&self, access: RegisterAccess) -> Peek;
    /// A write cycle of `value`, right-aligned; whether it was
    /// acknowledged.
    fn write(&mut self, access: RegisterAccess, value: u32) -> bool;
    /// Its pending interrupt request, if it makes one.
    fn request(&self) -> Option<InterruptRequest>;
    /// The CPU acknowledges `level` and the module won arbitration:
    /// its vector, or `None` if it has none to give.
    fn acknowledge(&mut self, level: InterruptLevel, now: Cycles) -> Option<u8>;
    /// The chip's reset, or the `reset` instruction's (SIMRM 8.2.6).
    fn reset(&mut self, now: Cycles);

    /// Its whole state as a snapshot, versioned
    /// ([`mc683xx_cpu32::cpu::state`]'s `encode`), for the chip's.
    ///
    /// # Errors
    ///
    /// [`StateError::Unsupported`] from a module that cannot save; the
    /// chip cannot either.
    fn save(&self) -> Result<Vec<u8>, StateError>;

    /// Restores a snapshot `save` made.  It must take what its own
    /// `save` gives without error: the chip checks a module's snapshot
    /// by restoring it, and puts back the module's own if a later part
    /// of the chip's snapshot is refused.
    ///
    /// # Errors
    ///
    /// A [`StateError`] if `snapshot` is not one this module's `save`
    /// made, or holds a state it cannot be in; the module is then
    /// unchanged.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError>;
}
