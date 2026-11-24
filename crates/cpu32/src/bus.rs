//! The external bus: every cycle the core does not serve from its own
//! memory goes here.

use crate::BusError;
use crate::PortSize;
use crate::cycles::Cycle;
use crate::cycles::Cycles;
use crate::hooks::ExceptionEntry;
use crate::memory::Memory;
use crate::memory::Page;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

/// Clocks in the bus cycle the timing tables assume: two clocks, no
/// wait states, a 16-bit port (CPU32RM 8.1.5, 8.3).
const TABLE_CLOCKS: u64 = 2;

/// The LPSTOP broadcast: a CPU space write of the interrupt mask to
/// $3FFFE, type 0011 (MC68331UM 4.5.4.2, Figure 4-13; CPU32RM
/// 5.3.1.4).
pub const LPSTOP_BROADCAST: Address = Address(0x3_fffe);

/// One bus cycle.
///
/// Built with [`BusAccess::new`] outside the core (a test of a
/// [`Bus`]), so a field added later gets a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BusAccess {
    /// Its address.
    pub address: Address,
    /// Byte or word.  The core splits a long operand into two word cycles,
    /// high word first, as the CPU32 does on its 16-bit bus; a [`Bus`] sees
    /// [`Size::Long`] only in [`read_long`](Bus::read_long) and
    /// [`write_long`](Bus::write_long).
    pub size: Size,
    /// Its address space.
    pub function_code: FunctionCode,
    /// When it starts, on the board's clock.
    pub now: Cycle,
}

impl BusAccess {
    /// A cycle at `address`, of `size` and `function_code`, starting at
    /// `now`.
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
            now,
        }
    }
}

/// How a bus cycle that read ended.
///
/// Built with [`Read::new`], so a field added later gets a default
/// and no host's code changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Read {
    /// The value, right-aligned.
    pub value: u32,
    /// The clocks it took beyond the timing tables' two-clock cycle
    /// (see [`stall`]).
    pub stall: Cycles,
}

impl Read {
    /// A read of `value` with no wait states.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self {
            value,
            stall: Cycles::ZERO,
        }
    }

    /// This read, taking `stall` clocks beyond two.
    #[must_use]
    pub const fn stalled(self, stall: Cycles) -> Self {
        Self { stall, ..self }
    }
}

/// The external bus, as the chip and the host's peripherals answer it.
pub trait Bus {
    /// A read cycle.
    ///
    /// # Errors
    ///
    /// [`BusError`] if the cycle ends in a bus error (BERR): the core
    /// takes a bus error exception (CPU32RM 6.2.3).
    fn read(&mut self, access: BusAccess) -> Result<Read, BusError>;
    /// A write cycle of `value`, right-aligned; the clocks it took
    /// beyond the timing tables' two-clock cycle.
    ///
    /// # Errors
    ///
    /// [`BusError`] if the cycle ends in a bus error (BERR): the core
    /// takes a bus error exception (CPU32RM 6.2.3).
    fn write(&mut self, access: BusAccess, value: u32) -> Result<Cycles, BusError>;

    /// A read cycle, with the core's memory at hand: a chip whose
    /// address decode builds the page tables serves the regions they
    /// cannot (a select shared with a peripheral) and rebuilds the tables
    /// when its decode changes.  The core calls this one; by default
    /// it is [`read`](Self::read).
    ///
    /// # Errors
    ///
    /// As [`Bus::read`].
    fn read_with_memory(
        &mut self,
        access: BusAccess,
        memory: &mut Memory,
    ) -> Result<Read, BusError> {
        let _ = memory;
        self.read(access)
    }

    /// A write cycle, with the core's memory at hand, as
    /// `read_with_memory`.  The core calls this one; by default it is
    /// [`write`](Self::write).
    ///
    /// # Errors
    ///
    /// As [`Bus::write`].
    fn write_with_memory(
        &mut self,
        access: BusAccess,
        value: u32,
        memory: &mut Memory,
    ) -> Result<Cycles, BusError> {
        let _ = memory;
        self.write(access, value)
    }

    /// The `reset` instruction asserted RESET at `now` (CPU32RM 4,
    /// RESET; SIMRM 8.2.6): the chip resets its external peripherals and
    /// its modules other than the CPU.
    fn reset_instruction(&mut self, now: Cycle) {
        let _ = now;
    }

    /// The CPU entered an exception in the instruction or interrupt
    /// entry that started at `now`, for the chip's trace.  By default
    /// nothing.
    fn exception_entered(&mut self, entry: &ExceptionEntry, now: Cycle) {
        let _ = (entry, now);
    }

    /// A long read in one access at `access.address`, whose first word
    /// memory does not serve: [`None`] to have it run as two word cycles,
    /// as the CPU32 does on its 16-bit bus, which is the default.  A
    /// bare host's peripherals take a long whole; a chip never does.
    fn read_long(&mut self, access: BusAccess) -> Option<Result<Read, BusError>> {
        let _ = access;
        None
    }

    /// A long write in one access, as [`read_long`](Self::read_long); the
    /// clocks it took beyond the timing tables'.
    fn write_long(&mut self, access: BusAccess, value: u32) -> Option<Result<Cycles, BusError>> {
        let _ = (access, value);
        None
    }

    /// Exception processing starts or ends stacking a frame: the write
    /// cycles between are its.  By default nothing.
    fn set_stacking(&mut self, stacking: bool) {
        let _ = stacking;
    }

    /// Asked after each cycle it ends, well or in a bus error, after
    /// [`reset_instruction`](Self::reset_instruction), and after an
    /// interrupt acknowledge ([`Interrupts::acknowledge`](crate::interrupt::Interrupts::acknowledge)):
    /// whether the host wants the run to stop once the instruction, or
    /// the interrupt's entry, ends
    /// ([`Stop::Requested`](crate::cpu::Stop::Requested)), as a peripheral that
    /// failed in a way BERR cannot say does.  By default never.
    fn stop_requested(&mut self) -> bool {
        false
    }
}

/// The clocks a byte or word cycle of `size` takes beyond the timing
/// tables' assumption, on a `port` whose cycles take `clocks` each, at
/// most a page's ([`Page::MOST_CLOCKS`]).
///
/// [`Page::MOST_CLOCKS`]: crate::memory::Page::MOST_CLOCKS
#[must_use]
pub fn stall(size: Size, port: PortSize, clocks: Cycles) -> Cycles {
    debug_assert!(clocks <= Page::MOST_CLOCKS, "more clocks than a page holds");
    let cycles = match (size, port) {
        (Size::Word, PortSize::Bits8) => 2,
        _ => 1,
    };
    Cycles::from_clocks((cycles * clocks.clocks()).saturating_sub(TABLE_CLOCKS))
}

#[cfg(test)]
mod tests {
    use super::BusAccess;
    use super::PortSize;
    use super::stall;
    use crate::cycles::Cycle;
    use crate::cycles::Cycles;
    use mc683xx_target::Address;
    use mc683xx_target::FunctionCode;
    use mc683xx_target::Size;

    #[test]
    fn an_access_holds_what_it_was_built_with() {
        let access = BusAccess::new(
            Address(0x400),
            Size::Word,
            FunctionCode::SupervisorProgram,
            Cycle::from_clocks(7),
        );
        assert_eq!(access.address, Address(0x400));
        assert_eq!(access.size, Size::Word);
        assert_eq!(access.function_code, FunctionCode::SupervisorProgram);
        assert_eq!(access.now, Cycle::from_clocks(7));
    }

    #[test]
    fn a_two_clock_word_costs_what_the_tables_say() {
        assert_eq!(
            stall(Size::Word, PortSize::Bits16, Cycles::from_clocks(2)),
            Cycles::ZERO
        );
    }

    #[test]
    fn a_word_on_an_8_bit_port_is_two_cycles() {
        assert_eq!(
            stall(Size::Word, PortSize::Bits8, Cycles::from_clocks(3)),
            Cycles::from_clocks(4)
        );
        assert_eq!(
            stall(Size::Byte, PortSize::Bits8, Cycles::from_clocks(3)),
            Cycles::from_clocks(1)
        );
    }
}
