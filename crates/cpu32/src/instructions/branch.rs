//! Conditional and unconditional branches (CPU32RM 4.3.8): Bcc, BRA,
//! BSR, DBcc and Scc.

use crate::address::Address;
use crate::cpu::Core;
use crate::effective_address::Category;
use crate::exception::Exception;
use crate::flags;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::instructions::data_movement::low_register;
use crate::size::Size;
use crate::timing;
use crate::timing::Decrement;

/// Bcc, BRA and BSR: 0110 cccc dddddddd (CPU32RM 4.4, Bcc, BRA, BSR).
const BRANCH_MASK: u16 = 0xf000;
/// Their fixed bits.
const BRANCH: u16 = 0x6000;
/// DBcc: 0101 cccc 1100 1rrr (CPU32RM 4.4, DBcc).
const DBCC_MASK: u16 = 0xf0f8;
/// DBcc's fixed bits.
const DBCC: u16 = 0x50c8;
/// Scc: 0101 cccc 11, then a data alterable address (CPU32RM 4.4,
/// Scc).
const SCC_MASK: u16 = 0xf0c0;
/// Scc's fixed bits.
const SCC: u16 = 0x50c0;
/// Where the condition field, bits 11:8, starts.
const CONDITION_SHIFT: u16 = 8;
/// The condition field: BRA's "true", BSR's "false" (CPU32RM Table
/// 4-10; 4.4, BRA, BSR).
const CONDITION_MASK: u16 = 0xf;
/// Condition 0000 in the branch line: BRA.
const ALWAYS: u16 = 0b0000;
/// Condition 0001 in the branch line: BSR.
const SUBROUTINE: u16 = 0b0001;
/// The 8-bit displacement field.
const DISPLACEMENT_MASK: u16 = 0xff;
/// An 8-bit displacement of $00: a 16-bit one follows (CPU32RM 4.4,
/// Bcc).
const WORD_DISPLACEMENT: u16 = 0x00;
/// An 8-bit displacement of $FF: a 32-bit one follows.
const LONG_DISPLACEMENT: u16 = 0xff;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;
/// The value of Dn's low word that ends a DBcc loop (CPU32RM 4.4,
/// DBcc).
const EXPIRED: u32 = 0xffff;
/// The displacement, -4, that loops DBcc over the one-word instruction
/// before it (CPU32RM 1.1.2, Figure 1-1).
const LOOP_DISPLACEMENT: u16 = 0xfffc;
/// A7, the stack pointer.
pub(super) const STACK_POINTER: usize = 7;
/// What Scc stores when the condition holds (CPU32RM 4.4, Scc).
const SET: u32 = 0xff;

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set(BRANCH_MASK, BRANCH, |_| true, branch);
    table.set(DBCC_MASK, DBCC, |_| true, dbcc);
    table.set(
        SCC_MASK,
        SCC,
        |opcode| {
            Category::DATA
                .and(Category::ALTERABLE)
                .admits(opcode & FIELD_MASK)
        },
        scc,
    );
}

/// The condition field of a branch line, DBcc or Scc opcode.
const fn condition(opcode: u16) -> u16 {
    opcode >> CONDITION_SHIFT & CONDITION_MASK
}

/// Bcc, BRA, BSR: the displacement from the address after the opcode
/// word, 8 bits in the opcode, or 16 or 32 in the words that follow
/// (CPU32RM 4.4, Bcc).
fn branch(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let base = core.cpu.registers.pc;
    let (displacement, size) = match opcode & DISPLACEMENT_MASK {
        WORD_DISPLACEMENT => (
            Size::Word.sign_extend(u32::from(core.fetch_word()?)),
            Size::Word,
        ),
        LONG_DISPLACEMENT => (core.fetch_long()?, Size::Long),
        _ => (Size::Byte.sign_extend(u32::from(opcode)), Size::Byte),
    };
    let target = base.wrapping_add(displacement);
    match condition(opcode) {
        ALWAYS => {}
        SUBROUTINE => push_return(core)?,
        condition if flags::test(&core.cpu.registers, condition) => {
            core.charge(timing::branch_taken(size));
        }
        _ => return Ok(Flow::Next),
    }
    core.cpu.registers.pc = target;
    Ok(Flow::Changed)
}

/// Pushes PC, the return address, on the stack: BSR, JSR.
pub(super) fn push_return(core: &mut Core<'_>) -> Result<(), Exception> {
    let registers = &mut core.cpu.registers;
    let pc = registers.pc;
    let sp = registers
        .address(STACK_POINTER)
        .wrapping_sub(Size::Long.bytes());
    registers.set_address(STACK_POINTER, sp);
    core.write_data(Address(sp), Size::Long, pc)
}

/// DBcc Dn, label: if the condition is false, Dn's low word less one,
/// and a branch unless it reached -1 (CPU32RM 4.4, DBcc).  The 16-bit
/// displacement is from its own word.  Taken back over a one-word
/// instruction, it enters loop mode, and in loop mode costs its loop
/// mode rows (CPU32RM 1.1.2, 8.3.11; [`Core::looping`]).
fn dbcc(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let base = core.cpu.registers.pc;
    let word = core.fetch_word()?;
    let looping = core.looping();
    let outcome = if flags::test(&core.cpu.registers, condition(opcode)) {
        Decrement::ConditionTrue
    } else {
        let register = &mut core.cpu.registers.data[low_register(opcode)];
        let count = register.wrapping_sub(1) & Size::Word.mask();
        *register = *register & !Size::Word.mask() | count;
        if count == EXPIRED {
            Decrement::Expired
        } else {
            Decrement::Taken
        }
    };
    if looping {
        let last_tail = core.last_tail();
        core.adjust(timing::decrement_and_branch_looping(outcome, last_tail));
    } else {
        core.charge(timing::decrement_and_branch(outcome));
    }
    if outcome != Decrement::Taken {
        return Ok(Flow::Next);
    }
    if word == LOOP_DISPLACEMENT {
        core.enter_loop_mode();
    }
    core.cpu.registers.pc = base.wrapping_add(Size::Word.sign_extend(u32::from(word)));
    Ok(Flow::Changed)
}

/// Scc <ea>: the byte all ones if the condition holds, else zero; the
/// condition codes unaffected (CPU32RM 4.4, Scc).
fn scc(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let destination = core.calculated_address(opcode & FIELD_MASK, Size::Byte)?;
    let value = if flags::test(&core.cpu.registers, condition(opcode)) {
        SET
    } else {
        0
    };
    core.write_operand(destination, Size::Byte, value)?;
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use crate::cpu::Cpu;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::step;
    use crate::registers::status;

    /// A stack in the test RAM.
    const STACK: u32 = 0x4000;

    fn with_ccr(code: &[u16], ccr: u16) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.set_ccr(ccr as u8);
        cpu.registers.ssp = STACK;
        cpu
    }

    #[test]
    fn bra_byte_word_and_long_displacements() {
        // BRA.S *+$12
        let mut cpu = with_ccr(&[0x6010], 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE + 2 + 0x10);
        // BRA.W: $00 selects the word that follows, from its address.
        let mut cpu = with_ccr(&[0x6000, 0xfffe], 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE);
        // BRA.L: $FF selects the long word that follows.
        let mut cpu = with_ccr(&[0x60ff, 0x0000, 0x0100], 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE + 2 + 0x100);
        // BRA.S back: a negative byte.
        let mut cpu = with_ccr(&[0x60fe], 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE);
    }

    #[test]
    fn bcc_taken_and_not_taken() {
        // BEQ.S *+$12
        let mut cpu = with_ccr(&[0x6710], status::Z);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE + 0x12);
        let mut cpu = with_ccr(&[0x6710], 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE + 2);
        // BNE.L not taken still skips its displacement.
        let mut cpu = with_ccr(&[0x66ff, 0x0000, 0x0100], status::Z);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE + 6);
    }

    #[test]
    fn a_taken_branch_costs_more() {
        let not_taken = {
            let mut cpu = with_ccr(&[0x6710], 0);
            step(&mut cpu, 1).cycles
        };
        let mut cpu = with_ccr(&[0x6710], status::Z);
        assert!(step(&mut cpu, 1).cycles > not_taken);
    }

    #[test]
    fn bsr_pushes_the_address_after_its_displacement() {
        // BSR.W *+$102
        let mut cpu = with_ccr(&[0x6100, 0x0100], 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE + 2 + 0x100);
        assert_eq!(cpu.registers.ssp, STACK - 4);
        assert_eq!(peek_long(&cpu, STACK - 4), CODE + 4);
    }

    #[test]
    fn dbcc_counts_down_and_wraps() {
        // DBF D1, *-0 (back to itself)
        let mut cpu = with_ccr(&[0x51c9, 0xfffe], 0);
        cpu.registers.data[1] = 0x1234_0001;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0x1234_0000);
        assert_eq!(cpu.registers.pc, CODE);
        // From zero the low word wraps to -1 and the loop ends.
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0x1234_ffff);
        assert_eq!(cpu.registers.pc, CODE + 4);
    }

    #[test]
    fn dbcc_with_its_condition_true_does_nothing() {
        // DBEQ D1, *-0
        let mut cpu = with_ccr(&[0x57c9, 0xfffe], status::Z);
        cpu.registers.data[1] = 5;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 5);
        assert_eq!(cpu.registers.pc, CODE + 4);
    }

    #[test]
    fn scc_sets_a_byte_and_keeps_the_flags() {
        // SNE D2; SEQ (A0)
        let mut cpu = with_ccr(&[0x56c2, 0x57d0], status::Z | status::C);
        cpu.registers.data[2] = 0x1234_5678;
        cpu.registers.set_address(0, 0x2000);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.data[2], 0x1234_5600);
        assert_eq!(peek_long(&cpu, 0x2000) >> 24, 0xff);
        assert_eq!(cpu.registers.ccr(), (status::Z | status::C) as u8);
    }
}
