//! The programming model's registers by the manual's names (CPU32RM
//! 2.3): `D0`-`D7`, `A0`-`A7`, `PC`, `SR`, `CCR`, `USP`, `SSP`, `VBR`,
//! `SFC`, `DFC`.  D0-A7 and the system registers are the target
//! crate's, by its names in upper case.

use std::fmt;
use std::str::FromStr;

use mc683xx_cpu32::registers::Registers;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register as DataOrAddress;
use mc683xx_target::register::SystemRegister;

use crate::error::Error;

/// A register a host reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Register {
    /// D0-D7, A0-A7; A7 is the stack pointer of the state S selects.
    DataOrAddress(DataOrAddress),
    /// A system register of the programming model: SR, USP, SSP, VBR,
    /// SFC or DFC, never one only background mode has (CPU32RM 7.2.5).
    System(SystemRegister),
    /// The program counter.
    Pc,
    /// The condition code register, SR's low byte.
    Ccr,
}

/// The registers after D0-D7 and A0-A7, in the order a register
/// dictionary lists them.
const OTHERS: [Register; 8] = [
    Register::Pc,
    Register::System(SystemRegister::Sr),
    Register::Ccr,
    Register::System(SystemRegister::Usp),
    Register::System(SystemRegister::Ssp),
    Register::System(SystemRegister::Vbr),
    Register::System(SystemRegister::Sfc),
    Register::System(SystemRegister::Dfc),
];

/// What a system register not of the programming model breaks.
const PROGRAMMING_MODEL: &str = "a system register of the programming model";

impl Register {
    /// Every register, D0-D7, A0-A7, then the rest.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        DataOrAddress::ALL
            .into_iter()
            .map(Self::DataOrAddress)
            .chain(OTHERS)
    }

    /// The largest value it holds.
    const fn maximum(self) -> u32 {
        match self {
            Self::System(SystemRegister::Sr) => u16::MAX as u32,
            Self::Ccr => u8::MAX as u32,
            Self::System(SystemRegister::Sfc | SystemRegister::Dfc) => {
                FunctionCode::CpuSpace as u32
            }
            _ => u32::MAX,
        }
    }

    /// Its value in `registers`.
    pub(crate) fn read(self, registers: &Registers) -> u32 {
        match self {
            Self::DataOrAddress(register) => registers.register(register),
            Self::System(register) => registers
                .system_register(register)
                .expect(PROGRAMMING_MODEL),
            Self::Pc => registers.pc.0,
            Self::Ccr => registers.ccr().into(),
        }
    }

    /// Sets it in `registers` to `value`, which must fit it.  SR's
    /// unimplemented bits stay clear, as `Registers::set_sr` keeps
    /// them.
    pub(crate) fn write(self, registers: &mut Registers, value: u32) -> Result<(), Error> {
        if value > self.maximum() {
            return Err(Error::Width {
                register: self.to_string(),
                value,
            });
        }
        match self {
            Self::DataOrAddress(register) => registers.set_register(register, value),
            Self::System(register) => registers
                .set_system_register(register, value)
                .expect(PROGRAMMING_MODEL),
            Self::Pc => registers.pc = Address(value),
            Self::Ccr => registers.set_ccr(value as u8),
        }
        Ok(())
    }
}

/// Its name in upper case, as the manual writes it: `D0`, `PC`, `SR`.
impl fmt::Display for Register {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::DataOrAddress(register) => register.to_string(),
            Self::System(register) => register.to_string(),
            Self::Pc => "pc".to_owned(),
            Self::Ccr => "ccr".to_owned(),
        };
        formatter.write_str(&name.to_ascii_uppercase())
    }
}

/// The names [`Register`]'s `Display` gives, in any case.
impl FromStr for Register {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self, Error> {
        if let Ok(register) = name.parse() {
            return Ok(Self::DataOrAddress(register));
        }
        let registers = Registers::default();
        match name.parse() {
            Ok(register) if registers.system_register(register).is_some() => {
                return Ok(Self::System(register));
            }
            _ => {}
        }
        [Self::Pc, Self::Ccr]
            .into_iter()
            .find(|register| register.to_string().eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::Register(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::registers::Registers;
    use mc683xx_cpu32::registers::status;
    use mc683xx_target::register::Register as DataOrAddress;
    use mc683xx_target::register::SystemRegister;

    use super::Register;
    use crate::error::Error;

    /// SR.
    const SR: Register = Register::System(SystemRegister::Sr);

    #[test]
    fn names_go_both_ways() {
        let all: Vec<Register> = Register::all().collect();
        assert_eq!(all.len(), 24);
        for register in all {
            assert_eq!(register.to_string().parse::<Register>().unwrap(), register);
        }
        assert_eq!(
            "a7".parse::<Register>().unwrap(),
            Register::DataOrAddress(DataOrAddress::A7)
        );
        assert_eq!(Register::Pc.to_string(), "PC");
        assert_eq!(SR.to_string(), "SR");
        assert!(matches!("D8".parse::<Register>(), Err(Error::Register(text)) if text == "D8"));
        // background mode's own are not the programming model's
        for name in ["RPC", "pcc", "ATEMP", "far"] {
            assert!(matches!(name.parse::<Register>(), Err(Error::Register(_))));
        }
    }

    #[test]
    fn a7_is_the_stack_pointer_of_the_state() {
        let mut registers = Registers::default();
        SR.write(&mut registers, status::S.into()).unwrap();
        Register::DataOrAddress(DataOrAddress::A7)
            .write(&mut registers, 0x1000)
            .unwrap();
        assert_eq!(
            Register::System(SystemRegister::Ssp).read(&registers),
            0x1000
        );
        assert_eq!(Register::System(SystemRegister::Usp).read(&registers), 0);
    }

    #[test]
    fn values_must_fit() {
        let mut registers = Registers::default();
        let sfc = Register::System(SystemRegister::Sfc);
        let dfc = Register::System(SystemRegister::Dfc);
        let vbr = Register::System(SystemRegister::Vbr);
        let d3 = Register::DataOrAddress(DataOrAddress::D3);
        assert!(matches!(
            SR.write(&mut registers, 0x1_0000),
            Err(Error::Width { register, value }) if (register.as_str(), value) == ("SR", 0x1_0000)
        ));
        assert!(sfc.write(&mut registers, 8).is_err());
        dfc.write(&mut registers, 7).unwrap();
        assert_eq!(dfc.read(&registers), 7);
        Register::Ccr.write(&mut registers, 0xff).unwrap();
        assert_eq!(SR.read(&registers), u32::from(status::CCR));
        d3.write(&mut registers, 0xdead_beef).unwrap();
        Register::Pc.write(&mut registers, 0x400).unwrap();
        vbr.write(&mut registers, 0x800).unwrap();
        assert_eq!(d3.read(&registers), 0xdead_beef);
        assert_eq!(Register::Pc.read(&registers), 0x400);
        assert_eq!(vbr.read(&registers), 0x800);
    }
}
