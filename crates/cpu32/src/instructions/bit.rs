//! Bit manipulation instructions (CPU32RM 4.3.6): BTST, BCHG, BCLR and
//! BSET, the bit number in a register or in a second word.

use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::kind;
use crate::effective_address::mode_of;
use crate::exception::Exception;
use crate::instructions::Flow;
use crate::instructions::Handler;
use crate::instructions::Table;
use crate::instructions::data_movement::register_field;
use crate::registers::status;
use crate::size::Size;
use crate::timing::AddressTable;

/// The dynamic forms: 0000 rrr1 tt <ea> (CPU32RM 4.4, BTST and the
/// rest).  Mode 001, An, is MOVEP's.
const DYNAMIC_MASK: u16 = 0xf100;
/// The dynamic forms' fixed bits.
const DYNAMIC: u16 = 0x0100;
/// The static forms: 0000 1000 tt <ea>, the bit number in a second
/// word.
const STATIC_MASK: u16 = 0xff00;
/// The static forms' fixed bits.
const STATIC: u16 = 0x0800;
/// Where the type field, bits 7:6, starts.
const TYPE_SHIFT: u16 = 6;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;
/// The mode field of An.
const ADDRESS_REGISTER_MODE: u16 = 0b001;
/// The six-bit field of #data.
const IMMEDIATE_FIELD: u16 = 0b111_100;

/// The four operations, by the type field: 00 BTST, 01 BCHG, 10 BCLR,
/// 11 BSET.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    /// BTST: test only.
    Test,
    /// BCHG: invert.
    Change,
    /// BCLR: clear.
    Clear,
    /// BSET: set.
    Set,
}

impl Operation {
    /// The operation of `opcode`'s type field.
    const fn of(opcode: u16) -> Self {
        Self::of_type(opcode >> TYPE_SHIFT)
    }

    /// The operation of type field `bits`.
    const fn of_type(bits: u16) -> Self {
        match bits & 0b11 {
            0b00 => Self::Test,
            0b01 => Self::Change,
            0b10 => Self::Clear,
            _ => Self::Set,
        }
    }

    /// The effective addresses it takes: BTST any data operand, the
    /// others a data alterable one.
    const fn category(self) -> Category {
        match self {
            Self::Test => Category::DATA,
            _ => Category::DATA.and(Category::ALTERABLE),
        }
    }

    /// `value` with bit `bit` tested and changed.
    const fn apply(self, value: u32, bit: u32) -> u32 {
        let mask = 1 << bit;
        match self {
            Self::Test => value,
            Self::Change => value ^ mask,
            Self::Clear => value & !mask,
            Self::Set => value | mask,
        }
    }
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set_by(
        DYNAMIC_MASK,
        DYNAMIC,
        |opcode| {
            mode_of(opcode) != ADDRESS_REGISTER_MODE
                && Operation::of(opcode).category().admits(opcode & FIELD_MASK)
        },
        handler::<false>,
    );
    table.set_by(
        STATIC_MASK,
        STATIC,
        |opcode| {
            opcode & FIELD_MASK != IMMEDIATE_FIELD
                && Operation::of(opcode).category().admits(opcode & FIELD_MASK)
        },
        handler::<true>,
    );
}

/// [`operate`]'s handler for `opcode`, specialized by operation and
/// operand kind; [`STATIC`] the static forms.
fn handler<const STATIC: bool>(opcode: u16) -> Handler {
    specialize_two_bits!(opcode >> TYPE_SHIFT, TYPE =>
        specialize_data!(kind::of(opcode & FIELD_MASK), OPERAND =>
            operate::<STATIC, TYPE, OPERAND>))
}

/// BTST, BCHG, BCLR, BSET Dn, <ea> and #number, <ea>: the bit number
/// in a data register or, [`STATIC`], in the low byte of a second word,
/// which comes before the effective address's words.  Tests that bit of
/// the destination into Z and changes it: modulo 32 in a data
/// register, which is a long; modulo 8 in memory, a byte (CPU32RM 4.4,
/// BTST).  `TYPE` is the type field, `OPERAND` the operand's kind.
fn operate<const STATIC: bool, const TYPE: u16, const OPERAND: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let bit = if STATIC {
        u32::from(core.fetch_word()?)
    } else {
        core.cpu.registers.data[register_field(opcode)]
    };
    let operation = const { Operation::of_type(TYPE) };
    let size = if OPERAND == kind::DATA_REGISTER {
        Size::Long
    } else {
        Size::Byte
    };
    let bit = bit % (size.bytes() * u8::BITS);
    let operand =
        core.effective_address_of(OPERAND, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    let value = core.read_operand(operand, size)?;
    core.cpu
        .registers
        .set_flag(status::Z, value & 1 << bit == 0);
    if operation != Operation::Test {
        core.write_operand(operand, size, operation.apply(value, bit))?;
    }
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::registers::status;

    #[test]
    fn btst_register_is_modulo_32_and_touches_only_z() {
        // BTST D1, D0: bit 33 is bit 1
        let mut cpu = machine(&[0x0300]);
        cpu.registers.data[0] = 0x2;
        cpu.registers.data[1] = 33;
        cpu.registers
            .set_ccr((status::X | status::N | status::Z | status::V | status::C) as u8);
        step(&mut cpu, 1);
        assert_eq!(
            cpu.registers.sr() & status::CCR,
            status::X | status::N | status::V | status::C
        );
        assert_eq!(cpu.registers.data[0], 0x2);
    }

    #[test]
    fn bset_static_in_memory_is_a_byte_modulo_8() {
        // BSET #9, (A0): bit 1 of the byte
        let mut cpu = machine(&[0x08d0, 0x0009]);
        cpu.registers.set_address(0, 0x2000);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, 0x2000), 0x0200_0000);
        assert_eq!(cpu.registers.sr() & status::Z, status::Z);
        assert_eq!(cpu.registers.pc, CODE + 4);
    }

    #[test]
    fn bchg_and_bclr_on_a_register() {
        // BCHG #31, D2; BCLR D3, D2
        let mut cpu = machine(&[0x0842, 0x001f, 0x0782]);
        cpu.registers.data[2] = 0x8000_0001;
        cpu.registers.data[3] = 0;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], 0x0000_0001);
        assert_eq!(cpu.registers.sr() & status::Z, 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], 0);
        assert_eq!(cpu.registers.sr() & status::Z, 0);
    }

    #[test]
    fn btst_dynamic_takes_an_immediate_and_pc_relative() {
        // BTST D0, #$80; BTST D0, (-6, PC) at CODE + 4, reading CODE.
        let mut cpu = machine(&[0x013c, 0x0080, 0x013a, 0xfffa]);
        cpu.registers.data[0] = 7;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.sr() & status::Z, 0);
        // The byte at CODE is $01: bit 7 clear.
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.sr() & status::Z, status::Z);
    }

    #[test]
    fn bclr_postincrement_steps_a_byte() {
        // BCLR D1, (A0)+
        let mut cpu = machine(&[0x0398]);
        cpu.registers.set_address(0, 0x2001);
        cpu.registers.data[1] = 0;
        poke_long(&mut cpu, 0x2000, 0x00ff_0000);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, 0x2000), 0x00fe_0000);
        assert_eq!(cpu.registers.address(0), 0x2002);
    }

    #[test]
    fn the_encodings_leave_movep_and_immediate_destinations_alone() {
        let admits = |opcode: u16| {
            let operation = super::Operation::of(opcode);
            operation.category().admits(opcode & super::FIELD_MASK)
        };
        // BCHG D0, #data is not data alterable; BTST D0, #data is data.
        assert!(!admits(0x017c));
        assert!(admits(0x013c));
    }
}
