//! The host's hooks: plain in-process callbacks on what the core does.
//!
//! They are cheap when absent ([PLAN.md, Hooks]).  Code, block and
//! memory hooks cover address ranges, whose pages carry a bit: a cycle
//! or an instruction looks a hook up only on a page with its bit, and
//! only while some page has one.  Any hook can stop the run
//! ([`Stop::Hook`]).
//!
//! [PLAN.md, Hooks]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#hooks

use std::fmt;
use std::ops::ControlFlow;

use crate::exception::BusCycleKind;
use crate::exception::Vector;
use crate::interrupt::InterruptLevel;
use crate::registers::Registers;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

#[cfg(doc)]
use crate::Cpu;
#[cfg(doc)]
use crate::Stop;

/// A hook, as the `Cpu::add_*_hook` that installed it returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HookId(pub(crate) u64);

/// `hook` and its number, in installing order from 1: `hook 3`.
impl fmt::Display for HookId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "hook {}", self.0)
    }
}

/// What a hook returns: [`Continue`](ControlFlow::Continue), or
/// [`Break`](ControlFlow::Break) to stop the run.
pub type Control = ControlFlow<()>;

/// An instruction about to execute, as a code or block hook sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Instruction {
    /// Its address.
    pub pc: Address,
    /// The registers.
    pub registers: Registers,
    /// The long word at A7, in data space, if the core serves it from
    /// memory (no bus cycle is run for it).  A word of data, not an
    /// [`Address`]: it is one only where the code makes it one, as at a
    /// subroutine's first instruction, its return address.
    pub stack_top: Option<u32>,
}

/// A bus cycle in user or supervisor space, as a memory hook sees it.
///
/// A byte or word cycle, once it has ended well.  A long operand is two
/// word cycles, as on the bus, but with [`Cpu::set_long_bus`] one
/// access of size 4 once the whole operand has ended well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryAccess {
    /// Its address, on the chip's address lines.
    pub address: Address,
    /// Byte, word, or long ([`Cpu::set_long_bus`]).
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// The value read or written, right-aligned.
    pub value: u32,
    /// For a write, the value it replaced, right-aligned, read with no
    /// cycle before it was written from the bytes it writes to (a
    /// page's write backing, not its read one): [`None`] for a read,
    /// for a write in CPU space, and for a write the core's memory does
    /// not serve (a peripheral's: a read there would be a cycle of its
    /// own).
    pub previous: Option<u32>,
    /// The address of the instruction it is part of.
    pub pc: Address,
    /// Whether it is exception processing stacking a frame.
    pub stacking: bool,
}

/// An operand read in user or supervisor space the core's memory is
/// about to serve, as a pre-read hook sees it, before its first cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PendingRead {
    /// Its address, on the chip's address lines.
    pub address: Address,
    /// Byte, word or long: the operand's, however many cycles it takes.
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// The address of the instruction it is part of.
    pub pc: Address,
    /// Whether it is exception processing stacking a frame.
    pub stacking: bool,
}

/// An operand write in user or supervisor space the core's memory is
/// about to store, as a pre-write hook sees it, before its first cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PendingWrite {
    /// Its address, on the chip's address lines.
    pub address: Address,
    /// Byte, word or long: the operand's, however many cycles it takes.
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// The address of the instruction it is part of.
    pub pc: Address,
    /// Whether it is exception processing stacking a frame.
    pub stacking: bool,
    /// Whether the store is dropped, as the hooks before left it.
    dropped: bool,
}

impl PendingWrite {
    /// A write of `size` at `address` in `function_code`'s space, part
    /// of the instruction at `pc`, not dropped.
    pub(crate) const fn new(
        address: Address,
        size: Size,
        function_code: FunctionCode,
        pc: Address,
        stacking: bool,
    ) -> Self {
        Self {
            address,
            size,
            function_code,
            pc,
            stacking,
            dropped: false,
        }
    }

    /// Whether the store is dropped: set by this hook or one before it.
    #[must_use]
    pub const fn dropped(&self) -> bool {
        self.dropped
    }

    /// Drops the store, or, with `false`, keeps it after all: once the
    /// hooks have run, a dropped store writes nothing.
    pub const fn set_dropped(&mut self, dropped: bool) {
        self.dropped = dropped;
    }
}

/// Why a bus cycle was invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Invalid {
    /// The bus ended it in a bus error: nothing answered, or a peripheral
    /// refused it.
    BusError,
    /// Its page's permissions do not allow it
    /// ([`Memory::protect`](crate::memory::Memory::protect)); it ends in a
    /// bus error.
    Protected,
}

/// A bus cycle that ended in a bus error, as an invalid access hook
/// sees it.  The CPU then takes the bus error exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InvalidAccess {
    /// Its address, on the chip's address lines.
    pub address: Address,
    /// Byte or word.
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// Fetch, read or write.
    pub kind: BusCycleKind,
    /// A write's value, right-aligned.
    pub value: Option<u32>,
    /// Why.
    pub reason: Invalid,
    /// The address of the instruction it is part of.
    pub pc: Address,
    /// Whether it is exception processing stacking a frame.
    pub stacking: bool,
}

/// An exception entered: its frame stacked and its handler's address
/// in PC (CPU32RM 6.1.3).  Reset is not one: the chip reports resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExceptionEntry {
    /// Its vector.
    pub vector: Vector,
    /// The return PC its frame stacked.
    pub pc: Address,
    /// Its handler's address.
    pub handler: Address,
    /// SSP: its frame's lowest address.
    pub stack: Address,
    /// The level, for an interrupt.
    pub level: Option<InterruptLevel>,
}

/// An exception returned from: `rte` restored SR and PC from its four-
/// or six-word frame, or from a format $C frame of a bus or address
/// error in an instruction, and deallocated it (CPU32RM 6.2.12).  A
/// format $C frame of a fault in exception processing (6.3.1.4) is not
/// one: its `rte` takes the faulted exception again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExceptionReturn {
    /// The vector its frame's format/vector word names.
    pub vector: Vector,
    /// The PC it returns to.
    pub pc: Address,
    /// The SR it restored.
    pub sr: u16,
    /// Its frame's lowest address, SSP as `rte` began: an
    /// [`ExceptionEntry`]'s `stack` for the same frame.
    pub stack: Address,
}
