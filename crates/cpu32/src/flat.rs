//! A bare CPU32 on flat memory, for hosts that are not a 68331 board.
//!
//! The host's RAM is mapped at any page of a 32-bit (or narrower)
//! address space with no chip-select decode, its peripherals at any byte
//! range, the rest a bus error, and interrupt requests the host sets:
//! for function-level firmware tests and the like ([PLAN.md, Bare
//! CPU]).
//!
//! [PLAN.md, Bare CPU]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#bare-cpu

use std::collections::BTreeMap;
use std::fmt;
use std::ops::RangeInclusive;

use crate::address::Address;
use crate::bus::Bus;
use crate::bus::BusAccess;
use crate::bus::BusError;
use crate::bus::LPSTOP_BROADCAST;
use crate::bus::PortSize;
use crate::bus::Read;
use crate::bus::StopRequest;
use crate::cpu::Cpu;
use crate::cpu::Halt;
use crate::cpu::bare::LimitedRun;
use crate::cpu::bare::Limits;
use crate::cpu::state::StateError;
use crate::cpu::state::decode;
use crate::cpu::state::encode;
use crate::cpu::state::restore_each;
use crate::cycles::Cycles;
use crate::interrupt::Acknowledge;
use crate::interrupt::Acknowledged;
use crate::interrupt::InterruptLevel;
use crate::interrupt::Interrupts;
use crate::interrupt::IrqPin;
use crate::memory::Backing;
use crate::memory::Memory;
use crate::memory::MemoryError;
use crate::memory::PAGE_BYTES;
use crate::memory::Page;
use crate::memory::Permissions;
use crate::memory::RegionId;
use crate::memory::Space;
use crate::peripheral::BusCycle;
use crate::peripheral::MappedPeripheral;
use crate::peripheral::PeripheralId;
use crate::size::Size;

#[cfg(test)]
mod tests;

/// The format of [`Machine::save`]'s bytes.
pub const VERSION: u32 = 3;
/// The address lines a machine has unless the host says otherwise:
/// all of the CPU32's (CPU32RM 5.3), so no address aliases.
pub const ADDRESS_BITS: u32 = 32;
/// Clocks in a zero-wait bus cycle (CPU32RM 8.1.5): the timing of
/// mapped memory and of peripherals.
const FASTEST_CLOCKS: u8 = 2;
/// The interrupt levels with a request line, 1 to 7.
const LEVELS: usize = 7;
/// Both spaces, each mapped alike.
const SPACES: [Space; 2] = [Space::User, Space::Supervisor];

/// What can go wrong mapping memory or peripherals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MapError {
    /// The page tables refused the range.
    #[error(transparent)]
    Memory(#[from] MemoryError),
    /// Memory where some is mapped already.
    #[error("{0:#x} is mapped already")]
    Mapped(u32),
    /// Contents longer than the memory.
    #[error("{contents:#x} bytes of contents in {length:#x} bytes")]
    Contents {
        /// The contents' length.
        contents: usize,
        /// The memory's.
        length: u32,
    },
    /// A peripheral range that overlaps another peripheral's, or is empty.
    #[error("peripherals overlap at {0:#x}")]
    Overlap(u32),
}

/// Host access to memory no map covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0:#x} is not mapped")]
pub struct Unmapped(pub u32);

/// A peripheral with the addresses it covers.
struct Wired {
    /// Its identity.
    id: PeripheralId,
    /// Its addresses.
    range: RangeInclusive<u32>,
    /// The peripheral.
    peripheral: Box<dyn MappedPeripheral>,
}

impl Wired {
    /// `error`, of this peripheral, named by its first address.
    fn named(&self, error: StateError) -> StateError {
        error.of_peripheral(&format!("the peripheral at {:#x}", self.range.start()))
    }
}

/// The bus: the peripherals, by address, and nothing else.
#[derive(Default)]
struct FlatBus {
    /// The peripherals, sorted by address, none overlapping.
    peripherals: Vec<Wired>,
    /// The next [`PeripheralId`].
    next: u64,
    /// Whether exception processing is stacking.
    stacking: bool,
    /// The peripherals' stop request.
    stop: StopRequest,
}

impl fmt::Debug for FlatBus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FlatBus")
            .field(
                "peripherals",
                &self
                    .peripherals
                    .iter()
                    .map(|wired| wired.range.clone())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl FlatBus {
    /// The peripherals' indices in attach order, the order of their ids.
    fn attach_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.peripherals.len()).collect();
        order.sort_by_key(|&index| u64::from(self.peripherals[index].id));
        order
    }

    /// Each peripheral's snapshot, in attach order; an error names the
    /// peripheral.
    fn save_peripherals(&self) -> Result<Vec<Vec<u8>>, StateError> {
        self.attach_order()
            .into_iter()
            .map(|index| {
                let wired = &self.peripherals[index];
                wired.peripheral.save().map_err(|error| wired.named(error))
            })
            .collect()
    }

    /// Restores the peripheral at `index` from `snapshot`; an error names it.
    fn restore_peripheral(&mut self, index: usize, snapshot: &[u8]) -> Result<(), StateError> {
        let wired = &mut self.peripherals[index];
        wired
            .peripheral
            .restore(snapshot)
            .map_err(|error| wired.named(error))
    }

    /// The peripheral that covers every byte of `access`, if one does.  No
    /// peripheral is in CPU space: its addresses are not memory's (CPU32RM
    /// 5.3.1).
    fn peripheral(&mut self, access: &BusAccess) -> Option<(&mut dyn MappedPeripheral, BusCycle)> {
        if access.function_code.is_cpu_space() {
            return None;
        }
        let first = access.address.0;
        let last = first.checked_add(access.size.bytes() - 1)?;
        let index = self
            .peripherals
            .partition_point(|wired| *wired.range.end() < first);
        let wired = self.peripherals.get_mut(index)?;
        if !(wired.range.contains(&first) && wired.range.contains(&last)) {
            return None;
        }
        let cycle = BusCycle::new(
            access.address.0,
            access.size,
            access.function_code,
            access.now,
        )
        .while_stacking(self.stacking);
        Some((wired.peripheral.as_mut(), cycle))
    }
}

impl Bus for FlatBus {
    fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
        let (peripheral, cycle) = self.peripheral(&access).ok_or(BusError)?;
        let value = peripheral.read(cycle)? & access.size.mask();
        Ok(Read {
            value,
            stall: Cycles::ZERO,
        })
    }

    /// The LPSTOP broadcast is acknowledged here, as the SIM would
    /// (MC68331UM 4.5.4.2), so LPSTOP stops; any other CPU space cycle,
    /// a breakpoint acknowledge among them, is a bus error (CPU32RM
    /// 6.2.5).
    fn write(&mut self, access: BusAccess, value: u32) -> Result<Cycles, BusError> {
        if is_lpstop_broadcast(&access) {
            return Ok(Cycles::ZERO);
        }
        let (peripheral, cycle) = self.peripheral(&access).ok_or(BusError)?;
        peripheral.write(cycle, value)?;
        Ok(Cycles::ZERO)
    }

    fn read_long(&mut self, access: BusAccess) -> Option<Result<Read, BusError>> {
        self.peripheral(&access)?;
        Some(self.read(access))
    }

    fn write_long(&mut self, access: BusAccess, value: u32) -> Option<Result<Cycles, BusError>> {
        self.peripheral(&access)?;
        Some(self.write(access, value))
    }

    fn stacking(&mut self, stacking: bool) {
        self.stacking = stacking;
    }

    fn stop_requested(&mut self) -> bool {
        self.stop.take()
    }
}

/// Whether `access` is the LPSTOP broadcast (MC68331UM 4.5.4.2).
fn is_lpstop_broadcast(access: &BusAccess) -> bool {
    access.function_code.is_cpu_space()
        && access.address == LPSTOP_BROADCAST
        && access.size == Size::Word
}

/// The host's interrupt requests: one line per level, each answered
/// at its acknowledge by a vector or the autovector (CPU32RM 6.2.11).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Requests {
    /// Levels 1 to 7's requests: their vector, [`None`] for the
    /// autovector; the outer [`None`] while not requested.
    lines: [Option<Option<u8>>; LEVELS],
}

impl Interrupts for Requests {
    fn level(&self) -> InterruptLevel {
        let level = self
            .lines
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |index| index + 1);
        InterruptLevel::new(level as u8).unwrap_or(InterruptLevel::NONE)
    }

    fn acknowledge(&mut self, level: InterruptLevel, _now: Cycles) -> Acknowledged {
        let line = usize::from(level.get()).checked_sub(1);
        match line.and_then(|line| self.lines[line]) {
            Some(Some(vector)) => Acknowledge::Vector(vector),
            Some(None) => Acknowledge::Autovector,
            None => Acknowledge::Spurious,
        }
        .into()
    }
}

/// What a snapshot of the machine holds beside the core's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct MachineState {
    /// [`Cpu::save`]'s bytes.
    cpu: Vec<u8>,
    /// The CPU's clock.
    now: Cycles,
    /// The interrupt requests.
    requests: Requests,
    /// Each peripheral's snapshot, in attach order.
    peripherals: Vec<Vec<u8>>,
}

/// A bare CPU32, its flat memory and peripherals.
///
/// # Examples
///
/// A function called from the host: RAM mapped, the code written to
/// it, PC and SP set, and the run limited to the function's return.
///
/// ```
/// use mc683xx_cpu32::address::Address;
/// use mc683xx_cpu32::cpu::bare::End;
/// use mc683xx_cpu32::cpu::bare::Limits;
/// use mc683xx_cpu32::flat::ADDRESS_BITS;
/// use mc683xx_cpu32::flat::Machine;
/// use mc683xx_cpu32::memory::PAGE_BYTES;
/// use mc683xx_cpu32::registers::status;
///
/// /// Where the function's caller returns to: nothing runs there.
/// const RETURN: u32 = 0x100;
/// /// The function.
/// const CODE: u32 = 0x400;
/// /// The supervisor stack's top.
/// const STACK: u32 = 0x800;
///
/// let mut machine = Machine::new(ADDRESS_BITS)?;
/// machine.map(Address(0), PAGE_BYTES, &[])?;
/// // ADD.L D1,D0; RTS
/// machine.write(Address(CODE), &[0xd0, 0x81, 0x4e, 0x75])?;
/// machine.write(Address(STACK - 4), &RETURN.to_be_bytes())?;
///
/// let registers = &mut machine.cpu.registers;
/// registers.set_sr(status::S);
/// registers.ssp = STACK - 4;
/// registers.pc = CODE;
/// registers.data[0] = 40;
/// registers.data[1] = 2;
/// let mut limits = Limits::default();
/// limits.until = Some(Address(RETURN));
/// let run = machine.run_limited(limits);
/// assert_eq!(run.end, End::Until);
/// assert_eq!(machine.cpu.registers.data[0], 42);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Machine {
    /// The CPU: its registers, memory and hooks.
    pub cpu: Cpu,
    /// The peripherals.
    bus: FlatBus,
    /// The interrupt requests.
    requests: Requests,
    /// The CPU's clock.
    now: Cycles,
    /// The regions [`map`](Self::map) made, each with the address it is
    /// mapped from, its bytes backing its pages in order: the only ones
    /// [`unmap`](Self::unmap) frees or trims.  A region the host added
    /// through `cpu.memory` is the host's, its pages unmapped but its bytes
    /// left alone.
    maps: BTreeMap<RegionId, Address>,
}

impl Machine {
    /// A machine on `address_bits` address lines ([`ADDRESS_BITS`] for a
    /// full 32-bit bus), nothing mapped, its registers zero, not reset.
    ///
    /// # Errors
    ///
    /// [`MemoryError::AddressBits`] for an address width [`Memory::new`]
    /// refuses.
    pub fn new(address_bits: u32) -> Result<Self, MemoryError> {
        let mut cpu = Cpu::new(Memory::new(address_bits)?);
        cpu.set_long_bus(true);
        Ok(Self {
            cpu,
            bus: FlatBus::default(),
            requests: Requests::default(),
            now: Cycles::ZERO,
            maps: BTreeMap::new(),
        })
    }

    /// Maps `length` bytes of RAM from `start`, whole pages
    /// ([`PAGE_BYTES`]), in both spaces: zero, then `contents` from its
    /// start.  None of it may be mapped already.
    ///
    /// # Errors
    ///
    /// [`MapError::Contents`] if `contents` is longer than `length`,
    /// [`MapError::Mapped`] if some of it is mapped already, and
    /// [`MapError::Memory`] for a range [`Memory::map`] refuses.
    pub fn map(&mut self, start: Address, length: u32, contents: &[u8]) -> Result<(), MapError> {
        if contents.len() > length as usize {
            return Err(MapError::Contents {
                contents: contents.len(),
                length,
            });
        }
        let pages = (0..length / PAGE_BYTES).map(|page| start.offset(page * PAGE_BYTES));
        if let Some(mapped) = pages
            .clone()
            .find(|&page| self.cpu.memory.entry(Space::Supervisor, page) != Page::BUS)
        {
            return Err(MapError::Mapped(mapped.0));
        }
        // Checked before the region is taken, so a refusal leaves none.
        self.cpu.memory.check(start, length)?;
        let mut bytes = vec![0; length as usize];
        bytes[..contents.len()].copy_from_slice(contents);
        let region = self.cpu.memory.add_region(bytes);
        Ok(self.place(start, region)?)
    }

    /// Maps all of `region` from `start`, in both spaces, as one of
    /// `maps`.
    fn place(&mut self, start: Address, region: RegionId) -> Result<(), MemoryError> {
        self.maps.insert(region, start);
        let length = self.cpu.memory.region(region).len() as u32;
        let backing = Some(Backing { region, offset: 0 });
        let page = Page {
            read: backing,
            write: backing,
            port: PortSize::Bits16,
            clocks: FASTEST_CLOCKS,
        };
        for space in SPACES {
            self.cpu.memory.map(space, start, length, page)?;
        }
        Ok(())
    }

    /// Unmaps `length` bytes from `start`, whole pages, in both spaces, and
    /// gives them every permission again.  The bytes of
    /// [`map`](Self::map)'s maps are freed: one wholly inside goes, one
    /// partly inside keeps the rest of its bytes, its pages still mapped
    /// with their permissions.  A region the host mapped keeps its bytes.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Unaligned`] or [`MemoryError::OutOfRange`] for a
    /// range that is not whole pages inside the address space.
    pub fn unmap(&mut self, start: Address, length: u32) -> Result<(), MemoryError> {
        self.cpu.memory.check(start, length)?;
        // Each of `maps` the range reaches where it is mapped from, with
        // that address: not a page the host mapped from it elsewhere.
        let mut reached: Vec<(RegionId, u64)> = Vec::new();
        for page in (0..length / PAGE_BYTES).map(|page| start.offset(page * PAGE_BYTES)) {
            if let Some(backing) = self.cpu.memory.entry(Space::Supervisor, page).read
                && let Some(&from) = self.maps.get(&backing.region)
                && u64::from(from.0) + u64::from(backing.offset) == u64::from(page.0)
                && reached
                    .last()
                    .is_none_or(|&(region, _)| region != backing.region)
            {
                reached.push((backing.region, u64::from(from.0)));
            }
        }
        for space in SPACES {
            self.cpu.memory.map(space, start, length, Page::BUS)?;
            self.cpu
                .memory
                .protect(space, start, length, Permissions::ALL)?;
        }
        let (first, end) = (u64::from(start.0), u64::from(start.0) + u64::from(length));
        for (region, from) in reached {
            let size = self.cpu.memory.region(region).len() as u64;
            let left = first.saturating_sub(from).min(size) as usize;
            let right = end.saturating_sub(from).min(size) as usize..size as usize;
            let right_start = Address((from + right.start as u64) as u32);
            let keeps_right = !right.is_empty();
            if keeps_right {
                if left == 0 {
                    self.cpu.memory.trim_region(region, right);
                    self.place(right_start, region)?;
                } else {
                    let bytes = self.cpu.memory.region(region)[right].to_vec();
                    let tail = self.cpu.memory.add_region(bytes);
                    self.place(right_start, tail)?;
                }
            }
            if left > 0 {
                self.cpu.memory.trim_region(region, 0..left);
            } else if !keeps_right {
                self.maps.remove(&region);
                self.cpu.memory.remove_region(region);
            }
        }
        Ok(())
    }

    /// Sets the permissions of `length` bytes from `start`, whole
    /// pages, in both spaces.
    ///
    /// # Errors
    ///
    /// As [`Machine::unmap`].
    pub fn protect(
        &mut self,
        start: Address,
        length: u32,
        permissions: Permissions,
    ) -> Result<(), MemoryError> {
        for space in SPACES {
            self.cpu.memory.protect(space, start, length, permissions)?;
        }
        Ok(())
    }

    /// The region and offset of mapped byte `address`.
    fn locate(&self, address: Address) -> Result<(Backing, usize), Unmapped> {
        let page = self.cpu.memory.entry(Space::Supervisor, address);
        let backing = page.read.ok_or(Unmapped(address.0))?;
        Ok((backing, (address.0 % PAGE_BYTES) as usize))
    }

    /// Fills `bytes` from mapped memory at `address`, whatever its
    /// permissions, with no bus cycle.
    ///
    /// # Errors
    ///
    /// [`Unmapped`], with the first address not mapped, if any is not.
    pub fn read(&self, address: Address, bytes: &mut [u8]) -> Result<(), Unmapped> {
        let mut done = 0;
        while done < bytes.len() {
            let at = address.offset(done as u32);
            let (backing, within) = self.locate(at)?;
            let count = (PAGE_BYTES as usize - within).min(bytes.len() - done);
            let from = backing.offset as usize + within;
            bytes[done..done + count]
                .copy_from_slice(&self.cpu.memory.region(backing.region)[from..from + count]);
            done += count;
        }
        Ok(())
    }

    /// Writes `bytes` to mapped memory at `address`, as
    /// [`read`](Self::read).  On an error nothing is written.
    ///
    /// # Errors
    ///
    /// As [`Machine::read`].
    pub fn write(&mut self, address: Address, bytes: &[u8]) -> Result<(), Unmapped> {
        let mut spans = Vec::new();
        let mut done = 0;
        while done < bytes.len() {
            let at = address.offset(done as u32);
            let (backing, within) = self.locate(at)?;
            let count = (PAGE_BYTES as usize - within).min(bytes.len() - done);
            spans.push((backing, within, done, count));
            done += count;
        }
        for (backing, within, done, count) in spans {
            let from = backing.offset as usize + within;
            self.cpu.memory.region_mut(backing.region)[from..from + count]
                .copy_from_slice(&bytes[done..done + count]);
        }
        Ok(())
    }

    /// Puts `peripheral` at `range`, which no other peripheral's overlaps: it
    /// answers the cycles there that no mapped memory serves.
    ///
    /// # Errors
    ///
    /// [`MapError::Overlap`] if `range` is empty or overlaps another
    /// peripheral's.
    pub fn add_peripheral(
        &mut self,
        range: RangeInclusive<Address>,
        peripheral: Box<dyn MappedPeripheral>,
    ) -> Result<PeripheralId, MapError> {
        let range = range.start().0..=range.end().0;
        if range.is_empty() {
            return Err(MapError::Overlap(*range.start()));
        }
        let index = self
            .bus
            .peripherals
            .partition_point(|wired| wired.range.end() < range.start());
        if let Some(next) = self.bus.peripherals.get(index)
            && next.range.start() <= range.end()
        {
            return Err(MapError::Overlap(*next.range.start().max(range.start())));
        }
        self.bus.next += 1;
        let id = PeripheralId::from(self.bus.next);
        self.bus.peripherals.insert(
            index,
            Wired {
                id,
                range,
                peripheral,
            },
        );
        Ok(id)
    }

    /// Removes peripheral `id`, giving it back.
    pub fn remove_peripheral(&mut self, id: PeripheralId) -> Option<Box<dyn MappedPeripheral>> {
        let index = self
            .bus
            .peripherals
            .iter()
            .position(|wired| wired.id == id)?;
        Some(self.bus.peripherals.remove(index).peripheral)
    }

    /// The handle by which peripherals ask the run to stop.
    #[must_use]
    pub fn stop_request(&self) -> StopRequest {
        self.bus.stop.clone()
    }

    /// Requests an interrupt on `pin`, answered at its acknowledge by
    /// `vector`, or the autovector if [`None`]; it stays requested, as a
    /// level-sensitive line does, until cleared.
    pub fn request_interrupt(&mut self, pin: IrqPin, vector: Option<u8>) {
        self.requests.lines[usize::from(pin.get() - 1)] = Some(vector);
    }

    /// Withdraws the request on `pin`.
    pub fn clear_interrupt(&mut self, pin: IrqPin) {
        self.requests.lines[usize::from(pin.get() - 1)] = None;
    }

    /// The reset exception (CPU32RM 6.2.1): SSP and PC from the long
    /// words at zero.
    ///
    /// # Errors
    ///
    /// As [`Cpu::reset`].
    pub fn reset(&mut self) -> Result<(), Halt> {
        self.cpu.reset(&mut self.bus, self.now)
    }

    /// Runs until `limits` or a stop ([`Cpu::run_limited`]).
    pub fn run_limited(&mut self, limits: Limits) -> LimitedRun {
        let run = self
            .cpu
            .run_limited(&mut self.bus, &mut self.requests, self.now, limits);
        self.now = Cycles(self.now.0.saturating_add(run.cycles.0));
        run
    }

    /// The CPU's clock: the cycles the runs took.
    #[must_use]
    pub const fn now(&self) -> Cycles {
        self.now
    }

    /// The machine's state as bytes: the core's, the clock, the
    /// interrupt requests and each peripheral's ([`Save`](crate::cpu::state::Save)), in the order
    /// they were added.
    ///
    /// # Errors
    ///
    /// A peripheral that cannot save, named by its first address
    /// ([`StateError::Peripheral`]).
    pub fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(
            VERSION,
            &MachineState {
                cpu: self.cpu.save(),
                now: self.now,
                requests: self.requests,
                peripherals: self.bus.save_peripherals()?,
            },
        ))
    }

    /// Restores bytes [`save`](Self::save) gave of a machine mapped alike:
    /// the same maps and unmaps, and as many peripherals, added in the same
    /// order.  On an error it is unchanged: each peripheral's snapshot is
    /// checked by restoring it, each peripheral saved first and the ones
    /// restored put back if a later one or the core (restored last)
    /// refuses ([`restore_each`]).
    ///
    /// # Errors
    ///
    /// As [`Cpu::restore`], for the machine's snapshot and the core's in
    /// it; [`StateError::Mismatch`] for another count of peripherals; a
    /// peripheral that cannot save or refuses its snapshot, named
    /// ([`StateError::Peripheral`]).
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let state: MachineState = decode(VERSION, snapshot)?;
        if state.peripherals.len() != self.bus.peripherals.len() {
            return Err(StateError::mismatch("peripherals"));
        }
        let backups = self.bus.save_peripherals()?;
        let order = self.bus.attach_order();
        let parts: Vec<&[u8]> = state
            .peripherals
            .iter()
            .map(Vec::as_slice)
            .chain([state.cpu.as_slice()])
            .collect();
        let (bus, cpu) = (&mut self.bus, &mut self.cpu);
        restore_each(&parts, &backups, |part, bytes| match order.get(part) {
            Some(&index) => bus.restore_peripheral(index, bytes),
            None => cpu.restore(bytes),
        })?;
        self.now = state.now;
        self.requests = state.requests;
        Ok(())
    }
}
