//! The Python module `mc683xx`, and the bindings' errors as Python
//! exceptions.

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_mc68331::host::AccessError;
use pyo3::Bound;
use pyo3::PyErr;
use pyo3::PyResult;
use pyo3::exceptions::PyIndexError;
use pyo3::exceptions::PyNotImplementedError;
use pyo3::exceptions::PyOverflowError;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyValueError;
use pyo3::pymodule;
use pyo3::types::PyModule;
use pyo3::types::PyModuleMethods;

use crate::bare::DeviationsView;
use crate::bare::EndView;
use crate::bare::FaultCauseKind;
use crate::bare::FaultStopView;
use crate::bare::InterceptedView;
use crate::bare::LimitedRunView;
use crate::bare::MachineView;
use crate::board::AccessView;
use crate::board::BoardRunView;
use crate::board::BoardStopRequestView;
use crate::board::BoardView;
use crate::board::DriveView;
use crate::board::ExternalView;
use crate::board::LaneView;
use crate::board::ObserverIdView;
use crate::board::RoleView;
use crate::board::SerialView;
use crate::board::TransferView;
use crate::chip::BoardChip;
use crate::chip::Chip;
use crate::chip::ChipBuilder;
use crate::chip::DsackFaultView;
use crate::chip::DsackView;
use crate::chip::Event;
use crate::chip::Halt;
use crate::chip::ResetCause;
use crate::chip::Run;
use crate::chip::Space;
use crate::chip::Stop;
use crate::control::BoardControlView;
use crate::control::PacingView;
use crate::error::Error;
use crate::exceptions;
use crate::exceptions::BusError;
use crate::gdb::Ending;
use crate::hooks::CycleKind;
use crate::hooks::ExceptionView;
use crate::hooks::HookIdView;
use crate::hooks::InstructionView;
use crate::hooks::InvalidAccessView;
use crate::hooks::InvalidReason;
use crate::hooks::MemoryAccessView;
use crate::mapped::BusCycleView;
use crate::peripherals::ChannelView;
use crate::peripherals::Frame;
use crate::peripherals::Output;
use crate::peripherals::OutputChangeView;
use crate::peripherals::PortNameView;
use crate::qspi::ClockPhase;
use crate::qspi::ClockPolarity;
use crate::qspi::QspiClock;
use crate::qspi::QspiTransfer;
use crate::requests::DiversionView;
use crate::requests::StopRequestView;
use crate::requests::SyncRequestView;

impl From<Error> for PyErr {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::Unknown { .. } | Error::Range { .. } => PyIndexError::new_err(message),
            Error::Budget(_) => PyOverflowError::new_err(message),
            Error::Built => PyRuntimeError::new_err(message),
            Error::State(StateError::Unsupported(_)) => PyNotImplementedError::new_err(message),
            Error::State(_) => exceptions::StateError::new_err(message),
            Error::Access(AccessError::BusError | AccessError::Unmapped) | Error::Unmapped(_) => {
                BusError::new_err(message)
            }
            Error::Access(AccessError::Unpeekable) => exceptions::UnpeekableError::new_err(message),
            Error::Select(_)
            | Error::Input(_)
            | Error::Frame(_)
            | Error::Shift { .. }
            | Error::Register(_)
            | Error::Width { .. }
            | Error::Level(_)
            | Error::Clock
            | Error::Frequency(_)
            | Error::Permissions(_)
            | Error::CycleSize(_)
            | Error::PortWidth(_)
            | Error::Length(_)
            | Error::FunctionCode(_)
            | Error::Access(_)
            | Error::Build(_)
            | Error::Memory(_)
            | Error::Map(_) => PyValueError::new_err(message),
        }
    }
}

/// The module.
#[pymodule]
fn mc683xx(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<MachineView>()?;
    module.add_class::<FaultStopView>()?;
    module.add_class::<FaultCauseKind>()?;
    module.add_class::<InterceptedView>()?;
    module.add_class::<DeviationsView>()?;
    module.add_class::<StopRequestView>()?;
    module.add_class::<SyncRequestView>()?;
    module.add_class::<DiversionView>()?;
    module.add_class::<DsackView>()?;
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
    module.add_class::<ResetCause>()?;
    module.add_class::<Event>()?;
    module.add_class::<Ending>()?;
    module.add_class::<Frame>()?;
    module.add_class::<Output>()?;
    module.add_class::<OutputChangeView>()?;
    module.add_class::<ChannelView>()?;
    module.add_class::<PortNameView>()?;
    module.add_class::<QspiClock>()?;
    module.add_class::<QspiTransfer>()?;
    module.add_class::<ClockPolarity>()?;
    module.add_class::<ClockPhase>()?;
    module.add_class::<Space>()?;
    module.add_class::<HookIdView>()?;
    module.add_class::<InstructionView>()?;
    module.add_class::<MemoryAccessView>()?;
    module.add_class::<InvalidAccessView>()?;
    module.add_class::<CycleKind>()?;
    module.add_class::<InvalidReason>()?;
    module.add_class::<ExceptionView>()?;
    module.add_class::<BoardView>()?;
    module.add_class::<AccessView>()?;
    module.add_class::<RoleView>()?;
    module.add_class::<LaneView>()?;
    module.add_class::<TransferView>()?;
    module.add_class::<DriveView>()?;
    module.add_class::<SerialView>()?;
    module.add_class::<ExternalView>()?;
    module.add_class::<ObserverIdView>()?;
    module.add_class::<BoardControlView>()?;
    module.add_class::<BoardStopRequestView>()?;
    module.add_class::<PacingView>()?;
    module.add("BusError", module.py().get_type::<BusError>())?;
    module.add(
        "UnpeekableError",
        module.py().get_type::<exceptions::UnpeekableError>(),
    )?;
    module.add(
        "StateError",
        module.py().get_type::<exceptions::StateError>(),
    )?;
    Ok(())
}
