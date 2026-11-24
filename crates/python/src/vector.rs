//! Exception vectors as Python takes them.

use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::exception::VectorSet;
use pyo3::Bound;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::types::PyAnyMethods;

/// The vectors an iterable of vector numbers names, every vector for
/// `None`.
pub(crate) fn vector_set(vectors: Option<&Bound<'_, PyAny>>) -> PyResult<VectorSet> {
    let Some(vectors) = vectors else {
        return Ok(VectorSet::ALL);
    };
    vectors
        .try_iter()?
        .map(|vector| vector?.extract::<u8>().map(Vector))
        .collect()
}
