//! Binary-coded decimal instructions (CPU32RM 4.3.7): ABCD, SBCD and
//! NBCD, on packed BCD bytes with X.
//!
//! N and V are undefined (CPU32RM 4.4, ABCD, SBCD, NBCD; Table 4-1):
//! the core leaves them unchanged ([PLAN.md, Where the manuals are
//! silent]).  So is the result of a digit above 9, which the manual
//! does not define either: the decimal adjust below decides the high
//! digit's adjust from the binary sum or difference, as the 68000
//! does by SingleStepTests' cases.
//!
//! [PLAN.md, Where the manuals are silent]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#where-the-manuals-are-silent

use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::THREE_BITS;
use crate::exception::Exception;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::registers::Registers;
use crate::registers::status;
use crate::size::Size;

/// ABCD: 1100 xxx1 0000 myyy (CPU32RM 4.4, ABCD).
const EXTENDED_MASK: u16 = 0xf1f0;
/// ABCD's fixed bits.
const ABCD: u16 = 0xc100;
/// SBCD: 1000 xxx1 0000 myyy (CPU32RM 4.4, SBCD).
const SBCD: u16 = 0x8100;
/// NBCD: 0100 1000 00 <ea> (CPU32RM 4.4, NBCD).
const NBCD_MASK: u16 = 0xffc0;
/// NBCD's fixed bits.
const NBCD: u16 = 0x4800;
/// R/M, bit 3: memory to memory, -(Ay), -(Ax).
const MEMORY_TO_MEMORY: u16 = 1 << 3;
/// Where the destination register field, bits 11:9, starts.
const DESTINATION_SHIFT: u16 = 9;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;
/// The mode field of -(An), placed in a six-bit field.
const PREDECREMENT_FIELD: u16 = 0b100 << 3;
/// A BCD digit.
const DIGIT: u32 = 0xf;
/// The largest BCD digit.
const MAXIMUM_DIGIT: i32 = 9;
/// What a digit's decimal adjust adds or takes away.
const LOW_ADJUST: i32 = 0x06;
/// What the high digit's decimal adjust adds or takes away.
const HIGH_ADJUST: i32 = 0x60;
/// The largest two-digit BCD value.
const MAXIMUM_BYTE: i32 = 0x99;

/// A BCD byte result and its decimal carry or borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Decimal {
    /// The result byte.
    result: u32,
    /// The decimal carry or borrow, to C and X.
    carry: bool,
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set(EXTENDED_MASK, ABCD, |_| true, abcd);
    table.set(EXTENDED_MASK, SBCD, |_| true, sbcd);
    table.set(
        NBCD_MASK,
        NBCD,
        |opcode| {
            Category::DATA
                .and(Category::ALTERABLE)
                .admits(opcode & FIELD_MASK)
        },
        nbcd,
    );
}

/// `destination + source + x` in BCD: the low digit adjusted past 9,
/// the high digit, with the carry, if the binary sum passes $99.
const fn add(destination: u32, source: u32, x: bool) -> Decimal {
    let low = (destination & DIGIT).cast_signed() + (source & DIGIT).cast_signed() + x as i32;
    let binary = destination.cast_signed() + source.cast_signed() + x as i32;
    let carry = binary > MAXIMUM_BYTE;
    let sum = if low > MAXIMUM_DIGIT {
        binary + LOW_ADJUST
    } else {
        binary
    };
    let sum = if carry { sum + HIGH_ADJUST } else { sum };
    Decimal {
        result: sum.cast_unsigned() & Size::Byte.mask(),
        carry,
    }
}

/// `destination - source - x` in BCD: the low digit adjusted on its
/// borrow, the high digit on the binary difference's; the borrow out
/// is the low-adjusted difference's.
const fn subtract(destination: u32, source: u32, x: bool) -> Decimal {
    let low = (destination & DIGIT).cast_signed() - (source & DIGIT).cast_signed() - x as i32;
    let binary = destination.cast_signed() - source.cast_signed() - x as i32;
    let difference = if low < 0 { binary - LOW_ADJUST } else { binary };
    let carry = difference < 0;
    let difference = if binary < 0 {
        difference - HIGH_ADJUST
    } else {
        difference
    };
    Decimal {
        result: difference.cast_unsigned() & Size::Byte.mask(),
        carry,
    }
}

/// X and C from the decimal carry; Z cleared if the result is nonzero,
/// else unchanged; N and V unchanged, being undefined.
const fn set_flags(registers: &mut Registers, decimal: Decimal) {
    registers.set_flag(status::X | status::C, decimal.carry);
    if decimal.result != 0 {
        registers.set_flag(status::Z, false);
    }
}

/// ABCD and SBCD's operands: Dy, Dx, or -(Ay), -(Ax), source first.
fn extended(
    core: &mut Core<'_>,
    opcode: u16,
    operation: fn(u32, u32, bool) -> Decimal,
) -> Result<Flow, Exception> {
    let mode = if opcode & MEMORY_TO_MEMORY != 0 {
        PREDECREMENT_FIELD
    } else {
        0
    };
    let source = core.effective_address(mode | opcode & THREE_BITS, Size::Byte)?;
    let source = core.read_operand(source, Size::Byte)?;
    let destination =
        core.effective_address(mode | opcode >> DESTINATION_SHIFT & THREE_BITS, Size::Byte)?;
    let value = core.read_operand(destination, Size::Byte)?;
    let decimal = operation(value, source, core.cpu.registers.flag(status::X));
    core.write_operand(destination, Size::Byte, decimal.result)?;
    set_flags(&mut core.cpu.registers, decimal);
    Ok(Flow::Next)
}

/// ABCD Dy, Dx and ABCD -(Ay), -(Ax).
fn abcd(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    extended(core, opcode, add)
}

/// SBCD Dy, Dx and SBCD -(Ay), -(Ax).
fn sbcd(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    extended(core, opcode, subtract)
}

/// NBCD <ea>: `0 - destination - x` in BCD.
fn nbcd(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let operand = core.effective_address(opcode & FIELD_MASK, Size::Byte)?;
    let value = core.read_operand(operand, Size::Byte)?;
    let decimal = subtract(0, value, core.cpu.registers.flag(status::X));
    core.write_operand(operand, Size::Byte, decimal.result)?;
    set_flags(&mut core.cpu.registers, decimal);
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use super::add;
    use super::subtract;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::registers::status;

    #[test]
    fn decimal_sums_and_differences() {
        assert_eq!(
            (add(0x19, 0x28, false).result, add(0x19, 0x28, false).carry),
            (0x47, false)
        );
        assert_eq!(
            (add(0x99, 0x00, true).result, add(0x99, 0x00, true).carry),
            (0x00, true)
        );
        assert_eq!(
            (add(0x55, 0x55, false).result, add(0x55, 0x55, false).carry),
            (0x10, true)
        );
        let difference = subtract(0x42, 0x17, false);
        assert_eq!((difference.result, difference.carry), (0x25, false));
        let difference = subtract(0x00, 0x01, false);
        assert_eq!((difference.result, difference.carry), (0x99, true));
    }

    #[test]
    fn abcd_registers_keeps_z_when_zero_and_n_v_unchanged() {
        // ABCD D1, D0: $99 + $00 + X = $00, carry
        let mut cpu = machine(&[0xc101]);
        cpu.registers.data[0] = 0x1234_5699;
        cpu.registers.data[1] = 0;
        cpu.registers
            .set_ccr((status::X | status::Z | status::N | status::V) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x1234_5600);
        assert_eq!(
            cpu.registers.sr() & status::CCR,
            status::X | status::N | status::Z | status::V | status::C
        );
    }

    #[test]
    fn sbcd_memory_predecrements_both() {
        // SBCD -(A1), -(A0): $42 - $17 = $25, Z cleared
        let mut cpu = machine(&[0x8109]);
        cpu.registers.set_address(0, 0x2001);
        cpu.registers.set_address(1, 0x3001);
        poke_long(&mut cpu, 0x2000, 0x4200_0000);
        poke_long(&mut cpu, 0x3000, 0x1700_0000);
        cpu.registers.set_ccr(status::Z as u8);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, 0x2000), 0x2500_0000);
        assert_eq!(cpu.registers.address(0), 0x2000);
        assert_eq!(cpu.registers.address(1), 0x3000);
        assert_eq!(cpu.registers.sr() & status::CCR, 0);
    }

    #[test]
    fn nbcd_is_the_tens_or_nines_complement() {
        // NBCD D0, X clear: tens complement of $01
        let mut cpu = machine(&[0x4800, 0x4800]);
        cpu.registers.data[0] = 0x01;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x99);
        assert_eq!(cpu.registers.sr() & status::CCR, status::X | status::C);
        // Again with X set: 0 - $99 - 1 is $00 and a borrow; Z stays.
        cpu.registers.set_ccr((status::X | status::Z) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x00);
        assert_eq!(
            cpu.registers.sr() & status::CCR,
            status::X | status::Z | status::C
        );
    }
}
