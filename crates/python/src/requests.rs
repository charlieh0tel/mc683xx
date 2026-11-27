//! The handles by which a Python host's own objects reach a run under
//! way: Rust's `StopRequest`, as the bare `Machine` and the chip's
//! `Builder` give it, and the chip's `SyncRequest` and `Diversion`.

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

use mc683xx_cpu32::StopRequest;
use mc683xx_mc68331::Diversion;
use mc683xx_mc68331::SyncRequest;
use pyo3::pyclass;
use pyo3::pymethods;

use crate::bare::PeripheralIdView;
use crate::chip::MemoryIdView;
use crate::chip::Wiring;
use crate::error::Error;

/// A handle by which the host asks a run to stop (Rust's
/// `StopRequest`): `Machine.stop_request`'s or
/// `Builder.stop_request`'s.  Clones share one request, which the stop
/// clears.
///
/// The binding asks the same request to stop a run for a Python
/// peripheral's exception, and drops its own once the call it was made
/// in returns ([`settle`](Self::settle)), so only the host's survives
/// between runs.
#[pyclass(name = "StopRequest", module = "mc683xx", unsendable)]
#[derive(Debug, Clone)]
pub(crate) struct StopRequestView {
    /// The request the run takes.
    request: StopRequest,
    /// Whether the host asked it, through this handle.
    host: Rc<Cell<bool>>,
    /// Whether the binding asked it, in the call under way.
    own: Rc<Cell<bool>>,
}

impl StopRequestView {
    /// A handle on `request`, which nobody has asked yet.
    pub(crate) fn new(request: StopRequest) -> Self {
        Self {
            request,
            host: Rc::default(),
            own: Rc::default(),
        }
    }

    /// The binding asks the run to stop, for a peripheral's exception.
    pub(crate) fn ask(&self) {
        self.own.set(true);
        self.request.request();
    }

    /// Once a call into the target returns: the binding's own request
    /// withdrawn unless the host asked too, and the host's forgotten if
    /// a run took it.
    pub(crate) fn settle(&self) {
        if !self.request.pending() {
            self.host.set(false);
        } else if !self.host.get() {
            self.request.withdraw();
        }
        self.own.set(false);
    }
}

#[pymethods]
impl StopRequestView {
    /// Asks the run to stop (`Stop.Requested()`).
    fn request(&self) {
        self.host.set(true);
        self.request.request();
    }

    /// Withdraws a request the run has not taken; the binding's own, for
    /// a peripheral's exception in the call under way, stays.
    fn withdraw(&self) {
        self.host.set(false);
        if !self.own.get() {
            self.request.withdraw();
        }
    }

    /// Whether the host asked for a stop that no run has taken and it
    /// has not withdrawn, the request left as it is.
    #[getter]
    fn pending(&self) -> bool {
        self.host.get() && self.request.pending()
    }

    fn __repr__(&self) -> &'static str {
        "StopRequest()"
    }
}

/// A handle by which the host's peripherals end the chip's run early
/// (Rust's `SyncRequest`): after the instruction whose cycle asked,
/// or at the sync point if none is running, the run returns
/// `Stop.Budget()` with the clocks run so far.  Asked between runs it
/// does nothing.
#[pyclass(name = "SyncRequest", module = "mc683xx", unsendable)]
#[derive(Debug, Clone)]
pub(crate) struct SyncRequestView(pub(crate) SyncRequest);

#[pymethods]
impl SyncRequestView {
    /// Asks the run to end early.
    fn request(&self) {
        self.0.request();
    }

    fn __repr__(&self) -> &'static str {
        "SyncRequest()"
    }
}

/// A handle by which the host's peripherals divert a memory's cycles to a
/// peripheral and back (Rust's `Diversion`), by the ids the `Builder`
/// gave them.
#[pyclass(name = "Diversion", module = "mc683xx", unsendable)]
#[derive(Debug, Clone)]
pub(crate) struct DiversionView {
    /// The chip's handle.
    pub(crate) diversion: Diversion,
    /// The ids, the builder's and then the chip's.
    pub(crate) wiring: Rc<RefCell<Wiring>>,
}

#[pymethods]
impl DiversionView {
    /// Every cycle that reaches memory `memory` reaches peripheral
    /// `peripheral` instead, as a cycle on the select that carried
    /// it, until `release`.  Asked during a cycle it takes effect when the
    /// cycle ends; asked between runs, at the next cycle or sync point.
    fn divert(&self, memory: MemoryIdView, peripheral: PeripheralIdView) -> Result<(), Error> {
        let wiring = self.wiring.borrow();
        self.diversion
            .divert(wiring.memory(memory)?, wiring.peripheral(peripheral)?);
        Ok(())
    }

    /// Memory `memory`'s cycles reach it again.
    fn release(&self, memory: MemoryIdView) -> Result<(), Error> {
        self.diversion.release(self.wiring.borrow().memory(memory)?);
        Ok(())
    }

    fn __repr__(&self) -> &'static str {
        "Diversion()"
    }
}
