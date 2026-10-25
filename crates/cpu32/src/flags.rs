//! Condition codes: the general cases of CPU32RM Table 4-1, and the
//! condition tests of Table 4-10.  An instruction with a special
//! definition (BCD, shifts, CHK2, multiply, divide) sets its own.

use crate::registers::Registers;
use crate::registers::status;
use crate::size::Size;

/// The most significant bit of `value` taken as `size`.
const fn msb(value: u32, size: Size) -> bool {
    value & size.sign_bit() != 0
}

/// Whether `value` is zero taken as `size`.
const fn is_zero(value: u32, size: Size) -> bool {
    value & size.mask() == 0
}

/// N and Z from `result`, V and C cleared, X unaffected: AND, EOR,
/// MOVEQ, MOVE, OR, CLR, EXT, NOT, TAS, TST.
pub(crate) const fn logical(registers: &mut Registers, result: u32, size: Size) {
    registers.set_flag(status::N, msb(result, size));
    registers.set_flag(status::Z, is_zero(result, size));
    registers.set_flag(status::V | status::C, false);
}

/// X, N, Z, V and C of `result = destination + source`: ADD, ADDI,
/// ADDQ.
pub(crate) const fn add(
    registers: &mut Registers,
    source: u32,
    destination: u32,
    result: u32,
    size: Size,
) {
    let (s, d, r) = (msb(source, size), msb(destination, size), msb(result, size));
    let carry = s && d || !r && d || s && !r;
    registers.set_flag(status::X | status::C, carry);
    registers.set_flag(status::N, r);
    registers.set_flag(status::Z, is_zero(result, size));
    registers.set_flag(status::V, s && d && !r || !s && !d && r);
}

/// N, Z, V and C of `result = destination - source`: CMP, CMPI, CMPM;
/// X unaffected.
pub(crate) const fn compare(
    registers: &mut Registers,
    source: u32,
    destination: u32,
    result: u32,
    size: Size,
) {
    let (s, d, r) = (msb(source, size), msb(destination, size), msb(result, size));
    registers.set_flag(status::C, s && !d || r && !d || s && r);
    registers.set_flag(status::N, r);
    registers.set_flag(status::Z, is_zero(result, size));
    registers.set_flag(status::V, !s && d && !r || s && !d && r);
}

/// X, N, Z, V and C of `result = destination - source`: SUB, SUBI,
/// SUBQ.
pub(crate) const fn subtract(
    registers: &mut Registers,
    source: u32,
    destination: u32,
    result: u32,
    size: Size,
) {
    compare(registers, source, destination, result, size);
    registers.set_flag(status::X, registers.flag(status::C));
}

/// Condition test `condition`, the four-bit field of Bcc, DBcc, Scc
/// and TRAPcc (CPU32RM 4.3.10, Table 4-10).
#[must_use]
pub(crate) const fn test(registers: &Registers, condition: u16) -> bool {
    let c = registers.flag(status::C);
    let v = registers.flag(status::V);
    let z = registers.flag(status::Z);
    let n = registers.flag(status::N);
    match condition & 0xf {
        0b0000 => true,
        0b0001 => false,
        0b0010 => !c && !z,
        0b0011 => c || z,
        0b0100 => !c,
        0b0101 => c,
        0b0110 => !z,
        0b0111 => z,
        0b1000 => !v,
        0b1001 => v,
        0b1010 => !n,
        0b1011 => n,
        0b1100 => n == v,
        0b1101 => n != v,
        0b1110 => n == v && !z,
        _ => z || n != v,
    }
}

#[cfg(test)]
mod tests {
    use super::add;
    use super::compare;
    use super::logical;
    use super::subtract;
    use super::test;
    use crate::registers::Registers;
    use crate::registers::status;
    use crate::size::Size;

    fn ccr(registers: &Registers) -> u16 {
        registers.sr() & status::CCR
    }

    #[test]
    fn logical_keeps_x() {
        let mut registers = Registers::default();
        registers.set_ccr(0xff);
        logical(&mut registers, 0x80, Size::Byte);
        assert_eq!(ccr(&registers), status::X | status::N);
    }

    #[test]
    fn add_overflows_and_carries() {
        let mut registers = Registers::default();
        add(&mut registers, 0x7fff, 1, 0x8000, Size::Word);
        assert_eq!(ccr(&registers), status::N | status::V);
        add(&mut registers, 0xffff, 1, 0x1_0000, Size::Word);
        assert_eq!(ccr(&registers), status::X | status::Z | status::C);
    }

    #[test]
    fn subtract_borrows() {
        let mut registers = Registers::default();
        subtract(&mut registers, 1, 0, 0xffff_ffff, Size::Long);
        assert_eq!(ccr(&registers), status::X | status::N | status::C);
        registers.set_ccr(0);
        compare(&mut registers, 1, 0, 0xffff_ffff, Size::Long);
        assert_eq!(ccr(&registers), status::N | status::C);
    }

    #[test]
    fn signed_tests() {
        let mut registers = Registers::default();
        registers.set_ccr((status::N | status::V) as u8);
        assert!(test(&registers, 0b1100));
        assert!(!test(&registers, 0b1101));
        assert!(test(&registers, 0b1110));
        registers.set_ccr(status::Z as u8);
        assert!(test(&registers, 0b1111));
    }
}
