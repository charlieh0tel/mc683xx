//! A board's thread-safe control handle, for a front end whose event
//! loop runs on another thread.
//!
//! It asks the run to stop, queues inputs to the board's run-time
//! endpoints and sets the pacing.  Nothing it does touches the chip or
//! a peripheral from another thread: the run takes what it holds at its
//! sync points, on the board's own thread ([PLAN.md, Boards, Front
//! ends][plan-boards]).
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::collections::BTreeMap;
use std::mem;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::error::SendError;
use crate::pacing::Pacing;
use crate::pacing::PacingError;
use crate::signal::Kind;
use crate::signal::Signal;

/// The board's run-time endpoints that drive nets, by name: each net
/// it drives, by index, with the net's type.  Fixed at load, so a value
/// queued from another thread is checked as it is queued.
#[derive(Debug, Default)]
pub(crate) struct Endpoints(pub(crate) BTreeMap<String, Vec<(usize, Kind)>>);

impl Endpoints {
    /// The nets the endpoint `name` drives that carry `signal`.
    pub(crate) fn carrying(&self, name: &str, signal: &Signal) -> Result<Vec<usize>, SendError> {
        let nets = self
            .0
            .get(name)
            .ok_or_else(|| SendError::Endpoint(name.to_owned()))?;
        let carrying: Vec<usize> = nets
            .iter()
            .filter(|(_, kind)| kind.carries(signal))
            .map(|&(net, _)| net)
            .collect();
        if carrying.is_empty() {
            let mut kinds: Vec<Kind> = nets.iter().map(|&(_, kind)| kind).collect();
            kinds.sort_unstable_by_key(ToString::to_string);
            kinds.dedup();
            return Err(SendError::Kind {
                endpoint: name.to_owned(),
                kinds,
            });
        }
        Ok(carrying)
    }
}

/// What a control holds for the run to take.
#[derive(Debug, Default)]
struct State {
    /// A stop asked, not yet taken.
    stop: bool,
    /// Inputs queued, in order: the endpoint and the value.
    inputs: Vec<(String, Signal)>,
    /// The pacing.
    pacing: Pacing,
}

/// What a control's handles share.
#[derive(Debug, Default)]
struct Inner {
    /// Whether a stop or an input waits: the run's one look a sync
    /// point, without the lock.
    pending: AtomicBool,
    /// The stop, the inputs and the pacing.
    state: Mutex<State>,
    /// Told when a stop is asked or the pacing changes, for a sleeping
    /// paced run.
    changed: Condvar,
    /// The endpoints a value may be queued to.
    endpoints: Endpoints,
}

/// What a run takes from its control at a sync point: whether to stop,
/// and the inputs, in the order queued.
pub(crate) type Taken = (bool, Vec<(String, Signal)>);

/// A board's control handle ([`Board::control`](crate::board::Board::control)).
///
/// It is [`Send`], [`Sync`] and cheap to clone, so another thread (a GUI's
/// event loop) may hold it.  The run takes what it holds at each sync point
/// (the end of each of the chip's quanta, and so of each of a paced run's), on the
/// board's thread: a stop ends the run there
/// ([`Stop::Requested`](mc683xx_cpu32::cpu::Stop::Requested)), as a
/// [`BoardStopRequest`](crate::board::BoardStopRequest)'s does, and the
/// inputs are driven there, at that board time, in the order queued.  So a
/// run is the same for the same inputs at the same board times; an input
/// from a live UI lands at the sync point after it was queued, timed by
/// when the UI sent it.  A stop or input asked between runs is taken at the
/// next run's start.
#[derive(Debug, Clone, Default)]
pub struct BoardControl(Arc<Inner>);

impl BoardControl {
    /// A control for a board whose endpoints are `endpoints`.
    pub(crate) fn new(endpoints: Endpoints) -> Self {
        Self(Arc::new(Inner {
            endpoints,
            ..Inner::default()
        }))
    }

    /// The state, locked.  A thread that panicked holding it left
    /// nothing half done, so a poisoned lock is taken as it is.
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks the run to stop at its next sync point.
    pub fn stop(&self) {
        self.state().stop = true;
        self.0.pending.store(true, Ordering::Release);
        self.0.changed.notify_all();
    }

    /// Whether a stop was asked that no run has taken yet.
    #[must_use]
    pub fn stopping(&self) -> bool {
        self.state().stop
    }

    /// Queues `signal` for the run-time endpoint `name`, as
    /// [`Board::send`](crate::board::Board::send) drives it, at the
    /// run's next sync point.
    ///
    /// # Errors
    ///
    /// As [`Board::send`](crate::board::Board::send), queuing nothing:
    /// [`SendError::Endpoint`] if it drives no net,
    /// [`SendError::Kind`] if none it drives carries `signal`.
    pub fn send(&self, name: &str, signal: Signal) -> Result<(), SendError> {
        self.0.endpoints.carrying(name, &signal)?;
        self.state().inputs.push((name.to_owned(), signal));
        self.0.pending.store(true, Ordering::Release);
        Ok(())
    }

    /// Paces the board's paced runs
    /// ([`Board::run_until_paced`](crate::board::Board::run_until_paced)) from their
    /// next quantum.
    ///
    /// # Errors
    ///
    /// [`PacingError::Factor`] for a factor not finite and above 0,
    /// the pacing left as it was.
    pub fn set_pacing(&self, pacing: Pacing) -> Result<(), PacingError> {
        self.state().pacing = pacing.checked()?;
        self.0.changed.notify_all();
        Ok(())
    }

    /// The pacing, [`Pacing::Unpaced`] until set.
    #[must_use]
    pub fn pacing(&self) -> Pacing {
        self.state().pacing
    }

    /// Sleeps up to `duration`, waking early when a stop is asked or
    /// the pacing changes: a paced run's wait between quanta.
    pub fn sleep(&self, duration: Duration) {
        let state = self.state();
        let pacing = state.pacing;
        drop(
            self.0
                .changed
                .wait_timeout_while(state, duration, |state| {
                    !state.stop && state.pacing == pacing
                })
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    /// The endpoints a value may be sent to.
    pub(crate) fn endpoints(&self) -> &Endpoints {
        &self.0.endpoints
    }

    /// What waits for the run, taken, if anything does.
    pub(crate) fn take(&self) -> Option<Taken> {
        // a plain load first: nothing waits at almost every sync point
        if !self.0.pending.load(Ordering::Relaxed) || !self.0.pending.swap(false, Ordering::Acquire)
        {
            return None;
        }
        let mut state = self.state();
        Some((mem::take(&mut state.stop), mem::take(&mut state.inputs)))
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn a_control_crosses_threads() {
        fn shared<T: Send + Sync + Clone + 'static>() {}
        shared::<BoardControl>();
    }

    #[test]
    fn a_sleep_wakes_for_a_stop() {
        let control = BoardControl::default();
        let stopper = control.clone();
        let thread = thread::spawn(move || stopper.stop());
        // an hour, cut short by the stop
        control.sleep(Duration::from_secs(3600));
        thread.join().unwrap();
        assert!(control.stopping());
        assert_eq!(control.take(), Some((true, Vec::new())));
        assert!(!control.stopping());
        assert_eq!(control.take(), None, "taken once");
    }
}
