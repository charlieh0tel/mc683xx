//! The handles by which a Python host's own objects reach a run under
//! way: Rust's `StopRequest`, as the bare `Machine` and the chip's
//! `Builder` give it, and the chip's `SyncRequest` and `Diversion`.

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
#[pyclass(name = "StopRequest", module = "mc683xx", unsendable)]
#[derive(Debug, Clone)]
pub(crate) struct StopRequestView(pub(crate) StopRequest);

#[pymethods]
impl StopRequestView {
    /// Asks the run to stop (`Stop.Requested()`).
    fn request(&self) {
        self.0.request();
    }

    /// Withdraws a request the run has not taken.
    fn withdraw(&self) {
        self.0.withdraw();
    }

    /// Whether a stop is asked for that no run has taken and the host
    /// has not withdrawn, the request left as it is.
    #[getter]
    fn pending(&self) -> bool {
        self.0.pending()
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
