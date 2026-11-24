//! Function codes: the address space of each bus cycle.

use core::fmt;
use core::str::FromStr;

use crate::integer::parse_integer;

/// The function code FC\[2:0\] a bus cycle carries, naming its address
/// space (CPU32RM 5.3, Table 5-1), each variant's value its code.
/// Codes 0, 3 and 4 are reserved (0 and 4 to Motorola, 3 to the user);
/// `moves` can still produce them through SFC and DFC.
///
/// It converts to and from its code with `u8::from` and
/// `FunctionCode::try_from`, and prints as its name, `supervisor-data`,
/// which [`FromStr`] takes back in any case.  A snapshot holds the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(into = "u8", try_from = "u8")
)]
#[repr(u8)]
pub enum FunctionCode {
    /// Code 0, reserved (Motorola).
    Reserved0 = 0,
    /// User data space.
    UserData = 1,
    /// User program space.
    UserProgram = 2,
    /// Code 3, reserved (user definition).
    Reserved3 = 3,
    /// Code 4, reserved (Motorola).
    Reserved4 = 4,
    /// Supervisor data space.
    SupervisorData = 5,
    /// Supervisor program space.
    SupervisorProgram = 6,
    /// CPU space: interrupt acknowledge, breakpoint acknowledge, the
    /// LPSTOP broadcast (CPU32RM 5.3.1).
    CpuSpace = 7,
}

impl FunctionCode {
    /// Bits FC\[2:0\] implement.
    const MASK: u8 = 0b111;
    /// FC2: set in the supervisor spaces and CPU space.
    const SUPERVISOR: u8 = 0b100;

    /// Every function code, in code order: `ALL[n] as u8 == n`.
    pub const ALL: [Self; 8] = [
        Self::Reserved0,
        Self::UserData,
        Self::UserProgram,
        Self::Reserved3,
        Self::Reserved4,
        Self::SupervisorData,
        Self::SupervisorProgram,
        Self::CpuSpace,
    ];

    /// The function code in the low three bits of `value`; the rest are
    /// ignored, as SFC and DFC ignore them (CPU32RM 2.3.1.3).
    #[must_use]
    pub const fn from_low_bits(value: u32) -> Self {
        Self::ALL[(value & Self::MASK as u32) as usize]
    }

    /// Whether FC2 is set: a supervisor space, or CPU space.
    #[must_use]
    pub const fn is_supervisor(self) -> bool {
        self as u8 & Self::SUPERVISOR != 0
    }

    /// Whether this is one of the four memory spaces: user and
    /// supervisor, data and program.  CPU space,
    /// and the reserved codes 0, 3 and 4 only `moves` produces, are
    /// not (CPU32RM Table 5-1; [PLAN.md, CPU space]).
    ///
    /// [PLAN.md, CPU space]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#cpu-space
    #[must_use]
    pub const fn is_memory_space(self) -> bool {
        matches!(
            self,
            Self::UserData | Self::UserProgram | Self::SupervisorData | Self::SupervisorProgram
        )
    }

    /// Its name, in kebab case.
    const fn name(self) -> &'static str {
        match self {
            Self::Reserved0 => "reserved0",
            Self::UserData => "user-data",
            Self::UserProgram => "user-program",
            Self::Reserved3 => "reserved3",
            Self::Reserved4 => "reserved4",
            Self::SupervisorData => "supervisor-data",
            Self::SupervisorProgram => "supervisor-program",
            Self::CpuSpace => "cpu-space",
        }
    }
}

/// `user-data`, `supervisor-program`, `cpu-space`, `reserved3` and so
/// on.
impl fmt::Display for FunctionCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The function code named so, as [`FunctionCode`]'s `Display` writes
/// it, in any case, or its code, 0 to 7, as [`parse_integer`] takes a
/// number: as a person types one to a command.
///
/// ```
/// use mc683xx_target::function_code::FunctionCode;
///
/// assert_eq!("cpu-space".parse(), Ok(FunctionCode::CpuSpace));
/// assert_eq!("0b101".parse(), Ok(FunctionCode::SupervisorData));
/// assert!("8".parse::<FunctionCode>().is_err());
/// ```
impl FromStr for FunctionCode {
    type Err = ParseFunctionCodeError;

    fn from_str(text: &str) -> Result<Self, ParseFunctionCodeError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name().eq_ignore_ascii_case(text))
            .or_else(|| {
                parse_integer::<u8>(text)
                    .ok()
                    .and_then(|code| Self::try_from(code).ok())
            })
            .ok_or(ParseFunctionCodeError {})
    }
}

/// Text that names no function code; without the text, as
/// [`ParseRegisterError`](crate::register::ParseRegisterError).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a function code")]
#[non_exhaustive]
pub struct ParseFunctionCodeError {}

impl TryFrom<u8> for FunctionCode {
    type Error = NoFunctionCode;

    fn try_from(code: u8) -> Result<Self, NoFunctionCode> {
        Self::ALL
            .get(usize::from(code))
            .copied()
            .ok_or(NoFunctionCode(code))
    }
}

/// A value past FC\[2:0\]: above 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no function code {0}: FC[2:0] is 0 to 7")]
pub struct NoFunctionCode(pub u8);

impl From<FunctionCode> for u8 {
    fn from(function_code: FunctionCode) -> Self {
        function_code as Self
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::FunctionCode;
    use super::NoFunctionCode;
    use super::ParseFunctionCodeError;

    #[test]
    fn codes_follow_table_5_1() {
        for (code, function_code) in (0..).zip(FunctionCode::ALL) {
            assert_eq!(u8::from(function_code), code);
            assert_eq!(FunctionCode::try_from(code), Ok(function_code));
        }
        assert_eq!(u8::from(FunctionCode::UserData), 1);
        assert_eq!(u8::from(FunctionCode::SupervisorData), 5);
        assert_eq!(u8::from(FunctionCode::CpuSpace), 7);
        assert_eq!(FunctionCode::try_from(8), Err(NoFunctionCode(8)));
    }

    #[test]
    fn only_three_bits() {
        assert_eq!(
            FunctionCode::from_low_bits(0xffff_fffd),
            FunctionCode::SupervisorData
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_snapshot_holds_the_code() {
        assert_eq!(postcard::to_allocvec(&FunctionCode::CpuSpace).unwrap(), [7]);
        assert_eq!(
            postcard::from_bytes::<FunctionCode>(&[7]),
            Ok(FunctionCode::CpuSpace)
        );
        assert!(postcard::from_bytes::<FunctionCode>(&[8]).is_err());
    }

    #[test]
    fn spaces() {
        assert!(!FunctionCode::UserProgram.is_supervisor());
        assert!(FunctionCode::SupervisorData.is_supervisor());
        assert!(FunctionCode::CpuSpace.is_supervisor());
        assert!(FunctionCode::UserData.is_memory_space());
        for code in [
            FunctionCode::Reserved0,
            FunctionCode::Reserved3,
            FunctionCode::Reserved4,
            FunctionCode::CpuSpace,
        ] {
            assert!(!code.is_memory_space());
        }
    }

    #[test]
    fn a_function_code_shows_as_its_name() {
        assert_eq!(FunctionCode::SupervisorData.to_string(), "supervisor-data");
        assert_eq!(FunctionCode::CpuSpace.to_string(), "cpu-space");
        assert_eq!(FunctionCode::Reserved3.to_string(), "reserved3");
    }

    #[test]
    fn a_function_code_parses_from_its_name_in_any_case_or_its_number() {
        for code in FunctionCode::ALL {
            assert_eq!(code.to_string().parse(), Ok(code));
            assert_eq!(u8::from(code).to_string().parse(), Ok(code));
        }
        assert_eq!("User-Program".parse(), Ok(FunctionCode::UserProgram));
        assert_eq!("0x6".parse(), Ok(FunctionCode::SupervisorProgram));
        for text in [
            "8",
            "05",
            "$5",
            "-1",
            "",
            "supervisor_data",
            "supervisor data",
            "cpu-space ",
        ] {
            assert_eq!(
                text.parse::<FunctionCode>(),
                Err(ParseFunctionCodeError {}),
                "{text}"
            );
        }
    }
}
