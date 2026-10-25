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
    use pyo3::exceptions::PyValueError;

    pyo3::create_exception!(
        mc683xx,
        BusError,
        PyException,
        "Raised by a peripheral's read or write: BERR ends \
         the cycle."
    );

    pyo3::create_exception!(
        mc683xx,
        UnpeekableError,
        PyException,
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
}
