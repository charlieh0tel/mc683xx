//! The programming model's registers by the manual's names (CPU32RM
//! 2.3): `D0`-`D7`, `A0`-`A7`, `PC`, `SR`, `CCR`, `USP`, `SSP`, `VBR`,
//! `SFC`, `DFC`.

use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::registers::Registers;

use crate::error::Error;

/// A register a host reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Register {
    /// D0-D7.
    Data(usize),
    /// A0-A7; A7 is the stack pointer of the state S selects.
    Address(usize),
    /// The program counter.
    Pc,
    /// The status register.
    Sr,
    /// The condition code register, SR's low byte.
    Ccr,
    /// The user stack pointer.
    Usp,
    /// The supervisor stack pointer.
    Ssp,
    /// The vector base register.
    Vbr,
    /// The source function code register.
    Sfc,
    /// The destination function code register.
    Dfc,
}

/// The registers but D0-D7 and A0-A7.
const OTHERS: [Register; 8] = [
    Register::Pc,
    Register::Sr,
    Register::Ccr,
    Register::Usp,
    Register::Ssp,
    Register::Vbr,
    Register::Sfc,
    Register::Dfc,
];

/// How many data registers and address registers there are.
const NUMBERED: usize = 8;

/// The names of D0-D7.
const DATA_NAMES: [&str; NUMBERED] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];

/// The names of A0-A7.
const ADDRESS_NAMES: [&str; NUMBERED] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6", "A7"];

impl Register {
    /// Every register, D0-D7, A0-A7, then the rest.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        (0..NUMBERED)
            .map(Self::Data)
            .chain((0..NUMBERED).map(Self::Address))
            .chain(OTHERS)
    }

    /// The register named `name`, in any case.
    pub(crate) fn parse(name: &str) -> Result<Self, Error> {
        Self::all()
            .find(|register| register.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::Register(name.to_owned()))
    }

    /// Its name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Data(number) => DATA_NAMES[number],
            Self::Address(number) => ADDRESS_NAMES[number],
            Self::Pc => "PC",
            Self::Sr => "SR",
            Self::Ccr => "CCR",
            Self::Usp => "USP",
            Self::Ssp => "SSP",
            Self::Vbr => "VBR",
            Self::Sfc => "SFC",
            Self::Dfc => "DFC",
        }
    }

    /// The largest value it holds.
    const fn maximum(self) -> u32 {
        match self {
            Self::Sr => u16::MAX as u32,
            Self::Ccr => u8::MAX as u32,
            Self::Sfc | Self::Dfc => FunctionCode::CPU_SPACE.bits() as u32,
            _ => u32::MAX,
        }
    }

    /// Its value in `registers`.
    pub(crate) fn read(self, registers: &Registers) -> u32 {
        match self {
            Self::Data(number) => registers.data[number],
            Self::Address(number) => registers.address(number),
            Self::Pc => registers.pc,
            Self::Sr => registers.sr().into(),
            Self::Ccr => registers.ccr().into(),
            Self::Usp => registers.usp,
            Self::Ssp => registers.ssp,
            Self::Vbr => registers.vbr,
            Self::Sfc => registers.sfc.bits().into(),
            Self::Dfc => registers.dfc.bits().into(),
        }
    }

    /// Sets it in `registers` to `value`, which must fit it.  SR's
    /// unimplemented bits stay clear, as `Registers::set_sr` keeps
    /// them.
    pub(crate) fn write(self, registers: &mut Registers, value: u32) -> Result<(), Error> {
        if value > self.maximum() {
            return Err(Error::Width {
                register: self.name(),
                value,
            });
        }
        match self {
            Self::Data(number) => registers.data[number] = value,
            Self::Address(number) => registers.set_address(number, value),
            Self::Pc => registers.pc = value,
            Self::Sr => registers.set_sr(value as u16),
            Self::Ccr => registers.set_ccr(value as u8),
            Self::Usp => registers.usp = value,
            Self::Ssp => registers.ssp = value,
            Self::Vbr => registers.vbr = value,
            Self::Sfc => registers.sfc = FunctionCode::new(value),
            Self::Dfc => registers.dfc = FunctionCode::new(value),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::registers::Registers;
    use mc683xx_cpu32::registers::status;

    use super::Register;
    use crate::error::Error;

    #[test]
    fn names_go_both_ways() {
        let all: Vec<Register> = Register::all().collect();
        assert_eq!(all.len(), 24);
        for register in all {
            assert_eq!(Register::parse(register.name()).unwrap(), register);
        }
        assert_eq!(Register::parse("a7").unwrap(), Register::Address(7));
        assert!(matches!(Register::parse("D8"), Err(Error::Register(text)) if text == "D8"));
    }

    #[test]
    fn a7_is_the_stack_pointer_of_the_state() {
        let mut registers = Registers::default();
        Register::Sr
            .write(&mut registers, status::S.into())
            .unwrap();
        Register::Address(7).write(&mut registers, 0x1000).unwrap();
        assert_eq!(Register::Ssp.read(&registers), 0x1000);
        assert_eq!(Register::Usp.read(&registers), 0);
    }

    #[test]
    fn values_must_fit() {
        let mut registers = Registers::default();
        assert!(matches!(
            Register::Sr.write(&mut registers, 0x1_0000),
            Err(Error::Width { register, value }) if (register, value) == ("SR", 0x1_0000)
        ));
        assert!(Register::Sfc.write(&mut registers, 8).is_err());
        Register::Dfc.write(&mut registers, 7).unwrap();
        assert_eq!(Register::Dfc.read(&registers), 7);
        Register::Ccr.write(&mut registers, 0xff).unwrap();
        assert_eq!(Register::Sr.read(&registers), u32::from(status::CCR));
        Register::Data(3)
            .write(&mut registers, 0xdead_beef)
            .unwrap();
        Register::Pc.write(&mut registers, 0x400).unwrap();
        Register::Vbr.write(&mut registers, 0x800).unwrap();
        assert_eq!(Register::Data(3).read(&registers), 0xdead_beef);
        assert_eq!(Register::Pc.read(&registers), 0x400);
        assert_eq!(Register::Vbr.read(&registers), 0x800);
    }
}
