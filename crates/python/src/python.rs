//! The Python module `mc683xx`, and the bindings' errors as Python
//! exceptions.

use std::fmt;
use std::io;
use std::path::Path;

use mc683xx_board::LoadError;
use mc683xx_cpu32::flat::PeekError;
use mc683xx_cpu32::state::StateError;
use mc683xx_mc68331::BusCycleError;
use mc683xx_target::Address;
use pyo3::Bound;
use pyo3::IntoPyObject;
use pyo3::IntoPyObjectExt;
use pyo3::PyAny;
use pyo3::PyErr;
use pyo3::PyResult;
use pyo3::Python;
use pyo3::exceptions::PyIndexError;
use pyo3::exceptions::PyKeyError;
use pyo3::exceptions::PyNotImplementedError;
use pyo3::exceptions::PyOSError;
use pyo3::exceptions::PyOverflowError;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyTypeError;
use pyo3::exceptions::PyValueError;
use pyo3::pymodule;
use pyo3::types::PyAnyMethods;
use pyo3::types::PyModule;
use pyo3::types::PyModuleMethods;

use crate::bare::EndView;
use crate::bare::FaultCauseKind;
use crate::bare::FaultStopView;
use crate::bare::InterceptedView;
use crate::bare::LimitedRunView;
use crate::bare::MachineView;
use crate::bare::PeripheralIdView;
use crate::board::AccessView;
use crate::board::BoardRunView;
use crate::board::BoardStopRequestView;
use crate::board::BoardView;
use crate::board::DriveView;
use crate::board::ExternalView;
use crate::board::LaneView;
use crate::board::MomentView;
use crate::board::ObserverIdView;
use crate::board::RoleView;
use crate::board::SerialView;
use crate::board::TimeBaseView;
use crate::board::TransferView;
use crate::chip::BoardChip;
use crate::chip::Chip;
use crate::chip::ChipBuilder;
use crate::chip::DsackFaultView;
use crate::chip::DsackView;
use crate::chip::Entry;
use crate::chip::Event;
use crate::chip::Halt;
use crate::chip::MemoryIdView;
use crate::chip::ResetCause;
use crate::chip::Run;
use crate::chip::Space;
use crate::chip::Stop;
use crate::chip::TerminationView;
use crate::control::BoardControlView;
use crate::control::PacingView;
use crate::error::Error;
use crate::error::RUN_END;
use crate::exceptions;
use crate::exceptions::BusError;
use crate::function_code::FunctionCode;
use crate::gdb::Ending;
use crate::hooks::BusCycleKind;
use crate::hooks::ExceptionReturnView;
use crate::hooks::ExceptionView;
use crate::hooks::HookIdView;
use crate::hooks::InstructionView;
use crate::hooks::InvalidAccessView;
use crate::hooks::InvalidReason;
use crate::hooks::MemoryAccessView;
use crate::hooks::PendingReadView;
use crate::mapped::BusCycleView;
use crate::peripherals::ChannelView;
use crate::peripherals::Frame;
use crate::peripherals::InputView;
use crate::peripherals::Output;
use crate::peripherals::PortStateView;
use crate::peripherals::PortView;
use crate::qspi::ClockPhase;
use crate::qspi::ClockPolarity;
use crate::qspi::QspiClock;
use crate::qspi::QspiTransfer;
use crate::requests::DiversionView;
use crate::requests::StopRequestView;
use crate::requests::SyncRequestView;
use crate::vector::VectorNumbers;
use crate::vector::VectorSetView;

impl From<Error> for PyErr {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::Unknown(_) => PyKeyError::new_err(message),
            Error::Range { .. } => PyIndexError::new_err(message),
            Error::Budget(_) => PyOverflowError::new_err(message),
            Error::Built => PyRuntimeError::new_err(message),
            Error::Unmapped { kind: RUN_END, .. } | Error::Run(_) | Error::Take(_) => {
                exceptions::RunError::new_err(message)
            }
            Error::State(StateError::Unsupported(_)) | Error::Unmapped { .. } => {
                PyNotImplementedError::new_err(message)
            }
            Error::State(_) => exceptions::StateError::new_err(message),
            Error::Access(BusCycleError::BusError(_) | BusCycleError::Unserved(_))
            | Error::Unserved(_)
            | Error::Peek(PeekError::Unserved(_)) => BusError::new_err(message),
            Error::Access(BusCycleError::Unpeekable(address))
            | Error::Peek(PeekError::Unpeekable(address)) => unpeekable(message, address),
            Error::Peek(_) => exceptions::UnpeekableError::new_err(message),
            Error::Target(_) => exceptions::TargetError::new_err(message),
            Error::Load(LoadError::Read { path, error } | LoadError::Image { path, error, .. }) => {
                os_error(&path, error)
            }
            Error::Load(_) => exceptions::LoadError::new_err(message),
            Error::Select(_)
            | Error::Frame(_)
            | Error::Shift { .. }
            | Error::Register(_)
            | Error::Width { .. }
            | Error::Level(_)
            | Error::Mask(_)
            | Error::Clock
            | Error::Frequency(_)
            | Error::Permissions(_)
            | Error::CycleSize(_)
            | Error::PortWidth(_)
            | Error::Length(_)
            | Error::Access(_)
            | Error::Build(_)
            | Error::Memory(_)
            | Error::Map(_)
            | Error::Registry(_)
            | Error::Send(_)
            | Error::Receive(_)
            | Error::Net(_)
            | Error::Kind(_)
            | Error::ParsePacing(_)
            | Error::Pacing(_)
            | Error::Inputs(_)
            | Error::Interval(_)
            | Error::NoMemory(_)
            | Error::NoPeripheral(_)
            | Error::Port(_)
            | Error::AnalogInput(_)
            | Error::Seconds(_)
            | Error::SecondsOrPicoseconds
            | Error::Wall(_)
            | Error::Quantum
            | Error::Nesting(_)
            | Error::DsackPort(_)
            | Error::ReadWidth { .. }
            | Error::PeekLength { .. }
            | Error::ShiftedIn { .. } => PyValueError::new_err(message),
        }
    }
}

/// An enum class member's name, as its class names it (`FETCH`): its
/// default `repr` (`BusCycleKind.FETCH`) past the class's name.
pub(crate) fn member_name(member: &Bound<'_, PyAny>) -> PyResult<String> {
    let repr = member.repr()?.to_string();
    Ok(repr
        .rsplit_once('.')
        .map_or_else(|| repr.clone(), |(_, name)| name.to_owned()))
}

/// What `int()` of an enum class's member raises: it is no number.
pub(crate) fn not_a_number() -> PyErr {
    PyTypeError::new_err("an enum member is not a number: use its name")
}

/// An opaque id's `repr`, `class(number)`, from its Rust `Display`,
/// `kind number` (`hook 3`).
pub(crate) fn opaque_repr(class: &str, id: &impl fmt::Display) -> String {
    let shown = id.to_string();
    let number = shown
        .rsplit_once(' ')
        .map_or(shown.as_str(), |(_, number)| number);
    format!("{class}({number})")
}

/// `value`'s Python `repr`, for a class's own: a value as Python
/// writes it, not as Rust's `Debug` does.
pub(crate) fn repr<'py>(py: Python<'py>, value: impl IntoPyObject<'py>) -> PyResult<String> {
    Ok(value.into_bound_py_any(py)?.repr()?.to_string())
}

/// A file `path` that could not be read, a board file or a memory's
/// image, as the `OSError` subclass
/// Python gives the error's errno (`FileNotFoundError` for `ENOENT`),
/// with `filename`; one with no errno as the bindings map its kind.
/// `UnpeekableError(message)`, its `address` the first byte declined's.
fn unpeekable(message: String, address: Address) -> PyErr {
    Python::attach(|py| {
        let error = exceptions::UnpeekableError::new_err(message);
        match error.value(py).setattr("address", address.0) {
            Ok(()) => error,
            Err(raised) => raised,
        }
    })
}

fn os_error(path: &Path, error: io::Error) -> PyErr {
    let Some(errno) = error.raw_os_error() else {
        let message = format!("{}: {error}", path.display());
        return io::Error::new(error.kind(), message).into();
    };
    Python::attach(|py| {
        let strerror = py.import("os")?.getattr("strerror")?.call1((errno,))?;
        Ok(PyOSError::new_err((
            errno,
            strerror.unbind(),
            path.as_os_str().to_os_string(),
        )))
    })
    .unwrap_or_else(|raised: PyErr| raised)
}

/// A Cargo version as PEP 440 spells it, as maturin names the wheel:
/// `0.9.0-alpha.1` is `0.9.0a1` (`-beta.N` `bN`, `-rc.N` `rcN`).
fn pep440_version(cargo: &str) -> String {
    let Some((release, pre)) = cargo.split_once('-') else {
        return cargo.to_owned();
    };
    let pre = [("alpha.", "a"), ("beta.", "b"), ("rc.", "rc")]
        .iter()
        .find_map(|(cargo, pep)| pre.strip_prefix(cargo).map(|n| format!("{pep}{n}")))
        .unwrap_or_else(|| format!("-{pre}"));
    format!("{release}{pre}")
}

/// The module.
#[pymodule]
fn mc683xx(module: &Bound<'_, PyModule>) -> PyResult<()> {
    // the package's version, the workspace's (docs/releasing.md) as the
    // wheel spells it; not in `__all__`, so a star import leaves it
    module.setattr("__version__", pep440_version(env!("CARGO_PKG_VERSION")))?;
    module.add_class::<MachineView>()?;
    module.add_class::<PeripheralIdView>()?;
    module.add_class::<MemoryIdView>()?;
    module.add_class::<FaultStopView>()?;
    module.add_class::<FaultCauseKind>()?;
    module.add_class::<InterceptedView>()?;
    module.add_class::<StopRequestView>()?;
    module.add_class::<SyncRequestView>()?;
    module.add_class::<DiversionView>()?;
    module.add_class::<DsackView>()?;
    module.add_class::<TerminationView>()?;
    module.add_class::<DsackFaultView>()?;
    module.add_class::<ChipBuilder>()?;
    module.add_class::<Chip>()?;
    module.add_class::<BoardChip>()?;
    module.add_class::<BusCycleView>()?;
    module.add_class::<Run>()?;
    module.add_class::<LimitedRunView>()?;
    module.add_class::<EndView>()?;
    module.add_class::<BoardRunView>()?;
    module.add_class::<Stop>()?;
    module.add_class::<Halt>()?;
    module.add_class::<Entry>()?;
    module.add_class::<ResetCause>()?;
    module.add_class::<FunctionCode>()?;
    module.add_class::<Event>()?;
    module.add_class::<Ending>()?;
    module.add_class::<Frame>()?;
    module.add_class::<Output>()?;
    module.add_class::<ChannelView>()?;
    module.add_class::<PortStateView>()?;
    module.add_class::<PortView>()?;
    module.add_class::<InputView>()?;
    module.add_class::<QspiClock>()?;
    module.add_class::<QspiTransfer>()?;
    module.add_class::<ClockPolarity>()?;
    module.add_class::<ClockPhase>()?;
    module.add_class::<Space>()?;
    module.add_class::<HookIdView>()?;
    module.add_class::<VectorNumbers>()?;
    module.add_class::<VectorSetView>()?;
    module.add_class::<InstructionView>()?;
    module.add_class::<MemoryAccessView>()?;
    module.add_class::<PendingReadView>()?;
    module.add_class::<InvalidAccessView>()?;
    module.add_class::<BusCycleKind>()?;
    module.add_class::<InvalidReason>()?;
    module.add_class::<ExceptionView>()?;
    module.add_class::<ExceptionReturnView>()?;
    module.add_class::<BoardView>()?;
    module.add_class::<AccessView>()?;
    module.add_class::<RoleView>()?;
    module.add_class::<LaneView>()?;
    module.add_class::<TransferView>()?;
    module.add_class::<DriveView>()?;
    module.add_class::<MomentView>()?;
    module.add_class::<TimeBaseView>()?;
    module.add_class::<SerialView>()?;
    module.add_class::<ExternalView>()?;
    module.add_class::<ObserverIdView>()?;
    module.add_class::<BoardControlView>()?;
    module.add_class::<BoardStopRequestView>()?;
    module.add_class::<PacingView>()?;
    module.add(
        "BusCycleError",
        module.py().get_type::<exceptions::BusCycleError>(),
    )?;
    module.add("BusError", module.py().get_type::<BusError>())?;
    module.add(
        "UnpeekableError",
        module.py().get_type::<exceptions::UnpeekableError>(),
    )?;
    module.add(
        "StateError",
        module.py().get_type::<exceptions::StateError>(),
    )?;
    module.add("LoadError", module.py().get_type::<exceptions::LoadError>())?;
    module.add("RunError", module.py().get_type::<exceptions::RunError>())?;
    module.add(
        "TargetError",
        module.py().get_type::<exceptions::TargetError>(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::pep440_version;

    #[test]
    fn a_cargo_pre_release_reads_as_pep_440() {
        assert_eq!(pep440_version("0.9.0"), "0.9.0");
        assert_eq!(pep440_version("0.9.0-alpha.1"), "0.9.0a1");
        assert_eq!(pep440_version("1.0.0-beta.2"), "1.0.0b2");
        assert_eq!(pep440_version("1.0.0-rc.3"), "1.0.0rc3");
    }
}
