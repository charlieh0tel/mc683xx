//! Effective addresses (CPU32RM 3.4): decoding the six-bit field, its
//! extension words, and reading and writing the operand it names.

use crate::address::Address;
use crate::cpu::Core;
use crate::exception::Exception;
use crate::registers::Registers;
use crate::size::Size;
use crate::timing;
use crate::timing::AddressTable;

/// A7, the stack pointer.
const STACK_POINTER: usize = 7;

/// A three-bit field: a register number, an effective address's mode
/// or register field, an opmode (CPU32RM 3.4, 4.4).
pub(crate) const THREE_BITS: u16 = 0b111;
/// Where the mode starts in a six-bit effective address field (CPU32RM
/// 3.4).
pub(crate) const MODE_SHIFT: u16 = 3;

/// The mode of six-bit effective address field `field`.
pub(crate) const fn mode_of(field: u16) -> u16 {
    field >> MODE_SHIFT & THREE_BITS
}

/// Mode 7's submodes, in the register field (CPU32RM 3.4.3).
mod special {
    /// (xxx).W
    pub(super) const ABSOLUTE_SHORT: u16 = 0b000;
    /// (xxx).L
    pub(super) const ABSOLUTE_LONG: u16 = 0b001;
    /// (d16, PC)
    pub(super) const PC_DISPLACEMENT: u16 = 0b010;
    /// (d8, PC, Xn) and (bd, PC, Xn)
    pub(super) const PC_INDEX: u16 = 0b011;
    /// #data
    pub(super) const IMMEDIATE: u16 = 0b100;
}

/// Effective address modes, the mode field (CPU32RM 3.4).
mod mode {
    /// Dn
    pub(super) const DATA_REGISTER: u16 = 0b000;
    /// An
    pub(super) const ADDRESS_REGISTER: u16 = 0b001;
    /// (An)
    pub(super) const INDIRECT: u16 = 0b010;
    /// (An)+
    pub(super) const POSTINCREMENT: u16 = 0b011;
    /// -(An)
    pub(super) const PREDECREMENT: u16 = 0b100;
    /// (d16, An)
    pub(super) const DISPLACEMENT: u16 = 0b101;
    /// (d8, An, Xn) and (bd, An, Xn)
    pub(super) const INDEX: u16 = 0b110;
    /// The special modes, by register field.
    pub(super) const SPECIAL: u16 = 0b111;
}

/// Effective address kinds: the mode field, the special modes told
/// apart by their register field (CPU32RM 3.4).  A handler takes its
/// operands' kinds as const generics, so its decoding is resolved
/// when it is compiled ([PLAN.md, Interpreter]).
///
/// [PLAN.md, Interpreter]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#interpreter
pub(crate) mod kind {
    use super::mode;
    use super::special;

    /// Dn
    pub(crate) const DATA_REGISTER: u8 = mode::DATA_REGISTER as u8;
    /// An
    pub(crate) const ADDRESS_REGISTER: u8 = mode::ADDRESS_REGISTER as u8;
    /// (An)
    pub(crate) const INDIRECT: u8 = mode::INDIRECT as u8;
    /// (An)+
    pub(crate) const POSTINCREMENT: u8 = mode::POSTINCREMENT as u8;
    /// -(An)
    pub(crate) const PREDECREMENT: u8 = mode::PREDECREMENT as u8;
    /// (d16, An)
    pub(crate) const DISPLACEMENT: u8 = mode::DISPLACEMENT as u8;
    /// (d8, An, Xn) and (bd, An, Xn)
    pub(crate) const INDEX: u8 = mode::INDEX as u8;
    /// (xxx).W, the first special kind.
    pub(crate) const ABSOLUTE_SHORT: u8 = SPECIAL + special::ABSOLUTE_SHORT as u8;
    /// (xxx).L
    pub(crate) const ABSOLUTE_LONG: u8 = SPECIAL + special::ABSOLUTE_LONG as u8;
    /// (d16, PC)
    pub(crate) const PC_DISPLACEMENT: u8 = SPECIAL + special::PC_DISPLACEMENT as u8;
    /// (d8, PC, Xn) and (bd, PC, Xn)
    pub(crate) const PC_INDEX: u8 = SPECIAL + special::PC_INDEX as u8;
    /// #data
    pub(crate) const IMMEDIATE: u8 = SPECIAL + special::IMMEDIATE as u8;
    /// Mode 7 with register field 5 to 7: no mode.
    pub(crate) const NONE: u8 = IMMEDIATE + 1;
    /// Where the special kinds start: mode 7's.
    const SPECIAL: u8 = mode::SPECIAL as u8;

    /// The kind of six-bit field `field` (mode, then register).
    #[must_use]
    pub(crate) const fn of(field: u16) -> u8 {
        let mode = super::mode_of(field);
        if mode != mode::SPECIAL {
            return mode as u8;
        }
        match field & super::THREE_BITS {
            submode @ ..=special::IMMEDIATE => SPECIAL + submode as u8,
            _ => NONE,
        }
    }
}

/// Extension word fields (CPU32RM 3.4.4, Figure 3-2).
mod extension {
    /// D/A: the index is an address register.
    pub(super) const ADDRESS_INDEX: u16 = 1 << 15;
    /// W/L: the index is the whole register, not its low word.
    pub(super) const LONG_INDEX: u16 = 1 << 11;
    /// Where SCALE starts.
    pub(super) const SCALE_SHIFT: u16 = 9;
    /// Where the index register number starts.
    pub(super) const INDEX_SHIFT: u16 = 12;
    /// Set in the full format, clear in the brief.
    pub(super) const FULL: u16 = 1 << 8;
    /// BS: base register suppressed.
    pub(super) const BASE_SUPPRESS: u16 = 1 << 7;
    /// IS: index suppressed.
    pub(super) const INDEX_SUPPRESS: u16 = 1 << 6;
    /// Where BD SIZE starts.
    pub(super) const BASE_DISPLACEMENT_SHIFT: u16 = 4;
    /// BD SIZE 01: no base displacement.
    pub(super) const NULL_DISPLACEMENT: u16 = 0b01;
    /// BD SIZE 10: a word.
    pub(super) const WORD_DISPLACEMENT: u16 = 0b10;
    /// BD SIZE 11: a long word.
    pub(super) const LONG_DISPLACEMENT: u16 = 0b11;
    /// I/IS: memory indirection, which the CPU32 does not have.
    pub(super) const INDIRECT: u16 = 0b111;
    /// Bit 3, zero in the full format.
    pub(super) const RESERVED: u16 = 1 << 3;
}

/// Effective address categories (CPU32RM 3.4.4, Table 3-1), as a bit
/// set an instruction's legal modes are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Category(u8);

impl Category {
    /// Data: a data operand.
    pub(crate) const DATA: Self = Self(1);
    /// Memory: an operand in memory.
    pub(crate) const MEMORY: Self = Self(1 << 1);
    /// Control: an unsized memory operand.
    pub(crate) const CONTROL: Self = Self(1 << 2);
    /// Alterable: a writable operand.
    pub(crate) const ALTERABLE: Self = Self(1 << 3);
    /// No category: a field that is no mode.
    pub(crate) const NONE: Self = Self(0);

    /// Both categories at once: data alterable, for example.
    #[must_use]
    pub(crate) const fn and(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The categories of the six-bit field `field`, none if it is no
    /// mode.  The PC-relative modes are not alterable: CPU32RM 3.4.3.1
    /// to 3.4.3.3 allow them only for reads, whatever Table 3-1's
    /// columns print.
    #[must_use]
    pub(crate) const fn of(field: u16) -> Self {
        let all = Self::DATA
            .and(Self::MEMORY)
            .and(Self::CONTROL)
            .and(Self::ALTERABLE);
        let increment = Self::DATA.and(Self::MEMORY).and(Self::ALTERABLE);
        let program = Self::DATA.and(Self::MEMORY).and(Self::CONTROL);
        match (mode_of(field), field & THREE_BITS) {
            (mode::DATA_REGISTER, _) => Self::DATA.and(Self::ALTERABLE),
            (mode::ADDRESS_REGISTER, _) => Self::ALTERABLE,
            (mode::POSTINCREMENT | mode::PREDECREMENT, _) => increment,
            (mode::INDIRECT | mode::DISPLACEMENT | mode::INDEX, _) => all,
            (mode::SPECIAL, special::ABSOLUTE_SHORT | special::ABSOLUTE_LONG) => all,
            (mode::SPECIAL, special::PC_DISPLACEMENT | special::PC_INDEX) => program,
            (mode::SPECIAL, special::IMMEDIATE) => Self::DATA.and(Self::MEMORY),
            _ => Self::NONE,
        }
    }

    /// Whether field `field` is in every category of `self`.
    #[must_use]
    pub(crate) const fn admits(self, field: u16) -> bool {
        Self::of(field).0 & self.0 == self.0 && Self::of(field).0 != Self::NONE.0
    }
}

/// The operand an effective address names, its address computed and
/// its extension words fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operand {
    /// Dn.
    DataRegister(usize),
    /// An.
    AddressRegister(usize),
    /// An operand in data space.
    Data(Address),
    /// An operand in program space: the PC-relative modes, read only.
    Program(Address),
    /// An immediate operand.
    Immediate(u32),
}

/// The address registers an instruction's (An)+ and -(An) changed,
/// with their values before it: what a type II fault or an exception
/// before execution restores (CPU32RM 6.3.1.2, 6.2.8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Altered {
    /// Register number and first value, in the order changed.  An
    /// instruction has at most two effective addresses.
    registers: [Option<(usize, u32)>; 2],
}

impl Altered {
    /// Records that register `number` held `value` before a change;
    /// only the first change of each register counts.
    fn record(&mut self, number: usize, value: u32) {
        if self
            .registers
            .iter()
            .flatten()
            .any(|&(seen, _)| seen == number)
        {
            return;
        }
        let slot = self.registers.iter_mut().find(|slot| slot.is_none());
        debug_assert!(slot.is_some(), "more than two effective addresses");
        if let Some(slot) = slot {
            *slot = Some((number, value));
        }
    }

    /// Register `number`, which held `value`, as the only one altered.
    #[cfg(test)]
    pub(crate) const fn only(number: usize, value: u32) -> Self {
        Self {
            registers: [Some((number, value)), None],
        }
    }

    /// Whether each register it records is an address register, A0 to
    /// A7: what a snapshot's must be.
    pub(crate) fn is_valid(self) -> bool {
        self.registers
            .iter()
            .flatten()
            .all(|&(number, _)| number < Registers::ADDRESS_REGISTERS)
    }

    /// Puts the registers it records back in `registers`.
    pub(crate) fn undo(self, registers: &mut Registers) {
        for &(number, value) in self.registers.iter().flatten() {
            registers.set_address(number, value);
        }
    }
}

impl Core<'_> {
    /// Restores the address registers the instruction's (An)+ and
    /// -(An) changed.
    pub(crate) fn restore_altered(&mut self) {
        let altered = core::mem::take(&mut self.cpu.altered);
        for &(number, value) in altered.registers.iter().flatten() {
            self.cpu.registers.set_address(number, value);
        }
    }

    /// The operand of six-bit field `field` (mode, then register) for
    /// an operand of `size`: extension words fetched from PC, An
    /// incremented or decremented.
    pub(crate) fn effective_address(
        &mut self,
        field: u16,
        size: Size,
    ) -> Result<Operand, Exception> {
        self.effective_address_of(kind::of(field), field, size, AddressTable::Fetch)
    }

    /// `effective_address` for an instruction whose address the
    /// calculate table charges (CPU32RM 8.3.2): LEA, PEA, JMP, JSR,
    /// MOVEM and the others [`AddressTable::Calculate`] lists.
    pub(crate) fn calculated_address(
        &mut self,
        field: u16,
        size: Size,
    ) -> Result<Operand, Exception> {
        self.effective_address_of(kind::of(field), field, size, AddressTable::Calculate)
    }

    /// `effective_address` for a field of kind `kind`, charged by
    /// `table`: inlined, so a handler that passes its const generic
    /// kind decodes only that kind.
    #[inline(always)]
    pub(crate) fn effective_address_of(
        &mut self,
        kind: u8,
        field: u16,
        size: Size,
        table: AddressTable,
    ) -> Result<Operand, Exception> {
        let register = usize::from(field & THREE_BITS);
        // A7 stays word-aligned (CPU32RM 3.4.2.2, 3.4.2.3).
        let step = if register == STACK_POINTER && size == Size::Byte {
            Size::Word.bytes()
        } else {
            size.bytes()
        };
        let base = self.cpu.registers.address(register);
        let operand = match kind {
            kind::DATA_REGISTER => Operand::DataRegister(register),
            kind::ADDRESS_REGISTER => Operand::AddressRegister(register),
            kind::INDIRECT => Operand::Data(Address(base)),
            kind::POSTINCREMENT => {
                self.cpu.altered.record(register, base);
                self.cpu
                    .registers
                    .set_address(register, base.wrapping_add(step));
                Operand::Data(Address(base))
            }
            kind::PREDECREMENT => {
                self.cpu.altered.record(register, base);
                let address = base.wrapping_sub(step);
                self.cpu.registers.set_address(register, address);
                Operand::Data(Address(address))
            }
            kind::DISPLACEMENT => {
                let displacement = Size::Word.sign_extend(u32::from(self.fetch_word()?));
                Operand::Data(Address(base.wrapping_add(displacement)))
            }
            kind::INDEX => Operand::Data(self.indexed(base, table)?),
            kind::ABSOLUTE_SHORT => Operand::Data(Address(
                Size::Word.sign_extend(u32::from(self.fetch_word()?)),
            )),
            kind::ABSOLUTE_LONG => Operand::Data(Address(self.fetch_long()?)),
            kind::PC_DISPLACEMENT => {
                // PC is the extension word's address (CPU32RM 3.4.3.1).
                let pc = self.cpu.registers.pc;
                let displacement = Size::Word.sign_extend(u32::from(self.fetch_word()?));
                Operand::Program(Address(pc.wrapping_add(displacement)))
            }
            kind::PC_INDEX => {
                let pc = self.cpu.registers.pc;
                Operand::Program(self.indexed(pc, table)?)
            }
            kind::IMMEDIATE => Operand::Immediate(match size {
                // A byte is the extension word's low byte (CPU32RM 3.4.3.6).
                Size::Byte => u32::from(self.fetch_word()?) & Size::Byte.mask(),
                Size::Word => u32::from(self.fetch_word()?),
                Size::Long => self.fetch_long()?,
            }),
            _ => return Err(self.illegal()),
        };
        Ok(operand)
    }

    /// The address of an indexed mode from `base` (An, or PC at the
    /// extension word): the brief format, or the full format without
    /// memory indirection (CPU32RM 3.4.2.5, 3.4.2.6, Figure 3-2).
    fn indexed(&mut self, base: u32, table: AddressTable) -> Result<Address, Exception> {
        let word = self.fetch_word()?;
        let index = {
            let number = usize::from(word >> extension::INDEX_SHIFT & THREE_BITS);
            let register = if word & extension::ADDRESS_INDEX != 0 {
                self.cpu.registers.address(number)
            } else {
                self.cpu.registers.data[number]
            };
            let value = if word & extension::LONG_INDEX != 0 {
                register
            } else {
                Size::Word.sign_extend(register)
            };
            value << (word >> extension::SCALE_SHIFT & 0b11)
        };
        if word & extension::FULL == 0 {
            let displacement = Size::Byte.sign_extend(u32::from(word));
            return Ok(Address(base.wrapping_add(displacement).wrapping_add(index)));
        }
        // Bits 3:0 other than 0000, memory indirection among them, or
        // BD SIZE 00 make an illegal instruction (CPU32RM 6.2.8).
        if word & (extension::RESERVED | extension::INDIRECT) != 0 {
            return Err(self.illegal());
        }
        let displacement = match word >> extension::BASE_DISPLACEMENT_SHIFT & 0b11 {
            extension::NULL_DISPLACEMENT => 0,
            extension::WORD_DISPLACEMENT => Size::Word.sign_extend(u32::from(self.fetch_word()?)),
            extension::LONG_DISPLACEMENT => self.fetch_long()?,
            _ => return Err(self.illegal()),
        };
        self.adjust(timing::full_extension(word, table));
        let base = if word & extension::BASE_SUPPRESS != 0 {
            0
        } else {
            base
        };
        let index = if word & extension::INDEX_SUPPRESS != 0 {
            0
        } else {
            index
        };
        Ok(Address(base.wrapping_add(displacement).wrapping_add(index)))
    }

    /// Reads `operand` as `size`.
    #[inline]
    pub(crate) fn read_operand(&mut self, operand: Operand, size: Size) -> Result<u32, Exception> {
        match operand {
            Operand::DataRegister(number) => Ok(self.cpu.registers.data[number] & size.mask()),
            Operand::AddressRegister(number) => {
                Ok(self.cpu.registers.address(number) & size.mask())
            }
            Operand::Data(address) => self.read_data(address, size),
            Operand::Program(address) => self.read_program(address, size),
            Operand::Immediate(value) => Ok(value),
        }
    }

    /// Writes `value` to `operand` as `size`: a data register's upper
    /// bits kept, an address register's all replaced (MOVEA and the
    /// like sign-extend first).
    #[inline]
    pub(crate) fn write_operand(
        &mut self,
        operand: Operand,
        size: Size,
        value: u32,
    ) -> Result<(), Exception> {
        match operand {
            Operand::DataRegister(number) => {
                let register = &mut self.cpu.registers.data[number];
                *register = *register & !size.mask() | value & size.mask();
                Ok(())
            }
            Operand::AddressRegister(number) => {
                self.cpu.registers.set_address(number, value);
                Ok(())
            }
            Operand::Data(address) => self.write_data(address, size, value),
            Operand::Program(_) | Operand::Immediate(_) => Err(self.illegal()),
        }
    }
}
