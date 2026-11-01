//! The registers a debugger reads and writes over BDM, numbered as its
//! commands number them (CPU32RM 7.2.8).

use core::fmt;
use core::str::FromStr;

/// A data or address register, numbered as RDREG/RAREG's 4-bit field
/// numbers it: D0-D7 are 0-7, A0-A7 are 8-15 (CPU32RM 7.2.8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Register {
    /// Data register 0.
    D0,
    /// Data register 1.
    D1,
    /// Data register 2.
    D2,
    /// Data register 3.
    D3,
    /// Data register 4.
    D4,
    /// Data register 5.
    D5,
    /// Data register 6.
    D6,
    /// Data register 7.
    D7,
    /// Address register 0.
    A0,
    /// Address register 1.
    A1,
    /// Address register 2.
    A2,
    /// Address register 3.
    A3,
    /// Address register 4.
    A4,
    /// Address register 5.
    A5,
    /// Address register 6.
    A6,
    /// Address register 7, the active stack pointer.
    A7,
}

impl Register {
    /// Every register, in field order: `Register::ALL[n].code() == n`.
    pub const ALL: [Self; 16] = [
        Self::D0,
        Self::D1,
        Self::D2,
        Self::D3,
        Self::D4,
        Self::D5,
        Self::D6,
        Self::D7,
        Self::A0,
        Self::A1,
        Self::A2,
        Self::A3,
        Self::A4,
        Self::A5,
        Self::A6,
        Self::A7,
    ];

    /// The A/D and register fields together, 0-15.
    #[must_use]
    pub const fn code(self) -> u16 {
        self as u16
    }

    /// The register with this code, if it is 0-15.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        if code < Self::ALL.len() as u16 {
            Some(Self::ALL[code as usize])
        } else {
            None
        }
    }

    /// Its name in lower case, `d0` to `a7`.
    const fn name(self) -> &'static str {
        match self {
            Self::D0 => "d0",
            Self::D1 => "d1",
            Self::D2 => "d2",
            Self::D3 => "d3",
            Self::D4 => "d4",
            Self::D5 => "d5",
            Self::D6 => "d6",
            Self::D7 => "d7",
            Self::A0 => "a0",
            Self::A1 => "a1",
            Self::A2 => "a2",
            Self::A3 => "a3",
            Self::A4 => "a4",
            Self::A5 => "a5",
            Self::A6 => "a6",
            Self::A7 => "a7",
        }
    }
}

/// `d0` to `a7`.
impl fmt::Display for Register {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The register [`Register`]'s `Display` names, in any case.
impl FromStr for Register {
    type Err = ParseRegisterError;

    fn from_str(name: &str) -> Result<Self, ParseRegisterError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name().eq_ignore_ascii_case(name))
            .ok_or(ParseRegisterError)
    }
}

/// Text that names no data or address register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a data or address register")]
pub struct ParseRegisterError;

/// A system register, with its RSREG/WSREG select code (CPU32RM
/// 7.2.8.6).  RPC, PCC, ATEMP and FAR exist only in background mode
/// (CPU32RM 7.2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum SystemRegister {
    /// Return program counter: where GO resumes.
    Rpc = 0x0,
    /// Current instruction program counter.
    Pcc = 0x1,
    /// Temporary register A: the BDM entry source, read first after
    /// entry because later commands overwrite it (CPU32RM 7.2.3).
    Atemp = 0x8,
    /// Fault address register.
    Far = 0x9,
    /// Vector base register.
    Vbr = 0xA,
    /// Status register.
    Sr = 0xB,
    /// User stack pointer.
    Usp = 0xC,
    /// Supervisor stack pointer.
    Ssp = 0xD,
    /// Source function code register.
    Sfc = 0xE,
    /// Destination function code register.
    Dfc = 0xF,
}

impl SystemRegister {
    /// Every system register, in select-code order.
    pub const ALL: [Self; 10] = [
        Self::Rpc,
        Self::Pcc,
        Self::Atemp,
        Self::Far,
        Self::Vbr,
        Self::Sr,
        Self::Usp,
        Self::Ssp,
        Self::Sfc,
        Self::Dfc,
    ];

    /// The 4-bit select code.
    #[must_use]
    pub const fn code(self) -> u16 {
        self as u16
    }

    /// The system register with this select code, if there is one.
    #[must_use]
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            0x0 => Some(Self::Rpc),
            0x1 => Some(Self::Pcc),
            0x8 => Some(Self::Atemp),
            0x9 => Some(Self::Far),
            0xA => Some(Self::Vbr),
            0xB => Some(Self::Sr),
            0xC => Some(Self::Usp),
            0xD => Some(Self::Ssp),
            0xE => Some(Self::Sfc),
            0xF => Some(Self::Dfc),
            _ => None,
        }
    }

    /// Its name in lower case, as CPU32RM 7.2.8.6 spells it.
    const fn name(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Pcc => "pcc",
            Self::Atemp => "atemp",
            Self::Far => "far",
            Self::Vbr => "vbr",
            Self::Sr => "sr",
            Self::Usp => "usp",
            Self::Ssp => "ssp",
            Self::Sfc => "sfc",
            Self::Dfc => "dfc",
        }
    }
}

/// `rpc`, `sr` and so on.
impl fmt::Display for SystemRegister {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The system register [`SystemRegister`]'s `Display` names, in any
/// case.
impl FromStr for SystemRegister {
    type Err = ParseSystemRegisterError;

    fn from_str(name: &str) -> Result<Self, ParseSystemRegisterError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name().eq_ignore_ascii_case(name))
            .ok_or(ParseSystemRegisterError)
    }
}

/// Text that names no system register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a system register")]
pub struct ParseSystemRegisterError;

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::ParseRegisterError;
    use super::ParseSystemRegisterError;
    use super::Register;
    use super::SystemRegister;

    #[test]
    fn register_codes_follow_field_order() {
        for (n, register) in Register::ALL.iter().enumerate() {
            assert_eq!(usize::from(register.code()), n);
        }
        assert_eq!(Register::D7.code(), 7);
        assert_eq!(Register::A0.code(), 8);
        assert_eq!(Register::A7.code(), 15);
    }

    #[test]
    fn system_register_codes_are_cpu32rm_select_codes() {
        assert_eq!(SystemRegister::Rpc.code(), 0b0000);
        assert_eq!(SystemRegister::Pcc.code(), 0b0001);
        assert_eq!(SystemRegister::Atemp.code(), 0b1000);
        assert_eq!(SystemRegister::Far.code(), 0b1001);
        assert_eq!(SystemRegister::Vbr.code(), 0b1010);
        assert_eq!(SystemRegister::Sr.code(), 0b1011);
        assert_eq!(SystemRegister::Usp.code(), 0b1100);
        assert_eq!(SystemRegister::Ssp.code(), 0b1101);
        assert_eq!(SystemRegister::Sfc.code(), 0b1110);
        assert_eq!(SystemRegister::Dfc.code(), 0b1111);
    }

    #[test]
    fn codes_round_trip() {
        for register in Register::ALL {
            assert_eq!(Register::from_code(register.code()), Some(register));
        }
        assert_eq!(Register::from_code(16), None);
        for register in SystemRegister::ALL {
            assert_eq!(SystemRegister::from_code(register.code()), Some(register));
        }
        for unused in 2..8 {
            assert_eq!(SystemRegister::from_code(unused), None);
        }
    }

    #[test]
    fn names_round_trip_in_any_case() {
        for register in Register::ALL {
            assert_eq!(register.to_string().parse(), Ok(register));
        }
        assert_eq!("A7".parse(), Ok(Register::A7));
        assert_eq!("d8".parse::<Register>(), Err(ParseRegisterError));
        for register in SystemRegister::ALL {
            assert_eq!(register.to_string().parse(), Ok(register));
        }
        assert_eq!("ATEMP".parse(), Ok(SystemRegister::Atemp));
        assert_eq!(
            "pc".parse::<SystemRegister>(),
            Err(ParseSystemRegisterError)
        );
    }
}
