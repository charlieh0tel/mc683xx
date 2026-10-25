//! The chip as a Python host builds and runs it: `Builder` wires
//! memory and Python peripherals to chip-select pins, `Mc68331` runs.

use std::cell::RefCell;
use std::ops::RangeInclusive;
use std::rc::Rc;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::cpu;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_cpu32::size::Size;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::chip::MemoryId;
use mc683xx_mc68331::event;
use mc683xx_mc68331::event::ChipTracing;
use mc683xx_mc68331::event::DsackFault;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::qsm::qspi::MOST_BITS;
use mc683xx_modules::sim;
use mc683xx_modules::sim::Configuration;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::chip_selects::Termination;
use mc683xx_modules::sim::clock::ClockSource;

use mc683xx_modules::sim::reset::ResetScope;
use pyo3::Bound;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::PyTraverseError;
use pyo3::PyVisit;
use pyo3::Python;
use pyo3::pybacked::PyBackedBytes;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyBytes;
use pyo3::types::PyDict;

use crate::board::BoardView;
use crate::error::Error;
use crate::gdb;
use crate::hooks;
use crate::hooks::HookIdView;
use crate::mapped::Pending;
use crate::mapped::PythonMapped;
use crate::peripherals;
use crate::peripherals::ChannelView;
use crate::peripherals::Frame;
use crate::peripherals::Output;
use crate::peripherals::OutputChangeView;
use crate::peripherals::PortNameView;
use crate::permissions;
use crate::qspi::PythonSpiPeripheral;
use crate::register::Register;
use crate::requests::DiversionView;
use crate::requests::StopRequestView;
use crate::requests::SyncRequestView;
use crate::select;

/// What the host wired, by the numbers the bindings gave it: the
/// builder's, then the chip's, and the `Diversion`'s that reads them.
#[derive(Debug, Default)]
pub(crate) struct Wiring {
    /// Memories, by number.
    memories: Vec<MemoryId>,
    /// Peripherals and models, by number.
    peripherals: Vec<PeripheralId>,
}

impl Wiring {
    /// Numbers memory `id`: its number.
    fn add_memory(&mut self, id: MemoryId) -> usize {
        self.memories.push(id);
        self.memories.len() - 1
    }

    /// Numbers peripheral `id`: its number.
    fn add_peripheral(&mut self, id: PeripheralId) -> usize {
        self.peripherals.push(id);
        self.peripherals.len() - 1
    }

    /// Memory number `index`.
    pub(crate) fn memory(&self, index: usize) -> Result<MemoryId, Error> {
        self.memories.get(index).copied().ok_or(Error::Unknown {
            kind: "memory",
            index,
        })
    }

    /// Peripheral number `index`.
    pub(crate) fn peripheral(&self, index: usize) -> Result<PeripheralId, Error> {
        self.peripherals.get(index).copied().ok_or(Error::Unknown {
            kind: "peripheral",
            index,
        })
    }
}

/// System clocks a Python run goes between looks for Ctrl-C: about
/// 6 ms of a 16.78 MHz chip, as gdb's quanta.
const QUANTUM: Cycles = Cycles(100_000);

/// A port's outputs as Python has them: a `(driven, levels)` pair, which
/// unpacks as `PortLevels`' fields do.
const fn pair(outputs: PortLevels) -> (u8, u8) {
    (outputs.driven, outputs.levels)
}

/// Pins by name.
fn pins(names: &[String]) -> Result<Vec<ChipSelect>, Error> {
    names.iter().map(|name| select::parse(name)).collect()
}

/// The resets `causes`: a `ResetScope` as Python gives one.
fn scope(causes: &[ResetCause]) -> ResetScope {
    causes
        .iter()
        .fold(ResetScope::NONE, |scope, &cause| scope.with(cause.into()))
}

/// A host cycle of `bytes`, 1, 2 or 4.
const fn cycle_size(bytes: u32) -> Result<Size, Error> {
    match bytes {
        1 => Ok(Size::Byte),
        2 => Ok(Size::Word),
        4 => Ok(Size::Long),
        _ => Err(Error::CycleSize(bytes)),
    }
}

/// FC[2:0] as `bits`.
fn function_code(bits: u8) -> Result<FunctionCode, Error> {
    let function_code = FunctionCode::new(bits.into());
    if function_code.bits() == bits {
        Ok(function_code)
    } else {
        Err(Error::FunctionCode(bits))
    }
}

/// The board as the host wires it, before the power-on reset.
#[pyclass(name = "Builder", module = "mc683xx", unsendable)]
#[derive(Debug)]
pub(crate) struct ChipBuilder {
    /// The builder, until `build`.
    builder: Option<Builder>,
    /// What is wired so far.
    wiring: Rc<RefCell<Wiring>>,
    /// Where the peripherals' exceptions wait.
    pending: Pending,
}

impl ChipBuilder {
    /// The builder, if `build` has not taken it.
    fn builder(&mut self) -> Result<&mut Builder, Error> {
        self.builder.as_mut().ok_or(Error::Built)
    }

    /// Wires `peripheral` to `pins`.
    fn wire(&mut self, peripheral: PythonMapped, pins: &[ChipSelect]) -> Result<usize, Error> {
        let id = self.builder()?.peripheral(peripheral, pins)?;
        Ok(self.wiring.borrow_mut().add_peripheral(id))
    }
}

#[pymethods]
impl ChipBuilder {
    /// Shows Python's cycle collector the Python objects it holds,
    /// until `build` hands them to the chip.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        if self.builder.is_some() {
            self.pending.traverse(&visit)?;
        }
        Ok(())
    }

    /// Drops them, for the collector to break a cycle through it.
    fn __clear__(&mut self) {
        if self.builder.is_some() {
            self.pending.clear();
        }
    }

    /// A board whose reset reads `data` on DATA[15:0] (MC68331UM
    /// 4.6.3), its clock synthesized from a `reference_hz` crystal
    /// (32768 by default) or, with MODCLK low, an `external_hz` clock
    /// (MC68331UM 4.3.1).
    #[new]
    #[pyo3(signature = (*, data = u16::MAX, reference_hz = None, external_hz = None))]
    fn new(data: u16, reference_hz: Option<u32>, external_hz: Option<u32>) -> Result<Self, Error> {
        let clock = match (reference_hz, external_hz) {
            (Some(_), Some(_)) => return Err(Error::Clock),
            (_, Some(hz)) => ClockSource::external(Frequency::from_hz(hz.into()))?,
            (Some(hz), None) => ClockSource::synthesizer(Frequency::from_hz(hz.into()))?,
            (None, None) => ClockSource::default(),
        };
        let builder = Builder::new(Configuration { data, clock });
        let pending = Pending::new(builder.stop_request());
        Ok(Self {
            builder: Some(builder),
            wiring: Rc::default(),
            pending,
        })
    }

    /// `contents` (a whole number of 2-Kbyte pages) on `pins`, RAM if
    /// `writable`, else ROM; its number.
    #[pyo3(signature = (contents, pins, *, writable = true))]
    fn memory(
        &mut self,
        contents: PyBackedBytes,
        pins: Vec<String>,
        writable: bool,
    ) -> Result<usize, Error> {
        let pins = self::pins(&pins)?;
        let id = self.builder()?.memory(contents.to_vec(), writable, &pins)?;
        Ok(self.wiring.borrow_mut().add_memory(id))
    }

    /// `contents` (a whole number of 2-Kbyte pages) at `base` on no pin,
    /// emulator-only: the program and data cycles there no chip select
    /// matches, at once; RAM if `writable`, else ROM; its number.
    #[pyo3(signature = (base, contents, *, writable = true))]
    fn fixed_memory(
        &mut self,
        base: u32,
        contents: PyBackedBytes,
        writable: bool,
    ) -> Result<usize, Error> {
        let id = self
            .builder()?
            .fixed_memory(Address(base), contents.to_vec(), writable)?;
        Ok(self.wiring.borrow_mut().add_memory(id))
    }

    /// Fixed memory number `memory`'s port, `port` bits (8 or 16), and
    /// its cycles' wait states, 0 to 13, or fast termination if `None`
    /// (Rust's `fixed_timing`, which takes a `PortSize` and a
    /// `Termination`); fast on 16 bits by default.
    #[pyo3(signature = (memory, *, port = 16, wait_states = None))]
    fn fixed_timing(
        &mut self,
        memory: usize,
        port: u8,
        wait_states: Option<u8>,
    ) -> Result<(), Error> {
        let memory = self.wiring.borrow().memory(memory)?;
        let port = match port {
            8 => PortSize::Bits8,
            16 => PortSize::Bits16,
            other => return Err(Error::PortWidth(other)),
        };
        let termination = wait_states.map_or(Termination::Fast, Termination::WaitStates);
        self.builder()?.fixed_timing(memory, port, termination)?;
        Ok(())
    }

    /// Only the resets in `scope`, `ResetCause`s, put memory number
    /// `memory`'s contents back as built; by default none do.
    fn reload_scope(&mut self, memory: usize, scope: Vec<ResetCause>) -> Result<(), Error> {
        let memory = self.wiring.borrow().memory(memory)?;
        self.builder()?.reload_scope(memory, self::scope(&scope));
        Ok(())
    }

    /// `peripheral`, which has `read(cycle)` and `write(cycle, value)`, on
    /// `pins`; its number.
    fn peripheral(&mut self, peripheral: &Bound<'_, PyAny>, pins: Vec<String>) -> PyResult<usize> {
        let pins = self::pins(&pins)?;
        let peripheral = PythonMapped::mapped(peripheral, self.pending.clone())?;
        Ok(self.wire(peripheral, &pins)?)
    }

    /// `peripheral` on no pin: it keeps time or drives an interrupt; its
    /// number.
    fn unwired_peripheral(&mut self, peripheral: &Bound<'_, PyAny>) -> PyResult<usize> {
        let peripheral = PythonMapped::unwired(peripheral, self.pending.clone())?;
        Ok(self.wire(peripheral, &[])?)
    }

    /// `peripheral`, which has `transfer(transfer) -> int` and may have
    /// `chip_selects(levels, now)`, `clock(clock, now)`, `reset(now)`
    /// and `deferrable() -> bool`, on the QSPI's pins, replacing any
    /// (QSMRM 4).  In the chip's snapshot by its `save() -> bytes` and
    /// `restore(bytes)`, as a peripheral is.
    fn qspi(&mut self, peripheral: &Bound<'_, PyAny>) -> PyResult<()> {
        let peripheral = PythonSpiPeripheral::new(peripheral, self.pending.clone())?;
        self.builder()?.qspi(peripheral);
        Ok(())
    }

    /// The DSACK pins peripheral number `peripheral` drives on an
    /// externally terminated select (SIMRM 7.10.3), checked against
    /// PEPAR at each such cycle (`Event.Dsack`), and sizing it (SIMRM
    /// Table 5-2); unstated by default: unchecked, the select's port
    /// field sizing it.
    fn dsack(&mut self, peripheral: usize, pins: DsackView) -> Result<(), Error> {
        let peripheral = self.wiring.borrow().peripheral(peripheral)?;
        self.builder()?.dsack(peripheral, pins.into());
        Ok(())
    }

    /// Only the resets in `scope`, `ResetCause`s, reach the QSPI's
    /// peripheral (its `reset(now)`); by default every one does.
    fn qspi_reset_scope(&mut self, scope: Vec<ResetCause>) -> Result<(), Error> {
        self.builder()?.qspi_reset_scope(self::scope(&scope));
        Ok(())
    }

    /// The handle by which the host's peripherals ask the chip's runs to
    /// stop (`Stop.Requested()`), for them to hold.
    fn stop_request(&mut self) -> Result<StopRequestView, Error> {
        Ok(StopRequestView(self.builder()?.stop_request()))
    }

    /// The handle by which the host's peripherals end the chip's runs
    /// early (`Stop.Budget()`), for them to hold.
    fn sync_request(&mut self) -> Result<SyncRequestView, Error> {
        Ok(SyncRequestView(self.builder()?.sync_request()))
    }

    /// The handle by which the host's peripherals divert a memory's
    /// cycles to a peripheral and back, by their numbers, for them to
    /// hold.
    fn diversion(&mut self) -> Result<DiversionView, Error> {
        let diversion = self.builder()?.diversion();
        Ok(DiversionView {
            diversion,
            wiring: Rc::clone(&self.wiring),
        })
    }

    /// Peripheral `peripheral`'s interrupt output to IRQ`pin`.
    fn interrupt(&mut self, peripheral: usize, pin: u8) -> Result<(), Error> {
        let peripheral = self.wiring.borrow().peripheral(peripheral)?;
        let pin = IrqPin::try_from(pin)?;
        self.builder()?.interrupt(peripheral, pin);
        Ok(())
    }

    /// Only the resets in `scope`, `ResetCause`s, reach peripheral number
    /// `peripheral`'s reset; by default every one does (SIMRM 8.2).
    fn reset_scope(&mut self, peripheral: usize, scope: Vec<ResetCause>) -> Result<(), Error> {
        let peripheral = self.wiring.borrow().peripheral(peripheral)?;
        self.builder()?.reset_scope(peripheral, self::scope(&scope));
        Ok(())
    }

    /// The board asserts AVEC at every acknowledge on IRQ`pin` (SIMRM
    /// 6.4).
    fn autovector(&mut self, pin: u8) -> Result<(), Error> {
        let pin = IrqPin::try_from(pin)?;
        self.builder()?.autovector(pin);
        Ok(())
    }

    /// The chip, through its power-on reset.  The builder is spent.
    fn build(&mut self) -> PyResult<Chip> {
        let chip = self.builder.take().ok_or(Error::Built)?.build();
        self.pending.check()?;
        Ok(Chip {
            chip,
            wiring: Rc::clone(&self.wiring),
            pending: self.pending.clone(),
        })
    }
}

/// Why a run ended (Rust's `cpu::Stop`).
#[pyclass(module = "mc683xx", frozen, eq, hash, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Stop {
    /// It spent its budget.
    Budget(),
    /// `stop` or `lpstop`: the CPU waits for an interrupt; the rest of
    /// the budget passed waiting.
    Stopped(),
    /// The CPU halted; only reset restarts it.
    Halted(Halt),
    /// A hook stopped it: the first to, if several did at once.
    Hook(HookIdView),
    /// The host or a peripheral asked it to stop, once the instruction
    /// whose cycle asked ended: a stop request, or a peripheral's
    /// exception, which the run raises.
    Requested(),
}

impl From<cpu::Stop> for Stop {
    fn from(stop: cpu::Stop) -> Self {
        match stop {
            cpu::Stop::Budget => Self::Budget(),
            cpu::Stop::Stopped => Self::Stopped(),
            cpu::Stop::Halted(halt) => Self::Halted(halt.into()),
            cpu::Stop::Hook(id) => Self::Hook(HookIdView(id)),
            _ => Self::Requested(),
        }
    }
}

/// Why the CPU halted.
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Halt {
    /// A double bus fault (CPU32RM 6.2.1 to 6.2.3).
    DoubleBusFault,
}

impl From<cpu::Halt> for Halt {
    fn from(halt: cpu::Halt) -> Self {
        match halt {
            cpu::Halt::DoubleBusFault => Self::DoubleBusFault,
            _ => panic!("a halt the bindings do not know"),
        }
    }
}

/// A reset's cause, as RSR records it (MC68331UM D.3.4).
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ResetCause {
    /// RESET driven by the board.
    External,
    /// Power-on.
    PowerOn,
    /// The software watchdog.
    Watchdog,
    /// The halt monitor, on a double bus fault.
    HaltMonitor,
    /// The `reset` instruction.
    System,
    /// Loss of the clock's reference.
    LossOfClock,
    /// The test submodule.
    Test,
}

impl From<sim::ResetCause> for ResetCause {
    fn from(cause: sim::ResetCause) -> Self {
        match cause {
            sim::ResetCause::External => Self::External,
            sim::ResetCause::PowerOn => Self::PowerOn,
            sim::ResetCause::Watchdog => Self::Watchdog,
            sim::ResetCause::HaltMonitor => Self::HaltMonitor,
            sim::ResetCause::System => Self::System,
            sim::ResetCause::LossOfClock => Self::LossOfClock,
            sim::ResetCause::Test => Self::Test,
            // every cause RSR has is above (`sim::ResetCause::ALL`)
            _ => unreachable!("a reset cause without an RSR bit"),
        }
    }
}

impl From<ResetCause> for sim::ResetCause {
    fn from(cause: ResetCause) -> Self {
        match cause {
            ResetCause::External => Self::External,
            ResetCause::PowerOn => Self::PowerOn,
            ResetCause::Watchdog => Self::Watchdog,
            ResetCause::HaltMonitor => Self::HaltMonitor,
            ResetCause::System => Self::System,
            ResetCause::LossOfClock => Self::LossOfClock,
            ResetCause::Test => Self::Test,
        }
    }
}

/// A run's outcome (Rust's `cpu::Run`).
#[pyclass(module = "mc683xx", frozen, eq, hash, get_all)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Run {
    /// The system clocks it took.
    cycles: u64,
    /// Why it ended.
    stop: Stop,
}

impl From<cpu::Run> for Run {
    fn from(run: cpu::Run) -> Self {
        Self {
            cycles: run.cycles.0,
            stop: run.stop.into(),
        }
    }
}

#[pymethods]
impl Run {
    fn __repr__(&self) -> String {
        format!("Run(cycles={}, stop={:?})", self.cycles, self.stop)
    }
}

/// An address space whose pages `Mc68331.protect` sets (SIMRM
/// 7.10.3, the SPACE field); program and data share one.
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Space {
    /// User space: FC2 clear.
    User,
    /// Supervisor space: FC2 set.
    Supervisor,
}

impl From<Space> for memory::Space {
    fn from(space: Space) -> Self {
        match space {
            Space::User => Self::User,
            Space::Supervisor => Self::Supervisor,
        }
    }
}

/// Addresses `begin` to `end`, both included.
const fn addresses(begin: u32, end: u32) -> RangeInclusive<Address> {
    Address(begin)..=Address(end)
}

/// The DSACK pins a peripheral drives (Rust's `chip_selects::Dsack`).
#[pyclass(
    name = "Dsack",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DsackView {
    /// DSACK0: an 8-bit port (SIMRM Table 5-2).
    Dsack0,
    /// DSACK1: a 16-bit port.
    Dsack1,
    /// Both: reserved, taken as a 16-bit port.
    Both,
}

impl From<DsackView> for Dsack {
    fn from(pins: DsackView) -> Self {
        match pins {
            DsackView::Dsack0 => Self::Dsack0,
            DsackView::Dsack1 => Self::Dsack1,
            DsackView::Both => Self::Both,
        }
    }
}

/// How a peripheral's DSACK pins disagree with the chip (Rust's
/// `event::DsackFault`).
#[pyclass(
    name = "DsackFault",
    module = "mc683xx",
    frozen,
    eq,
    hash,
    from_py_object
)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum DsackFaultView {
    /// PEPAR assigns none of the pins it drives as DSACK (SIMRM 9.1,
    /// Table 9-1), so its DSACK cannot end the cycle and the bus monitor
    /// does.
    Unassigned {
        /// The select whose cycle it was: `CSBOOT`, `CS0` to `CS10`.
        select: String,
    },
}

impl DsackFaultView {
    /// `fault` as Python sees it, if Python has it.
    fn of(fault: DsackFault) -> Option<Self> {
        match fault {
            DsackFault::Unassigned { select } => Some(Self::Unassigned {
                select: select.to_string(),
            }),
            _ => None,
        }
    }
}

/// Something the chip did that the host may want to know of.
#[pyclass(module = "mc683xx", frozen)]
#[derive(Debug, Clone)]
pub(crate) enum Event {
    /// The chip reset, at `now`, for `cause`.
    Reset {
        /// When, in system clocks.
        now: u64,
        /// Why.
        cause: ResetCause,
    },
    /// Two interrupt sources of one level had the same nonzero IARB;
    /// a fixed order chose (PLAN.md, Where the manuals are silent).
    ArbitrationTie {
        /// The level acknowledged.
        level: u8,
        /// The IARB value they shared.
        arbitration: u8,
    },
    /// A bus cycle that reached the chip, while accesses are traced.
    Access {
        /// When its instruction started, in system clocks.
        now: u64,
        /// Its address.
        address: u32,
        /// Bytes in it: 1 or 2.
        size: u32,
        /// FC[2:0].
        function_code: u8,
        /// Whether it wrote.
        write: bool,
        /// The value read or written; `None` for a read that ended in
        /// a bus error.
        value: Option<u32>,
        /// Whether it ended in a bus error.
        bus_error: bool,
    },
    /// An exception entered, while exceptions are traced.
    Exception {
        /// When its instruction or interrupt entry started.
        now: u64,
        /// Its vector number.
        vector: u8,
        /// The return PC its frame stacked.
        pc: u32,
        /// Its handler's address.
        handler: u32,
    },
    /// A peripheral's DSACK pins (`Builder.dsack`) disagree with the chip
    /// on an externally terminated cycle at `now`: told once for each
    /// peripheral and kind of fault.
    Dsack {
        /// When, in system clocks.
        now: u64,
        /// How.
        fault: DsackFaultView,
    },
}

impl Event {
    /// `event` as Python sees it, if Python has it.
    pub(crate) fn of(event: event::Event) -> Option<Self> {
        Some(match event {
            event::Event::Reset(now, cause) => Self::Reset {
                now: now.0,
                cause: cause.into(),
            },
            event::Event::ArbitrationTie { level, arbitration } => Self::ArbitrationTie {
                level: level.get(),
                arbitration,
            },
            event::Event::Access(access) => Self::Access {
                now: access.now.0,
                address: access.address.0,
                size: access.size.bytes(),
                function_code: access.function_code.bits(),
                write: access.write,
                value: access.value,
                bus_error: access.bus_error,
            },
            event::Event::Exception(now, entry) => Self::Exception {
                now: now.0,
                vector: entry.vector.0,
                pc: entry.pc.0,
                handler: entry.handler.0,
            },
            event::Event::Dsack(now, fault) => Self::Dsack {
                now: now.0,
                fault: DsackFaultView::of(fault)?,
            },
            _ => return None,
        })
    }
}

/// The `Mc68331` methods a board's chip has too, for `Chip` and
/// `BoardChip` alike: each reaches the chip through its type's
/// `with_chip` and `with_chip_mut`, and the rest of the type's methods
/// come after its name.
macro_rules! chip_methods {
    ($type:ty, $($rest:tt)*) => {
        #[pymethods]
        impl $type {
            /// A read cycle of `size` bytes (1, 2 or 4) at `address` in
            /// `function_code`'s space (5, supervisor data, by default;
            /// CPU32RM Table 5-1), now, through the chip's decode, with its
            /// side effects: a module's register or what a chip select
            /// reaches.  Raises `BusError` if it ends in one or nothing
            /// answers, and a peripheral's exception as `run_for` does.
            #[pyo3(signature = (address, size, function_code = 5))]
            fn bus_read(
                &mut self,
                py: Python<'_>,
                address: u32,
                size: u32,
                function_code: u8,
            ) -> PyResult<u32> {
                let size = cycle_size(size)?;
                let function_code = self::function_code(function_code)?;
                self.with_chip_mut(py, |chip, pending| {
                    let read = chip.bus_read(Address(address), size, function_code);
                    pending.check()?;
                    Ok(read.map_err(Error::from)?)
                })
            }

            /// A write cycle of `value`, as `bus_read`.
            #[pyo3(signature = (address, size, value, function_code = 5))]
            fn bus_write(
                &mut self,
                py: Python<'_>,
                address: u32,
                size: u32,
                value: u32,
                function_code: u8,
            ) -> PyResult<()> {
                let size = cycle_size(size)?;
                let function_code = self::function_code(function_code)?;
                self.with_chip_mut(py, |chip, pending| {
                    let written = chip.bus_write(Address(address), size, function_code, value);
                    pending.check()?;
                    Ok(written.map_err(Error::from)?)
                })
            }

            /// What `bus_read` would return now, without a cycle: a
            /// module's register as it holds it, memory's bytes.  Raises
            /// `UnpeekableError` where only a cycle can tell (a peripheral).
            #[pyo3(signature = (address, size, function_code = 5))]
            fn peek(
                &self,
                py: Python<'_>,
                address: u32,
                size: u32,
                function_code: u8,
            ) -> PyResult<u32> {
                let size = cycle_size(size)?;
                let function_code = self::function_code(function_code)?;
                self.with_chip(py, |chip| {
                    Ok(chip
                        .peek(Address(address), size, function_code)
                        .map_err(Error::from)?)
                })
            }

            /// Now, in system clocks.
            #[getter]
            fn now(&self, py: Python<'_>) -> PyResult<u64> {
                self.with_chip(py, |chip| Ok(chip.now().0))
            }

            /// Why the CPU halted, while it stays halted.
            #[getter]
            fn halted(&self, py: Python<'_>) -> PyResult<Option<Halt>> {
                self.with_chip(py, |chip| Ok(chip.halted().map(Halt::from)))
            }

            /// The vector of the first exception the last run's last
            /// instruction or interrupt took, if any.
            #[getter]
            fn exception_taken(&self, py: Python<'_>) -> PyResult<Option<u8>> {
                self.with_chip(py, |chip| {
                    Ok(chip
                        .cpu()
                        .exception_taken()
                        .map(|exception| exception.vector().0))
                })
            }

            /// Instructions run since the chip was built.
            #[getter]
            fn instructions(&self, py: Python<'_>) -> PyResult<u64> {
                self.with_chip(py, |chip| Ok(chip.instructions()))
            }

            /// Register `name`: `D0`-`D7`, `A0`-`A7`, `PC`, `SR`, `CCR`,
            /// `USP`, `SSP`, `VBR`, `SFC`, `DFC`.
            fn read_register(&self, py: Python<'_>, name: &str) -> PyResult<u32> {
                let register = Register::parse(name)?;
                self.with_chip(py, |chip| Ok(register.read(&chip.cpu().registers)))
            }

            /// Sets register `name` to `value`, which must fit it.
            fn write_register(&mut self, py: Python<'_>, name: &str, value: u32) -> PyResult<()> {
                let register = Register::parse(name)?;
                self.with_chip_mut(py, |chip, _| {
                    Ok(register.write(chip.registers_mut(), value)?)
                })
            }

            /// Every register by name.
            fn registers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
                self.with_chip(py, |chip| hooks::register_dictionary(py, &chip.cpu().registers))
            }

            /// Port C's output pins, PC0 to PC6 on CS3 to CS9, and their
            /// levels, as two masks (MC68331UM 4.8.1.4).
            fn port_c_outputs(&self, py: Python<'_>) -> PyResult<(u8, u8)> {
                self.with_chip(py, |chip| Ok(pair(chip.port_c_outputs())))
            }

            /// Port E's output pins and their levels, as two masks
            /// (MC68331UM 4.9).
            fn port_e_outputs(&self, py: Python<'_>) -> PyResult<(u8, u8)> {
                self.with_chip(py, |chip| Ok(pair(chip.port_e_outputs())))
            }

            /// Port F's output pins and their levels, as two masks
            /// (MC68331UM 4.9).
            fn port_f_outputs(&self, py: Python<'_>) -> PyResult<(u8, u8)> {
                self.with_chip(py, |chip| Ok(pair(chip.port_f_outputs())))
            }

            /// PORTC, the latch for the chip-select pins assigned as
            /// discrete outputs (SIMRM 7.10.4).
            #[getter]
            fn port_c(&self, py: Python<'_>) -> PyResult<u8> {
                self.with_chip(py, |chip| Ok(chip.sim().port_c()))
            }

            /// The system clock's frequency in hertz, as SYNCR sets it.
            #[getter]
            fn system_hz(&self, py: Python<'_>) -> PyResult<u64> {
                self.with_chip(py, |chip| Ok(chip.sim().clock().frequency().hz()))
            }

            /// Port QS's output pins and their levels, as two masks.
            fn port_qs_outputs(&self, py: Python<'_>) -> PyResult<(u8, u8)> {
                self.with_chip(py, |chip| Ok(pair(chip.port_qs_outputs())))
            }

            /// The levels on PCS[3:0].
            #[getter]
            fn qspi_chip_selects(&self, py: Python<'_>) -> PyResult<u8> {
                self.with_chip(py, |chip| Ok(chip.qsm().chip_selects()))
            }

            /// Port GP's output pins and their levels, as two masks.
            fn port_gp_outputs(&self, py: Python<'_>) -> PyResult<(u8, u8)> {
                self.with_chip(py, |chip| Ok(pair(chip.port_gp_outputs())))
            }

            /// What PWM `channel` carries now.
            fn pwm(&self, py: Python<'_>, channel: ChannelView) -> PyResult<Output> {
                self.with_chip(py, |chip| Ok(chip.pwm(channel.into()).into()))
            }

            /// What OC1's pin carries now: `Output.Clock` under CPROUT,
            /// `Output.Level` otherwise.
            fn oc1(&self, py: Python<'_>) -> PyResult<Output> {
                self.with_chip(py, |chip| Ok(chip.oc1().into()))
            }

            /// Calls `callback(Instruction)` before each instruction at an
            /// address from `begin` to `end`; a truthy result stops the run
            /// before it, and the next run executes it without calling the
            /// code hooks for it again.
            #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
            fn add_code_hook(
                &mut self,
                callback: &Bound<'_, PyAny>,
                begin: u32,
                end: u32,
            ) -> PyResult<HookIdView> {
                self.with_chip_mut(callback.py(), |chip, pending| {
                    let hook = hooks::hook(callback, pending.clone(), hooks::instruction)?;
                    Ok(HookIdView(chip.cpu_control().add_code_hook(addresses(begin, end), hook)))
                })
            }

            /// Calls `callback(Instruction)` before the first instruction
            /// of each block that starts from `begin` to `end`; it stops
            /// the run as a code hook does.
            #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
            fn add_block_hook(
                &mut self,
                callback: &Bound<'_, PyAny>,
                begin: u32,
                end: u32,
            ) -> PyResult<HookIdView> {
                self.with_chip_mut(callback.py(), |chip, pending| {
                    let hook = hooks::hook(callback, pending.clone(), hooks::instruction)?;
                    Ok(HookIdView(chip.cpu_control().add_block_hook(addresses(begin, end), hook)))
                })
            }

            /// Calls `callback(MemoryAccess)` after each operand read cycle
            /// from `begin` to `end`; a truthy result stops the run once
            /// the instruction completes.
            #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
            fn add_read_hook(
                &mut self,
                callback: &Bound<'_, PyAny>,
                begin: u32,
                end: u32,
            ) -> PyResult<HookIdView> {
                self.with_chip_mut(callback.py(), |chip, pending| {
                    let hook = hooks::hook(callback, pending.clone(), hooks::memory_access)?;
                    Ok(HookIdView(chip.cpu_control().add_read_hook(addresses(begin, end), hook)))
                })
            }

            /// Calls `callback(MemoryAccess)` after each write cycle from
            /// `begin` to `end`, as `add_read_hook`.
            #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
            fn add_write_hook(
                &mut self,
                callback: &Bound<'_, PyAny>,
                begin: u32,
                end: u32,
            ) -> PyResult<HookIdView> {
                self.with_chip_mut(callback.py(), |chip, pending| {
                    let hook = hooks::hook(callback, pending.clone(), hooks::memory_access)?;
                    Ok(HookIdView(chip.cpu_control().add_write_hook(addresses(begin, end), hook)))
                })
            }

            /// Calls `callback(InvalidAccess)` for each cycle from `begin`
            /// to `end` that ends in a bus error, before the CPU takes the
            /// exception; a truthy result stops the run once it is taken.
            #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
            fn add_invalid_hook(
                &mut self,
                callback: &Bound<'_, PyAny>,
                begin: u32,
                end: u32,
            ) -> PyResult<HookIdView> {
                self.with_chip_mut(callback.py(), |chip, pending| {
                    let hook = hooks::hook(callback, pending.clone(), hooks::invalid_access)?;
                    Ok(HookIdView(chip.cpu_control().add_invalid_hook(addresses(begin, end), hook)))
                })
            }

            /// Calls `callback(ExceptionEntry)` after each exception is
            /// entered; a truthy result stops the run with the handler not
            /// yet run.
            fn add_exception_hook(&mut self, callback: &Bound<'_, PyAny>) -> PyResult<HookIdView> {
                self.with_chip_mut(callback.py(), |chip, pending| {
                    let hook = hooks::hook(callback, pending.clone(), hooks::exception_entry)?;
                    Ok(HookIdView(chip.cpu_control().add_exception_hook(hook)))
                })
            }

            /// Removes `hook`; whether it was installed.
            fn remove_hook(&mut self, py: Python<'_>, hook: &HookIdView) -> PyResult<bool> {
                self.with_chip_mut(py, |chip, _| Ok(chip.cpu_control().remove_hook(hook.0)))
            }

            /// Sets the permissions of `length` bytes from `start` in
            /// `space`, whole 2-Kbyte pages: `permissions` holds `r`, `w`
            /// and `x` for the cycles allowed.  A cycle they do not allow
            /// ends in a bus error.
            fn protect(
                &mut self,
                py: Python<'_>,
                space: Space,
                start: u32,
                length: u32,
                permissions: &str,
            ) -> PyResult<()> {
                let permissions = permissions::parse(permissions)?;
                self.with_chip_mut(py, |chip, _| {
                    Ok(chip
                        .cpu_control()
                        .protect(space.into(), Address(start), length, permissions)
                        .map_err(Error::from)?)
                })
            }

            $($rest)*
        }
    };
}

/// The chip.
#[pyclass(name = "Mc68331", module = "mc683xx", unsendable)]
#[derive(Debug)]
pub(crate) struct Chip {
    /// The chip.
    chip: Mc68331,
    /// What the host wired.
    wiring: Rc<RefCell<Wiring>>,
    /// Where the peripherals' exceptions wait.
    pending: Pending,
}

impl Chip {
    /// `run_for` and `run_until`: the chip's `run_until(end)` in quanta of
    /// `QUANTUM`, Ctrl-C looked for between them.  A series of
    /// `run_until`s is one long run, so the quanta change nothing the
    /// chip does; a quantum that ends early (a stop, a halt, a clock
    /// change) ends the run as the one long run would.  The GIL stays
    /// held: the chip holds Python objects and is not `Send`.
    fn run_to(&mut self, py: Python<'_>, end: Cycles) -> PyResult<Run> {
        let start = self.chip.now();
        loop {
            let target = Cycles(self.chip.now().0.saturating_add(QUANTUM.0)).min(end);
            // A chip halted before the quantum spends it as time passes
            // (no instruction runs), so its halt goes on to the next.
            let halted = self.chip.halted().is_some();
            let run = self.chip.run_until(target);
            self.pending.check()?;
            let now = self.chip.now();
            let quantized = now >= target
                && (matches!(run.stop, cpu::Stop::Budget | cpu::Stop::Stopped)
                    || (halted && matches!(run.stop, cpu::Stop::Halted(_))));
            if !quantized || now >= end {
                return Ok(cpu::Run::new(now - start, run.stop).into());
            }
            py.check_signals()?;
        }
    }

    /// Memory number `index`'s bytes from `offset`, `length` of them.
    fn span(&self, index: usize, offset: usize, length: usize) -> Result<(MemoryId, usize), Error> {
        let id = self.wiring.borrow().memory(index)?;
        let size = self.chip.memory(id).len();
        match offset.checked_add(length) {
            Some(end) if end <= size => Ok((id, end)),
            _ => Err(Error::Range {
                offset,
                length,
                size,
            }),
        }
    }
}

impl Chip {
    /// `call` with the chip.
    fn with_chip<R>(
        &self,
        _py: Python<'_>,
        call: impl FnOnce(&Mc68331) -> PyResult<R>,
    ) -> PyResult<R> {
        call(&self.chip)
    }

    /// `call` with the chip, to change, and where its peripherals'
    /// exceptions wait.
    fn with_chip_mut<R>(
        &mut self,
        _py: Python<'_>,
        call: impl FnOnce(&mut Mc68331, &Pending) -> PyResult<R>,
    ) -> PyResult<R> {
        call(&mut self.chip, &self.pending)
    }
}

/// A board's chip, as `Board.chip` gives it: the `Mc68331` methods that
/// read the chip or hook it, on the chip the board runs.  The board
/// owns its pins, memories and peripherals, so the methods that drive pins,
/// run or snapshot the chip are the board's.  It holds the board: a
/// call while the board runs (from a peripheral or a hook) raises
/// `RuntimeError`.
#[pyclass(name = "BoardChip", module = "mc683xx", unsendable)]
#[derive(Debug)]
pub(crate) struct BoardChip {
    /// The board.
    pub(crate) board: Py<BoardView>,
}

impl BoardChip {
    /// `call` with the board's chip.
    fn with_chip<R>(
        &self,
        py: Python<'_>,
        call: impl FnOnce(&Mc68331) -> PyResult<R>,
    ) -> PyResult<R> {
        let board = self.board.try_borrow(py)?;
        call(board.board_chip())
    }

    /// `call` with the board's chip, to change, and where the board's
    /// exceptions wait.
    fn with_chip_mut<R>(
        &mut self,
        py: Python<'_>,
        call: impl FnOnce(&mut Mc68331, &Pending) -> PyResult<R>,
    ) -> PyResult<R> {
        let mut board = self.board.try_borrow_mut(py)?;
        let (chip, pending) = board.board_chip_mut();
        call(chip, pending)
    }
}

chip_methods! {
    BoardChip,
    /// Shows Python's cycle collector the board it holds.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.board)
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        self.with_chip(py, |chip| Ok(format!("BoardChip(now={})", chip.now().0)))
    }
}

chip_methods! {
    Chip,
    /// Shows Python's cycle collector the Python objects it holds.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        self.pending.traverse(&visit)
    }

    /// Drops them, for the collector to break a cycle through it.
    fn __clear__(&mut self) {
        self.pending.clear();
    }

    /// Runs for `budget` system clocks, or until the CPU halts; a run
    /// that starts halted spends its budget as time passes without the
    /// CPU, so the watchdog can reset the chip.  An
    /// exception a peripheral raises other than `BusError` answers its
    /// cycle undriven, the bus not faulted, and stops the run once that
    /// instruction ends; it is raised here, the chip as it then is.
    fn run_for(&mut self, py: Python<'_>, budget: u64) -> PyResult<Run> {
        let Some(end) = self.chip.now().0.checked_add(budget) else {
            return Err(Error::Budget(budget).into());
        };
        self.run_to(py, Cycles(end))
    }

    /// Waits for gdb at `address` (`host:port`, port 0 picks one),
    /// calls `ready(port)` once it listens, and lets gdb run the chip
    /// until it detaches (`Ending.DETACHED`) or kills it (`Ending.KILLED`)
    /// (docs/gdb.md).  Peripherals and hooks run as in `run_for`; an exception
    /// one raises ends the session and is raised here.
    #[pyo3(signature = (address = gdb::ADDRESS, ready = None))]
    fn serve_gdb(
        slf: &Bound<'_, Self>,
        address: &str,
        ready: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<gdb::Ending> {
        let connection = gdb::accept(slf.py(), address, ready)?;
        let mut this = slf.try_borrow_mut()?;
        let this = &mut *this;
        let mut target = gdb::Checked {
            target: &mut this.chip,
            pending: &this.pending,
        };
        gdb::serve(&mut target, connection)
    }

    /// Runs whole instructions until `now` is `target` or past it: to
    /// the first instruction boundary at or after it, so a series of
    /// these to rising targets is one long run.  Exceptions as `run_for`.
    fn run_until(&mut self, py: Python<'_>, target: u64) -> PyResult<Run> {
        self.run_to(py, Cycles(target))
    }

    /// RESET driven by the board.
    fn reset(&mut self) -> PyResult<()> {
        self.chip.reset();
        self.pending.check()
    }

    /// Memory number `memory`'s bytes.
    fn memory<'py>(&self, py: Python<'py>, memory: usize) -> Result<Bound<'py, PyBytes>, Error> {
        let id = self.wiring.borrow().memory(memory)?;
        Ok(PyBytes::new(py, self.chip.memory(id)))
    }

    /// `length` bytes of memory number `memory` from `offset`.
    fn read_memory<'py>(
        &self,
        py: Python<'py>,
        memory: usize,
        offset: usize,
        length: usize,
    ) -> Result<Bound<'py, PyBytes>, Error> {
        let (id, end) = self.span(memory, offset, length)?;
        Ok(PyBytes::new(py, &self.chip.memory(id)[offset..end]))
    }

    /// Writes `data` into memory number `memory` from `offset`.
    fn write_memory(
        &mut self,
        memory: usize,
        offset: usize,
        data: PyBackedBytes,
    ) -> Result<(), Error> {
        let (id, end) = self.span(memory, offset, data.len())?;
        self.chip.memory_mut(id)[offset..end].copy_from_slice(&data);
        Ok(())
    }

    /// Peripheral number `peripheral`'s object.
    fn peripheral(&self, py: Python<'_>, peripheral: usize) -> Result<Py<PyAny>, Error> {
        let id = self.wiring.borrow().peripheral(peripheral)?;
        self.chip
            .peripheral::<PythonMapped>(id)
            .and_then(|peripheral| peripheral.object(py))
            .ok_or(Error::Unknown {
                kind: "peripheral",
                index: peripheral,
            })
    }

    /// The host asserts or negates IRQ`pin`.
    fn set_interrupt_pin(&mut self, pin: u8, asserted: bool) -> Result<(), Error> {
        self.chip
            .set_interrupt_pin(IrqPin::try_from(pin).map_err(Error::from)?, asserted);
        Ok(())
    }

    /// The board drives port E's input pins to `pins`.
    const fn set_port_e_pins(&mut self, pins: u8) {
        self.chip.set_port_e_pins(pins);
    }

    /// The board drives port F's input pins to `pins`.
    const fn set_port_f_pins(&mut self, pins: u8) {
        self.chip.set_port_f_pins(pins);
    }

    /// The changes in what ports C, E and F drive since the last call:
    /// `(now, port, driven, levels)`, a `PortName`, its output pins and
    /// their levels as two masks (Rust's `ports::Change`, flattened).
    fn take_port_changes(&mut self) -> Vec<(u64, PortNameView, u8, u8)> {
        self.chip
            .take_port_changes()
            .into_iter()
            .map(|(now, change)| {
                (
                    now.0,
                    change.port.into(),
                    change.outputs.driven,
                    change.outputs.levels,
                )
            })
            .collect()
    }

    /// The changes in what port QS drives since the last call, in time
    /// order: `(now, driven, levels)` (Rust's `take_qsm_changes`).  A
    /// host with no board takes them, or they gather in the chip and
    /// its snapshot.
    fn take_qsm_changes(&mut self) -> Vec<(u64, u8, u8)> {
        self.chip
            .take_qsm_changes()
            .into_iter()
            .map(|(now, outputs)| (now.0, outputs.driven, outputs.levels))
            .collect()
    }

    /// The QSPI peripheral's object, if one is wired.
    fn qspi_peripheral(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.chip
            .qspi_peripheral::<PythonSpiPeripheral>()
            .and_then(|peripheral| peripheral.object(py))
    }

    /// The host sends `frame` on RXD now, after those it sent before
    /// (QSMRM 5).
    fn sci_receive(&mut self, frame: Frame) -> Result<(), Error> {
        self.chip.sci_receive(frame.try_into()?);
        Ok(())
    }

    /// The frames the SCI sent on TXD since the last call, each with
    /// the time its start bit began.
    fn take_sci_transmitted(&mut self) -> Vec<(u64, Frame)> {
        self.chip
            .take_sci_transmitted()
            .into_iter()
            .map(|sent| (sent.start.0, sent.frame.into()))
            .collect()
    }

    /// The board drives port QS's input pins to `pins` now (MC68331UM
    /// D.4.8).
    fn set_port_qs_pins(&mut self, pins: u8) -> PyResult<()> {
        self.chip.set_port_qs_pins(pins);
        self.pending.check()
    }

    /// The host, as an external SPI master, drives PCS0/SS low now if
    /// `asserted`, else high (QSMRM 4.4.2).
    fn set_qspi_slave_select(&mut self, asserted: bool) -> PyResult<()> {
        self.chip.set_qspi_slave_select(asserted);
        self.pending.check()
    }

    /// The host, as an external SPI master, shifts `bits` bits of
    /// `mosi` to the QSPI as a slave, most significant first, one every
    /// `period` system clocks, the chip running between them: what the
    /// QSPI drove on MISO and which bits it drove (QSMRM 4.4.2.1).
    fn qspi_slave_shift(&mut self, mosi: u16, bits: u8, period: u64) -> PyResult<(u16, u16)> {
        if bits > MOST_BITS || u32::from(mosi) >> bits != 0 {
            return Err(Error::Shift { mosi, bits }.into());
        }
        let budget = period.saturating_mul(bits.into());
        if self.chip.now().0.checked_add(budget).is_none() {
            return Err(Error::Budget(budget).into());
        }
        let shifted = self.chip.qspi_slave_shift(mosi, bits, Cycles(period));
        self.pending.check()?;
        Ok((shifted.miso, shifted.driven))
    }

    /// The board drives port GP's input pins, IC1 to IC4/OC5, to `pins`
    /// now (GPTRM 3.2).
    fn set_port_gp_pins(&mut self, pins: u8) {
        self.chip.set_port_gp_pins(pins);
    }

    /// The board drives PAI to `level` now (GPTRM 4).
    fn set_pai(&mut self, level: bool) {
        self.chip.set_pai(level);
    }

    /// The board drives PCLK to `level` now (GPTRM 2.7).
    fn set_pclk(&mut self, level: bool) {
        self.chip.set_pclk(level);
    }

    /// The board will drive GPT input `input`, `PGP0` to `PGP7`, `PAI`
    /// or `PCLK`, to `level` at time `at`, or now if that is past; a
    /// run's quantum ends there.
    fn schedule_gpt_input(&mut self, at: u64, input: &str, level: bool) -> Result<(), Error> {
        self.chip
            .schedule_gpt_input(Cycles(at), peripherals::input(input)?, level);
        Ok(())
    }

    /// The changes in what the GPT drives since the last call, in time
    /// order: `(now, OutputChange)`, port GP's, and PWMA's or PWMB's
    /// while `set_report_pwm_edges` asks for them; OC1's CPROUT clock in
    /// port GP's while `set_report_oc1_clock_edges` does.
    fn take_gpt_changes(&mut self) -> Vec<(u64, OutputChangeView)> {
        self.chip
            .take_gpt_changes()
            .into_iter()
            .map(|(now, change)| (now.0, change.into()))
            .collect()
    }

    /// Whether PWM `channel`'s edges are reported from now on.
    fn set_report_pwm_edges(&mut self, channel: ChannelView, report: bool) {
        self.chip.set_report_pwm_edges(channel.into(), report);
    }

    /// Whether the edges of TCNT's clock on OC1's pin under CPROUT are
    /// reported from now on, in port `GP`'s changes.
    fn set_report_oc1_clock_edges(&mut self, report: bool) {
        self.chip.set_report_oc1_clock_edges(report);
    }

    /// What happened since the last call.
    fn take_events(&mut self) -> Vec<Event> {
        self.chip
            .take_events()
            .into_iter()
            .filter_map(Event::of)
            .collect()
    }

    /// Whether the events trace each bus cycle that reaches the chip
    /// (`Event.Access`) and each exception entered
    /// (`Event.Exception`); both off at first.
    #[pyo3(signature = (*, accesses = false, exceptions = false))]
    fn set_tracing(&mut self, accesses: bool, exceptions: bool) {
        let mut tracing = ChipTracing::default();
        tracing.accesses = accesses;
        tracing.exceptions = exceptions;
        self.chip.set_tracing(tracing);
    }

    /// The chip's whole state as bytes, every peripheral's and the QSPI's
    /// peripheral's among it (each one's `save()`); not the hooks,
    /// tracing or events not yet taken.  A peripheral or QSPI peripheral
    /// with no `save` raises `StateError` naming it.
    fn save<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let saved = self.chip.save();
        self.pending.check()?;
        Ok(PyBytes::new(py, &saved.map_err(Error::from)?))
    }

    /// Restores bytes `save` gave, of a chip built the same way, each
    /// peripheral and the QSPI's peripheral by its `restore(bytes)`; the
    /// hooks stay.  Another format, other hardware or another count of
    /// peripherals raises `StateError`, the chip and its peripherals unchanged.
    fn restore(&mut self, snapshot: PyBackedBytes) -> PyResult<()> {
        let restored = self.chip.restore(&snapshot);
        self.pending.check()?;
        Ok(restored.map_err(Error::from)?)
    }
}
