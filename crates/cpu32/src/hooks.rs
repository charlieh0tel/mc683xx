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
use std::str::FromStr;

use crate::exception::BusCycleKind;
use crate::exception::Vector;
use crate::interrupt::InterruptLevel;
use crate::memory::PAGE_BYTES;
use crate::memory::PAGE_SHIFT;
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
    /// Its number: the instructions run before it, as
    /// [`Cpu::instructions`] counts them, from 0.
    pub instruction: u64,
    /// The registers.
    pub registers: Registers,
    /// The long word at A7, in data space, if the core serves it from
    /// memory (no bus cycle is run for it).  A word of data, not an
    /// [`Address`]: it is one only where the code makes it one, as at a
    /// subroutine's first instruction, its return address.
    pub stack_top: Option<u32>,
    /// Its first word, the opcode, in program space, if the core serves
    /// it from memory (no bus cycle is run for it): [`None`] for an
    /// instruction the bus would supply, or at an odd PC.
    pub opcode: Option<u16>,
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
    /// The number of the instruction it is part of, its code hooks'
    /// [`Instruction::instruction`]: the instructions run before it
    /// ([`Cpu::instructions`]).  Exception processing's is the number
    /// of the instruction that raised it or that a trace follows;
    /// an interrupt's, taken between instructions, the next one's.
    pub instruction: u64,
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
    /// The number of the instruction it is part of, its code hooks'
    /// [`Instruction::instruction`]: the instructions run before it
    /// ([`Cpu::instructions`]).  Exception processing's is the number
    /// of the instruction that raised it or that a trace follows;
    /// an interrupt's, taken between instructions, the next one's.
    pub instruction: u64,
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
    /// The number of the instruction it is part of, its code hooks'
    /// [`Instruction::instruction`]: the instructions run before it
    /// ([`Cpu::instructions`]).  Exception processing's is the number
    /// of the instruction that raised it or that a trace follows;
    /// an interrupt's, taken between instructions, the next one's.
    pub instruction: u64,
    /// Whether it is exception processing stacking a frame.
    pub stacking: bool,
    /// Whether the store is dropped, as the hooks before left it.
    dropped: bool,
}

impl PendingWrite {
    /// A write of `size` at `address` in `function_code`'s space, part
    /// of the instruction at `pc`, number `instruction`, not dropped.
    pub(crate) const fn new(
        address: Address,
        size: Size,
        function_code: FunctionCode,
        pc: Address,
        instruction: u64,
        stacking: bool,
    ) -> Self {
        Self {
            address,
            size,
            function_code,
            pc,
            instruction,
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
    /// The number of the instruction it is part of, its code hooks'
    /// [`Instruction::instruction`]: the instructions run before it
    /// ([`Cpu::instructions`]).  Exception processing's is the number
    /// of the instruction that raised it or that a trace follows;
    /// an interrupt's, taken between instructions, the next one's.
    pub instruction: u64,
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

/// What a coverage records ([`Cpu::add_coverage`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CoverageKind {
    /// Each instruction executed: a bit for each word, set when an
    /// instruction that starts there executes.
    Execute,
    /// Each byte read: a bit for each byte, set by each operand read
    /// cycle a read hook over it would see.
    Read,
    /// Each byte written: a bit for each byte, set by each write cycle
    /// a write hook over it would see.
    Write,
}

impl CoverageKind {
    /// The kinds, in order, with their names.
    const NAMES: [(Self, &'static str); 3] = [
        (Self::Execute, "execute"),
        (Self::Read, "read"),
        (Self::Write, "write"),
    ];

    /// log2 of the bytes each bit stands for: a word for instructions,
    /// a byte for reads and writes.
    const fn shift(self) -> u32 {
        match self {
            Self::Execute => 1,
            Self::Read | Self::Write => 0,
        }
    }
}

/// `execute`, `read` or `write`.
impl fmt::Display for CoverageKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (_, name) = Self::NAMES
            .iter()
            .find(|(kind, _)| kind == self)
            .expect("every kind is named");
        formatter.write_str(name)
    }
}

/// `execute`, `read` or `write`, as [`Display`](fmt::Display) prints it.
impl FromStr for CoverageKind {
    type Err = ParseCoverageKindError;

    fn from_str(text: &str) -> Result<Self, ParseCoverageKindError> {
        Self::NAMES
            .iter()
            .find(|(_, name)| *name == text)
            .map(|&(kind, _)| kind)
            .ok_or_else(|| ParseCoverageKindError(text.to_owned()))
    }
}

/// Text that names no [`CoverageKind`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no coverage kind {0:?}: give execute, read or write")]
pub struct ParseCoverageKindError(String);

/// What a range of addresses saw executed, read or written
/// ([`Cpu::add_coverage`]): a bit for each word (execute) or byte (read,
/// write) of it, kept a page at a time, a page's bits allocated when the
/// first of them is set, so it holds what was recorded, not the range.
/// Host instrumentation, as the hooks are: not part of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    /// What it records.
    kind: CoverageKind,
    /// Its first and last addresses, on the address lines, the first
    /// even for [`CoverageKind::Execute`]; [`None`] for an empty range.
    bounds: Option<(Address, Address)>,
    /// The number of the page its first address is in.
    first: u32,
    /// Each run of [`Coverage::SLAB`] pages from `first`, allocated at
    /// its first record: each page's bits, allocated at its first
    /// record, the lowest bit of each `u64` the lowest address.
    slabs: Vec<Option<Box<[Option<PageBits>]>>>,
}

/// A page's bits: one for each byte, or each word, in the low half.
type PageBits = Box<[u64; Coverage::PAGE_WORDS]>;

impl Coverage {
    /// Bits in each `u64` of a page's.
    const BITS: u32 = u64::BITS;
    /// The `u64`s of a page's bits: one bit for each byte.
    const PAGE_WORDS: usize = (PAGE_BYTES / u64::BITS) as usize;
    /// Pages in a slab of the table.
    const SLAB: u32 = 1024;

    /// `kind` over `range`, nothing recorded: its start rounded up to a
    /// word for [`CoverageKind::Execute`], its end no further than
    /// `mask`, the address lines'; empty if its start is past its end
    /// or the lines.
    pub(crate) fn new(range: &RangeInclusive<Address>, mask: u32, kind: CoverageKind) -> Self {
        let step = 1 << kind.shift();
        let start = range.start().0.checked_next_multiple_of(step);
        let end = range.end().0.min(mask);
        let bounds = start
            .filter(|&start| start <= end)
            .map(|start| (Address(start), Address(end)));
        let (first, slabs) = bounds.map_or((0, 0), |(start, end)| {
            let (first, last) = (start.0 >> PAGE_SHIFT, end.0 >> PAGE_SHIFT);
            (first, (last - first) / Self::SLAB + 1)
        });
        Self {
            kind,
            bounds,
            first,
            slabs: vec![None; slabs as usize],
        }
    }

    /// What it records.
    #[must_use]
    pub const fn kind(&self) -> CoverageKind {
        self.kind
    }

    /// The addresses it covers: empty (its start past its end) if none.
    #[must_use]
    pub const fn range(&self) -> RangeInclusive<Address> {
        match self.bounds {
            Some((start, end)) => start..=end,
            None => Address(1)..=Address(0),
        }
    }

    /// The slab, the page in it and the bit's index in the page of
    /// `address`, if it covers it, and it is even for
    /// [`CoverageKind::Execute`].
    fn locate(&self, address: Address) -> Option<(usize, usize, u32)> {
        let (start, end) = self.bounds?;
        let shift = self.kind.shift();
        if address < start || address > end || address.0 & ((1 << shift) - 1) != 0 {
            return None;
        }
        let page = (address.0 >> PAGE_SHIFT) - self.first;
        Some((
            (page / Self::SLAB) as usize,
            (page % Self::SLAB) as usize,
            (address.0 % PAGE_BYTES) >> shift,
        ))
    }

    /// Records `address`, if it covers it.
    pub(crate) fn record(&mut self, address: Address) {
        let Some((slab, page, index)) = self.locate(address) else {
            return;
        };
        let pages = self.slabs[slab]
            .get_or_insert_with(|| vec![None; Self::SLAB as usize].into_boxed_slice());
        let bits = pages[page].get_or_insert_with(|| Box::new([0; Self::PAGE_WORDS]));
        bits[(index / Self::BITS) as usize] |= 1 << (index % Self::BITS);
    }

    /// Records each byte of `size` from `address`, on lines `mask`, it
    /// covers.
    pub(crate) fn record_bytes(&mut self, address: Address, size: Size, mask: u32) {
        for offset in 0..size.bytes() {
            self.record(Address(address.0.wrapping_add(offset) & mask));
        }
    }

    /// Whether `address` was recorded.
    #[must_use]
    pub fn contains(&self, address: Address) -> bool {
        self.locate(address).is_some_and(|(slab, page, index)| {
            self.slabs[slab]
                .as_ref()
                .and_then(|pages| pages[page].as_ref())
                .is_some_and(|bits| {
                    bits[(index / Self::BITS) as usize] & 1 << (index % Self::BITS) != 0
                })
        })
    }

    /// The addresses recorded, in order.
    pub fn addresses(&self) -> impl Iterator<Item = Address> + '_ {
        let shift = self.kind.shift();
        let pages = self.slabs.iter().enumerate().flat_map(|(slab, pages)| {
            pages.iter().flat_map(move |pages| {
                pages.iter().enumerate().filter_map(move |(page, bits)| {
                    bits.as_ref()
                        .map(|bits| (slab as u32 * Self::SLAB + page as u32, bits))
                })
            })
        });
        pages.flat_map(move |(page, bits)| {
            let base = (self.first + page) << PAGE_SHIFT;
            bits.iter().enumerate().flat_map(move |(word, &value)| {
                (0..Self::BITS)
                    .filter(move |bit| value & 1 << bit != 0)
                    .map(move |bit| Address(base | (word as u32 * Self::BITS + bit) << shift))
            })
        })
    }

    /// Forgets everything recorded.
    pub(crate) fn clear(&mut self) {
        self.slabs.fill(None);
    }

    /// The pages whose bits it holds.
    #[cfg(test)]
    pub(crate) fn pages_held(&self) -> usize {
        self.slabs
            .iter()
            .flatten()
            .map(|pages| pages.iter().flatten().count())
            .sum()
    }
}
