//! Multiply and divide (CPU32RM 4.3.3): MULU, MULS, DIVU and DIVS, word
//! and long, with 64-bit products and dividends.
//!
//! After a divide overflow or by zero, N and Z are undefined, and V
//! too for zero (CPU32RM 4.4, DIVS, DIVU): the core leaves them
//! unchanged ([PLAN.md, Where the manuals are silent]).
//!
//! [PLAN.md, Where the manuals are silent]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#where-the-manuals-are-silent

use crate::address::Address;
use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::THREE_BITS;
use crate::exception::Exception;
use crate::exception::Vector;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::instructions::data_movement::register_field;
use crate::registers::Registers;
use crate::registers::status;
use crate::size::Size;
use crate::timing;

/// The word forms: 1x00 rrr s11 <ea> (CPU32RM 4.4, MULU, MULS, DIVU,
/// DIVS word forms).
const WORD_MASK: u16 = 0xf1c0;
/// MULU.W's fixed bits.
const MULU_WORD: u16 = 0xc0c0;
/// MULS.W's fixed bits.
const MULS_WORD: u16 = 0xc1c0;
/// DIVU.W's fixed bits.
const DIVU_WORD: u16 = 0x80c0;
/// DIVS.W's fixed bits.
const DIVS_WORD: u16 = 0x81c0;
/// The long forms: 0100 1100 0d <ea>, then a second word (CPU32RM 4.4,
/// the long forms).
const LONG_MASK: u16 = 0xffc0;
/// MULU.L and MULS.L's fixed bits.
const MULTIPLY_LONG: u16 = 0x4c00;
/// DIVU.L, DIVUL.L, DIVS.L and DIVSL.L's fixed bits.
const DIVIDE_LONG: u16 = 0x4c40;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;

/// The long forms' second word (CPU32RM 4.4, MULS and DIVS long form):
/// 0 rrr s z 0000000 rrr.
mod extension {
    /// The bits that must be zero: 15 and 9:3.
    pub(super) const RESERVED: u16 = 0x83f8;
    /// Where Dl or Dq, bits 14:12, starts.
    pub(super) const LOW_SHIFT: u16 = 12;
    /// Bit 11: signed.
    pub(super) const SIGNED: u16 = 1 << 11;
    /// Bit 10, Size: a 64-bit product or dividend.
    pub(super) const QUAD: u16 = 1 << 10;
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    let data = |opcode: u16| Category::DATA.admits(opcode & FIELD_MASK);
    table.set(WORD_MASK, MULU_WORD, data, multiply_word);
    table.set(WORD_MASK, MULS_WORD, data, multiply_word);
    table.set(WORD_MASK, DIVU_WORD, data, divide_word);
    table.set(WORD_MASK, DIVS_WORD, data, divide_word);
    table.set(LONG_MASK, MULTIPLY_LONG, data, multiply_long);
    table.set(LONG_MASK, DIVIDE_LONG, data, divide_long);
}

/// Whether a word form is signed: bit 8.
const fn word_is_signed(opcode: u16) -> bool {
    opcode & 1 << 8 != 0
}

/// N and Z from `result` taken as `size`, and V; C cleared.
const fn set_flags(registers: &mut Registers, result: u32, size: Size, overflow: bool) {
    registers.set_flag(status::N, result & size.sign_bit() != 0);
    registers.set_flag(status::Z, result & size.mask() == 0);
    registers.set_flag(status::V, overflow);
    registers.set_flag(status::C, false);
}

/// The zero divide exception: a six-word frame with the next
/// instruction's PC and the divide's own address (CPU32RM 6.2.4).  C
/// is cleared, as always; N, Z and V are undefined and kept.
fn zero_divide(core: &mut Core<'_>) -> Exception {
    core.cpu.registers.set_flag(status::C, false);
    Exception::Long {
        vector: Vector::ZERO_DIVIDE,
        pc: Address(core.cpu.registers.pc),
        address: core.instruction_pc(),
    }
}

/// A divide overflow: V set, C cleared, the operands unaffected; N and
/// Z are undefined and kept (CPU32RM 4.4, DIVS).
const fn overflow(registers: &mut Registers) {
    registers.set_flag(status::V, true);
    registers.set_flag(status::C, false);
}

/// The source operand of `size`.
fn source(core: &mut Core<'_>, opcode: u16, size: Size) -> Result<u32, Exception> {
    let operand = core.effective_address(opcode & FIELD_MASK, size)?;
    core.read_operand(operand, size)
}

/// MULU.W and MULS.W <ea>, Dn: 16 x 16 to 32.
fn multiply_word(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let source = source(core, opcode, Size::Word)?;
    let register = register_field(opcode);
    let registers = &mut core.cpu.registers;
    let destination = registers.data[register];
    let product = if word_is_signed(opcode) {
        Size::Word
            .sign_extend(source)
            .cast_signed()
            .wrapping_mul(Size::Word.sign_extend(destination).cast_signed())
            .cast_unsigned()
    } else {
        (source & Size::Word.mask()) * (destination & Size::Word.mask())
    };
    registers.data[register] = product;
    set_flags(registers, product, Size::Long, false);
    Ok(Flow::Next)
}

/// DIVU.W and DIVS.W <ea>, Dn: 32 / 16 to a 16-bit remainder and
/// quotient; overflow if the quotient does not fit 16 bits.
fn divide_word(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let divisor = source(core, opcode, Size::Word)?;
    if divisor == 0 {
        return Err(zero_divide(core));
    }
    let register = register_field(opcode);
    let registers = &mut core.cpu.registers;
    let dividend = registers.data[register];
    let divided = if word_is_signed(opcode) {
        let dividend = i64::from(dividend.cast_signed());
        let divisor = i64::from(Size::Word.sign_extend(divisor).cast_signed());
        let quotient = dividend / divisor;
        i16::try_from(quotient).ok().map(|quotient| {
            (
                quotient.cast_unsigned(),
                ((dividend % divisor) as i16).cast_unsigned(),
            )
        })
    } else {
        u16::try_from(dividend / divisor)
            .ok()
            .map(|quotient| (quotient, (dividend % divisor) as u16))
    };
    match divided {
        Some((quotient, remainder)) => {
            registers.data[register] = u32::from(remainder) << 16 | u32::from(quotient);
            set_flags(registers, u32::from(quotient), Size::Word, false);
        }
        None => overflow(registers),
    }
    Ok(Flow::Next)
}

/// The long forms' second word and its registers: the low (Dl, Dq)
/// and the other (Dh, Dr).  Reserved bits set are an illegal
/// instruction ([PLAN.md, Where the manuals are silent]).
///
/// [PLAN.md, Where the manuals are silent]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#where-the-manuals-are-silent
fn long_extension(core: &mut Core<'_>) -> Result<(u16, usize, usize), Exception> {
    let extension = core.fetch_word()?;
    if extension & extension::RESERVED != 0 {
        return Err(core.illegal());
    }
    Ok((
        extension,
        usize::from(extension >> extension::LOW_SHIFT & THREE_BITS),
        usize::from(extension & THREE_BITS),
    ))
}

/// MULU.L and MULS.L <ea>, Dl (32 x 32 to 32, V on overflow) and
/// <ea>, Dh:Dl (to 64).  With Dh = Dl a 64-bit product is undefined:
/// Dl, written last, holds the low half, as on Musashi's 68020.
fn multiply_long(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let (extension, low, high) = long_extension(core)?;
    core.charge(timing::multiply_long(extension));
    let source = source(core, opcode, Size::Long)?;
    let registers = &mut core.cpu.registers;
    let destination = registers.data[low];
    let signed = extension & extension::SIGNED != 0;
    let product = if signed {
        (i64::from(source.cast_signed()) * i64::from(destination.cast_signed())).cast_unsigned()
    } else {
        u64::from(source) * u64::from(destination)
    };
    let low_half = product as u32;
    let high_half = (product >> u32::BITS) as u32;
    if extension & extension::QUAD != 0 {
        registers.data[high] = high_half;
        registers.data[low] = low_half;
        registers.set_flag(status::N, product >> (u64::BITS - 1) != 0);
        registers.set_flag(status::Z, product == 0);
        registers.set_flag(status::V | status::C, false);
    } else {
        let extended = if signed && low_half & Size::Long.sign_bit() != 0 {
            u32::MAX
        } else {
            0
        };
        registers.data[low] = low_half;
        set_flags(registers, low_half, Size::Long, high_half != extended);
    }
    Ok(Flow::Next)
}

/// DIVU.L and DIVS.L <ea>, Dq (32 / 32) and <ea>, Dr:Dq (64 / 32), and
/// DIVUL.L and DIVSL.L <ea>, Dr:Dq (32 / 32 with a remainder): the
/// quotient to Dq, the remainder to Dr unless Dr is Dq.
fn divide_long(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let (extension, quotient_register, remainder_register) = long_extension(core)?;
    core.charge(timing::divide_long(extension));
    let divisor = source(core, opcode, Size::Long)?;
    if divisor == 0 {
        return Err(zero_divide(core));
    }
    let registers = &mut core.cpu.registers;
    let low = registers.data[quotient_register];
    let quad = extension & extension::QUAD != 0;
    let high = registers.data[remainder_register];
    let divided = if extension & extension::SIGNED != 0 {
        let dividend = if quad {
            i128::from((u64::from(high) << u32::BITS | u64::from(low)).cast_signed())
        } else {
            i128::from(low.cast_signed())
        };
        let divisor = i128::from(divisor.cast_signed());
        i32::try_from(dividend / divisor).ok().map(|quotient| {
            (
                quotient.cast_unsigned(),
                ((dividend % divisor) as i32).cast_unsigned(),
            )
        })
    } else {
        let dividend = if quad {
            u64::from(high) << u32::BITS | u64::from(low)
        } else {
            u64::from(low)
        };
        let divisor = u64::from(divisor);
        u32::try_from(dividend / divisor)
            .ok()
            .map(|quotient| (quotient, (dividend % divisor) as u32))
    };
    match divided {
        Some((quotient, remainder)) => {
            if remainder_register != quotient_register {
                registers.data[remainder_register] = remainder;
            }
            registers.data[quotient_register] = quotient;
            set_flags(registers, quotient, Size::Long, false);
        }
        None => overflow(registers),
    }
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use crate::address::Address;
    use crate::cpu::Cpu;
    use crate::exception::Exception;
    use crate::exception::Vector;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::step;
    use crate::registers::status;

    /// Where the tests' supervisor stack starts, inside RAM.
    const STACK: u32 = 0x8000;

    fn ccr(cpu: &Cpu) -> u16 {
        cpu.registers.sr() & status::CCR
    }

    fn prepared(code: &[u16]) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.set_address(7, STACK);
        cpu
    }

    #[test]
    fn mulu_and_muls_word() {
        // MULU.W D1, D0; MULS.W D1, D2
        let mut cpu = prepared(&[0xc0c1, 0xc5c1]);
        cpu.registers.data[0] = 0xdead_ffff;
        cpu.registers.data[1] = 0x0000_ffff;
        cpu.registers.data[2] = 0x0000_0002;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xfffe_0001);
        assert_eq!(ccr(&cpu), status::N);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], 0xffff_fffe);
        assert_eq!(ccr(&cpu), status::N);
    }

    #[test]
    fn divu_word_quotient_and_remainder() {
        // DIVU.W #7, D0
        let mut cpu = prepared(&[0x80fc, 0x0007]);
        cpu.registers.data[0] = 100;
        cpu.registers.set_ccr((status::X | status::C) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 2 << 16 | 14);
        assert_eq!(ccr(&cpu), status::X);
    }

    #[test]
    fn divs_word_remainder_takes_the_dividends_sign() {
        // DIVS.W #7, D0: -100 / 7 = -14 remainder -2
        let mut cpu = prepared(&[0x81fc, 0x0007]);
        cpu.registers.data[0] = (-100i32).cast_unsigned();
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xfffe_fff2);
        assert_eq!(ccr(&cpu), status::N);
    }

    #[test]
    fn divide_overflow_sets_v_and_leaves_the_operand() {
        // DIVU.W #1, D0 with a quotient of 17 bits; DIVS.W #-1 of $80000000
        let mut cpu = prepared(&[0x80fc, 0x0001, 0x81fc, 0xffff]);
        cpu.registers.data[0] = 0x8000_0000;
        cpu.registers.set_ccr((status::N | status::C) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x8000_0000);
        assert_eq!(ccr(&cpu), status::N | status::V);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x8000_0000);
        assert_eq!(ccr(&cpu), status::N | status::V);
    }

    #[test]
    fn divide_by_zero_traps_with_a_six_word_frame() {
        // DIVU.W D1, D0
        let mut cpu = prepared(&[0x80c1]);
        cpu.registers.data[0] = 5;
        cpu.registers.data[1] = 0xffff_0000;
        cpu.registers.set_ccr(status::C as u8);
        step(&mut cpu, 1);
        assert_eq!(
            cpu.exception_taken(),
            Some(Exception::Long {
                vector: Vector::ZERO_DIVIDE,
                pc: Address(CODE + 2),
                address: Address(CODE),
            })
        );
        assert_eq!(cpu.registers.data[0], 5);
    }

    #[test]
    fn divl_by_zero_stacks_the_pc_past_the_operand() {
        // DIVU.L #0, D0
        let mut cpu = prepared(&[0x4c7c, 0x0000, 0x0000, 0x0000]);
        step(&mut cpu, 1);
        assert_eq!(
            cpu.exception_taken(),
            Some(Exception::Long {
                vector: Vector::ZERO_DIVIDE,
                pc: Address(CODE + 8),
                address: Address(CODE),
            })
        );
    }

    #[test]
    fn mulu_long_overflows_into_v_or_a_quad_product() {
        // MULU.L D1, D0; MULU.L D1, D3:D2
        let mut cpu = prepared(&[0x4c01, 0x0000, 0x4c01, 0x2403]);
        cpu.registers.data[0] = 0x1_0000;
        cpu.registers.data[1] = 0x1_0001;
        cpu.registers.data[2] = 0xffff_ffff;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x0001_0000);
        assert_eq!(ccr(&cpu), status::V);
        step(&mut cpu, 1);
        // $FFFFFFFF * $10001 = $10000_FFFEFFFF
        assert_eq!(cpu.registers.data[2], 0xfffe_ffff);
        assert_eq!(cpu.registers.data[3], 0x0001_0000);
        assert_eq!(ccr(&cpu), 0);
    }

    #[test]
    fn muls_long_sign_extension_is_no_overflow() {
        // MULS.L D1, D0: -2 * 3; MULS.L D1, D3:D0 of -6 * 3, negative
        let mut cpu = prepared(&[0x4c01, 0x0800, 0x4c01, 0x0c03]);
        cpu.registers.data[0] = (-2i32).cast_unsigned();
        cpu.registers.data[1] = 3;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], (-6i32).cast_unsigned());
        assert_eq!(ccr(&cpu), status::N);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], (-18i32).cast_unsigned());
        assert_eq!(cpu.registers.data[3], u32::MAX);
        assert_eq!(ccr(&cpu), status::N);
    }

    #[test]
    fn divs_long_quad_and_divsl_remainder() {
        // DIVS.L D1, D3:D0 (64 / 32); DIVSL.L D1, D4:D2 (32 / 32)
        let mut cpu = prepared(&[0x4c41, 0x0c03, 0x4c41, 0x2804]);
        // Dr:Dq = -1
        cpu.registers.data[3] = 0xffff_ffff;
        cpu.registers.data[0] = 0xffff_ffff;
        cpu.registers.data[1] = 3;
        cpu.registers.data[2] = (-7i32).cast_unsigned();
        cpu.registers.data[4] = 0x1234;
        step(&mut cpu, 1);
        // -1 / 3 is 0 remainder -1.
        assert_eq!(cpu.registers.data[0], 0);
        assert_eq!(cpu.registers.data[3], u32::MAX);
        assert_eq!(ccr(&cpu), status::Z);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], (-2i32).cast_unsigned());
        assert_eq!(cpu.registers.data[4], (-1i32).cast_unsigned());
    }

    #[test]
    fn divu_long_quad_overflow_and_quotient_only() {
        // DIVU.L D1, D3:D0 overflows; DIVU.L D1, D0 with Dr = Dq
        let mut cpu = prepared(&[0x4c41, 0x0403, 0x4c41, 0x0000]);
        cpu.registers.data[3] = 5;
        cpu.registers.data[0] = 0;
        cpu.registers.data[1] = 5;
        step(&mut cpu, 1);
        assert_eq!((cpu.registers.data[3], cpu.registers.data[0]), (5, 0));
        assert_eq!(ccr(&cpu), status::V);
        cpu.registers.data[0] = 17;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 3);
        assert_eq!(ccr(&cpu), 0);
    }

    #[test]
    fn reserved_second_word_bits_are_illegal() {
        // MULU.L D1, D0 with bit 3 set
        let mut cpu = prepared(&[0x4c01, 0x0008]);
        step(&mut cpu, 1);
        assert_eq!(
            cpu.exception_taken(),
            Some(Exception::Short {
                vector: Vector::ILLEGAL_INSTRUCTION,
                pc: Address(CODE),
            })
        );
    }
}
