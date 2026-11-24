//! Exception vectors as Python takes them: `Vector`'s numbers, Rust's
//! `exception::Vector` constants, and `VectorSet`, Rust's
//! `exception::VectorSet`.

use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::exception::VectorSet;
use mc683xx_cpu32::interrupt::InterruptLevel;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::exceptions::PyValueError;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyIterator;
use pyo3::types::PyList;

use crate::error::Error;

/// TRAP's vectors, #0 to #15 (CPU32RM 4, TRAP).
const TRAP_VECTORS: u8 = 16;

/// Rust's `exception::Vector` constants and constructors, as vector
/// numbers (CPU32RM Table 6-1): a vector crosses as its number, so
/// `Vector` names numbers and is no type a value has.
#[pyclass(name = "Vector", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct VectorNumbers;

#[pymethods]
impl VectorNumbers {
    /// Reset: the initial supervisor stack pointer.
    #[classattr]
    const RESET_STACK_POINTER: u8 = Vector::RESET_STACK_POINTER.0;
    /// Reset: the initial program counter.
    #[classattr]
    const RESET_PROGRAM_COUNTER: u8 = Vector::RESET_PROGRAM_COUNTER.0;
    /// Bus error.
    #[classattr]
    const BUS_ERROR: u8 = Vector::BUS_ERROR.0;
    /// Address error.
    #[classattr]
    const ADDRESS_ERROR: u8 = Vector::ADDRESS_ERROR.0;
    /// Illegal instruction.
    #[classattr]
    const ILLEGAL_INSTRUCTION: u8 = Vector::ILLEGAL_INSTRUCTION.0;
    /// Zero divide.
    #[classattr]
    const ZERO_DIVIDE: u8 = Vector::ZERO_DIVIDE.0;
    /// CHK and CHK2.
    #[classattr]
    const CHK: u8 = Vector::CHK.0;
    /// TRAPcc and TRAPV.
    #[classattr]
    const TRAPCC: u8 = Vector::TRAPCC.0;
    /// Privilege violation.
    #[classattr]
    const PRIVILEGE_VIOLATION: u8 = Vector::PRIVILEGE_VIOLATION.0;
    /// Trace.
    #[classattr]
    const TRACE: u8 = Vector::TRACE.0;
    /// Line 1010 emulator.
    #[classattr]
    const LINE_A: u8 = Vector::LINE_A.0;
    /// Line 1111 emulator.
    #[classattr]
    const LINE_F: u8 = Vector::LINE_F.0;
    /// Hardware breakpoint.
    #[classattr]
    const HARDWARE_BREAKPOINT: u8 = Vector::HARDWARE_BREAKPOINT.0;
    /// Format error.
    #[classattr]
    const FORMAT_ERROR: u8 = Vector::FORMAT_ERROR.0;
    /// Uninitialized interrupt.
    #[classattr]
    const UNINITIALIZED_INTERRUPT: u8 = Vector::UNINITIALIZED_INTERRUPT.0;
    /// Spurious interrupt; the autovectors for levels 1 to 7 follow it.
    #[classattr]
    const SPURIOUS_INTERRUPT: u8 = Vector::SPURIOUS_INTERRUPT.0;
    /// TRAP #0; #1 to #15 follow it.
    #[classattr]
    const TRAP: u8 = Vector::TRAP.0;

    /// TRAP #`number`'s vector, 32 to 47 for `number` 0 to 15;
    /// `ValueError` past 15.
    #[staticmethod]
    fn trap(number: u8) -> PyResult<u8> {
        if number >= TRAP_VECTORS {
            return Err(PyValueError::new_err(format!(
                "no TRAP #{number}: give 0 to 15"
            )));
        }
        Ok(Vector::trap(number).0)
    }

    /// The autovector of interrupt level `level`, 0 to 7: 24 to 31.
    #[staticmethod]
    fn autovector(level: u8) -> PyResult<u8> {
        let level = InterruptLevel::try_from(level).map_err(Error::from)?;
        Ok(Vector::autovector(level).0)
    }
}

/// A set of exception vectors, any of the 256 (Rust's
/// `exception::VectorSet`): which exceptions stop a run or an intercept
/// hook covers.  `VectorSet(iterable)` of vector numbers, or `ALL`,
/// `NONE` or `INSTRUCTION`; iterating gives its numbers, ascending.
#[pyclass(
    name = "VectorSet",
    module = "mc683xx",
    frozen,
    eq,
    hash,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct VectorSetView(pub(crate) VectorSet);

#[pymethods]
impl VectorSetView {
    /// Every vector.
    #[classattr]
    const ALL: Self = Self(VectorSet::ALL);
    /// No vector.
    #[classattr]
    const NONE: Self = Self(VectorSet::NONE);
    /// The vectors of the exceptions an instruction raises, the ones an
    /// intercept hook can see (CPU32RM Table 6-2, groups 2 and 3).
    #[classattr]
    const INSTRUCTION: Self = Self(VectorSet::INSTRUCTION);

    /// The vectors `vectors`, an iterable of vector numbers, names; none
    /// if it is empty or left out.
    #[new]
    #[pyo3(signature = (vectors = None))]
    fn new(vectors: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        vectors.map_or(Ok(Self(VectorSet::NONE)), |vectors| {
            numbers(vectors).map(Self)
        })
    }

    /// Whether vector `vector` is in it; `False` for a number that is
    /// no vector.
    fn contains(&self, vector: i64) -> bool {
        u8::try_from(vector).is_ok_and(|vector| self.0.contains(Vector(vector)))
    }

    /// `vector in set`: `contains`, `False` for what is no number.
    fn __contains__(&self, vector: &Bound<'_, PyAny>) -> bool {
        vector
            .extract::<i64>()
            .is_ok_and(|vector| self.contains(vector))
    }

    /// Whether it holds no vector.
    const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// It but `vector`.
    const fn without(&self, vector: u8) -> Self {
        Self(self.0.without(Vector(vector)))
    }

    /// The vectors in it or in `other`.
    const fn union(&self, other: &Self) -> Self {
        Self(self.0.union(other.0))
    }

    /// The vectors in it but not in `other`, a `VectorSet` or an
    /// iterable of vector numbers.
    fn difference(&self, other: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self(self.0.difference(numbers(other)?)))
    }

    /// Its vector numbers, ascending (Rust's `iter`).
    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyIterator>> {
        PyList::new(py, self.0.iter().map(u8::from))?
            .as_any()
            .try_iter()
    }

    /// How many vectors it holds.
    fn __len__(&self) -> usize {
        self.0.iter().count()
    }

    /// `VectorSet.ALL`, `VectorSet.NONE`, `VectorSet.INSTRUCTION`, or
    /// `VectorSet([32, 39])`.
    fn __repr__(&self) -> String {
        let named = [
            (VectorSet::ALL, "ALL"),
            (VectorSet::NONE, "NONE"),
            (VectorSet::INSTRUCTION, "INSTRUCTION"),
        ];
        named.iter().find(|(set, _)| *set == self.0).map_or_else(
            || {
                let numbers: Vec<String> = self.0.iter().map(|vector| vector.to_string()).collect();
                format!("VectorSet([{}])", numbers.join(", "))
            },
            |(_, name)| format!("VectorSet.{name}"),
        )
    }
}

/// The vectors an iterable of vector numbers, or a `VectorSet`, names.
fn numbers(vectors: &Bound<'_, PyAny>) -> PyResult<VectorSet> {
    if let Ok(set) = vectors.extract::<VectorSetView>() {
        return Ok(set.0);
    }
    vectors
        .try_iter()?
        .map(|vector| vector?.extract::<u8>().map(Vector))
        .collect()
}

/// The vectors a `VectorSet` or an iterable of vector numbers names,
/// every vector for `None`.
pub(crate) fn vector_set(vectors: Option<&Bound<'_, PyAny>>) -> PyResult<VectorSet> {
    vectors.map_or(Ok(VectorSet::ALL), numbers)
}
