//! The CPU32 programming model (CPU32RM 2.1, 2.3.1).

use crate::function_code::FunctionCode;

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
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Registers {
    /// D0-D7.
    pub data: [u32; 8],
    /// A0-A6.
    address: [u32; 7],
    /// The user stack pointer.
    pub usp: u32,
    /// The supervisor stack pointer.
    pub ssp: u32,
    /// The program counter.
    pub pc: u32,
    /// The status register, its unimplemented bits clear.
    sr: u16,
    /// The vector base register.
    pub vbr: u32,
    /// The source function code register, for `moves`.
    pub sfc: FunctionCode,
    /// The destination function code register, for `moves`.
    pub dfc: FunctionCode,
}

impl Registers {
    /// A7's number.
    const STACK_POINTER: usize = 7;
    /// Address registers, A0 to A7.
    pub(crate) const ADDRESS_REGISTERS: usize = Self::STACK_POINTER + 1;

    /// Address register `number`, 0 to 7.
    #[must_use]
    pub fn address(&self, number: usize) -> u32 {
        match number {
            Self::STACK_POINTER if self.is_supervisor() => self.ssp,
            Self::STACK_POINTER => self.usp,
            _ => self.address[number],
        }
    }

    /// Sets address register `number`, 0 to 7.
    pub fn set_address(&mut self, number: usize, value: u32) {
        match number {
            Self::STACK_POINTER if self.is_supervisor() => self.ssp = value,
            Self::STACK_POINTER => self.usp = value,
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

    /// The interrupt priority mask, 0 to 7.
    #[must_use]
    pub const fn interrupt_mask(&self) -> u8 {
        ((self.sr & status::INTERRUPT_MASK) >> status::INTERRUPT_SHIFT) as u8
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
            FunctionCode::SUPERVISOR_PROGRAM
        } else {
            FunctionCode::USER_PROGRAM
        }
    }

    /// The data space of the current state.
    #[must_use]
    pub const fn data_space(&self) -> FunctionCode {
        if self.is_supervisor() {
            FunctionCode::SUPERVISOR_DATA
        } else {
            FunctionCode::USER_DATA
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Registers;
    use super::status;

    #[test]
    fn a7_follows_s() {
        let mut registers = Registers::default();
        registers.set_address(7, 0x100);
        registers.set_sr(status::S);
        registers.set_address(7, 0x200);
        assert_eq!((registers.usp, registers.ssp), (0x100, 0x200));
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
}
