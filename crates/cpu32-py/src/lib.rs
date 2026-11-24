//! Python bindings for the MC68331: the Python module `mc683xx`
//! (PLAN.md, Python).  The host builds the chip with `Builder`, wiring
//! memory and Python peripheral objects to chip-select pins, and runs it
//! as `Mc68331`.
//!
//! The bindings build only with the `python` feature, which maturin
//! enables; without it the crate is its Python-free peripherals, so the
//! workspace's tests need no Python.

#![cfg_attr(
    not(feature = "python"),
    expect(dead_code, reason = "the bindings use these with `python`")
)]

/// An enum class's `name` and `__str__` (python.md, Names and values):
/// `name` the member's (`FETCH`), `__str__` Rust's `Display` of the
/// member as `$rust` where the Rust type has one (`fetch`), else the
/// name.
#[cfg(feature = "python")]
macro_rules! enum_names {
    ($class:ty) => {
        #[pyo3::pymethods]
        impl $class {
            /// The member's name.
            #[getter]
            fn name(slf: &pyo3::Bound<'_, Self>) -> pyo3::PyResult<String> {
                crate::python::member_name(slf.as_any())
            }

            /// No `int`: a member is not a number (python.md, Names and values).
            fn __int__(&self) -> pyo3::PyResult<i64> {
                Err(crate::python::not_a_number())
            }

            /// The member's name.
            fn __str__(slf: &pyo3::Bound<'_, Self>) -> pyo3::PyResult<String> {
                crate::python::member_name(slf.as_any())
            }
        }
    };
    ($class:ty, $rust:ty) => {
        #[pyo3::pymethods]
        impl $class {
            /// The member's name.
            #[getter]
            fn name(slf: &pyo3::Bound<'_, Self>) -> pyo3::PyResult<String> {
                crate::python::member_name(slf.as_any())
            }

            /// No `int`: a member is not a number (python.md, Names and values).
            fn __int__(&self) -> pyo3::PyResult<i64> {
                Err(crate::python::not_a_number())
            }

            /// Rust's `Display`.
            fn __str__(&self) -> String {
                <$rust>::from(*self).to_string()
            }
        }
    };
}

#[cfg(feature = "python")]
mod bare;
#[cfg(feature = "python")]
mod board;
#[cfg(feature = "python")]
mod chip;
#[cfg(feature = "python")]
mod control;
mod error;
#[cfg(feature = "python")]
mod function_code;
#[cfg(feature = "python")]
mod gdb;
#[cfg(feature = "python")]
mod held;
#[cfg(feature = "python")]
mod hooks;
#[cfg(feature = "python")]
mod mapped;
#[cfg(feature = "python")]
mod peripherals;
mod permissions;
#[cfg(feature = "python")]
mod python;
#[cfg(feature = "python")]
mod qspi;
mod register;
#[cfg(feature = "python")]
mod requests;
mod select;

#[cfg(all(test, feature = "python"))]
mod tests;

/// The module's own exceptions.
#[cfg(feature = "python")]
mod exceptions {
    use pyo3::exceptions::PyException;
    use pyo3::exceptions::PyRuntimeError;
    use pyo3::exceptions::PyValueError;

    pyo3::create_exception!(
        mc683xx,
        BusCycleError,
        PyException,
        "Raised by a host's bus cycle or peek that gave no value (Rust's \
         BusCycleError): BusError or UnpeekableError."
    );

    pyo3::create_exception!(
        mc683xx,
        BusError,
        BusCycleError,
        "Raised by a peripheral's read or write: BERR ends \
         the cycle."
    );

    pyo3::create_exception!(
        mc683xx,
        UnpeekableError,
        BusCycleError,
        "Raised by peek where only a real bus cycle can tell: a peripheral, \
         or a module register a read would change."
    );

    pyo3::create_exception!(
        mc683xx,
        StateError,
        PyValueError,
        "Raised by restore for a snapshot of another format or other \
         hardware, bytes that do not decode, or values no run could save."
    );

    pyo3::create_exception!(
        mc683xx,
        LoadError,
        PyValueError,
        "Raised by Board for a board file it refuses, naming what in the \
         file is wrong; a file it cannot read raises OSError instead."
    );

    pyo3::create_exception!(
        mc683xx,
        RunError,
        PyRuntimeError,
        "Raised by a run that could not go on: a board peripheral that \
         stalls board time, or a run's end the bindings do not know; and \
         by take_exception for an exception the CPU could not take."
    );

    pyo3::create_exception!(
        mc683xx,
        TargetError,
        PyRuntimeError,
        "Raised by the chip's background debug controls: halt with \
         background mode not enabled at the last reset, or go or step \
         with the CPU not in background mode."
    );
}
