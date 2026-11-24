//! The host's own bus cycles, as a debugger makes them (BDM's memory
//! commands, CPU32RM 7).
//!
//! They go through the chip's decode, to the modules' registers at the
//! base SIMCR's MM puts them and to whatever a chip select reaches.  A
//! cycle is real, with its side effects, at the chip's time; a peek is
//! a look without them, where one can be had.

use mc683xx_cpu32::BusError;
use mc683xx_cpu32::Cycle;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::memory::Unserved;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::Select;
use mc683xx_modules::module;
use mc683xx_modules::module::Peek;
use mc683xx_modules::sim::BLOCK;
use mc683xx_modules::sim::DecodeRequest;
use mc683xx_modules::sim::Lanes;
use mc683xx_modules::sim::MODULE_SPACE_BYTES;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

use crate::board::Board;
use crate::board::HostMemory;
use crate::board::Wire;
use crate::board::load;
use crate::board::store;
use crate::chip::Mc68331;

/// Bits in a word cycle, the most a cycle carries: a long is two.
const WORD_BITS: u32 = 16;

/// Why a host's bus cycle or peek did not give a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BusCycleError {
    /// No module acknowledged the cycle and no chip select matched it:
    /// nothing answers at the address, the cycle's on the address
    /// lines.
    #[error("no memory at {0}")]
    Unserved(Address),
    /// The cycle at the address ended in a bus error: a peripheral's
    /// BERR, a select waiting for a DSACK that never comes, the bus
    /// monitor.
    #[error("bus error at {0}")]
    BusError(Address),
    /// A word or long at an odd address, which the CPU32 never makes
    /// (CPU32RM 6.2.3, address error).
    #[error("a word or long at odd address {0}")]
    Misaligned(Address),
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
    /// [`BusCycleError::Misaligned`] for a word or long at an odd
    /// address, [`BusCycleError::Unserved`] if nothing answers,
    /// [`BusCycleError::BusError`] if the cycle ends in a bus error.
    pub fn bus_read(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<u32, BusCycleError> {
        self.words(address, size)?
            .try_fold(0, |value, (at, size, shift)| {
                Ok(value | self.cycle(at, size, function_code, None)? << shift)
            })
    }

    /// A write cycle of `value`, right-aligned, as `bus_read`.  A write
    /// to the SIM's chip-select registers remaps at once.
    ///
    /// # Errors
    ///
    /// [`BusCycleError::Width`] if `value` is wider than `size`, and
    /// otherwise as `bus_read`.
    pub fn bus_write(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Result<(), BusCycleError> {
        if value & !size.mask() != 0 {
            return Err(BusCycleError::Width(value));
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
    /// register as it holds it (no flag armed or cleared), a word at a
    /// time, and elsewhere the bytes [`peek_bytes`](Self::peek_bytes)
    /// finds.  A byte a peripheral declines, or one a matching select
    /// reaches nothing through, is [`BusCycleError::Unpeekable`].
    ///
    /// # Errors
    ///
    /// [`BusCycleError::Unpeekable`] as above, and otherwise as
    /// `bus_read`.
    pub fn peek(
        &self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<u32, BusCycleError> {
        let words = self.words(address, size)?;
        let bytes = size.bytes();
        let modules = (0..bytes).any(|index| {
            self.module_offset(self.cpu.memory.mask(address.offset(index)).0, function_code)
                .is_some()
        });
        if modules {
            return words.into_iter().try_fold(0, |value, (at, size, shift)| {
                Ok(value | self.peek_cycle(at, size, function_code)? << shift)
            });
        }
        let mut data = [0; 4];
        self.peek_bytes(address, function_code, &mut data[..bytes as usize])?;
        Ok(data[..bytes as usize]
            .iter()
            .fold(0, |value, &byte| value << 8 | u32::from(byte)))
    }

    /// Stores `value`, right-aligned, where `bus_write` would, without a
    /// cycle, as [`poke_bytes`](Self::poke_bytes) stores its bytes,
    /// high byte first.
    ///
    /// # Errors
    ///
    /// [`BusCycleError::Width`] if `value` is wider than `size`, and
    /// otherwise as `poke_bytes` and `bus_read`.
    pub fn poke(
        &mut self,
        address: Address,
        size: Size,
        function_code: FunctionCode,
        value: u32,
    ) -> Result<(), BusCycleError> {
        if value & !size.mask() != 0 {
            return Err(BusCycleError::Width(value));
        }
        if size != Size::Byte && address.is_odd() {
            return Err(BusCycleError::Misaligned(address));
        }
        let bytes = value.to_be_bytes();
        self.poke_bytes(
            address,
            function_code,
            &bytes[(4 - size.bytes()) as usize..],
        )
    }

    /// Fills `data` from `address` up, without a cycle: a module's
    /// register's bytes as it holds them, a byte at a time, a memory's
    /// bytes through the decode, and a peripheral's from its
    /// [`MappedPeripheral::peek`], one call for each run of bytes it
    /// would be first to drive.  Each byte is on the address lines.
    ///
    /// # Errors
    ///
    /// [`BusCycleError::Unpeekable`] for a register a read would change,
    /// a run a peripheral declines or a byte a matching select reaches
    /// nothing through, [`BusCycleError::Unserved`] for one nothing
    /// answers; `data` is then filled up to the byte or run.
    ///
    /// [`MappedPeripheral::peek`]: mc683xx_cpu32::peripheral::MappedPeripheral::peek
    pub fn peek_bytes(
        &self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<(), BusCycleError> {
        let mut done = 0;
        while done < data.len() {
            let at = self.cpu.memory.mask(address.offset(done as u32)).0;
            if self.module_offset(at, function_code).is_some()
                && let Some(value) = self.peek_register(at, Size::Byte, function_code)?
            {
                data[done] = value as u8;
                done += 1;
                continue;
            }
            let count = self.external_run(at, function_code, data.len() - done);
            done += self.board.peek_external(
                &self.cpu.memory,
                at,
                function_code,
                self.now,
                &mut data[done..done + count],
            )?;
        }
        Ok(())
    }

    /// Stores `data` from `address` up where `bus_write` would, without
    /// a cycle: memory's bytes as [`write`](Self::write) writes them,
    /// ROM included, whether the core serves the byte or the decode
    /// reaches it, and a peripheral's through its
    /// [`MappedPeripheral::poke`], one call for each run of bytes the
    /// same wires would take, each that would take them.  A byte the
    /// core serves is written in supervisor space, whatever
    /// `function_code`, as `write` writes it.  A module's register is
    /// not poked.  Each byte is on the address lines.
    ///
    /// # Errors
    ///
    /// [`BusCycleError::Unpeekable`] for a module's register, a run a
    /// peripheral declines or a byte a matching select reaches nothing
    /// through, [`BusCycleError::Unserved`] for one nothing answers.
    /// What was stored before it stays stored.
    ///
    /// [`MappedPeripheral::poke`]: mc683xx_cpu32::peripheral::MappedPeripheral::poke
    pub fn poke_bytes(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), BusCycleError> {
        let mut done = 0;
        while done < data.len() {
            let at = self.cpu.memory.mask(address.offset(done as u32)).0;
            if let Some(offset) = self.module_offset(at, function_code)
                && (BLOCK.contains(offset)
                    || self
                        .board
                        .modules
                        .iter()
                        .any(|module| module.block().contains(offset)))
            {
                return Err(BusCycleError::Unpeekable);
            }
            let page_left = (PAGE_BYTES - at % PAGE_BYTES) as usize;
            let count = self
                .external_run(at, function_code, data.len() - done)
                .min(page_left);
            let run = &data[done..done + count];
            // a page's bytes the core serves are all served
            if self.cpu.memory.write_bytes(Address(at), run).is_err() {
                self.board
                    .poke_external(&mut self.cpu.memory, at, function_code, self.now, run)?;
            }
            done += count;
        }
        Ok(())
    }
    /// Fills `data` from `address` up, with no bus cycle, a debugger's
    /// read: from the memories the core serves in supervisor space, as
    /// the decode now maps them, whatever the pages' permissions; as the
    /// bare machine's [`Machine::read`](mc683xx_cpu32::flat::Machine::read)
    /// ([`Memory::read_bytes`](mc683xx_cpu32::memory::Memory::read_bytes)).
    /// [`peek`](Self::peek) reads the modules' registers too.
    ///
    /// # Errors
    ///
    /// [`Unserved`] for the first byte the core does not serve a read of
    /// (a peripheral's, or one a peripheral shares); `data` is then filled
    /// up to it.
    pub fn read(&self, address: Address, data: &mut [u8]) -> Result<(), Unserved> {
        self.cpu.memory.read_bytes(address, data)
    }

    /// Writes `data` at `address` up, with no bus cycle, a debugger's
    /// write: into the bytes [`read`](Self::read) reads, a ROM's
    /// included, whatever the pages' permissions, so a read gives back
    /// what was written; where the core serves a page's writes but not
    /// its reads (a write-only select), into the memory it writes.  As
    /// the bare machine's
    /// [`Machine::write`](mc683xx_cpu32::flat::Machine::write)
    /// ([`Memory::write_bytes`](mc683xx_cpu32::memory::Memory::write_bytes)).
    ///
    /// # Errors
    ///
    /// [`Unserved`] for a byte the core serves neither a read nor a
    /// write of (a peripheral's, or one a peripheral shares); then
    /// nothing is written.
    pub fn write(&mut self, address: Address, data: &[u8]) -> Result<(), Unserved> {
        self.cpu.memory.write_bytes(address, data)
    }

    /// The byte or word cycles of an access, each on the address lines
    /// with its place in the value.
    fn words(
        &self,
        address: Address,
        size: Size,
    ) -> Result<impl Iterator<Item = (u32, Size, u32)> + use<>, BusCycleError> {
        if size != Size::Byte && address.is_odd() {
            return Err(BusCycleError::Misaligned(address));
        }
        let at = |by| self.cpu.memory.mask(address.offset(by)).0;
        let first = (at(0), size, 0);
        let (words, count) = match size {
            Size::Long => ([(at(0), Size::Word, WORD_BITS), (at(2), Size::Word, 0)], 2),
            _ => ([first, first], 1),
        };
        Ok(words.into_iter().take(count))
    }

    /// One byte or word cycle, now, leaving the stop request as it was.
    fn cycle(
        &mut self,
        address: u32,
        size: Size,
        function_code: FunctionCode,
        write: Option<u32>,
    ) -> Result<u32, BusCycleError> {
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
    /// acknowledge it (MC68331UM 4.8), else the bytes the external bus
    /// would drive.
    fn peek_cycle(
        &self,
        address: u32,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<u32, BusCycleError> {
        if let Some(value) = self.peek_register(address, size, function_code)? {
            return Ok(value);
        }
        let mut data = [0; 2];
        let data = &mut data[..size.bytes() as usize];
        let mut done = 0;
        while done < data.len() {
            done += self.board.peek_external(
                &self.cpu.memory,
                address + done as u32,
                function_code,
                self.now,
                &mut data[done..],
            )?;
        }
        Ok(data
            .iter()
            .fold(0, |value, &byte| value << 8 | u32::from(byte)))
    }

    /// The offset in the module space of `address`, if a cycle in
    /// `function_code`'s space there is in it: never one in CPU space.
    fn module_offset(&self, address: u32, function_code: FunctionCode) -> Option<u32> {
        if function_code == FunctionCode::CpuSpace {
            return None;
        }
        address
            .checked_sub(self.board.sim.module_base().0)
            .filter(|&offset| offset < MODULE_SPACE_BYTES)
    }

    /// A module's register of `size` at `address` as it holds it, if one
    /// would acknowledge a cycle there (MC68331UM 4.8): [`None`] if none
    /// would.
    fn peek_register(
        &self,
        address: u32,
        size: Size,
        function_code: FunctionCode,
    ) -> Result<Option<u32>, BusCycleError> {
        let Some(offset) = self.module_offset(address, function_code) else {
            return Ok(None);
        };
        let board = &self.board;
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
            Peek::Value(value) => Ok(Some(value)),
            Peek::Unpeekable => Err(BusCycleError::Unpeekable),
            Peek::Unacknowledged => Ok(None),
        }
    }

    /// How many of `left` bytes from `address` on, outside the module
    /// space, go to the external bus together: up to the module space
    /// or the top of the address lines.  One in the module space, which
    /// no module acknowledged, alone.
    fn external_run(&self, address: u32, function_code: FunctionCode, left: usize) -> usize {
        if self.module_offset(address, function_code).is_some() {
            return 1;
        }
        let top = self.cpu.memory.mask(Address(u32::MAX)).0;
        let mut end = u64::from(top) + 1;
        let base = self.board.sim.module_base().0;
        if function_code != FunctionCode::CpuSpace && address < base {
            end = end.min(u64::from(base));
        }
        (end - u64::from(address)).min(left as u64) as usize
    }
}

impl Board {
    /// Why a cycle `request` describes ended in a bus error: nothing
    /// matched it, or something answered it so.
    fn unanswered(&self, request: DecodeRequest) -> BusCycleError {
        if self.sim.chip_selects().matches(request).next().is_none() {
            BusCycleError::Unserved(Address(request.address))
        } else {
            BusCycleError::BusError(Address(request.address))
        }
    }

    /// Fills the start of `data`, a run of bytes from `address` up, as
    /// the external bus would drive them at `now`: a byte the memory
    /// that alone answers it holds, or that the first to drive it holds;
    /// or the bytes from `address` on that one peripheral, on one
    /// select, would be first to drive, from one call to its peek.  How
    /// many it filled.
    fn peek_external(
        &self,
        memory: &Memory,
        address: u32,
        function_code: FunctionCode,
        now: Cycle,
        data: &mut [u8],
    ) -> Result<usize, BusCycleError> {
        let first = |at: u32| -> Result<Driver, BusCycleError> {
            let request = byte_request(at, function_code, false);
            if let Some((wired, _)) = self.memory_alone(request) {
                return Ok(Driver::Memory(wired));
            }
            match self.takers(request, now).first() {
                Some(&(Wire::Memory(wired), _)) => Ok(Driver::Memory(wired)),
                Some(&(Wire::Peripheral(peripheral), cycle)) => {
                    Ok(Driver::Peripheral(peripheral, cycle))
                }
                None => Err(self.unreached(request)),
            }
        };
        match first(address)? {
            Driver::Memory(wired) => {
                data[0] = load(memory, wired, address, Size::Byte) as u8;
                Ok(1)
            }
            Driver::Peripheral(peripheral, cycle) => {
                let count = (1..data.len())
                    .find(|&index| {
                        !matches!(first(address + index as u32),
                            Ok(Driver::Peripheral(other, next))
                                if other == peripheral && next.select == cycle.select)
                    })
                    .unwrap_or(data.len());
                if self.peripherals[peripheral].peek(cycle, &mut data[..count]) {
                    Ok(count)
                } else {
                    Err(BusCycleError::Unpeekable)
                }
            }
        }
    }

    /// Stores `data`, a run of bytes from `address` up the core does not
    /// serve, where the external bus would write them at `now`: each run
    /// of them the same wires take, into each memory, a ROM too, as a
    /// debugger's write, and through each peripheral's poke.
    fn poke_external(
        &mut self,
        memory: &mut Memory,
        address: u32,
        function_code: FunctionCode,
        now: Cycle,
        data: &[u8],
    ) -> Result<(), BusCycleError> {
        let takers = |board: &Self, at: u32| {
            let request = byte_request(at, function_code, true);
            let takers = board.takers(request, now);
            if takers.is_empty() {
                return Err(board.unreached(request));
            }
            Ok(takers)
        };
        let wires = |takers: &[(Wire, BusCycle)]| -> Vec<(Wire, Option<Select>)> {
            takers
                .iter()
                .map(|&(wire, cycle)| (wire, cycle.select))
                .collect()
        };
        let mut done = 0;
        while done < data.len() {
            let at = address + done as u32;
            let first = takers(self, at)?;
            let same = wires(&first);
            let count = (done + 1..data.len())
                .find(|&index| {
                    takers(self, address + index as u32)
                        .map_or(true, |others| wires(&others) != same)
                })
                .unwrap_or(data.len())
                - done;
            let run = &data[done..done + count];
            for (wire, cycle) in first {
                match wire {
                    Wire::Peripheral(peripheral) => {
                        if !self.peripherals[peripheral].poke(cycle, run) {
                            return Err(BusCycleError::Unpeekable);
                        }
                    }
                    // a debugger's write: ROM's too, as `write`'s
                    Wire::Memory(wired) => {
                        for (index, &byte) in run.iter().enumerate() {
                            store(
                                memory,
                                wired,
                                at + index as u32,
                                Size::Byte,
                                Lanes::BOTH,
                                byte.into(),
                            );
                        }
                    }
                }
            }
            done += count;
        }
        Ok(())
    }

    /// Why nothing takes a byte cycle `request` describes: no select
    /// matches it and no peripheral on no select takes it, or a select
    /// that does reaches nothing.
    fn unreached(&self, request: DecodeRequest) -> BusCycleError {
        if self.sim.chip_selects().matches(request).next().is_none()
            && !self.bus_takes(request.address, request.function_code)
        {
            BusCycleError::Unserved(Address(request.address))
        } else {
            BusCycleError::Unpeekable
        }
    }
}

/// What would drive a byte on the external bus first.
#[derive(Debug, Clone, Copy)]
enum Driver {
    /// A memory.
    Memory(HostMemory),
    /// The peripheral at this index in the board's, with its cycle.
    Peripheral(usize, BusCycle),
}

/// A byte cycle at `address` in `function_code`'s space, a write or a
/// read, as the decode sees it.
const fn byte_request(address: u32, function_code: FunctionCode, write: bool) -> DecodeRequest {
    DecodeRequest {
        address,
        function_code,
        write,
        size: Size::Byte,
    }
}

#[cfg(test)]
mod tests;
