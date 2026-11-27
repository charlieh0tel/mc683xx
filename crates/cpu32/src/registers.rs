//! The CPU32 programming model (CPU32RM 2.1, 2.3.1).

use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Register;
use mc683xx_target::SystemRegister;

use crate::interrupt::InterruptLevel;

/// Status register bits (CPU32RM 2.3.1.3, Figure 2-3).
pub mod status {
    /// T1: trace on every instruction.
    pub const T1: u16 = 1 << 15;
    /// T0: trace on change of flow.
    pub const T0: u16 = 1 << 14;
    /// S: supervisor state.
    pub const S: u16 = 1 << 13;
    /// I\[2:0\]: the interrupt priority mask.
    pub const INTERRUPT_MASK: u16 = 0b111 << INTERRUPT_SHIFT;
    /// Where the interrupt mask starts.
    pub const INTERRUPT_SHIFT: u16 = 8;
    /// X: extend.
    pub const X: u16 = 1 << 4;
    /// N: negative.
    pub const N: u16 = 1 << 3;
    /// Z: zero.
    pub const Z: u16 = 1 << 2;
    /// V: overflow.
    pub const V: u16 = 1 << 1;
    /// C: carry.
    pub const C: u16 = 1;
    /// The condition code register, the low byte.
    pub const CCR: u16 = X | N | Z | V | C;
    /// The 11 bits defined; the rest read as zero (CPU32RM 2.3.1.3).
    pub const IMPLEMENTED: u16 = T1 | T0 | S | INTERRUPT_MASK | CCR;
}

/// The registers of the programming model.  A7 is the stack pointer
/// of the state S selects: USP in user state, SSP in supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Registers {
    /// D0-D7.
    pub data: [u32; 8],
    /// A0-A6.
    address: [u32; 7],
    /// The user stack pointer.
    pub usp: Address,
    /// The supervisor stack pointer.
    pub ssp: Address,
    /// The program counter.
    pub pc: Address,
    /// The status register, its unimplemented bits clear.
    sr: u16,
    /// The vector base register.
    pub vbr: Address,
    /// The source function code register, for `moves`.
    pub sfc: FunctionCode,
    /// The destination function code register, for `moves`.
    pub dfc: FunctionCode,
}

/// Every register zero, SFC and DFC code 0 among them: reset leaves
/// SFC and DFC undefined (CPU32RM 6.2.1 lists what it sets), so a new
/// CPU's 0 is our choice ([PLAN.md, Where the manuals are silent]).
///
/// [PLAN.md, Where the manuals are silent]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#where-the-manuals-are-silent
impl Default for Registers {
    fn default() -> Self {
        Self {
            data: [0; 8],
            address: [0; 7],
            usp: Address(0),
            ssp: Address(0),
            pc: Address(0),
            sr: 0,
            vbr: Address(0),
            sfc: FunctionCode::Reserved0,
            dfc: FunctionCode::Reserved0,
        }
    }
}

/// A system register [`Registers`] lacks: RPC, PCC, ATEMP and FAR exist
/// only in background mode (CPU32RM 7.2.5), where the CPU holds them
/// ([`Cpu::pcc`](crate::cpu::Cpu::pcc), [`atemp`](crate::cpu::Cpu::atemp),
/// [`far`](crate::cpu::Cpu::far)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0} exists only in background mode")]
pub struct BackgroundOnly(pub SystemRegister);

impl Registers {
    /// A7's number.
    const STACK_POINTER: usize = 7;
    /// Address registers, A0 to A7.
    pub(crate) const ADDRESS_REGISTERS: usize = Self::STACK_POINTER + 1;
    /// Data registers, D0 to D7, numbered before A0 (CPU32RM 7.2.8.4).
    const DATA_REGISTERS: usize = 8;

    /// Data or address register `register`; A7 is the stack pointer S
    /// selects.
    #[must_use]
    pub const fn register(&self, register: Register) -> u32 {
        let number = register.code() as usize;
        if number < Self::DATA_REGISTERS {
            self.data[number]
        } else {
            self.address(number - Self::DATA_REGISTERS)
        }
    }

    /// Sets data or address register `register`, all 32 bits; A7 is the
    /// stack pointer S selects.
    pub const fn set_register(&mut self, register: Register, value: u32) {
        let number = register.code() as usize;
        if number < Self::DATA_REGISTERS {
            self.data[number] = value;
        } else {
            self.set_address(number - Self::DATA_REGISTERS, value);
        }
    }

    /// System register `register`, zero-extended, or [`None`] for RPC,
    /// PCC, ATEMP and FAR, which exist only in background mode (CPU32RM
    /// 7.2.5): the PC is [`pc`](Self::pc).
    #[must_use]
    pub const fn system_register(&self, register: SystemRegister) -> Option<u32> {
        Some(match register {
            SystemRegister::Vbr => self.vbr.0,
            SystemRegister::Sr => self.sr as u32,
            SystemRegister::Usp => self.usp.0,
            SystemRegister::Ssp => self.ssp.0,
            SystemRegister::Sfc => self.sfc as u32,
            SystemRegister::Dfc => self.dfc as u32,
            SystemRegister::Rpc
            | SystemRegister::Pcc
            | SystemRegister::Atemp
            | SystemRegister::Far => return None,
        })
    }

    /// Sets system register `register` as WSREG does: a register
    /// narrower than 32 bits takes the low bits (CPU32RM 7.2.8.7), SR's
    /// unimplemented bits staying clear as [`set_sr`](Self::set_sr)
    /// keeps them.
    ///
    /// # Errors
    ///
    /// [`BackgroundOnly`] for RPC, PCC, ATEMP and FAR, which exist only
    /// in background mode (CPU32RM 7.2.5); nothing is set.
    pub const fn set_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), BackgroundOnly> {
        match register {
            SystemRegister::Vbr => self.vbr = Address(value),
            SystemRegister::Sr => self.set_sr(value as u16),
            SystemRegister::Usp => self.usp = Address(value),
            SystemRegister::Ssp => self.ssp = Address(value),
            SystemRegister::Sfc => self.sfc = FunctionCode::from_low_bits(value),
            SystemRegister::Dfc => self.dfc = FunctionCode::from_low_bits(value),
            SystemRegister::Rpc
            | SystemRegister::Pcc
            | SystemRegister::Atemp
            | SystemRegister::Far => return Err(BackgroundOnly(register)),
        }
        Ok(())
    }

    /// Address register `number`, 0 to 7, as an instruction's register
    /// field numbers it.
    #[must_use]
    pub(crate) const fn address(&self, number: usize) -> u32 {
        match number {
            Self::STACK_POINTER if self.is_supervisor() => self.ssp.0,
            Self::STACK_POINTER => self.usp.0,
            _ => self.address[number],
        }
    }

    /// Sets address register `number`, 0 to 7.
    pub(crate) const fn set_address(&mut self, number: usize, value: u32) {
        match number {
            Self::STACK_POINTER if self.is_supervisor() => self.ssp = Address(value),
            Self::STACK_POINTER => self.usp = Address(value),
            _ => self.address[number] = value,
        }
    }

    /// The status register.
    #[must_use]
    pub const fn sr(&self) -> u16 {
        self.sr
    }

    /// Sets every bit of the status register, as no instruction can:
    /// for a snapshot no run could save.
    #[cfg(test)]
    pub(crate) const fn set_sr_unmasked(&mut self, value: u16) {
        self.sr = value;
    }

    /// Sets the status register; unimplemented bits stay clear.  A7
    /// follows S.
    pub const fn set_sr(&mut self, value: u16) {
        self.sr = value & status::IMPLEMENTED;
    }

    /// The condition code register.
    #[must_use]
    pub const fn ccr(&self) -> u8 {
        self.sr as u8
    }

    /// Sets the condition code register, the upper byte unchanged.
    pub const fn set_ccr(&mut self, value: u8) {
        self.set_sr(self.sr & !status::CCR | value as u16);
    }

    /// Whether S is set.
    #[must_use]
    pub const fn is_supervisor(&self) -> bool {
        self.sr & status::S != 0
    }

    /// The interrupt priority mask.
    #[must_use]
    pub const fn interrupt_mask(&self) -> InterruptLevel {
        InterruptLevel::from_low_bits(
            ((self.sr & status::INTERRUPT_MASK) >> status::INTERRUPT_SHIFT) as u32,
        )
    }

    /// Whether condition code `flag` is set.
    #[must_use]
    pub const fn flag(&self, flag: u16) -> bool {
        self.sr & flag != 0
    }

    /// Sets or clears condition codes `flags`.
    pub const fn set_flag(&mut self, flags: u16, on: bool) {
        if on {
            self.sr |= flags & status::CCR;
        } else {
            self.sr &= !(flags & status::CCR);
        }
    }

    /// The program space of the current state, for fetches.
    #[must_use]
    pub const fn program_space(&self) -> FunctionCode {
        if self.is_supervisor() {
            FunctionCode::SupervisorProgram
        } else {
            FunctionCode::UserProgram
        }
    }

    /// The data space of the current state.
    #[must_use]
    pub const fn data_space(&self) -> FunctionCode {
        if self.is_supervisor() {
            FunctionCode::SupervisorData
        } else {
            FunctionCode::UserData
        }
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_target::Address;
    use mc683xx_target::FunctionCode;
    use mc683xx_target::Register;
    use mc683xx_target::SystemRegister;

    use super::BackgroundOnly;
    use super::Registers;
    use super::status;

    #[test]
    fn a7_follows_s() {
        let mut registers = Registers::default();
        registers.set_address(7, 0x100);
        registers.set_sr(status::S);
        registers.set_address(7, 0x200);
        assert_eq!(
            (registers.usp, registers.ssp),
            (Address(0x100), Address(0x200))
        );
        assert_eq!(registers.address(7), 0x200);
    }

    #[test]
    fn unimplemented_sr_bits_read_zero() {
        let mut registers = Registers::default();
        registers.set_sr(0xffff);
        assert_eq!(registers.sr(), status::IMPLEMENTED);
        registers.set_ccr(0);
        assert_eq!(registers.sr(), status::IMPLEMENTED & !status::CCR);
    }

    #[test]
    fn registers_by_name() {
        let mut registers = Registers::default();
        for (n, register) in Register::ALL.into_iter().enumerate() {
            registers.set_register(register, n as u32 + 1);
        }
        assert_eq!(registers.data[7], 8);
        assert_eq!(registers.address(0), 9);
        assert_eq!(registers.register(Register::D3), 4);
        assert_eq!(registers.register(Register::A6), 15);
        // A7 is USP in user state
        assert_eq!(registers.usp, Address(16));
        registers.set_sr(status::S);
        registers.set_register(Register::A7, 0x200);
        assert_eq!(registers.ssp, Address(0x200));
        assert_eq!(registers.register(Register::A7), 0x200);
    }

    #[test]
    fn system_registers_by_name() {
        let mut registers = Registers::default();
        let set = |registers: &mut Registers, register, value| {
            registers.set_system_register(register, value).unwrap();
        };
        set(&mut registers, SystemRegister::Vbr, 0x1000);
        set(&mut registers, SystemRegister::Usp, 0x2000);
        set(&mut registers, SystemRegister::Ssp, 0x3000);
        // WSREG takes the low bits of a narrower register (CPU32RM 7.2.8.7)
        set(&mut registers, SystemRegister::Sfc, 0xffff_fff5);
        set(&mut registers, SystemRegister::Dfc, 1);
        set(&mut registers, SystemRegister::Sr, 0xffff_ffff);
        assert_eq!(registers.vbr, Address(0x1000));
        assert_eq!(registers.sfc, FunctionCode::SupervisorData);
        assert_eq!(registers.dfc, FunctionCode::UserData);
        assert_eq!(registers.sr(), status::IMPLEMENTED);
        assert_eq!(
            [
                SystemRegister::Vbr,
                SystemRegister::Sr,
                SystemRegister::Usp,
                SystemRegister::Ssp,
                SystemRegister::Sfc,
                SystemRegister::Dfc,
            ]
            .map(|register| registers.system_register(register)),
            [
                Some(0x1000),
                Some(u32::from(status::IMPLEMENTED)),
                Some(0x2000),
                Some(0x3000),
                Some(5),
                Some(1),
            ]
        );
    }

    #[test]
    fn background_registers_are_not_the_programming_model_s() {
        let mut registers = Registers::default();
        for register in [
            SystemRegister::Rpc,
            SystemRegister::Pcc,
            SystemRegister::Atemp,
            SystemRegister::Far,
        ] {
            assert_eq!(registers.system_register(register), None);
            assert_eq!(
                registers.set_system_register(register, 1),
                Err(BackgroundOnly(register))
            );
        }
        assert_eq!(registers, Registers::default());
        assert_eq!(
            BackgroundOnly(SystemRegister::Rpc).to_string(),
            "rpc exists only in background mode"
        );
    }
}
