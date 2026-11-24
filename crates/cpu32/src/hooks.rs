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
use std::ops::RangeInclusive;

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

/// A coverage, as [`Cpu::add_coverage`] returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoverageId(pub(crate) u64);

/// `coverage` and its number, in adding order from 1: `coverage 2`.
impl fmt::Display for CoverageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "coverage {}", self.0)
    }
}

/// The instructions executed in a range of addresses: a bit for each
/// word of it, set when an instruction that starts there executes
/// ([`Cpu::add_coverage`]).  Host instrumentation, as the hooks are:
/// not part of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    /// The first address, even.
    start: Address,
    /// The last address, on the address lines.
    end: Address,
    /// A bit for each word from `start`, the lowest bit of each the
    /// lowest address.
    words: Vec<u64>,
}

impl Coverage {
    /// Bits in each of `words`.
    const BITS: u32 = u64::BITS;

    /// `range`, nothing executed, its end no further than `mask`, the
    /// address lines'.
    pub(crate) fn new(range: &RangeInclusive<Address>, mask: u32) -> Self {
        let start = Address(range.start().0 & !1);
        let end = Address(range.end().0.min(mask));
        let count = if start > end {
            0
        } else {
            u64::from((end.0 - start.0) / 2) + 1
        };
        Self {
            start,
            end,
            words: vec![0; count.div_ceil(u64::from(Self::BITS)) as usize],
        }
    }

    /// The addresses it covers.
    #[must_use]
    pub const fn range(&self) -> RangeInclusive<Address> {
        self.start..=self.end
    }

    /// The word and bit of `address`, if it covers it and it is even.
    fn bit(&self, address: Address) -> Option<(usize, u64)> {
        if address.is_odd() || address < self.start || address > self.end {
            return None;
        }
        let index = (address.0 - self.start.0) / 2;
        Some(((index / Self::BITS) as usize, 1 << (index % Self::BITS)))
    }

    /// Records an instruction at `address` executed, if it covers it.
    pub(crate) fn record(&mut self, address: Address) {
        if let Some((word, bit)) = self.bit(address) {
            self.words[word] |= bit;
        }
    }

    /// Whether an instruction at `address` executed.
    #[must_use]
    pub fn contains(&self, address: Address) -> bool {
        self.bit(address)
            .is_some_and(|(word, bit)| self.words[word] & bit != 0)
    }

    /// The addresses of the instructions executed, in order.
    pub fn addresses(&self) -> impl Iterator<Item = Address> + '_ {
        self.words
            .iter()
            .enumerate()
            .flat_map(move |(index, &word)| {
                (0..Self::BITS)
                    .filter(move |bit| word & 1 << bit != 0)
                    .map(move |bit| self.start.offset(2 * (index as u32 * Self::BITS + bit)))
            })
    }

    /// Forgets every instruction executed.
    pub(crate) fn clear(&mut self) {
        self.words.fill(0);
    }
}
