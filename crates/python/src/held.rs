//! Python objects held in Rust state Python cannot see: a hook's
//! closure in the core, a peripheral inside the chip or board.
//! Each is held in a `Held` slot, and the pyclass that owns the target
//! keeps the slots in `Holdings`, so its `__traverse__` shows them to
//! Python's cycle collector and its `__clear__` drops them: a harness
//! that holds its CPU and hooks it with its own bound methods is
//! collected.

use std::cell::Cell;
use std::cell::RefCell;
use std::mem;
use std::rc::Rc;
use std::rc::Weak;

use pyo3::Bound;
use pyo3::Py;
use pyo3::PyAny;
use pyo3::PyResult;
use pyo3::PyTraverseError;
use pyo3::PyVisit;
use pyo3::Python;
use pyo3::call::PyCallArgs;
use pyo3::exceptions::PyRuntimeError;
use pyo3::types::PyAnyMethods;

/// A slot's object: `None` once `__clear__` has dropped it.
type Slot = RefCell<Option<Py<PyAny>>>;

/// Slots `Holdings` keeps before it first forgets the dropped ones.
const FIRST_PRUNE: usize = 16;

/// A Python object the bindings hold in Rust, in a slot its owner's
/// `Holdings` can visit and clear.  Clones share the slot.
#[derive(Debug, Clone)]
pub(crate) struct Held(Rc<Slot>);

impl Held {
    /// The object, unless the collector cleared it.
    pub(crate) fn bind<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.0
            .borrow()
            .as_ref()
            .map(|object| object.bind(py).clone())
            .ok_or_else(|| PyRuntimeError::new_err("the object was collected"))
    }

    /// The object, as a new reference.
    pub(crate) fn object(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.bind(py).map(Bound::unbind)
    }

    /// Calls the object with `arguments`.  The slot is not borrowed
    /// during the call, so the collector can visit it then.
    pub(crate) fn call1<A>(&self, py: Python<'_>, arguments: A) -> PyResult<Py<PyAny>>
    where
        A: for<'py> PyCallArgs<'py>,
    {
        self.bind(py)?.call1(arguments).map(Bound::unbind)
    }

    /// Calls the object with no arguments.
    pub(crate) fn call0(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.bind(py)?.call0().map(Bound::unbind)
    }
}

/// The slots of the Python objects one target holds, for its owner's
/// `__traverse__` and `__clear__`.  Weak, so a slot whose holder is
/// gone (a removed hook) lets its object go.  Clones share them; only
/// one pyclass at a time visits them, so each reference is reported
/// once.
#[derive(Debug, Default, Clone)]
pub(crate) struct Holdings(Rc<Kept>);

/// `Holdings`' slots.
#[derive(Debug, Default)]
struct Kept {
    /// The slots, the dropped among them until the next prune.
    slots: RefCell<Vec<Weak<Slot>>>,
    /// The count of slots at which the dropped ones are next forgotten:
    /// twice the count kept at the last prune, so holding is amortized
    /// constant time.
    prune_at: Cell<usize>,
}

impl Holdings {
    /// Holds `object` in a new slot.
    pub(crate) fn hold(&self, object: &Bound<'_, PyAny>) -> Held {
        let held = Held(Rc::new(RefCell::new(Some(object.clone().unbind()))));
        let mut slots = self.0.slots.borrow_mut();
        if slots.len() >= self.0.prune_at.get().max(FIRST_PRUNE) {
            slots.retain(|slot| slot.strong_count() > 0);
            self.0.prune_at.set(slots.len() * 2);
        }
        slots.push(Rc::downgrade(&held.0));
        held
    }

    /// Shows `visit` every object held.
    pub(crate) fn traverse(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        let Ok(slots) = self.0.slots.try_borrow() else {
            return Ok(());
        };
        for slot in slots.iter().filter_map(Weak::upgrade) {
            if let Ok(object) = slot.try_borrow() {
                visit.call(object.as_ref())?;
            }
        }
        Ok(())
    }

    /// Drops every object held, once none is borrowed.
    pub(crate) fn clear(&self) {
        let slots = mem::take(&mut *self.0.slots.borrow_mut());
        let objects: Vec<Py<PyAny>> = slots
            .iter()
            .filter_map(Weak::upgrade)
            .filter_map(|slot| slot.borrow_mut().take())
            .collect();
        drop(objects);
    }
}
