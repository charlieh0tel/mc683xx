//! Table lookup and interpolate (CPU32RM 4.4, TBLS, TBLU; 4.6): TBLS,
//! TBLSN, TBLU and TBLUN, from a table in memory or two data
//! registers.
//!
//! $F800 is both TBL Dym:Dyn with Dym = D0 and LPSTOP's first word
//! (CPU32RM 4.4, LPSTOP); its second word tells them apart, so its
//! handler here dispatches on it, handing LPSTOP's to the system
//! control family.  LPSTOP's word is no TBL second word (its size field
//! is 11).

use std::ops::RangeInclusive;

use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::THREE_BITS;
use crate::effective_address::kind;
use crate::exception::Exception;
use crate::exception::Vector;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::instructions::bounds::read_at;
use crate::instructions::data_movement::low_register;
use crate::instructions::system;
use crate::registers::status;
use crate::size::Size;
use crate::timing;

/// TBL: 1111 1000 00 <ea>, mode 000 the register form (CPU32RM 4.4,
/// TBLS).
const TABLE_MASK: u16 = 0xffc0;
/// TBL's fixed bits.
const TABLE: u16 = 0xf800;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;
/// The interpolation fraction, Dx\[7:0\], and the entry number above
/// it.
const FRACTION_BITS: u32 = 8;
/// The fraction's bits.
const FRACTION_MASK: u32 = 0xff;
/// One half in the fraction: the rounding point.
const HALF: i64 = 0x80;
/// The integer part of an unrounded result as TBLSN keeps it, signed:
/// -2^23 to 2^23 - 1 (CPU32RM 4.4, TBLS condition codes).
const SIGNED_INTEGER: RangeInclusive<i64> = -(1 << 23)..=(1 << 23) - 1;
/// The integer part as TBLUN keeps it: 0 to 2^24 - 1 (CPU32RM 4.4,
/// TBLU condition codes).
const UNSIGNED_INTEGER: RangeInclusive<i64> = 0..=(1 << 24) - 1;

/// The second word: 0 xxx s r 0 t ss 000 yyy (CPU32RM 4.4, TBLS, TBLU).
mod extension {
    /// Where Dx, bits 14:12, starts.
    pub(super) const RESULT_SHIFT: u16 = 12;
    /// Bit 11: signed, TBLS and TBLSN.
    pub(super) const SIGNED: u16 = 1 << 11;
    /// Bit 10, R: unrounded, TBLSN and TBLUN.
    pub(super) const UNROUNDED: u16 = 1 << 10;
    /// Bit 8: set for the table form, clear for the register form.
    pub(super) const TABLE_FORM: u16 = 1 << 8;
    /// Where the size field, bits 7:6, starts.
    pub(super) const SIZE_SHIFT: u16 = 6;
    /// The bits that are zero in both forms: 15, 9 and 5:3.  Bits 2:0
    /// are Dyn in the register form, unused in the table form, where
    /// they "should be zero" and are ignored.
    pub(super) const RESERVED: u16 = 0x8238;
}

/// Registers the family's opcodes: the register form, mode 000, and
/// the table form's control modes.
pub(super) fn register(table: &mut Table) {
    table.set(
        TABLE_MASK,
        TABLE,
        |opcode| {
            kind::of(opcode) == kind::DATA_REGISTER || Category::CONTROL.admits(opcode & FIELD_MASK)
        },
        table_lookup,
    );
}

/// An illegal second word: the F-line exception (CPU32RM 6.2.8, 8.3.13
/// "F-line (Second word illegal)"), its own address stacked.
const fn line_f(core: &Core<'_>) -> Exception {
    Exception::Short {
        vector: Vector::LINE_F,
        pc: core.instruction_pc(),
    }
}

/// The entry `value` of `size`, signed or unsigned, widened.
const fn entry(value: u32, size: Size, signed: bool) -> i64 {
    if signed {
        size.sign_extend(value).cast_signed() as i64
    } else {
        (value & size.mask()) as i64
    }
}

/// `entry + interpolation / 256` rounded to nearest, where
/// `interpolation` is the difference times the fraction (CPU32RM 4.4,
/// the rounding tables of TBLS and TBLU).  TBLS reads the adjusted
/// difference's fraction n signed, -1 for n <= -1/2 and +1 for n >=
/// 1/2: a half rounds away from zero.  TBLU's table has no negative
/// n, though a falling table's difference is negative: its fraction
/// is taken as the two's complement low byte, so a half rounds up.
const fn rounded(entry: i64, interpolation: i64, signed: bool) -> i64 {
    let one = 1 << FRACTION_BITS;
    let adjusted = if signed {
        let part = interpolation % one;
        interpolation / one + (part >= HALF) as i64 - (part <= -HALF) as i64
    } else {
        (interpolation >> FRACTION_BITS) + ((interpolation & (one - 1)) >= HALF) as i64
    };
    entry + adjusted
}

/// TBLS, TBLSN, TBLU, TBLUN <ea>, Dx and Dym:Dyn, Dx; on $F800 also
/// LPSTOP.
fn table_lookup(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let extension = core.fetch_word()?;
    if opcode == system::LPSTOP && extension == system::LPSTOP_EXTENSION {
        return system::lpstop(core);
    }
    let register_form = kind::of(opcode) == kind::DATA_REGISTER;
    let Some(size) = Size::from_field(extension >> extension::SIZE_SHIFT) else {
        return Err(line_f(core));
    };
    if extension & extension::RESERVED != 0
        || (extension & extension::TABLE_FORM != 0) == register_form
    {
        return Err(line_f(core));
    }
    core.charge(timing::table_lookup(opcode, extension));
    let signed = extension & extension::SIGNED != 0;
    let result_register = usize::from(extension >> extension::RESULT_SHIFT & THREE_BITS);
    let independent = core.cpu.registers.data[result_register];
    let (first, second) = if register_form {
        let registers = &core.cpu.registers;
        (
            registers.data[low_register(opcode)],
            registers.data[usize::from(extension & THREE_BITS)],
        )
    } else {
        let operand = core.calculated_address(opcode & FIELD_MASK, size)?;
        let offset = (independent >> FRACTION_BITS & FRACTION_MASK) * size.bytes();
        let first = read_at(core, operand, offset, size)?;
        let second = read_at(core, operand, offset + size.bytes(), size)?;
        (first, second)
    };
    let (first, second) = (entry(first, size, signed), entry(second, size, signed));
    let fraction = i64::from(independent & FRACTION_MASK);
    let interpolation = (second - first) * fraction;
    let registers = &mut core.cpu.registers;
    if extension & extension::UNROUNDED != 0 {
        // The result with its fraction, Dx[31:8].Dx[7:0], sign or zero
        // extended, which a long's integer part may overflow.
        let unrounded = first * (1 << FRACTION_BITS) + interpolation;
        let integer = unrounded >> FRACTION_BITS;
        let range = if signed {
            SIGNED_INTEGER
        } else {
            UNSIGNED_INTEGER
        };
        let result = (unrounded as i32).cast_unsigned();
        registers.data[result_register] = result;
        registers.set_flag(status::N, result & Size::Long.sign_bit() != 0);
        registers.set_flag(status::Z, result == 0);
        registers.set_flag(status::V, !range.contains(&integer));
    } else {
        let result = (rounded(first, interpolation, signed) as i32).cast_unsigned() & size.mask();
        let destination = &mut registers.data[result_register];
        *destination = *destination & !size.mask() | result;
        registers.set_flag(status::N, result & size.sign_bit() != 0);
        registers.set_flag(status::Z, result == 0);
        registers.set_flag(status::V, false);
    }
    registers.set_flag(status::C, false);
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use super::rounded;
    use crate::address::Address;
    use crate::cpu::Cpu;
    use crate::exception::Exception;
    use crate::exception::Vector;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::registers::status;

    /// Where the tests' tables are.
    const TABLE: u32 = 0x2000;

    fn ccr(cpu: &Cpu) -> u16 {
        cpu.registers.sr() & status::CCR
    }

    /// Writes word entries from [`TABLE`].
    fn table(cpu: &mut Cpu, entries: &[u16]) {
        for (index, pair) in entries.chunks(2).enumerate() {
            let low = pair.get(1).copied().unwrap_or(0);
            poke_long(
                cpu,
                TABLE + 4 * index as u32,
                u32::from(pair[0]) << 16 | u32::from(low),
            );
        }
    }

    #[test]
    fn example_1_standard_usage() {
        // CPU32RM 4.6.1: TBLU.W (A0), D0 on entries 163 and 164, 1669
        // and 1679, Dx = $A380: 1674.
        let mut cpu = machine(&[0xf810, 0x0140]);
        let mut entries = vec![0; 165];
        entries[163] = 1669;
        entries[164] = 1679;
        table(&mut cpu, &entries);
        cpu.registers.set_address(0, TABLE);
        cpu.registers.data[0] = 0xffff_a380;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xffff_0000 | 1674);
        assert_eq!(ccr(&cpu), 0);
        assert_eq!(cpu.registers.pc, CODE + 4);
    }

    #[test]
    fn example_2_compressed_table() {
        // CPU32RM 4.6.2: entries 2 and 3, 1311 and 1966, Dx = $028E: 1674
        // (the manual's 1331 is a misprint of entry 2's 1311).
        let mut cpu = machine(&[0xf810, 0x0940]);
        table(&mut cpu, &[0, 0, 1311, 1966, 0]);
        cpu.registers.set_address(0, TABLE);
        cpu.registers.data[0] = 0x028e;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 1674);
    }

    #[test]
    fn example_3_falling_entries() {
        // CPU32RM 4.6.3: entries 11 and 12, 80 and 64, Dx = $0BD0: 67.
        // TBLS.W (A0), D0.
        let mut cpu = machine(&[0xf810, 0x0940]);
        let mut entries = vec![0; 13];
        entries[11] = 80;
        entries[12] = 64;
        table(&mut cpu, &entries);
        cpu.registers.set_address(0, TABLE);
        cpu.registers.data[0] = 0x0bd0;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 67);
    }

    #[test]
    fn example_4_unrounded_keeps_the_fraction() {
        // CPU32RM 4.6.4: TLI #1, 0010 0000 . 0111 0000, unrounded from
        // the registers: TBLSN.B D1:D2, D0 between $20 and $21 at
        // fraction $70; rounded, $20.
        let mut cpu = machine(&[0xf801, 0x0c02, 0xf801, 0x0802]);
        cpu.registers.data[1] = 0x20;
        cpu.registers.data[2] = 0x21;
        cpu.registers.data[0] = 0x70;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x2070);
        cpu.registers.data[0] = 0xab70;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xab20);
    }

    #[test]
    fn signed_rounding_is_half_away_from_zero() {
        assert_eq!(rounded(0, -0x80, true), -1);
        assert_eq!(rounded(0, -0x7f, true), 0);
        assert_eq!(rounded(0, 0x80, true), 1);
        assert_eq!(rounded(2, -0x180, true), 0);
        assert_eq!(rounded(5, -0x80, false), 5);
        assert_eq!(rounded(5, -0x81, false), 4);
    }

    #[test]
    fn unrounded_byte_results_are_sign_or_zero_extended() {
        // TBLSN.B D1:D2, D0 between -2 and -4 at one half: -3.0
        let mut cpu = machine(&[0xf801, 0x0c02, 0xf801, 0x0402]);
        cpu.registers.data[1] = 0xfe;
        cpu.registers.data[2] = 0xfc;
        cpu.registers.data[0] = 0x80;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], (-0x300i32).cast_unsigned());
        assert_eq!(ccr(&cpu), status::N);
        // TBLUN.B: 254 to 252, 253.0
        cpu.registers.data[0] = 0x80;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xfd00);
        assert_eq!(ccr(&cpu), 0);
    }

    #[test]
    fn unrounded_long_sets_v_past_24_bits() {
        // TBLUN.L D1:D2, D0 from $0100_0000
        let mut cpu = machine(&[0xf801, 0x0482]);
        cpu.registers.data[1] = 0x0100_0000;
        cpu.registers.data[2] = 0x0100_0000;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0);
        assert_eq!(ccr(&cpu), status::Z | status::V);
    }

    #[test]
    fn an_illegal_second_word_is_line_f() {
        // TBLS with size 11; the register form with bit 8 set.
        for extension in [0x08c0, 0x0900] {
            let mut cpu = machine(&[0xf801, extension]);
            cpu.registers.set_address(7, 0x8000);
            step(&mut cpu, 1);
            assert_eq!(
                cpu.exception_taken(),
                Some(Exception::Short {
                    vector: Vector::LINE_F,
                    pc: Address(CODE),
                })
            );
        }
    }

    #[test]
    fn tbl_with_d0_shares_lpstops_first_word() {
        // TBLU.W D0:D1, D2
        let mut cpu = machine(&[0xf800, 0x2041]);
        cpu.registers.data[0] = 100;
        cpu.registers.data[1] = 200;
        cpu.registers.data[2] = 0x40;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], 125);
    }
}
