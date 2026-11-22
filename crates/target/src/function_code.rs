//! Function codes: the address space of each bus cycle.

use core::fmt;
use core::str::FromStr;

/// The function code FC\[2:0\] a bus cycle carries, naming its address
/// space (CPU32RM 5.3, Table 5-1).  Codes 0, 3 and 4 are reserved;
/// `moves` can still produce them through SFC and DFC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FunctionCode(u8);

/// A snapshot's function code, refused beyond FC\[2:0\].
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for FunctionCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bits = u8::deserialize(deserializer)?;
        Self::new(bits).ok_or_else(|| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Unsigned(bits.into()),
                &"a function code, 0 to 7",
            )
        })
    }
}

impl FunctionCode {
    /// Bits FC\[2:0\] implement.
    const MASK: u8 = 0b111;
    /// FC2: set in the supervisor spaces and CPU space.
    const SUPERVISOR: u8 = 0b100;

    /// User data space.
    pub const USER_DATA: Self = Self(1);
    /// User program space.
    pub const USER_PROGRAM: Self = Self(2);
    /// Supervisor data space.
    pub const SUPERVISOR_DATA: Self = Self(5);
    /// Supervisor program space.
    pub const SUPERVISOR_PROGRAM: Self = Self(6);
    /// CPU space: interrupt acknowledge, breakpoint acknowledge, the
    /// LPSTOP broadcast (CPU32RM 5.3.1).
    pub const CPU_SPACE: Self = Self(7);

    /// The function code FC\[2:0\] `bits` give, if they are one: 0 to 7.
    #[must_use]
    pub const fn new(bits: u8) -> Option<Self> {
        if bits & !Self::MASK == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// The function code in the low three bits of `value`; the rest are
    /// ignored, as SFC and DFC ignore them (CPU32RM 2.3.1.3).
    #[must_use]
    pub const fn from_low_bits(value: u32) -> Self {
        Self(value as u8 & Self::MASK)
    }

    /// FC\[2:0\], 0 to 7.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Whether FC2 is set: a supervisor space, or CPU space.
    #[must_use]
    pub const fn is_supervisor(self) -> bool {
        self.0 & Self::SUPERVISOR != 0
    }

    /// Whether this is CPU space.
    #[must_use]
    pub const fn is_cpu_space(self) -> bool {
        self.0 == Self::CPU_SPACE.0
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
            Self::USER_DATA | Self::USER_PROGRAM | Self::SUPERVISOR_DATA | Self::SUPERVISOR_PROGRAM
        )
    }
}

/// FC\[2:0\] in decimal, as CPU32RM Table 5-1 numbers the spaces.
impl fmt::Display for FunctionCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// The function code its number, as `Display` writes it, names: `0`
/// to `7`.
impl FromStr for FunctionCode {
    type Err = ParseFunctionCodeError;

    fn from_str(text: &str) -> Result<Self, ParseFunctionCodeError> {
        match text.as_bytes() {
            [digit @ b'0'..=b'7'] => Ok(Self(digit - b'0')),
            _ => Err(ParseFunctionCodeError {}),
        }
    }
}

/// Text that names no function code; without the text, as
/// [`ParseRegisterError`](crate::register::ParseRegisterError).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a function code, 0 to 7")]
#[non_exhaustive]
pub struct ParseFunctionCodeError {}

impl TryFrom<u8> for FunctionCode {
    type Error = NoFunctionCode;

    fn try_from(bits: u8) -> Result<Self, NoFunctionCode> {
        Self::new(bits).ok_or(NoFunctionCode(bits))
    }
}

/// A value past FC\[2:0\]: above 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no function code {0}")]
pub struct NoFunctionCode(pub u8);

impl From<FunctionCode> for u8 {
    fn from(function_code: FunctionCode) -> Self {
        function_code.0
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::FunctionCode;
    use super::NoFunctionCode;
    use super::ParseFunctionCodeError;

    #[test]
    fn only_three_bits() {
        assert_eq!(
            FunctionCode::from_low_bits(0xffff_fffd),
            FunctionCode::SUPERVISOR_DATA
        );
        assert_eq!(FunctionCode::new(7), Some(FunctionCode::CPU_SPACE));
        assert_eq!(FunctionCode::new(8), None);
        assert_eq!(FunctionCode::CPU_SPACE.get(), 7);
        assert_eq!(FunctionCode::try_from(7), Ok(FunctionCode::CPU_SPACE));
        assert_eq!(FunctionCode::try_from(8), Err(NoFunctionCode(8)));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_snapshot_code_has_only_three_bits() {
        assert_eq!(
            postcard::from_bytes::<FunctionCode>(&[7]),
            Ok(FunctionCode::CPU_SPACE)
        );
        assert!(postcard::from_bytes::<FunctionCode>(&[8]).is_err());
    }

    #[test]
    fn spaces() {
        assert!(!FunctionCode::USER_PROGRAM.is_supervisor());
        assert!(FunctionCode::SUPERVISOR_DATA.is_supervisor());
        assert!(FunctionCode::CPU_SPACE.is_cpu_space());
        assert!(FunctionCode::USER_DATA.is_memory_space());
        for code in [0, 3, 4, 7] {
            assert!(!FunctionCode::from_low_bits(code).is_memory_space());
        }
    }

    #[test]
    fn a_function_code_shows_as_its_number() {
        assert_eq!(FunctionCode::SUPERVISOR_DATA.to_string(), "5");
        assert_eq!(u8::from(FunctionCode::CPU_SPACE), 7);
    }

    #[test]
    fn a_function_code_parses_from_its_number() {
        for bits in 0..=7 {
            let code = FunctionCode::from_low_bits(bits);
            assert_eq!(code.to_string().parse(), Ok(code));
        }
        for text in ["8", "", "05", "+5", "-1", "5 "] {
            assert_eq!(text.parse::<FunctionCode>(), Err(ParseFunctionCodeError {}));
        }
    }
}
