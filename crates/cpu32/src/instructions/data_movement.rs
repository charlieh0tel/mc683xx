//! Data movement instructions (CPU32RM 4.3.2, Table 4-2), and SWAP
//! (Table 4-5).

use crate::address::Address;
use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::MODE_SHIFT;
use crate::effective_address::Operand;
use crate::effective_address::THREE_BITS;
use crate::effective_address::kind;
use crate::effective_address::mode_of;
use crate::exception::Exception;
use crate::flags;
use crate::function_code::FunctionCode;
use crate::instructions::Flow;
use crate::instructions::Handler;
use crate::instructions::Table;
use crate::registers::Registers;
use crate::size::Size;
use crate::timing;
use crate::timing::AddressTable;

/// MOVEQ: 0111 rrr0 dddddddd (CPU32RM 4.4, MOVEQ).
const MOVEQ_MASK: u16 = 0xf100;
/// MOVEQ's fixed bits.
const MOVEQ: u16 = 0x7000;
/// Where an instruction's register field at bits 11:9 starts.
pub(super) const REGISTER_SHIFT: u16 = 9;
/// MOVE: 00ss, then the destination register and mode, then the
/// source mode and register (CPU32RM 4.4, MOVE).
const MOVE_MASK: u16 = 0xc000;
/// MOVE's fixed bits.
const MOVE: u16 = 0x0000;
/// Where MOVE's size field starts.
const MOVE_SIZE_SHIFT: u16 = 12;
/// The bits of a six-bit effective address field.
pub(super) const FIELD_MASK: u16 = 0x3f;
/// The mode of An (CPU32RM 3.4.4).
pub(super) const ADDRESS_REGISTER_MODE: u16 = 0b001;
/// The mode of (An)+.
const POSTINCREMENT_MODE: u16 = 0b011;
/// The mode of -(An).
const PREDECREMENT_MODE: u16 = 0b100;
/// MOVEA: 00ss rrr0 01, then the source (CPU32RM 4.4, MOVEA).
const MOVEA_MASK: u16 = 0xc1c0;
/// MOVEA's fixed bits.
const MOVEA: u16 = 0x0040;
/// LEA: 0100 rrr1 11, then the source (CPU32RM 4.4, LEA).
const LEA_MASK: u16 = 0xf1c0;
/// LEA's fixed bits.
const LEA: u16 = 0x41c0;
/// PEA: 0100 1000 01, then the source (CPU32RM 4.4, PEA).
const PEA: u16 = 0x4840;
/// The opcode bits that pick an instruction with a six-bit effective
/// address field and no other.
const EFFECTIVE_ADDRESS_ONLY_MASK: u16 = 0xffc0;
/// The opcode bits that pick an instruction with a register field at
/// bits 2:0 and no other.
const REGISTER_ONLY_MASK: u16 = 0xfff8;
/// LINK.W: 0100 1110 0101 0rrr (CPU32RM 4.4, LINK).
const LINK_WORD: u16 = 0x4e50;
/// LINK.L: 0100 1000 0000 1rrr.
const LINK_LONG: u16 = 0x4808;
/// UNLK: 0100 1110 0101 1rrr (CPU32RM 4.4, UNLK).
const UNLK: u16 = 0x4e58;
/// SWAP: 0100 1000 0100 0rrr (CPU32RM 4.4, SWAP).
const SWAP: u16 = 0x4840;
/// EXT.W: 0100 1000 1000 0rrr (CPU32RM 4.4, EXT, opmode 010).
const EXT_WORD: u16 = 0x4880;
/// EXT.L: opmode 011.
const EXT_LONG: u16 = 0x48c0;
/// EXTB.L: opmode 111.
const EXTB: u16 = 0x49c0;
/// EXG: 1100 xxx1, then its opmode and yyy (CPU32RM 4.4, EXG).
const EXG_MASK: u16 = 0xf1f8;
/// EXG Dx, Dy: opmode 01000.
const EXG_DATA: u16 = 0xc140;
/// EXG Ax, Ay: opmode 01001.
const EXG_ADDRESS: u16 = 0xc148;
/// EXG Dx, Ay: opmode 10001.
const EXG_DATA_ADDRESS: u16 = 0xc188;
/// MOVEM: 0100 1d00 1s, then the effective address (CPU32RM 4.4,
/// MOVEM).
const MOVEM_MASK: u16 = 0xfb80;
/// MOVEM's fixed bits.
const MOVEM: u16 = 0x4880;
/// MOVEM dr: memory to registers.
const MOVEM_TO_REGISTERS: u16 = 1 << 10;
/// MOVEM size: long.
const MOVEM_LONG: u16 = 1 << 6;
/// MOVEP: 0000 ddd1 oo00 1aaa, opmode 1oo (CPU32RM 4.4, MOVEP).
const MOVEP_MASK: u16 = 0xf138;
/// MOVEP's fixed bits.
const MOVEP: u16 = 0x0108;
/// MOVEP opmode bit 7: register to memory.
const MOVEP_TO_MEMORY: u16 = 1 << 7;
/// MOVEP opmode bit 6: long.
const MOVEP_LONG: u16 = 1 << 6;
/// MOVEP's stride: alternate bytes.
const MOVEP_STRIDE: u32 = 2;
/// Registers in a MOVEM mask: D0-D7, then A0-A7 (CPU32RM 4.4, MOVEM).
const GENERAL_REGISTERS: u16 = 16;
/// Data registers, numbered 0 to 7 before the address registers.
const DATA_REGISTERS: usize = 8;
/// A7, the stack pointer.
const STACK_POINTER: usize = 7;
/// The bits a SWAP rotates by: a register's halves.
const HALF: u32 = 16;

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set(MOVEQ_MASK, MOVEQ, |_| true, moveq);
    table.set_by(MOVE_MASK, MOVE, is_move, move_handler);
    table.set_by(MOVEA_MASK, MOVEA, is_movea, movea_handler);
    table.set_by(LEA_MASK, LEA, is_control, lea_handler);
    table.set_by(EFFECTIVE_ADDRESS_ONLY_MASK, PEA, is_control, pea_handler);
    table.set(REGISTER_ONLY_MASK, LINK_WORD, |_| true, link::<false>);
    table.set(REGISTER_ONLY_MASK, LINK_LONG, |_| true, link::<true>);
    table.set(REGISTER_ONLY_MASK, UNLK, |_| true, unlk);
    table.set(REGISTER_ONLY_MASK, SWAP, |_| true, swap);
    table.set(REGISTER_ONLY_MASK, EXT_WORD, |_| true, ext);
    table.set(REGISTER_ONLY_MASK, EXT_LONG, |_| true, ext);
    table.set(REGISTER_ONLY_MASK, EXTB, |_| true, ext);
    for pattern in [EXG_DATA, EXG_ADDRESS, EXG_DATA_ADDRESS] {
        table.set(EXG_MASK, pattern, |_| true, exg);
    }
    table.set(MOVEM_MASK, MOVEM, is_movem, movem);
    table.set(MOVEP_MASK, MOVEP, |_| true, movep);
}

/// The register field at bits 11:9.
pub(super) const fn register_field(opcode: u16) -> usize {
    (opcode >> REGISTER_SHIFT & THREE_BITS) as usize
}

/// The register field at bits 2:0.
pub(super) const fn low_register(opcode: u16) -> usize {
    (opcode & THREE_BITS) as usize
}

/// Whether the effective address field is a control mode (CPU32RM
/// Table 3-1): LEA, PEA.
fn is_control(opcode: u16) -> bool {
    Category::CONTROL.admits(opcode & FIELD_MASK)
}

/// Register `number` of D0-D7 then A0-A7, MOVEM's numbering.
fn general(registers: &Registers, number: usize) -> u32 {
    if number < DATA_REGISTERS {
        registers.data[number]
    } else {
        registers.address(number - DATA_REGISTERS)
    }
}

/// Sets register `number` of D0-D7 then A0-A7.
fn set_general(registers: &mut Registers, number: usize, value: u32) {
    if number < DATA_REGISTERS {
        registers.data[number] = value;
    } else {
        registers.set_address(number - DATA_REGISTERS, value);
    }
}

/// The address a control mode names: LEA, PEA, MOVEM.
#[inline]
fn control_address(core: &Core<'_>, operand: Operand) -> Result<Address, Exception> {
    match operand {
        Operand::Data(address) | Operand::Program(address) => Ok(address),
        _ => Err(core.illegal()),
    }
}

/// Pushes `value`, a long, on the active stack: SP - 4 -> SP, then
/// value -> (SP).
pub(super) fn push_long(core: &mut Core<'_>, value: u32) -> Result<(), Exception> {
    let sp = core
        .cpu
        .registers
        .address(STACK_POINTER)
        .wrapping_sub(Size::Long.bytes());
    core.cpu.registers.set_address(STACK_POINTER, sp);
    core.write_data(Address(sp), Size::Long, value)
}

/// MOVE's size: 01 byte, 11 word, 10 long.
const fn move_size(opcode: u16) -> Option<Size> {
    match opcode >> MOVE_SIZE_SHIFT & 0b11 {
        0b01 => Some(Size::Byte),
        0b11 => Some(Size::Word),
        0b10 => Some(Size::Long),
        _ => None,
    }
}

/// MOVE's destination field, mode and register swapped back to the
/// usual order.
const fn move_destination(opcode: u16) -> u16 {
    (opcode >> MODE_SHIFT & FIELD_MASK & !THREE_BITS) | (opcode >> REGISTER_SHIFT & THREE_BITS)
}

/// Whether `opcode` is a MOVE: a size, a data alterable destination,
/// and any source but An for a byte.
fn is_move(opcode: u16) -> bool {
    let source = opcode & FIELD_MASK;
    let byte_from_address_register =
        move_size(opcode) == Some(Size::Byte) && mode_of(source) == ADDRESS_REGISTER_MODE;
    move_size(opcode).is_some()
        && Category::DATA
            .and(Category::ALTERABLE)
            .admits(move_destination(opcode))
        && Category::of(source) != Category::NONE
        && !byte_from_address_register
}

/// MOVE's handler for `opcode`, specialized by size and by the kinds
/// of its source and its destination, a data alterable one.
fn move_handler(opcode: u16) -> Handler {
    let source = kind::of(opcode & FIELD_MASK);
    let destination = kind::of(move_destination(opcode));
    specialize_size!(move_size(opcode), SIZE => specialize_source!(source, SOURCE =>
        specialize_data_alterable!(destination, DESTINATION =>
            move_::<SIZE, SOURCE, DESTINATION>)))
}

/// MOVE <ea>, <ea>: the source to the destination, N and Z from it.
/// `SIZE` is a [`Size::index`], `SOURCE` and `DESTINATION` the fields'
/// kinds.
fn move_<const SIZE: u8, const SOURCE: u8, const DESTINATION: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let (source_table, destination_table) = const { timing::move_tables(SOURCE, DESTINATION) };
    let source = core.effective_address_of(SOURCE, opcode & FIELD_MASK, size, source_table)?;
    let value = core.read_operand(source, size)?;
    let destination = core.effective_address_of(
        DESTINATION,
        move_destination(opcode),
        size,
        destination_table,
    )?;
    core.write_operand(destination, size, value)?;
    flags::logical(&mut core.cpu.registers, value, size);
    Ok(Flow::Next)
}

/// Whether `opcode` is a MOVEA: a word or long, any source (CPU32RM
/// 4.4, MOVEA).
fn is_movea(opcode: u16) -> bool {
    matches!(move_size(opcode), Some(Size::Word | Size::Long))
        && Category::of(opcode & FIELD_MASK) != Category::NONE
}

/// MOVEA's handler for `opcode`, specialized by size and source kind.
fn movea_handler(opcode: u16) -> Handler {
    specialize_size!(move_size(opcode), SIZE =>
        specialize_source!(kind::of(opcode & FIELD_MASK), SOURCE => movea::<SIZE, SOURCE>))
}

/// MOVEA <ea>, An: a word sign extended; condition codes unaffected.
/// `SIZE` is a [`Size::index`], `SOURCE` the source's kind.
fn movea<const SIZE: u8, const SOURCE: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let source =
        core.effective_address_of(SOURCE, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    let value = size.sign_extend(core.read_operand(source, size)?);
    core.cpu
        .registers
        .set_address(register_field(opcode), value);
    Ok(Flow::Next)
}

/// MOVEQ #data, Dn: the low byte, sign extended to a long.
fn moveq(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let register = register_field(opcode);
    let value = Size::Byte.sign_extend(u32::from(opcode));
    core.cpu.registers.data[register] = value;
    flags::logical(&mut core.cpu.registers, value, Size::Long);
    Ok(Flow::Next)
}

/// LEA's handler for `opcode`, specialized by source kind.
fn lea_handler(opcode: u16) -> Handler {
    specialize_control!(kind::of(opcode & FIELD_MASK), SOURCE => lea::<SOURCE>)
}

/// LEA <ea>, An: the address, not the operand (CPU32RM 4.4, LEA).
/// `SOURCE` is the source's kind.
fn lea<const SOURCE: u8>(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let operand = core.effective_address_of(
        SOURCE,
        opcode & FIELD_MASK,
        Size::Long,
        AddressTable::Calculate,
    )?;
    let address = control_address(core, operand)?;
    core.cpu
        .registers
        .set_address(register_field(opcode), address.0);
    Ok(Flow::Next)
}

/// PEA's handler for `opcode`, specialized by source kind.
fn pea_handler(opcode: u16) -> Handler {
    specialize_control!(kind::of(opcode & FIELD_MASK), SOURCE => pea::<SOURCE>)
}

/// PEA <ea>: the address pushed (CPU32RM 4.4, PEA).  `SOURCE` is the
/// source's kind.
fn pea<const SOURCE: u8>(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let operand = core.effective_address_of(
        SOURCE,
        opcode & FIELD_MASK,
        Size::Long,
        AddressTable::Calculate,
    )?;
    let address = control_address(core, operand)?;
    push_long(core, address.0)?;
    Ok(Flow::Next)
}

/// LINK An, #d, a word displacement or, `LONG`, a long one: SP - 4 ->
/// SP; An -> (SP); SP -> An; SP + d -> SP (CPU32RM 4.4, LINK).  For
/// A7 the value pushed is SP after its decrement, as the operation
/// reads.
fn link<const LONG: bool>(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let displacement = if LONG {
        core.fetch_long()?
    } else {
        Size::Word.sign_extend(u32::from(core.fetch_word()?))
    };
    let register = low_register(opcode);
    let sp = core
        .cpu
        .registers
        .address(STACK_POINTER)
        .wrapping_sub(Size::Long.bytes());
    core.cpu.registers.set_address(STACK_POINTER, sp);
    let value = core.cpu.registers.address(register);
    core.write_data(Address(sp), Size::Long, value)?;
    core.cpu.registers.set_address(register, sp);
    core.cpu
        .registers
        .set_address(STACK_POINTER, sp.wrapping_add(displacement));
    Ok(Flow::Next)
}

/// UNLK An: An -> SP; (SP) -> An; SP + 4 -> SP (CPU32RM 4.4, UNLK).
/// For A7 the long pulled is A7's: the manual's order is silent on
/// it; see [PLAN.md, Where the manuals are silent].
///
/// [PLAN.md, Where the manuals are silent]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#where-the-manuals-are-silent
fn unlk(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let register = low_register(opcode);
    let sp = core.cpu.registers.address(register);
    let value = core.read_data(Address(sp), Size::Long)?;
    core.cpu
        .registers
        .set_address(STACK_POINTER, sp.wrapping_add(Size::Long.bytes()));
    core.cpu.registers.set_address(register, value);
    Ok(Flow::Next)
}

/// SWAP Dn: the halves exchanged; N and Z of the long, V and C clear
/// (CPU32RM 4.4, SWAP).
fn swap(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let register = low_register(opcode);
    let value = core.cpu.registers.data[register].rotate_left(HALF);
    core.cpu.registers.data[register] = value;
    flags::logical(&mut core.cpu.registers, value, Size::Long);
    Ok(Flow::Next)
}

/// EXT.W, EXT.L, EXTB.L Dn: a byte to a word, a word to a long, a
/// byte to a long; N and Z of the result, V and C clear (CPU32RM 4.4,
/// EXT, EXTB).
fn ext(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let (from, to) = match opcode & !THREE_BITS {
        EXT_WORD => (Size::Byte, Size::Word),
        EXT_LONG => (Size::Word, Size::Long),
        _ => (Size::Byte, Size::Long),
    };
    let register = low_register(opcode);
    let original = core.cpu.registers.data[register];
    let value = from.sign_extend(original) & to.mask() | original & !to.mask();
    core.cpu.registers.data[register] = value;
    flags::logical(&mut core.cpu.registers, value, to);
    Ok(Flow::Next)
}

/// EXG Rx, Ry: the two longs exchanged (CPU32RM 4.4, EXG).
fn exg(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let (x, y) = match opcode & EXG_MASK {
        EXG_DATA => (register_field(opcode), low_register(opcode)),
        EXG_ADDRESS => (
            DATA_REGISTERS + register_field(opcode),
            DATA_REGISTERS + low_register(opcode),
        ),
        _ => (
            register_field(opcode),
            DATA_REGISTERS + low_register(opcode),
        ),
    };
    let registers = &mut core.cpu.registers;
    let (first, second) = (general(registers, x), general(registers, y));
    set_general(registers, x, second);
    set_general(registers, y, first);
    Ok(Flow::Next)
}

/// Whether `opcode` is a MOVEM: to memory, a control alterable mode or
/// -(An); to registers, a control mode or (An)+ (CPU32RM 4.4, MOVEM).
fn is_movem(opcode: u16) -> bool {
    let field = opcode & FIELD_MASK;
    if opcode & MOVEM_TO_REGISTERS != 0 {
        Category::CONTROL.admits(field) || mode_of(opcode) == POSTINCREMENT_MODE
    } else {
        Category::CONTROL.and(Category::ALTERABLE).admits(field)
            || mode_of(opcode) == PREDECREMENT_MODE
    }
}

/// MOVEM: the registers of the mask word to or from consecutive
/// operands (CPU32RM 4.4, MOVEM).  Words loaded are sign extended.
/// With -(An) the mask runs A7 to D0 and the operands down from An;
/// a stored An is its initial value less one operand (CPU32RM; the
/// 68000 stores it unchanged).  With (An)+ the register ends past the
/// last operand, over any value loaded into it.  An is written when
/// the transfers end, so a type III fault leaves it as it was.
///
/// Each transfer records its progress for a type III fault; after
/// `rte` of one, MOVEM continues with the faulted transfer at the
/// stacked address, the effective address not recalculated (CPU32RM
/// 6.3.1.3, 6.3.2.6).
fn movem(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let mask = core.fetch_word()?;
    let size = if opcode & MOVEM_LONG != 0 {
        Size::Long
    } else {
        Size::Word
    };
    let to_registers = opcode & MOVEM_TO_REGISTERS != 0;
    let register = low_register(opcode);
    let predecrement = mode_of(opcode) == PREDECREMENT_MODE;
    let postincrement = mode_of(opcode) == POSTINCREMENT_MODE;
    let data_space = core.cpu.registers.data_space();
    let (start, space): (u32, FunctionCode) = if predecrement || postincrement {
        (core.cpu.registers.address(register), data_space)
    } else {
        let operand = core.calculated_address(opcode & FIELD_MASK, size)?;
        let space = match operand {
            Operand::Program(_) => core.cpu.registers.program_space(),
            _ => data_space,
        };
        (control_address(core, operand)?.0, space)
    };
    let continuation = core.movem_continuation();
    core.charge(timing::movem(mask.count_ones()));
    let step = size.bytes();
    let first = if predecrement {
        start.wrapping_sub(step)
    } else {
        start
    };
    let (skip, mut operand) = continuation.map_or((0, first), |progress| {
        (progress.attempted.saturating_sub(1), progress.next.0)
    });
    let mut attempted: u8 = 0;
    for bit in (0..GENERAL_REGISTERS).filter(|bit| mask & 1 << bit != 0) {
        attempted += 1;
        if attempted <= skip {
            continue;
        }
        let number = if predecrement {
            usize::from(GENERAL_REGISTERS - 1 - bit)
        } else {
            usize::from(bit)
        };
        core.movem_progress(attempted, Address(operand));
        if to_registers {
            let value = size.sign_extend(core.read(Address(operand), size, space)?);
            set_general(&mut core.cpu.registers, number, value);
        } else {
            let value = if predecrement && number == DATA_REGISTERS + register {
                start.wrapping_sub(step)
            } else {
                general(&core.cpu.registers, number)
            };
            core.write_unreleased(Address(operand), size, space, value)?;
        }
        operand = if predecrement {
            operand.wrapping_sub(step)
        } else {
            operand.wrapping_add(step)
        };
    }
    if to_registers {
        // One read more than the registers need, at the next operand
        // (CPU32RM 4.4, MOVEM, note); its value is not used.  A fault
        // in it continues from the last transfer.
        core.read(Address(operand), Size::Word, space)?;
    }
    if predecrement {
        core.cpu
            .registers
            .set_address(register, operand.wrapping_add(step));
    } else if postincrement {
        core.cpu.registers.set_address(register, operand);
    }
    Ok(Flow::Next)
}

/// MOVEP: a data register's bytes, high first, to or from alternate
/// bytes from (d16, Ay); condition codes unaffected (CPU32RM 4.4,
/// MOVEP).  Every write but the last is not released (CPU32RM
/// 6.3.1.2).
fn movep(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let displacement = Size::Word.sign_extend(u32::from(core.fetch_word()?));
    let base = core
        .cpu
        .registers
        .address(low_register(opcode))
        .wrapping_add(displacement);
    let size = if opcode & MOVEP_LONG != 0 {
        Size::Long
    } else {
        Size::Word
    };
    let register = register_field(opcode);
    let bytes = size.bytes();
    let shift = |index: u32| u8::BITS * (bytes - 1 - index);
    let address = |index: u32| Address(base.wrapping_add(MOVEP_STRIDE * index));
    let space = core.cpu.registers.data_space();
    if opcode & MOVEP_TO_MEMORY != 0 {
        let value = core.cpu.registers.data[register];
        for index in 0..bytes {
            let byte = value >> shift(index) & Size::Byte.mask();
            if index + 1 == bytes {
                core.write(address(index), Size::Byte, space, byte)?;
            } else {
                core.write_unreleased(address(index), Size::Byte, space, byte)?;
            }
        }
    } else {
        let value = (0..bytes).try_fold(0, |value, index| {
            core.read(address(index), Size::Byte, space)
                .map(|byte| value | byte << shift(index))
        })?;
        let original = core.cpu.registers.data[register];
        core.cpu.registers.data[register] = original & !size.mask() | value;
    }
    Ok(Flow::Next)
}

#[cfg(test)]
pub(super) mod tests {
    use crate::address::Address;
    use crate::cpu::Cpu;
    use crate::exception::Exception;
    use crate::exception::Vector;
    use crate::instructions::tests::CODE;
    use crate::instructions::tests::machine;
    use crate::instructions::tests::peek_long;
    use crate::instructions::tests::poke_long;
    use crate::instructions::tests::step;
    use crate::memory::Space;
    use crate::registers::status;
    use crate::size::Size;
    use crate::timing;

    /// The supervisor stack's top in [`prepared`] machines.
    pub(crate) const STACK: u32 = 0x6000;
    /// Where [`prepared`] machines' exception handlers are.
    const HANDLER: u32 = 0x2000;
    /// The first address past the test machine's RAM: a bus error.
    pub(crate) const RAM_END: u32 = 0x8000;
    /// Bytes in a format $C frame (CPU32RM 6.4.3, Figure 6-6).
    pub(crate) const FAULT_FRAME_BYTES: u32 = 24;
    /// RTE (CPU32RM 4.4, RTE).
    const RTE: u16 = 0x4e73;

    /// A machine running `code`, its stack at [`STACK`] and every
    /// vector at [`HANDLER`].
    pub(crate) fn prepared(code: &[u16]) -> Cpu {
        let mut cpu = machine(code);
        cpu.registers.ssp = STACK;
        for vector in 0..=u8::MAX {
            poke_long(&mut cpu, Vector(vector).offset(), HANDLER);
        }
        cpu
    }

    /// The vector of the exception the last instruction took.
    pub(crate) fn taken(cpu: &Cpu) -> Option<Vector> {
        cpu.exception_taken().map(Exception::vector)
    }

    /// The word at `at` in supervisor space.
    pub(crate) fn peek_word(cpu: &Cpu, at: u32) -> u32 {
        cpu.memory
            .read(Space::Supervisor, Address(at), Size::Word)
            .expect("RAM")
            .0
    }

    /// The byte at `at` in supervisor space.
    pub(crate) fn peek_byte(cpu: &Cpu, at: u32) -> u32 {
        cpu.memory
            .read(Space::Supervisor, Address(at), Size::Byte)
            .expect("RAM")
            .0
    }

    /// Writes the word at `at` in supervisor space.
    pub(crate) fn poke_word(cpu: &mut Cpu, at: u32, value: u16) {
        cpu.memory
            .write(Space::Supervisor, Address(at), Size::Word, u32::from(value))
            .expect("RAM");
    }

    /// The condition codes.
    pub(crate) fn ccr(cpu: &Cpu) -> u16 {
        cpu.registers.sr() & status::CCR
    }

    #[test]
    fn moveq_sign_extends_and_sets_n() {
        // MOVEQ #-1, D3
        let mut cpu = machine(&[0x76ff]);
        cpu.registers
            .set_ccr((status::X | status::V | status::C) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[3], 0xffff_ffff);
        assert_eq!(ccr(&cpu), status::X | status::N);
    }

    #[test]
    fn moveq_zero_sets_z() {
        // MOVEQ #0, D0
        let mut cpu = machine(&[0x7000]);
        cpu.registers.data[0] = 0x1234;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0);
        assert_eq!(ccr(&cpu), status::Z);
    }

    #[test]
    fn move_long_memory_to_memory_with_postincrement_and_index() {
        // MOVE.L (A0)+, (8, A1, D2.W*4)
        let mut cpu = machine(&[0x2398, 0x2408]);
        cpu.registers.set_address(0, 0x2000);
        cpu.registers.set_address(1, 0x3000);
        cpu.registers.data[2] = 0xffff_0003;
        poke_long(&mut cpu, 0x2000, 0x8000_0001);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.address(0), 0x2004);
        assert_eq!(peek_long(&cpu, 0x3000 + 8 + 12), 0x8000_0001);
        assert_eq!(cpu.registers.pc, CODE + 4);
        assert_eq!(ccr(&cpu), status::N);
    }

    #[test]
    fn move_byte_through_a7_keeps_it_aligned() {
        // MOVE.B D0, -(A7)
        let mut cpu = machine(&[0x1f00]);
        cpu.registers.set_address(7, 0x2000);
        cpu.registers.data[0] = 0x42;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.address(7), 0x1ffe);
    }

    #[test]
    fn move_from_pc_relative_reads_program_space() {
        // MOVE.W (4, PC), D1: PC is the extension word's address
        let mut cpu = machine(&[0x323a, 0x0004, 0x4e71, 0xabcd]);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1] & 0xffff, 0xabcd);
    }

    #[test]
    fn move_to_pc_relative_is_not_a_move() {
        // MOVE.W D0, (d16, PC) has destination mode 111, register 010.
        assert!(!super::is_move(0x35c0));
        assert!(super::is_move(0x3000));
    }

    #[test]
    fn movea_word_sign_extends_and_keeps_the_flags() {
        // MOVEA.W D0, A1
        let mut cpu = machine(&[0x3240]);
        cpu.registers.data[0] = 0x1234_8000;
        cpu.registers.set_ccr(status::Z as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.address(1), 0xffff_8000);
        assert_eq!(ccr(&cpu), status::Z);
        // MOVEA.L #$12345678, A2
        let mut cpu = machine(&[0x247c, 0x1234, 0x5678]);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.address(2), 0x1234_5678);
    }

    #[test]
    fn movea_from_an_odd_address_is_an_address_error() {
        // MOVEA.W (A0), A1
        let mut cpu = prepared(&[0x3250]);
        cpu.registers.set_address(0, 0x3001);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::ADDRESS_ERROR));
        assert_eq!(cpu.registers.address(1), 0);
    }

    #[test]
    fn illegal_combinations_are_not_claimed() {
        // MOVEA.B, LEA D0, MOVEM to A0, MOVEM -(A0) to registers,
        // MOVEM (A0)+ to memory, PEA (A0)+.
        for opcode in [0x1040, 0x41c0, 0x4888, 0x4ca0, 0x4898, 0x4858] {
            let mut cpu = prepared(&[opcode, 0, 0]);
            step(&mut cpu, 1);
            assert_eq!(
                taken(&cpu),
                Some(Vector::ILLEGAL_INSTRUCTION),
                "{opcode:#06x}"
            );
        }
    }

    #[test]
    fn a_full_extension_word_with_bits_3_to_0_set_is_illegal() {
        // LEA (0, A0, D0), A1 in the full format: bits 3:0 must be
        // 0000 (CPU32RM 6.2.8).
        for (extension, illegal) in [(0x0110, false), (0x0118, true), (0x0111, true)] {
            let mut cpu = prepared(&[0x43f0, extension]);
            cpu.registers.set_address(0, 0x100);
            cpu.registers.data[0] = 0x10;
            step(&mut cpu, 1);
            let vector = illegal.then_some(Vector::ILLEGAL_INSTRUCTION);
            assert_eq!(taken(&cpu), vector, "{extension:#06x}");
            if !illegal {
                assert_eq!(cpu.registers.address(1), 0x110);
            }
        }
    }

    #[test]
    fn lea_and_pea_take_the_address() {
        // LEA (4, PC), A1; PEA (8, A0)
        let mut cpu = prepared(&[0x43fa, 0x0004, 0x4868, 0x0008]);
        cpu.registers.set_address(0, 0x3000);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.address(1), CODE + 2 + 4);
        assert_eq!(cpu.registers.ssp, STACK - 4);
        assert_eq!(peek_long(&cpu, STACK - 4), 0x3008);
    }

    #[test]
    fn link_and_unlk_build_and_drop_a_frame() {
        // LINK.W A6, #-8; UNLK A6
        let mut cpu = prepared(&[0x4e56, 0xfff8, 0x4e5e]);
        cpu.registers.set_address(6, 0x1234);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, STACK - 4), 0x1234);
        assert_eq!(cpu.registers.address(6), STACK - 4);
        assert_eq!(cpu.registers.ssp, STACK - 12);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.address(6), 0x1234);
        assert_eq!(cpu.registers.ssp, STACK);
    }

    #[test]
    fn link_long_takes_a_long_displacement() {
        // LINK.L A5, #-$10000
        let mut cpu = prepared(&[0x480d, 0xffff, 0x0000]);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.address(5), STACK - 4);
        assert_eq!(cpu.registers.ssp, (STACK - 4).wrapping_sub(0x1_0000));
        assert_eq!(cpu.registers.pc, CODE + 6);
    }

    #[test]
    fn link_and_unlk_of_a7() {
        // LINK.W A7, #-4: A7 after its decrement is pushed.
        let mut cpu = prepared(&[0x4e57, 0xfffc, 0x4e5f]);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, STACK - 4), STACK - 4);
        assert_eq!(cpu.registers.ssp, STACK - 8);
        // UNLK A7: A7 is the long pulled.
        cpu.registers.ssp = STACK - 4;
        poke_long(&mut cpu, STACK - 4, 0x4000);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.ssp, 0x4000);
    }

    #[test]
    fn link_on_an_odd_stack_is_an_address_error() {
        let mut cpu = prepared(&[0x4e56, 0xfff8]);
        cpu.registers.ssp = STACK + 1;
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::ADDRESS_ERROR));
    }

    #[test]
    fn unlk_from_an_odd_frame_is_an_address_error() {
        let mut cpu = prepared(&[0x4e5e]);
        cpu.registers.set_address(6, 0x3001);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::ADDRESS_ERROR));
        assert_eq!(cpu.registers.address(6), 0x3001);
    }

    #[test]
    fn exg_exchanges_each_pairing() {
        // EXG D1, D2; EXG A1, A2; EXG D1, A2
        let mut cpu = machine(&[0xc342, 0xc34a, 0xc38a]);
        cpu.registers.data[1] = 1;
        cpu.registers.data[2] = 2;
        cpu.registers.set_address(1, 11);
        cpu.registers.set_address(2, 12);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.data[1..3], [2, 1]);
        assert_eq!(
            (cpu.registers.address(1), cpu.registers.address(2)),
            (12, 11)
        );
        step(&mut cpu, 1);
        assert_eq!((cpu.registers.data[1], cpu.registers.address(2)), (11, 2));
    }

    #[test]
    fn swap_exchanges_halves_and_tests_the_long() {
        // SWAP D0
        let mut cpu = machine(&[0x4840]);
        cpu.registers.data[0] = 0x0000_8001;
        cpu.registers
            .set_ccr((status::X | status::V | status::C) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x8001_0000);
        assert_eq!(ccr(&cpu), status::X | status::N);
    }

    #[test]
    fn ext_and_extb_sign_extend() {
        // EXT.W D0; EXT.L D1; EXTB.L D2
        let mut cpu = machine(&[0x4880, 0x48c1, 0x49c2]);
        cpu.registers.data[0] = 0x1234_5680;
        cpu.registers.data[1] = 0x1234_7fff;
        cpu.registers.data[2] = 0x1234_5600;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x1234_ff80);
        assert_eq!(ccr(&cpu), status::N);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0x0000_7fff);
        assert_eq!(ccr(&cpu), 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], 0);
        assert_eq!(ccr(&cpu), status::Z);
    }

    #[test]
    fn movem_stores_and_loads_in_mask_order() {
        // MOVEM.L D0-D1/A1, (A0); MOVEM.L (A0), D2-D3/A2
        let mut cpu = machine(&[0x48d0, 0x0203, 0x4cd0, 0x040c]);
        cpu.registers.set_address(0, 0x3000);
        cpu.registers.data[0] = 0x1111_1111;
        cpu.registers.data[1] = 0x2222_2222;
        cpu.registers.set_address(1, 0x3333_3333);
        cpu.registers.set_ccr(status::Z as u8);
        step(&mut cpu, 1);
        assert_eq!(
            [0, 4, 8].map(|offset| peek_long(&cpu, 0x3000 + offset)),
            [0x1111_1111, 0x2222_2222, 0x3333_3333]
        );
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2..4], [0x1111_1111, 0x2222_2222]);
        assert_eq!(cpu.registers.address(2), 0x3333_3333);
        assert_eq!(cpu.registers.address(0), 0x3000);
        assert_eq!(ccr(&cpu), status::Z);
    }

    #[test]
    fn movem_words_load_sign_extended_through_postincrement() {
        // MOVEM.W (A0)+, D0/A1
        let mut cpu = machine(&[0x4c98, 0x0201]);
        cpu.registers.set_address(0, 0x3000);
        cpu.registers.data[0] = 0xffff_ffff;
        poke_word(&mut cpu, 0x3000, 0x0001);
        poke_word(&mut cpu, 0x3002, 0x8000);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 1);
        assert_eq!(cpu.registers.address(1), 0xffff_8000);
        assert_eq!(cpu.registers.address(0), 0x3004);
    }

    #[test]
    fn movem_postincrement_ends_past_the_operands_over_a_loaded_an() {
        // MOVEM.L (A0)+, D1/A0
        let mut cpu = machine(&[0x4cd8, 0x0102]);
        cpu.registers.set_address(0, 0x3000);
        poke_long(&mut cpu, 0x3004, 0xdead_beef);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.address(0), 0x3008);
    }

    #[test]
    fn movem_predecrement_stores_a7_to_d0_downward() {
        // MOVEM.L D0/A0/A1, -(A0): mask D0 bit 15, A0 bit 7, A1 bit 6.
        let mut cpu = machine(&[0x48e0, 0x80c0]);
        cpu.registers.set_address(0, 0x3010);
        cpu.registers.set_address(1, 0xa1a1_a1a1);
        cpu.registers.data[0] = 0xd0d0_d0d0;
        step(&mut cpu, 1);
        // A1 first at $300C, then A0 as its value less 4, then D0.
        assert_eq!(peek_long(&cpu, 0x300c), 0xa1a1_a1a1);
        assert_eq!(peek_long(&cpu, 0x3008), 0x300c);
        assert_eq!(peek_long(&cpu, 0x3004), 0xd0d0_d0d0);
        assert_eq!(cpu.registers.address(0), 0x3004);
    }

    #[test]
    fn movem_from_pc_relative_reads_program_space() {
        // MOVEM.W (4, PC), D0: the operand two words on.
        let mut cpu = machine(&[0x4cba, 0x0001, 0x0004, 0x4e71, 0x8001]);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xffff_8001);
        assert_eq!(cpu.registers.pc, CODE + 6);
    }

    #[test]
    fn movem_charges_four_clocks_a_register() {
        let cycles = |mask: u16| {
            let mut cpu = machine(&[0x48d0, mask]);
            cpu.registers.set_address(0, 0x3000);
            step(&mut cpu, 1).cycles
        };
        assert_eq!(cycles(0x000f) - cycles(0x0001), timing::movem(3));
    }

    #[test]
    fn movem_to_an_odd_address_is_an_address_error() {
        let mut cpu = prepared(&[0x48d0, 0x0001]);
        cpu.registers.set_address(0, 0x3001);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::ADDRESS_ERROR));
    }

    #[test]
    fn movem_continues_after_rte_of_a_type_iii_fault() {
        // MOVEM.L D0-D3, (A0): D2's write bus-errors at RAM's end.
        let mut cpu = prepared(&[0x48d0, 0x000f]);
        poke_word(&mut cpu, HANDLER, RTE);
        cpu.registers.set_address(0, RAM_END - 8);
        cpu.registers.data[..4].copy_from_slice(&[0xd0, 0xd1, 0xd2, 0xd3]);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::BUS_ERROR));
        let sp = STACK - FAULT_FRAME_BYTES;
        // The faulted operand's address, and 16 less 3 transfers
        // attempted (CPU32RM 6.3.1.3).
        assert_eq!(peek_long(&cpu, sp + 8), RAM_END);
        assert_eq!(peek_word(&cpu, sp + 0x14) & 0xff, 13);
        // The handler points the rest of the operands at RAM and
        // returns.
        poke_long(&mut cpu, sp + 8, 0x3008);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.pc, CODE + 4);
        assert_eq!(peek_long(&cpu, RAM_END - 8), 0xd0);
        assert_eq!(peek_long(&cpu, RAM_END - 4), 0xd1);
        assert_eq!(peek_long(&cpu, 0x3008), 0xd2);
        assert_eq!(peek_long(&cpu, 0x300c), 0xd3);
        assert_eq!(cpu.registers.address(0), RAM_END - 8);
    }

    #[test]
    fn movem_to_registers_reads_one_word_more() {
        // MOVEM.W (A0), D0 with A0 at RAM's last word: the extra read
        // bus-errors.
        let mut cpu = prepared(&[0x4c90, 0x0001]);
        cpu.registers.set_address(0, RAM_END - 2);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::BUS_ERROR));
    }

    #[test]
    fn movep_spreads_a_long_over_alternate_bytes() {
        // MOVEP.L D0, (2, A0); MOVEP.W (0, A1), D1
        let mut cpu = machine(&[0x01c8, 0x0002, 0x0309, 0x0000]);
        cpu.registers.set_address(0, 0x3000);
        cpu.registers.set_address(1, 0x3003);
        cpu.registers.data[0] = 0x1122_3344;
        cpu.registers.data[1] = 0xffff_ffff;
        step(&mut cpu, 1);
        assert_eq!(
            [0x3002, 0x3004, 0x3006, 0x3008].map(|at| peek_byte(&cpu, at)),
            [0x11, 0x22, 0x33, 0x44]
        );
        assert_eq!(peek_byte(&cpu, 0x3003), 0);
        poke_word(&mut cpu, 0x3002, 0x00ab);
        poke_word(&mut cpu, 0x3004, 0x00cd);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0xffff_abcd);
    }

    #[test]
    fn movep_releases_only_its_last_write() {
        // MOVEP.L D0, (0, A0): a fault on the third byte aborts it (the
        // frame's PC is the instruction's), on the fourth completes it.
        for (start, pc) in [(RAM_END - 4, CODE), (RAM_END - 6, CODE + 4)] {
            let mut cpu = prepared(&[0x01c8, 0x0000]);
            cpu.registers.set_address(0, start);
            step(&mut cpu, 1);
            assert_eq!(taken(&cpu), Some(Vector::BUS_ERROR));
            assert_eq!(peek_long(&cpu, STACK - FAULT_FRAME_BYTES + 2), pc);
        }
    }
}
