//! The bare CPU32 as a Python host runs it, `mc683xx.Machine`: flat memory
//! at any address, Python peripherals at byte ranges, interrupt requests,
//! and hooks that may read and write the CPU (PLAN.md, Bare CPU and
//! Python).
//!
//! A hook's Rust closure only queues what it saw and stops the core's
//! run; `run_limited` then calls the Python callables with nothing borrowed,
//! so they can call back into the `Machine`, and runs on unless one
//! stopped it.  When each callable runs, and so when its writes take
//! effect, is in docs/python.md (Bare CPU).

use std::cell::Cell;
use std::cell::RefCell;
use std::mem;
use std::ops::ControlFlow;

use std::rc::Rc;

use mc683xx_cpu32::CpuControl;
use mc683xx_cpu32::Cycles;
use mc683xx_cpu32::End;
use mc683xx_cpu32::FaultCause;
use mc683xx_cpu32::FaultStop;
use mc683xx_cpu32::Intercepted;
use mc683xx_cpu32::Limits;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::flat;
use mc683xx_cpu32::flat::Machine;
use mc683xx_cpu32::hooks::Control;
use mc683xx_cpu32::hooks::ExceptionEntry;
use mc683xx_cpu32::hooks::ExceptionReturn;
use mc683xx_cpu32::hooks::HookId;
use mc683xx_cpu32::hooks::Instruction;
use mc683xx_cpu32::hooks::InvalidAccess;
use mc683xx_cpu32::hooks::MemoryAccess;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::memory::Permissions;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_cpu32::registers::Registers;
use mc683xx_gdb::AccessError;
use mc683xx_gdb::Target;
use mc683xx_gdb::TargetReset;
use mc683xx_gdb::TargetRun;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use pyo3::Bound;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyErr;
use pyo3::PyRefMut;
use pyo3::PyResult;
use pyo3::PyTraverseError;
use pyo3::PyVisit;
use pyo3::Python;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyTypeError;
use pyo3::pybacked::PyBackedBytes;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyBytes;
use pyo3::types::PyDict;

use crate::chip::Halt;

use crate::chip::Stop;
use crate::error::Error;
use crate::function_code::FunctionCode as FunctionCodeView;
use crate::gdb;
use crate::held::Held;
use crate::hooks;
use crate::hooks::BusCycleKind;
use crate::hooks::HookIdView;
use crate::mapped::Pending;
use crate::mapped::PythonMapped;
use crate::python::opaque_repr;
use crate::python::repr;
use crate::register::Register;
use crate::requests::StopRequestView;

/// Instructions a run goes between looks for Ctrl-C.
const QUANTUM: u64 = 100_000;

/// What a hook saw, kept until its callable is called.
#[derive(Debug)]
enum Context {
    /// A code or block hook's instruction.
    Instruction(Instruction),
    /// A read or write hook's cycle.
    Memory(MemoryAccess),
    /// An invalid access hook's cycle.
    Invalid(InvalidAccess),
    /// An exception hook's entry.
    Exception(ExceptionEntry),
    /// An exception return hook's return.
    ExceptionReturn(ExceptionReturn),
}

/// A hook that fired: its callable and what it saw.
#[derive(Debug)]
struct Fired {
    /// The hook.
    id: HookId,
    /// Its callable.
    callable: Held,
    /// What it saw.
    context: Context,
}

/// The hooks that fired in a core run, in order.
type Queue = Rc<RefCell<Vec<Fired>>>;

/// An intercept hook's callable and the vectors it covers.
#[derive(Debug)]
struct Intercept {
    /// The hook.
    id: HookId,
    /// The vectors it covers.
    vectors: Vec<u8>,
    /// Its callable.
    callable: Held,
}

/// A peripheral on a bare `Machine`, as `add_peripheral` returns it and
/// `remove_peripheral` takes it (Rust's `PeripheralId`).
#[pyclass(
    name = "PeripheralId",
    module = "mc683xx",
    frozen,
    eq,
    hash,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PeripheralIdView(PeripheralId);

#[pymethods]
impl PeripheralIdView {
    /// `PeripheralId(3)`, from Rust's `Display`, `peripheral 3`.
    fn __repr__(&self) -> String {
        opaque_repr("PeripheralId", &self.0)
    }

    /// Rust's `Display`: `peripheral 3`.
    fn __str__(&self) -> String {
        self.0.to_string()
    }
}

/// Why a fault was one.
#[pyclass(
    name = "FaultCause",
    module = "mc683xx",
    eq,
    hash,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FaultCauseKind {
    /// Nothing answered (unmapped), or a peripheral raised `BusError`.
    BusError,
    /// Its page's permissions do not allow it.
    Protected,
    /// A word or long at an odd address, or an odd PC.
    AddressError,
}

impl From<FaultCauseKind> for FaultCause {
    fn from(cause: FaultCauseKind) -> Self {
        match cause {
            FaultCauseKind::BusError => Self::BusError,
            FaultCauseKind::Protected => Self::Protected,
            FaultCauseKind::AddressError => Self::AddressError,
        }
    }
}

enum_names!(FaultCauseKind, FaultCause);

impl TryFrom<FaultCause> for FaultCauseKind {
    type Error = Error;

    /// The cause, or [`unknown_fault_cause`] for one the bindings do not
    /// know.
    fn try_from(cause: FaultCause) -> Result<Self, Error> {
        Ok(match cause {
            FaultCause::BusError => Self::BusError,
            FaultCause::Protected => Self::Protected,
            FaultCause::AddressError => Self::AddressError,
            other => return Err(unknown_fault_cause(other)),
        })
    }
}

/// A fault cause the bindings do not know, [`Error::Unmapped`] naming
/// it: a value, not a run's end, so `NotImplementedError`.
fn unknown_fault_cause(cause: FaultCause) -> Error {
    Error::named("fault cause", cause)
}

/// A fault that stopped a run (`set_stop_on_fault`), as `End.Fault`
/// and a chip's `fault_stop` carry it (Rust's `FaultStop`).
#[pyclass(
    name = "FaultStop",
    module = "mc683xx",
    frozen,
    eq,
    hash,
    from_py_object
)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct FaultStopView(pub(crate) FaultStop);

#[pymethods]
impl FaultStopView {
    /// Fetch, read or write.
    #[getter]
    fn kind(&self) -> BusCycleKind {
        self.0.kind.into()
    }

    /// Why.
    #[getter]
    fn cause(&self) -> PyResult<FaultCauseKind> {
        Ok(self.0.cause.try_into()?)
    }

    /// The cycle's address, all 32 bits.
    #[getter]
    const fn address(&self) -> u32 {
        self.0.address.0
    }

    /// Bytes in the operand: 1, 2 or 4.
    #[getter]
    const fn size(&self) -> u32 {
        self.0.size.bytes()
    }

    /// Its function code.
    #[getter]
    fn function_code(&self) -> FunctionCodeView {
        self.0.function_code.into()
    }

    /// The faulting instruction's address.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.registers.pc.0
    }

    /// The registers as the instruction, or the trace, hardware
    /// breakpoint or interrupt processing after one, began, which the
    /// CPU holds again.
    #[getter]
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        hooks::register_dictionary(py, &self.0.registers)
    }

    /// Rust's `Display`: `bus error: write of 2 bytes at 0x90000000 in
    /// supervisor-data, pc 0x00001000`.
    fn __str__(&self) -> String {
        self.0.to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "FaultStop(kind={}, cause={}, address={:#010x}, size={}, function_code={}, pc={:#010x})",
            repr(py, self.kind())?,
            repr(py, self.cause()?)?,
            self.address(),
            self.size(),
            repr(py, self.function_code())?,
            self.pc()
        ))
    }
}

/// An exception an instruction raised that an intercept hook answers:
/// nothing stacked, PC at the instruction (Rust's `Intercepted`).
#[pyclass(name = "Intercepted", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct InterceptedView(Intercepted);

#[pymethods]
impl InterceptedView {
    /// Its vector number: 32 to 47 for TRAP #0 to #15.
    #[getter]
    const fn vector(&self) -> u8 {
        self.0.vector.0
    }

    /// The instruction's address, where PC is.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.pc.0
    }

    fn __repr__(&self) -> String {
        format!(
            "Intercepted(vector={}, pc={:#010x})",
            self.vector(),
            self.pc()
        )
    }
}

/// How a limited run ended (Rust's `End`).  No run ends for an
/// intercepted exception: `run_limited` answers it through the intercept
/// hooks, or the CPU takes it natively.  A variant's value is a field
/// named for it, not Rust's `_0`.
#[pyclass(name = "End", module = "mc683xx", frozen, eq, hash)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum EndView {
    /// As a chip's run ends: its `Stop`.
    Stop {
        /// The stop.
        stop: Stop,
    },
    /// PC reached `reach`.
    Reached(),
    /// It ran `instructions`.
    Instructions(),
    /// A fault, with `set_stop_on_fault`.
    Fault {
        /// The fault.
        fault: FaultStopView,
    },
    /// An exception the CPU was about to take, with
    /// `set_stop_on_exception`: nothing stacked.
    Exception {
        /// Its vector number.
        vector: u8,
        /// PC, where its processing begins when the run goes on.
        pc: u32,
    },
}

impl TryFrom<End> for EndView {
    type Error = Error;

    /// The end, or [`Error::Unmapped`] naming one with no Python class:
    /// a step limit's (`run_limited` takes none), an intercepted
    /// exception's (`run_limited` answers it within the run), or one the
    /// bindings do not know.
    fn try_from(end: End) -> Result<Self, Error> {
        Ok(match end {
            End::Stop(stop) => Self::Stop {
                stop: stop.try_into()?,
            },
            End::Reached => Self::Reached(),
            End::Instructions => Self::Instructions(),
            End::Fault(fault) => Self::Fault {
                fault: FaultStopView(fault),
            },
            End::Exception { vector, pc } => Self::Exception {
                vector: vector.0,
                pc: pc.0,
            },
            other => return Err(Error::run_end(other)),
        })
    }
}

impl From<EndView> for End {
    fn from(end: EndView) -> Self {
        match end {
            EndView::Stop { stop } => Self::Stop(stop.into()),
            EndView::Reached() => Self::Reached,
            EndView::Instructions() => Self::Instructions,
            EndView::Fault { fault } => Self::Fault(fault.0),
            EndView::Exception { vector, pc } => Self::Exception {
                vector: Vector(vector),
                pc: Address(pc),
            },
        }
    }
}

#[pymethods]
impl EndView {
    /// Rust's `Display`.
    fn __str__(&self) -> String {
        End::from(self.clone()).to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(match self {
            Self::Stop { stop } => format!("End.Stop({})", repr(py, *stop)?),
            Self::Reached() => "End.Reached()".to_owned(),
            Self::Instructions() => "End.Instructions()".to_owned(),
            Self::Fault { fault } => format!("End.Fault({})", repr(py, fault.clone())?),
            Self::Exception { vector, pc } => format!("End.Exception({vector}, {pc:#010x})"),
        })
    }
}

/// A limited run's outcome (Rust's `LimitedRun`).
#[pyclass(name = "LimitedRun", module = "mc683xx", frozen, eq, hash, get_all)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LimitedRunView {
    /// The cycles it took.
    cycles: u64,
    /// Why it ended.
    end: EndView,
}

#[pymethods]
impl LimitedRunView {
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "LimitedRun(cycles={}, end={})",
            self.cycles,
            repr(py, self.end.clone())?
        ))
    }
}

/// A bare CPU32 on flat memory (Rust's `flat::Machine`).
#[pyclass(name = "Machine", module = "mc683xx", unsendable)]
#[derive(Debug)]
pub(crate) struct MachineView {
    /// The machine.
    machine: Machine,
    /// Where the peripherals' exceptions wait.
    pending: Pending,
    /// The hooks that fired in the core run under way.
    queue: Queue,
    /// The intercept hooks' callables, in the order installed, with the
    /// vectors each covers.
    intercepts: Vec<Intercept>,
    /// Whether a run is under way.
    running: bool,
}

impl MachineView {
    /// Installs `callable`, which must be callable, as a hook with
    /// `install`: the hook queues `context` of what it sees and stops
    /// the core's run, for `run_limited` to call the callable.
    fn install<T: 'static>(
        &mut self,
        callable: &Bound<'_, PyAny>,
        context: impl Fn(&T) -> Context + 'static,
        install: impl FnOnce(&mut mc683xx_cpu32::Cpu, Box<dyn FnMut(&T) -> Control>) -> HookId,
    ) -> PyResult<HookIdView> {
        if !callable.is_callable() {
            return Err(PyTypeError::new_err("a hook must be callable"));
        }
        let callable = self.pending.keep(callable);
        let queue = Rc::clone(&self.queue);
        let id = Rc::new(Cell::new(None));
        let own = Rc::clone(&id);
        let hook = move |seen: &T| {
            if let Some(id) = own.get() {
                queue.borrow_mut().push(Fired {
                    id,
                    callable: callable.clone(),
                    context: context(seen),
                });
            }
            ControlFlow::Break(())
        };
        let assigned = install(&mut self.machine.cpu, Box::new(hook));
        id.set(Some(assigned));
        Ok(HookIdView(assigned))
    }
}

/// The Python view of what a fired hook saw.
fn view(py: Python<'_>, context: Context, registers: &Registers) -> PyResult<Py<PyAny>> {
    Ok(match context {
        Context::Instruction(instruction) => {
            Py::new(py, hooks::instruction(&instruction))?.into_any()
        }
        Context::Memory(access) => {
            Py::new(py, hooks::memory_access_with(access, registers.clone()))?.into_any()
        }
        Context::Invalid(access) => {
            Py::new(py, hooks::invalid_access_with(access, registers.clone()))?.into_any()
        }
        Context::Exception(entry) => Py::new(py, hooks::exception_entry(&entry))?.into_any(),
        Context::ExceptionReturn(exception_return) => {
            Py::new(py, hooks::exception_return(&exception_return))?.into_any()
        }
    })
}

/// How a run's dispatch of its fired hooks went.
enum Dispatched {
    /// Run on.
    Continue,
    /// Stop, for the hook that asked by its result.
    Stop(HookId),
}

/// The run's limits and what it has spent of them.
#[derive(Debug, Clone, Copy)]
struct Budget {
    /// `reach`.
    reach: Option<Address>,
    /// `instructions`, and the count when the run began.
    instructions: Option<(u64, u64)>,
    /// `cycles`.
    cycles: Option<u64>,
    /// The cycles spent so far.
    spent: u64,
}

impl Budget {
    /// The limits that remain, or the run's end if a limit is spent.
    fn remaining(&self, executed: u64) -> Result<Limits, End> {
        let instructions = match self.instructions {
            Some((count, first)) => {
                let done = executed - first;
                if done >= count {
                    return Err(End::Instructions);
                }
                Some(count - done)
            }
            None => None,
        };
        let cycles = match self.cycles {
            Some(cycles) if self.spent >= cycles => {
                return Err(End::Stop(mc683xx_cpu32::Stop::Budget));
            }
            Some(cycles) => Some(Cycles::from_clocks(cycles - self.spent)),
            None => None,
        };
        let mut limits = Limits::default();
        limits.cycles = cycles;
        limits.instructions = instructions;
        limits.reach = self.reach;
        Ok(limits)
    }
}

#[pymethods]
impl MachineView {
    /// Shows Python's cycle collector the Python objects it holds.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        self.pending.traverse(&visit)
    }

    /// Drops them, for the collector to break a cycle through it.
    fn __clear__(&mut self) {
        self.pending.clear();
    }

    /// A CPU32 on `address_bits` address lines, 32 by default (no
    /// address aliases), nothing mapped, its registers zero, not
    /// reset.
    #[new]
    #[pyo3(signature = (*, address_bits = flat::ADDRESS_BITS))]
    fn new(address_bits: u32) -> Result<Self, Error> {
        let machine = Machine::new(address_bits)?;
        let pending = Pending::new(machine.stop_request());
        Ok(Self {
            machine,
            pending,
            queue: Queue::default(),
            intercepts: Vec::new(),
            running: false,
        })
    }

    /// Maps `length` bytes of RAM from `start` in both spaces, whole
    /// 2-Kbyte pages, none mapped already: zeros, then `contents`;
    /// `permissions` as `protect` takes them.
    #[pyo3(signature = (start, length, *, permissions = "rwx", contents = None))]
    fn map(
        &mut self,
        start: u32,
        length: u32,
        permissions: &str,
        contents: Option<PyBackedBytes>,
    ) -> Result<(), Error> {
        let permissions = permissions.parse::<Permissions>()?;
        let contents = contents.as_deref().unwrap_or_default();
        self.machine.map(Address(start), length, contents)?;
        Ok(self.machine.protect(Address(start), length, permissions)?)
    }

    /// Unmaps `length` bytes from `start`, whole pages.
    fn unmap(&mut self, start: u32, length: u32) -> Result<(), Error> {
        Ok(self.machine.unmap(Address(start), length)?)
    }

    /// Sets the permissions of `length` bytes from `start`, whole pages,
    /// in both spaces: `r`, `w` and `x` for the cycles allowed.  A
    /// cycle they do not allow ends in a bus error.
    fn protect(&mut self, start: u32, length: u32, permissions: &str) -> Result<(), Error> {
        let permissions = permissions.parse::<Permissions>()?;
        Ok(self.machine.protect(Address(start), length, permissions)?)
    }

    /// `length` bytes at `address` with no bus cycle: mapped memory's,
    /// whatever its permissions, and a peripheral's from its `peek`.
    /// Raises `BusError` if any is neither, `UnpeekableError` for one a
    /// peripheral declines, and `ValueError` for more than 32 bits of
    /// addresses.
    fn read<'py>(
        &self,
        py: Python<'py>,
        address: u32,
        length: usize,
    ) -> Result<Bound<'py, PyBytes>, Error> {
        let size = length;
        let length = u32::try_from(size).map_err(|_| Error::Length(size))?;
        // A byte of each page first, so that an unmapped one raises
        // before the bytes are allocated.
        let mut offset = 0;
        while offset < length {
            let at = Address(address).offset(offset);
            self.machine.read(at, &mut [0])?;
            offset = offset.saturating_add(PAGE_BYTES - at.0 % PAGE_BYTES);
        }
        let mut bytes = vec![0; size];
        self.machine.read(Address(address), &mut bytes)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Writes `data` at `address`, as `read` reads it: into mapped
    /// memory, and a peripheral's bytes through its `poke`.  On an error
    /// nothing is written, but for the bytes before one a peripheral
    /// declines.
    fn write(&mut self, address: u32, data: PyBackedBytes) -> Result<(), Error> {
        Ok(self.machine.write(Address(address), &data)?)
    }

    /// At addresses `begin` to `end`, both included, which no other
    /// peripheral's overlap, `peripheral`, which has `read(cycle) -> int`
    /// and `write(cycle, value)`: it answers the cycles there that no
    /// mapped memory serves, before they land.  It saves as a chip's
    /// mapped peripheral does, through `save()` and `restore(bytes)`.
    /// Its id, for `remove_peripheral`.
    fn add_peripheral(
        &mut self,
        begin: u32,
        end: u32,
        peripheral: &Bound<'_, PyAny>,
    ) -> PyResult<PeripheralIdView> {
        let peripheral = PythonMapped::mapped(peripheral, self.pending.clone())?;
        let id = self
            .machine
            .add_peripheral(Address(begin)..=Address(end), Box::new(peripheral))
            .map_err(Error::from)?;
        Ok(PeripheralIdView(id))
    }

    /// Takes the peripheral `peripheral` off the bus: whether there was
    /// one.
    fn remove_peripheral(&mut self, peripheral: PeripheralIdView) -> bool {
        self.machine.remove_peripheral(peripheral.0).is_some()
    }

    /// Register `name`: `D0`-`D7`, `A0`-`A7`, `PC`, `SR`, `CCR`, `USP`,
    /// `SSP`, `VBR`, `SFC`, `DFC`.
    fn read_register(&self, name: &str) -> Result<u32, Error> {
        Ok(name.parse::<Register>()?.read(&self.machine.cpu.registers))
    }

    /// Sets register `name` to `value`, which must fit it.
    fn write_register(&mut self, name: &str, value: u32) -> Result<(), Error> {
        name.parse::<Register>()?
            .write(&mut self.machine.cpu.registers, value)
    }

    /// Every register by name.
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        hooks::register_dictionary(py, &self.machine.cpu.registers)
    }

    /// The reset exception (CPU32RM 6.2.1): SSP and PC from the long
    /// words at zero.  A fault reading them halts the CPU (`halt`).  A
    /// peripheral's exception in the reads is raised here.
    fn reset(&mut self) -> PyResult<()> {
        self.machine.reset();
        self.pending.check()
    }

    /// Requests an interrupt on IRQ`pin`, 1 to 7, answered by `vector`
    /// at its acknowledge, or the autovector if `None`; it stays
    /// requested until `clear_interrupt`.
    #[pyo3(signature = (pin, vector = None))]
    fn request_interrupt(&mut self, pin: u8, vector: Option<u8>) -> Result<(), Error> {
        self.machine.request_interrupt(
            IrqPin::try_from(pin).map_err(Error::from)?,
            vector.map(Vector),
        );
        Ok(())
    }

    /// Takes the exception of `vector` now, as the CPU takes its own: a
    /// four-word frame with SR and PC stacked, S set, T1 and T0 clear,
    /// PC at the handler.  `level`, 0 to 7, sets the interrupt mask, as
    /// an interrupt does, whatever it was; 0, which no interrupt sets,
    /// clears it.  No clocks pass.  The hooks
    /// it fires are called before it returns, their results stopping
    /// nothing.  A halted CPU, or a fault that stops
    /// (`set_stop_on_fault`), raises `RunError`: no exception entered,
    /// the registers rolled back, the frame's writes before the fault
    /// kept.
    #[pyo3(signature = (vector, level = None))]
    fn take_exception(slf: &Bound<'_, Self>, vector: u8, level: Option<u8>) -> PyResult<()> {
        let level = level
            .map(InterruptLevel::try_from)
            .transpose()
            .map_err(Error::from)?;
        let (taken, fired, registers) = {
            let mut this = running(slf)?;
            let taken = this.machine.take_exception(Vector(vector), level);
            let fired = mem::take(&mut *this.queue.borrow_mut());
            let registers = this.machine.cpu.instruction_registers().clone();
            (taken, fired, registers)
        };
        let pending = slf.borrow().pending.clone();
        let dispatched = pending
            .check()
            .and_then(|()| Self::dispatch(slf, slf.py(), fired, &registers));
        slf.borrow_mut().running = false;
        dispatched?;
        Ok(taken.map_err(Error::from)?)
    }

    /// Withdraws the request on IRQ`pin`.
    fn clear_interrupt(&mut self, pin: u8) -> Result<(), Error> {
        self.machine
            .clear_interrupt(IrqPin::try_from(pin).map_err(Error::from)?);
        Ok(())
    }

    /// Runs until PC reaches `reach` (before the hooks there), it has
    /// run `instructions`, its cycle estimate reaches `cycles`, a hook
    /// or the stop request stops it, a fault does
    /// (`set_stop_on_fault`), or the CPU
    /// stops or halts.  `begin` sets PC first and forgets a hook stop's
    /// skip, so the hooks at `begin` are called.  With no `cycles`, a
    /// `stop` instruction ends the run at once.
    #[pyo3(signature = (*, begin = None, reach = None, instructions = None, cycles = None))]
    fn run_limited(
        slf: &Bound<'_, Self>,
        begin: Option<u32>,
        reach: Option<u32>,
        instructions: Option<u64>,
        cycles: Option<u64>,
    ) -> PyResult<LimitedRunView> {
        let first = {
            let mut this = running(slf)?;
            if let Some(begin) = begin {
                this.machine.cpu.registers.pc = Address(begin);
                this.machine.cpu.clear_resume();
            }
            this.machine.stop_request().withdraw();
            this.machine.cpu.instructions()
        };
        let budget = Budget {
            reach: reach.map(Address),
            instructions: instructions.map(|count| (count, first)),
            cycles,
            spent: 0,
        };
        let run = Self::run_dispatching(slf, budget, &|| false);
        slf.borrow_mut().running = false;
        let (cycles, end) = run?;
        Ok(LimitedRunView {
            cycles,
            end: end.try_into()?,
        })
    }

    /// The handle by which the host asks the run to stop: from a hook's
    /// callable, once the callables for this stop have been called; from
    /// a peripheral, once its instruction ends.  A request left from
    /// before a run is withdrawn as `run_limited` starts.
    fn stop_request(&self) -> StopRequestView {
        StopRequestView(self.machine.stop_request())
    }

    /// Waits for gdb at `address` (`host:port`, port 0 picks one),
    /// calls `ready(port)` once it listens, and lets gdb run the CPU
    /// until it detaches (`Ending.DETACHED`), kills it (`Ending.KILLED`)
    /// or hangs up (`Ending.HUNG_UP`)
    /// (docs/gdb.md).  Peripherals, hooks and intercepts run as in `run_limited`;
    /// an exception one raises ends the session and is raised here.
    #[pyo3(signature = (address = gdb::ADDRESS, ready = None))]
    fn serve_gdb(
        slf: &Bound<'_, Self>,
        address: &str,
        ready: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<gdb::Ending> {
        drop(running(slf)?);
        let served = gdb::accept(slf.py(), address, ready)
            .and_then(|connection| gdb::serve(&mut Debugged(slf), connection));
        slf.borrow_mut().running = false;
        served
    }

    /// Calls `callback(Intercepted)` for each exception an instruction
    /// raises with a vector of `vectors`, before anything is stacked, PC
    /// at the instruction.  A truthy result means it handled it: the CPU
    /// takes nothing and runs on from PC as the callable left it.
    /// Otherwise the next hook that covers the vector is called, in the
    /// order installed, and if none handles it the CPU takes it
    /// natively.
    fn add_intercept_hook(
        &mut self,
        callback: &Bound<'_, PyAny>,
        vectors: Vec<u8>,
    ) -> PyResult<HookIdView> {
        if !callback.is_callable() {
            return Err(PyTypeError::new_err("a hook must be callable"));
        }
        let callable = self.pending.keep(callback);
        let id = self
            .machine
            .cpu
            .add_intercept_hook(vectors.iter().copied().map(Vector), |_| {
                ControlFlow::Break(())
            });
        self.intercepts.push(Intercept {
            id,
            vectors,
            callable,
        });
        Ok(HookIdView(id))
    }

    /// Whether a fault stops the run (`End.Fault`) rather
    /// than being taken as a bus or address error exception, a fault
    /// while stacking included (no double bus fault).  Off at first.
    fn set_stop_on_fault(&mut self, stop: bool) {
        self.machine.cpu.set_stop_on_fault(stop);
    }

    /// Whether a fault stops the run (`set_stop_on_fault`).
    #[getter]
    fn stops_on_fault(&self) -> bool {
        self.machine.cpu.stops_on_fault()
    }

    /// Whether any exception the CPU is about to take, a trap, a fault,
    /// a trace, a hardware breakpoint or an interrupt, stops the run
    /// before anything is stacked (`End.Exception`).  The next run
    /// takes it natively.  A fault with `set_stop_on_fault` is a fault
    /// stop instead, and an intercept hook's exception is the hook's.
    /// Off at first.
    fn set_stop_on_exception(&mut self, stop: bool) {
        self.machine.cpu.set_stop_on_exception(stop);
    }

    /// Whether any exception stops the run (`set_stop_on_exception`).
    #[getter]
    fn stops_on_exception(&self) -> bool {
        self.machine.cpu.stops_on_exception()
    }

    /// Instructions run since the CPU was made.
    #[getter]
    const fn instructions(&self) -> u64 {
        self.machine.cpu.instructions()
    }

    /// The CPU's clock: the cycles its runs took.
    #[getter]
    const fn now(&self) -> u64 {
        self.machine.now().clocks()
    }

    /// Why the CPU halted, while it stays halted.
    #[getter]
    fn halted(&self) -> PyResult<Option<Halt>> {
        Ok(self.machine.cpu.halted().map(Halt::try_from).transpose()?)
    }

    /// The vector of the first exception the last instruction or
    /// interrupt took, if any.
    #[getter]
    fn exception_taken(&self) -> Option<u8> {
        self.machine
            .cpu
            .exception_taken()
            .map(|exception| exception.vector().0)
    }

    /// Calls `callback(Instruction)` before each instruction from
    /// `begin` to `end`; it may change the CPU, and a PC it writes is
    /// where the run goes on, the instruction not run.
    #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
    fn add_code_hook(
        &mut self,
        callback: &Bound<'_, PyAny>,
        begin: u32,
        end: u32,
    ) -> PyResult<HookIdView> {
        let range = Address(begin)..=Address(end);
        self.install(
            callback,
            |seen: &Instruction| Context::Instruction(seen.clone()),
            |cpu, hook| cpu.add_code_hook(range, hook),
        )
    }

    /// Calls `callback(Instruction)` before the first instruction of
    /// each block that starts from `begin` to `end`, before the code
    /// hooks, as `add_code_hook`.
    #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
    fn add_block_hook(
        &mut self,
        callback: &Bound<'_, PyAny>,
        begin: u32,
        end: u32,
    ) -> PyResult<HookIdView> {
        let range = Address(begin)..=Address(end);
        self.install(
            callback,
            |seen: &Instruction| Context::Instruction(seen.clone()),
            |cpu, hook| cpu.add_block_hook(range, hook),
        )
    }

    /// Calls `callback(MemoryAccess)` for each operand read from `begin`
    /// to `end`, once the instruction has completed.
    #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
    fn add_read_hook(
        &mut self,
        callback: &Bound<'_, PyAny>,
        begin: u32,
        end: u32,
    ) -> PyResult<HookIdView> {
        let range = Address(begin)..=Address(end);
        self.install(
            callback,
            |seen: &MemoryAccess| Context::Memory(*seen),
            |cpu, hook| cpu.add_read_hook(range, hook),
        )
    }

    /// Calls `callback(MemoryAccess)` for each write from `begin` to
    /// `end`, once the instruction has completed.
    #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
    fn add_write_hook(
        &mut self,
        callback: &Bound<'_, PyAny>,
        begin: u32,
        end: u32,
    ) -> PyResult<HookIdView> {
        let range = Address(begin)..=Address(end);
        self.install(
            callback,
            |seen: &MemoryAccess| Context::Memory(*seen),
            |cpu, hook| cpu.add_write_hook(range, hook),
        )
    }

    /// Calls `callback(InvalidAccess)` for each cycle from `begin` to
    /// `end` that ends in a bus error, once the exception is taken (or
    /// the run stopped for it, with `stop_on_fault`).
    #[pyo3(signature = (callback, begin = 0, end = u32::MAX))]
    fn add_invalid_hook(
        &mut self,
        callback: &Bound<'_, PyAny>,
        begin: u32,
        end: u32,
    ) -> PyResult<HookIdView> {
        let range = Address(begin)..=Address(end);
        self.install(
            callback,
            |seen: &InvalidAccess| Context::Invalid(*seen),
            |cpu, hook| cpu.add_invalid_hook(range, hook),
        )
    }

    /// Calls `callback(ExceptionEntry)` after each exception the CPU
    /// takes, its frame stacked and PC the handler's, before the
    /// handler runs.
    fn add_exception_hook(&mut self, callback: &Bound<'_, PyAny>) -> PyResult<HookIdView> {
        self.install(
            callback,
            |seen: &ExceptionEntry| Context::Exception(*seen),
            |cpu, hook| cpu.add_exception_hook(hook),
        )
    }

    /// Calls `callback(ExceptionReturn)` after each exception is
    /// returned from, `rte` having restored SR and PC from its frame,
    /// before the instruction it returns to runs.
    fn add_exception_return_hook(&mut self, callback: &Bound<'_, PyAny>) -> PyResult<HookIdView> {
        self.install(
            callback,
            |seen: &ExceptionReturn| Context::ExceptionReturn(*seen),
            |cpu, hook| cpu.add_exception_return_hook(hook),
        )
    }

    /// Removes `hook`; whether it was installed.
    fn remove_hook(&mut self, hook: &HookIdView) -> bool {
        self.intercepts.retain(|intercept| intercept.id != hook.0);
        self.machine.cpu.remove_hook(hook.0)
    }

    /// The CPU's whole state as bytes: registers, memory, the clock, the
    /// interrupt requests and each peripheral's `save()`, in the order
    /// added; not the hooks or settings.  A peripheral with no `save` raises
    /// `StateError` naming it.
    fn save<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let saved = self.machine.save();
        self.pending.check()?;
        Ok(PyBytes::new(py, &saved.map_err(Error::from)?))
    }

    /// Restores bytes `save` gave, of a CPU mapped alike with as many
    /// peripherals, added in the same order, each by its `restore(bytes)`.
    /// Another format, other memory or another count of peripherals raises
    /// `StateError`, the CPU and its peripherals unchanged.
    fn restore(&mut self, snapshot: PyBackedBytes) -> PyResult<()> {
        let restored = self.machine.restore(&snapshot);
        self.pending.check()?;
        Ok(restored.map_err(Error::from)?)
    }
}

/// `slf`, borrowed for a run, which must not be under way.
fn running<'py>(slf: &Bound<'py, MachineView>) -> PyResult<PyRefMut<'py, MachineView>> {
    let mut this = slf.try_borrow_mut()?;
    if this.running {
        return Err(PyRuntimeError::new_err("run from within a run"));
    }
    this.running = true;
    Ok(this)
}

impl MachineView {
    /// `run_limited`'s loop: core runs, each followed by the callables of the
    /// hooks that stopped it, with nothing borrowed.  Each runs at most
    /// `QUANTUM` instructions, and Ctrl-C is looked for between them; a
    /// run ended for its instructions starts again where it was, so the
    /// quanta change nothing the CPU does.  The GIL stays held: the
    /// machine holds Python objects and is not `Send`.  It stops after a
    /// core run once `halt` is true: the GDB stub's hook asked, which
    /// the stub tells by `halt`, not by the end.  The cycles it took and
    /// why it ended.
    fn run_dispatching(
        slf: &Bound<'_, Self>,
        mut budget: Budget,
        halt: &dyn Fn() -> bool,
    ) -> PyResult<(u64, End)> {
        let py = slf.py();
        loop {
            let (run, fired, registers) = {
                let mut this = slf.borrow_mut();
                let limits = match budget.remaining(this.machine.cpu.instructions()) {
                    Ok(mut limits) => {
                        limits.instructions = Some(
                            limits
                                .instructions
                                .map_or(QUANTUM, |count| count.min(QUANTUM)),
                        );
                        limits
                    }
                    Err(end) => return Ok((budget.spent, end)),
                };
                let run = this.machine.run_limited(limits);
                let fired = mem::take(&mut *this.queue.borrow_mut());
                let registers = if fired.is_empty() {
                    None
                } else {
                    Some(this.machine.cpu.instruction_registers().clone())
                };
                (run, fired, registers)
            };
            budget.spent = budget.spent.saturating_add(run.cycles.clocks());
            let pending = slf.borrow().pending.clone();
            pending.check()?;
            let dispatched = match registers {
                Some(registers) => Self::dispatch(slf, py, fired, &registers)?,
                None => Dispatched::Continue,
            };
            let stop = slf.borrow().machine.stop_request().take();
            let requested = End::Stop(mc683xx_cpu32::Stop::Requested);
            if halt() {
                return Ok((budget.spent, requested));
            }
            match (run.end, dispatched) {
                (End::Intercepted(interception), Dispatched::Continue) if !stop => {
                    Self::answer(slf, py, interception)?;
                    if slf.borrow().machine.stop_request().take() {
                        return Ok((budget.spent, requested));
                    }
                }
                (End::Stop(mc683xx_cpu32::Stop::Hook(_)), Dispatched::Continue) if !stop => {}
                // A quantum's end, or the run's, which `remaining` finds.
                (End::Instructions, Dispatched::Continue) if !stop => py.check_signals()?,
                (_, Dispatched::Stop(hook)) => {
                    return Ok((budget.spent, End::Stop(mc683xx_cpu32::Stop::Hook(hook))));
                }
                (_, Dispatched::Continue) if stop => return Ok((budget.spent, requested)),
                (end, Dispatched::Continue) => return Ok((budget.spent, end)),
            }
        }
    }

    /// Calls the callables of `fired`, in order: the first that stopped
    /// the run by its result, or an exception one raised.
    fn dispatch(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        fired: Vec<Fired>,
        registers: &Registers,
    ) -> PyResult<Dispatched> {
        let mut stopped = None;
        for fired in fired {
            // A hook removed by an earlier callable is not called.
            let installed = slf.borrow().machine.cpu.has_hook(fired.id);
            if !installed {
                continue;
            }
            let argument = view(py, fired.context, registers)?;
            let result = fired.callable.call1(py, (argument,))?;
            if result.bind(py).is_truthy()? {
                stopped.get_or_insert(fired.id);
            }
        }
        Ok(stopped.map_or(Dispatched::Continue, Dispatched::Stop))
    }

    /// Asks the intercept hooks' callables covering `interception`'s
    /// vector to answer it, in the order installed, until one handles
    /// it; unless one did, the instruction takes it natively when it
    /// reruns.  A hook an earlier callable removed is not called.
    fn answer(slf: &Bound<'_, Self>, py: Python<'_>, interception: Intercepted) -> PyResult<()> {
        let covering: Vec<(HookId, Held)> = slf
            .borrow()
            .intercepts
            .iter()
            .filter(|intercept| intercept.vectors.contains(&interception.vector.0))
            .map(|intercept| (intercept.id, intercept.callable.clone()))
            .collect();
        let mut handled = false;
        for (id, callable) in covering {
            if !slf.borrow().machine.cpu.has_hook(id) {
                continue;
            }
            let answer = callable.call1(py, (InterceptedView(interception),))?;
            if answer.bind(py).is_truthy()? {
                handled = true;
                break;
            }
        }
        if !handled {
            let mut this = slf.borrow_mut();
            if this.machine.cpu.registers.pc == interception.pc {
                this.machine.cpu.take_natively_once();
            }
        }
        Ok(())
    }
}

/// The bare CPU under gdb: its runs dispatch the Python hooks as `run_limited`
/// does, and end once the stub's hook asks.
struct Debugged<'a, 'py>(&'a Bound<'py, MachineView>);

impl Target for Debugged<'_, '_> {
    type Error = PyErr;

    fn cpu_scope<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        Some(call(&mut CpuControl::new(
            &mut self.0.borrow_mut().machine.cpu,
        )))
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<PyErr>> {
        Target::read_memory(
            &mut self.0.borrow_mut().machine,
            address,
            function_code,
            data,
        )
        .map_err(|error| error.map(|never| match never {}))
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<PyErr>> {
        Target::write_memory(
            &mut self.0.borrow_mut().machine,
            address,
            function_code,
            data,
        )
        .map_err(|error| error.map(|never| match never {}))
    }

    fn run_for(&mut self, budget: Cycles, hit: &dyn Fn() -> bool) -> PyResult<TargetRun> {
        let limits = Budget {
            reach: None,
            instructions: None,
            cycles: Some(budget.clocks()),
            spent: 0,
        };
        let (_, end) = MachineView::run_dispatching(self.0, limits, hit)?;
        self.0.py().check_signals()?;
        if hit() {
            return Ok(TargetRun::Hook);
        }
        Ok(match end {
            End::Stop(stop) => stop.into(),
            End::Reached | End::Instructions | End::Steps => TargetRun::Quantum,
            End::Fault(_) | End::Intercepted(_) => TargetRun::Fault,
            other => return Err(Error::run_end(other).into()),
        })
    }

    fn reset(&mut self) -> PyResult<TargetReset> {
        Ok(Target::reset(&mut self.0.borrow_mut().machine).unwrap_or_else(|never| match never {}))
    }
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;

    use mc683xx_board::peripheral::Role;
    use mc683xx_cpu32::Cycles;
    use mc683xx_cpu32::End;
    use mc683xx_cpu32::FaultCause;
    use mc683xx_cpu32::Limits;
    use mc683xx_cpu32::exception::Vector;
    use mc683xx_cpu32::flat::Machine;
    use mc683xx_cpu32::hooks::Invalid;
    use mc683xx_cpu32::memory::PAGE_BYTES;
    use mc683xx_mc68331::gpt::pwm;
    use mc683xx_modules::sim;
    use mc683xx_target::Address;
    use mc683xx_target::Entry;
    use mc683xx_target::Register;
    use pyo3::PyErr;
    use pyo3::Python;
    use pyo3::exceptions::PyNotImplementedError;

    use super::EndView;
    use super::FaultCauseKind;
    use super::unknown_fault_cause;
    use crate::board::RoleView;
    use crate::chip::Halt;
    use crate::chip::ResetCause;
    use crate::chip::Stop;
    use crate::error::Error;
    use crate::error::RUN_END;
    use crate::exceptions::RunError;
    use crate::hooks::InvalidReason;
    use crate::peripherals::Output;

    /// Where the test programs start, past the reset vectors.
    const START: u32 = 0x400;
    /// The initial SSP, the top of the page.
    const STACK: u32 = PAGE_BYTES;
    /// An address no page maps.
    const UNMAPPED: u32 = 0x10_0000;
    /// NOP (CPU32RM 4.3).
    const NOP: u16 = 0x4E71;
    /// TRAP #0 (CPU32RM 4.3), vector 32 (CPU32RM Table 6-1).
    const TRAP_0: u16 = 0x4E40;
    /// TRAP #0's vector.
    const TRAP_0_VECTOR: Vector = Vector(32);
    /// MOVE.L (A0),D0 (CPU32RM 4.3).
    const MOVE_LONG_A0_TO_D0: u16 = 0x2010;

    /// A machine reset into `program` at [`START`], set up by `setup`,
    /// and how a run under `limits` ends.
    fn end(program: &[u16], limits: Limits, setup: impl FnOnce(&mut Machine)) -> End {
        let mut image = vec![0; PAGE_BYTES as usize];
        image[0..4].copy_from_slice(&STACK.to_be_bytes());
        image[4..8].copy_from_slice(&START.to_be_bytes());
        for (index, word) in program.iter().enumerate() {
            let at = START as usize + 2 * index;
            image[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
        let mut machine = Machine::new(32).unwrap();
        machine.map(Address(0), PAGE_BYTES, &image).unwrap();
        machine.reset();
        setup(&mut machine);
        machine.run_limited(limits).end
    }

    /// An unknown fault cause is a value, not a run's end: it raises
    /// `NotImplementedError`, not `RunError`.
    #[test]
    fn an_unknown_fault_cause_is_not_implemented() {
        Python::initialize();
        let error = PyErr::from(unknown_fault_cause(FaultCause::BusError));
        Python::attach(|py| {
            assert!(error.is_instance_of::<PyNotImplementedError>(py));
            assert!(!error.is_instance_of::<RunError>(py));
        });
    }

    /// Every `End` and every `Stop` a run gives has its Python class,
    /// or raises `RunError` naming it, the two `run_limited` never
    /// gives; none panics or maps to another.  A variant added to
    /// either belongs here, and in the mappings.
    #[test]
    fn every_run_end_maps_or_names_itself() {
        let mut reach = Limits::default();
        reach.reach = Some(Address(START + 2));
        let mut instructions = Limits::default();
        instructions.instructions = Some(1);
        let mut steps = Limits::default();
        steps.steps = Some(1);
        let mut budget = Limits::default();
        budget.cycles = Some(Cycles::from_clocks(1));
        let ends = [
            end(&[NOP, NOP], reach, |_| ()),
            end(&[NOP, NOP], instructions, |_| ()),
            end(&[NOP, NOP], steps, |_| ()),
            end(&[NOP, NOP], budget, |_| ()),
            end(&[MOVE_LONG_A0_TO_D0], instructions, |machine| {
                machine.cpu.registers.set_register(Register::A0, UNMAPPED);
                machine.cpu.set_stop_on_fault(true);
            }),
            end(&[TRAP_0], instructions, |machine| {
                machine
                    .cpu
                    .add_intercept_hook([TRAP_0_VECTOR], |_| ControlFlow::Break(()));
            }),
        ];
        let mapped: Vec<_> = ends.into_iter().map(EndView::try_from).collect();
        assert!(matches!(mapped[0], Ok(EndView::Reached())));
        assert!(matches!(mapped[1], Ok(EndView::Instructions())));
        assert!(
            matches!(&mapped[2], Err(Error::Unmapped { kind: RUN_END, name: Some(name) }) if name == "steps")
        );
        assert!(matches!(
            mapped[3],
            Ok(EndView::Stop {
                stop: Stop::Budget()
            })
        ));
        assert!(matches!(mapped[4], Ok(EndView::Fault { .. })));
        assert!(
            matches!(&mapped[5], Err(Error::Unmapped { kind: RUN_END, name: Some(name) }) if name == "intercepted")
        );

        let mut machine = Machine::new(32).unwrap();
        let hook = machine
            .cpu
            .add_code_hook(Address(0)..=Address(0), |_| ControlFlow::Continue(()));
        let stops = [
            mc683xx_cpu32::Stop::Budget,
            mc683xx_cpu32::Stop::Stopped,
            mc683xx_cpu32::Stop::Halted(mc683xx_cpu32::Halt::DoubleBusFault),
            mc683xx_cpu32::Stop::Hook(hook),
            mc683xx_cpu32::Stop::Requested,
            mc683xx_cpu32::Stop::Background(Entry::Breakpoint),
            mc683xx_cpu32::Stop::Fault,
        ];
        let mapped: Vec<_> = stops.into_iter().map(Stop::try_from).collect();
        assert!(matches!(mapped[0], Ok(Stop::Budget())));
        assert!(matches!(mapped[1], Ok(Stop::Stopped())));
        assert!(matches!(mapped[2], Ok(Stop::Halted { .. })));
        assert!(matches!(mapped[3], Ok(Stop::Hook { .. })));
        assert!(matches!(mapped[4], Ok(Stop::Requested())));
        assert!(matches!(mapped[5], Ok(Stop::Background { .. })));
        assert!(matches!(mapped[6], Ok(Stop::Fault())));
    }

    /// Every variant of a `non_exhaustive` enum the bindings convert
    /// has its Python value; a variant added to one belongs here, and
    /// in its mapping, which otherwise raises rather than panics.
    #[test]
    fn every_value_maps() {
        for cause in [
            FaultCause::BusError,
            FaultCause::Protected,
            FaultCause::AddressError,
        ] {
            assert!(FaultCauseKind::try_from(cause).is_ok());
        }
        assert!(Halt::try_from(mc683xx_cpu32::Halt::DoubleBusFault).is_ok());
        for cause in sim::ResetCause::ALL {
            assert_eq!(sim::ResetCause::from(ResetCause::from(cause)), cause);
        }
        for role in [Role::Read, Role::Write, Role::ReadWrite, Role::Iack] {
            assert!(RoleView::try_from(role).is_ok());
        }
        for invalid in [Invalid::BusError, Invalid::Protected] {
            assert!(InvalidReason::try_from(invalid).is_ok());
        }
        for output in [
            pwm::Output::Level(true),
            pwm::Output::Wave {
                period_counts: 256,
                high_counts: 1,
                clocks_per_count: None,
            },
            pwm::Output::Clock {
                clocks_per_count: None,
            },
        ] {
            assert!(Output::try_from(output).is_ok());
        }
    }
}
