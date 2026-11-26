//! The host's request that a run stop.

use std::cell::Cell;
use std::rc::Rc;

/// A handle by which the host asks a run to stop.
///
/// The host's peripherals, or anything else of the host's, stop the run
/// after the instruction whose cycle asked, or, on the chip, at the
/// sync point if no instruction is running
/// ([`Stop::Requested`](crate::cpu::Stop::Requested)).  Clones share
/// one request, which the stop clears.  The bare machine and the chip
/// each give one.
///
/// A request no run has taken stays pending, and ends the next run
/// after its first instruction: one made between runs, or, on the bare
/// machine, one made during a run that ends for another reason at the
/// same boundary (a hook's stop, a fault or an exception stop in the
/// instruction that asked, its own limits), which the chip takes
/// with that stop instead.  A host reusing a machine checks
/// [`pending`](Self::pending) or calls [`withdraw`](Self::withdraw)
/// before its next run.
#[derive(Debug, Clone, Default)]
pub struct StopRequest(Rc<Cell<bool>>);

impl StopRequest {
    /// Asks the run to stop.
    pub fn request(&self) {
        self.0.set(true);
    }

    /// Withdraws a request the run has not taken: for a host whose own
    /// run loop stopped for it first, between runs.
    pub fn withdraw(&self) {
        self.0.set(false);
    }

    /// Whether a stop is asked for that no run has taken and the host
    /// has not withdrawn, the request left as it is.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.0.get()
    }

    /// Whether a stop is asked for, clearing the request: for the run
    /// loop that serves it ([`Bus::stop_requested`](crate::bus::Bus::stop_requested)).
    #[must_use = "a stop taken and dropped is lost"]
    pub fn take(&self) -> bool {
        self.0.replace(false)
    }
}

#[cfg(test)]
mod tests {
    use super::StopRequest;

    #[test]
    fn pending_reads_the_request_and_take_clears_it() {
        let request = StopRequest::default();
        let clone = request.clone();
        assert!(!request.pending());
        clone.request();
        assert!(request.pending());
        assert!(request.pending(), "reading leaves it");
        assert!(request.take());
        assert!(!clone.pending());
        clone.request();
        request.withdraw();
        assert!(!clone.pending());
    }
}
