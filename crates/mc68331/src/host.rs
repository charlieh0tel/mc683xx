//! The host's own bus cycles, as a debugger makes them (BDM's memory
//! commands, CPU32RM 7).
//!
//! They go through the chip's decode, to the modules' registers at the
//! base SIMCR's MM puts them and to whatever a chip select reaches.  A
//! cycle is real, with its side effects, at the chip's time; a peek is
//! a look without them, where one can be had.

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::memory::Space;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::module;
use mc683xx_modules::module::Peek;
use mc683xx_modules::sim::BLOCK;
use mc683xx_modules::sim::MODULE_SPACE_BYTES;
use mc683xx_modules::sim::chip_selects::DecodeRequest;

use crate::board::Board;
use crate::board::load;
use crate::chip::Mc68331;

/// Bits in a word cycle, the most a cycle carries: a long is two.
const WORD_BITS: u32 = 16;

/// A byte a debugger's write cannot reach: not memory the core serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not memory the core serves")]
pub struct Unserved(pub Address);

/// Why a host's cycle or peek did not give a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AccessError {
    /// No module acknowledged the cycle and no chip select matched it:
    /// nothing answers there.
    #[error("nothing answers there")]
    Unmapped,
    /// The cycle ended in a bus error: a peripheral's BERR, a select
    /// waiting for a DSACK that never comes, the bus monitor.
    #[error("bus error")]
    BusError,
    /// A word or long at an odd address, which the CPU32 never makes
    /// (CPU32RM 6.2.3, address error).
    #[error("a word or long at an odd address")]
    Misaligned,
    /// A value wider than the cycle written.
    #[error("{0:#x} does not fit the cycle")]
    Width(u32),
    /// A peek where only a real cycle can tell: a peripheral, or a module
    /// register a read would change.
    #[error("not peekable")]
    Unpeekable,
}

impl Mc68331 {
    /// A read cycle of `size` at `address` in `function_code`'s space,
    /// now, as the CPU would make it: through the chip's decode, its
    /// side effects with it (a flag a read clears, a peripheral's read).  A
    /// long is two word cycles, high word first.  The chip's time does
    /// not move, the trace does not see it, and a stop a peripheral asks
    /// for in it is dropped, with no run to stop.
    ///
    /// # Errors
    ///
    /// [`AccessError::Misaligned`] for a word or long at an odd
    /// address, [`AccessError::Unmapped`] if nothing answers,
    /// [`AccessError::BusError`] if the cycle ends in a bus error.
    pub fn bus_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<u32, AccessError> {
        self.words(address, size)?
            .into_iter()
            .try_fold(0, |value, (at, size, shift)| {
                Ok(value | self.cycle(at, size, function_code, None)? << shift)
            })
    }

    /// A write cycle of `value`, right-aligned, as `bus_read`.  A write
    /// to the SIM's chip-select registers remaps at once.
    ///
    /// # Errors
    ///
    /// [`AccessError::Width`] if `value` is wider than `size`, and
    /// otherwise as `bus_read`.
    pub fn bus_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Result<(), AccessError> {
        if value & !size.mask() != 0 {
            return Err(AccessError::Width(value));
        }
        for (at, size, shift) in self.words(address, size)? {
            self.cycle(at, size, function_code, Some(value >> shift & size.mask()))?;
        }
        if self.board.decode_changed {
            self.board.rebuild(&mut self.cpu.memory);
        }
        Ok(())
    }

    /// What `bus_read` would return now, without a cycle: a module's
    /// register as it holds it (no flag armed or cleared), a memory's
    /// bytes through the decode.  A byte no memory alone answers, a
    /// peripheral's or one no cycle would end well, is
    /// [`AccessError::Unpeekable`].
    ///
    /// # Errors
    ///
    /// [`AccessError::Unpeekable`] as above, and otherwise as
    /// `bus_read`.
    pub fn peek(
        &self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<u32, AccessError> {
        self.words(address, size)?
            .into_iter()
            .try_fold(0, |value, (at, size, shift)| {
                Ok(value | self.peek_cycle(at, size, function_code)? << shift)
            })
    }

    /// Writes `data` at `address` up, with no bus cycle, a debugger's
    /// write: into the memories the core serves in supervisor space, as
    /// the decode now maps them, a ROM's included, whatever the pages'
    /// permissions.
    ///
    /// # Errors
    ///
    /// [`Unserved`] for a byte the core does not serve (a peripheral's,
    /// or one a peripheral shares); then nothing is written.
    pub fn poke(&mut self, address: Address, data: &[u8]) -> Result<(), Unserved> {
        let memory = &mut self.cpu.memory;
        let places = (0..data.len() as u32)
            .map(|offset| {
                let at = address.offset(offset);
                let page = memory.entry(Space::Supervisor, at);
                let backing = page.write.or(page.read).ok_or(Unserved(at))?;
                let within = memory.mask(at).0 % PAGE_BYTES;
                Ok((backing.region, (backing.offset + within) as usize))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for ((region, index), &byte) in places.into_iter().zip(data) {
            memory.region_mut(region)[index] = byte;
        }
        Ok(())
    }

    /// The byte or word cycles of an access, each on the address lines
    /// with its place in the value.
    fn words(&self, address: Address, size: Size) -> Result<Vec<(u32, Size, u32)>, AccessError> {
        if size != Size::Byte && address.is_odd() {
            return Err(AccessError::Misaligned);
        }
        let at = |by| self.cpu.memory.mask(address.offset(by)).0;
        Ok(match size {
            Size::Long => vec![(at(0), Size::Word, WORD_BITS), (at(2), Size::Word, 0)],
            _ => vec![(at(0), size, 0)],
        })
    }

    /// One byte or word cycle, now, leaving the stop request as it was.
    fn cycle(
        &mut self,
        address: u32,
        size: Size,
        function_code: FunctionCode,
        write: Option<u32>,
    ) -> Result<u32, AccessError> {
        let now = self.now;
        let asked = self.board.stop_request.take();
        let ended = self.board.cycle(
            Some(&mut self.cpu.memory),
            address,
            size,
            function_code,
            now,
            write,
        );
        self.board.stop_request.withdraw();
        if asked {
            self.board.stop_request.request();
        }
        ended.map(|(value, _)| value).map_err(|BusError| {
            let request = DecodeRequest {
                address,
                function_code,
                write: write.is_some(),
                size,
            };
            self.board.unanswered(request)
        })
    }

    /// One byte or word peek, now: a module's register if one would
    /// acknowledge it (MC68331UM 4.8), else memory's bytes.
    fn peek_cycle(
        &self,
        address: u32,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<u32, AccessError> {
        let board = &self.board;
        if !function_code.is_cpu_space()
            && let Some(offset) = address
                .checked_sub(board.sim.module_base())
                .filter(|&offset| offset < MODULE_SPACE_BYTES)
        {
            let access = |block: module::Block| module::RegisterAccess {
                offset: offset - block.offset,
                size,
                supervisor: function_code.is_supervisor(),
                now: self.now,
            };
            let peeked = if BLOCK.contains(offset) {
                board
                    .sim
                    .peek(access(BLOCK))
                    .map_or(Peek::Unacknowledged, Peek::Value)
            } else {
                board
                    .modules
                    .iter()
                    .find(|module| module.block().contains(offset))
                    .map_or(Peek::Unacknowledged, |module| {
                        module.peek(access(module.block()))
                    })
            };
            match peeked {
                Peek::Value(value) => return Ok(value),
                Peek::Unpeekable => return Err(AccessError::Unpeekable),
                Peek::Unacknowledged => {}
            }
        }
        let bytes = if size == Size::Byte { 1 } else { 2 };
        (address..address + bytes).try_fold(0, |value, at| {
            let byte = board.peek_byte(&self.cpu.memory, at, function_code)?;
            Ok(value << 8 | byte)
        })
    }
}

impl Board {
    /// Why a cycle `request` describes ended in a bus error: nothing
    /// matched it, or something answered it so.
    fn unanswered(&self, request: DecodeRequest) -> AccessError {
        if self.sim.chip_selects().matches(request).next().is_none() {
            AccessError::Unmapped
        } else {
            AccessError::BusError
        }
    }

    /// The byte at `address` as the memory that alone answers it holds
    /// it.
    fn peek_byte(
        &self,
        memory: &Memory,
        address: u32,
        function_code: FunctionCode,
    ) -> Result<u32, AccessError> {
        let request = DecodeRequest {
            address,
            function_code,
            write: false,
            size: Size::Byte,
        };
        match self.memory_alone(request) {
            Some((wired, _)) => Ok(load(memory, wired, address, Size::Byte)),
            None if self.sim.chip_selects().matches(request).next().is_none()
                && !self.bus_takes(address, function_code) =>
            {
                Err(AccessError::Unmapped)
            }
            None => Err(AccessError::Unpeekable),
        }
    }
}

#[cfg(test)]
mod tests;
