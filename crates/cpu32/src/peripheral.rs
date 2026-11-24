//! What a host maps on the CPU's bus around the core: a peripheral the
//! CPU reaches by address, the bare machine's at a byte range
//! ([`Machine::add_peripheral`]) and a chip's on its chip-select pins
//! (`mc683xx-mc68331`'s `Builder::peripheral`), with one trait for both.
//!
//! [`Machine::add_peripheral`]: crate::flat::Machine::add_peripheral

use std::fmt;

use crate::bus::BusError;
use crate::bus::PortSize;
use crate::cpu::state::Save;
use crate::cycles::Cycle;
use crate::cycles::Cycles;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

/// A chip-select pin by the chip's own number for it: which select
/// carried a cycle.  The chip names its pins
/// (`mc683xx-modules`'s `ChipSelect` converts to and from it); the bare
/// machine has none.  Every number is one, as the core sees it, so its
/// field is public, as [`Address`]'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Select(pub u8);

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
    pub address: Address,
    /// Byte, word or long.  On a chip, byte or word after dynamic bus
    /// sizing to the port: the select's field, or with an external
    /// DSACK the peripheral's DSACK pins'.  On the bare machine a long
    /// operand comes as one cycle when the peripheral covers all four
    /// bytes.
    pub size: Size,
    /// The cycle's function code.
    pub function_code: FunctionCode,
    /// Whether it is a write: R/W low (SIMRM 5.1.5).  Set before the
    /// chip asks `wait_states` and `dsack_port`, so a peripheral that
    /// times or sizes its writes apart from its reads sees which.
    pub write: bool,
    /// When it starts (on the bare machine, when its instruction
    /// started), on the system clock.
    pub now: Cycle,
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
    /// A read cycle at `address`, of `size` and `function_code`,
    /// starting at `now`, on no select and not stacking.
    #[must_use]
    pub const fn new(
        address: Address,
        size: Size,
        function_code: FunctionCode,
        now: Cycle,
    ) -> Self {
        Self {
            address,
            size,
            function_code,
            write: false,
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

    /// This cycle, a write or a read.
    #[must_use]
    pub const fn writing(self, write: bool) -> Self {
        Self { write, ..self }
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
/// ([`FunctionCode::CpuSpace`]) on a select programmed for it (SIMRM
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
    fn wait_states(&mut self, cycle: BusCycle) -> Option<Cycles> {
        let _ = cycle;
        Some(Cycles::ZERO)
    }

    /// With an external DSACK, the port this cycle's DSACK gives, for a
    /// peripheral whose pins differ from cycle to cycle: 16 bits on
    /// DSACK1, 8 on DSACK0 alone (SIMRM Table 5-2).  Asked with
    /// `wait_states`.  By default `None`: the pins the board gives the
    /// peripheral (`mc683xx-mc68331`'s `Builder::set_dsack`) say.
    fn dsack_port(&mut self, cycle: BusCycle) -> Option<PortSize> {
        let _ = cycle;
        None
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
    fn reset(&mut self, now: Cycle) {
        let _ = now;
    }

    /// What a read `cycle` would return now, right-aligned, with none of
    /// its side effects (a flag a read clears, a FIFO a read pops): a
    /// debugger's look, as the bare machine's
    /// [`read`](crate::flat::Machine::read), a chip's `peek` and gdb's
    /// memory reads take it, a byte at a time.  By default [`None`]:
    /// only a real cycle can tell, and the look fails as unpeekable.
    fn peek(&self, cycle: BusCycle) -> Option<u32> {
        let _ = cycle;
        None
    }

    /// Stores `value`, right-aligned, where a write `cycle` would, with
    /// none of its side effects (a command it starts, a transmitter it
    /// loads): a debugger's poke, as the bare machine's
    /// [`write`](crate::flat::Machine::write), a chip's `poke` and gdb's
    /// memory writes make it, a byte at a time.  Whether it took it; by
    /// default it declines, and the poke fails as unpeekable.
    fn poke(&mut self, cycle: BusCycle, value: u32) -> bool {
        let _ = (cycle, value);
        false
    }
}

/// A mapped peripheral, as the bare machine's
/// [`add_peripheral`](crate::flat::Machine::add_peripheral) and a
/// chip's `Builder::peripheral` return it: a host receives one and
/// gives it back, and cannot make one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeripheralId(u64);

impl PeripheralId {
    /// The id numbered `number` by whatever added the peripheral.
    ///
    /// Not part of the semver API: for the workspace's crates that add
    /// peripherals.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_number(number: u64) -> Self {
        Self(number)
    }

    /// Its number, as [`from_number`](Self::from_number) took it.
    ///
    /// Not part of the semver API: for the workspace's crates that add
    /// peripherals.
    #[doc(hidden)]
    #[must_use]
    pub const fn number(self) -> u64 {
        self.0
    }
}

/// `peripheral` and its number: `peripheral 3`.
impl fmt::Display for PeripheralId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "peripheral {}", self.0)
    }
}
