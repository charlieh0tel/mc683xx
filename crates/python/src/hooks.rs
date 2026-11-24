//! Python callables as the core's hooks (PLAN.md, Hooks and Python):
//! each is called with what the Rust hook sees, and `True` stops the
//! run (`Stop.Hook(hook)`), `None` or `False` does not, and any other
//! result raises `TypeError`.  An exception it raises cannot cross the
//! run: it stops the run there, as `True` does, and waits in `Pending`
//! for the call into the chip to raise it when it returns.

use std::cell::Cell;
use std::ops::ControlFlow;

use mc683xx_cpu32::exception;
use mc683xx_cpu32::hooks::Control;
use mc683xx_cpu32::hooks::ExceptionEntry;
use mc683xx_cpu32::hooks::ExceptionReturn;
use mc683xx_cpu32::hooks::HookId;
use mc683xx_cpu32::hooks::Instruction;
use mc683xx_cpu32::hooks::Invalid;
use mc683xx_cpu32::hooks::InvalidAccess;
use mc683xx_cpu32::hooks::MemoryAccess;
use mc683xx_cpu32::hooks::PendingRead;
use mc683xx_cpu32::memory::Memory;
use mc683xx_cpu32::registers::Registers;
use mc683xx_target::Address;
use pyo3::Bound;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::call::PyCallArgs;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyTypeError;
use pyo3::exceptions::PyValueError;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyBool;
use pyo3::types::PyByteArray;
use pyo3::types::PyByteArrayMethods;
use pyo3::types::PyBytes;
use pyo3::types::PyDict;
use pyo3::types::PyDictMethods;
use pyo3::types::PyTypeMethods;

use crate::error::Error;
use crate::function_code::FunctionCode;
use crate::held::Held;
use crate::mapped::Pending;
use crate::python::opaque_repr;
use crate::python::repr;
use crate::register::Register;

scoped_tls::scoped_thread_local! {
    /// The core's memory, lent to the pre-read hook call under way.
    static LENT: Memory
}

thread_local! {
    /// The number of the last pre-read hook call on this thread, from 1:
    /// a `PendingRead` peeks only within the call that made it.
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

/// A hook, as `add_*_hook` returns it and `Stop.Hook` carries it (Rust's
/// `hooks::HookId`).
#[pyclass(name = "HookId", module = "mc683xx", frozen, eq, hash, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HookIdView(pub(crate) HookId);

#[pymethods]
impl HookIdView {
    /// `HookId(3)`, from Rust's `Display`, `hook 3`.
    fn __repr__(&self) -> String {
        opaque_repr("HookId", &self.0)
    }

    /// Rust's `Display`: `hook 3`.
    fn __str__(&self) -> String {
        self.0.to_string()
    }
}

/// Every register in `registers` by name.
pub(crate) fn register_dictionary<'py>(
    py: Python<'py>,
    registers: &Registers,
) -> PyResult<Bound<'py, PyDict>> {
    let dictionary = PyDict::new(py);
    for register in Register::all() {
        dictionary.set_item(register.to_string(), register.read(registers))?;
    }
    Ok(dictionary)
}

/// An instruction about to execute, as a code or block hook sees it.
#[pyclass(name = "Instruction", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct InstructionView(Instruction);

#[pymethods]
impl InstructionView {
    /// Its address.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.pc.0
    }

    /// Every register by name, as `Mc68331.registers()` gives them.
    #[getter]
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        register_dictionary(py, &self.0.registers)
    }

    /// The long word at A7, in data space, if memory serves it; `None`
    /// if a peripheral would.
    #[getter]
    fn stack_top(&self) -> Option<u32> {
        self.0.stack_top
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "Instruction(pc={:#08x}, stack_top={})",
            self.0.pc.0,
            repr(py, self.stack_top())?
        ))
    }
}

/// A read or write cycle that ended well, as a memory hook sees it.
#[pyclass(name = "MemoryAccess", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct MemoryAccessView(MemoryAccess, Option<Registers>);

#[pymethods]
impl MemoryAccessView {
    /// Its address, on the chip's address lines.
    #[getter]
    const fn address(&self) -> u32 {
        self.0.address.0
    }

    /// Bytes in it: 1 or 2, or 4 for a long operand on a bare `Machine`.
    #[getter]
    const fn size(&self) -> u32 {
        self.0.size.bytes()
    }

    /// Its function code.
    #[getter]
    fn function_code(&self) -> FunctionCode {
        self.0.function_code.into()
    }

    /// The value read or written, right-aligned.
    #[getter]
    const fn value(&self) -> u32 {
        self.0.value
    }

    /// For a write, the value it replaced, read with no cycle; `None`
    /// for a read, a write in CPU space, and a write to a peripheral.
    #[getter]
    const fn previous(&self) -> Option<u32> {
        self.0.previous
    }

    /// The address of the instruction it is part of.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.pc.0
    }

    /// Whether exception processing is stacking a frame.
    #[getter]
    const fn stacking(&self) -> bool {
        self.0.stacking
    }

    /// The registers as the instruction began, or, while a trace,
    /// hardware breakpoint or interrupt taken after one stacks its
    /// frame, as that processing began; by name, on the bare `Machine`
    /// only, `None` on the chip.
    #[getter]
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.1
            .as_ref()
            .map(|registers| register_dictionary(py, registers))
            .transpose()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "MemoryAccess(address={:#08x}, size={}, function_code={}, value={:#x})",
            self.address(),
            self.size(),
            repr(py, self.function_code())?,
            self.value()
        ))
    }
}

/// An operand read the core's memory is about to serve, as a pre-read
/// hook sees it.
#[pyclass(name = "PendingRead", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct PendingReadView {
    /// The read.
    read: PendingRead,
    /// The number of the hook call it was made for (`CALLS`).
    call: u64,
}

#[pymethods]
impl PendingReadView {
    /// `length` bytes of the core's memory from `address`, as a
    /// debugger reads them (supervisor space, no cycle, no peripheral),
    /// while its hook's call is under way: `BusError` at a byte the
    /// core's memory does not serve, `RuntimeError` once the call is
    /// over.
    fn peek<'py>(
        &self,
        py: Python<'py>,
        address: u32,
        length: usize,
    ) -> PyResult<Bound<'py, PyBytes>> {
        if !LENT.is_set() || CALLS.get() != self.call {
            return Err(PyRuntimeError::new_err(
                "a PendingRead peeks only within its hook's call",
            ));
        }
        let mut data = vec![0; length];
        LENT.with(|memory| memory.read_bytes(Address(address), &mut data))
            .map_err(Error::from)?;
        Ok(PyBytes::new(py, &data))
    }

    /// Its address, on the chip's address lines.
    #[getter]
    const fn address(&self) -> u32 {
        self.read.address.0
    }

    /// Bytes in it: 1, 2 or 4, the operand's.
    #[getter]
    const fn size(&self) -> u32 {
        self.read.size.bytes()
    }

    /// Its function code.
    #[getter]
    fn function_code(&self) -> FunctionCode {
        self.read.function_code.into()
    }

    /// The address of the instruction it is part of.
    #[getter]
    const fn pc(&self) -> u32 {
        self.read.pc.0
    }

    /// Whether exception processing is stacking a frame.
    #[getter]
    const fn stacking(&self) -> bool {
        self.read.stacking
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "PendingRead(address={:#08x}, size={}, function_code={})",
            self.address(),
            self.size(),
            repr(py, self.function_code())?
        ))
    }
}

/// A bus cycle's kind (CPU32RM 6.3).
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BusCycleKind {
    /// An instruction fetch.
    Fetch,
    /// An operand read.
    Read,
    /// An operand write.
    Write,
}

impl From<BusCycleKind> for exception::BusCycleKind {
    fn from(cycle: BusCycleKind) -> Self {
        match cycle {
            BusCycleKind::Fetch => Self::Fetch,
            BusCycleKind::Read => Self::Read,
            BusCycleKind::Write => Self::Write,
        }
    }
}

enum_names!(BusCycleKind, exception::BusCycleKind);

impl From<exception::BusCycleKind> for BusCycleKind {
    fn from(cycle: exception::BusCycleKind) -> Self {
        match cycle {
            exception::BusCycleKind::Fetch => Self::Fetch,
            exception::BusCycleKind::Read => Self::Read,
            exception::BusCycleKind::Write => Self::Write,
        }
    }
}

/// Why a bus cycle was invalid.
#[pyclass(
    name = "Invalid",
    module = "mc683xx",
    eq,
    hash,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum InvalidReason {
    /// Nothing answered, or a peripheral ended it in a bus error.
    BusError,
    /// Its page's permissions do not allow it (`Mc68331.protect`).
    Protected,
}

enum_names!(InvalidReason);

impl TryFrom<Invalid> for InvalidReason {
    type Error = Error;

    /// The reason, or [`Error::Unmapped`] for one the bindings do not
    /// know.
    fn try_from(invalid: Invalid) -> Result<Self, Error> {
        Ok(match invalid {
            Invalid::BusError => Self::BusError,
            Invalid::Protected => Self::Protected,
            _ => return Err(Error::unmapped("invalid access")),
        })
    }
}

/// A bus cycle that ended in a bus error, as an invalid access hook
/// sees it.
#[pyclass(name = "InvalidAccess", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct InvalidAccessView(InvalidAccess, Option<Registers>);

#[pymethods]
impl InvalidAccessView {
    /// Its address, on the chip's address lines.
    #[getter]
    const fn address(&self) -> u32 {
        self.0.address.0
    }

    /// Bytes in it: 1 or 2, or 4 for a long a bare `Machine`'s peripheral
    /// takes whole.
    #[getter]
    const fn size(&self) -> u32 {
        self.0.size.bytes()
    }

    /// Its function code.
    #[getter]
    fn function_code(&self) -> FunctionCode {
        self.0.function_code.into()
    }

    /// Fetch, read or write.
    #[getter]
    fn kind(&self) -> BusCycleKind {
        self.0.kind.into()
    }

    /// A write's value, right-aligned.
    #[getter]
    const fn value(&self) -> Option<u32> {
        self.0.value
    }

    /// Why it was invalid.
    #[getter]
    fn reason(&self) -> PyResult<InvalidReason> {
        Ok(self.0.reason.try_into()?)
    }

    /// The address of the instruction it is part of.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.pc.0
    }

    /// Whether exception processing is stacking a frame.
    #[getter]
    const fn stacking(&self) -> bool {
        self.0.stacking
    }

    /// The registers as the instruction began, or, while a trace,
    /// hardware breakpoint or interrupt taken after one stacks its
    /// frame, as that processing began; by name, on the bare `Machine`
    /// only, `None` on the chip.
    #[getter]
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.1
            .as_ref()
            .map(|registers| register_dictionary(py, registers))
            .transpose()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "InvalidAccess(address={:#08x}, size={}, function_code={}, kind={}, value={}, \
             reason={})",
            self.address(),
            self.size(),
            repr(py, self.function_code())?,
            repr(py, self.kind())?,
            repr(py, self.value())?,
            repr(py, self.reason()?)?
        ))
    }
}

/// An exception entered, as an exception hook sees it.
#[pyclass(name = "ExceptionEntry", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct ExceptionView(ExceptionEntry);

#[pymethods]
impl ExceptionView {
    /// Its vector number.
    #[getter]
    const fn vector(&self) -> u8 {
        self.0.vector.0
    }

    /// The return PC its frame stacked.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.pc.0
    }

    /// Its handler's address.
    #[getter]
    const fn handler(&self) -> u32 {
        self.0.handler.0
    }

    /// SSP: its frame's lowest address.
    #[getter]
    const fn stack(&self) -> u32 {
        self.0.stack.0
    }

    /// The interrupt level, for an interrupt.
    #[getter]
    fn level(&self) -> Option<u8> {
        self.0.level.map(u8::from)
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "ExceptionEntry(vector={}, pc={:#08x}, handler={:#08x}, stack={:#08x}, level={})",
            self.vector(),
            self.pc(),
            self.handler(),
            self.stack(),
            repr(py, self.level())?
        ))
    }
}

/// An exception returned from, as an exception return hook sees it.
#[pyclass(name = "ExceptionReturn", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct ExceptionReturnView(ExceptionReturn);

#[pymethods]
impl ExceptionReturnView {
    /// The vector number its frame names.
    #[getter]
    const fn vector(&self) -> u8 {
        self.0.vector.0
    }

    /// The PC it returns to.
    #[getter]
    const fn pc(&self) -> u32 {
        self.0.pc.0
    }

    /// The SR it restored.
    #[getter]
    const fn sr(&self) -> u16 {
        self.0.sr
    }

    /// Its frame's lowest address: the `ExceptionEntry`'s `stack`.
    #[getter]
    const fn stack(&self) -> u32 {
        self.0.stack.0
    }

    fn __repr__(&self) -> String {
        format!(
            "ExceptionReturn(vector={}, pc={:#08x}, sr={:#06x}, stack={:#08x})",
            self.vector(),
            self.pc(),
            self.sr(),
            self.stack()
        )
    }
}

/// What a hook's `result` says: whether it stops the run, or, for an
/// intercept hook, handled the exception.  `True` does, `None` and
/// `False` do not; anything else is a `TypeError`, so a hook that
/// returns what it computed by mistake (a count, a list) is caught.
pub(crate) fn is_true(result: &Bound<'_, PyAny>) -> PyResult<bool> {
    if result.is_none() {
        return Ok(false);
    }
    if result.is_instance_of::<PyBool>() {
        return result.extract::<bool>();
    }
    Err(PyTypeError::new_err(format!(
        "a hook returns True, False or None, not {}",
        result.get_type().name()?
    )))
}

/// `callable`, which must be callable, as a hook whose argument is
/// `view` of what the Rust hook sees.
pub(crate) fn hook<T, V>(
    callable: &Bound<'_, PyAny>,
    pending: Pending,
    view: impl Fn(&T) -> V + 'static,
) -> PyResult<impl FnMut(&T) -> Control + 'static>
where
    (V,): for<'py> PyCallArgs<'py>,
{
    if !callable.is_callable() {
        return Err(PyTypeError::new_err("a hook must be callable"));
    }
    let callable = pending.keep(callable);
    Ok(move |context: &T| {
        Python::attach(|py| {
            let answer = callable
                .call1(py, (view(context),))
                .and_then(|result| is_true(result.bind(py)));
            control(answer, &pending)
        })
    })
}

/// `callable`, which must be callable, as a pre-read hook, called before
/// the read as `callable(PendingRead, bytearray)`, the memory lent to
/// `PendingRead.peek` for the call: what it leaves in the bytearray,
/// which must keep its length, answers the read.  `True`, or an
/// exception, which waits in `pending`, stops the run.
pub(crate) fn pre_read_hook(
    callable: &Bound<'_, PyAny>,
    pending: Pending,
) -> PyResult<impl FnMut(&PendingRead, &Memory, &mut [u8]) -> Control + 'static> {
    if !callable.is_callable() {
        return Err(PyTypeError::new_err("a hook must be callable"));
    }
    let callable = pending.keep(callable);
    Ok(
        move |read: &PendingRead, memory: &Memory, bytes: &mut [u8]| {
            Python::attach(|py| {
                let answer = lend(py, &callable, memory, bytes, "pre-read", |call| {
                    PendingReadView { read: *read, call }
                });
                control(answer, &pending)
            })
        },
    )
}

/// Calls `callable(view, data)`, `view` made with the call's number
/// (`CALLS`), `data` a bytearray copy of `bytes`, with `memory` lent to
/// the views' `peek` for the call: whether its result stops the run.
/// What it leaves in `data`, which must keep its length, goes back into
/// `bytes`; `kind` names the hook in the error if it does not.
fn lend<V>(
    py: Python<'_>,
    callable: &Held,
    memory: &Memory,
    bytes: &mut [u8],
    kind: &str,
    view: impl FnOnce(u64) -> V,
) -> PyResult<bool>
where
    (V, Py<PyByteArray>): for<'py> PyCallArgs<'py>,
{
    let data = PyByteArray::new(py, bytes);
    let call = CALLS.get() + 1;
    CALLS.set(call);
    let stop = LENT
        .set(memory, || {
            callable.call1(py, (view(call), data.clone().unbind()))
        })
        .and_then(|result| is_true(result.bind(py)))?;
    let changed = data.to_vec();
    if changed.len() != bytes.len() {
        return Err(PyValueError::new_err(format!(
            "a {kind} hook must keep the bytes' length"
        )));
    }
    bytes.copy_from_slice(&changed);
    Ok(stop)
}

/// A hook's `answer` as the core takes it: `True` stops the run, and
/// so does an exception, which waits in `pending`.
fn control(answer: PyResult<bool>, pending: &Pending) -> Control {
    match answer {
        Ok(false) => ControlFlow::Continue(()),
        Ok(true) => ControlFlow::Break(()),
        Err(error) => {
            pending.hold(error);
            ControlFlow::Break(())
        }
    }
}

/// A code or block hook's argument.
pub(crate) fn instruction(instruction: &Instruction) -> InstructionView {
    InstructionView(instruction.clone())
}

/// A read or write hook's argument.
pub(crate) const fn memory_access(access: &MemoryAccess) -> MemoryAccessView {
    MemoryAccessView(*access, None)
}

/// An invalid access hook's argument.
pub(crate) const fn invalid_access(access: &InvalidAccess) -> InvalidAccessView {
    InvalidAccessView(*access, None)
}

/// A read or write hook's argument on the bare CPU, with the registers
/// as the instruction, or the exception processing after one, began.
pub(crate) const fn memory_access_with(
    access: MemoryAccess,
    registers: Registers,
) -> MemoryAccessView {
    MemoryAccessView(access, Some(registers))
}

/// An invalid access hook's argument on the bare CPU, as
/// `memory_access_with`.
pub(crate) const fn invalid_access_with(
    access: InvalidAccess,
    registers: Registers,
) -> InvalidAccessView {
    InvalidAccessView(access, Some(registers))
}

/// An exception hook's argument.
pub(crate) const fn exception_entry(entry: &ExceptionEntry) -> ExceptionView {
    ExceptionView(*entry)
}

/// An exception return hook's argument.
pub(crate) const fn exception_return(exception_return: &ExceptionReturn) -> ExceptionReturnView {
    ExceptionReturnView(*exception_return)
}
