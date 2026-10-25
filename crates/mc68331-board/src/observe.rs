//! Read-only observers on a board's nets, for a front end.
//!
//! A front end (a TUI, a GUI, a logger) attaches them after loading:
//! board files stay hardware only, and what watches the board is the
//! host's, outside it ([PLAN.md, Boards][plan-boards]).
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::any::Any;
use std::fmt;

use crate::signal::Signal;
use crate::time::Time;

/// What watches a net ([`Board::observe`]): told each change the net
/// carries, at its board time, in the order the net's peripherals see
/// them, after they have.
///
/// It can drive nothing and stop nothing; one
/// that should stop the run holds a [`BoardStopRequest`].  It is not
/// told the net's value as it is attached: [`Board::net_value`] gives
/// that.  A closure `FnMut(&str, Time, Signal)` is one, told nothing of
/// a restore.
///
/// [`Board::observe`]: crate::board::Board::observe
/// [`BoardStopRequest`]: crate::board::BoardStopRequest
/// [`Board::net_value`]: crate::board::Board::net_value
pub trait Observer: Any {
    /// Net `net` carries `signal` from `at`.
    fn changed(&mut self, net: &str, at: Time, signal: Signal);

    /// The board was restored ([`Board::restore`]) to board time `at`,
    /// net `net` carrying `signal` ([`Board::net_value`]; [`None`]
    /// before its first change), so a front end resyncs: told once,
    /// after the restore succeeds.  By default it is ignored.
    ///
    /// [`Board::restore`]: crate::board::Board::restore
    /// [`Board::net_value`]: crate::board::Board::net_value
    #[expect(
        unused_variables,
        reason = "the default ignores its arguments, and the names document them"
    )]
    fn restored(&mut self, net: &str, at: Time, signal: Option<Signal>) {}
}

impl<F: FnMut(&str, Time, Signal) + 'static> Observer for F {
    fn changed(&mut self, net: &str, at: Time, signal: Signal) {
        self(net, at, signal);
    }
}

/// An observer attached to a board, as [`Board::observe`] returns it,
/// for [`Board::unobserve`].
///
/// [`Board::observe`]: crate::board::Board::observe
/// [`Board::unobserve`]: crate::board::Board::unobserve
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObserverId(pub(crate) u64);

/// An attached observer.
pub(crate) struct Attached {
    /// Its id.
    pub(crate) id: ObserverId,
    /// The observer.
    pub(crate) observer: Box<dyn Observer>,
}

impl fmt::Debug for Attached {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Attached")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
