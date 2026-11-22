//! What the chip tells the host of as it runs.

use std::fmt;

use mc683xx_cpu32::cpu::hooks::ExceptionEntry;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::sim::ResetCause;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

/// Something the chip did that the host may want to know of, each at
/// its time.  More kinds may come, and a kind more fields: a host's
/// match needs a wildcard arm, and its patterns `..`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// The chip reset.
    #[non_exhaustive]
    Reset {
        /// When.
        now: Cycles,
        /// Why.
        cause: ResetCause,
    },
    /// Two interrupt sources of one level had the same nonzero IARB
    /// (SIMRM 6.3: "unpredictable"); the fixed order SIM, then the
    /// modules in order, then the IRQ pins, chose ([PLAN.md]).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    #[non_exhaustive]
    ArbitrationTie {
        /// When the level was acknowledged.
        now: Cycles,
        /// The level acknowledged.
        level: InterruptLevel,
        /// The IARB value they shared.
        arbitration: u8,
    },
    /// A bus cycle the core sent to the chip, while
    /// [`ChipTracing::accesses`] is set: one it could not serve from its
    /// own memory, a module register's, a peripheral's, or host memory a
    /// peripheral shares a select with.  RAM and ROM the core serves are
    /// not traced; memory hooks see those.
    #[non_exhaustive]
    Access {
        /// When its instruction started.
        now: Cycles,
        /// Its address, on the address lines.
        address: Address,
        /// Byte or word.
        size: Size,
        /// Its function code.
        function_code: FunctionCode,
        /// Whether it wrote.
        write: bool,
        /// The value read or written, right-aligned; `None` for a read
        /// that ended in a bus error.
        value: Option<u32>,
        /// Whether it ended in a bus error.
        bus_error: bool,
    },
    /// The CPU entered an exception, while [`ChipTracing::exceptions`] is
    /// set.
    #[non_exhaustive]
    Exception {
        /// When the instruction or interrupt entry it ended started.
        now: Cycles,
        /// The exception.
        entry: ExceptionEntry,
    },
    /// A peripheral's DSACK pins
    /// ([`Builder::set_dsack`](crate::chip::Builder::set_dsack)) disagree with
    /// the chip on an externally terminated cycle: told once for each
    /// peripheral and kind of fault, what was told kept in the chip's
    /// snapshot.
    #[non_exhaustive]
    Dsack {
        /// When.
        now: Cycles,
        /// How.
        fault: DsackFault,
    },
}

impl Event {
    /// When it happened.
    #[must_use]
    pub const fn now(&self) -> Cycles {
        match *self {
            Self::Reset { now, .. }
            | Self::ArbitrationTie { now, .. }
            | Self::Access { now, .. }
            | Self::Exception { now, .. }
            | Self::Dsack { now, .. } => now,
        }
    }
}

/// What happened, without its time: `reset (power-on)`.
impl fmt::Display for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reset { cause, .. } => write!(formatter, "reset ({cause})"),
            Self::ArbitrationTie {
                level, arbitration, ..
            } => write!(formatter, "arbitration tie at {level}, IARB {arbitration}"),
            Self::Access {
                address,
                size,
                function_code,
                write,
                value,
                ..
            } => {
                let direction = if *write { "write" } else { "read" };
                write!(
                    formatter,
                    "{size} {direction} at {address}, FC {function_code}"
                )?;
                match value {
                    Some(value) => write!(formatter, ": {value:#x}"),
                    None => formatter.write_str(": bus error"),
                }
            }
            Self::Exception { entry, .. } => {
                write!(formatter, "exception, vector {}", entry.vector)
            }
            Self::Dsack { fault, .. } => write!(formatter, "DSACK fault: {fault}"),
        }
    }
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
    /// The same for a peripheral on no select
    /// ([`Builder::bus_peripheral`](crate::chip::Builder::bus_peripheral)).
    BusUnassigned {
        /// The peripheral.
        peripheral: PeripheralId,
    },
}

/// The fault: `PEPAR assigns no DSACK pin of CS3's peripheral`.
impl fmt::Display for DsackFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unassigned { select } => {
                write!(
                    formatter,
                    "PEPAR assigns no DSACK pin of {select}'s peripheral"
                )
            }
            Self::BusUnassigned { .. } => {
                formatter.write_str("PEPAR assigns no DSACK pin of a bus peripheral")
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::cycles::Cycles;
    use mc683xx_modules::chip_select::ChipSelect;
    use mc683xx_modules::sim::ResetCause;
    use mc683xx_target::address::Address;
    use mc683xx_target::function_code::FunctionCode;
    use mc683xx_target::size::Size;

    use super::DsackFault;
    use super::Event;

    #[test]
    fn an_event_prints_what_happened_and_tells_when() {
        let now = Cycles::from_clocks(7);
        let reset = Event::Reset {
            now,
            cause: ResetCause::PowerOn,
        };
        assert_eq!(reset.now(), now);
        assert_eq!(reset.to_string(), "reset (power-on)");
        let access = Event::Access {
            now,
            address: Address(0x10),
            size: Size::Word,
            function_code: FunctionCode::SUPERVISOR_DATA,
            write: false,
            value: None,
            bus_error: true,
        };
        assert_eq!(access.now(), now);
        assert_eq!(
            access.to_string(),
            "word read at 0x00000010, FC 5: bus error"
        );
        let dsack = Event::Dsack {
            now,
            fault: DsackFault::Unassigned {
                select: ChipSelect::Boot,
            },
        };
        assert_eq!(
            dsack.to_string(),
            "DSACK fault: PEPAR assigns no DSACK pin of CSBOOT's peripheral"
        );
    }
}
