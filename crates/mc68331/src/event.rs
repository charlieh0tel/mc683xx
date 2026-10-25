//! What the chip tells the host of as it runs.

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::hooks::ExceptionEntry;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::sim::ResetCause;

/// Something the chip did that the host may want to know of.  More
/// kinds may come: a host's match needs a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// The chip reset, at this time, for this cause.
    Reset(Cycles, ResetCause),
    /// Two interrupt sources of one level had the same nonzero IARB
    /// (SIMRM 6.3: "unpredictable"); the fixed order SIM, then the
    /// modules in order, then the IRQ pins, chose ([PLAN.md]).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    ArbitrationTie {
        /// The level acknowledged.
        level: InterruptLevel,
        /// The IARB value they shared.
        arbitration: u8,
    },
    /// A bus cycle the core sent to the chip, while [`ChipTracing::accesses`]
    /// is set.
    Access(TracedAccess),
    /// The CPU entered an exception in the instruction or interrupt
    /// entry that started at this time, while
    /// [`ChipTracing::exceptions`] is set.
    Exception(Cycles, ExceptionEntry),
    /// A peripheral's DSACK pins
    /// ([`Builder::dsack`](crate::chip::Builder::dsack)) disagree with
    /// the chip on an externally terminated cycle at this time: told
    /// once for each peripheral and kind of fault, what was told kept in
    /// the chip's snapshot.
    Dsack(Cycles, DsackFault),
}

/// How a peripheral's DSACK pins disagree with the chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DsackFault {
    /// PEPAR assigns none of the pins it drives as DSACK (SIMRM 9.1,
    /// Table 9-1): they are port E pins, so its DSACK cannot end the
    /// cycle, and the bus monitor does.
    Unassigned {
        /// The select whose cycle it was.
        select: ChipSelect,
    },
}

/// A bus cycle the core could not serve from its own memory: a module
/// register's, a peripheral's, or host memory a peripheral shares a select
/// with.
///
/// RAM and ROM the core serves are not traced; memory hooks see those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TracedAccess {
    /// When its instruction started.
    pub now: Cycles,
    /// Its address, on the address lines.
    pub address: Address,
    /// Byte or word.
    pub size: Size,
    /// Its function code.
    pub function_code: FunctionCode,
    /// Whether it wrote.
    pub write: bool,
    /// The value read or written, right-aligned; `None` for a read
    /// that ended in a bus error.
    pub value: Option<u32>,
    /// Whether it ended in a bus error.
    pub bus_error: bool,
}

/// What the chip adds to its events beyond resets and IARB ties, all
/// off by default: each costs a little on every cycle or exception.
///
/// It may gain fields: build one from [`ChipTracing::default()`] and set
/// those it needs (`let mut x = ChipTracing::default(); x.accesses = ...;`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChipTracing {
    /// Each bus cycle that reaches the chip ([`Event::Access`]).
    pub accesses: bool,
    /// Each exception entered ([`Event::Exception`]).
    pub exceptions: bool,
}
