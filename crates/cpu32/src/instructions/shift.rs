//! Shift and rotate instructions (CPU32RM 4.3.5): ASL, ASR, LSL, LSR,
//! ROL, ROR, ROXL and ROXR, on a data register or a word in memory.

use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::THREE_BITS;
use crate::effective_address::kind;
use crate::exception::Exception;
use crate::instructions::Flow;
use crate::instructions::Handler;
use crate::instructions::Table;
use crate::instructions::data_movement::low_register;
use crate::registers::Registers;
use crate::registers::status;
use crate::size::Size;
use crate::timing;
use crate::timing::AddressTable;

/// Line E: 1110 (CPU32RM 4.4, ASL, ASR and the rest).
const LINE_MASK: u16 = 0xf000;
/// Line E's fixed bits.
const LINE: u16 = 0xe000;
/// The memory forms: 1110 0tt d 11 <ea>; 1110 1xxx 11 are the 68020's
/// bit field instructions, which the CPU32 does not have.
const MEMORY_MASK: u16 = 0xf8c0;
/// The memory forms' fixed bits.
const MEMORY: u16 = 0xe0c0;
/// The size field, bits 7:6; 11 is the memory forms'.
const SIZE_SHIFT: u16 = 6;
/// dr, bit 8: 1 left, 0 right.
const LEFT: u16 = 1 << 8;
/// i/r, bit 5: the count is in a register.
const COUNT_IN_REGISTER: u16 = 1 << 5;
/// Where a register form's type field, bits 4:3, starts.
const REGISTER_TYPE_SHIFT: u16 = 3;
/// Where a memory form's type field, bits 10:9, starts.
const MEMORY_TYPE_SHIFT: u16 = 9;
/// Where the count or count register field, bits 11:9, starts.
const COUNT_SHIFT: u16 = 9;
/// An immediate count field of zero is a count of eight.
const IMMEDIATE_COUNT_EIGHT: u32 = 8;
/// A count in a register is taken modulo 64 (CPU32RM 4.4, ASL, ASR).
const REGISTER_COUNT_MODULUS: u32 = 64;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;

/// The four kinds, by the type field (CPU32RM 4.4: 00 `ASd`, 01 `LSd`,
/// 10 `ROXd`, 11 `ROd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Arithmetic shift.
    Arithmetic,
    /// Logical shift.
    Logical,
    /// Rotate through X.
    RotateExtend,
    /// Rotate.
    Rotate,
}

impl Kind {
    /// The kind of type field `bits`.
    const fn of(bits: u16) -> Self {
        match bits & 0b11 {
            0b00 => Self::Arithmetic,
            0b01 => Self::Logical,
            0b10 => Self::RotateExtend,
            _ => Self::Rotate,
        }
    }
}

/// A shift's result and the condition codes it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shifted {
    /// The result, as the operation's size.
    result: u32,
    /// The last bit shifted out, or for `ROXd` of zero, X.
    carry: bool,
    /// ASL only: whether the MSB changed at any step.
    overflow: bool,
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set_by(
        MEMORY_MASK,
        MEMORY,
        |opcode| {
            Category::MEMORY
                .and(Category::ALTERABLE)
                .admits(opcode & FIELD_MASK)
        },
        shift_memory_handler,
    );
    table.set_by(
        LINE_MASK,
        LINE,
        |opcode| Size::from_field(opcode >> SIZE_SHIFT).is_some(),
        shift_register_handler,
    );
}

/// [`shift_register`]'s handler for `opcode`, specialized by size,
/// type, direction and where the count is.
fn shift_register_handler(opcode: u16) -> Handler {
    specialize_size!(Size::from_field(opcode >> SIZE_SHIFT), SIZE =>
        specialize_two_bits!(opcode >> REGISTER_TYPE_SHIFT, TYPE =>
            specialize_flag!(opcode & LEFT != 0, LEFTWARD =>
                specialize_flag!(opcode & COUNT_IN_REGISTER != 0, IN_REGISTER =>
                    shift_register::<SIZE, TYPE, LEFTWARD, IN_REGISTER>))))
}

/// [`shift_memory`]'s handler for `opcode`, specialized by type,
/// direction and operand kind.
fn shift_memory_handler(opcode: u16) -> Handler {
    specialize_two_bits!(opcode >> MEMORY_TYPE_SHIFT, TYPE =>
        specialize_flag!(opcode & LEFT != 0, LEFTWARD =>
            specialize_memory_alterable!(kind::of(opcode & FIELD_MASK), OPERAND =>
                shift_memory::<TYPE, LEFTWARD, OPERAND>)))
}

/// `value` of `size` shifted or rotated `count` places, `x` the X bit
/// it starts with (CPU32RM 4.4, the shift and rotate pages).  Inlined,
/// so a specialized handler folds to its kind and direction.
#[inline(always)]
fn shift(kind: Kind, left: bool, value: u32, count: u32, size: Size, x: bool) -> Shifted {
    let width = size.bytes() * u8::BITS;
    let value = value & size.mask();
    if count == 0 {
        // C is cleared, or for `ROXd` set to X (CPU32RM Table 4-1).
        return Shifted {
            result: value,
            carry: kind == Kind::RotateExtend && x,
            overflow: false,
        };
    }
    let wide = u128::from(value);
    let mask = u128::from(size.mask());
    let (result, carry, overflow) = match (kind, left) {
        (Kind::Arithmetic | Kind::Logical, true) => {
            let shifted = wide << count;
            // ASL: the MSB at each step is one of the top count + 1
            // bits, or a zero shifted in; V if they are not all equal.
            let overflow = kind == Kind::Arithmetic
                && if count < width {
                    let top =
                        i64::from(size.sign_extend(value).cast_signed()) >> (width - 1 - count);
                    top != 0 && top != -1
                } else {
                    value != 0
                };
            (shifted & mask, shifted >> width & 1 != 0, overflow)
        }
        (Kind::Logical, false) => (wide >> count, wide >> (count - 1) & 1 != 0, false),
        (Kind::Arithmetic, false) => {
            let signed = i64::from(size.sign_extend(value).cast_signed());
            let shift = |by: u32| signed >> by.min(i64::BITS - 1);
            (
                i128::from(shift(count)).cast_unsigned() & mask,
                shift(count - 1) & 1 != 0,
                false,
            )
        }
        (Kind::Rotate, _) => {
            let places = count % width;
            let places = if left {
                places
            } else {
                (width - places) % width
            };
            let rotated = (wide << places | wide >> ((width - places) % width)) & mask;
            let carry = if left {
                rotated & 1 != 0
            } else {
                rotated >> (width - 1) & 1 != 0
            };
            (rotated, carry, false)
        }
        (Kind::RotateExtend, _) => {
            // X is the (width + 1)th bit of the rotation.
            let span = width + 1;
            let places = count % span;
            let places = if left { places } else { (span - places) % span };
            let combined = u128::from(x) << width | wide;
            let all = (1u128 << span) - 1;
            let rotated = (combined << places | combined >> ((span - places) % span)) & all;
            (rotated & mask, rotated >> width & 1 != 0, false)
        }
    };
    Shifted {
        result: result as u32,
        carry,
        overflow,
    }
}

/// Sets the condition codes of a shift by `count`: X as C unless the
/// count is zero or the kind is `ROd` (CPU32RM 4.4, the shift and
/// rotate pages; Table 4-1).  Always inlined: the handlers' code
/// changes without it ([docs/performance.md, Forced inlining]).
///
/// [docs/performance.md, Forced inlining]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/performance.md#made-since-the-baseline
#[inline(always)]
fn set_flags(registers: &mut Registers, kind: Kind, count: u32, shifted: Shifted, size: Size) {
    registers.set_flag(status::N, shifted.result & size.sign_bit() != 0);
    registers.set_flag(status::Z, shifted.result & size.mask() == 0);
    registers.set_flag(status::V, shifted.overflow);
    registers.set_flag(status::C, shifted.carry);
    if count != 0 && kind != Kind::Rotate {
        registers.set_flag(status::X, shifted.carry);
    }
}

/// `ASd`, `LSd`, `ROXd`, `ROd` #count, Dy and Dx, Dy.  `SIZE` is a
/// [`Size::index`], `TYPE` the type field, `LEFTWARD` dr, `IN_REGISTER`
/// i/r.
fn shift_register<
    const SIZE: u8,
    const TYPE: u16,
    const LEFTWARD: bool,
    const IN_REGISTER: bool,
>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let kind = const { Kind::of(TYPE) };
    let field = u32::from(opcode >> COUNT_SHIFT & THREE_BITS);
    let count = if IN_REGISTER {
        let count = core.cpu.registers.data[field as usize] % REGISTER_COUNT_MODULUS;
        core.charge(timing::shift_count(opcode, count));
        count
    } else if field == 0 {
        IMMEDIATE_COUNT_EIGHT
    } else {
        field
    };
    let register = low_register(opcode);
    let registers = &mut core.cpu.registers;
    let shifted = shift(
        kind,
        LEFTWARD,
        registers.data[register],
        count,
        size,
        registers.flag(status::X),
    );
    let destination = &mut registers.data[register];
    *destination = *destination & !size.mask() | shifted.result;
    set_flags(registers, kind, count, shifted, size);
    Ok(Flow::Next)
}

/// `ASd`, `LSd`, `ROXd`, `ROd` <ea>: a word in memory, by one.  `TYPE`
/// is the type field, `LEFTWARD` dr, `OPERAND` the operand's kind.
fn shift_memory<const TYPE: u16, const LEFTWARD: bool, const OPERAND: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let kind = const { Kind::of(TYPE) };
    let operand = core.effective_address_of(
        OPERAND,
        opcode & FIELD_MASK,
        Size::Word,
        AddressTable::Fetch,
    )?;
    let value = core.read_operand(operand, Size::Word)?;
    let shifted = shift(
        kind,
        LEFTWARD,
        value,
        1,
        Size::Word,
        core.cpu.registers.flag(status::X),
    );
    core.write_operand(operand, Size::Word, shifted.result)?;
    set_flags(&mut core.cpu.registers, kind, 1, shifted, Size::Word);
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use super::Kind;
    use super::shift;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::step;
    use crate::registers::status;
    use crate::size::Size;

    fn ccr(cpu: &crate::cpu::Cpu) -> u16 {
        cpu.registers.sr() & status::CCR
    }

    #[test]
    fn asl_sets_v_when_the_sign_changes() {
        // ASL.B #1, D0: $40 -> $80, the MSB changed.
        let mut cpu = machine(&[0xe300]);
        cpu.registers.data[0] = 0x40;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x80);
        assert_eq!(ccr(&cpu), status::N | status::V);
    }

    #[test]
    fn asl_by_more_than_the_width_overflows_unless_zero() {
        let shifted = shift(Kind::Arithmetic, true, 0xffff, 17, Size::Word, false);
        assert_eq!(
            (shifted.result, shifted.carry, shifted.overflow),
            (0, false, true)
        );
        let shifted = shift(Kind::Arithmetic, true, 0x0, 40, Size::Long, false);
        assert!(!shifted.overflow);
        // All ones shifted by fewer than the width: the sign never changes.
        let shifted = shift(Kind::Arithmetic, true, 0xff, 7, Size::Byte, false);
        assert_eq!(
            (shifted.result, shifted.carry, shifted.overflow),
            (0x80, true, false)
        );
    }

    #[test]
    fn asr_copies_the_sign_and_carries_the_last_bit() {
        // ASR.W #2, D1
        let mut cpu = machine(&[0xe441]);
        cpu.registers.data[1] = 0x1234_8006;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0x1234_e001);
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
        let shifted = shift(Kind::Arithmetic, false, 0x8000_0000, 63, Size::Long, false);
        assert_eq!((shifted.result, shifted.carry), (0xffff_ffff, true));
    }

    #[test]
    fn lsr_by_register_count_modulo_64() {
        // LSR.L D2, D3: count 65 is 1
        let mut cpu = machine(&[0xe4ab]);
        cpu.registers.data[2] = 65;
        cpu.registers.data[3] = 0x8000_0001;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[3], 0x4000_0000);
        assert_eq!(ccr(&cpu), status::X | status::C);
    }

    #[test]
    fn a_zero_count_clears_c_and_keeps_x() {
        // LSL.L D2, D3 with D2 = 64
        let mut cpu = machine(&[0xe5ab]);
        cpu.registers.data[2] = 64;
        cpu.registers.data[3] = 0x8000_0000;
        cpu.registers
            .set_ccr((status::X | status::C | status::V) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[3], 0x8000_0000);
        assert_eq!(ccr(&cpu), status::X | status::N);
    }

    #[test]
    fn roxl_by_zero_sets_c_to_x() {
        // ROXL.B D2, D0 with D2 = 0
        let mut cpu = machine(&[0xe530]);
        cpu.registers.set_ccr(status::X as u8);
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::X | status::Z | status::C);
    }

    #[test]
    fn roxr_rotates_through_x() {
        // ROXR.B #1, D0: X into bit 7, bit 0 into X and C
        let mut cpu = machine(&[0xe210]);
        cpu.registers.data[0] = 0x01;
        cpu.registers.set_ccr(status::X as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x80);
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
        // Nine places on a byte is all the way round.
        let shifted = shift(Kind::RotateExtend, true, 0x5a, 9, Size::Byte, true);
        assert_eq!((shifted.result, shifted.carry), (0x5a, true));
    }

    #[test]
    fn rol_leaves_x_and_carries_the_new_lsb() {
        // ROL.W #8, D4 (count field 0)
        let mut cpu = machine(&[0xe15c]);
        cpu.registers.data[4] = 0x0000_80ff;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[4], 0x0000_ff80);
        assert_eq!(ccr(&cpu), status::N);
        // A whole turn still carries.
        let shifted = shift(Kind::Rotate, false, 0x8000_0000, 32, Size::Long, false);
        assert_eq!((shifted.result, shifted.carry), (0x8000_0000, true));
    }

    #[test]
    fn memory_shift_is_a_word_by_one() {
        // ASR.W (A0)
        let mut cpu = machine(&[0xe0d0]);
        cpu.registers.set_address(0, 0x2000);
        crate::instructions::tests::poke_long(&mut cpu, 0x2000, 0x8001_0000);
        step(&mut cpu, 1);
        assert_eq!(
            crate::instructions::tests::peek_long(&cpu, 0x2000),
            0xc000_0000
        );
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
    }
}
