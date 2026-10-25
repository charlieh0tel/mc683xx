//! Python callables as the core's hooks (PLAN.md, Hooks and Python):
//! each is called with what the Rust hook sees, and a truthy result
//! stops the run (`Stop.Hook(hook)`).  An exception it raises cannot cross
//! the run: it stops the run there, as a truthy result does, and waits
//! in `Pending` for the call into the chip to raise it when it returns.

use std::ops::ControlFlow;

use mc683xx_cpu32::cpu::hooks::Control;
use mc683xx_cpu32::cpu::hooks::ExceptionEntry;
use mc683xx_cpu32::cpu::hooks::HookId;
use mc683xx_cpu32::cpu::hooks::Instruction;
use mc683xx_cpu32::cpu::hooks::Invalid;
use mc683xx_cpu32::cpu::hooks::InvalidAccess;
use mc683xx_cpu32::cpu::hooks::MemoryAccess;
use mc683xx_cpu32::exception;
use mc683xx_cpu32::registers::Registers;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::call::PyCallArgs;
use pyo3::exceptions::PyTypeError;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyDict;
use pyo3::types::PyDictMethods;

use crate::mapped::Pending;
use crate::register::Register;

/// A hook, as `add_*_hook` returns it and `Stop.Hook` carries it (Rust's
/// `hooks::HookId`).
#[pyclass(name = "HookId", module = "mc683xx", frozen, eq, hash, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HookIdView(pub(crate) HookId);

#[pymethods]
impl HookIdView {
    fn __repr__(&self) -> String {
        format!("{:?}", self.0)
    }
}

/// Every register in `registers` by name.
pub(crate) fn register_dictionary<'py>(
    py: Python<'py>,
    registers: &Registers,
) -> PyResult<Bound<'py, PyDict>> {
    let dictionary = PyDict::new(py);
    for register in Register::all() {
        dictionary.set_item(register.name(), register.read(registers))?;
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
    const fn stack_top(&self) -> Option<u32> {
        self.0.stack_top
    }

    fn __repr__(&self) -> String {
        format!(
            "Instruction(pc={:#08x}, stack_top={:?})",
            self.0.pc.0, self.0.stack_top
        )
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

    /// FC[2:0].
    #[getter]
    const fn function_code(&self) -> u8 {
        self.0.function_code.bits()
    }

    /// The value read or written, right-aligned.
    #[getter]
    const fn value(&self) -> u32 {
        self.0.value
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

    /// The registers as the instruction began, by name; on the bare
    /// `Machine` only, `None` on the chip.
    #[getter]
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.1
            .as_ref()
            .map(|registers| register_dictionary(py, registers))
            .transpose()
    }

    fn __repr__(&self) -> String {
        format!(
            "MemoryAccess(address={:#08x}, size={}, function_code={}, value={:#x})",
            self.address(),
            self.size(),
            self.function_code(),
            self.value()
        )
    }
}

/// A bus cycle's kind (CPU32RM 6.3).
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CycleKind {
    /// An instruction fetch.
    Fetch,
    /// An operand read.
    Read,
    /// An operand write.
    Write,
}

impl From<exception::CycleKind> for CycleKind {
    fn from(cycle: exception::CycleKind) -> Self {
        match cycle {
            exception::CycleKind::Fetch => Self::Fetch,
            exception::CycleKind::Read => Self::Read,
            exception::CycleKind::Write => Self::Write,
        }
    }
}

/// Why a bus cycle was invalid.
#[pyclass(
    name = "Invalid",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
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

impl From<Invalid> for InvalidReason {
    fn from(invalid: Invalid) -> Self {
        match invalid {
            Invalid::BusError => Self::BusError,
            Invalid::Protected => Self::Protected,
            _ => panic!("an invalid access the bindings do not know"),
        }
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

    /// FC[2:0].
    #[getter]
    const fn function_code(&self) -> u8 {
        self.0.function_code.bits()
    }

    /// Fetch, read or write.
    #[getter]
    fn cycle(&self) -> CycleKind {
        self.0.cycle.into()
    }

    /// A write's value, right-aligned.
    #[getter]
    const fn value(&self) -> Option<u32> {
        self.0.value
    }

    /// Why it was invalid.
    #[getter]
    fn reason(&self) -> InvalidReason {
        self.0.reason.into()
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

    /// The registers as the instruction began, by name; on the bare
    /// `Machine` only, `None` on the chip.
    #[getter]
    fn registers<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        self.1
            .as_ref()
            .map(|registers| register_dictionary(py, registers))
            .transpose()
    }

    fn __repr__(&self) -> String {
        format!(
            "InvalidAccess(address={:#08x}, size={}, function_code={}, cycle={:?}, value={:?}, \
             reason={:?})",
            self.address(),
            self.size(),
            self.function_code(),
            self.cycle(),
            self.value(),
            self.reason()
        )
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
        self.0.stack
    }

    /// The interrupt level, for an interrupt.
    #[getter]
    fn level(&self) -> Option<u8> {
        self.0.level.map(|level| level.get())
    }

    fn __repr__(&self) -> String {
        format!(
            "ExceptionEntry(vector={}, pc={:#08x}, handler={:#08x}, stack={:#08x}, level={:?})",
            self.vector(),
            self.pc(),
            self.handler(),
            self.stack(),
            self.level()
        )
    }
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
            match callable
                .call1(py, (view(context),))
                .and_then(|result| result.bind(py).is_truthy())
            {
                Ok(false) => ControlFlow::Continue(()),
                Ok(true) => ControlFlow::Break(()),
                Err(error) => {
                    pending.hold(error);
                    ControlFlow::Break(())
                }
            }
        })
    })
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
/// as the instruction began.
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
