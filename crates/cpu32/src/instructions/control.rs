//! Program control instructions (CPU32RM 4.3.8): NOP, and the jumps,
//! calls and returns.  The branches are in `branch`.

use crate::address::Address;
use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::Operand;
use crate::exception::Exception;
use crate::instructions::Flow;
use crate::instructions::Table;
use crate::instructions::branch::STACK_POINTER;
use crate::instructions::branch::push_return;
use crate::size::Size;

/// NOP's opcode (CPU32RM 4.4, NOP).
const NOP: u16 = 0x4e71;
/// RTD #d16 (CPU32RM 4.4, RTD).
const RTD: u16 = 0x4e74;
/// RTS (CPU32RM 4.4, RTS).
const RTS: u16 = 0x4e75;
/// RTR (CPU32RM 4.4, RTR).
const RTR: u16 = 0x4e77;
/// JSR and JMP: 0100 1110 1j, then a control address (CPU32RM 4.4,
/// JSR, JMP).
const JUMP_MASK: u16 = 0xffc0;
/// JSR's fixed bits.
const JSR: u16 = 0x4e80;
/// JMP's fixed bits.
const JMP: u16 = 0x4ec0;
/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    let control = |opcode| Category::CONTROL.admits(opcode & FIELD_MASK);
    table.set(u16::MAX, NOP, |_| true, nop);
    table.set(u16::MAX, RTD, |_| true, rtd);
    table.set(u16::MAX, RTS, |_| true, rts);
    table.set(u16::MAX, RTR, |_| true, rtr);
    table.set(JUMP_MASK, JSR, control, jsr);
    table.set(JUMP_MASK, JMP, control, jmp);
}

/// NOP: nothing.
fn nop(_core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    Ok(Flow::Next)
}

/// The address a control mode names.
fn control_address(core: &mut Core<'_>, opcode: u16) -> Result<u32, Exception> {
    match core.calculated_address(opcode & FIELD_MASK, Size::Long)? {
        Operand::Data(Address(address)) | Operand::Program(Address(address)) => Ok(address),
        _ => Err(core.illegal()),
    }
}

/// JMP <ea>: the address to PC (CPU32RM 4.4, JMP).
fn jmp(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    core.cpu.registers.pc = control_address(core, opcode)?;
    Ok(Flow::Changed)
}

/// JSR <ea>: the address after the instruction pushed, then the
/// address to PC (CPU32RM 4.4, JSR).  The address is computed first,
/// from SP as it was.
fn jsr(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let target = control_address(core, opcode)?;
    push_return(core)?;
    core.cpu.registers.pc = target;
    Ok(Flow::Changed)
}

/// The `size` at SP offset by `offset`.
fn stacked(core: &mut Core<'_>, offset: u32, size: Size) -> Result<u32, Exception> {
    let sp = core.cpu.registers.address(STACK_POINTER);
    core.read_data(Address(sp.wrapping_add(offset)), size)
}

/// Pops `bytes` and more off the stack, then returns to `pc`.
fn return_to(core: &mut Core<'_>, pc: u32, bytes: u32) -> Flow {
    let registers = &mut core.cpu.registers;
    let sp = registers.address(STACK_POINTER).wrapping_add(bytes);
    registers.set_address(STACK_POINTER, sp);
    registers.pc = pc;
    Flow::Changed
}

/// RTS: PC from the stack (CPU32RM 4.4, RTS).
fn rts(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    let pc = stacked(core, 0, Size::Long)?;
    Ok(return_to(core, pc, Size::Long.bytes()))
}

/// RTD #d16: PC from the stack, then SP + 4 + d16 to SP, d16 sign
/// extended (CPU32RM 4.4, RTD).
fn rtd(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    let displacement = Size::Word.sign_extend(u32::from(core.fetch_word()?));
    let pc = stacked(core, 0, Size::Long)?;
    Ok(return_to(
        core,
        pc,
        Size::Long.bytes().wrapping_add(displacement),
    ))
}

/// RTR: the CCR from the low byte of the stacked word, then PC from
/// the stack; SR's upper byte unaffected (CPU32RM 4.4, RTR).
fn rtr(core: &mut Core<'_>, _opcode: u16) -> Result<Flow, Exception> {
    let ccr = stacked(core, 0, Size::Word)?;
    let pc = stacked(core, Size::Word.bytes(), Size::Long)?;
    core.cpu.registers.set_ccr(ccr as u8);
    Ok(return_to(core, pc, Size::Word.bytes() + Size::Long.bytes()))
}

#[cfg(test)]
mod tests {
    use super::NOP;
    use crate::cpu::Cpu;
    use crate::cpu::Stop;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::registers::status;

    /// A stack in the test RAM.
    const STACK: u32 = 0x4000;
    /// A subroutine's address in the test RAM.
    const TARGET: u32 = 0x2468;

    fn stacked(code: &[u16]) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.ssp = STACK;
        cpu
    }

    #[test]
    fn nop_changes_only_pc() {
        let mut cpu = machine(&[NOP]);
        let before = cpu.registers.clone();
        assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
        let mut expected = before;
        expected.pc = CODE + 2;
        assert_eq!(cpu.registers, expected);
    }

    #[test]
    fn jmp_through_an_address_register_and_absolute() {
        // JMP (A0)
        let mut cpu = stacked(&[0x4ed0]);
        cpu.registers.set_address(0, TARGET);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, TARGET);
        // JMP (xxx).L
        let mut cpu = stacked(&[0x4ef9, 0x0000, TARGET as u16]);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, TARGET);
    }

    #[test]
    fn jmp_pc_relative() {
        // JMP (-4, PC): from the extension word's address.
        let mut cpu = stacked(&[0x4efa, 0xfffc]);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE - 2);
    }

    #[test]
    fn jsr_and_rts_round_trip() {
        // JSR (d16, A7) computes from SP before the push.
        let mut cpu = stacked(&[0x4eaf, 0x0010]);
        poke_long(&mut cpu, STACK + 0x10, 0x4e75_0000);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, STACK + 0x10);
        assert_eq!(cpu.registers.ssp, STACK - 4);
        assert_eq!(peek_long(&cpu, STACK - 4), CODE + 4);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, CODE + 4);
        assert_eq!(cpu.registers.ssp, STACK);
    }

    #[test]
    fn rtd_deallocates_its_displacement() {
        let mut cpu = stacked(&[0x4e74, 0x000c]);
        poke_long(&mut cpu, STACK, TARGET);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, TARGET);
        assert_eq!(cpu.registers.ssp, STACK + 4 + 12);
        // A negative displacement.
        let mut cpu = stacked(&[0x4e74, 0xfffc]);
        poke_long(&mut cpu, STACK, TARGET);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.ssp, STACK);
    }

    #[test]
    fn rtr_restores_only_the_ccr() {
        let mut cpu = stacked(&[0x4e77]);
        poke_long(&mut cpu, STACK, 0xa71f_0000 | TARGET >> 16);
        poke_long(&mut cpu, STACK + 4, TARGET << 16);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, TARGET);
        assert_eq!(cpu.registers.ssp, STACK + 6);
        assert_eq!(
            cpu.registers.sr(),
            status::S | status::INTERRUPT_MASK | 0x1f
        );
    }

    #[test]
    fn user_rts_uses_usp() {
        let mut cpu = stacked(&[0x4e75]);
        cpu.registers.set_sr(0);
        cpu.registers.usp = STACK + 0x100;
        poke_long(&mut cpu, STACK + 0x100, TARGET);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.pc, TARGET);
        assert_eq!(cpu.registers.usp, STACK + 0x104);
        assert_eq!(cpu.registers.ssp, STACK);
    }
}
