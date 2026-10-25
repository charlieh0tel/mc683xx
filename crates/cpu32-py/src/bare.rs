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

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu;
use mc683xx_cpu32::cpu::bare::Deviations;
use mc683xx_cpu32::cpu::bare::End;
use mc683xx_cpu32::cpu::bare::FaultCause;
use mc683xx_cpu32::cpu::bare::FaultStop;
use mc683xx_cpu32::cpu::bare::Intercepted;
use mc683xx_cpu32::cpu::bare::Limits;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cpu::hooks::Control;
use mc683xx_cpu32::cpu::hooks::ExceptionEntry;
use mc683xx_cpu32::cpu::hooks::HookId;
use mc683xx_cpu32::cpu::hooks::Instruction;
use mc683xx_cpu32::cpu::hooks::InvalidAccess;
use mc683xx_cpu32::cpu::hooks::MemoryAccess;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::flat;
use mc683xx_cpu32::flat::Machine;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_cpu32::registers::Registers;
use mc683xx_gdb::target::Target;
use mc683xx_gdb::target::TargetRun;
use mc683xx_gdb::target::Unwritable;
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
use crate::gdb;
use crate::held::Held;
use crate::hooks;
use crate::hooks::CycleKind;
use crate::hooks::HookIdView;
use crate::mapped::Pending;
use crate::mapped::PythonMapped;
use crate::permissions;
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

/// Why a fault was one.
#[pyclass(
    name = "FaultCause",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
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

impl From<FaultCause> for FaultCauseKind {
    fn from(cause: FaultCause) -> Self {
        match cause {
            FaultCause::BusError => Self::BusError,
            FaultCause::Protected => Self::Protected,
            FaultCause::AddressError => Self::AddressError,
            _ => panic!("a fault cause the bindings do not know"),
        }
    }
}

/// A fault that stopped a run, as `End.Fault` carries it (Rust's
/// `bare::FaultStop`).
#[pyclass(name = "FaultStop", module = "mc683xx", frozen, eq, from_py_object)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FaultStopView(FaultStop);

#[pymethods]
impl FaultStopView {
    /// Fetch, read or write.
    #[getter]
    fn cycle(&self) -> CycleKind {
        self.0.cycle.into()
    }

    /// Why.
    #[getter]
    fn cause(&self) -> FaultCauseKind {
        self.0.cause.into()
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

    /// FC[2:0].
    #[getter]
    const fn function_code(&self) -> u8 {
        self.0.function_code.bits()
    }

    /// The faulting instruction's address.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.registers.pc
    }

    /// The registers as the instruction began, which the CPU holds
    /// again.
    #[getter]
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        hooks::register_dictionary(py, &self.0.registers)
    }

    fn __repr__(&self) -> String {
        format!(
            "FaultStop(cycle={:?}, cause={:?}, address={:#010x}, size={}, function_code={}, pc={:#010x})",
            self.cycle(),
            self.cause(),
            self.address(),
            self.size(),
            self.function_code(),
            self.pc()
        )
    }
}

/// An exception an instruction raised that the intercept callback
/// answers: nothing stacked, PC at the instruction (Rust's
/// `bare::Intercepted`).
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

/// How a limited run ended (Rust's `bare::End`).  No run ends for an
/// intercepted exception: `run_limited` answers it through the intercept
/// callback, or the CPU takes it natively.
#[pyclass(name = "End", module = "mc683xx", frozen, eq)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EndView {
    /// As a chip's run ends: its `Stop`.
    Stop(Stop),
    /// PC reached `until`.
    Until(),
    /// It ran `instructions`.
    Instructions(),
    /// A fault, with `set_stop_on_fault`.
    Fault(FaultStopView),
}

impl From<End> for EndView {
    fn from(end: End) -> Self {
        match end {
            End::Stop(stop) => Self::Stop(stop.into()),
            End::Until => Self::Until(),
            End::Instructions => Self::Instructions(),
            End::Fault(fault) => Self::Fault(FaultStopView(fault)),
            // answered within the run, never its end
            _ => Self::Stop(Stop::Requested()),
        }
    }
}

/// A limited run's outcome (Rust's `bare::LimitedRun`).
#[pyclass(name = "LimitedRun", module = "mc683xx", frozen, eq, get_all)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LimitedRunView {
    /// The cycles it took.
    cycles: u64,
    /// Why it ended.
    end: EndView,
}

#[pymethods]
impl LimitedRunView {
    fn __repr__(&self) -> String {
        format!("LimitedRun(cycles={}, end={:?})", self.cycles, self.end)
    }
}

/// Deviations from the CPU32, for parity runs against an emulator that
/// has them; never the CPU's behavior (Rust's `bare::Deviations`).
#[pyclass(
    name = "Deviations",
    module = "mc683xx",
    frozen,
    eq,
    hash,
    get_all,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DeviationsView {
    /// A word or long at an odd data address is run as byte cycles, not
    /// an address error.  Fetches stay strict.
    misaligned: bool,
    /// T1 and T0 trace nothing.
    untraced: bool,
}

#[pymethods]
impl DeviationsView {
    /// None by default: the CPU32's behavior.
    #[new]
    #[pyo3(signature = (*, misaligned = false, untraced = false))]
    const fn new(misaligned: bool, untraced: bool) -> Self {
        Self {
            misaligned,
            untraced,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Deviations(misaligned={}, untraced={})",
            python_bool(self.misaligned),
            python_bool(self.untraced)
        )
    }
}

/// `value` as Python prints it.
const fn python_bool(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

impl From<Deviations> for DeviationsView {
    fn from(deviations: Deviations) -> Self {
        Self {
            misaligned: deviations.misaligned,
            untraced: deviations.untraced,
        }
    }
}

impl From<DeviationsView> for Deviations {
    fn from(view: DeviationsView) -> Self {
        let mut deviations = Self::default();
        deviations.misaligned = view.misaligned;
        deviations.untraced = view.untraced;
        deviations
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
    /// The callable that answers intercepted exceptions.
    intercept: Option<Held>,
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
        install: impl FnOnce(&mut cpu::Cpu, Box<dyn FnMut(&T) -> Control>) -> HookId,
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
    /// `until`.
    until: Option<Address>,
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
            Some(cycles) if self.spent >= cycles => return Err(End::Stop(cpu::Stop::Budget)),
            Some(cycles) => Some(Cycles(cycles - self.spent)),
            None => None,
        };
        let mut limits = Limits::default();
        limits.cycles = cycles;
        limits.instructions = instructions;
        limits.until = self.until;
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
            intercept: None,
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
        let permissions = permissions::parse(permissions)?;
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
        let permissions = permissions::parse(permissions)?;
        Ok(self.machine.protect(Address(start), length, permissions)?)
    }

    /// `length` bytes of mapped memory at `address`, whatever its
    /// permissions, with no bus cycle.  Raises `BusError` if any is not
    /// mapped, and `ValueError` for more than 32 bits of addresses,
    /// before anything is copied.
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

    /// Writes `data` to mapped memory at `address`, as `read`; on an
    /// error nothing is written.
    fn write(&mut self, address: u32, data: PyBackedBytes) -> Result<(), Error> {
        Ok(self.machine.write(Address(address), &data)?)
    }

    /// At addresses `begin` to `end`, both included, which no other
    /// peripheral's overlap, `peripheral`, which has `read(cycle) -> int`
    /// and `write(cycle, value)`: it answers the cycles there that no
    /// mapped memory serves, before they land.  It saves as a chip's
    /// mapped peripheral does, through `save()` and `restore(bytes)`.
    fn add_peripheral(
        &mut self,
        begin: u32,
        end: u32,
        peripheral: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let peripheral = PythonMapped::mapped(peripheral, self.pending.clone())?;
        self.machine
            .add_peripheral(Address(begin)..=Address(end), Box::new(peripheral))
            .map_err(Error::from)?;
        Ok(())
    }

    /// Register `name`: `D0`-`D7`, `A0`-`A7`, `PC`, `SR`, `CCR`, `USP`,
    /// `SSP`, `VBR`, `SFC`, `DFC`.
    fn read_register(&self, name: &str) -> Result<u32, Error> {
        Ok(Register::parse(name)?.read(&self.machine.cpu.registers))
    }

    /// Sets register `name` to `value`, which must fit it.
    fn write_register(&mut self, name: &str, value: u32) -> Result<(), Error> {
        Register::parse(name)?.write(&mut self.machine.cpu.registers, value)
    }

    /// Every register by name.
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        hooks::register_dictionary(py, &self.machine.cpu.registers)
    }

    /// The reset exception (CPU32RM 6.2.1): SSP and PC from the long
    /// words at zero.  A fault reading them halts the CPU (`halt`).  A
    /// peripheral's exception in the reads is raised here.
    fn reset(&mut self) -> PyResult<()> {
        // A halt is what `halt` reports.
        let _ = self.machine.reset();
        self.pending.check()
    }

    /// Requests an interrupt on IRQ`pin`, 1 to 7, answered by `vector`
    /// at its acknowledge, or the autovector if `None`; it stays
    /// requested until `clear_interrupt`.
    #[pyo3(signature = (pin, vector = None))]
    fn request_interrupt(&mut self, pin: u8, vector: Option<u8>) -> Result<(), Error> {
        self.machine
            .request_interrupt(IrqPin::try_from(pin).map_err(Error::from)?, vector);
        Ok(())
    }

    /// Withdraws the request on IRQ`pin`.
    fn clear_interrupt(&mut self, pin: u8) -> Result<(), Error> {
        self.machine
            .clear_interrupt(IrqPin::try_from(pin).map_err(Error::from)?);
        Ok(())
    }

    /// Runs until PC reaches `until` (before the hooks there), it has
    /// run `instructions`, its cycle estimate reaches `cycles`, a hook
    /// or the stop request stops it, a fault does
    /// (`set_stop_on_fault`), or the CPU
    /// stops or halts.  `begin` sets PC first and forgets a hook stop's
    /// skip, so the hooks at `begin` are called.  With no `cycles`, a
    /// `stop` instruction ends the run at once.
    #[pyo3(signature = (*, begin = None, until = None, instructions = None, cycles = None))]
    fn run_limited(
        slf: &Bound<'_, Self>,
        begin: Option<u32>,
        until: Option<u32>,
        instructions: Option<u64>,
        cycles: Option<u64>,
    ) -> PyResult<LimitedRunView> {
        let first = {
            let mut this = running(slf)?;
            if let Some(begin) = begin {
                this.machine.cpu.registers.pc = begin;
                this.machine.cpu.clear_resume();
            }
            this.machine.stop_request().withdraw();
            this.machine.cpu.instructions()
        };
        let budget = Budget {
            until: until.map(Address),
            instructions: instructions.map(|count| (count, first)),
            cycles,
            spent: 0,
        };
        let run = Self::run_dispatching(slf, budget, &|| false);
        slf.borrow_mut().running = false;
        let (cycles, end) = run?;
        Ok(LimitedRunView {
            cycles,
            end: end.into(),
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
    /// until it detaches (`Ending.DETACHED`) or kills it (`Ending.KILLED`)
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

    /// Whether the exceptions an instruction raises with `vector` are
    /// intercepted: before anything is stacked, PC at the instruction,
    /// the run calls the intercept callback (`set_intercept_callback`),
    /// or takes them natively if there is none.  None are at first.
    fn set_intercepted(&mut self, vector: u8, intercepted: bool) {
        self.machine
            .cpu
            .set_intercepted(Vector(vector), intercepted);
    }

    /// Calls `callback(Intercepted)` for each intercepted exception
    /// (`set_intercepted`).  A truthy result means it handled it: the
    /// CPU takes nothing and runs on from PC as the callable left it.
    /// Otherwise the CPU takes it natively.  `None` takes every one
    /// natively.
    fn set_intercept_callback(&mut self, callback: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        if let Some(callback) = callback
            && !callback.is_callable()
        {
            return Err(PyTypeError::new_err(
                "an intercept callback must be callable",
            ));
        }
        self.intercept = callback.map(|callback| self.pending.keep(callback));
        Ok(())
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

    /// Sets the deviations from the CPU32 its runs take, for parity
    /// runs; none at first.
    fn set_deviations(&mut self, deviations: DeviationsView) {
        self.machine.cpu.set_deviations(deviations.into());
    }

    /// The deviations its runs take (`set_deviations`).
    #[getter]
    fn deviations(&self) -> DeviationsView {
        self.machine.cpu.deviations().into()
    }

    /// Instructions run since the CPU was made.
    #[getter]
    const fn instructions(&self) -> u64 {
        self.machine.cpu.instructions()
    }

    /// The CPU's clock: the cycles its runs took.
    #[getter]
    const fn now(&self) -> u64 {
        self.machine.now().0
    }

    /// Why the CPU halted, while it stays halted.
    #[getter]
    fn halted(&self) -> Option<Halt> {
        self.machine.cpu.halted().map(Halt::from)
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

    /// Removes `hook`; whether it was installed.
    fn remove_hook(&mut self, hook: &HookIdView) -> bool {
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
            budget.spent = budget.spent.saturating_add(run.cycles.0);
            let pending = slf.borrow().pending.clone();
            pending.check()?;
            let dispatched = match registers {
                Some(registers) => Self::dispatch(slf, py, fired, &registers)?,
                None => Dispatched::Continue,
            };
            let stop = slf.borrow().machine.stop_request().take();
            let requested = End::Stop(cpu::Stop::Requested);
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
                (End::Stop(cpu::Stop::Hook(_)), Dispatched::Continue) if !stop => {}
                // A quantum's end, or the run's, which `remaining` finds.
                (End::Instructions, Dispatched::Continue) if !stop => py.check_signals()?,
                (_, Dispatched::Stop(hook)) => {
                    return Ok((budget.spent, End::Stop(cpu::Stop::Hook(hook))));
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

    /// Asks `intercept`'s callable to answer `interception`; unless it
    /// handled it, the instruction takes it natively when it reruns.
    fn answer(slf: &Bound<'_, Self>, py: Python<'_>, interception: Intercepted) -> PyResult<()> {
        let callable = slf.borrow().intercept.clone();
        let handled = match callable {
            Some(callable) => callable
                .call1(py, (InterceptedView(interception),))?
                .bind(py)
                .is_truthy()?,
            None => false,
        };
        if !handled {
            let mut this = slf.borrow_mut();
            if this.machine.cpu.registers.pc == interception.pc.0 {
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

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> R {
        call(&mut CpuControl::new(&mut self.0.borrow_mut().machine.cpu))
    }

    fn peek(&mut self, address: Address, data: &mut [u8]) -> usize {
        Target::peek(&mut self.0.borrow_mut().machine, address, data)
    }

    fn poke(&mut self, address: Address, data: &[u8]) -> Result<(), Unwritable> {
        Target::poke(&mut self.0.borrow_mut().machine, address, data)
    }

    fn run_for(&mut self, cycles: Cycles, hit: &dyn Fn() -> bool) -> PyResult<TargetRun> {
        let budget = Budget {
            until: None,
            instructions: None,
            cycles: Some(cycles.0),
            spent: 0,
        };
        let (_, end) = MachineView::run_dispatching(self.0, budget, hit)?;
        self.0.py().check_signals()?;
        if hit() {
            return Ok(TargetRun::Hook);
        }
        Ok(match end {
            End::Stop(stop) => stop.into(),
            End::Until | End::Instructions => TargetRun::Quantum,
            End::Fault(_) | End::Intercepted(_) => TargetRun::Fault,
            _ => TargetRun::Requested,
        })
    }

    fn reset(&mut self) -> bool {
        Target::reset(&mut self.0.borrow_mut().machine)
    }
}
