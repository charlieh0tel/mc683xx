//! The handles by which a Python host's own objects reach a run under
//! way: Rust's `bus::StopRequest`, as the bare `Machine` and the chip's
//! `Builder` give it, and the chip's `SyncRequest` and `Diversion`.

use std::cell::RefCell;
use std::rc::Rc;

use mc683xx_cpu32::bus::StopRequest;
use mc683xx_mc68331::chip::Diversion;
use mc683xx_mc68331::chip::SyncRequest;
use pyo3::pyclass;
use pyo3::pymethods;

use crate::chip::Wiring;
use crate::error::Error;

/// A handle by which the host asks a run to stop (Rust's
/// `bus::StopRequest`): `Machine.stop_request`'s or
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

    fn __repr__(&self) -> &'static str {
        "StopRequest()"
    }
}

/// A handle by which the host's peripherals end the chip's run early
/// (Rust's `chip::SyncRequest`): after the instruction whose cycle asked,
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
/// peripheral and back (Rust's `chip::Diversion`), by the numbers the
/// `Builder` gave them.
#[pyclass(name = "Diversion", module = "mc683xx", unsendable)]
#[derive(Debug, Clone)]
pub(crate) struct DiversionView {
    /// The chip's handle.
    pub(crate) diversion: Diversion,
    /// The numbers, the builder's and then the chip's.
    pub(crate) wiring: Rc<RefCell<Wiring>>,
}

#[pymethods]
impl DiversionView {
    /// Every cycle that reaches memory number `memory` reaches peripheral
    /// number `peripheral` instead, as a cycle on the select that carried
    /// it, until `release`.  Asked during a cycle it takes effect when the
    /// cycle ends; asked between runs, at the next cycle or sync point.
    fn divert(&self, memory: usize, peripheral: usize) -> Result<(), Error> {
        let wiring = self.wiring.borrow();
        self.diversion
            .divert(wiring.memory(memory)?, wiring.peripheral(peripheral)?);
        Ok(())
    }

    /// Memory number `memory`'s cycles reach it again.
    fn release(&self, memory: usize) -> Result<(), Error> {
        self.diversion.release(self.wiring.borrow().memory(memory)?);
        Ok(())
    }

    fn __repr__(&self) -> &'static str {
        "Diversion()"
    }
}
