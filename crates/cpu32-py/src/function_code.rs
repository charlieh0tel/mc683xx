//! `FunctionCode`: a bus cycle's address space, a value a host matches
//! on, so a class (python.md, Names and values).

use pyo3::pyclass;

use mc683xx_target::function_code;

/// The function code FC[2:0] a bus cycle carries, naming its address
/// space (CPU32RM 5.3, Table 5-1; Rust's
/// `mc683xx_target::function_code::FunctionCode`).  `str()` is its
/// Rust name, `supervisor-data`.
#[pyclass(
    module = "mc683xx",
    eq,
    hash,
    frozen,
    from_py_object,
    rename_all = "SCREAMING_SNAKE_CASE"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FunctionCode {
    /// Code 0, reserved (Motorola).
    Reserved0,
    /// User data space.
    UserData,
    /// User program space.
    UserProgram,
    /// Code 3, reserved (user definition).
    Reserved3,
    /// Code 4, reserved (Motorola).
    Reserved4,
    /// Supervisor data space.
    SupervisorData,
    /// Supervisor program space.
    SupervisorProgram,
    /// CPU space.
    CpuSpace,
}

impl From<function_code::FunctionCode> for FunctionCode {
    fn from(function_code: function_code::FunctionCode) -> Self {
        match function_code {
            function_code::FunctionCode::Reserved0 => Self::Reserved0,
            function_code::FunctionCode::UserData => Self::UserData,
            function_code::FunctionCode::UserProgram => Self::UserProgram,
            function_code::FunctionCode::Reserved3 => Self::Reserved3,
            function_code::FunctionCode::Reserved4 => Self::Reserved4,
            function_code::FunctionCode::SupervisorData => Self::SupervisorData,
            function_code::FunctionCode::SupervisorProgram => Self::SupervisorProgram,
            function_code::FunctionCode::CpuSpace => Self::CpuSpace,
        }
    }
}

impl From<FunctionCode> for function_code::FunctionCode {
    fn from(function_code: FunctionCode) -> Self {
        match function_code {
            FunctionCode::Reserved0 => Self::Reserved0,
            FunctionCode::UserData => Self::UserData,
            FunctionCode::UserProgram => Self::UserProgram,
            FunctionCode::Reserved3 => Self::Reserved3,
            FunctionCode::Reserved4 => Self::Reserved4,
            FunctionCode::SupervisorData => Self::SupervisorData,
            FunctionCode::SupervisorProgram => Self::SupervisorProgram,
            FunctionCode::CpuSpace => Self::CpuSpace,
        }
    }
}

enum_names!(FunctionCode, function_code::FunctionCode);
