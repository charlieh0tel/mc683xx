//! Bounds checks (CPU32RM 4.3.3, 4.3.9): CHK, CHK2 and CMP2.
//!
//! Undefined flags are left unchanged ([PLAN.md, Where the manuals are
//! silent]): CHK's Z, V and C, and its N when it does not trap; CHK2
//! and CMP2's N and V (CPU32RM 4.4, CHK, CHK2, CMP2; Table 4-1).
//!
//! [PLAN.md, Where the manuals are silent]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#where-the-manuals-are-silent

use crate::address::Address;
use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::Operand;
use crate::effective_address::THREE_BITS;
use crate::exception::Exception;
use crate::exception::Vector;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::instructions::data_movement::register_field;
use crate::registers::status;
use crate::size::Size;

/// CHK: 0100 rrr1 s0 <ea>, size 11 word, 10 long (CPU32RM 4.4, CHK).
const CHK_MASK: u16 = 0xf1c0;
/// CHK.W's fixed bits.
const CHK_WORD: u16 = 0x4180;
/// CHK.L's fixed bits.
const CHK_LONG: u16 = 0x4100;
/// CHK2 and CMP2: 0000 0ss0 11 <ea>, then a second word (CPU32RM 4.4,
/// CHK2, CMP2).  Size 11 is not theirs.
const BOUNDS_MASK: u16 = 0xf9c0;
/// CHK2 and CMP2's fixed bits.
const BOUNDS: u16 = 0x00c0;
/// Where CHK2 and CMP2's size field, bits 10:9, starts.
const BOUNDS_SIZE_SHIFT: u16 = 9;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;

/// CHK2 and CMP2's second word: D/A rrr c 000 0000 0000.
mod extension {
    /// D/A, bit 15: the register is an address register.
    pub(super) const ADDRESS_REGISTER: u16 = 1 << 15;
    /// Where the register number, bits 14:12, starts.
    pub(super) const REGISTER_SHIFT: u16 = 12;
    /// Bit 11: CHK2, which traps; clear for CMP2.
    pub(super) const TRAP: u16 = 1 << 11;
    /// The bits that must be zero, 10:0.
    pub(super) const RESERVED: u16 = 0x07ff;
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    let data = |opcode: u16| Category::DATA.admits(opcode & FIELD_MASK);
    table.set(CHK_MASK, CHK_WORD, data, chk);
    table.set(CHK_MASK, CHK_LONG, data, chk);
    table.set(
        BOUNDS_MASK,
        BOUNDS,
        |opcode| {
            Size::from_field(opcode >> BOUNDS_SIZE_SHIFT).is_some()
                && Category::CONTROL.admits(opcode & FIELD_MASK)
        },
        compare_bounds,
    );
}

/// The CHK instruction exception: a six-word frame with the next
/// instruction's PC and the instruction's own address (CPU32RM 6.2.4).
fn chk_exception(core: &Core<'_>) -> Exception {
    Exception::Long {
        vector: Vector::CHK,
        pc: Address(core.cpu.registers.pc),
        address: core.instruction_pc(),
    }
}

/// Reads the operand of `size` at `offset` bytes from a memory
/// operand's address, in its space: CHK2's upper bound, TBL's entries.
pub(super) fn read_at(
    core: &mut Core<'_>,
    operand: Operand,
    offset: u32,
    size: Size,
) -> Result<u32, Exception> {
    match operand {
        Operand::Data(address) => core.read_data(address.offset(offset), size),
        Operand::Program(address) => core.read_program(address.offset(offset), size),
        _ => Err(core.illegal()),
    }
}

/// CHK <ea>, Dn: a trap if Dn < 0 or Dn > the bound, signed.  N set if
/// Dn < 0, cleared if Dn > the bound; Dn < 0 decides when both hold.
fn chk(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let size = if opcode & CHK_MASK == CHK_WORD {
        Size::Word
    } else {
        Size::Long
    };
    let operand = core.effective_address(opcode & FIELD_MASK, size)?;
    let bound = size
        .sign_extend(core.read_operand(operand, size)?)
        .cast_signed();
    let registers = &mut core.cpu.registers;
    let value = size
        .sign_extend(registers.data[register_field(opcode)])
        .cast_signed();
    if value < 0 {
        registers.set_flag(status::N, true);
    } else if value > bound {
        registers.set_flag(status::N, false);
    } else {
        return Ok(Flow::Next);
    }
    Err(chk_exception(core))
}

/// CHK2 and CMP2 <ea>, Rn: Z if Rn equals a bound, C if it is out of
/// bounds (CPU32RM Table 4-1); CHK2 then traps.  A data register's
/// low byte or word is compared, an address register whole against
/// sign-extended bounds.  The comparisons are unsigned: Table 4-1's C,
/// with its UB < LB term, then gives the bounds a signed reading too.
fn compare_bounds(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let Some(size) = Size::from_field(opcode >> BOUNDS_SIZE_SHIFT) else {
        return Err(core.illegal());
    };
    let extension = core.fetch_word()?;
    if extension & extension::RESERVED != 0 {
        return Err(core.illegal());
    }
    let operand = core.effective_address(opcode & FIELD_MASK, size)?;
    let lower = read_at(core, operand, 0, size)?;
    let upper = read_at(core, operand, size.bytes(), size)?;
    let number = usize::from(extension >> extension::REGISTER_SHIFT & THREE_BITS);
    let registers = &mut core.cpu.registers;
    let (value, lower, upper) = if extension & extension::ADDRESS_REGISTER != 0 {
        (
            registers.address(number),
            size.sign_extend(lower),
            size.sign_extend(upper),
        )
    } else {
        (registers.data[number] & size.mask(), lower, upper)
    };
    let out_of_bounds = if lower <= upper {
        value < lower || value > upper
    } else {
        value > upper && value < lower
    };
    registers.set_flag(status::Z, value == lower || value == upper);
    registers.set_flag(status::C, out_of_bounds);
    if out_of_bounds && extension & extension::TRAP != 0 {
        return Err(chk_exception(core));
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
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::registers::status;

    /// Where the tests' supervisor stack starts, inside RAM.
    const STACK: u32 = 0x8000;
    /// Where the tests' bounds pairs are.
    const BOUNDS: u32 = 0x2000;

    fn prepared(code: &[u16]) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.set_address(7, STACK);
        cpu.registers.set_address(0, BOUNDS);
        cpu
    }

    fn ccr(cpu: &Cpu) -> u16 {
        cpu.registers.sr() & status::CCR
    }

    /// The CHK exception of the instruction at `at`, `next` past it.
    fn chk_trap(at: u32, next: u32) -> Option<Exception> {
        Some(Exception::Long {
            vector: Vector::CHK,
            pc: Address(next),
            address: Address(at),
        })
    }

    #[test]
    fn chk_word_in_bounds_does_nothing() {
        // CHK.W #10, D1
        let mut cpu = prepared(&[0x43bc, 0x000a]);
        cpu.registers.data[1] = 0xffff_000a;
        cpu.registers.set_ccr(0x1f);
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), None);
        assert_eq!(ccr(&cpu), 0x1f);
        assert_eq!(cpu.registers.pc, CODE + 4);
    }

    #[test]
    fn chk_traps_below_zero_with_n_set() {
        // CHK.W #10, D1
        let mut cpu = prepared(&[0x43bc, 0x000a]);
        cpu.registers.data[1] = 0x0000_8000;
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), chk_trap(CODE, CODE + 4));
        assert_eq!(ccr(&cpu), status::N);
    }

    #[test]
    fn chk_long_traps_above_the_bound_with_n_clear() {
        // CHK.L D2, D1
        let mut cpu = prepared(&[0x4302]);
        cpu.registers.data[1] = 0x0001_0000;
        cpu.registers.data[2] = 0x0000_ffff;
        cpu.registers.set_ccr(status::N as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), chk_trap(CODE, CODE + 2));
        assert_eq!(ccr(&cpu), 0);
    }

    #[test]
    fn cmp2_byte_on_a_data_register() {
        // CMP2.B (A0), D1: bounds $10..$20
        let mut cpu = prepared(&[0x00d0, 0x1000, 0x00d0, 0x1000, 0x00d0, 0x1000]);
        poke_long(&mut cpu, BOUNDS, 0x1020_0000);
        cpu.registers.data[1] = 0xffff_ff10;
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::Z);
        cpu.registers.data[1] = 0x21;
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::C);
        cpu.registers.data[1] = 0x15;
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), 0);
        assert_eq!(cpu.exception_taken(), None);
    }

    #[test]
    fn signed_bounds_wrap_round() {
        // CMP2.W (A0), D1: bounds -5..5
        let mut cpu = prepared(&[0x02d0, 0x1000, 0x02d0, 0x1000]);
        poke_long(&mut cpu, BOUNDS, 0xfffb_0005);
        cpu.registers.data[1] = 0xffff;
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), 0);
        cpu.registers.data[1] = 0xfffa;
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::C);
    }

    #[test]
    fn chk2_on_an_address_register_sign_extends_the_bounds() {
        // CHK2.W (A0), A1: bounds -2..2, A1 = -2 in range; then A1 = $FFFF
        let mut cpu = prepared(&[0x02d0, 0x9800, 0x02d0, 0x9800]);
        poke_long(&mut cpu, BOUNDS, 0xfffe_0002);
        cpu.registers.set_address(1, (-2i32).cast_unsigned());
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), None);
        assert_eq!(ccr(&cpu), status::Z);
        cpu.registers.set_address(1, 0xffff);
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), chk_trap(CODE + 4, CODE + 8));
        assert_eq!(ccr(&cpu), status::C);
    }

    #[test]
    fn chk2_long_pc_relative_bounds() {
        // CHK2.L (2, PC), D0: bounds 1..3 at CODE + 6
        let mut cpu = prepared(&[0x04fa, 0x0800, 0x0002, 0x0000, 0x0001, 0x0000, 0x0003]);
        cpu.registers.data[0] = 3;
        step(&mut cpu, 1);
        assert_eq!(cpu.exception_taken(), None);
        assert_eq!(ccr(&cpu), status::Z);
    }

    #[test]
    fn reserved_second_word_bits_are_illegal() {
        let mut cpu = prepared(&[0x00d0, 0x1001]);
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
