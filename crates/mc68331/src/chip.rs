//! The MC68331 as the host builds and runs it: the CPU32 core and the
//! board around it, the host's peripherals and memory wired to
//! chip-select pins as the board wires them.

use std::any::Any;
use std::cell::Cell;
use std::cell::RefCell;
use std::fmt;
use std::mem;
use std::rc::Rc;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::Bus;
use mc683xx_cpu32::bus::BusAccess;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::bus::Read;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::Run;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cpu::hooks::ExceptionEntry;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::Acknowledged;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::interrupt::Interrupts;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::memory::MemoryError;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::memory::RegionId;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_cpu32::registers::Registers;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::peripheral::SpiPeripheral;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::qsm::Qsm;
use mc683xx_modules::qsm::qspi::AnySpiPeripheral;
use mc683xx_modules::qsm::qspi::MOST_BITS;
use mc683xx_modules::qsm::qspi::Shifted;
use mc683xx_modules::qsm::sci::Frame;
use mc683xx_modules::qsm::sci::Transmitted;
use mc683xx_modules::sim::Configuration;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::Sim;
use mc683xx_modules::sim::ports::Change;
use mc683xx_modules::sim::ports::PortName;

use crate::board::ADDRESS_LINES;
use crate::board::Board;
use crate::board::BusWiring;
use crate::board::FIXED_TIMING;
use crate::board::FixedMemory;
use crate::board::GPT;
use crate::board::HostMemory;
use crate::board::QSM;
use crate::board::Reload;
use crate::board::Seen;
use crate::board::Wire;
use crate::board::chip_memory;
use crate::board::stall;
use crate::event;
use crate::event::ChipTracing;
use crate::event::Event;
use crate::gpt::Gpt;
use crate::gpt::Input;
use crate::gpt::OutputChange;
use crate::gpt::pwm::Channel;
use crate::gpt::pwm::Output;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::chip_selects::Termination;
use mc683xx_modules::sim::reset::ResetScope;

/// The index in the chip's peripherals of `id`, which the chip numbers
/// from 0 in the order added.
fn index(id: PeripheralId) -> usize {
    usize::try_from(u64::from(id)).unwrap_or(usize::MAX)
}

/// Host memory wired into the chip, as [`Builder::memory`] returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemoryId(pub(crate) RegionId);

/// A handle by which the host's peripherals end the chip's run early.
///
/// After the instruction whose cycle asked, or at the sync point if no
/// instruction is running (a peripheral's `advance`, a reset), `run_for`
/// returns [`Stop::Budget`] with the clocks run so far.  Not a stop: it
/// is for a host whose own scheduler must see what a peripheral did within
/// the run before the run goes on (a board's nets), and that runs on.
/// Asked between runs it does nothing.  Clones share one request.
#[derive(Debug, Clone, Default)]
pub struct SyncRequest(Rc<Cell<bool>>);

impl SyncRequest {
    /// Asks the run to end early.
    pub fn request(&self) {
        self.0.set(true);
    }

    /// Whether the run's end is asked for.
    pub(crate) fn get(&self) -> bool {
        self.0.get()
    }

    /// Whether the run's end is asked for; clears the request.
    pub(crate) fn take(&self) -> bool {
        self.0.replace(false)
    }
}

/// The chip's outputs whose change ends `run_for` after its instruction
/// ([`Mc68331::set_output_stop`]).
///
/// For a host whose peripherals sense them and may answer within the
/// run, so the run is the same however the host cuts it.  A pin's
/// level is its port's, an undriven pin's high; none by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OutputStop {
    /// Port C's pins, a mask: chip selects as discrete outputs.
    pub port_c: u8,
    /// Port E's.
    pub port_e: u8,
    /// Port F's.
    pub port_f: u8,
    /// Port GP's, as [`Mc68331::take_gpt_changes`] reports them.
    pub port_gp: u8,
    /// Port QS's, PCS\[3:0\] and TXD among them as levels.
    pub port_qs: u8,
    /// PWMA, while its edges are reported (`set_report_pwm_edges`).
    pub pwm_a: bool,
    /// PWMB, likewise.
    pub pwm_b: bool,
    /// A frame's start on TXD (`take_sci_transmitted`).
    pub txd: bool,
    /// A reset the chip drives on RESET: a `reset` instruction's, the
    /// watchdog's or the halt monitor's.
    pub reset: bool,
}

/// A handle by which the host's peripherals divert a memory's cycles to a
/// peripheral and back (a flash answering status reads while it programs).
///
/// Asked during a cycle, the diversion takes effect when the cycle
/// ends, the core's fast path for the memory's pages with it; asked
/// between runs, at the next cycle or sync point.  Clones share one
/// queue.
#[derive(Debug, Clone, Default)]
pub struct Diversion(Rc<RefCell<Vec<Diverted>>>);

/// A diversion asked for: a memory's region, and the peripheral's index or
/// `None` to restore it.
type Diverted = (RegionId, Option<usize>);

impl Diversion {
    /// Every cycle that reaches `memory` reaches `peripheral` instead, as a
    /// cycle on the select that carried it, until `restore`; the core
    /// serves none of its pages meanwhile.
    pub fn divert(&self, memory: MemoryId, peripheral: PeripheralId) {
        self.0
            .borrow_mut()
            .push((memory.0, Some(index(peripheral))));
    }

    /// `memory`'s cycles reach it again, its pages back on the fast
    /// path.
    pub fn release(&self, memory: MemoryId) {
        self.0.borrow_mut().push((memory.0, None));
    }

    /// The diversions asked for since the last call.
    pub(crate) fn take(&self) -> Vec<Diverted> {
        mem::take(&mut *self.0.borrow_mut())
    }
}

/// What can go wrong wiring the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// A pin already has something wired to it.
    #[error("{0} is already wired")]
    Wired(ChipSelect),
    /// Memory that is not a whole number of 2-Kbyte pages, the
    /// smallest chip-select block (SIMRM Table 7-2).
    #[error("{0:#x} bytes: not a whole number of pages")]
    Length(usize),
    /// A fixed memory's base off a page boundary, or the memory past
    /// ADDR\[23:0\] (MC68331UM 4.4.1.1).
    #[error("{base}: a fixed memory of {length:#x} bytes does not fit there")]
    Placement {
        /// Its base.
        base: Address,
        /// Its bytes.
        length: usize,
    },
    /// A fixed memory overlapping another.
    #[error("the fixed memory at {base} overlaps the one at {other}")]
    Overlap {
        /// Its base.
        base: Address,
        /// The other's.
        other: Address,
    },
    /// A bus peripheral's window that is empty or runs past ADDR\[23:0\].
    #[error("{base}: a bus window of {length:#x} bytes does not fit ADDR[23:0]")]
    Window {
        /// Its base.
        base: Address,
        /// Its bytes.
        length: u32,
    },
    /// Timing for a memory that is not a fixed memory.
    #[error("not a fixed memory")]
    NotFixed,
    /// A fixed memory's termination other than fast or 0 to 13 wait
    /// states.
    #[error("a fixed memory's cycles end on fast termination or 0 to 13 wait states")]
    Termination,
    /// The core's memory refused it.
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

/// The board as the host wires it, before the power-on reset.
pub struct Builder {
    /// The pins the SIM reads at reset.
    configuration: Configuration,
    /// The core's memory, holding the host's regions.
    memory: Memory,
    /// The board under construction.
    board: Board,
}

impl fmt::Debug for Builder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Builder")
            .field("configuration", &self.configuration)
            .field("board", &self.board)
            .finish_non_exhaustive()
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new(Configuration::default())
    }
}

impl Builder {
    /// A board with nothing wired, whose reset reads `configuration`.
    #[must_use]
    pub fn new(configuration: Configuration) -> Self {
        let sim = Sim::new(configuration, Cycles::ZERO);
        let system_clock = sim.clock().frequency();
        Self {
            configuration,
            memory: chip_memory(),
            board: Board {
                sim,
                modules: vec![
                    Box::new(Qsm::new(system_clock)),
                    Box::new(Gpt::new(Cycles::ZERO)),
                ],
                peripherals: Vec::new(),
                reset_scopes: Vec::new(),
                qspi_reset_scope: ResetScope::ALL,
                dsack_pins: Vec::new(),
                dsack_told: Vec::new(),
                diverted: Vec::new(),
                diversion: Diversion::default(),
                wiring: [None; ChipSelect::COUNT],
                fixed: Vec::new(),
                bus: Vec::new(),
                reloads: Vec::new(),
                system_reload: false,
                interrupt_wires: Vec::new(),
                interrupt_pins: 0,
                autovector_levels: 0,
                events: Vec::new(),
                decode_changed: false,
                stop_request: StopRequest::default(),
                stop_taken: false,
                sync_request: SyncRequest::default(),
                quantum_end: Cycles::ZERO,
                sync: false,
                clock_stop: false,
                clock_moved: false,
                output_stop: OutputStop::default(),
                output_seen: Seen::default(),
                output_moved: false,
                tracing: ChipTracing::default(),
                page_selects: Vec::new(),
                level: InterruptLevel::NONE,
                level_stale: true,
                modules_next: None,
                modules_next_stale: true,
            },
        }
    }

    /// The handle by which the host's peripherals divert a memory's cycles
    /// to a peripheral ([`Diversion`]), for them to hold.
    #[must_use]
    pub fn diversion(&self) -> Diversion {
        self.board.diversion.clone()
    }

    /// The system clock's frequency out of the power-on reset, as the
    /// configuration's clock pins give it (MC68331UM 4.3).
    #[must_use]
    pub const fn system_frequency(&self) -> Frequency {
        self.board.sim.clock().frequency()
    }

    /// The handle by which the host asks the chip's runs to stop, for
    /// its peripherals to hold.
    #[must_use]
    pub fn stop_request(&self) -> StopRequest {
        self.board.stop_request.clone()
    }

    /// The handle by which the host's peripherals end the chip's runs early
    /// ([`SyncRequest`]), for them to hold.
    #[must_use]
    pub fn sync_request(&self) -> SyncRequest {
        self.board.sync_request.clone()
    }

    /// Wires `wire` to `pins`.
    fn wire(&mut self, wire: Wire, pins: &[ChipSelect]) -> Result<(), BuildError> {
        if let Some(&pin) = pins
            .iter()
            .find(|pin| self.board.wiring[pin.index()].is_some())
        {
            return Err(BuildError::Wired(pin));
        }
        for pin in pins {
            self.board.wiring[pin.index()] = Some(wire);
        }
        Ok(())
    }

    /// Memory of `bytes`, writable or not, on `pins`: every cycle
    /// those selects take, in any space, reads or writes it, its
    /// address pins on ADDR from bit 0 up (a block larger than it
    /// repeats it).  The core serves it itself wherever no peripheral
    /// shares the cycle.
    ///
    /// # Errors
    ///
    /// [`BuildError::Length`] if `bytes` is not a whole number of
    /// pages, [`BuildError::Wired`] if one of `pins` is already wired,
    /// [`BuildError::Memory`] if the core's memory refuses it.
    pub fn memory(
        &mut self,
        bytes: Vec<u8>,
        writable: bool,
        pins: &[ChipSelect],
    ) -> Result<MemoryId, BuildError> {
        let length = pages(&bytes)?;
        let region = self.memory.add_region(bytes);
        let wired = HostMemory {
            region,
            base: 0,
            length,
            writable,
        };
        self.wire(Wire::Memory(wired), pins)?;
        Ok(MemoryId(region))
    }

    /// Memory of `bytes`, writable or not, at `base` up: every program
    /// and data cycle there that no chip select matches, in any space,
    /// reads or writes it, at once, with no wait states and no DSACK
    /// (a 16-bit port costing nothing beyond the timing tables, as an
    /// internal module).  A select that matches a cycle takes it as on
    /// the chip, and the internal modules win over both (MC68331UM 4.8);
    /// CPU-space cycles never reach it, and it is never diverted
    /// ([`Diversion`]).  Out of reset only CSBOOT decodes, so it answers
    /// at once wherever CSBOOT's block does not reach.
    ///
    /// Emulator-only: the chip has no such decode.  It is for code and
    /// data the host maps without repurposing a select (a rewritten
    /// routine, a test stub), the firmware's image left as it is.  Its
    /// contents survive every reset unless `reload_scope` says.
    ///
    /// # Errors
    ///
    /// [`BuildError::Length`] if `bytes` is not a whole number of
    /// pages, [`BuildError::Placement`] if `base` is not on a page
    /// boundary or the memory runs past ADDR\[23:0\],
    /// [`BuildError::Overlap`] if it overlaps another fixed memory,
    /// [`BuildError::Memory`] if the core's memory refuses it.
    pub fn fixed_memory(
        &mut self,
        base: Address,
        bytes: Vec<u8>,
        writable: bool,
    ) -> Result<MemoryId, BuildError> {
        let length = pages(&bytes)?;
        let placement = BuildError::Placement {
            base,
            length: bytes.len(),
        };
        if !base.0.is_multiple_of(PAGE_BYTES)
            || base
                .0
                .checked_add(length)
                .is_none_or(|end| end > 1 << ADDRESS_LINES)
        {
            return Err(placement);
        }
        if let Some(other) = self
            .board
            .fixed
            .iter()
            .map(|other| other.memory)
            .find(|other| base.0 < other.base + other.length && other.base < base.0 + length)
        {
            return Err(BuildError::Overlap {
                base,
                other: Address(other.base),
            });
        }
        let region = self.memory.add_region(bytes);
        self.board.fixed.push(FixedMemory {
            memory: HostMemory {
                region,
                base: base.0,
                length,
                writable,
            },
            timing: FIXED_TIMING,
        });
        Ok(MemoryId(region))
    }

    /// The port and the termination of fixed memory `memory`'s cycles,
    /// as a chip select's option register's DSACK field would give them
    /// (MC68331UM D.3.26): [`Termination::Fast`] on a 16-bit port by
    /// default, a cycle that costs nothing beyond the timing tables;
    /// [`Termination::WaitStates`] for slower memory, a word on an
    /// 8-bit port taking two cycles.  The bus monitor ends a cycle that
    /// takes longer than its period, as it does a select's.
    /// Emulator-only, as the memory is.
    ///
    /// # Errors
    ///
    /// [`BuildError::NotFixed`] if `memory` is not a fixed memory,
    /// [`BuildError::Termination`] for a termination other than fast or
    /// 0 to 13 wait states: a fixed memory asserts no DSACK and runs on
    /// no ECLK.
    pub fn fixed_timing(
        &mut self,
        memory: MemoryId,
        port: PortSize,
        termination: Termination,
    ) -> Result<(), BuildError> {
        let clocks = match termination {
            Termination::Fast | Termination::WaitStates(..=Termination::MOST_WAIT_STATES) => {
                termination.clocks(0)
            }
            _ => None,
        }
        .ok_or(BuildError::Termination)?;
        let fixed = self
            .board
            .fixed
            .iter_mut()
            .find(|fixed| fixed.memory.region == memory.0)
            .ok_or(BuildError::NotFixed)?;
        fixed.timing = (port, clocks);
        Ok(())
    }

    /// The resets that put `memory`'s contents back to its bytes as
    /// built, now: none by default, its contents surviving every reset
    /// as RAM's do.  A reset in `scope` (the power-on reset, RESET from
    /// the board, the watchdog's, the halt monitor's, the `reset`
    /// instruction's) reloads it before the CPU fetches its reset
    /// vectors, the `reset` instruction's after the instruction: code a
    /// warm restart must find as built.  Emulator-only, as no memory
    /// reloads itself; for any memory, a fixed one or one on pins.
    pub fn reload_scope(&mut self, memory: MemoryId, scope: ResetScope) {
        self.board
            .reloads
            .retain(|reload| reload.region != memory.0);
        if scope != ResetScope::NONE {
            self.board.reloads.push(Reload {
                region: memory.0,
                image: self.memory.region(memory.0).to_vec(),
                scope,
            });
        }
    }

    /// Adds `peripheral`, which every reset reaches until `reset_scope`
    /// says otherwise.
    fn add<D: MappedPeripheral + Timed + Save + Any>(&mut self, peripheral: D) -> PeripheralId {
        self.board.peripherals.push(Box::new(peripheral));
        self.board.reset_scopes.push(ResetScope::ALL);
        self.board.dsack_pins.push(None);
        self.board.dsack_told.push(0);
        PeripheralId::from((self.board.peripherals.len() - 1) as u64)
    }

    /// A peripheral on `pins`, which it tells apart by
    /// [`BusCycle::select`](mc683xx_cpu32::peripheral::BusCycle::select).
    /// It says whether it saves ([`Save`]): the chip's snapshot holds
    /// every peripheral and model, in the order added.
    ///
    /// # Errors
    ///
    /// [`BuildError::Wired`] if one of `pins` is already wired.
    pub fn peripheral<D: MappedPeripheral + Timed + Save + Any>(
        &mut self,
        peripheral: D,
        pins: &[ChipSelect],
    ) -> Result<PeripheralId, BuildError> {
        let id = self.board.peripherals.len();
        self.wire(Wire::Peripheral(id), pins)?;
        Ok(self.add(peripheral))
    }

    /// A peripheral on no chip select that takes the cycles `decodes`
    /// give by its own decode of the address bus, as board logic does:
    /// an address window, an interrupt acknowledge, or both.  It drives
    /// DSACK `pins` itself, as a peripheral on an externally terminated
    /// select does ([`Builder::dsack`]): its `wait_states` time each
    /// cycle, the pins' port sizes it (or its `dsack_port`, cycle by
    /// cycle), and with none of them assigned as DSACK by PEPAR the bus
    /// monitor ends it ([`DsackFault::BusUnassigned`](crate::event::DsackFault::BusUnassigned)).  It takes a cycle
    /// beside any select that matches, the first DSACK ending it, and
    /// before a fixed memory, which takes only the cycles nothing on the
    /// bus decodes.  Memory it overlaps is not served in the core: every
    /// cycle there reaches it.
    ///
    /// # Errors
    ///
    /// [`BuildError::Window`] for a window that is empty or runs past
    /// ADDR\[23:0\].
    pub fn bus_peripheral<D: MappedPeripheral + Timed + Save + Any>(
        &mut self,
        peripheral: D,
        decodes: &[BusDecode],
        pins: Dsack,
    ) -> Result<PeripheralId, BuildError> {
        for &decode in decodes {
            if let BusDecode::Window { base, length } = decode
                && (length == 0
                    || base
                        .0
                        .checked_add(length)
                        .is_none_or(|end| end > 1 << ADDRESS_LINES))
            {
                return Err(BuildError::Window { base, length });
            }
        }
        let id = self.add(peripheral);
        let peripheral = index(id);
        self.board.dsack_pins[peripheral] = Some(pins);
        self.board.bus.push(BusWiring {
            peripheral,
            decodes: decodes.to_vec(),
        });
        self.board.decode_changed = true;
        Ok(id)
    }

    /// A peripheral on no pin: a model beside the CPU that only keeps time,
    /// or drives an interrupt.  In the snapshot as a peripheral is.
    pub fn unwired_peripheral<D: MappedPeripheral + Timed + Save + Any>(
        &mut self,
        peripheral: D,
    ) -> PeripheralId {
        self.add(peripheral)
    }

    /// The resets that reach `peripheral`'s reset input ([`MappedPeripheral::reset`]):
    /// all by default, as on the RESET pin; a board that wires it
    /// elsewhere says which (SIMRM 8.2).
    pub fn reset_scope(&mut self, peripheral: PeripheralId, scope: ResetScope) {
        self.board.reset_scopes[index(peripheral)] = scope;
    }

    /// `peripheral` on the QSPI's pins, replacing any: it sees each
    /// transfer and the levels on PCS\[3:0\] (QSMRM 4).  It says
    /// whether it saves ([`Save`]), as a peripheral does: the chip's snapshot
    /// holds it.
    pub fn qspi<D: SpiPeripheral + Save + Any>(&mut self, peripheral: D) {
        self.board
            .module_mut::<Qsm>(QSM)
            .attach(Box::new(peripheral));
    }

    /// The DSACK pins `peripheral` drives on an externally terminated select
    /// (DSACK %1111, SIMRM 7.10.3).  The chip checks them against PEPAR
    /// at each such cycle: with none assigned as DSACK, its DSACK cannot
    /// end the cycle and the bus monitor does (SIMRM 9.1, told as
    /// [`Event::Dsack`]); otherwise the port size they encode (SIMRM
    /// Table 5-2: DSACK0 8 bits, DSACK1 or both 16) sizes the cycle,
    /// not the select's port field (SIMRM 5.3.3).  Unstated by default:
    /// unchecked, and the select's field sizes it.
    pub fn dsack(&mut self, peripheral: PeripheralId, pins: Dsack) {
        self.board.dsack_pins[index(peripheral)] = Some(pins);
    }

    /// The resets that reach the QSPI's peripheral
    /// ([`SpiPeripheral::reset`]): all by default, as `reset_scope` for a
    /// peripheral.
    pub const fn qspi_reset_scope(&mut self, scope: ResetScope) {
        self.board.qspi_reset_scope = scope;
    }

    /// `peripheral`'s interrupt output wired to `pin`.
    pub fn interrupt(&mut self, peripheral: PeripheralId, pin: IrqPin) {
        self.board
            .interrupt_wires
            .push((index(peripheral), pin.level()));
    }

    /// The board asserts AVEC (PE2) at every acknowledge of `pin`'s
    /// level, as a pin tied low or decode logic does (SIMRM 6.4).
    pub const fn autovector(&mut self, pin: IrqPin) {
        self.board.autovector_levels |= 1 << pin.get();
    }

    /// The chip, through its power-on reset.
    #[must_use]
    pub fn build(self) -> Mc68331 {
        let mut chip = Mc68331 {
            cpu: Cpu::new(self.memory),
            board: self.board,
            now: Cycles::ZERO,
            halt: None,
        };
        chip.reset_for(ResetCause::PowerOn);
        chip
    }
}

/// The bytes of a memory of `bytes`: a whole number of pages.
fn pages(bytes: &[u8]) -> Result<u32, BuildError> {
    let length = bytes.len();
    if length == 0 || !length.is_multiple_of(PAGE_BYTES as usize) {
        return Err(BuildError::Length(length));
    }
    u32::try_from(length).map_err(|_| BuildError::Length(length))
}

/// The chip.
#[derive(Debug)]
pub struct Mc68331 {
    /// The CPU32.
    pub(crate) cpu: Cpu,
    /// Everything else.
    pub(crate) board: Board,
    /// Now, on the system clock.
    pub(crate) now: Cycles,
    /// Why the CPU halted, while it stays halted.
    pub(crate) halt: Option<Halt>,
}

impl Mc68331 {
    /// Runs from now for `budget` system clocks, in quanta that end at
    /// the earliest next event of the SIM, the modules and the peripherals
    /// ([PLAN.md], Peripherals): each sees time move between quanta.
    /// The watchdog's and the halt monitor's resets are taken as they
    /// come; it stops early only if the CPU halts, a hook stops it
    /// ([`Cpu::add_code_hook`] and the rest, through `cpu_control`), or the
    /// host asks it to ([`StopRequest`]); it ends early, at an instruction
    /// boundary, with [`Stop::Budget`] if the host's peripherals ask
    /// ([`SyncRequest`]).  A run that starts halted
    /// spends its budget as time passes for the SIM, the modules and
    /// the peripherals without the CPU, ending [`Stop::Halted`] unless a
    /// reset, the watchdog's say, restarts the CPU (MC68331UM 4.2.10;
    /// [PLAN.md], A halted CPU).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    pub fn run_for(&mut self, budget: Cycles) -> Run {
        let start = self.now;
        let end = start + budget;
        let mut stop = Stop::Budget;
        // diversions asked for between runs
        if self.board.take_diversions() {
            self.board.rebuild(&mut self.cpu.memory);
        }
        // asked between runs: nothing to end
        self.board.sync_request.take();
        self.board.output_moved = false;
        while self.now < end {
            if let Some(cause) = self.board.sim.take_reset() {
                let frequency = self.board.sim.clock().frequency();
                self.reset_for(cause);
                stop = self.halt.map_or(Stop::Budget, Stop::Halted);
                // or a peripheral the reset reached asks, or the reset
                // reaches a watched output
                let moved = self.board.outputs_moved() | self.board.output_stop.reset;
                if self.board.clock_stop && self.board.sim.clock().frequency() != frequency
                    || self.board.sync_request.take()
                    || moved
                {
                    break;
                }
                continue;
            }
            let next = self.board.next_event().map_or(end, |next| next.min(end));
            let quantum = self.now.until(next).max(Cycles(1));
            if let Some(halt) = self.halt {
                // time passes without the CPU
                stop = Stop::Halted(halt);
                self.now += quantum;
                self.board.advance(self.now);
                if self.board.take_stop() {
                    stop = Stop::Requested;
                    break;
                }
                if self.board.sync_request.take() | self.board.outputs_moved() {
                    break;
                }
                continue;
            }
            self.board.quantum_end = self.now + quantum;
            self.board.sync = false;
            let run = {
                let board = RefCell::new(&mut self.board);
                let mut bus = Connection(&board);
                let mut interrupts = Connection(&board);
                self.cpu
                    .run_for(&mut bus, &mut interrupts, self.now, quantum)
            };
            self.now += run.cycles;
            if mem::take(&mut self.board.system_reload) {
                self.board.reload(&mut self.cpu.memory, ResetCause::System);
            }
            if self.board.decode_changed {
                self.board.rebuild(&mut self.cpu.memory);
            }
            self.board.advance(self.now);
            self.board.output_moved |= self.board.outputs_moved();
            if self.board.take_diversions() {
                self.board.rebuild(&mut self.cpu.memory);
            }
            // a sync, the board's or a peripheral's sync request, ends the
            // quantum only, not the run, unless the host's stop came with
            // it (one left is taken below)
            let synced = mem::take(&mut self.board.sync) | self.board.sync_request.get();
            stop = match run.stop {
                Stop::Requested if !mem::take(&mut self.board.stop_taken) && synced => Stop::Budget,
                other => other,
            };
            match stop {
                Stop::Halted(halt) => {
                    if halt == Halt::DoubleBusFault && self.board.sim.halt_monitor() {
                        let frequency = self.board.sim.clock().frequency();
                        self.reset_for(ResetCause::HaltMonitor);
                        stop = self.halt.map_or(Stop::Budget, Stop::Halted);
                        // as the SIM's own resets: a clock the reset
                        // changes ends the run there
                        let moved = self.board.outputs_moved() | self.board.output_stop.reset;
                        if self.board.clock_stop && self.board.sim.clock().frequency() != frequency
                            || self.board.sync_request.take()
                            || moved
                        {
                            break;
                        }
                    } else {
                        // the host sees the halt as it comes
                        self.halt = Some(halt);
                        break;
                    }
                }
                Stop::Budget | Stop::Stopped => {}
                // a hook, a request: the host sees it
                _ => break,
            }
            // A request from between the CPU's cycles: a peripheral's
            // `advance`, or an interrupt acknowledge.
            if self.board.take_stop() {
                stop = Stop::Requested;
                break;
            }
            if self.board.sync_request.take() {
                break;
            }
            if mem::take(&mut self.board.clock_moved) | mem::take(&mut self.board.output_moved) {
                break;
            }
        }
        // asked in the run's last instruction: the run ends anyway
        self.board.sync_request.take();
        Run::new(self.now - start, stop)
    }

    /// Runs whole instructions until now is `target` or past it, as
    /// `run_for` does: it ends at the first instruction boundary at or
    /// after `target` (at once if now already is; at `target` itself
    /// if the CPU is stopped), or early for `run_for`'s reasons.  The end depends only on the chip and `target`, so a
    /// series of `run_until` to rising targets is one long run to the
    /// last, however the host cuts it; a series of `run_for(budget)` drifts
    /// by each run's overshoot.
    pub fn run_until(&mut self, target: Cycles) -> Run {
        self.run_for(self.now.until(target))
    }

    /// RESET driven by the board: an external reset.
    pub fn reset(&mut self) {
        self.reset_for(ResetCause::External);
    }

    /// A reset for `cause`: the SIM, the modules and the peripherals it
    /// reaches reset, the decode back to CSBOOT alone, then the CPU's
    /// reset exception (SIMRM 8; MC68331UM 4.6.8).
    fn reset_for(&mut self, cause: ResetCause) {
        let now = self.now;
        self.board.modules_changed();
        self.board.sim.reset(cause, now);
        self.board.clock_changed();
        self.board.reset_others(now, cause);
        self.board.reload(&mut self.cpu.memory, cause);
        self.board.events.push(Event::Reset(now, cause));
        self.board.rebuild(&mut self.cpu.memory);
        let board = RefCell::new(&mut self.board);
        self.halt = self.cpu.reset(&mut Connection(&board), now).err();
    }

    /// Now, on the system clock.
    #[must_use]
    pub const fn now(&self) -> Cycles {
        self.now
    }

    /// The CPU.
    #[must_use]
    pub const fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// The CPU's registers, to change.
    pub const fn registers_mut(&mut self) -> &mut Registers {
        &mut self.cpu.registers
    }

    /// The CPU's registers, hooks and page permissions, to change; not
    /// its memory's map, which the chip keeps in step with its decode,
    /// nor its runs, which are the chip's.
    pub const fn cpu_control(&mut self) -> CpuControl<'_> {
        CpuControl::new(&mut self.cpu)
    }

    /// Why the CPU halted, while it stays halted.
    #[must_use]
    pub const fn halted(&self) -> Option<Halt> {
        self.halt
    }

    /// The SIM: its registers, ports, clock and chip selects.
    #[must_use]
    pub const fn sim(&self) -> &Sim {
        &self.board.sim
    }

    /// The QSM: its registers, SCI and QSPI.
    #[must_use]
    pub fn qsm(&self) -> &Qsm {
        self.board.module(QSM)
    }

    /// The host sends `frame` on RXD now: its start bit begins now, or
    /// as the frames it sent before end; each takes a frame time at
    /// SCCR0's baud (QSMRM 5).
    pub fn sci_receive(&mut self, frame: Frame) {
        let now = self.now;
        self.board.module_mut::<Qsm>(QSM).receive(frame, now);
    }

    /// The frames the SCI sent on TXD since the last call, each as its
    /// start bit went out ([`Transmitted`]: the frame, its start and end,
    /// and its bit time), so a frame still on the line is reported.
    pub fn take_sci_transmitted(&mut self) -> Vec<Transmitted> {
        self.board.output_seen.frames = 0;
        self.board.module_mut::<Qsm>(QSM).take_transmitted()
    }

    /// The board drives port QS's input pins to `pins` (MC68331UM
    /// D.4.8).
    pub fn set_port_qs_pins(&mut self, pins: u8) {
        let now = self.now;
        self.board.module_mut::<Qsm>(QSM).set_port_pins(pins, now);
    }

    /// The host, as an external SPI master, drives PCS0/SS low now if
    /// `asserted`, else high: it selects the QSPI as a slave (QSMRM
    /// 4.4.2), or faults it as a master (4.3.5).
    pub fn set_qspi_slave_select(&mut self, asserted: bool) {
        let now = self.now;
        self.board
            .module_mut::<Qsm>(QSM)
            .set_slave_select(asserted, now);
    }

    /// The host, as an external SPI master, shifts the low `bits` bits
    /// of `mosi` (at most 16) to the QSPI as a slave, most significant
    /// first, one an SCK `period` of system clocks: the chip runs to
    /// the end of each period from now, and the QSPI takes the bit
    /// there, or at the end of the instruction that ran past it.
    /// Returns what the QSPI drove on MISO.  A word the QSPI completes
    /// goes to receive RAM at its bit's time, its SPIF and interrupt
    /// with it (QSMRM 4.4.2.1).  If the CPU halts, the bit whose
    /// period it halts in goes at the halt, and the rest at their
    /// times, as time passes while it is halted.
    pub fn qspi_slave_shift(&mut self, mosi: u16, bits: u8, period: Cycles) -> Shifted {
        let mut shifted = Shifted::default();
        let mut edge = self.now;
        for bit in (0..bits.min(MOST_BITS)).rev() {
            edge += period;
            self.run_for(self.now.until(edge));
            let now = self.now;
            let miso = self
                .board
                .module_mut::<Qsm>(QSM)
                .clock_slave(mosi >> bit & 1 != 0, now);
            shifted.record(bit, miso);
        }
        shifted
    }

    /// The QSPI's peripheral, if it is a `D`.
    #[must_use]
    pub fn qspi_peripheral<D: AnySpiPeripheral>(&self) -> Option<&D> {
        self.qsm().peripheral()
    }

    /// The QSPI's peripheral, to change, if it is a `D`.
    pub fn qspi_peripheral_mut<D: AnySpiPeripheral>(&mut self) -> Option<&mut D> {
        self.board.module_mut::<Qsm>(QSM).peripheral_mut()
    }

    /// The GPT: its registers, counters and pins.
    #[must_use]
    pub fn gpt(&self) -> &Gpt {
        self.board.module(GPT)
    }

    /// The board drives port GP's input pins, IC1 to IC4/OC5, to `pins`
    /// now: an input capture takes a selected edge (GPTRM 3.2).
    pub fn set_port_gp_pins(&mut self, pins: u8) {
        let now = self.now;
        self.board.module_mut::<Gpt>(GPT).set_port_pins(pins, now);
    }

    /// The board drives PAI to `level` now: the pulse accumulator's
    /// input (GPTRM 4).
    pub fn set_pai(&mut self, level: bool) {
        let now = self.now;
        self.board.module_mut::<Gpt>(GPT).set_pai(level, now);
    }

    /// The board drives PCLK to `level` now: the GPT's external clock
    /// (GPTRM 2.7).
    pub fn set_pclk(&mut self, level: bool) {
        let now = self.now;
        self.board.module_mut::<Gpt>(GPT).set_pclk(level, now);
    }

    /// The board will drive the GPT's `input` to `level` at `at`, or
    /// now if that is past: an edge a board model times itself, inside
    /// a later quantum.  The GPT captures, counts and clocks at `at`, and
    /// a run's quantum ends there ([PLAN.md], Peripherals).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    pub fn schedule_gpt_input(&mut self, at: Cycles, input: Input, level: bool) {
        self.board
            .module_mut::<Gpt>(GPT)
            .schedule_input(at, input, level);
    }

    /// Withdraws the GPT inputs `schedule_gpt_input` scheduled past now,
    /// those up to now taking effect first: for a host whose time base
    /// moved (a SYNCR write, a reset held), to schedule them again at
    /// their new clocks.
    pub fn unschedule_gpt_inputs(&mut self) {
        let now = self.now;
        self.board.module_mut::<Gpt>(GPT).unschedule_inputs(now);
    }

    /// The pins of port GP the GPT drives and their levels.
    #[must_use]
    pub fn port_gp_outputs(&self) -> PortLevels {
        self.gpt().port_outputs(self.now)
    }

    /// What `channel`'s PWM pin carries now.
    #[must_use]
    pub fn pwm(&self, channel: Channel) -> Output {
        self.gpt().pwm(channel, self.now)
    }

    /// What OC1's pin carries now: TCNT's clock under CPROUT unless
    /// OC1M3 gives the pin to the compare, its level otherwise.
    #[must_use]
    pub fn oc1(&self) -> Output {
        self.gpt().oc1(self.now)
    }

    /// The changes in what the GPT drives since the last call, in time
    /// order, each at the time it happened, as `take_port_changes`
    /// gives the SIM's: port GP's always, OC1's CPROUT clock's while
    /// `set_report_oc1_clock_edges` asks for them, the PWM pins' while
    /// `set_report_pwm_edges` does ([PLAN.md], Peripherals).
    ///
    /// [PLAN.md]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md
    pub fn take_gpt_changes(&mut self) -> Vec<(Cycles, OutputChange)> {
        self.board.module_mut::<Gpt>(GPT).take_output_changes()
    }

    /// The changes in what port QS drives since the last call, in time
    /// order, each at the time it happened, as `take_port_changes`
    /// gives the SIM's: a register write's at its instruction's start,
    /// the QSPI's PCS and the SCI's TXD at their entries' and frames'
    /// times.
    pub fn take_qsm_changes(&mut self) -> Vec<(Cycles, PortLevels)> {
        self.board.module_mut::<Qsm>(QSM).take_output_changes()
    }

    /// Whether `channel`'s PWM edges are reported from now on, from the
    /// level its pin has now.  Off out of power-on; a reset keeps it.
    pub fn set_report_pwm_edges(&mut self, channel: Channel, report: bool) {
        let now = self.now;
        self.board
            .module_mut::<Gpt>(GPT)
            .set_report_pwm_edges(channel, report, now);
    }

    /// Whether the edges of TCNT's clock on OC1's pin under CPROUT are
    /// reported from now on, in port GP's changes; while they are not,
    /// the changes show the pin low.  Off out of power-on; a reset keeps
    /// it.
    pub fn set_report_oc1_clock_edges(&mut self, report: bool) {
        let now = self.now;
        self.board
            .module_mut::<Gpt>(GPT)
            .set_report_oc1_clock_edges(report, now);
    }

    /// The board drives port E's input pins to `pins`.
    pub const fn set_port_e_pins(&mut self, pins: u8) {
        self.board.modules_changed();
        self.board.sim.set_port_e_pins(pins);
    }

    /// The board drives port F's input pins to `pins`.
    pub const fn set_port_f_pins(&mut self, pins: u8) {
        self.board.modules_changed();
        self.board.sim.set_port_f_pins(pins);
    }

    /// The pins of port C, PC0 to PC6 on CS3 to CS9, the SIM drives and
    /// their levels (MC68331UM 4.8.1.4).
    #[must_use]
    pub fn port_c_outputs(&self) -> PortLevels {
        self.board.sim.outputs(PortName::C)
    }

    /// The pins of port E the SIM drives and their levels (MC68331UM
    /// 4.9).
    #[must_use]
    pub fn port_e_outputs(&self) -> PortLevels {
        self.board.sim.outputs(PortName::E)
    }

    /// The pins of port F the SIM drives and their levels (MC68331UM
    /// 4.9).
    #[must_use]
    pub fn port_f_outputs(&self) -> PortLevels {
        self.board.sim.outputs(PortName::F)
    }

    /// The pins of port QS the QSM drives and their levels (MC68331UM
    /// D.4.8).
    #[must_use]
    pub fn port_qs_outputs(&self) -> PortLevels {
        self.qsm().port_outputs()
    }

    /// The changes in what ports C, E and F drive since the last call,
    /// each with the time of the write that made it: a pin the firmware
    /// pulses within a quantum is both its edges.
    pub fn take_port_changes(&mut self) -> Vec<(Cycles, Change)> {
        self.board.sim.take_output_changes()
    }

    /// The host asserts or negates `pin`.
    pub const fn set_interrupt_pin(&mut self, pin: IrqPin, asserted: bool) {
        let bit = 1 << pin.get();
        self.board.level_stale = true;
        if asserted {
            self.board.interrupt_pins |= bit;
        } else {
            self.board.interrupt_pins &= !bit;
        }
    }

    /// A peripheral, if it is a `D`.
    #[must_use]
    pub fn peripheral<D: Any>(&self, id: PeripheralId) -> Option<&D> {
        let peripheral: &dyn Any = self.board.peripherals.get(index(id))?.as_ref();
        peripheral.downcast_ref()
    }

    /// A peripheral, to change, if it is a `D`.
    pub fn peripheral_mut<D: Any>(&mut self, id: PeripheralId) -> Option<&mut D> {
        self.board.level_stale = true;
        let peripheral: &mut dyn Any = self.board.peripherals.get_mut(index(id))?.as_mut();
        peripheral.downcast_mut()
    }

    /// `memory`'s cycles go to `peripheral` from now on, or with `None`
    /// back to the memory ([`Diversion`], which a peripheral holds to do it
    /// within a cycle).
    pub fn divert_memory(&mut self, memory: MemoryId, peripheral: Option<PeripheralId>) {
        match peripheral {
            Some(peripheral) => self.board.diversion.divert(memory, peripheral),
            None => self.board.diversion.release(memory),
        }
        if self.board.take_diversions() {
            self.board.rebuild(&mut self.cpu.memory);
        }
    }

    /// A memory's bytes.
    #[must_use]
    pub fn memory(&self, id: MemoryId) -> &[u8] {
        self.cpu.memory.region(id.0)
    }

    /// A memory's bytes, to change.
    pub fn memory_mut(&mut self, id: MemoryId) -> &mut [u8] {
        self.cpu.memory.region_mut(id.0)
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        mem::take(&mut self.board.events)
    }

    /// Whether a change of the system clock's frequency (a SYNCR write,
    /// a reset) ends `run_for` after its instruction, [`Stop::Budget`] with
    /// budget left: for a host on its own time base (a board's
    /// picoseconds), which moves it there.  Off by default: `run_for` spends
    /// its budget.
    pub const fn set_clock_stop(&mut self, stop: bool) {
        self.board.clock_stop = stop;
    }

    /// Which outputs' changes end `run_for` ([`OutputStop`]): after the
    /// instruction that made one, or the quantum whose end did (a compare,
    /// a PWM edge, a frame's start), [`Stop::Budget`] with budget left.
    /// For a host whose peripherals sense them within its own scheduler (a
    /// board's nets).  Watching none, the default, costs nothing.
    pub fn set_output_stop(&mut self, stop: OutputStop) {
        self.board.output_stop = stop;
        self.board.output_seen = self.board.seen();
    }

    /// What the events trace beyond resets and IARB ties.
    pub const fn set_tracing(&mut self, tracing: ChipTracing) {
        self.board.tracing = tracing;
    }

    /// Instructions the CPU has run since the chip was built, for the
    /// trace ([`Cpu::instructions`]).
    #[must_use]
    pub const fn instructions(&self) -> u64 {
        self.cpu.instructions()
    }
}

/// The core's view of the board, its bus and its interrupt inputs at
/// once: both borrow the board in turn.
struct Connection<'a, 'b>(&'a RefCell<&'b mut Board>);

impl Connection<'_, '_> {
    /// A cycle from the core: its value and its stall.
    fn read_or_write(
        &self,
        memory: Option<&mut Memory>,
        access: BusAccess,
        write: Option<u32>,
    ) -> Result<(u32, Cycles), BusError> {
        let mut board = self.0.borrow_mut();
        let ended = board.cycle(
            memory,
            access.address.0,
            access.size,
            access.function_code,
            access.now,
            write,
        );
        if board.tracing.accesses {
            board.events.push(Event::Access(event::TracedAccess {
                now: access.now,
                address: access.address,
                size: access.size,
                function_code: access.function_code,
                write: write.is_some(),
                value: write.or(ended.ok().map(|(value, _)| value)),
                bus_error: ended.is_err(),
            }));
        }
        let (value, clocks) = ended?;
        Ok((value, stall(clocks)))
    }
}

impl Bus for Connection<'_, '_> {
    fn read(&mut self, access: BusAccess) -> Result<Read, BusError> {
        let (value, stall) = self.read_or_write(None, access, None)?;
        Ok(Read { value, stall })
    }

    fn write(&mut self, access: BusAccess, value: u32) -> Result<Cycles, BusError> {
        self.read_or_write(None, access, Some(value))
            .map(|(_, stall)| stall)
    }

    fn read_with_memory(
        &mut self,
        access: BusAccess,
        memory: &mut Memory,
    ) -> Result<Read, BusError> {
        let (value, stall) = self.read_or_write(Some(memory), access, None)?;
        Ok(Read { value, stall })
    }

    fn write_with_memory(
        &mut self,
        access: BusAccess,
        value: u32,
        memory: &mut Memory,
    ) -> Result<Cycles, BusError> {
        self.read_or_write(Some(memory), access, Some(value))
            .map(|(_, stall)| stall)
    }

    fn reset_instruction(&mut self, now: Cycles) {
        self.0.borrow_mut().reset_instruction(now);
    }

    fn exception_entered(&mut self, entry: &ExceptionEntry, now: Cycles) {
        let mut board = self.0.borrow_mut();
        if board.tracing.exceptions {
            board.events.push(Event::Exception(now, *entry));
        }
    }

    /// The host's request to stop or to end the run, or the board's to
    /// end the quantum.
    fn stop_requested(&mut self) -> bool {
        let mut board = self.0.borrow_mut();
        board.stop_taken |= board.stop_request.take();
        board.sync || board.sync_request.get() || board.stop_taken
    }
}

impl Interrupts for Connection<'_, '_> {
    fn level(&self) -> InterruptLevel {
        self.0.borrow_mut().level()
    }

    fn acknowledge(&mut self, level: InterruptLevel, now: Cycles) -> Acknowledged {
        self.0.borrow_mut().acknowledge(level, now)
    }
}
