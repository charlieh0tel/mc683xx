//! The external bus: every cycle the core does not serve from its own
//! memory goes here.

use std::cell::Cell;
use std::rc::Rc;

use crate::address::Address;
use crate::cpu::hooks::ExceptionEntry;
use crate::cycles::Cycles;
use crate::function_code::FunctionCode;
use crate::memory::Memory;
use crate::size::Size;

/// Clocks in the bus cycle the timing tables assume: two clocks, no
/// wait states, a 16-bit port (CPU32RM 8.1.5, 8.3).
const TABLE_CLOCKS: u64 = 2;

/// The LPSTOP broadcast: a CPU space write of the interrupt mask to
/// $3FFFE, type 0011 (MC68331UM 4.5.4.2, Figure 4-13; CPU32RM
/// 5.3.1.4).
pub const LPSTOP_BROADCAST: Address = Address(0x3_fffe);

/// One bus cycle.
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
    pub now: Cycles,
}

/// How a bus cycle that read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Read {
    /// The value, right-aligned.
    pub value: u32,
    /// The clocks it took beyond the timing tables' two-clock cycle
    /// (see [`stall`]).
    pub stall: Cycles,
}

/// A handle by which the host asks a run to stop.
///
/// The host's peripherals, or anything else of the host's, stop the run
/// after the instruction whose cycle asked, or, on the chip, at the
/// sync point if no instruction is running
/// ([`Stop::Requested`](crate::cpu::Stop::Requested)).  Clones share
/// one request, which the stop clears.  The bare machine and the chip
/// each give one.
#[derive(Debug, Clone, Default)]
pub struct StopRequest(Rc<Cell<bool>>);

impl StopRequest {
    /// Asks the run to stop.
    pub fn request(&self) {
        self.0.set(true);
    }

    /// Withdraws a request the run has not taken: for a host whose own
    /// run loop stopped for it first, between runs.
    pub fn withdraw(&self) {
        self.0.set(false);
    }

    /// Whether a stop is asked for, clearing the request: for the run
    /// loop that serves it ([`Bus::stop_requested`]).
    #[must_use = "a stop taken and dropped is lost"]
    pub fn take(&self) -> bool {
        self.0.replace(false)
    }
}

/// The cycle ended in a bus error (BERR, CPU32RM 6.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bus error")]
pub struct BusError;

/// The width of the port a cycle reaches, which DSACK reports and the
/// CPU32 sizes its cycles to (MC68331UM 4.4.2, dynamic bus sizing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PortSize {
    /// An 8-bit port: a word takes two byte cycles.
    Bits8,
    /// A 16-bit port.
    Bits16,
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
    fn reset_instruction(&mut self, now: Cycles) {
        let _ = now;
    }

    /// The CPU entered an exception in the instruction or interrupt
    /// entry that started at `now`, for the chip's trace.  By default
    /// nothing.
    fn exception_entered(&mut self, entry: &ExceptionEntry, now: Cycles) {
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
    fn stacking(&mut self, stacking: bool) {
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
/// tables' assumption, on a `port` whose cycles take `clocks` each.
#[must_use]
pub fn stall(size: Size, port: PortSize, clocks: u8) -> Cycles {
    let cycles = match (size, port) {
        (Size::Word, PortSize::Bits8) => 2,
        _ => 1,
    };
    Cycles((cycles * u64::from(clocks)).saturating_sub(TABLE_CLOCKS))
}

#[cfg(test)]
mod tests {
    use super::PortSize;
    use super::stall;
    use crate::cycles::Cycles;
    use crate::size::Size;

    #[test]
    fn a_two_clock_word_costs_what_the_tables_say() {
        assert_eq!(stall(Size::Word, PortSize::Bits16, 2), Cycles::ZERO);
    }

    #[test]
    fn a_word_on_an_8_bit_port_is_two_cycles() {
        assert_eq!(stall(Size::Word, PortSize::Bits8, 3), Cycles(4));
        assert_eq!(stall(Size::Byte, PortSize::Bits8, 3), Cycles(1));
    }
}
