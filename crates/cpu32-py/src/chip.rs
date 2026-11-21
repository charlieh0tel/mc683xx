//! The chip as a Python host builds and runs it: `Builder` wires
//! memory and Python peripherals to chip-select pins, `Mc68331` runs.

use std::cell::RefCell;
use std::ops::RangeInclusive;
use std::rc::Rc;

use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::cpu;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory;
use mc683xx_cpu32::memory::MemoryKind;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::chip::MemoryId;
use mc683xx_mc68331::event;
use mc683xx_mc68331::event::ChipTracing;
use mc683xx_mc68331::event::DsackFault;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::pins::PortLevels;
use mc683xx_modules::qsm::qspi::MOST_BITS;
use mc683xx_modules::sim;
use mc683xx_modules::sim::Configuration;
use mc683xx_modules::sim::chip_selects::Dsack;
use mc683xx_modules::sim::chip_selects::Termination;
use mc683xx_modules::sim::clock::ClockSource;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

use mc683xx_modules::sim::reset::ResetScope;
use pyo3::Bound;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::PyTraverseError;
use pyo3::PyVisit;
use pyo3::Python;
use pyo3::exceptions::PyValueError;
use pyo3::pybacked::PyBackedBytes;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyBytes;
use pyo3::types::PyDict;

use crate::bare::FaultStopView;
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
use crate::peripherals::PortStateView;
use crate::peripherals::PortView;
use crate::permissions;
use crate::python::repr;
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
    (outputs.driven(), outputs.levels())
}

/// Pins by name.
fn pins(names: &[String]) -> Result<Vec<ChipSelect>, Error> {
    names.iter().map(|name| select::parse(name)).collect()
}

/// A memory's kind as Python gives it, `writable` or not.
const fn memory_kind(writable: bool) -> MemoryKind {
    if writable {
        MemoryKind::Ram
    } else {
        MemoryKind::Rom
    }
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
    FunctionCode::from_bits(bits).ok_or(Error::FunctionCode(bits))
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
        let builder = Builder::new(Configuration::new(data, clock));
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
        let id = self
            .builder()?
            .memory(contents.to_vec(), memory_kind(writable), &pins)?;
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
        let id = self.builder()?.fixed_memory(
            Address(base),
            contents.to_vec(),
            memory_kind(writable),
        )?;
        Ok(self.wiring.borrow_mut().add_memory(id))
    }

    /// Fixed memory number `memory`'s port, `port` bits (8 or 16, Rust's
    /// `PortSize`), and its cycles' `termination`, a `Termination`:
    /// `Fast()`, the default, or `WaitStates(n)`, 0 to 13 (Rust's
    /// `set_fixed_timing`).
    #[pyo3(signature = (memory, *, port = 16, termination = None))]
    fn set_fixed_timing(
        &mut self,
        memory: usize,
        port: u8,
        termination: Option<TerminationView>,
    ) -> Result<(), Error> {
        let memory = self.wiring.borrow().memory(memory)?;
        let port = match port {
            8 => PortSize::Bits8,
            16 => PortSize::Bits16,
            other => return Err(Error::PortWidth(other)),
        };
        let termination = termination.map_or(Termination::Fast, Termination::from);
        self.builder()?
            .set_fixed_timing(memory, port, termination)?;
        Ok(())
    }

    /// Only the resets in `scope`, `ResetCause`s, put memory number
    /// `memory`'s contents back as built; by default none do.
    fn set_reload_scope(&mut self, memory: usize, scope: Vec<ResetCause>) -> Result<(), Error> {
        let memory = self.wiring.borrow().memory(memory)?;
        self.builder()?
            .set_reload_scope(memory, self::scope(&scope));
        Ok(())
    }

    /// `peripheral`, which has `read(cycle)` and `write(cycle, value)`, on
    /// `pins`; its number.
    fn peripheral(&mut self, peripheral: &Bound<'_, PyAny>, pins: Vec<String>) -> PyResult<usize> {
        let pins = self::pins(&pins)?;
        let peripheral = PythonMapped::mapped(peripheral, self.pending.clone())?;
        Ok(self.wire(peripheral, &pins)?)
    }

    /// `peripheral`, which has `read(cycle)` and `write(cycle, value)`,
    /// on no select, taking the cycles its own decode does: each
    /// `(base, length)` of `windows` and the acknowledges of each IRQ
    /// level of `acknowledges`, driving the DSACK pins `dsack`; its
    /// number (Rust's `bus_peripheral`, whose `BusDecode`s these are).
    #[pyo3(signature = (peripheral, *, windows = Vec::new(), acknowledges = Vec::new(), dsack))]
    fn bus_peripheral(
        &mut self,
        peripheral: &Bound<'_, PyAny>,
        windows: Vec<(u32, u32)>,
        acknowledges: Vec<u8>,
        dsack: DsackView,
    ) -> PyResult<usize> {
        let mut decodes: Vec<BusDecode> = windows
            .into_iter()
            .map(|(base, length)| BusDecode::Window {
                base: Address(base),
                length,
            })
            .collect();
        for level in acknowledges {
            let pin = IrqPin::try_from(level).map_err(Error::from)?;
            decodes.push(BusDecode::Acknowledge(pin));
        }
        let peripheral = PythonMapped::mapped(peripheral, self.pending.clone())?;
        let id = self
            .builder()?
            .bus_peripheral(peripheral, &decodes, dsack.into())
            .map_err(Error::from)?;
        Ok(self.wiring.borrow_mut().add_peripheral(id))
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
    fn set_dsack(&mut self, peripheral: usize, pins: DsackView) -> Result<(), Error> {
        let peripheral = self.wiring.borrow().peripheral(peripheral)?;
        self.builder()?.set_dsack(peripheral, pins.into());
        Ok(())
    }

    /// Only the resets in `scope`, `ResetCause`s, reach the QSPI's
    /// peripheral (its `reset(now)`); by default every one does.
    fn set_qspi_reset_scope(&mut self, scope: Vec<ResetCause>) -> Result<(), Error> {
        self.builder()?.set_qspi_reset_scope(self::scope(&scope));
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
    fn set_interrupt(&mut self, peripheral: usize, pin: u8) -> Result<(), Error> {
        let peripheral = self.wiring.borrow().peripheral(peripheral)?;
        let pin = IrqPin::try_from(pin)?;
        self.builder()?.set_interrupt(peripheral, pin);
        Ok(())
    }

    /// Only the resets in `scope`, `ResetCause`s, reach peripheral number
    /// `peripheral`'s reset; by default every one does (SIMRM 8.2).
    fn set_reset_scope(&mut self, peripheral: usize, scope: Vec<ResetCause>) -> Result<(), Error> {
        let peripheral = self.wiring.borrow().peripheral(peripheral)?;
        self.builder()?
            .set_reset_scope(peripheral, self::scope(&scope));
        Ok(())
    }

    /// The board asserts AVEC at every acknowledge on IRQ`pin` (SIMRM
    /// 6.4).
    fn set_autovector(&mut self, pin: u8) -> Result<(), Error> {
        let pin = IrqPin::try_from(pin)?;
        self.builder()?.set_autovector(pin);
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

/// Why a run ended (Rust's `cpu::Stop`).  A variant's value is a field
/// named for it, not Rust's `_0`.
#[pyclass(module = "mc683xx", frozen, eq, hash, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Stop {
    /// It spent its budget.
    Budget(),
    /// `stop` or `lpstop`: the CPU waits for an interrupt; the rest of
    /// the budget passed waiting.
    Stopped(),
    /// The CPU halted; only reset restarts it.
    Halted {
        /// Why.
        halt: Halt,
    },
    /// A hook stopped it: the first to, if several did at once.
    Hook {
        /// The hook.
        hook: HookIdView,
    },
    /// The host or a peripheral asked it to stop, once the instruction
    /// whose cycle asked ended: a stop request, or a peripheral's
    /// exception, which the run raises.
    Requested(),
    /// The CPU is in background mode, for its entry source.
    Background {
        /// The entry source.
        entry: Entry,
    },
    /// A fault stopped it rather than being taken (`set_stop_on_fault`):
    /// `fault_stop` says which.
    Fault(),
}

/// Why the CPU entered background mode (Rust's
/// `mc683xx_target::entry::Entry`; CPU32RM Table 7-2).
#[pyclass(module = "mc683xx", frozen, eq, hash, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Entry {
    /// BKPT asserted.
    Breakpoint(),
    /// A BGND instruction.
    Background(),
    /// A double bus fault, with the second fault's SSW.
    DoubleBusFault {
        /// The special status word.
        ssw: u16,
    },
    /// An ATEMP Table 7-2 does not list.
    Unknown {
        /// ATEMP as read.
        atemp: u32,
    },
}

impl From<entry::Entry> for Entry {
    fn from(entry: entry::Entry) -> Self {
        match entry {
            entry::Entry::Breakpoint => Self::Breakpoint(),
            entry::Entry::Background => Self::Background(),
            entry::Entry::DoubleBusFault { ssw } => Self::DoubleBusFault { ssw },
            entry::Entry::Unknown { atemp } => Self::Unknown { atemp },
        }
    }
}

#[pymethods]
impl Entry {
    fn __repr__(&self) -> String {
        match self {
            Self::Breakpoint() => "Entry.Breakpoint()".to_owned(),
            Self::Background() => "Entry.Background()".to_owned(),
            Self::DoubleBusFault { ssw } => format!("Entry.DoubleBusFault(ssw={ssw:#06x})"),
            Self::Unknown { atemp } => format!("Entry.Unknown(atemp={atemp:#010x})"),
        }
    }
}

#[pymethods]
impl Stop {
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(match self {
            Self::Budget() => "Stop.Budget()".to_owned(),
            Self::Stopped() => "Stop.Stopped()".to_owned(),
            Self::Halted { halt } => format!("Stop.Halted({})", repr(py, *halt)?),
            Self::Hook { hook } => format!("Stop.Hook({})", repr(py, *hook)?),
            Self::Requested() => "Stop.Requested()".to_owned(),
            Self::Background { entry } => format!("Stop.Background({})", entry.__repr__()),
            Self::Fault() => "Stop.Fault()".to_owned(),
        })
    }
}

impl TryFrom<cpu::Stop> for Stop {
    type Error = Error;

    /// The stop, or [`Error::Unmapped`] naming one the bindings do not
    /// know.
    fn try_from(stop: cpu::Stop) -> Result<Self, Error> {
        Ok(match stop {
            cpu::Stop::Budget => Self::Budget(),
            cpu::Stop::Stopped => Self::Stopped(),
            cpu::Stop::Halted(halt) => Self::Halted {
                halt: halt.try_into()?,
            },
            cpu::Stop::Hook(id) => Self::Hook {
                hook: HookIdView(id),
            },
            cpu::Stop::Requested => Self::Requested(),
            cpu::Stop::Background(entry) => Self::Background {
                entry: entry.into(),
            },
            cpu::Stop::Fault => Self::Fault(),
            other => return Err(Error::run_end(other)),
        })
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

impl TryFrom<cpu::Halt> for Halt {
    type Error = Error;

    /// The halt, or [`Error::Unmapped`] naming one the bindings do not
    /// know.
    fn try_from(halt: cpu::Halt) -> Result<Self, Error> {
        match halt {
            cpu::Halt::DoubleBusFault => Ok(Self::DoubleBusFault),
            other => Err(Error::run_end(other)),
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

impl TryFrom<sim::ResetCause> for ResetCause {
    type Error = Error;

    /// The cause, or [`Error::Unmapped`] for one the bindings do not
    /// know.
    fn try_from(cause: sim::ResetCause) -> Result<Self, Error> {
        Ok(match cause {
            sim::ResetCause::External => Self::External,
            sim::ResetCause::PowerOn => Self::PowerOn,
            sim::ResetCause::Watchdog => Self::Watchdog,
            sim::ResetCause::HaltMonitor => Self::HaltMonitor,
            sim::ResetCause::System => Self::System,
            sim::ResetCause::LossOfClock => Self::LossOfClock,
            sim::ResetCause::Test => Self::Test,
            _ => return Err(Error::unmapped("reset cause")),
        })
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

impl TryFrom<cpu::Run> for Run {
    type Error = Error;

    fn try_from(run: cpu::Run) -> Result<Self, Error> {
        Ok(Self {
            cycles: run.cycles.0,
            stop: run.stop.try_into()?,
        })
    }
}

#[pymethods]
impl Run {
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "Run(cycles={}, stop={})",
            self.cycles,
            self.stop.__repr__(py)?
        ))
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

/// How a select would end a cycle it matches (Rust's
/// `chip_selects::Termination`; SIMRM 7.5, 7.7), a class per variant.
#[pyclass(
    name = "Termination",
    module = "mc683xx",
    frozen,
    eq,
    hash,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TerminationView {
    /// Internal DSACK after two clocks.
    Fast(),
    /// Internal DSACK after `wait_states` wait states, 0 to 13.
    WaitStates {
        /// The wait states.
        wait_states: u8,
    },
    /// The peripheral asserts DSACK.
    External(),
    /// MODE set: the cycle runs on ECLK.
    Eclk(),
}

impl From<TerminationView> for Termination {
    fn from(termination: TerminationView) -> Self {
        match termination {
            TerminationView::Fast() => Self::Fast,
            TerminationView::WaitStates { wait_states } => Self::WaitStates(wait_states),
            TerminationView::External() => Self::External,
            TerminationView::Eclk() => Self::Eclk,
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
    /// The same for a peripheral on no select (`bus_peripheral`).
    BusUnassigned {
        /// The peripheral's number.
        peripheral: u64,
    },
}

impl DsackFaultView {
    /// `fault` as Python sees it, or [`Error::Unmapped`] for one the
    /// bindings do not know.
    fn of(fault: DsackFault) -> Result<Self, Error> {
        match fault {
            DsackFault::Unassigned { select } => Ok(Self::Unassigned {
                select: select.to_string(),
            }),
            // Python numbers the chip's peripherals as it adds them
            DsackFault::BusUnassigned { peripheral } => Ok(Self::BusUnassigned {
                peripheral: peripheral.into(),
            }),
            _ => Err(Error::unmapped("DSACK fault")),
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
        /// When the level was acknowledged, in system clocks.
        now: u64,
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
        /// SSP: its frame's lowest address.
        stack: u32,
        /// The interrupt level, for an interrupt.
        level: Option<u8>,
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
    /// `event` as Python sees it, `None` for a kind of event Python
    /// lacks, or [`Error::Unmapped`] for a value in it (a reset cause,
    /// a DSACK fault) the bindings do not know.
    pub(crate) fn of(event: event::Event) -> Result<Option<Self>, Error> {
        Ok(Some(match event {
            event::Event::Reset { now, cause, .. } => Self::Reset {
                now: now.0,
                cause: cause.try_into()?,
            },
            event::Event::ArbitrationTie {
                now,
                level,
                arbitration,
                ..
            } => Self::ArbitrationTie {
                now: now.0,
                level: level.get(),
                arbitration,
            },
            event::Event::Access {
                now,
                address,
                size,
                function_code,
                write,
                value,
                bus_error,
                ..
            } => Self::Access {
                now: now.0,
                address: address.0,
                size: size.bytes(),
                function_code: function_code.bits(),
                write,
                value,
                bus_error,
            },
            event::Event::Exception { now, entry, .. } => Self::Exception {
                now: now.0,
                vector: entry.vector.0,
                pc: entry.pc.0,
                handler: entry.handler.0,
                stack: entry.stack.0,
                level: entry.level.map(|level| level.get()),
            },
            event::Event::Dsack { now, fault, .. } => Self::Dsack {
                now: now.0,
                fault: DsackFaultView::of(fault)?,
            },
            _ => return Ok(None),
        }))
    }
}

/// `events` as Python sees them, those of a kind Python lacks left
/// out; [`Error::Unmapped`] for a value in one the bindings do not know.
pub(crate) fn events(events: Vec<event::Event>) -> Result<Vec<Event>, Error> {
    events
        .into_iter()
        .filter_map(|event| Event::of(event).transpose())
        .collect()
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
                self.with_chip(py, |chip| {
                    Ok(chip.cpu().halted().map(Halt::try_from).transpose()?)
                })
            }

            /// The `Entry` while the CPU is in background mode (CPU32RM
            /// 7.2.3).
            #[getter]
            fn background(&self, py: Python<'_>) -> PyResult<Option<Entry>> {
                self.with_chip(py, |chip| Ok(chip.cpu().background().map(Entry::from)))
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

            /// `port`'s output pins and their levels now, as two masks.
            fn outputs(&self, py: Python<'_>, port: PortView) -> PyResult<(u8, u8)> {
                self.with_chip(py, |chip| Ok(pair(chip.outputs(port.into()))))
            }

            /// `port`'s registers and the levels the board drives on its
            /// pins.
            fn port(&self, py: Python<'_>, port: PortView) -> PyResult<PortStateView> {
                self.with_chip(py, |chip| Ok(chip.port(port.into()).into()))
            }

            /// The system clock's frequency in hertz, as SYNCR sets it.
            #[getter]
            fn system_hz(&self, py: Python<'_>) -> PyResult<u64> {
                self.with_chip(py, |chip| Ok(chip.sim().clock().frequency().hz()))
            }

            /// The levels on PCS[3:0].
            #[getter]
            fn qspi_chip_selects(&self, py: Python<'_>) -> PyResult<u8> {
                self.with_chip(py, |chip| Ok(chip.qsm().chip_selects()))
            }

            /// What PWM `channel` carries now.
            fn pwm(&self, py: Python<'_>, channel: ChannelView) -> PyResult<Output> {
                self.with_chip(py, |chip| Ok(chip.pwm(channel.into()).try_into()?))
            }

            /// What OC1's pin carries now: `Output.Clock` under CPROUT,
            /// `Output.Level` otherwise.
            fn oc1(&self, py: Python<'_>) -> PyResult<Output> {
                self.with_chip(py, |chip| Ok(chip.oc1().try_into()?))
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

            /// Whether a bus or address error, the bus monitor's
            /// included, stops the run (`Stop.Fault()`) rather than
            /// being taken as an exception, a fault while stacking
            /// included (no double bus fault).  Off at first.
            fn set_stop_on_fault(&mut self, py: Python<'_>, stop: bool) -> PyResult<()> {
                self.with_chip_mut(py, |chip, _| {
                    chip.cpu_control().set_stop_on_fault(stop);
                    Ok(())
                })
            }

            /// Whether a fault stops the run (`set_stop_on_fault`).
            #[getter]
            fn stops_on_fault(&self, py: Python<'_>) -> PyResult<bool> {
                self.with_chip(py, |chip| Ok(chip.cpu().stops_on_fault()))
            }

            /// The fault the last run stopped for (`Stop.Fault()`),
            /// until the next run starts; `None` if none did.
            #[getter]
            fn fault_stop(&self, py: Python<'_>) -> PyResult<Option<FaultStopView>> {
                self.with_chip(py, |chip| Ok(chip.cpu().fault_stop().cloned().map(FaultStopView)))
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
            let halted = self.chip.cpu().halted().is_some();
            let run = self.chip.run_until(target);
            self.pending.check()?;
            let now = self.chip.now();
            let quantized = now >= target
                && (matches!(run.stop, cpu::Stop::Budget | cpu::Stop::Stopped)
                    || (halted && matches!(run.stop, cpu::Stop::Halted(_))));
            if !quantized || now >= end {
                return Ok(cpu::Run::new(now - start, run.stop).try_into()?);
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

    /// A reset now for `cause`, a `ResetCause`, as its source makes it
    /// (Rust's `Mc68331::reset_for`): RSR its bit alone; `SYSTEM`, the
    /// `reset` instruction's, resets all but the CPU; `EXTERNAL` is
    /// `reset()`.
    fn reset_for(&mut self, cause: ResetCause) -> PyResult<()> {
        self.chip.reset_for(cause.into());
        self.pending.check()
    }

    /// RESET driven by the board with BKPT held low through its rising
    /// edge, as a BDM debugger's: background mode enabled, and the CPU
    /// in it, `Entry.Breakpoint()`, before its first instruction
    /// (CPU32RM 7.2.1).
    fn reset_with_breakpoint(&mut self) -> PyResult<()> {
        self.chip.reset_with_breakpoint();
        self.pending.check()
    }

    /// BKPT asserted between runs (Rust's `Mc68331::assert_breakpoint`):
    /// with background mode enabled the CPU enters it at once, as `halt`
    /// makes it, and this is true; disabled, a hardware breakpoint is
    /// left pending for the next run, which takes vector 12 unless a
    /// peripheral ends its acknowledge cycle, and this is false.  A
    /// halted CPU ignores it, and this is false.
    fn assert_breakpoint(&mut self) -> PyResult<bool> {
        let entered = self.chip.assert_breakpoint();
        self.pending.check()?;
        Ok(entered)
    }

    /// BKPT asserted: the CPU enters background mode at once, between
    /// instructions, and its `Entry`.  Raises `TargetError` unless
    /// background mode was enabled at the last reset.
    fn halt(&mut self) -> PyResult<Entry> {
        let entry = Cpu32Target::halt(&mut self.chip);
        self.pending.check()?;
        Ok(entry.map_err(Error::from)?.into())
    }

    /// GO: leaves background mode, to run on at the next run.  Raises
    /// `TargetError` unless the CPU is in background mode.
    fn go(&mut self) -> PyResult<()> {
        Ok(Cpu32Target::go(&mut self.chip).map_err(Error::from)?)
    }

    /// One instruction from background mode, back in it after, and the
    /// `Entry`.  Raises `TargetError` unless the CPU is in background
    /// mode, or if a hook or a stop request ended the step early (the CPU
    /// in background mode where it stopped); a peripheral's exception as
    /// `run_for`.
    fn step(&mut self) -> PyResult<Entry> {
        let entry = Cpu32Target::step(&mut self.chip);
        self.pending.check()?;
        Ok(entry.map_err(Error::from)?.into())
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

    /// The board drives the pins of `port` in `mask` (all of them by
    /// default) to their levels in `value` now, the others as they
    /// were; port C, with no inputs, raises `ValueError`.
    #[pyo3(signature = (port, value, mask = u8::MAX))]
    fn set_pins(&mut self, port: PortView, value: u8, mask: u8) -> PyResult<()> {
        self.chip
            .set_pins(port.into(), value, mask)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        self.pending.check()
    }

    /// The changes in what each port drives since the last call, in time
    /// order: `(now, port, driven, levels)`, a `Port`, its output pins
    /// and their levels as two masks (Rust's `PortChange`, flattened).
    /// A host with no board takes them, or they gather in the chip and
    /// its snapshot.
    fn take_port_changes(&mut self) -> Vec<(u64, PortView, u8, u8)> {
        self.chip
            .take_port_changes()
            .into_iter()
            .map(|change| {
                (
                    change.now.0,
                    change.port.into(),
                    change.outputs.driven(),
                    change.outputs.levels(),
                )
            })
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

    /// The board drives GPT input `input`, `PGP0` to `PGP7`, `PAI` or
    /// `PCLK`, to `level` now.
    fn set_gpt_input(&mut self, input: &str, level: bool) -> Result<(), Error> {
        self.chip.set_gpt_input(peripherals::input(input)?, level);
        Ok(())
    }

    /// The board will drive GPT input `input`, `PGP0` to `PGP7`, `PAI`
    /// or `PCLK`, to `level` at time `at`, or now if that is past; a
    /// run's quantum ends there.
    fn schedule_gpt_input(&mut self, at: u64, input: &str, level: bool) -> Result<(), Error> {
        self.chip
            .schedule_gpt_input(Cycles(at), peripherals::input(input)?, level);
        Ok(())
    }

    /// The PWM pins' edges since the last call, in time order, while
    /// `set_report_pwm_edges` asks for them: `(now, channel, level)`
    /// (Rust's `PwmChange`, flattened).
    fn take_pwm_changes(&mut self) -> Vec<(u64, ChannelView, bool)> {
        self.chip
            .take_pwm_changes()
            .into_iter()
            .map(|change| (change.now.0, change.channel.into(), change.level))
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
    fn take_events(&mut self) -> Result<Vec<Event>, Error> {
        events(self.chip.take_events())
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
