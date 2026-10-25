//! What a host maps on the CPU's bus around the core: a peripheral the
//! CPU reaches by address, the bare machine's at a byte range
//! ([`Machine::add_peripheral`]) and a chip's on its chip-select pins
//! (`mc683xx-mc68331`'s `Builder::peripheral`), with one trait for both.
//!
//! [`Machine::add_peripheral`]: crate::flat::Machine::add_peripheral

use crate::bus::BusError;
use crate::cpu::state::Save;
use crate::cycles::Cycles;
use crate::function_code::FunctionCode;
use crate::size::Size;

/// A chip-select pin by the chip's own number for it: which select
/// carried a cycle.  The chip names its pins
/// (`mc683xx-modules`'s `ChipSelect` converts to and from it); the bare
/// machine has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Select(u8);

impl From<u8> for Select {
    fn from(number: u8) -> Self {
        Self(number)
    }
}

impl From<Select> for u8 {
    fn from(select: Select) -> Self {
        select.0
    }
}

/// A bus cycle as a mapped peripheral sees it.
///
/// Built with [`BusCycle::new`], so a field added later gets a default
/// and no host's code changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BusCycle {
    /// The address: all 32 bits on the bare machine, ADDR (the chip's
    /// address lines) on a chip.  Board decode within a select's window
    /// is the peripheral's: it reads these.
    pub address: u32,
    /// Byte, word or long.  On a chip, byte or word after dynamic bus
    /// sizing to the port: the select's field, or with an external
    /// DSACK the peripheral's DSACK pins'.  On the bare machine a long
    /// operand comes as one cycle when the peripheral covers all four
    /// bytes.
    pub size: Size,
    /// The cycle's function code.
    pub function_code: FunctionCode,
    /// When it starts (on the bare machine, when its instruction
    /// started), on the system clock.
    pub now: Cycles,
    /// The chip select asserted, on a chip: a peripheral wired to
    /// several (separate read and write selects, byte-lane selects)
    /// sees which.  [`None`] on the bare machine.
    pub select: Option<Select>,
    /// Whether exception processing is stacking a frame, on the bare
    /// machine.  A chip leaves it false: telling its peripherals would
    /// cost every run a borrow of its bus (docs/performance.md).
    pub stacking: bool,
}

impl BusCycle {
    /// A cycle at `address`, of `size` and `function_code`, starting at
    /// `now`, on no select and not stacking.
    #[must_use]
    pub const fn new(address: u32, size: Size, function_code: FunctionCode, now: Cycles) -> Self {
        Self {
            address,
            size,
            function_code,
            now,
            select: None,
            stacking: false,
        }
    }

    /// This cycle, carried by `select`.
    #[must_use]
    pub const fn through(self, select: Select) -> Self {
        Self {
            select: Some(select),
            ..self
        }
    }

    /// This cycle, stacking a frame or not.
    #[must_use]
    pub const fn while_stacking(self, stacking: bool) -> Self {
        Self { stacking, ..self }
    }
}

/// A host peripheral the CPU reaches by address.
///
/// On a chip it is wired to chip-select pins, and the chip times the
/// cycle from the select's option register (SIMRM 7.10.3); an interrupt
/// acknowledge cycle reaches it as a CPU-space byte read
/// ([`FunctionCode::CPU_SPACE`]) on a select programmed for it (SIMRM
/// 7.8.1), and the vector is the value it returns.  On the bare
/// machine it covers a byte range and sees `read` and `write` alone:
/// the machine has no DSACK, no IRQ wiring and no RESET pin, so the
/// other methods' defaults stand.
///
/// It says whether it saves ([`Save`]), so a snapshot that holds it
/// holds it or names it.
pub trait MappedPeripheral: Save {
    /// A read cycle: its value, right-aligned.
    ///
    /// # Errors
    ///
    /// [`BusError`] if the peripheral asserts BERR: the cycle ends in a
    /// bus error.
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError>;

    /// A write cycle of `value`, right-aligned, before it lands
    /// anywhere.
    ///
    /// # Errors
    ///
    /// [`BusError`] if the peripheral asserts BERR, as for `read`.
    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError>;

    /// On a select whose DSACK field is external (%1111, SIMRM
    /// 7.10.3), the wait states before the peripheral asserts DSACK, or
    /// `None` if it never does and the bus monitor ends the cycle.
    /// Asked before the cycle's `read` or `write`.  By default it
    /// answers at once.
    fn wait_states(&mut self, cycle: BusCycle) -> Option<u8> {
        let _ = cycle;
        Some(0)
    }

    /// Whether its interrupt output is asserted now; the board wires
    /// the output to an IRQ pin.  By default it has none.
    ///
    /// It may change only at the board's events: a bus cycle that
    /// reaches the board (any of the peripheral's own), an interrupt
    /// acknowledge, `advance`, `reset`, the host's change of the
    /// peripheral through the chip (its `peripheral_mut`), or between
    /// runs.  The chip caches the level it requests of the CPU between
    /// them, so a request that moves on its own, from host state shared
    /// through a [`Cell`](std::cell::Cell) say, is not seen until the
    /// next.
    fn interrupt_request(&self) -> bool {
        false
    }

    /// RESET asserted at `now`: a reset of the chip, or the `reset`
    /// instruction (SIMRM 8.2).  By default nothing changes.
    fn reset(&mut self, now: Cycles) {
        let _ = now;
    }
}

/// A mapped peripheral, as the bare machine's
/// [`add_peripheral`](crate::flat::Machine::add_peripheral) and a
/// chip's `Builder::peripheral` return it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeripheralId(u64);

impl From<u64> for PeripheralId {
    fn from(number: u64) -> Self {
        Self(number)
    }
}

impl From<PeripheralId> for u64 {
    fn from(id: PeripheralId) -> Self {
        id.0
    }
}
