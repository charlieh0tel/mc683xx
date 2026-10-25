//! Everything on the chip's bus but the CPU: the SIM, the other
//! modules, and the host's peripherals and memory on the chip-select pins.
//! It answers the core's bus cycles and interrupt acknowledges, and
//! builds the core's page tables from the chip-select decode.

use std::any::Any;
use std::collections::BTreeSet;
use std::fmt;
use std::mem;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::LPSTOP_BROADCAST;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::interrupt::Acknowledge;
use mc683xx_cpu32::interrupt::Acknowledged;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::memory::Backing;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::memory::Page;
use mc683xx_cpu32::memory::RegionId;
use mc683xx_cpu32::memory::Space;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::module;
use mc683xx_modules::module::Module;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::peripheral::iack_address;
use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::qsm::Qsm;
use mc683xx_modules::sim::BLOCK;
use mc683xx_modules::sim::MODULE_SPACE_BYTES;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::Sim;
use mc683xx_modules::sim::chip_selects::DecodeRequest;
use mc683xx_modules::sim::chip_selects::Lanes;
use mc683xx_modules::sim::chip_selects::Match;
use mc683xx_modules::sim::chip_selects::Selects;
use mc683xx_modules::sim::chip_selects::Termination;
use mc683xx_modules::sim::ports::PortName;

use crate::chip::Diversion;
use crate::chip::OutputStop;
use crate::chip::SyncRequest;
use crate::event::ChipTracing;
use crate::event::DsackFault;
use crate::event::Event;
use crate::gpt::Gpt;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::reset::ResetScope;

/// The 68331's address lines, ADDR\[23:0\] (MC68331UM 4.4.1.1).
pub(crate) const ADDRESS_LINES: u32 = 24;
/// Pages in the address space.
const PAGES: usize = 1 << (ADDRESS_LINES - PAGE_BYTES.trailing_zeros());
/// A word cycle's lower byte, on DATA\[7:0\] at the odd address
/// (MC68331UM 4.4.2).
const LOWER_LANE: u32 = Size::Byte.mask();
/// Its upper byte, on DATA\[15:8\] at the even address.
const UPPER_LANE: u32 = LOWER_LANE << u8::BITS;
/// The LPSTOP broadcast's interrupt mask bits (MC68331UM Figure 4-13).
const LPSTOP_MASK: u32 = 0b111;

/// PEPAR's PEPA0: PE0 is DSACK0 (SIMRM 9.1, Table 9-1).
const PEPA_DSACK0: u8 = 1 << 0;
/// PEPA1: PE1 is DSACK1.
const PEPA_DSACK1: u8 = 1 << 1;
/// [`DsackFault::Unassigned`]'s bit in [`Board::dsack_told`].
const TOLD_UNASSIGNED: u8 = 1 << 0;
/// Every bit [`Board::dsack_told`] holds.
const TOLD_ALL: u8 = TOLD_UNASSIGNED;
/// Clocks in the timing tables' bus cycle (CPU32RM 8.1.5): an internal
/// module's two-clock cycle costs nothing beyond them (MC68331UM 4.5).
const TABLE_CLOCKS: u8 = 2;
/// A fixed memory's port and clocks by default: a 16-bit port with no
/// wait states that costs nothing beyond the timing tables, as an
/// internal module's cycle does.  It has no DSACK: the chip has no such
/// decode.
pub(crate) const FIXED_TIMING: (PortSize, u8) = (PortSize::Bits16, TABLE_CLOCKS);

/// A host peripheral the chip holds: a peripheral on chip-select pins with its
/// own time, in the chip's snapshot, which the host can get back by
/// type.
pub(crate) trait Peripheral: MappedPeripheral + Timed + Save + Any {}

impl<T: MappedPeripheral + Timed + Save + Any> Peripheral for T {}

/// A module the chip holds, which the host can get back by type.
pub(crate) trait Part: Module + Any {}

impl<T: Module + Any> Part for T {}

/// The QSM's index in [`Board::modules`].
pub(crate) const QSM: usize = 0;
/// The GPT's.
pub(crate) const GPT: usize = 1;

/// Host memory: on chip-select pins, its address pins take ADDR from
/// bit 0 up to its size, so a block larger than it repeats it; at a
/// fixed address, it answers from its base up to its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostMemory {
    /// The core's region holding it.
    pub(crate) region: RegionId,
    /// The address its first byte answers at: zero on chip-select pins,
    /// a fixed memory's base.
    pub(crate) base: u32,
    /// Its bytes: a whole number of pages.
    pub(crate) length: u32,
    /// Whether writes reach it; ROM ignores them.
    pub(crate) writable: bool,
}

/// A memory at a fixed address, and how its cycles are timed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixedMemory {
    /// The memory.
    pub(crate) memory: HostMemory,
    /// Its port and the clocks of each of its cycles
    /// ([`FIXED_TIMING`] unless the host set them).
    pub(crate) timing: (PortSize, u8),
}

impl HostMemory {
    /// The byte offset in it of `address`.
    const fn offset(self, address: u32) -> u32 {
        address.wrapping_sub(self.base) % self.length
    }

    /// Whether it holds `address`, at a fixed address.
    const fn holds(self, address: u32) -> bool {
        address >= self.base && address - self.base < self.length
    }
}

/// A memory whose contents go back to an image at some resets
/// ([`Builder::reload_scope`](crate::chip::Builder::reload_scope)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reload {
    /// The core's region holding it.
    pub(crate) region: RegionId,
    /// Its contents as built.
    pub(crate) image: Vec<u8>,
    /// The resets that reload it.
    pub(crate) scope: ResetScope,
}

/// A peripheral on no chip select and the cycles its own decode takes
/// ([`Builder::bus_peripheral`](crate::chip::Builder::bus_peripheral)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BusWiring {
    /// Its index in [`Board::peripherals`].
    pub(crate) peripheral: usize,
    /// The cycles it takes.
    pub(crate) decodes: Vec<BusDecode>,
}

/// What the board wires to a chip-select pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wire {
    /// The host peripheral at this index.
    Peripheral(usize),
    /// Host memory.
    Memory(HostMemory),
}

/// An interrupt source contending at an acknowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The SIM's periodic interrupt timer.
    Timer,
    /// The module at this index.
    Module(usize),
    /// An IRQ pin: the SIM routes the acknowledge to the external bus.
    External,
}

/// What the outputs an [`OutputStop`] watches showed, each masked by it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    /// Ports C, E, F, GP and QS's levels, an undriven pin high.
    ports: [u8; 5],
    /// PWMA's and PWMB's levels, bits 0 and 1.
    pwm: u8,
    /// The frames sent on TXD the host has not taken.
    pub(crate) frames: usize,
}

/// The board.
pub(crate) struct Board {
    /// The SIM.
    pub(crate) sim: Sim,
    /// The other modules on the IMB, in arbitration order on a tie:
    /// the QSM, then the GPT.
    pub(crate) modules: Vec<Box<dyn Part>>,
    /// The host's peripherals.
    pub(crate) peripherals: Vec<Box<dyn Peripheral>>,
    /// The resets each peripheral sees, by its index in `peripherals`.
    pub(crate) reset_scopes: Vec<ResetScope>,
    /// The resets the QSPI's peripheral sees.
    pub(crate) qspi_reset_scope: ResetScope,
    /// The DSACK pins each peripheral drives, if the board says, by its
    /// index in `peripherals`.
    pub(crate) dsack_pins: Vec<Option<Dsack>>,
    /// The `DsackFault`s told of each peripheral, `TOLD_` bits.
    pub(crate) dsack_told: Vec<u8>,
    /// The memories diverted, each to the peripheral that takes its cycles.
    pub(crate) diverted: Vec<(RegionId, usize)>,
    /// The diversions the host's peripherals asked for, not yet made.
    pub(crate) diversion: Diversion,
    /// What each chip-select pin reaches, by [`ChipSelect::index`].
    pub(crate) wiring: [Option<Wire>; ChipSelect::COUNT],
    /// The memories at fixed addresses, which take the program and data
    /// cycles no chip select matches: emulator-only, with no decode on
    /// the chip.  None overlaps another.
    pub(crate) fixed: Vec<FixedMemory>,
    /// The peripherals on no select, in the order added.
    pub(crate) bus: Vec<BusWiring>,
    /// The memories reloaded at resets.
    pub(crate) reloads: Vec<Reload>,
    /// Whether a `reset` instruction's reload awaits the end of its
    /// quantum, the core's memory out of reach within it.
    pub(crate) system_reload: bool,
    /// Peripherals' interrupt outputs and the IRQ pins they drive.
    pub(crate) interrupt_wires: Vec<(usize, InterruptLevel)>,
    /// The IRQ pins the host asserts, bit `n` for IRQ`n`.
    pub(crate) interrupt_pins: u8,
    /// The levels at whose acknowledge the board asserts AVEC, bit `n`
    /// for level `n`.
    pub(crate) autovector_levels: u8,
    /// What happened, for the host.
    pub(crate) events: Vec<Event>,
    /// Whether a decode change awaits a page table rebuild, a write
    /// having come without the core's memory.
    pub(crate) decode_changed: bool,
    /// The host's request to stop the run.
    pub(crate) stop_request: StopRequest,
    /// Whether the core took `stop_request` in the quantum in progress
    /// ([`Bus::stop_requested`](mc683xx_cpu32::bus::Bus::stop_requested)):
    /// the run stops after it even if the quantum also syncs.
    pub(crate) stop_taken: bool,
    /// The host's peripherals' request to end the run early.
    pub(crate) sync_request: SyncRequest,
    /// Where the run's quantum in progress ends.
    pub(crate) quantum_end: Cycles,
    /// Whether an access to the SIM or a module moved its next event
    /// before `quantum_end`, so the quantum ends after the instruction.
    pub(crate) sync: bool,
    /// What the events trace beyond resets and ties.
    pub(crate) tracing: ChipTracing,
    /// Whether a change of the system clock's frequency ends `run_for`
    /// ([`Mc68331::set_clock_stop`](crate::chip::Mc68331::set_clock_stop)).
    pub(crate) clock_stop: bool,
    /// Whether the frequency changed in the quantum in progress.
    pub(crate) clock_moved: bool,
    /// The outputs whose change ends `run_for`
    /// ([`Mc68331::set_output_stop`](crate::chip::Mc68331::set_output_stop)).
    pub(crate) output_stop: OutputStop,
    /// What they showed when last looked at.
    pub(crate) output_seen: Seen,
    /// Whether one changed in the quantum in progress.
    pub(crate) output_moved: bool,
    /// The selects whose blocks hold each page, by page number, as the
    /// last `rebuild` found them: the decode an external cycle need
    /// try, while no decode change awaits a rebuild.
    pub(crate) page_selects: Vec<Selects>,
    /// The level requested of the CPU, while `level_stale` is clear:
    /// the CPU samples it every instruction, and it changes only when
    /// a source does, at a bus cycle to the board, an acknowledge, a
    /// quantum's `advance`, a reset, or the host's change of a pin, a
    /// peripheral or a module.
    pub(crate) level: InterruptLevel,
    /// Whether `level` needs recomputing.
    pub(crate) level_stale: bool,
    /// The earliest next event of the SIM and the modules, while
    /// `modules_next_stale` is clear: the run asks for it every quantum,
    /// and it moves only at an access to the SIM or a module, an
    /// acknowledge, a reset, a restore, the host's change of a module,
    /// or an advance that reaches it (an advance short of a next event
    /// finds nothing to move it).  The peripherals' are the host's, asked
    /// every quantum.
    pub(crate) modules_next: Option<Cycles>,
    /// Whether `modules_next` needs recomputing.
    pub(crate) modules_next_stale: bool,
}

impl fmt::Debug for Board {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Board")
            .field("sim", &self.sim)
            .field("modules", &self.modules.len())
            .field("peripherals", &self.peripherals.len())
            .field("wiring", &self.wiring)
            .finish_non_exhaustive()
    }
}

/// How a bus cycle ended well: its value, right-aligned, and its
/// clocks, those of both byte cycles when a word goes to an 8-bit port
/// (each up to a select's, `u8`).
type Ended = Result<(u32, u16), BusError>;

impl Board {
    /// The module at `index`, which is an `M`.
    pub(crate) fn module<M: Any>(&self, index: usize) -> &M {
        let module: &dyn Any = self.modules[index].as_ref();
        module.downcast_ref().expect("the chip's module")
    }

    /// The module at `index`, which is an `M`, to change.
    pub(crate) fn module_mut<M: Any>(&mut self, index: usize) -> &mut M {
        self.modules_changed();
        let module: &mut dyn Any = self.modules[index].as_mut();
        module.downcast_mut().expect("the chip's module")
    }

    /// The SIM or a module may have changed other than by an advance:
    /// its interrupt request and its next event are found anew.
    pub(crate) const fn modules_changed(&mut self) {
        self.level_stale = true;
        self.modules_next_stale = true;
    }

    /// The earliest next event of the SIM, the modules and the peripherals,
    /// the SIM's and the modules' recomputed only when they may have
    /// moved.
    pub(crate) fn next_event(&mut self) -> Option<Cycles> {
        if self.modules_next_stale {
            self.modules_next = self.modules_next_event();
            self.modules_next_stale = false;
        }
        debug_assert_eq!(
            self.modules_next,
            self.modules_next_event(),
            "a stale next event"
        );
        let peripherals = self
            .peripherals
            .iter()
            .filter_map(|peripheral| peripheral.next_event());
        self.modules_next.into_iter().chain(peripherals).min()
    }

    /// The earliest next event of the SIM and the modules.
    fn modules_next_event(&self) -> Option<Cycles> {
        let modules = self.modules.iter().filter_map(|module| module.next_event());
        self.sim.next_event().into_iter().chain(modules).min()
    }

    /// Whether the host asked the run to stop, taken by the core or not
    /// yet, clearing the request.
    pub(crate) fn take_stop(&mut self) -> bool {
        mem::take(&mut self.stop_taken) | self.stop_request.take()
    }

    /// Brings everything to `now`, the SIM first.
    pub(crate) fn advance(&mut self, now: Cycles) {
        self.level_stale = true;
        if self.modules_next.is_some_and(|next| next <= now) {
            self.modules_next_stale = true;
        }
        self.sim.advance(now);
        for module in &mut self.modules {
            module.advance(now);
        }
        for peripheral in &mut self.peripherals {
            peripheral.advance(now);
        }
    }

    /// What the watched outputs show now ([`Seen`]).
    pub(crate) fn seen(&self) -> Seen {
        let stop = self.output_stop;
        // an undriven pin is high
        let level = |outputs: PortLevels| outputs.pin_levels(u8::MAX);
        let qsm = self.module::<Qsm>(QSM);
        let (port_gp, pwm) = self.module::<Gpt>(GPT).reported_outputs();
        let pwm_mask = u8::from(stop.pwm_a) | u8::from(stop.pwm_b) << 1;
        Seen {
            ports: [
                level(self.sim.outputs(PortName::C)) & stop.port_c,
                level(self.sim.outputs(PortName::E)) & stop.port_e,
                level(self.sim.outputs(PortName::F)) & stop.port_f,
                level(port_gp) & stop.port_gp,
                level(qsm.port_outputs()) & stop.port_qs,
            ],
            pwm: pwm & pwm_mask,
            frames: if stop.txd { qsm.untaken_frames() } else { 0 },
        }
    }

    /// Whether a watched output changed since last looked at; nothing
    /// to look at while none is watched.
    pub(crate) fn outputs_moved(&mut self) -> bool {
        if self.output_stop == OutputStop::default() {
            return false;
        }
        let seen = self.seen();
        let moved = seen != self.output_seen;
        self.output_seen = seen;
        moved
    }

    /// A watched output that changed within an instruction ends the
    /// quantum after it, and the run there.
    fn output_written(&mut self) {
        if self.outputs_moved() {
            self.output_moved = true;
            self.sync = true;
        }
    }

    /// The SIM's system clock may have changed (SYNCR, or a reset): the
    /// QSM learns it.
    pub(crate) fn clock_changed(&mut self) {
        let frequency = self.sim.clock().frequency();
        self.module_mut::<Qsm>(QSM).set_system_clock(frequency);
    }

    /// The modules, and the peripherals `cause` reaches, see RESET at
    /// `now`.
    pub(crate) fn reset_others(&mut self, now: Cycles, cause: ResetCause) {
        self.modules_changed();
        for module in &mut self.modules {
            module.reset(now);
        }
        for (peripheral, scope) in self.peripherals.iter_mut().zip(&self.reset_scopes) {
            if scope.contains(cause) {
                peripheral.reset(now);
            }
        }
        if self.qspi_reset_scope.contains(cause) {
            self.module_mut::<Qsm>(QSM).reset_peripheral(now);
        }
    }

    /// The `reset` instruction at `now` (SIMRM 8.2.6).  A memory it
    /// reloads is reloaded after the instruction, its quantum ending
    /// there, as it ends if the modules' resets move a next event before
    /// the sync point.
    pub(crate) fn reset_instruction(&mut self, now: Cycles) {
        self.sim.system_reset();
        self.reset_others(now, ResetCause::System);
        self.sync_at(self.modules_next_event());
        if self
            .reloads
            .iter()
            .any(|reload| reload.scope.contains(ResetCause::System))
        {
            self.system_reload = true;
            self.sync = true;
        }
        self.events.push(Event::Reset(now, ResetCause::System));
        self.output_written();
        if self.output_stop.reset {
            self.output_moved = true;
            self.sync = true;
        }
    }

    /// The memories `cause` reloads, back to their images.
    pub(crate) fn reload(&self, memory: &mut Memory, cause: ResetCause) {
        for reload in self
            .reloads
            .iter()
            .filter(|reload| reload.scope.contains(cause))
        {
            memory
                .region_mut(reload.region)
                .copy_from_slice(&reload.image);
        }
    }

    /// The fixed memory that takes a cycle at `address` in
    /// `function_code`'s space no chip select matches: never a CPU-space
    /// cycle.
    pub(crate) fn fixed_at(
        &self,
        address: u32,
        function_code: FunctionCode,
    ) -> Option<FixedMemory> {
        if function_code.is_cpu_space() {
            return None;
        }
        self.fixed
            .iter()
            .copied()
            .find(|fixed| fixed.memory.holds(address))
    }

    /// The peripherals on no select whose decode takes a cycle at
    /// `address` in `function_code`'s space, by index.
    fn bus_takers(&self, address: u32, function_code: FunctionCode) -> Vec<usize> {
        self.bus
            .iter()
            .filter(|wiring| {
                wiring
                    .decodes
                    .iter()
                    .any(|decode| decode.takes(address, function_code))
            })
            .map(|wiring| wiring.peripheral)
            .collect()
    }

    /// Whether a bus peripheral's window touches the page at `address`:
    /// its decode is by the byte, so the core serves no byte there.
    fn bus_window_on(&self, address: u32) -> bool {
        self.bus
            .iter()
            .flat_map(|wiring| &wiring.decodes)
            .any(|decode| {
                matches!(*decode, BusDecode::Window { base, length }
                if base.0 < address + PAGE_BYTES && address < base.0 + length)
            })
    }

    /// Whether a peripheral on no select takes a cycle at `address` in
    /// `function_code`'s space.
    pub(crate) fn bus_takes(&self, address: u32, function_code: FunctionCode) -> bool {
        self.bus.iter().any(|wiring| {
            wiring
                .decodes
                .iter()
                .any(|decode| decode.takes(address, function_code))
        })
    }

    /// Each fixed memory's base and length, in the order added.
    pub(crate) fn fixed_layout(&self) -> Vec<(u32, u32)> {
        self.fixed
            .iter()
            .map(|fixed| (fixed.memory.base, fixed.memory.length))
            .collect()
    }

    /// Whether `region` is a fixed memory's.
    pub(crate) fn is_fixed(&self, region: RegionId) -> bool {
        self.fixed.iter().any(|fixed| fixed.memory.region == region)
    }

    /// A bus cycle, byte or word, from the core: an internal module's
    /// if one acknowledges it (MC68331UM 4.8), else the external bus's.
    /// A diversion a peripheral asked for in it takes effect after it.
    pub(crate) fn cycle(
        &mut self,
        mut memory: Option<&mut Memory>,
        address: u32,
        size: Size,
        function_code: FunctionCode,
        now: Cycles,
        write: Option<u32>,
    ) -> Ended {
        let ended = self.decoded_cycle(
            memory.as_deref_mut(),
            address,
            size,
            function_code,
            now,
            write,
        );
        if self.take_diversions()
            && let Some(memory) = memory
        {
            self.rebuild(memory);
        }
        ended
    }

    /// The diversions asked for, made: whether any changed, so the page
    /// tables need rebuilding.
    pub(crate) fn take_diversions(&mut self) -> bool {
        // a fixed memory is never diverted: no select carries its cycle
        let asked: Vec<_> = self
            .diversion
            .take()
            .into_iter()
            .filter(|&(region, _)| !self.is_fixed(region))
            .collect();
        let mut changed = false;
        for (region, peripheral) in asked {
            let before = self.diverted_to(region);
            self.diverted.retain(|&(other, _)| other != region);
            if let Some(peripheral) = peripheral {
                self.diverted.push((region, peripheral));
            }
            changed |= before != peripheral;
        }
        self.decode_changed |= changed;
        changed
    }

    /// Whether a snapshot's diversions fit the board: each to one of
    /// its peripherals, each memory once.
    pub(crate) fn check_diverted(&self, diverted: &[(RegionId, usize)]) -> Result<(), StateError> {
        let regions: BTreeSet<RegionId> = diverted.iter().map(|&(region, _)| region).collect();
        if regions.len() != diverted.len() {
            return Err(StateError::invalid("a memory diverted twice"));
        }
        if diverted
            .iter()
            .any(|&(_, peripheral)| peripheral >= self.peripherals.len())
        {
            return Err(StateError::invalid("a diversion to no peripheral"));
        }
        if diverted.iter().any(|&(region, _)| self.is_fixed(region)) {
            return Err(StateError::invalid("a fixed memory diverted"));
        }
        Ok(())
    }

    /// Whether a snapshot's `DsackFault`s told fit this board: one set
    /// of `TOLD_` bits for each peripheral.
    pub(crate) fn check_dsack_told(&self, told: &[u8]) -> Result<(), StateError> {
        if told.len() != self.peripherals.len() || told.iter().any(|bits| bits & !TOLD_ALL != 0) {
            return Err(StateError::invalid("the DSACK faults told"));
        }
        Ok(())
    }

    /// The peripheral `region`'s cycles go to, while it is diverted.
    pub(crate) fn diverted_to(&self, region: RegionId) -> Option<usize> {
        self.diverted
            .iter()
            .find(|&&(other, _)| other == region)
            .map(|&(_, peripheral)| peripheral)
    }

    /// `cycle`, before any diversion it asks for.
    fn decoded_cycle(
        &mut self,
        memory: Option<&mut Memory>,
        address: u32,
        size: Size,
        function_code: FunctionCode,
        now: Cycles,
        write: Option<u32>,
    ) -> Ended {
        self.level_stale = true;
        if function_code.is_cpu_space() {
            if let Some(value) = write
                && address == LPSTOP_BROADCAST.0
            {
                self.modules_changed();
                self.sim.low_power_stop((value & LPSTOP_MASK) as u8, now);
                self.module_mut::<Gpt>(GPT).set_low_power(true, now);
                self.module_mut::<Qsm>(QSM).stop_clock(now);
                return Ok((0, TABLE_CLOCKS.into()));
            }
        } else if let Some(offset) = address
            .checked_sub(self.sim.module_base())
            .filter(|&offset| offset < MODULE_SPACE_BYTES)
            && let Some(value) = self.internal(offset, size, function_code, now, write)
        {
            if write.is_some() {
                self.output_written();
            }
            if self.decode_changed
                && let Some(memory) = memory
            {
                self.rebuild(memory);
            }
            return Ok((value, TABLE_CLOCKS.into()));
        }
        self.external(memory, address, size, function_code, now, write)
    }

    /// A cycle at `offset` in the module space, if a module
    /// acknowledges it.
    fn internal(
        &mut self,
        offset: u32,
        size: Size,
        function_code: FunctionCode,
        now: Cycles,
        write: Option<u32>,
    ) -> Option<u32> {
        self.modules_changed();
        let access = |block: module::Block| module::RegisterAccess {
            offset: offset - block.offset,
            size,
            supervisor: function_code.is_supervisor(),
            now,
        };
        if BLOCK.contains(offset) {
            let value = match write {
                None => self.sim.read(access(BLOCK)),
                Some(value) => {
                    let frequency = self.sim.clock().frequency();
                    let written = self.sim.write(access(BLOCK), value)?;
                    self.decode_changed |= written.decode;
                    if written.decode {
                        self.clock_changed();
                    }
                    if self.clock_stop && self.sim.clock().frequency() != frequency {
                        // the quantum ends after the instruction
                        self.clock_moved = true;
                        self.sync = true;
                    }
                    Some(0)
                }
            };
            self.sync_at(self.sim.next_event());
            return value;
        }
        let module = self
            .modules
            .iter_mut()
            .find(|module| module.block().contains(offset))?;
        let access = access(module.block());
        let value = match write {
            None => module.read(access),
            Some(value) => module.write(access, value).then_some(0),
        };
        let next = module.next_event();
        self.sync_at(next);
        value
    }

    /// An access moved a next event to `next`: before the sync point,
    /// the quantum ends after the instruction, so the run syncs there
    /// as it would have had the event been known at the quantum's start
    /// (PLAN.md, Peripherals).
    fn sync_at(&mut self, next: Option<Cycles>) {
        self.sync |= next.is_some_and(|next| next < self.quantum_end);
    }

    /// The clocks after which `found` ends a cycle, if it does, and the
    /// port size its DSACK gives for dynamic bus sizing: its own DSACK
    /// at its port field (SIMRM 7.6), or with an external DSACK its
    /// peripheral's (memory answers at once, at the field's size).
    fn dsack(&mut self, found: Match, cycle: BusCycle) -> Option<(u8, PortSize)> {
        if let Some(clocks) = found.termination.clocks(self.sim.clock().eclk_divisor()) {
            return Some((clocks, found.port));
        }
        if !found.asserted {
            return None;
        }
        match self.wiring[found.select.index()]? {
            Wire::Peripheral(peripheral) => {
                self.peripheral_dsack(peripheral, cycle, Some(found.port))
            }
            Wire::Memory(_) => Some((Termination::external_clocks(0), found.port)),
        }
    }

    /// The clocks after which `peripheral`'s own DSACK ends `cycle`, and
    /// the port size it gives, if it does: its wait states, at
    /// [`Self::dsack_port`]'s size.
    fn peripheral_dsack(
        &mut self,
        peripheral: usize,
        cycle: BusCycle,
        field: Option<PortSize>,
    ) -> Option<(u8, PortSize)> {
        let port = self.dsack_port(peripheral, cycle, field)?;
        self.peripherals[peripheral]
            .wait_states(cycle)
            .map(|wait| (Termination::external_clocks(wait), port))
    }

    /// The port size `peripheral`'s DSACK gives `cycle`, if it can end
    /// it: the size its pins encode (SIMRM 5.3.2, Table 5-2), its
    /// `dsack_port` for this cycle or the pins the board gave, else the
    /// select's port `field`; none if PEPAR assigns none of them as
    /// DSACK (SIMRM 9.1), a fault told once for each peripheral
    /// ([`Event::Dsack`]).
    fn dsack_port(
        &mut self,
        peripheral: usize,
        cycle: BusCycle,
        field: Option<PortSize>,
    ) -> Option<PortSize> {
        let pins = match self.peripherals[peripheral].dsack_port(cycle) {
            Some(PortSize::Bits16) => Dsack::Dsack1,
            Some(PortSize::Bits8) => Dsack::Dsack0,
            None => match self.dsack_pins[peripheral] {
                Some(pins) => pins,
                None => return field,
            },
        };
        let assigned = self.sim.port_e_assignment();
        let dsack0 = pins.drives_dsack0() && assigned & PEPA_DSACK0 != 0;
        let dsack1 = pins.drives_dsack1() && assigned & PEPA_DSACK1 != 0;
        if !dsack0 && !dsack1 {
            let fault = match cycle.select.and_then(ChipSelect::from_select) {
                Some(select) => DsackFault::Unassigned { select },
                None => DsackFault::BusUnassigned {
                    peripheral: PeripheralId::from(peripheral as u64),
                },
            };
            self.tell_dsack(peripheral, TOLD_UNASSIGNED, cycle.now, fault);
            return None;
        }
        // DSACK1, alone or with DSACK0, is a 16-bit port (SIMRM Table 5-2)
        Some(if dsack1 {
            PortSize::Bits16
        } else {
            PortSize::Bits8
        })
    }

    /// `fault` of `peripheral` at `now`, if not yet told.
    fn tell_dsack(&mut self, peripheral: usize, told: u8, now: Cycles, fault: DsackFault) {
        if self.dsack_told[peripheral] & told == 0 {
            self.dsack_told[peripheral] |= told;
            self.events.push(Event::Dsack(now, fault));
        }
    }

    /// A cycle on the external bus: the selects it matches take it, the
    /// first DSACK ends it (SIMRM 7.5.1) and gives the port size, and a
    /// word to an 8-bit port becomes two byte cycles (MC68331UM 4.4.2,
    /// SIMRM 5.3.3).  No DSACK, or none
    /// before the bus monitor's period, is a bus error.  One no select
    /// matches goes to a fixed memory that holds it, if one does, in its
    /// own timing ([`FixedMemory::timing`]), a word on an 8-bit port two
    /// cycles; the bus monitor ends one longer than its period.
    fn external(
        &mut self,
        mut memory: Option<&mut Memory>,
        address: u32,
        size: Size,
        function_code: FunctionCode,
        now: Cycles,
        write: Option<u32>,
    ) -> Ended {
        let request = DecodeRequest {
            address,
            function_code,
            write: write.is_some(),
            size,
        };
        let candidates = if self.decode_changed {
            Selects::ALL
        } else {
            self.page_selects[page(address)]
        };
        let mut found: [Option<Match>; ChipSelect::COUNT] = [None; ChipSelect::COUNT];
        for (slot, candidate) in found
            .iter_mut()
            .zip(self.sim.chip_selects().matches_among(request, candidates))
        {
            *slot = Some(candidate);
        }
        debug_assert!(
            self.sim
                .chip_selects()
                .matches(request)
                .eq(found.iter().flatten().copied()),
            "a stale page decode"
        );
        let bus = if self.bus.is_empty() {
            Vec::new()
        } else {
            self.bus_takers(address, function_code)
        };
        if found[0].is_none()
            && bus.is_empty()
            && let Some(fixed) = self.fixed_at(address, function_code)
        {
            let (port, clocks) = fixed.timing;
            if self.sim.bus_monitor().is_some_and(|limit| clocks > limit) {
                return Err(BusError);
            }
            let cycles: u16 = if size == Size::Word && port == PortSize::Bits8 {
                2
            } else {
                1
            };
            let value = match (write, memory) {
                (Some(data), Some(memory)) if fixed.memory.writable => {
                    store(memory, fixed.memory, address, size, Lanes::BOTH, data);
                    0
                }
                (None, Some(memory)) => load(memory, fixed.memory, address, size),
                _ => 0,
            };
            return Ok((value, cycles * u16::from(clocks)));
        }
        let mut first: Option<(u8, PortSize)> = None;
        for &candidate in found.iter().flatten() {
            let cycle =
                BusCycle::new(address, size, function_code, now).through(candidate.select.into());
            if let Some((clocks, port)) = self.dsack(candidate, cycle)
                && first.is_none_or(|(earliest, _)| clocks < earliest)
            {
                first = Some((clocks, port));
            }
        }
        let bus_cycle = BusCycle::new(address, size, function_code, now);
        for &peripheral in &bus {
            if let Some((clocks, port)) = self.peripheral_dsack(peripheral, bus_cycle, None)
                && first.is_none_or(|(earliest, _)| clocks < earliest)
            {
                first = Some((clocks, port));
            }
        }
        let (clocks, port) = first.ok_or(BusError)?;
        if self.sim.bus_monitor().is_some_and(|limit| clocks > limit) {
            return Err(BusError);
        }
        if size == Size::Word && port == PortSize::Bits8 {
            let byte = |board: &mut Self, memory: Option<&mut Memory>, at: u32, value| {
                board.external(memory, at, Size::Byte, function_code, now, value)
            };
            let (high, first_clocks) = byte(
                self,
                memory.as_deref_mut(),
                address,
                write.map(|value| value >> 8),
            )?;
            let (low, second_clocks) = byte(
                self,
                memory,
                address + 1,
                write.map(|value| value & LOWER_LANE),
            )?;
            return Ok((high << 8 | low, first_clocks + second_clocks));
        }
        let mut value = 0;
        let mut driven = Lanes {
            upper: false,
            lower: false,
        };
        for candidate in found
            .into_iter()
            .flatten()
            .filter(|candidate| candidate.asserted)
        {
            let Some(wire) = self.wiring[candidate.select.index()] else {
                continue;
            };
            let cycle =
                BusCycle::new(address, size, function_code, now).through(candidate.select.into());
            let lanes = candidate.lanes;
            let wire = match wire {
                Wire::Memory(wired) => self
                    .diverted_to(wired.region)
                    .map_or(wire, Wire::Peripheral),
                Wire::Peripheral(_) => wire,
            };
            match (wire, write) {
                (Wire::Peripheral(peripheral), Some(data)) => {
                    self.peripherals[peripheral].write(cycle, data)?
                }
                (Wire::Peripheral(peripheral), None) => {
                    let read = self.peripherals[peripheral].read(cycle)?;
                    value = merge(value, read, size, lanes, &mut driven);
                }
                (Wire::Memory(wired), Some(data)) => {
                    if wired.writable
                        && let Some(memory) = memory.as_deref_mut()
                    {
                        store(memory, wired, address, size, lanes, data);
                    }
                }
                (Wire::Memory(wired), None) => {
                    if let Some(memory) = memory.as_deref() {
                        let read = load(memory, wired, address, size);
                        value = merge(value, read, size, lanes, &mut driven);
                    }
                }
            }
        }
        for peripheral in bus {
            match write {
                Some(data) => self.peripherals[peripheral].write(bus_cycle, data)?,
                None => {
                    let read = self.peripherals[peripheral].read(bus_cycle)?;
                    value = merge(value, read, size, Lanes::BOTH, &mut driven);
                }
            }
        }
        Ok((value, clocks.into()))
    }

    /// The level of the highest IRQ pin asserted with its interrupt
    /// function (MC68331UM 4.7.2).
    fn external_level(&self) -> InterruptLevel {
        let driven = self
            .interrupt_wires
            .iter()
            .filter(|(peripheral, _)| self.peripherals[*peripheral].interrupt_request())
            .fold(self.interrupt_pins, |pins, (_, level)| {
                pins | 1 << level.get()
            });
        (1..=InterruptLevel::NONMASKABLE.get())
            .rev()
            .filter_map(InterruptLevel::new)
            .find(|&level| driven & 1 << level.get() != 0 && self.sim.interrupt_pin(level))
            .unwrap_or(InterruptLevel::NONE)
    }

    /// The highest level requested of the CPU, recomputed only when a
    /// source may have changed.
    pub(crate) fn level(&mut self) -> InterruptLevel {
        if self.level_stale {
            self.level = self.requested_level();
            self.level_stale = false;
        }
        debug_assert_eq!(self.level, self.requested_level(), "a stale level");
        self.level
    }

    /// The highest level the sources request now.  In low-power stop
    /// only the PIT and the IRQ pins can wake the CPU: the other
    /// modules' clocks are off, their requests waiting for the wake
    /// (MC68331UM 4.2.12, 4.3.4).
    fn requested_level(&self) -> InterruptLevel {
        let awake = self.sim.low_power().is_none();
        let modules = self
            .modules
            .iter()
            .filter(|_| awake)
            .filter_map(|module| module.request())
            .map(|request| request.level);
        self.sim
            .timer_request()
            .into_iter()
            .chain(modules)
            .chain([self.external_level()])
            .max()
            .unwrap_or(InterruptLevel::NONE)
    }

    /// The acknowledge of `level` at `now`: arbitration by IARB among
    /// the sources requesting it (SIMRM 6.3), the SIM's timer before
    /// its IRQ pins (MC68331UM 4.7.3).  No contender is a spurious
    /// interrupt.
    pub(crate) fn acknowledge(&mut self, level: InterruptLevel, now: Cycles) -> Acknowledged {
        self.modules_changed();
        self.wake(now);
        let sim = self.sim.arbitration();
        let timer = (self.sim.timer_request() == Some(level)).then_some((sim, Source::Timer));
        let modules = self
            .modules
            .iter()
            .enumerate()
            .filter_map(|(index, module)| module.request().map(|request| (index, request)))
            .filter(|(_, request)| request.level == level)
            .map(|(index, request)| (request.arbitration, Source::Module(index)));
        let external = (self.external_level() == level).then_some((sim, Source::External));
        let contenders: Vec<(u8, Source)> = timer
            .into_iter()
            .chain(modules)
            .chain(external)
            .filter(|&(arbitration, _)| arbitration != 0)
            .collect();
        // the first of the highest IARB wins; the timer and the pins
        // share the SIM's
        let Some(&(arbitration, winner)) = contenders
            .iter()
            .rev()
            .max_by_key(|(arbitration, _)| *arbitration)
        else {
            return Acknowledge::Spurious.into();
        };
        let modules_at = contenders
            .iter()
            .filter(|(other, source)| *other == arbitration && !is_sim(*source))
            .count();
        let sim_at = contenders
            .iter()
            .any(|(other, source)| *other == arbitration && is_sim(*source));
        if modules_at + usize::from(sim_at) > 1 {
            self.events
                .push(Event::ArbitrationTie { level, arbitration });
        }
        match winner {
            Source::Timer => Acknowledge::Vector(self.sim.acknowledge_timer()).into(),
            Source::Module(index) => self.modules[index]
                .acknowledge(level, now)
                .map_or(Acknowledge::Spurious, Acknowledge::Vector)
                .into(),
            Source::External => self.external_acknowledge(level, now),
        }
    }

    /// An interrupt ends low-power stop at `now`, if the MCU is in it:
    /// the SIM's watchdog and the other modules' clocks run again
    /// (MC68331UM 4.2.12, 4.3.4), their next events found anew, before
    /// the sync point ending the quantum.  A reset ends it by resetting
    /// them.
    fn wake(&mut self, now: Cycles) {
        if self.sim.low_power().is_some() {
            self.sim.wake(now);
            self.module_mut::<Gpt>(GPT).set_low_power(false, now);
            self.module_mut::<Qsm>(QSM).start_clock(now);
            self.sync_at(self.modules_next_event());
        }
    }

    /// The acknowledge cycle the SIM passes to the external bus: a
    /// select's AVEC, then the board's AVEC pin, then a peripheral's vector
    /// (SIMRM 6.4, 7.8); nothing answering is a spurious interrupt.  A
    /// peripheral's vector costs its cycle's clocks, a bus monitor time-out
    /// its period; an autovector costs none beyond the interrupt's
    /// (PLAN.md).
    fn external_acknowledge(&mut self, level: InterruptLevel, now: Cycles) -> Acknowledged {
        let address = iack_address(level);
        let request = DecodeRequest {
            address,
            function_code: FunctionCode::CPU_SPACE,
            write: false,
            size: Size::Byte,
        };
        if self
            .sim
            .chip_selects()
            .matches(request)
            .any(|found| found.autovector)
            || self.sim.autovector_pin() && self.autovector_levels & 1 << level.get() != 0
        {
            return Acknowledge::Autovector.into();
        }
        match self.external(
            None,
            address,
            Size::Byte,
            FunctionCode::CPU_SPACE,
            now,
            None,
        ) {
            Ok((vector, clocks)) => Acknowledged {
                answer: Acknowledge::Vector(vector as u8),
                stall: stall(clocks),
            },
            // no DSACK in time: the bus monitor's BERR ends the cycle
            // at its period (SIMRM 3.2, 6.4.3); with BME clear, a bus
            // error at once (PLAN.md, A cycle no DSACK ends)
            Err(BusError) => Acknowledged {
                answer: Acknowledge::Spurious,
                stall: self
                    .sim
                    .bus_monitor()
                    .map_or(Cycles::ZERO, |limit| stall(limit.into())),
            },
        }
    }

    /// Rebuilds the core's page tables from the chip-select decode
    /// (PLAN.md, Memory): a page is served in the core where memory
    /// alone answers every byte and word cycle of a direction, with
    /// its select's timing; everything else, and the module space,
    /// goes to the bus.
    pub(crate) fn rebuild(&mut self, memory: &mut Memory) {
        self.decode_changed = false;
        self.decode_pages();
        memory.clear();
        let mut covered = vec![false; PAGES];
        let fixed = self
            .fixed
            .iter()
            .map(|fixed| (fixed.memory.base, fixed.memory.length));
        for (start, length) in self.sim.chip_selects().blocks().chain(fixed) {
            let first = (start / PAGE_BYTES) as usize;
            let count = (length / PAGE_BYTES) as usize;
            covered[first..(first + count).min(PAGES)].fill(true);
        }
        let modules = self.sim.module_base();
        for page in (0..PAGES).filter(|&page| covered[page]) {
            let address = page as u32 * PAGE_BYTES;
            if (modules..modules + MODULE_SPACE_BYTES).contains(&address)
                || self.bus_window_on(address)
            {
                continue;
            }
            for (space, function_code) in [
                (Space::User, FunctionCode::USER_DATA),
                (Space::Supervisor, FunctionCode::SUPERVISOR_DATA),
            ] {
                let read = self.served(address, function_code, false);
                let write = self
                    .served(address, function_code, true)
                    .filter(|write| read.is_none_or(|read| read.1 == write.1));
                let Some((_, timing)) = read.or(write) else {
                    continue;
                };
                let page = Page {
                    read: read.map(|(backing, _)| backing),
                    write: write.map(|(backing, _)| backing),
                    port: timing.0,
                    clocks: timing.1,
                };
                memory
                    .map(space, Address(address), PAGE_BYTES, page)
                    .expect("a whole page inside its region");
            }
        }
    }

    /// Records the selects whose blocks hold each page, from the
    /// chip-select decode.
    pub(crate) fn decode_pages(&mut self) {
        let chip_selects = self.sim.chip_selects();
        self.page_selects = (0..PAGES)
            .map(|page| chip_selects.at(page as u32 * PAGE_BYTES))
            .collect();
    }

    /// Where the page at `address` reads or writes in host memory, and
    /// its port and clocks, if memory alone answers every cycle of the
    /// direction: an even byte, an odd byte and a word.
    fn served(
        &self,
        address: u32,
        function_code: FunctionCode,
        write: bool,
    ) -> Option<(Backing, (PortSize, u8))> {
        let kinds = [
            (address, Size::Byte),
            (address + 1, Size::Byte),
            (address, Size::Word),
        ];
        let mut served = None;
        for (at, size) in kinds {
            let request = DecodeRequest {
                address: at,
                function_code,
                write,
                size,
            };
            let (wired, timing) = self.memory_alone(request)?;
            if served.is_some_and(|(other, _): (HostMemory, _)| other != wired) {
                return None;
            }
            served = Some((wired, timing));
        }
        let (wired, timing) = served?;
        let backing = Backing {
            region: wired.region,
            offset: wired.offset(address),
        };
        Some((backing, timing))
    }

    /// The memory that alone answers `request`, with the port and
    /// clocks of the select that ends it, or a fixed memory's own
    /// ([`FixedMemory::timing`]) where no select matches.
    pub(crate) fn memory_alone(
        &self,
        request: DecodeRequest,
    ) -> Option<(HostMemory, (PortSize, u8))> {
        if self.bus_takes(request.address, request.function_code) {
            return None;
        }
        if self.sim.chip_selects().matches(request).next().is_none() {
            return self
                .fixed_at(request.address, request.function_code)
                .filter(|fixed| {
                    (!request.write || fixed.memory.writable)
                        && self
                            .sim
                            .bus_monitor()
                            .is_none_or(|limit| fixed.timing.1 <= limit)
                })
                .map(|fixed| (fixed.memory, fixed.timing));
        }
        let eclk = self.sim.clock().eclk_divisor();
        let mut first: Option<(u8, PortSize)> = None;
        let mut wired = None;
        let mut lanes = Lanes {
            upper: false,
            lower: false,
        };
        for found in self.sim.chip_selects().matches(request) {
            let wire = if found.asserted {
                self.wiring[found.select.index()]
            } else {
                None
            };
            let clocks = match (found.termination.clocks(eclk), wire) {
                (Some(clocks), _) => Some(clocks),
                (None, Some(Wire::Memory(_))) => Some(Termination::external_clocks(0)),
                (None, Some(Wire::Peripheral(_))) => return None,
                (None, None) => None,
            };
            if let Some(clocks) = clocks
                && first.is_none_or(|(earliest, _)| clocks < earliest)
            {
                first = Some((clocks, found.port));
            }
            match wire {
                Some(Wire::Memory(memory)) if self.diverted_to(memory.region).is_some() => {
                    return None;
                }
                Some(Wire::Memory(memory)) if wired.is_none_or(|other| other == memory) => {
                    if request.write && !memory.writable {
                        return None;
                    }
                    wired = Some(memory);
                    lanes = lanes.or(found.lanes);
                }
                Some(_) => return None,
                None => {}
            }
        }
        let (clocks, port) = first?;
        let needed = match request.size {
            Size::Byte if request.address & 1 == 0 => Lanes {
                upper: true,
                lower: false,
            },
            Size::Byte => Lanes {
                upper: false,
                lower: true,
            },
            _ => Lanes::BOTH,
        };
        if self.sim.bus_monitor().is_some_and(|limit| clocks > limit) || !lanes.covers(needed) {
            return None;
        }
        wired.map(|memory| (memory, (port, clocks)))
    }
}
/// The core's memory on the chip's address lines.  It cannot fail: the
/// core's memory takes more lines than a page's and at most 32, as the
/// assertion holds [`ADDRESS_LINES`] to.
pub(crate) fn chip_memory() -> Memory {
    const {
        assert!(ADDRESS_LINES > PAGE_BYTES.trailing_zeros() && ADDRESS_LINES <= u32::BITS);
    }
    Memory::new(ADDRESS_LINES).expect("24 address lines")
}

/// The number of the page holding `address`.
const fn page(address: u32) -> usize {
    (address / PAGE_BYTES) as usize % PAGES
}

/// The clocks of a cycle of `clocks` beyond the timing tables' bus
/// cycle: what the core adds to an instruction or interrupt.
pub(crate) fn stall(clocks: u16) -> Cycles {
    Cycles(u64::from(clocks.saturating_sub(TABLE_CLOCKS.into())))
}

/// Whether `source` arbitrates with the SIM's IARB.
const fn is_sim(source: Source) -> bool {
    matches!(source, Source::Timer | Source::External)
}

/// `read`, a driver's value for a cycle of `size`, merged into `value`
/// on the `lanes` it drives that no driver before it has.
fn merge(value: u32, read: u32, size: Size, lanes: Lanes, driven: &mut Lanes) -> u32 {
    if size == Size::Byte {
        if driven.is_empty() {
            *driven = Lanes::BOTH;
            return read & LOWER_LANE;
        }
        return value;
    }
    let mut merged = value;
    if lanes.upper && !driven.upper {
        merged = merged & LOWER_LANE | read & UPPER_LANE;
    }
    if lanes.lower && !driven.lower {
        merged = merged & UPPER_LANE | read & LOWER_LANE;
    }
    *driven = driven.or(lanes);
    merged
}

/// A byte or word of host memory, big-endian.
pub(crate) fn load(memory: &Memory, wired: HostMemory, address: u32, size: Size) -> u32 {
    let bytes = memory.region(wired.region);
    let offset = wired.offset(address) as usize;
    match size {
        Size::Byte => u32::from(bytes[offset]),
        _ => u32::from(u16::from_be_bytes([bytes[offset], bytes[offset + 1]])),
    }
}

/// A byte or word into host memory, on the `lanes` the select strobes.
fn store(
    memory: &mut Memory,
    wired: HostMemory,
    address: u32,
    size: Size,
    lanes: Lanes,
    value: u32,
) {
    let bytes = memory.region_mut(wired.region);
    let offset = wired.offset(address) as usize;
    match size {
        Size::Byte => bytes[offset] = value as u8,
        _ => {
            if lanes.upper {
                bytes[offset] = (value >> 8) as u8;
            }
            if lanes.lower {
                bytes[offset + 1] = value as u8;
            }
        }
    }
}
