//! Logic instructions (CPU32RM 4.3.4, Table 4-4) and TAS.  The CCR and
//! SR forms of ANDI, EORI and ORI are the system control
//! instructions'.

use crate::address::Address;
use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::Operand;
use crate::effective_address::kind;
use crate::exception::Exception;
use crate::flags;
use crate::instructions::Flow;
use crate::instructions::Handler;
use crate::instructions::Table;
use crate::instructions::arithmetic::CMP_LINE;
use crate::instructions::arithmetic::LINE_MASK;
use crate::instructions::arithmetic::Operation;
use crate::instructions::arithmetic::SINGLE_MASK;
use crate::instructions::arithmetic::Unary;
use crate::instructions::arithmetic::is_any_sized;
use crate::instructions::arithmetic::is_data_alterable_sized;
use crate::instructions::arithmetic::is_from_data;
use crate::instructions::arithmetic::is_to_data_alterable;
use crate::instructions::arithmetic::is_to_memory;
use crate::instructions::arithmetic::memory_destination_handler;
use crate::instructions::arithmetic::register_destination_handler;
use crate::instructions::arithmetic::register_immediate;
use crate::instructions::arithmetic::single_handler;
use crate::instructions::arithmetic::size;
use crate::instructions::data_movement::FIELD_MASK;
use crate::registers::Registers;
use crate::size::Size;
use crate::timing::AddressTable;

/// OR: line 1000 (CPU32RM 4.4, OR).
const OR_LINE: u16 = 0x8000;
/// AND: line 1100 (CPU32RM 4.4, AND).
const AND_LINE: u16 = 0xc000;
/// ORI: 0000 0000 ss, then the destination (CPU32RM 4.4, ORI).
const ORI: u16 = 0x0000;
/// ANDI: 0000 0010 (CPU32RM 4.4, ANDI).
const ANDI: u16 = 0x0200;
/// EORI: 0000 1010 (CPU32RM 4.4, EORI).
const EORI: u16 = 0x0a00;
/// NOT: 0100 0110 ss, then the operand (CPU32RM 4.4, NOT).
const NOT: u16 = 0x4600;
/// TST: 0100 1010 ss, then the operand (CPU32RM 4.4, TST).
const TST: u16 = 0x4a00;
/// TAS: 0100 1010 11, then the operand (CPU32RM 4.4, TAS).
const TAS: u16 = 0x4ac0;
/// TAS's opcode bits but the operand.
const TAS_MASK: u16 = 0xffc0;
/// The bit TAS sets: the byte's high-order bit.
const TAS_BIT: u32 = 0x80;

/// AND, ANDI: destination & source (CPU32RM 4.4, AND).
struct And;

impl Operation for And {
    fn combine(destination: u32, source: u32) -> u32 {
        destination & source
    }

    fn apply(registers: &mut Registers, source: u32, destination: u32, size: Size) -> u32 {
        logical::<Self>(registers, source, destination, size)
    }
}

/// OR, ORI: destination | source (CPU32RM 4.4, OR).
struct Or;

impl Operation for Or {
    fn combine(destination: u32, source: u32) -> u32 {
        destination | source
    }

    fn apply(registers: &mut Registers, source: u32, destination: u32, size: Size) -> u32 {
        logical::<Self>(registers, source, destination, size)
    }
}

/// EOR, EORI: destination ^ source (CPU32RM 4.4, EOR).
struct Eor;

impl Operation for Eor {
    fn combine(destination: u32, source: u32) -> u32 {
        destination ^ source
    }

    fn apply(registers: &mut Registers, source: u32, destination: u32, size: Size) -> u32 {
        logical::<Self>(registers, source, destination, size)
    }
}

/// NOT: the ones complement (CPU32RM 4.4, NOT).
struct Not;

impl Unary for Not {
    fn apply(registers: &mut Registers, operand: u32, size: Size) -> u32 {
        let result = !operand & size.mask();
        flags::logical(registers, result, size);
        result
    }
}

/// A logical operation's result, N and Z from it, V and C clear, X
/// unaffected (CPU32RM Table 4-1).
fn logical<O: Operation>(
    registers: &mut Registers,
    source: u32,
    destination: u32,
    size: Size,
) -> u32 {
    let result = O::combine(destination, source) & size.mask();
    flags::logical(registers, result, size);
    result
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set_by(
        LINE_MASK,
        OR_LINE,
        is_from_data,
        register_destination_handler::<Or>,
    );
    table.set_by(
        LINE_MASK,
        OR_LINE,
        is_to_memory,
        memory_destination_handler::<Or>,
    );
    table.set_by(
        LINE_MASK,
        AND_LINE,
        is_from_data,
        register_destination_handler::<And>,
    );
    table.set_by(
        LINE_MASK,
        AND_LINE,
        is_to_memory,
        memory_destination_handler::<And>,
    );
    table.set_by(
        LINE_MASK,
        CMP_LINE,
        is_to_data_alterable,
        memory_destination_handler::<Eor>,
    );
    register_immediate::<Or>(table, ORI);
    register_immediate::<And>(table, ANDI);
    register_immediate::<Eor>(table, EORI);
    table.set_by(
        SINGLE_MASK,
        NOT,
        is_data_alterable_sized,
        single_handler::<Not>,
    );
    table.set_by(SINGLE_MASK, TST, is_any_sized, tst_handler);
    table.set(TAS_MASK, TAS, is_tas, tas);
}

/// TST's handler for `opcode`, specialized by size and operand kind.
fn tst_handler(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE =>
        specialize_source!(kind::of(opcode & FIELD_MASK), OPERAND => tst::<SIZE, OPERAND>))
}

/// TST <ea>: N and Z of the operand, V and C clear; any mode, An not
/// for a byte (CPU32RM 4.4, TST).  `SIZE` is a [`Size::index`],
/// `OPERAND` the operand's kind.
fn tst<const SIZE: u8, const OPERAND: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let operand =
        core.effective_address_of(OPERAND, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    let value = core.read_operand(operand, size)?;
    flags::logical(&mut core.cpu.registers, value, size);
    Ok(Flow::Next)
}

/// TAS: a data alterable byte (CPU32RM 4.4, TAS).
fn is_tas(opcode: u16) -> bool {
    Category::DATA
        .and(Category::ALTERABLE)
        .admits(opcode & FIELD_MASK)
}

/// TAS <ea>: N and Z of the byte, V and C clear, then its bit 7 set
/// (CPU32RM 4.4, TAS).  In memory the read and the write are one
/// indivisible read-modify-write cycle pair: no other access comes
/// between them here, since the core runs alone and an instruction is
/// never interrupted.  A fault in either is marked RM and aborts the
/// instruction, the write not released (CPU32RM 6.3.1.2).
fn tas(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let operand = core.calculated_address(opcode & FIELD_MASK, Size::Byte)?;
    let Operand::Data(address) = operand else {
        let value = core.read_operand(operand, Size::Byte)?;
        flags::logical(&mut core.cpu.registers, value, Size::Byte);
        return core
            .write_operand(operand, Size::Byte, value | TAS_BIT)
            .map(|()| Flow::Next);
    };
    test_and_set(core, address)
}

/// TAS's read-modify-write at `address`.
fn test_and_set(core: &mut Core<'_>, address: Address) -> Result<Flow, Exception> {
    let space = core.cpu.registers.data_space();
    let value = core
        .read(address, Size::Byte, space)
        .map_err(Exception::in_read_modify_write)?;
    flags::logical(&mut core.cpu.registers, value, Size::Byte);
    core.write_unreleased(address, Size::Byte, space, value | TAS_BIT)
        .map_err(Exception::in_read_modify_write)?;
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use crate::exception::Exception;
    use crate::exception::Vector;
    use crate::instructions::data_movement::tests::RAM_END;
    use crate::instructions::data_movement::tests::ccr;
    use crate::instructions::data_movement::tests::peek_byte;
    use crate::instructions::data_movement::tests::peek_word;
    use crate::instructions::data_movement::tests::poke_word;
    use crate::instructions::data_movement::tests::prepared;
    use crate::instructions::data_movement::tests::taken;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::registers::status;

    #[test]
    fn and_or_eor_set_n_and_z_and_keep_x() {
        // AND.W D1, D0; OR.B #$F0, D2; EOR.W D1, D3
        let mut cpu = machine(&[0xc041, 0x843c, 0x00f0, 0xb343]);
        cpu.registers.data[0] = 0xffff_00ff;
        cpu.registers.data[1] = 0x0000_ff00;
        cpu.registers.data[2] = 0x0f;
        cpu.registers.data[3] = 0x1234_ff00;
        cpu.registers
            .set_ccr((status::X | status::V | status::C) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xffff_0000);
        assert_eq!(ccr(&cpu), status::X | status::Z);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], 0xff);
        assert_eq!(ccr(&cpu), status::X | status::N);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[3], 0x1234_0000);
        assert_eq!(ccr(&cpu), status::X | status::Z);
    }

    #[test]
    fn and_and_or_to_memory() {
        // AND.L D0, (A0); OR.W D0, (A0)
        let mut cpu = machine(&[0xc190, 0x8150]);
        cpu.registers.set_address(0, 0x3000);
        cpu.registers.data[0] = 0x8000_0001;
        poke_long(&mut cpu, 0x3000, 0xffff_0000);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, 0x3000), 0x8000_0000);
        assert_eq!(ccr(&cpu), status::N);
        step(&mut cpu, 1);
        assert_eq!(peek_word(&cpu, 0x3000), 0x8001);
    }

    #[test]
    fn immediate_logic() {
        // ORI.W #$0101, (A0); ANDI.B #$0F, D0; EORI.L #-1, D1
        let mut cpu = machine(&[0x0050, 0x0101, 0x0200, 0x000f, 0x0a81, 0xffff, 0xffff]);
        cpu.registers.set_address(0, 0x3000);
        cpu.registers.data[0] = 0x1234_56f0;
        step(&mut cpu, 1);
        assert_eq!(peek_word(&cpu, 0x3000), 0x0101);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x1234_5600);
        assert_eq!(ccr(&cpu), status::Z);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0xffff_ffff);
        assert_eq!(ccr(&cpu), status::N);
        assert_eq!(cpu.registers.pc, CODE + 14);
    }

    #[test]
    fn not_complements() {
        // NOT.B D0
        let mut cpu = machine(&[0x4600]);
        cpu.registers.data[0] = 0x1234_56ff;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x1234_5600);
        assert_eq!(ccr(&cpu), status::Z);
    }

    #[test]
    fn tst_takes_an_immediates_and_pc_relative() {
        // TST.W A0; TST.L #$80000000; TST.B (2, PC)
        let mut cpu = machine(&[0x4a48, 0x4abc, 0x8000, 0x0000, 0x4a3a, 0x0002, 0x0000]);
        cpu.registers.set_address(0, 0x0001_0000);
        cpu.registers
            .set_ccr((status::X | status::V | status::C) as u8);
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::X | status::Z);
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::X | status::N);
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::X | status::Z);
    }

    #[test]
    fn tas_tests_then_sets_bit_7() {
        // TAS D0; TAS (A0)
        let mut cpu = machine(&[0x4ac0, 0x4ad0]);
        cpu.registers.set_address(0, 0x3000);
        poke_word(&mut cpu, 0x3000, 0x8100);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x80);
        assert_eq!(ccr(&cpu), status::Z);
        step(&mut cpu, 1);
        assert_eq!(peek_byte(&cpu, 0x3000), 0x81);
        assert_eq!(ccr(&cpu), status::N);
    }

    #[test]
    fn a_tas_fault_is_marked_read_modify_write() {
        // TAS (A0) past RAM.
        let mut cpu = prepared(&[0x4ad0]);
        cpu.registers.set_address(0, RAM_END);
        step(&mut cpu, 1);
        let Some(Exception::Fault(fault)) = cpu.exception_taken() else {
            panic!("a bus error");
        };
        assert_eq!(fault.vector, Vector::BUS_ERROR);
        assert!(fault.read_modify_write);
    }

    #[test]
    fn an_odd_logical_operand_is_an_address_error() {
        // AND.W (A0), D0
        let mut cpu = prepared(&[0xc050]);
        cpu.registers.set_address(0, 0x3001);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::ADDRESS_ERROR));
    }

    #[test]
    fn illegal_combinations_are_not_claimed() {
        // AND.W A0, D0; TST.B A0; TAS (d8, PC, Xn); NOT.W A0;
        // ORI.W #, (d16, PC).
        for opcode in [0xc048, 0x4a08, 0x4afb, 0x4648, 0x007a] {
            let mut cpu = prepared(&[opcode, 0, 0, 0]);
            step(&mut cpu, 1);
            assert_eq!(
                taken(&cpu),
                Some(Vector::ILLEGAL_INSTRUCTION),
                "{opcode:#06x}"
            );
        }
    }
}
