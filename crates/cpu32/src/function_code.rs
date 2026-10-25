//! Function codes: the address space of each bus cycle.

use serde::Deserialize;
use serde::Deserializer;
use serde::de::Error;
use serde::de::Unexpected;
use std::fmt;

/// The function code FC\[2:0\] a bus cycle carries, naming its address
/// space (CPU32RM 5.3, Table 5-1).  Codes 0, 3 and 4 are reserved;
/// `moves` can still produce them through SFC and DFC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize)]
pub struct FunctionCode(u8);

/// A snapshot's function code, refused beyond FC\[2:0\].
impl<'de> Deserialize<'de> for FunctionCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bits = u8::deserialize(deserializer)?;
        if bits & !Self::MASK == 0 {
            Ok(Self(bits))
        } else {
            Err(D::Error::invalid_value(
                Unexpected::Unsigned(bits.into()),
                &"a function code, 0 to 7",
            ))
        }
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

    /// The function code in the low three bits of `bits`; the rest are
    /// ignored, as SFC and DFC ignore them (CPU32RM 2.3.1.3).
    #[must_use]
    pub const fn new(bits: u32) -> Self {
        Self(bits as u8 & Self::MASK)
    }

    /// FC\[2:0\].
    #[must_use]
    pub const fn bits(self) -> u8 {
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

    /// Whether this is one of the four memory spaces the core's page
    /// tables serve: user and supervisor, data and program.  CPU space,
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

impl From<FunctionCode> for u8 {
    fn from(function_code: FunctionCode) -> Self {
        function_code.0
    }
}

#[cfg(test)]
mod tests {
    use super::FunctionCode;

    #[test]
    fn only_three_bits() {
        assert_eq!(
            FunctionCode::new(0xffff_fffd),
            FunctionCode::SUPERVISOR_DATA
        );
    }

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
            assert!(!FunctionCode::new(code).is_memory_space());
        }
    }

    #[test]
    fn a_function_code_shows_as_its_number() {
        assert_eq!(FunctionCode::SUPERVISOR_DATA.to_string(), "5");
        assert_eq!(u8::from(FunctionCode::CPU_SPACE), 7);
        assert_eq!(super::super::exception::Vector(24).to_string(), "24");
    }
}
