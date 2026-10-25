//! A board file run from Python (PLAN.md, Python and Boards): `Board`
//! loads it with the built-in peripheral types and the host's Python
//! classes, registered by name, and runs it on board time.  A Python
//! peripheral is any object whose class declares its ports in `PORTS` and has
//! the `Peripheral` trait's methods it needs; a missing one takes the trait's
//! default.

use std::collections::BTreeMap;
use std::fmt;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::num::NonZeroU64;
use std::path::PathBuf;

use mc683xx_board::board::Board;
use mc683xx_board::board::BoardStopRequest;
use mc683xx_board::board::BoardTracing;
use mc683xx_board::board::External;
use mc683xx_board::chip::Chip;
use mc683xx_board::description::Options;
use mc683xx_board::inspect::Field;
use mc683xx_board::inspect::Inspection;
use mc683xx_board::inspect::Value;
use mc683xx_board::observe::Observer;
use mc683xx_board::observe::ObserverId;
use mc683xx_board::pacing::QUANTUM;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Lane;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::PeripheralAccess;
use mc683xx_board::peripheral::Role;
use mc683xx_board::peripheral::Transfer;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::ParameterTable;
use mc683xx_board::registry::ParameterValue;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Direction;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::ParseKindError;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Clock;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Phase;
use mc683xx_board::time::Time;
use mc683xx_board::trace::Line;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::peripheral;
use mc683xx_modules::peripheral::floating_miso;
use mc683xx_modules::qsm::sci::Frame;
use pyo3::Bound;
use pyo3::IntoPyObjectExt;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyErr;
use pyo3::PyResult;
use pyo3::PyTraverseError;
use pyo3::PyVisit;
use pyo3::Python;
use pyo3::exceptions::PyOverflowError;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyTypeError;
use pyo3::exceptions::PyValueError;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyBool;
use pyo3::types::PyBoolMethods;
use pyo3::types::PyBytes;
use pyo3::types::PyBytesMethods;
use pyo3::types::PyDict;
use pyo3::types::PyDictMethods;
use pyo3::types::PyInt;
use pyo3::types::PyList;
use pyo3::types::PyListMethods;
use pyo3::types::PyMapping;
use pyo3::types::PyMappingMethods;
use pyo3::types::PyString;
use pyo3::types::PyTuple;

use crate::chip::BoardChip;
use crate::chip::Event;
use crate::chip::Stop;
use crate::control;
use crate::control::BoardControlView;
use crate::control::PacingView;
use crate::exceptions;
use crate::gdb;
use crate::held::Held;
use crate::hooks::HookIdView;
use crate::mapped::Pending;
use crate::mapped::UNDRIVEN;
use crate::mapped::python_method;
use crate::mapped::python_restore;
use crate::mapped::python_save;
use crate::peripherals::Frame as FrameView;
use crate::qspi::QspiClock;
use crate::qspi::shifted_in;

/// A board error as Python's `ValueError`.
fn value_error(error: impl fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

/// What a peripheral does with an attachment's cycles (Rust's
/// `peripheral::Role`).
#[pyclass(
    name = "Role",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RoleView {
    /// Reads only.
    Read,
    /// Writes only.
    Write,
    /// Both.
    ReadWrite,
    /// Interrupt acknowledge cycles, in CPU space (SIMRM 7.8.1).
    Iack,
}

impl From<Role> for RoleView {
    fn from(role: Role) -> Self {
        match role {
            Role::Read => Self::Read,
            Role::Write => Self::Write,
            Role::ReadWrite => Self::ReadWrite,
            Role::Iack => Self::Iack,
            _ => panic!("a role the bindings do not know"),
        }
    }
}

/// The data lanes an attachment wires to a peripheral (Rust's
/// `peripheral::Lane`).
#[pyclass(
    name = "Lane",
    module = "mc683xx",
    eq,
    hash,
    eq_int,
    frozen,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LaneView {
    /// Both: the cycle as it is, byte or word.
    Both,
    /// The upper byte lane, DATA[15:8].
    Upper,
    /// The lower byte lane, DATA[7:0].
    Lower,
}

impl From<Lane> for LaneView {
    fn from(lane: Lane) -> Self {
        match lane {
            Lane::Both => Self::Both,
            Lane::Upper => Self::Upper,
            Lane::Lower => Self::Lower,
        }
    }
}

/// A bus cycle as a Python peripheral sees it.
#[pyclass(name = "PeripheralAccess", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct AccessView(PeripheralAccess);

#[pymethods]
impl AccessView {
    /// The attachment it came through: its index in `attach`.
    #[getter]
    const fn attachment(&self) -> usize {
        self.0.attachment
    }

    /// The chip select that carried it: `CSBOOT`, `CS0` to `CS10`.
    #[getter]
    fn select(&self) -> String {
        self.0.select.to_string()
    }

    /// What the peripheral does with the attachment's cycles.
    #[getter]
    fn role(&self) -> RoleView {
        self.0.role.into()
    }

    /// The data lanes the attachment wires.
    #[getter]
    fn lane(&self) -> LaneView {
        self.0.lane.into()
    }

    /// The address on the peripheral's own address pins, plus the offset.
    #[getter]
    const fn address(&self) -> u32 {
        self.0.address
    }

    /// Bytes: 1 or 2.
    #[getter]
    const fn size(&self) -> u32 {
        self.0.size.bytes()
    }

    /// FC[2:0].
    #[getter]
    const fn function_code(&self) -> u8 {
        self.0.function_code.bits()
    }

    /// When its instruction started, in picoseconds.
    #[getter]
    const fn now(&self) -> u64 {
        self.0.now.0
    }

    fn __repr__(&self) -> String {
        format!(
            "PeripheralAccess(attachment={}, select={}, role={:?}, lane={:?}, address={:#x}, \
             size={}, now={})",
            self.attachment(),
            self.select(),
            self.role(),
            self.lane(),
            self.address(),
            self.size(),
            self.now()
        )
    }
}

/// A QSPI transfer as a Python peripheral sees it (`peripheral::Transfer`).
#[pyclass(name = "Transfer", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct TransferView(Transfer);

#[pymethods]
impl TransferView {
    /// The attachment it came through: its index in `attach`.
    #[getter]
    const fn attachment(&self) -> usize {
        self.0.attachment
    }

    /// The queue entry, 0 to 15.
    #[getter]
    const fn entry(&self) -> u8 {
        self.0.entry
    }

    /// The levels on PCS[3:0].
    #[getter]
    const fn chip_selects(&self) -> u8 {
        self.0.chip_selects
    }

    /// The bits shifted out, right-aligned.
    #[getter]
    const fn out(&self) -> u16 {
        self.0.out
    }

    /// How many.
    #[getter]
    const fn bits(&self) -> u8 {
        self.0.bits
    }

    /// The serial clock it is shifted with: its mode and SCK period.
    #[getter]
    fn clock(&self) -> QspiClock {
        self.0.clock.into()
    }

    /// Whether it is the first transfer since the attachment's select
    /// asserted.
    #[getter]
    const fn starts_frame(&self) -> bool {
        self.0.starts_frame
    }

    /// When it ends, in picoseconds.
    #[getter]
    const fn now(&self) -> u64 {
        self.0.now.0
    }

    fn __repr__(&self) -> String {
        format!(
            "Transfer(attachment={}, entry={}, chip_selects={:#06b}, out={:#x}, bits={}, \
             starts_frame={}, now={})",
            self.0.attachment,
            self.0.entry,
            self.0.chip_selects,
            self.0.out,
            self.0.bits,
            if self.0.starts_frame { "True" } else { "False" },
            self.0.now.0
        )
    }
}

/// A serial frame with its timing, as Python sees one: the frame, the
/// sender's bit rate and its length (`signal::Serial`).
#[pyclass(name = "Serial", module = "mc683xx", frozen, eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SerialView(Serial);

#[pymethods]
impl SerialView {
    /// `frame` at `baud_nanohertz`, `bits` bits on the line, start and
    /// stop bits included: its length rounded down to a picosecond
    /// (Rust's `Serial::new`).
    #[new]
    fn new(frame: FrameView, baud_nanohertz: u64, bits: u32) -> PyResult<Self> {
        Ok(Self(Serial::new(
            Frame::try_from(frame)?,
            Frequency(baud_nanohertz),
            bits,
        )))
    }

    /// `frame` at `baud_nanohertz`, `length` picoseconds from its start
    /// bit's leading edge to the end of its stop bits (Rust's
    /// `Serial::timed`).
    #[staticmethod]
    fn timed(frame: FrameView, baud_nanohertz: u64, length: u64) -> PyResult<Self> {
        Ok(Self(Serial::timed(
            Frame::try_from(frame)?,
            Frequency(baud_nanohertz),
            length,
        )))
    }

    /// `frame` with no timing: its rate and length zero (Rust's
    /// `Serial::untimed`).
    #[staticmethod]
    fn untimed(frame: FrameView) -> PyResult<Self> {
        Ok(Self(Serial::untimed(Frame::try_from(frame)?)))
    }

    /// The frame.
    #[getter]
    fn frame(&self) -> FrameView {
        self.0.frame.into()
    }

    /// The sender's bit rate, in nanohertz; zero if untimed.
    #[getter]
    const fn baud_nanohertz(&self) -> u64 {
        self.0.baud.0
    }

    /// Picoseconds from start bit to the end of the stop bits; zero if
    /// untimed.
    #[getter]
    const fn length(&self) -> u64 {
        self.0.length
    }

    /// When its stop bits end, for a start bit at `start` picoseconds.
    fn end(&self, start: u64) -> u64 {
        self.0.end(Time(start)).0
    }

    fn __repr__(&self) -> String {
        format!(
            "Serial(frame={:?}, baud_nanohertz={}, length={})",
            self.0.frame,
            self.baud_nanohertz(),
            self.length()
        )
    }

    /// Its frame's, rate's and length's, so that equal ones hash alike.
    fn __hash__(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        (self.frame(), self.baud_nanohertz(), self.length()).hash(&mut hasher);
        hasher.finish()
    }
}

/// A run-time endpoint, as Python sees one (`board::External`): its
/// name, its nets' type and its direction.  As a string it is its
/// name, and it equals its name, as `Board.externals` once gave.
#[pyclass(name = "External", module = "mc683xx", frozen)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalView(External);

#[pymethods]
impl ExternalView {
    /// Its name.
    #[getter]
    fn name(&self) -> &str {
        &self.0.name
    }

    /// Its nets' type, as a board file names it.
    #[getter]
    fn kind(&self) -> String {
        self.0.kind.to_string()
    }

    /// `out` if it drives a net, `in` if it is driven, `in-out` if both.
    #[getter]
    fn direction(&self) -> String {
        self.0.direction.to_string()
    }

    fn __str__(&self) -> &str {
        &self.0.name
    }

    fn __repr__(&self) -> String {
        format!(
            "External(name={:?}, kind={:?}, direction={:?})",
            self.0.name,
            self.kind(),
            self.direction()
        )
    }

    /// Equal to another with the same name, type and direction, or to
    /// its name.
    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        if let Ok(other) = other.cast::<Self>() {
            return *other.get() == *self;
        }
        other
            .extract::<&str>()
            .is_ok_and(|name| name == self.0.name)
    }

    /// Its name's, so that it hashes as the string it equals.
    fn __hash__(&self, py: Python<'_>) -> PyResult<isize> {
        PyString::new(py, &self.0.name).hash()
    }
}

/// An observer attached to a net, as `Board.observe` returns it
/// (`observe::ObserverId`).
#[pyclass(name = "ObserverId", module = "mc683xx", frozen, eq, hash)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ObserverIdView(ObserverId);

#[pymethods]
impl ObserverIdView {
    fn __repr__(&self) -> String {
        format!("{:?}", self.0)
    }
}

/// A Python observer on a net (`Board.observe`).
struct PythonObserver {
    /// Called at each change.
    callback: Held,
    /// Called after a restore, if given.
    on_restore: Option<Held>,
    /// Whether serial values come timed.
    timed: bool,
    /// Where an exception waits.
    pending: Pending,
}

impl Observer for PythonObserver {
    /// An exception stops the run, and `run_for` raises it.
    fn changed(&mut self, net: &str, at: Time, signal: Signal) {
        Python::attach(|py| {
            let called = python_value(py, signal, self.timed)
                .and_then(|value| self.callback.call1(py, (net, at.0, value)));
            if let Err(error) = called {
                self.pending.record(error);
            }
        });
    }

    /// An exception is raised by `restore`, the board restored: it
    /// stops nothing.
    fn restored(&mut self, net: &str, at: Time, signal: Option<Signal>) {
        let Some(on_restore) = &self.on_restore else {
            return;
        };
        Python::attach(|py| {
            let called = signal
                .map(|signal| python_value(py, signal, self.timed))
                .transpose()
                .and_then(|value| on_restore.call1(py, (net, at.0, value)));
            if let Err(error) = called {
                self.pending.hold(error);
            }
        });
    }
}

/// The handle by which the host asks a board's run to stop (Rust's
/// `board::BoardStopRequest`), as `Board.stop_request` gives it: for an
/// observer's callback, which ends the run by it without raising.  It
/// holds the board weakly; once the board is gone it does nothing.
#[pyclass(name = "BoardStopRequest", module = "mc683xx", unsendable)]
#[derive(Debug, Clone)]
pub(crate) struct BoardStopRequestView(BoardStopRequest);

#[pymethods]
impl BoardStopRequestView {
    /// Asks the run to stop (`Stop.Requested()`).
    fn request(&self) {
        self.0.request();
    }

    fn __repr__(&self) -> &'static str {
        "BoardStopRequest()"
    }
}

/// Where a Python peripheral drives its outputs during a call.
#[pyclass(name = "Drive", module = "mc683xx", unsendable)]
#[derive(Debug)]
pub(crate) struct DriveView {
    /// The call's time.
    now: u64,
    /// Each port's kind, by index.
    kinds: Vec<Kind>,
    /// What was driven: time, port, value.
    driven: Vec<(Time, usize, Signal)>,
    /// Memory writes: memory, offset, bytes.
    writes: Vec<(String, usize, Vec<u8>)>,
    /// Whether the peripheral asked the run to stop.
    stop: bool,
    /// Its analog inputs' values now, by port, sampled as the call
    /// began.
    samples: Vec<(usize, Option<f64>)>,
    /// The memories taken (`true`) or released.
    memories: Vec<(String, bool)>,
}

#[pymethods]
impl DriveView {
    /// The value on analog input `port` now, or `None` if nothing has
    /// driven it.
    fn sample(&self, port: usize) -> PyResult<Option<f64>> {
        self.samples
            .iter()
            .find(|&&(input, _)| input == port)
            .map(|&(_, value)| value)
            .ok_or_else(|| PyValueError::new_err(format!("no analog input {port}")))
    }

    /// Takes the board memory `memory` off the fast path: its cycles
    /// come to this peripheral's `memory_read` and `memory_write` until
    /// `release_memory`.
    fn take_memory(&mut self, memory: String) {
        self.memories.push((memory, true));
    }

    /// Gives the memory `memory` back to the fast path.
    fn release_memory(&mut self, memory: String) {
        self.memories.push((memory, false));
    }

    /// The call's time, in picoseconds.
    #[getter]
    const fn now(&self) -> u64 {
        self.now
    }

    /// Drives `value` on output `port` now.
    fn drive(&mut self, port: usize, value: &Bound<'_, PyAny>) -> PyResult<()> {
        self.drive_at(self.now, port, value)
    }

    /// Drives `value` on output `port` from `at` picoseconds, or now if
    /// that is past.
    fn drive_at(&mut self, at: u64, port: usize, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let kind = *self
            .kinds
            .get(port)
            .ok_or_else(|| PyValueError::new_err(format!("no port {port}")))?;
        let signal = signal(kind, value)?;
        self.driven.push((Time(at.max(self.now)), port, signal));
        Ok(())
    }

    /// Writes `data` at `offset` in the board's memory `memory` when the
    /// chip's quantum ends.
    fn write_memory(&mut self, memory: String, offset: usize, data: Vec<u8>) {
        self.writes.push((memory, offset, data));
    }

    /// Asks the run to stop after the instruction running now.
    const fn stop(&mut self) {
        self.stop = true;
    }
}

/// A Python value as a signal of `kind`: a bool for a level, a `Serial`
/// or, untimed, an int for a serial frame's data (`None` a break), an
/// int for a bus's word, a float for an analog value, `(at, nanohertz,
/// phase)` for a clock.
pub(crate) fn signal(kind: Kind, value: &Bound<'_, PyAny>) -> PyResult<Signal> {
    Ok(match kind {
        Kind::Logic | Kind::Edge => Signal::Level(value.is_truthy()?),
        Kind::Serial => {
            if let Ok(serial) = value.extract::<SerialView>() {
                Signal::Serial(serial.0)
            } else {
                let frame = match value.extract::<Option<u16>>()? {
                    Some(data) => Frame::try_from(FrameView::Data(data))?,
                    None => Frame::Break,
                };
                Signal::Serial(Serial::untimed(frame))
            }
        }
        Kind::Analog => Signal::Analog(value.extract()?),
        Kind::Bus => Signal::Bus(value.extract()?),
        Kind::Clock => {
            let (at, frequency, phase): (u64, u64, u128) = value.extract()?;
            Signal::Clock(Clock {
                at: Time(at),
                frequency: Frequency(frequency),
                phase: Phase(phase),
            })
        }
        _ => return Err(PyTypeError::new_err("no Python value for this net type")),
    })
}

/// A signal as a Python value, as `signal` takes one: a serial frame a
/// `Serial` if `timed`, else its data alone.
fn python_value(py: Python<'_>, signal: Signal, timed: bool) -> PyResult<Py<PyAny>> {
    match signal {
        Signal::Level(level) => level.into_py_any(py),
        Signal::Serial(serial) if timed => SerialView(serial).into_py_any(py),
        Signal::Serial(serial) => match serial.frame {
            Frame::Data(data) => data.into_py_any(py),
            Frame::Break => Ok(py.None()),
        },
        Signal::Analog(value) => value.into_py_any(py),
        Signal::Bus(word) => word.into_py_any(py),
        Signal::Clock(clock) => (clock.at.0, clock.frequency.0, clock.phase.0).into_py_any(py),
        _ => Err(PyTypeError::new_err("no Python value for this signal")),
    }
}

/// A parameter value as a Python one.
fn parameter_value(py: Python<'_>, value: &ParameterValue) -> PyResult<Py<PyAny>> {
    match value {
        ParameterValue::String(text) => text.into_py_any(py),
        ParameterValue::Integer(integer) => integer.into_py_any(py),
        ParameterValue::Float(float) => float.into_py_any(py),
        ParameterValue::Boolean(boolean) => boolean.into_py_any(py),
        ParameterValue::Datetime(datetime) => datetime.into_py_any(py),
        ParameterValue::Array(values) => {
            let values = values
                .iter()
                .map(|value| parameter_value(py, value))
                .collect::<PyResult<Vec<_>>>()?;
            Ok(PyList::new(py, values)?.into_any().unbind())
        }
        ParameterValue::Table(table) => Ok(parameter_table(py, table)?.into_any().unbind()),
        _ => Err(PyTypeError::new_err("a parameter with no Python value")),
    }
}

/// A Python value as a parameter value: a bool, int, float, str, list or
/// tuple, or mapping (a dict) with str keys.
fn python_parameter(value: &Bound<'_, PyAny>) -> PyResult<ParameterValue> {
    if let Ok(boolean) = value.cast::<PyBool>() {
        return Ok(ParameterValue::Boolean(boolean.is_true()));
    }
    if let Ok(mapping) = value.cast::<PyMapping>() {
        return python_table(mapping).map(ParameterValue::Table);
    }
    if value.cast::<PyString>().is_ok() {
        return Ok(ParameterValue::String(value.extract()?));
    }
    if let Ok(integer) = value.extract::<i64>() {
        return Ok(ParameterValue::Integer(integer));
    }
    if let Ok(float) = value.extract::<f64>() {
        return Ok(ParameterValue::Float(float));
    }
    if value.cast::<PyList>().is_ok() || value.cast::<PyTuple>().is_ok() {
        return value
            .try_iter()?
            .map(|item| python_parameter(&item?))
            .collect::<PyResult<Vec<_>>>()
            .map(ParameterValue::Array);
    }
    Err(PyTypeError::new_err(format!(
        "{value}: a parameter is a bool, int, float, str, list or dict"
    )))
}

/// A Python mapping as a parameter table.
fn python_table(mapping: &Bound<'_, PyMapping>) -> PyResult<ParameterTable> {
    entries(mapping)?
        .into_iter()
        .map(|(key, value)| Ok((key.extract::<String>()?, python_parameter(&value)?)))
        .collect()
}

/// A Python mapping's keys and values, a dict's or any other's.
fn entries<'py>(
    mapping: &Bound<'py, PyMapping>,
) -> PyResult<Vec<(Bound<'py, PyAny>, Bound<'py, PyAny>)>> {
    mapping.items()?.iter().map(|item| item.extract()).collect()
}

/// A parameter table as a Python dict.
fn parameter_table<'py>(py: Python<'py>, table: &ParameterTable) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (key, value) in table {
        dict.set_item(key, parameter_value(py, value)?)?;
    }
    Ok(dict)
}

/// An inspection as a Python dict: a field's name to its value, an
/// `int`, `bool`, `float`, `str`, `bytes` or, for a group, a dict.
fn inspection_dict<'py>(py: Python<'py>, inspection: &Inspection) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for field in &inspection.fields {
        let value = match &field.value {
            Value::Integer(integer) => integer.into_py_any(py)?,
            Value::Boolean(boolean) => boolean.into_py_any(py)?,
            Value::Real(real) => real.into_py_any(py)?,
            Value::Text(text) => text.into_py_any(py)?,
            Value::Bytes(bytes) => PyBytes::new(py, bytes).into_any().unbind(),
            Value::Group(group) => inspection_dict(py, group)?.into_any().unbind(),
            _ => {
                return Err(PyTypeError::new_err(
                    "an inspected value with no Python value",
                ));
            }
        };
        dict.set_item(&field.name, value)?;
    }
    Ok(dict)
}

/// A Python mapping as an inspection, as `inspection_dict` gives one.
fn python_inspection(mapping: &Bound<'_, PyMapping>) -> PyResult<Inspection> {
    entries(mapping)?
        .into_iter()
        .try_fold(Inspection::default(), |inspection, (key, value)| {
            let name: String = key.extract()?;
            Ok(if let Ok(boolean) = value.cast::<PyBool>() {
                inspection.boolean(name, boolean.is_true())
            } else if let Ok(bytes) = value.cast::<PyBytes>() {
                inspection.bytes(name, bytes.as_bytes())
            } else if let Ok(mapping) = value.cast::<PyMapping>() {
                inspection.group(name, python_inspection(mapping)?)
            } else if value.cast::<PyString>().is_ok() {
                inspection.text(name, value.extract::<String>()?)
            } else if value.cast::<PyInt>().is_ok() {
                let integer = value.extract::<i64>().map_err(|_| {
                    PyOverflowError::new_err(format!(
                        "{name}: {value} does not fit an inspected int, 64 bits signed"
                    ))
                })?;
                inspection.integer(name, integer)
            } else if let Ok(real) = value.extract::<f64>() {
                inspection.field(Field::new(name, Value::Real(real)))
            } else {
                return Err(PyTypeError::new_err(format!(
                    "{name}: an inspected value is an int, bool, float, str, bytes or dict"
                )));
            })
        })
}

/// A Python object as a board peripheral.
struct PythonPeripheral {
    /// The object.
    object: Held,
    /// Its ports' kinds, by index.
    kinds: Vec<Kind>,
    /// Its analog input ports.
    analog_inputs: Vec<usize>,
    /// Whether its class asks for serial frames as `Serial`s
    /// (`TIMED_SERIAL`).
    timed_serial: bool,
    /// Where an exception it raises waits.
    pending: Pending,
}

impl PythonPeripheral {
    /// Calls `name` with `arguments` and a `Drive` for `drive`, if the
    /// object has it.  `BusError` is the cycle's bus error; any other
    /// exception (a `KeyboardInterrupt` too) waits and stops the run,
    /// and `undriven` answers, the bus not faulted.  `None` if it has
    /// no `name`.
    fn call<T>(
        &self,
        name: &str,
        drive: &mut Drive<'_>,
        arguments: impl FnOnce(Python<'_>) -> PyResult<Vec<Py<PyAny>>>,
        convert: impl FnOnce(&Bound<'_, PyAny>) -> PyResult<T>,
        undriven: T,
    ) -> Option<Result<T, BusError>> {
        Python::attach(|py| {
            let object = self.object.bind(py).ok()?;
            let method = object.getattr_opt(name).ok().flatten()?;
            let view = Py::new(
                py,
                DriveView {
                    now: drive.now().0,
                    kinds: self.kinds.clone(),
                    driven: Vec::new(),
                    writes: Vec::new(),
                    stop: false,
                    samples: self
                        .analog_inputs
                        .iter()
                        .map(|&port| (port, drive.sample(port)))
                        .collect(),
                    memories: Vec::new(),
                },
            )
            .ok()?;
            let result = arguments(py)
                .and_then(|mut arguments| {
                    arguments.push(view.clone_ref(py).into_any());
                    method.call1(PyTuple::new(py, arguments)?)
                })
                .and_then(|value| convert(&value));
            let view = view.borrow(py);
            for &(at, port, signal) in &view.driven {
                drive.drive_at(at, port, signal);
            }
            for (memory, offset, bytes) in &view.writes {
                drive.write_memory(memory.clone(), *offset, bytes.clone());
            }
            for (memory, take) in &view.memories {
                if *take {
                    drive.take_memory(memory.clone());
                } else {
                    drive.release_memory(memory.clone());
                }
            }
            if view.stop {
                drive.stop();
            }
            Some(result.or_else(|error| {
                if error.is_instance_of::<exceptions::BusError>(py) {
                    return Err(BusError);
                }
                self.pending.hold(error);
                drive.stop();
                Ok(undriven)
            }))
        })
    }

    /// The object's `name`, if it has one.
    fn method<'py>(&self, py: Python<'py>, name: &str) -> Option<Bound<'py, PyAny>> {
        python_method(py, &self.object, name)
    }

    /// Calls `name` with no `Drive`; an exception waits, stops the run
    /// through the board's stop request, and the default answers.
    fn ask<T>(
        &self,
        name: &str,
        arguments: impl FnOnce(Python<'_>) -> PyResult<Vec<Py<PyAny>>>,
        convert: impl FnOnce(&Bound<'_, PyAny>) -> PyResult<T>,
        default: T,
    ) -> T {
        Python::attach(|py| {
            let Some(method) = self.method(py, name) else {
                return default;
            };
            match arguments(py)
                .and_then(|arguments| method.call1(PyTuple::new(py, arguments)?))
                .and_then(|value| convert(&value))
            {
                Ok(value) => value,
                Err(error) => {
                    self.pending.record(error);
                    default
                }
            }
        })
    }
}

/// An access as a Python argument.
fn access_argument(py: Python<'_>, access: PeripheralAccess) -> PyResult<Py<PyAny>> {
    Ok(Py::new(py, AccessView(access))?.into_any())
}

impl Peripheral for PythonPeripheral {
    fn read(&mut self, access: PeripheralAccess, drive: &mut Drive<'_>) -> Result<u32, BusError> {
        self.call(
            "read",
            drive,
            |py| Ok(vec![access_argument(py, access)?]),
            |value| value.extract::<u32>(),
            UNDRIVEN,
        )
        .unwrap_or(Err(BusError))
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        self.call(
            "write",
            drive,
            |py| Ok(vec![access_argument(py, access)?, value.into_py_any(py)?]),
            |_| Ok(()),
            (),
        )
        .unwrap_or(Err(BusError))
    }

    fn wait_states(&mut self, access: PeripheralAccess) -> Option<u8> {
        self.ask(
            "wait_states",
            |py| Ok(vec![access_argument(py, access)?]),
            |value| value.extract::<Option<u8>>(),
            Some(0),
        )
    }

    fn interrupt_request(&self) -> bool {
        self.ask(
            "interrupt_request",
            |_| Ok(Vec::new()),
            |value| value.is_truthy(),
            false,
        )
    }

    fn memory_read(
        &mut self,
        memory: &str,
        access: PeripheralAccess,
        drive: &mut Drive<'_>,
    ) -> Result<u32, BusError> {
        self.call(
            "memory_read",
            drive,
            |py| Ok(vec![memory.into_py_any(py)?, access_argument(py, access)?]),
            |value| value.extract::<u32>(),
            UNDRIVEN,
        )
        .unwrap_or(Err(BusError))
    }

    fn memory_write(
        &mut self,
        memory: &str,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        self.call(
            "memory_write",
            drive,
            |py| {
                Ok(vec![
                    memory.into_py_any(py)?,
                    access_argument(py, access)?,
                    value.into_py_any(py)?,
                ])
            },
            |_| Ok(()),
            (),
        )
        .unwrap_or(Ok(()))
    }

    fn power_on(&mut self, drive: &mut Drive<'_>) {
        let _ = self.call("power_on", drive, |_| Ok(Vec::new()), |_| Ok(()), ());
    }

    fn reset(&mut self, now: Time, drive: &mut Drive<'_>) {
        let _ = self.call(
            "reset",
            drive,
            |py| Ok(vec![now.0.into_py_any(py)?]),
            |_| Ok(()),
            (),
        );
    }

    fn transfer(&mut self, transfer: Transfer, drive: &mut Drive<'_>) -> u16 {
        let floating = floating_miso(transfer.bits);
        match self.call(
            "transfer",
            drive,
            |py| Ok(vec![Py::new(py, TransferView(transfer))?.into_any()]),
            |value| shifted_in(value, transfer.bits),
            floating,
        ) {
            Some(Ok(value)) => value,
            Some(Err(BusError)) | None => floating,
        }
    }

    fn qspi_clock(&mut self, clock: peripheral::QspiClock, now: Time) {
        self.ask(
            "qspi_clock",
            |py| {
                Ok(vec![
                    Py::new(py, QspiClock::from(clock))?.into_any(),
                    now.0.into_py_any(py)?,
                ])
            },
            |_| Ok(()),
            (),
        );
    }

    fn select(&mut self, attachment: usize, selected: bool, now: Time, drive: &mut Drive<'_>) {
        let _ = self.call(
            "select",
            drive,
            |py| {
                Ok(vec![
                    attachment.into_py_any(py)?,
                    selected.into_py_any(py)?,
                    now.0.into_py_any(py)?,
                ])
            },
            |_| Ok(()),
            (),
        );
    }

    fn qspi_deferrable(&self) -> bool {
        self.ask(
            "qspi_deferrable",
            |_| Ok(Vec::new()),
            |value| value.is_truthy(),
            false,
        )
    }

    fn input(&mut self, port: usize, signal: Signal, now: Time, drive: &mut Drive<'_>) {
        let _ = self.call(
            "input",
            drive,
            |py| {
                Ok(vec![
                    port.into_py_any(py)?,
                    python_value(py, signal, self.timed_serial)?,
                    now.0.into_py_any(py)?,
                ])
            },
            |_| Ok(()),
            (),
        );
    }

    fn sample(&mut self, port: usize, now: Time) -> Option<f64> {
        self.ask(
            "sample",
            |py| Ok(vec![port.into_py_any(py)?, now.0.into_py_any(py)?]),
            |value| value.extract::<Option<f64>>(),
            None,
        )
    }

    fn next_event(&self) -> Option<Time> {
        self.ask(
            "next_event",
            |_| Ok(Vec::new()),
            |value| Ok(value.extract::<Option<u64>>()?.map(Time)),
            None,
        )
    }

    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {
        let _ = self.call(
            "advance",
            drive,
            |py| Ok(vec![now.0.into_py_any(py)?]),
            |_| Ok(()),
            (),
        );
    }

    /// Its `inspect()`, a mapping; an exception waits for the board's
    /// `inspect` to raise it.
    fn inspect(&self) -> Inspection {
        Python::attach(|py| {
            let Some(method) = self.method(py, "inspect") else {
                return Inspection::default();
            };
            method
                .call0()
                .and_then(|value| python_inspection(value.cast()?))
                .unwrap_or_else(|error| {
                    self.pending.hold(error);
                    Inspection::default()
                })
        })
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        python_save(&self.object, &self.pending, PYTHON_PERIPHERAL)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        python_restore(&self.object, &self.pending, PYTHON_PERIPHERAL, snapshot)
    }
}

/// What a Python peripheral's snapshot errors name.
const PYTHON_PERIPHERAL: &str = "a Python peripheral";

/// A class's `PORTS`: `(name, kind, direction)` each, as Rust's
/// `PortSpec`, the kind a net type, the direction `in`, `out` or
/// `in-out`.
fn ports(class: &Bound<'_, PyAny>) -> PyResult<Vec<PortSpec>> {
    let Some(declared) = class.getattr_opt("PORTS")? else {
        return Ok(Vec::new());
    };
    declared
        .extract::<Vec<(String, String, String)>>()?
        .into_iter()
        .map(|(name, kind, direction)| {
            let kind: Kind = kind
                .parse()
                .map_err(|error| PyTypeError::new_err(format!("{name}: {error}")))?;
            let direction: Direction = direction
                .parse()
                .map_err(|error| PyTypeError::new_err(format!("{name}: {error}")))?;
            Ok(PortSpec {
                name,
                kind,
                direction,
            })
        })
        .collect()
}

/// Why a board's run ended, and when (Rust's `board::BoardRun`).
#[pyclass(name = "BoardRun", module = "mc683xx", frozen, eq, hash, get_all)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BoardRunView {
    /// Why: its time ran out (`Stop.Budget()`), the CPU halted, a hook
    /// stopped it, or the host or a peripheral asked (`Stop.Requested()`).
    stop: Stop,
    /// Board time at its end, picoseconds: never past the time it ran
    /// to, though the chip's last instruction may end past it.
    now: u64,
}

#[pymethods]
impl BoardRunView {
    fn __repr__(&self) -> String {
        format!("BoardRun(stop={:?}, now={})", self.stop, self.now)
    }
}

/// A board, from its file, as Python runs it.
#[pyclass(name = "Board", module = "mc683xx", unsendable)]
pub(crate) struct BoardView {
    /// The board.
    board: Board<Mc68331>,
    /// Where Python peripherals' exceptions wait.
    pending: Pending,
}

impl fmt::Debug for BoardView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Board")
            .field("board", &self.board)
            .finish_non_exhaustive()
    }
}

impl BoardView {
    /// The waiting exception, if a peripheral raised one.
    fn check(&self) -> PyResult<()> {
        self.pending.check()
    }

    /// The board's chip.
    pub(crate) const fn board_chip(&self) -> &Mc68331 {
        self.board.chip()
    }

    /// The board's chip, to change, and where the board's exceptions
    /// wait.
    pub(crate) const fn board_chip_mut(&mut self) -> (&mut Mc68331, &Pending) {
        (self.board.chip_mut(), &self.pending)
    }
}

/// Picoseconds from `seconds` or `picoseconds`, whichever is given.
fn picoseconds_of(seconds: Option<f64>, picoseconds: Option<u64>) -> PyResult<u64> {
    match (seconds, picoseconds) {
        (Some(seconds), None) if seconds.is_finite() && seconds >= 0.0 => {
            Ok((seconds * PICOSECONDS_PER_SECOND as f64) as u64)
        }
        (Some(seconds), None) => Err(PyValueError::new_err(format!(
            "{seconds}: give a finite number of seconds, 0 or more"
        ))),
        (None, Some(picoseconds)) => Ok(picoseconds),
        _ => Err(PyValueError::new_err("give seconds or picoseconds")),
    }
}

impl BoardView {
    /// Runs to `until` in quanta of `quantum`, paced, for `run_for` and
    /// `run_until`.
    fn run_until_paced(
        &mut self,
        py: Python<'_>,
        until: Time,
        pacing: Option<PacingView>,
        quantum: u64,
    ) -> PyResult<BoardRunView> {
        let quantum = NonZeroU64::new(quantum)
            .ok_or_else(|| PyValueError::new_err("give a quantum of 1 ps or more"))?;
        let control = self.board.control();
        let previous = control.pacing();
        if let Some(pacing) = pacing {
            control.set_pacing(pacing.0).map_err(value_error)?;
        }
        let outcome = control::run_until_paced(py, &mut self.board, until, quantum);
        // this call's pacing put back, unless another thread changed it
        if let Some(pacing) = pacing
            && control.pacing() == pacing.0
        {
            control.set_pacing(previous).map_err(value_error)?;
        }
        // Ctrl-C first, even with a peripheral's error waiting
        let outcome = match outcome {
            Ok(outcome) => {
                self.check()?;
                outcome
            }
            Err(raised) => return Err(self.pending.after(raised)),
        };
        let run = outcome.map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok(BoardRunView {
            stop: run.stop.into(),
            now: run.now.0,
        })
    }
}

#[pymethods]
impl BoardView {
    /// Shows Python's cycle collector the Python objects it holds.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        self.pending.traverse(&visit)
    }

    /// Drops them, for the collector to break a cycle through it.
    fn __clear__(&mut self) {
        self.pending.clear();
    }

    /// The board file at `path`, its peripheral types the built-in ones and
    /// `types`' classes by name, through its power-on reset.
    #[new]
    #[pyo3(signature = (path, *, types = None, options = Vec::new(), firmware = None, images = None, seed = 0, work_dir = None, parameters = None))]
    #[expect(
        clippy::too_many_arguments,
        reason = "Python's keyword arguments, each optional"
    )]
    fn new(
        path: PathBuf,
        types: Option<&Bound<'_, PyMapping>>,
        options: Vec<String>,
        firmware: Option<PathBuf>,
        images: Option<&Bound<'_, PyMapping>>,
        seed: u64,
        work_dir: Option<PathBuf>,
        parameters: Option<&Bound<'_, PyMapping>>,
    ) -> PyResult<Self> {
        let pending = Pending::board();
        let mut registry = Registry::with_builtins();
        if let Some(types) = types {
            for (name, class) in entries(types)? {
                let name: String = name.extract()?;
                let declared = ports(&class)?;
                let kinds: Vec<Kind> = declared.iter().map(|port| port.kind).collect();
                let analog_inputs: Vec<usize> = declared
                    .iter()
                    .enumerate()
                    .filter(|(_, port)| port.kind == Kind::Analog && port.direction.sinks())
                    .map(|(index, _)| index)
                    .collect();
                let timed_serial = match class.getattr_opt("TIMED_SERIAL")? {
                    Some(timed) => timed.is_truthy()?,
                    None => false,
                };
                let class = pending.keep(&class);
                let pending = pending.clone();
                registry
                    .register(name, declared, move |context| {
                        Python::attach(|py| {
                            let parameters = parameter_table(py, context.parameters.table())
                                .and_then(|parameters| class.bind(py)?.call((), Some(&parameters)))
                                .map_err(|error| ParameterError::invalid("", error.to_string()))?;
                            Ok(Box::new(PythonPeripheral {
                                object: pending.keep(&parameters),
                                kinds: kinds.clone(),
                                analog_inputs: analog_inputs.clone(),
                                timed_serial,
                                pending: pending.clone(),
                            }) as Box<dyn Peripheral>)
                        })
                    })
                    .map_err(value_error)?;
            }
        }
        mc683xx_peripherals::register(&mut registry).map_err(value_error)?;
        let parameters = match parameters {
            Some(parameters) => entries(parameters)?
                .into_iter()
                .map(|(peripheral, table)| {
                    Ok((
                        peripheral.extract::<String>()?,
                        python_table(table.cast()?)?,
                    ))
                })
                .collect::<PyResult<_>>()?,
            None => BTreeMap::new(),
        };
        let images = match images {
            Some(images) => entries(images)?
                .into_iter()
                .map(|(name, path)| Ok((name.extract::<String>()?, path.extract::<PathBuf>()?)))
                .collect::<PyResult<_>>()?,
            None => BTreeMap::new(),
        };
        let options = {
            let mut built = Options::default();
            built.enabled = options.into_iter().collect();
            built.firmware = firmware;
            built.images = images;
            built.parameters = parameters;
            built
        };
        let mut environment = Environment::default();
        environment.seed = seed;
        environment.work_dir = work_dir;
        let board =
            Board::<Mc68331>::read(&path, &options, &registry, environment).map_err(value_error)?;
        pending.set_board_stop(board.stop_request());
        let view = Self { board, pending };
        view.check()?;
        Ok(view)
    }

    /// Runs `seconds`, or `picoseconds`, of board time from now, or until
    /// the CPU halts, a hook stops it, a peripheral asks it to or a stop
    /// comes through its `control()`.  A peripheral's exception is raised
    /// once the run stops.  It runs in quanta of `quantum_picoseconds` (5
    /// ms by default), paced by the control's pacing (Rust's
    /// `Board::run_until_paced`); the GIL is released while it sleeps
    /// between quanta.  `pacing`, if given, paces this call alone: the
    /// control's pacing is put back when the run returns or raises,
    /// unless another thread set a pacing during the run, which then
    /// stays.  Seconds not finite, or below 0, raise `ValueError`.
    #[pyo3(signature = (*, seconds = None, picoseconds = None, pacing = None, quantum_picoseconds = QUANTUM.get()))]
    fn run_for(
        &mut self,
        py: Python<'_>,
        seconds: Option<f64>,
        picoseconds: Option<u64>,
        pacing: Option<PacingView>,
        quantum_picoseconds: u64,
    ) -> PyResult<BoardRunView> {
        let span = picoseconds_of(seconds, picoseconds)?;
        let until = Time(self.board.now().0.saturating_add(span));
        self.run_until_paced(py, until, pacing, quantum_picoseconds)
    }

    /// Runs to board time `seconds`, or `picoseconds`, as `run_for` runs
    /// for them: at once if the board is already there.
    #[pyo3(signature = (*, seconds = None, picoseconds = None, pacing = None, quantum_picoseconds = QUANTUM.get()))]
    fn run_until(
        &mut self,
        py: Python<'_>,
        seconds: Option<f64>,
        picoseconds: Option<u64>,
        pacing: Option<PacingView>,
        quantum_picoseconds: u64,
    ) -> PyResult<BoardRunView> {
        let until = Time(picoseconds_of(seconds, picoseconds)?);
        self.run_until_paced(py, until, pacing, quantum_picoseconds)
    }

    /// The board's control handle, for another thread (Rust's
    /// `Board::control`): every call gives a handle to the one control.
    fn control(&self) -> BoardControlView {
        BoardControlView(self.board.control())
    }

    /// The handle by which the host asks the run to stop, on the
    /// board's own thread (Rust's `Board::stop_request`): an observer's
    /// callback holds it to end the run without raising.
    fn stop_request(&self) -> BoardStopRequestView {
        BoardStopRequestView(self.board.stop_request())
    }

    /// Waits for gdb at `address` (`host:port`, port 0 picks one),
    /// calls `ready(port)` once it listens, and lets gdb run the board
    /// until it detaches (`Ending.DETACHED`) or kills it (`Ending.KILLED`)
    /// (docs/gdb.md).  Peripherals run as in `run_for`; an exception one raises
    /// ends the session and is raised here.
    #[pyo3(signature = (address = gdb::ADDRESS, ready = None))]
    fn serve_gdb(
        slf: &Bound<'_, Self>,
        address: &str,
        ready: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<gdb::Ending> {
        let connection = gdb::accept(slf.py(), address, ready)?;
        let mut this = slf.try_borrow_mut()?;
        let this = &mut *this;
        let mut target = gdb::Checked {
            target: &mut this.board,
            pending: &this.pending,
        };
        gdb::serve(&mut target, connection)
    }

    /// Board time, picoseconds.
    #[getter]
    const fn now(&self) -> u64 {
        self.board.now().0
    }

    /// The chip's system clocks since power-on.
    #[getter]
    const fn cycles(&self) -> u64 {
        self.board.chip().now().0
    }

    /// The chip's instructions run since power-on (`Mc68331.instructions`).
    #[getter]
    const fn instructions(&self) -> u64 {
        self.board.chip().instructions()
    }

    /// The Python object of the peripheral `name`, if it is a Python peripheral.
    fn peripheral(&self, py: Python<'_>, name: &str) -> Option<Py<PyAny>> {
        self.board
            .peripheral::<PythonPeripheral>(name)
            .and_then(|peripheral| peripheral.object.object(py).ok())
    }

    /// What the peripheral `name` shows a front end, as a dict (Rust's
    /// `Board::inspect`), or `None` if there is none.
    fn inspect<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Option<Bound<'py, PyDict>>> {
        let inspection = self.board.inspect(name);
        self.check()?;
        inspection
            .map(|inspection| inspection_dict(py, &inspection))
            .transpose()
    }

    /// The marking of the peripheral `name`, if its peripheral gives one.
    fn marking(&self, name: &str) -> Option<String> {
        self.board.marking(name).map(str::to_owned)
    }

    /// The memory `name`'s bytes.
    fn memory<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = self
            .board
            .memory(name)
            .ok_or_else(|| PyValueError::new_err(format!("no memory {name:?}")))?;
        Ok(PyBytes::new(py, bytes))
    }

    /// The run-time endpoints the board's nets name, by name, then
    /// type.
    fn externals(&self) -> Vec<ExternalView> {
        self.board
            .externals()
            .into_iter()
            .map(ExternalView)
            .collect()
    }

    /// The run-time endpoint `name` drives `value` now; `kind` is the
    /// net's type (`serial` by default).
    #[pyo3(signature = (name, value, *, kind = "serial"))]
    fn send(&mut self, name: &str, value: &Bound<'_, PyAny>, kind: &str) -> PyResult<()> {
        let kind: Kind = kind
            .parse()
            .map_err(|error: ParseKindError| PyValueError::new_err(error.to_string()))?;
        self.board
            .send(name, signal(kind, value)?)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// What reached the run-time endpoint `name` since the last call:
    /// `(time, value)` each, a serial frame a `Serial` if `timed`.
    #[pyo3(signature = (name, *, timed = false))]
    fn take_received(
        &mut self,
        py: Python<'_>,
        name: &str,
        timed: bool,
    ) -> PyResult<Vec<(u64, Py<PyAny>)>> {
        self.board
            .take_received(name)
            .map_err(|error| PyValueError::new_err(error.to_string()))?
            .into_iter()
            .map(|(at, signal)| Ok((at.0, python_value(py, signal, timed)?)))
            .collect()
    }

    /// The nets, in file order: `(name, type)` each.
    fn nets(&self) -> Vec<(String, String)> {
        self.board
            .nets()
            .map(|(name, kind)| (name.to_owned(), kind.to_string()))
            .collect()
    }

    /// The value the net `name` last carried, `None` before its first
    /// change, as `take_received` gives one.
    #[pyo3(signature = (name, *, timed = false))]
    fn net_value(&self, py: Python<'_>, name: &str, timed: bool) -> PyResult<Option<Py<PyAny>>> {
        self.board
            .net_value(name)
            .map_err(value_error)?
            .map(|signal| python_value(py, signal, timed))
            .transpose()
    }

    /// Calls `callback(net, time, value)` at each change the net `name`
    /// carries from now on, as `take_received` gives a value
    /// (Rust's `Board::observe`).  An exception it raises stops the
    /// run, and is raised when `run_for` returns, as a peripheral's.
    /// `on_restore(net, time, value)`, if given, is called once after
    /// each successful `restore` with the net's restored value, `None`
    /// before its first change (Rust's `Observer::restored`); an
    /// exception it raises is raised by `restore`, the board restored.
    #[pyo3(signature = (name, callback, *, timed = false, on_restore = None))]
    fn observe(
        &mut self,
        name: &str,
        callback: &Bound<'_, PyAny>,
        timed: bool,
        on_restore: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<ObserverIdView> {
        let observer = PythonObserver {
            callback: self.pending.keep(callback),
            on_restore: on_restore.map(|on_restore| self.pending.keep(on_restore)),
            timed,
            pending: self.pending.clone(),
        };
        self.board
            .observe(name, observer)
            .map(ObserverIdView)
            .map_err(value_error)
    }

    /// Detaches the observer `observer`; whether it was attached.
    fn unobserve(&mut self, observer: &ObserverIdView) -> bool {
        self.board.unobserve(observer.0).is_some()
    }

    /// What the board traces from now on (docs/tracing.md).
    #[pyo3(signature = (*, bus_cycles = false, exceptions = false, pins = false, qspi = false, nets = false))]
    fn set_tracing(
        &mut self,
        bus_cycles: bool,
        exceptions: bool,
        pins: bool,
        qspi: bool,
        nets: bool,
    ) {
        let mut tracing = BoardTracing::default();
        tracing.nets = nets;
        tracing.bus_cycles = bus_cycles;
        tracing.exceptions = exceptions;
        tracing.pins = pins;
        tracing.qspi = qspi;
        self.board.set_tracing(tracing);
    }

    /// Traces each time the instruction at `pc` is about to run, through
    /// a hook the chip's `remove_hook` removes.
    fn watch_code(&mut self, pc: u32) -> HookIdView {
        HookIdView(self.board.watch_code(pc))
    }

    /// The board's chip: its registers, hooks, bus cycles and the
    /// levels it drives, as `Mc68331`'s methods give them (Rust's
    /// `Board::chip` and `chip_mut`).
    #[getter]
    fn chip(slf: &Bound<'_, Self>) -> BoardChip {
        BoardChip {
            board: slf.clone().unbind(),
        }
    }

    /// What the chip did since the last call (Rust's
    /// `Board::take_events`), as `Mc68331.take_events` gives it.
    fn take_events(&mut self) -> Vec<Event> {
        self.board
            .take_events()
            .into_iter()
            .filter_map(Event::of)
            .collect()
    }

    /// The trace since the last call, a line each (docs/tracing.md).
    fn take_trace(&mut self) -> Vec<String> {
        let trace = self.board.take_trace();
        let chip = <Mc68331 as Chip>::TYPE;
        trace
            .records
            .iter()
            .map(|record| Line::new(chip, record).to_string())
            .chain(
                trace
                    .nets
                    .iter()
                    .map(|net| Line::new(chip, net).to_string()),
            )
            .collect()
    }

    /// The whole board's state; every peripheral must save.
    fn save<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let saved = self.board.save();
        self.pending.check()?;
        let snapshot = saved.map_err(|error| exceptions::StateError::new_err(error.to_string()))?;
        Ok(PyBytes::new(py, &snapshot))
    }

    /// Restores a snapshot `save` made of a board loaded the same way,
    /// then calls each observer's `on_restore`.
    fn restore(&mut self, snapshot: &[u8]) -> PyResult<()> {
        let restored = self.board.restore(snapshot);
        self.pending.check()?;
        restored.map_err(|error| exceptions::StateError::new_err(error.to_string()))
    }

    fn __repr__(&self) -> String {
        format!("Board(now={}, cycles={})", self.now(), self.cycles())
    }
}
