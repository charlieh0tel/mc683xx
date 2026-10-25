//! A Python object as one of the chip's peripherals: its methods answer the
//! `MappedPeripheral` and `Timed` traits.  An exception a method raises cannot
//! cross the chip's run: `BusError` ends the cycle in a bus error; any
//! other answers as an undriven bus (a read 0, a write ignored), the
//! run stops once its instruction ends (`StopRequest`), and the
//! exception waits in `Pending` for the call into the chip to raise it
//! when it returns.

use std::cell::RefCell;
use std::rc::Rc;

use mc683xx_board::board;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::cpu::state::Save;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::Timed;
use pyo3::Bound;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyErr;
use pyo3::PyResult;
use pyo3::PyTraverseError;
use pyo3::PyVisit;
use pyo3::Python;
use pyo3::call::PyCallArgs;
use pyo3::exceptions::PyBaseException;
use pyo3::exceptions::PyKeyboardInterrupt;
use pyo3::exceptions::PyTypeError;
use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::pyclass;
use pyo3::pymethods;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyBytes;

use crate::exceptions;
use crate::held::Held;
use crate::held::Holdings;

/// Whose run a peripheral's exception stops.
#[derive(Debug, Clone)]
enum Stopper {
    /// The chip's or the bare CPU's.
    Core(StopRequest),
    /// A board's, once it is loaded: its peripherals are made, and may fail,
    /// before it has one.
    Board(Rc<RefCell<Option<board::BoardStopRequest>>>),
}

impl Default for Stopper {
    fn default() -> Self {
        Self::Core(StopRequest::default())
    }
}

/// What a target shares with its Python objects: the first exception
/// one raised in a call into the target, kept for that call to raise
/// (each also asks the target's run to stop), and the objects the
/// target holds, for its owner's `__traverse__` and `__clear__`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    /// The exception, as its value, which keeps its traceback.
    error: Rc<RefCell<Option<Py<PyBaseException>>>>,
    /// The run's stop request.
    stop: Stopper,
    /// The Python objects the target holds.
    holdings: Holdings,
}

impl Pending {
    /// Nothing waiting, for a chip or a bare CPU whose runs `stop`
    /// stops.
    pub(crate) fn new(stop: StopRequest) -> Self {
        Self {
            stop: Stopper::Core(stop),
            ..Self::default()
        }
    }

    /// Nothing waiting, for a board, whose runs stop once
    /// `set_board_stop` gives its stop request.
    pub(crate) fn board() -> Self {
        Self {
            stop: Stopper::Board(Rc::default()),
            ..Self::default()
        }
    }

    /// The board's stop request, once it is loaded.
    pub(crate) fn set_board_stop(&self, stop: board::BoardStopRequest) {
        if let Stopper::Board(slot) = &self.stop {
            *slot.borrow_mut() = Some(stop);
        }
    }

    /// Keeps `error`, unless an earlier one is waiting, and asks the
    /// run to stop once the instruction ends.
    pub(crate) fn record(&self, error: PyErr) {
        self.hold(error);
        match &self.stop {
            Stopper::Core(stop) => stop.request(),
            Stopper::Board(slot) => {
                if let Some(stop) = &*slot.borrow() {
                    stop.request();
                }
            }
        }
    }

    /// Keeps `error`, unless an earlier one is waiting, for a hook that
    /// stops the run itself; but a `KeyboardInterrupt` takes the place
    /// of an earlier other exception, which becomes its context.
    pub(crate) fn hold(&self, error: PyErr) {
        Python::attach(|py| {
            let waiting = self.error.borrow_mut().take();
            let kept = match waiting {
                None => error,
                Some(value) => interrupt_first(
                    py,
                    PyErr::from_value(value.into_bound(py).into_any()),
                    error,
                ),
            };
            *self.error.borrow_mut() = Some(kept.into_value(py));
        });
    }

    /// The waiting exception, if any, as the call's result.
    pub(crate) fn check(&self) -> PyResult<()> {
        let Some(value) = self.error.borrow_mut().take() else {
            return Ok(());
        };
        Python::attach(|py| Err(PyErr::from_value(value.into_bound(py).into_any())))
    }

    /// The exception to raise for the call's own `raised` (a signal
    /// handler's), the waiting one taken: a `KeyboardInterrupt` in either
    /// goes first, the other its context; otherwise the waiting one.
    pub(crate) fn after(&self, raised: PyErr) -> PyErr {
        match self.check() {
            Ok(()) => raised,
            Err(waiting) => Python::attach(|py| interrupt_first(py, waiting, raised)),
        }
    }

    /// Holds `object` for the target, where its owner's collector
    /// methods reach it.
    pub(crate) fn keep(&self, object: &Bound<'_, PyAny>) -> Held {
        self.holdings.hold(object)
    }

    /// Shows `visit` the waiting exception and the objects held: the
    /// owner's `__traverse__`.
    pub(crate) fn traverse(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        if let Ok(error) = self.error.try_borrow() {
            visit.call(error.as_ref())?;
        }
        self.holdings.traverse(visit)
    }

    /// Drops the waiting exception and the objects held: the owner's
    /// `__clear__`.
    pub(crate) fn clear(&self) {
        let error = self.error.borrow_mut().take();
        drop(error);
        self.holdings.clear();
    }
}

/// Of `first` and the `later` exception, the one to raise: `later` if
/// it is a `KeyboardInterrupt` and `first` is not, `first` its context
/// (unless it has one), so Ctrl-C is never hidden behind an error;
/// otherwise `first`.
fn interrupt_first(py: Python<'_>, first: PyErr, later: PyErr) -> PyErr {
    let interrupt = |error: &PyErr| error.is_instance_of::<PyKeyboardInterrupt>(py);
    if !interrupt(&later) || interrupt(&first) {
        return first;
    }
    let value = later.value(py);
    if value
        .getattr(intern!(py, "__context__"))
        .is_ok_and(|context| context.is_none())
    {
        // a failure here leaves the interrupt unchained, still raised
        let _ = value.setattr(intern!(py, "__context__"), first.value(py));
    }
    later
}

/// A bus cycle as a Python mapped peripheral sees it, a chip's or a bare
/// `Machine`'s.
#[pyclass(name = "BusCycle", module = "mc683xx", frozen)]
#[derive(Debug)]
pub(crate) struct BusCycleView(BusCycle);

#[pymethods]
impl BusCycleView {
    /// The chip select asserted, on a chip: `CSBOOT`, `CS0` to `CS10`;
    /// `None` on a bare `Machine`.
    #[getter]
    fn select(&self) -> Option<String> {
        self.0
            .select
            .and_then(ChipSelect::from_select)
            .map(|select| select.to_string())
    }

    /// Whether exception processing is stacking a frame, on a bare `Machine`;
    /// always `False` on a chip.
    #[getter]
    const fn stacking(&self) -> bool {
        self.0.stacking
    }

    /// The address: on a chip ADDR, on a bare `Machine` all 32 bits.
    #[getter]
    const fn address(&self) -> u32 {
        self.0.address
    }

    /// Bytes in the cycle: 1 or 2, or 4 on a bare `Machine` for a long the
    /// peripheral takes whole.
    #[getter]
    const fn size(&self) -> u32 {
        self.0.size.bytes()
    }

    /// FC[2:0].
    #[getter]
    const fn function_code(&self) -> u8 {
        self.0.function_code.bits()
    }

    /// When it starts, in system clocks.
    #[getter]
    const fn now(&self) -> u64 {
        self.0.now.0
    }

    fn __repr__(&self) -> String {
        format!(
            "BusCycle(select={}, address={:#010x}, size={}, function_code={}, now={}, stacking={})",
            self.select()
                .map_or_else(|| "None".to_owned(), |select| format!("{select:?}")),
            self.address(),
            self.size(),
            self.function_code(),
            self.now(),
            self.stacking()
        )
    }
}

/// A peripheral's methods, looked up once when it is wired.  Any may be
/// missing: a model on no pin needs neither `read` nor `write`.
#[derive(Debug)]
struct Methods {
    /// `read(cycle) -> int`.
    read: Option<Held>,
    /// `write(cycle, value)`.
    write: Option<Held>,
    /// `wait_states(cycle) -> int | None`.
    wait_states: Option<Held>,
    /// `interrupt_request() -> bool`.
    interrupt_request: Option<Held>,
    /// `reset(now)`.
    reset: Option<Held>,
    /// `next_event() -> int | None`.
    next_event: Option<Held>,
    /// `advance(now)`.
    advance: Option<Held>,
}

/// A Python object wired into the chip.
#[derive(Debug)]
pub(crate) struct PythonMapped {
    /// The object, for the host to have back.
    object: Held,
    /// Its methods.
    methods: Methods,
    /// Where an exception it raises waits.
    pending: Pending,
}

impl PythonMapped {
    /// `object` as a peripheral on chip-select pins: it must have `read`
    /// and `write`.
    pub(crate) fn mapped(object: &Bound<'_, PyAny>, pending: Pending) -> PyResult<Self> {
        let peripheral = Self::unwired(object, pending)?;
        if peripheral.methods.read.is_none() || peripheral.methods.write.is_none() {
            return Err(PyTypeError::new_err(
                "a mapped peripheral needs read and write",
            ));
        }
        Ok(peripheral)
    }

    /// `object` as a model on no pin.
    pub(crate) fn unwired(object: &Bound<'_, PyAny>, pending: Pending) -> PyResult<Self> {
        let method = |name: &str| -> PyResult<Option<Held>> {
            Ok(object
                .getattr_opt(name)?
                .map(|method| pending.keep(&method)))
        };
        Ok(Self {
            object: pending.keep(object),
            methods: Methods {
                read: method("read")?,
                write: method("write")?,
                wait_states: method("wait_states")?,
                interrupt_request: method("interrupt_request")?,
                reset: method("reset")?,
                next_event: method("next_event")?,
                advance: method("advance")?,
            },
            pending,
        })
    }

    /// The object, unless the collector cleared it.
    pub(crate) fn object(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.object.object(py).ok()
    }

    /// Calls `method` with `arguments` and converts its result with
    /// `convert`.  `BusError` is the cycle's bus error; any other
    /// exception (a `KeyboardInterrupt` too) waits in `pending`, asks the
    /// run to stop, and `undriven` answers, the bus not faulted.  `None`
    /// if the method is missing.
    fn call<A, T>(
        &self,
        method: Option<&Held>,
        arguments: A,
        convert: impl FnOnce(&Bound<'_, PyAny>) -> PyResult<T>,
        undriven: T,
    ) -> Option<Result<T, BusError>>
    where
        A: for<'py> PyCallArgs<'py>,
    {
        let method = method?;
        Python::attach(|py| {
            let result = method
                .call1(py, arguments)
                .and_then(|value| convert(value.bind(py)));
            Some(result.or_else(|error| {
                if error.is_instance_of::<exceptions::BusError>(py) {
                    return Err(BusError);
                }
                self.pending.record(error);
                Ok(undriven)
            }))
        })
    }

    /// Calls a method whose failure the trait cannot report: the
    /// exception waits, and the trait's default answers.
    fn call_or<A, T>(
        &self,
        method: Option<&Held>,
        arguments: A,
        convert: impl FnOnce(&Bound<'_, PyAny>) -> PyResult<T>,
        default: T,
    ) -> T
    where
        A: for<'py> PyCallArgs<'py>,
        T: Copy,
    {
        match self.call(method, arguments, convert, default) {
            Some(Ok(value)) => value,
            Some(Err(BusError)) | None => default,
        }
    }

    /// A cycle the peripheral answers by `method`; a missing method is a
    /// bus error.
    fn cycle<A, T>(
        &self,
        method: Option<&Held>,
        arguments: A,
        convert: impl FnOnce(&Bound<'_, PyAny>) -> PyResult<T>,
        undriven: T,
    ) -> Result<T, BusError>
    where
        A: for<'py> PyCallArgs<'py>,
    {
        self.call(method, arguments, convert, undriven)
            .unwrap_or(Err(BusError))
    }
}

/// What a read answers when a Python peripheral's method raises other
/// than `BusError`: its data lanes undriven, which read 0 (PLAN.md,
/// Undriven data lanes; SIMRM 7.3).
pub(crate) const UNDRIVEN: u32 = 0;

/// A read's value, which must fit the cycle.
pub(crate) fn value(value: &Bound<'_, PyAny>, size: Size) -> PyResult<u32> {
    let value: u32 = value.extract()?;
    if value & !size.mask() == 0 {
        Ok(value)
    } else {
        Err(PyValueError::new_err(format!(
            "{value:#x} does not fit a {}-byte cycle",
            size.bytes()
        )))
    }
}

/// A result nothing reads.
const fn ignored(_: &Bound<'_, PyAny>) -> PyResult<()> {
    Ok(())
}

/// A truth value, as Python tests one.
fn truth(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    value.is_truthy()
}

/// An optional time.
fn time(value: &Bound<'_, PyAny>) -> PyResult<Option<Cycles>> {
    Ok(value.extract::<Option<u64>>()?.map(Cycles))
}

impl MappedPeripheral for PythonMapped {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        self.cycle(
            self.methods.read.as_ref(),
            (BusCycleView(cycle),),
            |result| value(result, cycle.size),
            UNDRIVEN,
        )
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        self.cycle(
            self.methods.write.as_ref(),
            (BusCycleView(cycle), value),
            ignored,
            (),
        )
    }

    fn wait_states(&mut self, cycle: BusCycle) -> Option<u8> {
        match self.call(
            self.methods.wait_states.as_ref(),
            (BusCycleView(cycle),),
            |result| result.extract::<Option<u8>>(),
            Some(0),
        ) {
            Some(Ok(wait_states)) => wait_states,
            Some(Err(BusError)) => None,
            None => Some(0),
        }
    }

    fn interrupt_request(&self) -> bool {
        self.call_or(self.methods.interrupt_request.as_ref(), (), truth, false)
    }

    fn reset(&mut self, now: Cycles) {
        self.call_or(self.methods.reset.as_ref(), (now.0,), ignored, ());
    }
}

impl Save for PythonMapped {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        python_save(&self.object, &self.pending, PYTHON_PERIPHERAL)
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        python_restore(&self.object, &self.pending, PYTHON_PERIPHERAL, snapshot)
    }
}

impl Timed for PythonMapped {
    fn next_event(&self) -> Option<Cycles> {
        self.call_or(self.methods.next_event.as_ref(), (), time, None)
    }

    fn advance(&mut self, now: Cycles) {
        self.call_or(self.methods.advance.as_ref(), (now.0,), ignored, ());
    }
}

/// What a Python peripheral's snapshot errors name.
pub(crate) const PYTHON_PERIPHERAL: &str = "a Python peripheral";

/// `object`'s `name`, if it has one and the collector has not cleared
/// it.
pub(crate) fn python_method<'py>(
    py: Python<'py>,
    object: &Held,
    name: &str,
) -> Option<Bound<'py, PyAny>> {
    object.bind(py).ok()?.getattr_opt(name).ok().flatten()
}

/// `object`'s `name`d snapshot method, if it has both `save` and
/// `restore`: one with only one of them cannot save
/// ([`StateError::Unsupported`], naming it `what`), so that it never
/// gives a snapshot it cannot take back.
fn snapshot_method<'py>(
    py: Python<'py>,
    object: &Held,
    name: &str,
    what: &'static str,
) -> Result<Bound<'py, PyAny>, StateError> {
    let both = python_method(py, object, "save").is_some()
        && python_method(py, object, "restore").is_some();
    both.then(|| python_method(py, object, name))
        .flatten()
        .ok_or(StateError::unsupported(what))
}

/// A Python object's state, by its `save() -> bytes`: one without both
/// `save` and `restore` cannot save ([`StateError::Unsupported`],
/// naming it `what`), and an exception `save` raises waits in
/// `pending`.
pub(crate) fn python_save(
    object: &Held,
    pending: &Pending,
    what: &'static str,
) -> Result<Vec<u8>, StateError> {
    Python::attach(|py| {
        snapshot_method(py, object, "save", what)?
            .call0()
            .and_then(|value| value.extract::<Vec<u8>>())
            .map_err(|error| {
                pending.hold(error);
                StateError::unsupported(what)
            })
    })
}

/// A Python object restored by its `restore(bytes)`: one without both
/// `save` and `restore` cannot ([`StateError::Unsupported`]), and an
/// exception it raises (a `KeyboardInterrupt` too) waits in `pending`,
/// the snapshot refused ([`StateError::Mismatch`]).
pub(crate) fn python_restore(
    object: &Held,
    pending: &Pending,
    what: &'static str,
    snapshot: &[u8],
) -> Result<(), StateError> {
    Python::attach(|py| {
        snapshot_method(py, object, "restore", what)?
            .call1((PyBytes::new(py, snapshot),))
            .map(|_| ())
            .map_err(|error| {
                pending.hold(error);
                StateError::mismatch(what)
            })
    })
}
