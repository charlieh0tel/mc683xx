//! Exception vectors as Python takes them: `VectorSet`, Rust's
//! `exception::VectorSet`.

use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::exception::VectorSet;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyIterator;
use pyo3::types::PyList;

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
