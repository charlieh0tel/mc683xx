//! A Python object as the QSPI's peripheral (QSMRM 4): its methods
//! answer the `SpiPeripheral` trait.  As for a bus peripheral, an exception it
//! raises waits in `Pending`; the transfer shifts in zeros.

use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_modules::peripheral;
use mc683xx_modules::peripheral::SpiPeripheral;
use pyo3::Bound;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::exceptions::PyTypeError;
use pyo3::exceptions::PyValueError;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;

use crate::held::Held;
use crate::mapped::PYTHON_PERIPHERAL;
use crate::mapped::Pending;
use crate::mapped::python_restore;
use crate::mapped::python_save;

/// SCK's level between transfers (QSMRM 4.3.1, CPOL).
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ClockPolarity {
    /// CPOL 0.
    IdleLow,
    /// CPOL 1.
    IdleHigh,
}

/// Which SCK edges change and capture the data (QSMRM 4.3.1, CPHA).
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ClockPhase {
    /// CPHA 0: captured on the leading edge.
    CaptureLeading,
    /// CPHA 1: changed on the leading edge.
    ChangeLeading,
}

/// The QSPI's serial clock, as a peripheral's `clock` is told it.
#[pyclass(module = "mc683xx", frozen, get_all)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct QspiClock {
    /// CPOL.
    polarity: ClockPolarity,
    /// CPHA.
    phase: ClockPhase,
    /// System clocks in an SCK period, 2 x SPBR.
    period: u64,
    /// The system clock's frequency.
    system_clock_hz: u64,
    /// SCK's frequency (QSMRM equation 4-1), rounded down.
    baud_hz: u64,
}

impl From<peripheral::QspiClock> for QspiClock {
    fn from(clock: peripheral::QspiClock) -> Self {
        Self {
            polarity: match clock.polarity {
                peripheral::ClockPolarity::IdleLow => ClockPolarity::IdleLow,
                peripheral::ClockPolarity::IdleHigh => ClockPolarity::IdleHigh,
            },
            phase: match clock.phase {
                peripheral::ClockPhase::CaptureLeading => ClockPhase::CaptureLeading,
                peripheral::ClockPhase::ChangeLeading => ClockPhase::ChangeLeading,
            },
            period: clock.period.0,
            system_clock_hz: clock.system_clock.hz(),
            baud_hz: clock.baud().hz(),
        }
    }
}

#[pymethods]
impl QspiClock {
    fn __repr__(&self) -> String {
        format!(
            "QspiClock(polarity={:?}, phase={:?}, period={}, system_clock_hz={}, baud_hz={})",
            self.polarity, self.phase, self.period, self.system_clock_hz, self.baud_hz
        )
    }
}

/// A QSPI transfer, one queue entry's, as a peripheral's `transfer`
/// gets it (`QspiTransfer`).
#[pyclass(module = "mc683xx", frozen, get_all)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct QspiTransfer {
    /// The queue entry, 0 to 15.
    entry: u8,
    /// The levels on PCS[3:0].
    chip_selects: u8,
    /// The bits shifted out, right-aligned.
    out: u16,
    /// How many.
    bits: u8,
    /// The serial clock it is shifted with.
    clock: QspiClock,
    /// When it ends, in system clocks.
    now: u64,
}

impl From<peripheral::QspiTransfer> for QspiTransfer {
    fn from(transfer: peripheral::QspiTransfer) -> Self {
        Self {
            entry: transfer.entry,
            chip_selects: transfer.chip_selects,
            out: transfer.out,
            bits: transfer.bits,
            clock: transfer.clock.into(),
            now: transfer.now.0,
        }
    }
}

#[pymethods]
impl QspiTransfer {
    fn __repr__(&self) -> String {
        format!(
            "QspiTransfer(entry={}, chip_selects={:#06b}, out={:#x}, bits={}, now={})",
            self.entry, self.chip_selects, self.out, self.bits, self.now
        )
    }
}

/// A Python object on the QSPI's pins.
#[derive(Debug)]
pub(crate) struct PythonSpiPeripheral {
    /// The object, for the host to have back.
    object: Held,
    /// `transfer(transfer) -> int`, given a `QspiTransfer`.
    transfer: Held,
    /// `chip_selects(levels, now)`, if it has one.
    chip_selects: Option<Held>,
    /// `clock(clock, now)`, if it has one.
    clock: Option<Held>,
    /// `reset(now)`, if it has one.
    reset: Option<Held>,
    /// `deferrable() -> bool`, if it has one.
    deferrable: Option<Held>,
    /// Where an exception it raises waits.
    pending: Pending,
}

impl PythonSpiPeripheral {
    /// `object`, which must have `transfer`.
    pub(crate) fn new(object: &Bound<'_, PyAny>, pending: Pending) -> PyResult<Self> {
        let transfer = object
            .getattr_opt("transfer")?
            .ok_or_else(|| PyTypeError::new_err("a QSPI peripheral needs transfer"))?;
        Ok(Self {
            object: pending.keep(object),
            transfer: pending.keep(&transfer),
            chip_selects: object
                .getattr_opt("chip_selects")?
                .map(|method| pending.keep(&method)),
            clock: object
                .getattr_opt("clock")?
                .map(|method| pending.keep(&method)),
            reset: object
                .getattr_opt("reset")?
                .map(|method| pending.keep(&method)),
            deferrable: object
                .getattr_opt("deferrable")?
                .map(|method| pending.keep(&method)),
            pending,
        })
    }

    /// The object, unless the collector cleared it.
    pub(crate) fn object(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.object.object(py).ok()
    }
}

/// The bits shifted back, which must fit the transfer.
pub(crate) fn shifted_in(value: &Bound<'_, PyAny>, bits: u8) -> PyResult<u16> {
    let value: u16 = value.extract()?;
    if u32::from(value) >> bits == 0 {
        Ok(value)
    } else {
        Err(PyValueError::new_err(format!(
            "{value:#x} does not fit a {bits}-bit transfer"
        )))
    }
}

impl Save for PythonSpiPeripheral {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        python_save(&self.object, &self.pending, PYTHON_PERIPHERAL)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        python_restore(&self.object, &self.pending, PYTHON_PERIPHERAL, snapshot)
    }
}

impl SpiPeripheral for PythonSpiPeripheral {
    fn transfer(&mut self, transfer: peripheral::QspiTransfer) -> u16 {
        Python::attach(|py| {
            self.transfer
                .call1(py, (QspiTransfer::from(transfer),))
                .and_then(|value| shifted_in(value.bind(py), transfer.bits))
                .unwrap_or_else(|error| {
                    self.pending.record(error);
                    0
                })
        })
    }

    fn chip_selects(&mut self, levels: u8, now: Cycles) {
        let Some(method) = &self.chip_selects else {
            return;
        };
        Python::attach(|py| {
            if let Err(error) = method.call1(py, (levels, now.0)) {
                self.pending.record(error);
            }
        });
    }

    fn deferrable(&self) -> bool {
        let Some(method) = &self.deferrable else {
            return false;
        };
        Python::attach(|py| {
            method
                .call0(py)
                .and_then(|value| value.bind(py).is_truthy())
                .unwrap_or_else(|error| {
                    self.pending.record(error);
                    false
                })
        })
    }

    fn reset(&mut self, now: Cycles) {
        let Some(method) = &self.reset else {
            return;
        };
        Python::attach(|py| {
            if let Err(error) = method.call1(py, (now.0,)) {
                self.pending.record(error);
            }
        });
    }

    fn clock(&mut self, clock: peripheral::QspiClock, now: Cycles) {
        let Some(method) = &self.clock else {
            return;
        };
        Python::attach(|py| {
            if let Err(error) = method.call1(py, (QspiClock::from(clock), now.0)) {
                self.pending.record(error);
            }
        });
    }
}
