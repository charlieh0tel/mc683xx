//! Integer arithmetic instructions (CPU32RM 4.3.3, Table 4-3) but
//! multiply, divide, CMP2 and TBL; and the two-operand and
//! single-operand forms the logic instructions share.

use crate::cpu::Core;
use crate::effective_address::Category;
use crate::effective_address::Operand;
use crate::effective_address::THREE_BITS;
use crate::effective_address::kind;
use crate::effective_address::mode_of;
use crate::exception::Exception;
use crate::flags;
use crate::instructions::Flow;
use crate::instructions::Handler;
use crate::instructions::Table;
use crate::instructions::data_movement::ADDRESS_REGISTER_MODE;
use crate::instructions::data_movement::FIELD_MASK;
use crate::instructions::data_movement::REGISTER_SHIFT;
use crate::instructions::data_movement::low_register;
use crate::instructions::data_movement::register_field;
use crate::registers::Registers;
use crate::registers::status;
use crate::size::Size;
use crate::timing::AddressTable;

/// The opcode bits of a line, 15:12 (CPU32RM Table 4-11).
pub(super) const LINE_MASK: u16 = 0xf000;
/// Where the size field, bits 7:6, starts (CPU32RM 4.5).
const SIZE_SHIFT: u16 = 6;
/// Where the opmode field, bits 8:6, starts (CPU32RM 4.4, ADD).
const OPMODE_SHIFT: u16 = 6;
/// Opmode bit 8: Dn, <ea> rather than <ea>, Dn; for the address forms,
/// long rather than word (CPU32RM 4.4, ADD, ADDA).
const TO_EFFECTIVE_ADDRESS: u16 = 0b100;
/// The opmode's size bits, 7:6.
const OPMODE_SIZE: u16 = 0b011;
/// Size bits 11 in a two-operand line: the address forms, ADDA, CMPA
/// and SUBA (CPU32RM 4.4, ADDA).
const NO_SIZE: u16 = 0b11;
/// The immediate effective address field, mode 111 register 100
/// (CPU32RM 3.4.3.6).
const IMMEDIATE_FIELD: u16 = 0b111_100;
/// The opcode bits of an immediate instruction, 0000 oooo ss (CPU32RM
/// 4.4, ADDI).
const IMMEDIATE_MASK: u16 = 0xff00;
/// ADDI: 0000 0110 ss, then the destination.
const ADDI: u16 = 0x0600;
/// SUBI: 0000 0100.
const SUBI: u16 = 0x0400;
/// CMPI: 0000 1100.
const CMPI: u16 = 0x0c00;
/// ADDQ and SUBQ: 0101 ddd s ss, then the destination (CPU32RM 4.4,
/// ADDQ, SUBQ).
const QUICK_MASK: u16 = 0xf100;
/// ADDQ's fixed bits.
const ADDQ: u16 = 0x5000;
/// SUBQ's fixed bits.
const SUBQ: u16 = 0x5100;
/// A quick data field of zero stands for eight.
const QUICK_EIGHT: u32 = 8;
/// ADD, ADDA, ADDX: line 1101 (CPU32RM 4.4, ADD).
const ADD_LINE: u16 = 0xd000;
/// SUB, SUBA, SUBX: line 1001.
const SUB_LINE: u16 = 0x9000;
/// CMP, CMPA, CMPM, EOR: line 1011.
pub(super) const CMP_LINE: u16 = 0xb000;
/// ADDX and SUBX: 1x01 xxx1 ss00 myyy (CPU32RM 4.4, ADDX).
const EXTENDED_MASK: u16 = 0xf130;
/// ADDX's fixed bits.
const ADDX: u16 = 0xd100;
/// SUBX's fixed bits.
const SUBX: u16 = 0x9100;
/// ADDX, SUBX R/M: -(Ay), -(Ax) rather than Dy, Dx.
const MEMORY_TO_MEMORY: u16 = 1 << 3;
/// CMPM: 1011 xxx1 ss00 1yyy (CPU32RM 4.4, CMPM).
const CMPM_MASK: u16 = 0xf138;
/// CMPM's fixed bits.
const CMPM: u16 = 0xb108;
/// The mode bits of (An)+, CMPM's operands.
const POSTINCREMENT_FIELD: u16 = 0b011_000;
/// The mode bits of -(An), ADDX's and SUBX's memory operands.
const PREDECREMENT_FIELD: u16 = 0b100_000;
/// The single-operand instructions: 0100 oooo ss, then the operand
/// (CPU32RM 4.4, NEG).
pub(super) const SINGLE_MASK: u16 = 0xff00;
/// NEGX: 0100 0000.
const NEGX: u16 = 0x4000;
/// CLR: 0100 0010.
const CLR: u16 = 0x4200;
/// NEG: 0100 0100.
const NEG: u16 = 0x4400;

/// A two-operand operation: its result from `source` and
/// `destination`, both `size`, and the condition codes it sets.
pub(super) trait Operation {
    /// Whether it writes its result: all but the compares.
    const WRITES: bool = true;

    /// The result as a long, condition codes untouched: for An.
    fn combine(destination: u32, source: u32) -> u32;

    /// The result, `size` of it, with the condition codes set.
    fn apply(registers: &mut Registers, source: u32, destination: u32, size: Size) -> u32;
}

/// ADD, ADDI, ADDQ: destination + source (CPU32RM 4.4, ADD).
struct Add;

impl Operation for Add {
    fn combine(destination: u32, source: u32) -> u32 {
        destination.wrapping_add(source)
    }

    fn apply(registers: &mut Registers, source: u32, destination: u32, size: Size) -> u32 {
        let result = Self::combine(destination, source) & size.mask();
        flags::add(registers, source, destination, result, size);
        result
    }
}

/// SUB, SUBI, SUBQ: destination - source (CPU32RM 4.4, SUB).
struct Subtract;

impl Operation for Subtract {
    fn combine(destination: u32, source: u32) -> u32 {
        destination.wrapping_sub(source)
    }

    fn apply(registers: &mut Registers, source: u32, destination: u32, size: Size) -> u32 {
        let result = Self::combine(destination, source) & size.mask();
        flags::subtract(registers, source, destination, result, size);
        result
    }
}

/// CMP, CMPA, CMPI: destination - source, the condition codes only, X
/// unaffected (CPU32RM 4.4, CMP).
struct Compare;

impl Operation for Compare {
    const WRITES: bool = false;

    fn combine(destination: u32, source: u32) -> u32 {
        destination.wrapping_sub(source)
    }

    fn apply(registers: &mut Registers, source: u32, destination: u32, size: Size) -> u32 {
        let result = Self::combine(destination, source) & size.mask();
        flags::compare(registers, source, destination, result, size);
        result
    }
}

/// Registers the family's opcodes.
pub(super) fn register(table: &mut Table) {
    table.set_by(
        LINE_MASK,
        ADD_LINE,
        is_to_memory,
        memory_destination_handler::<Add>,
    );
    table.set_by(
        LINE_MASK,
        SUB_LINE,
        is_to_memory,
        memory_destination_handler::<Subtract>,
    );
    table.set_by(
        LINE_MASK,
        ADD_LINE,
        is_from_any,
        register_destination_handler::<Add>,
    );
    table.set_by(
        LINE_MASK,
        SUB_LINE,
        is_from_any,
        register_destination_handler::<Subtract>,
    );
    table.set_by(
        LINE_MASK,
        CMP_LINE,
        is_from_any,
        register_destination_handler::<Compare>,
    );
    table.set_by(
        LINE_MASK,
        ADD_LINE,
        is_address_form,
        address_arithmetic_handler::<Add>,
    );
    table.set_by(
        LINE_MASK,
        SUB_LINE,
        is_address_form,
        address_arithmetic_handler::<Subtract>,
    );
    table.set_by(
        LINE_MASK,
        CMP_LINE,
        is_address_form,
        address_arithmetic_handler::<Compare>,
    );
    table.set(EXTENDED_MASK, ADDX, has_size, extended::<false>);
    table.set(EXTENDED_MASK, SUBX, has_size, extended::<true>);
    table.set(CMPM_MASK, CMPM, has_size, cmpm);
    register_immediate::<Add>(table, ADDI);
    register_immediate::<Subtract>(table, SUBI);
    table.set_by(
        IMMEDIATE_MASK,
        CMPI,
        is_compare_immediate,
        compare_immediate_handler,
    );
    table.set_by(QUICK_MASK, ADDQ, is_quick, quick_handler::<Add>);
    table.set_by(QUICK_MASK, SUBQ, is_quick, quick_handler::<Subtract>);
    table.set_by(
        SINGLE_MASK,
        NEGX,
        is_data_alterable_sized,
        single_handler::<NegateExtended>,
    );
    table.set_by(
        SINGLE_MASK,
        NEG,
        is_data_alterable_sized,
        single_handler::<Negate>,
    );
    table.set_by(SINGLE_MASK, CLR, is_data_alterable_sized, clr_handler);
}

/// The size field, bits 7:6.
pub(super) const fn size(opcode: u16) -> Option<Size> {
    Size::from_field(opcode >> SIZE_SHIFT)
}

/// Whether the size field is a size.
pub(super) const fn has_size(opcode: u16) -> bool {
    size(opcode).is_some()
}

/// The opmode field, bits 8:6.
const fn opmode(opcode: u16) -> u16 {
    opcode >> OPMODE_SHIFT & THREE_BITS
}

/// Whether `opcode` has a size and its effective address is any mode,
/// An not for a byte (CPU32RM 4.4, ADD, CMP, TST: "Word and Long
/// only").
pub(super) fn is_any_sized(opcode: u16) -> bool {
    let field = opcode & FIELD_MASK;
    has_size(opcode)
        && Category::of(field) != Category::NONE
        && !(size(opcode) == Some(Size::Byte) && mode_of(field) == ADDRESS_REGISTER_MODE)
}

/// Whether `opcode` has a size and a data alterable effective address.
pub(super) fn is_data_alterable_sized(opcode: u16) -> bool {
    has_size(opcode)
        && Category::DATA
            .and(Category::ALTERABLE)
            .admits(opcode & FIELD_MASK)
}

/// Whether a two-operand opcode is `<ea>, Dn` from a data mode: AND,
/// OR (CPU32RM 4.4, AND, OR).
pub(super) fn is_from_data(opcode: u16) -> bool {
    opmode(opcode) & TO_EFFECTIVE_ADDRESS == 0
        && has_size(opcode)
        && Category::DATA.admits(opcode & FIELD_MASK)
}

/// Whether a two-operand opcode is `<ea>, Dn` from any mode, An not
/// for a byte: ADD, SUB, CMP (CPU32RM 4.4, ADD, CMP, SUB).
fn is_from_any(opcode: u16) -> bool {
    opmode(opcode) & TO_EFFECTIVE_ADDRESS == 0 && is_any_sized(opcode)
}

/// Whether a two-operand opcode is `Dn, <ea>` to a memory alterable
/// mode: ADD, AND, OR, SUB (CPU32RM 4.4, ADD, AND, OR, SUB).
pub(super) fn is_to_memory(opcode: u16) -> bool {
    opmode(opcode) & TO_EFFECTIVE_ADDRESS != 0
        && has_size(opcode)
        && Category::MEMORY
            .and(Category::ALTERABLE)
            .admits(opcode & FIELD_MASK)
}

/// Whether a two-operand opcode is `Dn, <ea>` to a data alterable mode:
/// EOR (CPU32RM 4.4, EOR).
pub(super) fn is_to_data_alterable(opcode: u16) -> bool {
    opmode(opcode) & TO_EFFECTIVE_ADDRESS != 0 && is_data_alterable_sized(opcode)
}

/// Whether a two-operand opcode is ADDA, CMPA or SUBA: opmode 011 or
/// 111, any source (CPU32RM 4.4, ADDA, CMPA, SUBA).
fn is_address_form(opcode: u16) -> bool {
    opmode(opcode) & OPMODE_SIZE == NO_SIZE && Category::of(opcode & FIELD_MASK) != Category::NONE
}

/// Registers the immediate instruction `pattern` of operation `O` on
/// a data alterable destination (CPU32RM 4.4, ADDI, ANDI, EORI, ORI,
/// SUBI).  The CCR and SR forms, immediate mode, are not data
/// alterable and stay with the system control instructions.
pub(super) fn register_immediate<O: Operation>(table: &mut Table, pattern: u16) {
    table.set_by(
        IMMEDIATE_MASK,
        pattern,
        is_data_alterable_sized,
        immediate_handler::<O>,
    );
}

/// CMPI: any data destination but an immediate (CPU32RM 4.4, CMPI).
fn is_compare_immediate(opcode: u16) -> bool {
    has_size(opcode)
        && Category::DATA.admits(opcode & FIELD_MASK)
        && opcode & FIELD_MASK != IMMEDIATE_FIELD
}

/// ADDQ, SUBQ: an alterable destination, An not for a byte (CPU32RM
/// 4.4, ADDQ, SUBQ).
fn is_quick(opcode: u16) -> bool {
    Category::ALTERABLE.admits(opcode & FIELD_MASK) && is_any_sized(opcode)
}

/// [`register_destination`]'s handler for `opcode`, specialized by size
/// and source kind.
pub(super) fn register_destination_handler<O: Operation>(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE => specialize_source!(
        kind::of(opcode & FIELD_MASK),
        SOURCE => register_destination::<O, SIZE, SOURCE>
    ))
}

/// `<ea>, Dn`: the operation on Dn and the source, into Dn unless it
/// is a compare.  `SIZE` is a [`Size::index`], `SOURCE` the source's
/// kind.
fn register_destination<O: Operation, const SIZE: u8, const SOURCE: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let source =
        core.effective_address_of(SOURCE, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    let source = core.read_operand(source, size)?;
    let register = register_field(opcode);
    let destination = core.cpu.registers.data[register] & size.mask();
    let result = O::apply(&mut core.cpu.registers, source, destination, size);
    if O::WRITES {
        core.write_operand(Operand::DataRegister(register), size, result)?;
    }
    Ok(Flow::Next)
}

/// [`memory_destination`]'s handler for `opcode`, specialized by size
/// and destination kind: memory alterable, or for EOR data alterable.
pub(super) fn memory_destination_handler<O: Operation>(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE => specialize_data_alterable!(
        kind::of(opcode & FIELD_MASK),
        DESTINATION => memory_destination::<O, SIZE, DESTINATION>
    ))
}

/// `Dn, <ea>`: the operation on the operand and Dn, into the operand.
/// `SIZE` is a [`Size::index`], `DESTINATION` the operand's kind.
fn memory_destination<O: Operation, const SIZE: u8, const DESTINATION: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let source = core.cpu.registers.data[register_field(opcode)] & size.mask();
    let operand =
        core.effective_address_of(DESTINATION, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    read_modify_write::<O>(core, operand, source, size)
}

/// The operation on `operand` and `source`, into the operand unless it
/// is a compare.  Inlined, so a specialized handler's operand folds to
/// its kind.
#[inline(always)]
fn read_modify_write<O: Operation>(
    core: &mut Core<'_>,
    operand: Operand,
    source: u32,
    size: Size,
) -> Result<Flow, Exception> {
    let destination = core.read_operand(operand, size)?;
    let result = O::apply(&mut core.cpu.registers, source, destination, size);
    if O::WRITES {
        core.write_operand(operand, size, result)?;
    }
    Ok(Flow::Next)
}

/// [`immediate`]'s handler for `opcode`, specialized by size and the
/// kind of its data alterable destination.
fn immediate_handler<O: Operation>(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE => specialize_data_alterable!(
        kind::of(opcode & FIELD_MASK),
        DESTINATION => immediate::<O, SIZE, DESTINATION>
    ))
}

/// CMPI's handler for `opcode`, specialized by size and the kind of
/// its data destination.
fn compare_immediate_handler(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE => specialize_data!(
        kind::of(opcode & FIELD_MASK),
        DESTINATION => immediate::<Compare, SIZE, DESTINATION>
    ))
}

/// `#<data>, <ea>`: the immediate first, then the destination's
/// extension words (CPU32RM 4.4, ADDI).  `SIZE` is a [`Size::index`],
/// `DESTINATION` the destination's kind.
fn immediate<O: Operation, const SIZE: u8, const DESTINATION: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let source =
        core.effective_address_of(kind::IMMEDIATE, IMMEDIATE_FIELD, size, AddressTable::Fetch)?;
    let source = core.read_operand(source, size)?;
    let operand =
        core.effective_address_of(DESTINATION, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    read_modify_write::<O>(core, operand, source, size)
}

/// [`quick`]'s handler for `opcode`, specialized by size and the kind
/// of its alterable destination.
fn quick_handler<O: Operation>(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE => specialize_alterable!(
        kind::of(opcode & FIELD_MASK),
        DESTINATION => quick::<O, SIZE, DESTINATION>
    ))
}

/// ADDQ, SUBQ #<data>, <ea>: data 1 to 8.  To An, the whole register
/// whatever the size, condition codes unaffected (CPU32RM 4.4, ADDQ,
/// SUBQ).  `SIZE` is a [`Size::index`], `DESTINATION` the destination's
/// kind.
fn quick<O: Operation, const SIZE: u8, const DESTINATION: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let data = match u32::from(opcode >> REGISTER_SHIFT & THREE_BITS) {
        0 => QUICK_EIGHT,
        data => data,
    };
    let operand =
        core.effective_address_of(DESTINATION, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    if let Operand::AddressRegister(register) = operand {
        let value = core.cpu.registers.address(register);
        core.cpu
            .registers
            .set_address(register, O::combine(value, data));
        return Ok(Flow::Next);
    }
    read_modify_write::<O>(core, operand, data, size)
}

/// [`address_arithmetic`]'s handler for `opcode`, specialized by size
/// and source kind.
fn address_arithmetic_handler<O: Operation>(opcode: u16) -> Handler {
    let size = if opmode(opcode) & TO_EFFECTIVE_ADDRESS != 0 {
        Size::Long
    } else {
        Size::Word
    };
    specialize_size!(Some(size), SIZE => specialize_source!(
        kind::of(opcode & FIELD_MASK),
        SOURCE => address_arithmetic::<O, SIZE, SOURCE>
    ))
}

/// ADDA, SUBA, CMPA <ea>, An: a word source sign extended, the whole
/// of An; ADDA and SUBA leave the condition codes, CMPA sets them from
/// the long compare (CPU32RM 4.4, ADDA, CMPA, SUBA).  `SIZE` is a
/// [`Size::index`], word or long, `SOURCE` the source's kind.
fn address_arithmetic<O: Operation, const SIZE: u8, const SOURCE: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let source =
        core.effective_address_of(SOURCE, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    let source = size.sign_extend(core.read_operand(source, size)?);
    let register = register_field(opcode);
    let destination = core.cpu.registers.address(register);
    if O::WRITES {
        core.cpu
            .registers
            .set_address(register, O::combine(destination, source));
    } else {
        O::apply(&mut core.cpu.registers, source, destination, Size::Long);
    }
    Ok(Flow::Next)
}

/// Z as the extended instructions set it: cleared by a nonzero
/// `result`, else `zero`, its value before the instruction (CPU32RM
/// Table 4-1, ADDX: Z = Z & !Rm & ... & !R0).
const fn keep_zero(registers: &mut Registers, zero: bool, result: u32, size: Size) {
    registers.set_flag(status::Z, zero && result & size.mask() == 0);
}

/// ADDX, SUBX Dy, Dx or -(Ay), -(Ax): destination plus or minus the
/// source and X.  The carry and overflow rules are the general ones on
/// the result (CPU32RM Table 4-1; 4.4, ADDX, SUBX).
fn extended<const SUBTRACT: bool>(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let Some(size) = size(opcode) else {
        return Err(core.illegal());
    };
    // Ry, the source, is at bits 2:0; Rx, the destination, at 11:9.
    let mode = if opcode & MEMORY_TO_MEMORY != 0 {
        PREDECREMENT_FIELD
    } else {
        0
    };
    let source = core.effective_address(mode | low_register(opcode) as u16, size)?;
    let source = core.read_operand(source, size)?;
    let operand = core.effective_address(mode | register_field(opcode) as u16, size)?;
    let destination = core.read_operand(operand, size)?;
    let registers = &mut core.cpu.registers;
    let extend = u32::from(registers.flag(status::X));
    let zero = registers.flag(status::Z);
    let result = if SUBTRACT {
        let result = destination.wrapping_sub(source).wrapping_sub(extend) & size.mask();
        flags::subtract(registers, source, destination, result, size);
        result
    } else {
        let result = destination.wrapping_add(source).wrapping_add(extend) & size.mask();
        flags::add(registers, source, destination, result, size);
        result
    };
    keep_zero(registers, zero, result, size);
    core.write_operand(operand, size, result)?;
    Ok(Flow::Next)
}

/// CMPM (Ay)+, (Ax)+: (Ax) - (Ay), the condition codes only (CPU32RM
/// 4.4, CMPM).
fn cmpm(core: &mut Core<'_>, opcode: u16) -> Result<Flow, Exception> {
    let Some(size) = size(opcode) else {
        return Err(core.illegal());
    };
    let source = core.effective_address(POSTINCREMENT_FIELD | low_register(opcode) as u16, size)?;
    let source = core.read_operand(source, size)?;
    let destination =
        core.effective_address(POSTINCREMENT_FIELD | register_field(opcode) as u16, size)?;
    let destination = core.read_operand(destination, size)?;
    Compare::apply(&mut core.cpu.registers, source, destination, size);
    Ok(Flow::Next)
}

/// A single-operand operation: the result from the operand, and the
/// condition codes it sets.
pub(super) trait Unary {
    /// The result, `size` of it, with the condition codes set.
    fn apply(registers: &mut Registers, operand: u32, size: Size) -> u32;
}

/// NEG: 0 - destination (CPU32RM 4.4, NEG; Table 4-1's V = Dm & Rm,
/// C = Dm + Rm are the general subtract's with a zero destination).
struct Negate;

impl Unary for Negate {
    fn apply(registers: &mut Registers, operand: u32, size: Size) -> u32 {
        Subtract::apply(registers, operand, 0, size)
    }
}

/// NEGX: 0 - destination - X; Z as ADDX's (CPU32RM 4.4, NEGX).
struct NegateExtended;

impl Unary for NegateExtended {
    fn apply(registers: &mut Registers, operand: u32, size: Size) -> u32 {
        let extend = u32::from(registers.flag(status::X));
        let zero = registers.flag(status::Z);
        let result = 0u32.wrapping_sub(operand).wrapping_sub(extend) & size.mask();
        flags::subtract(registers, operand, 0, result, size);
        keep_zero(registers, zero, result, size);
        result
    }
}

/// [`single`]'s handler for `opcode`, specialized by size and operand
/// kind.
pub(super) fn single_handler<U: Unary>(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE => specialize_data_alterable!(
        kind::of(opcode & FIELD_MASK),
        OPERAND => single::<U, SIZE, OPERAND>
    ))
}

/// A single-operand instruction on a data alterable operand: read, the
/// operation, written back.  `SIZE` is a [`Size::index`], `OPERAND` the
/// operand's kind.
fn single<U: Unary, const SIZE: u8, const OPERAND: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let operand =
        core.effective_address_of(OPERAND, opcode & FIELD_MASK, size, AddressTable::Fetch)?;
    let value = core.read_operand(operand, size)?;
    let result = U::apply(&mut core.cpu.registers, value, size);
    core.write_operand(operand, size, result)?;
    Ok(Flow::Next)
}

/// [`clr`]'s handler for `opcode`, specialized by size and operand
/// kind.
fn clr_handler(opcode: u16) -> Handler {
    specialize_size!(size(opcode), SIZE => specialize_data_alterable!(
        kind::of(opcode & FIELD_MASK),
        OPERAND => clr::<SIZE, OPERAND>
    ))
}

/// CLR <ea>: zero written, Z set, N, V and C clear.  The operand is not
/// read: CPU32RM 8.3.8 addresses it with the calculate effective
/// address table, which has no read (CPU32RM 4.4, CLR).  `SIZE` is a
/// [`Size::index`], `OPERAND` the operand's kind.
fn clr<const SIZE: u8, const OPERAND: u8>(
    core: &mut Core<'_>,
    opcode: u16,
) -> Result<Flow, Exception> {
    let size = Size::of_index(SIZE);
    let operand =
        core.effective_address_of(OPERAND, opcode & FIELD_MASK, size, AddressTable::Calculate)?;
    core.write_operand(operand, size, 0)?;
    flags::logical(&mut core.cpu.registers, 0, size);
    Ok(Flow::Next)
}

#[cfg(test)]
mod tests {
    use crate::exception::Vector;
    use crate::instructions::data_movement::tests::FAULT_FRAME_BYTES;
    use crate::instructions::data_movement::tests::RAM_END;
    use crate::instructions::data_movement::tests::STACK;
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
    fn add_to_a_data_register_keeps_its_upper_bits() {
        // ADD.W D1, D0
        let mut cpu = machine(&[0xd041]);
        cpu.registers.data[0] = 0xabcd_7fff;
        cpu.registers.data[1] = 1;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xabcd_8000);
        assert_eq!(ccr(&cpu), status::N | status::V);
    }

    #[test]
    fn add_to_memory_carries_into_x() {
        // ADD.B D0, (A0)
        let mut cpu = machine(&[0xd110]);
        cpu.registers.set_address(0, 0x3000);
        cpu.registers.data[0] = 1;
        poke_word(&mut cpu, 0x3000, 0xff00);
        step(&mut cpu, 1);
        assert_eq!(peek_byte(&cpu, 0x3000), 0);
        assert_eq!(ccr(&cpu), status::X | status::Z | status::C);
    }

    #[test]
    fn add_long_from_an_address_register() {
        // ADD.L A1, D0
        let mut cpu = machine(&[0xd089]);
        cpu.registers.set_address(1, 0x8000_0000);
        cpu.registers.data[0] = 0x8000_0000;
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0);
        assert_eq!(ccr(&cpu), status::X | status::Z | status::V | status::C);
    }

    #[test]
    fn sub_borrows_and_sub_to_memory() {
        // SUB.L D1, D0; SUB.W D0, (A0)
        let mut cpu = machine(&[0x9081, 0x9150]);
        cpu.registers.data[1] = 1;
        cpu.registers.set_address(0, 0x3000);
        poke_word(&mut cpu, 0x3000, 0x0005);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xffff_ffff);
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
        step(&mut cpu, 1);
        assert_eq!(peek_word(&cpu, 0x3000), 6);
        assert_eq!(ccr(&cpu), status::X | status::C);
    }

    #[test]
    fn cmp_sets_flags_only_and_keeps_x() {
        // CMP.B D1, D0
        let mut cpu = machine(&[0xb001]);
        cpu.registers.data[0] = 0x80;
        cpu.registers.data[1] = 1;
        cpu.registers.set_ccr(status::X as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0x80);
        assert_eq!(ccr(&cpu), status::X | status::V);
    }

    #[test]
    fn address_forms_take_the_whole_register() {
        // ADDA.W D1, A0; SUBA.L D2, A1; CMPA.W D3, A2
        let mut cpu = machine(&[0xd0c1, 0x93c2, 0xb4c3]);
        cpu.registers.set_address(0, 0x1000);
        cpu.registers.data[1] = 0x0001_fffe;
        cpu.registers.set_address(1, 0x1000);
        cpu.registers.data[2] = 0x2000;
        cpu.registers.set_address(2, 0xffff_8000);
        cpu.registers.data[3] = 0x8000;
        cpu.registers.set_ccr(status::C as u8);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.address(0), 0x0ffe);
        assert_eq!(cpu.registers.address(1), 0xffff_f000);
        assert_eq!(ccr(&cpu), status::C);
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::Z);
    }

    #[test]
    fn immediates_come_before_the_destination_words() {
        // ADDI.L #$10000, (4, A0); SUBI.B #1, D0
        let mut cpu = machine(&[0x06a8, 0x0001, 0x0000, 0x0004, 0x0400, 0x0001]);
        cpu.registers.set_address(0, 0x3000);
        poke_long(&mut cpu, 0x3004, 0xffff);
        step(&mut cpu, 1);
        assert_eq!(peek_long(&cpu, 0x3004), 0x0001_ffff);
        assert_eq!(cpu.registers.pc, CODE + 8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xff);
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
    }

    #[test]
    fn cmpi_reads_a_pc_relative_destination() {
        // CMPI.W #5, (2, PC): the displacement's PC is its own word's.
        let mut cpu = machine(&[0x0c7a, 0x0005, 0x0002, 0x0005]);
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::Z);
        assert_eq!(cpu.registers.pc, CODE + 6);
    }

    #[test]
    fn quick_data_zero_is_eight_and_an_takes_no_flags() {
        // ADDQ.L #8, D0; ADDQ.W #1, A0; SUBQ.B #1, D1
        let mut cpu = machine(&[0x5080, 0x5248, 0x5301]);
        cpu.registers.set_address(0, 0xffff);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.data[0], 8);
        assert_eq!(cpu.registers.address(0), 0x1_0000);
        assert_eq!(ccr(&cpu), 0);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0xff);
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
    }

    #[test]
    fn addx_adds_x_and_only_clears_z() {
        // ADDX.L D1, D0 twice
        let mut cpu = machine(&[0xd181, 0xd181]);
        cpu.registers.data[0] = 0xffff_ffff;
        cpu.registers.set_ccr((status::X | status::Z) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0);
        assert_eq!(ccr(&cpu), status::X | status::Z | status::C);
        // 0 + 0 + 1: nonzero, Z cleared.
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 1);
        assert_eq!(ccr(&cpu), 0);
    }

    #[test]
    fn addx_and_subx_chain_through_memory() {
        // ADDX.B -(A1), -(A0); SUBX.W D1, D0
        let mut cpu = machine(&[0xd109, 0x9141]);
        cpu.registers.set_address(0, 0x3002);
        cpu.registers.set_address(1, 0x3004);
        poke_word(&mut cpu, 0x3000, 0x0080);
        poke_word(&mut cpu, 0x3002, 0x0080);
        cpu.registers.set_ccr(status::Z as u8);
        step(&mut cpu, 1);
        assert_eq!(peek_byte(&cpu, 0x3001), 0);
        assert_eq!(
            (cpu.registers.address(0), cpu.registers.address(1)),
            (0x3001, 0x3003)
        );
        assert_eq!(ccr(&cpu), status::X | status::Z | status::V | status::C);
        // 0 - 0 - X: a borrow, Z cleared.
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[0], 0xffff);
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
    }

    #[test]
    fn cmpm_compares_and_postincrements_both() {
        // CMPM.B (A1)+, (A0)+
        let mut cpu = machine(&[0xb109]);
        cpu.registers.set_address(0, 0x3000);
        cpu.registers.set_address(1, 0x3001);
        poke_word(&mut cpu, 0x3000, 0x4242);
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::Z);
        assert_eq!(
            (cpu.registers.address(0), cpu.registers.address(1)),
            (0x3001, 0x3002)
        );
    }

    #[test]
    fn neg_and_negx() {
        // NEG.W D0; NEG.W D1; NEGX.B D2
        let mut cpu = machine(&[0x4440, 0x4441, 0x4002]);
        cpu.registers.data[1] = 0x8000;
        step(&mut cpu, 1);
        assert_eq!(ccr(&cpu), status::Z);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[1], 0x8000);
        assert_eq!(ccr(&cpu), status::X | status::N | status::V | status::C);
        // 0 - 0 - X with Z set: $FF, Z cleared.
        cpu.registers.set_ccr((status::X | status::Z) as u8);
        step(&mut cpu, 1);
        assert_eq!(cpu.registers.data[2], 0xff);
        assert_eq!(ccr(&cpu), status::X | status::N | status::C);
    }

    #[test]
    fn clr_zeroes_and_sets_z() {
        // CLR.W D0; CLR.L (A0)+
        let mut cpu = machine(&[0x4240, 0x4298]);
        cpu.registers.data[0] = 0xffff_ffff;
        cpu.registers.set_address(0, 0x3000);
        poke_long(&mut cpu, 0x3000, 0xffff_ffff);
        cpu.registers
            .set_ccr((status::X | status::N | status::C) as u8);
        step(&mut cpu, 2);
        assert_eq!(cpu.registers.data[0], 0xffff_0000);
        assert_eq!(peek_long(&cpu, 0x3000), 0);
        assert_eq!(cpu.registers.address(0), 0x3004);
        assert_eq!(ccr(&cpu), status::X | status::Z);
    }

    #[test]
    fn an_odd_operand_restores_postincrement() {
        // ADD.W (A0)+, D0 with A0 odd
        let mut cpu = prepared(&[0xd058]);
        cpu.registers.set_address(0, 0x3001);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::ADDRESS_ERROR));
        assert_eq!(cpu.registers.address(0), 0x3001);
    }

    #[test]
    fn a_faulted_result_write_is_released_and_a_read_fault_restarts() {
        // CLR.L (A0) past RAM: the write is released, the instruction
        // completed, its flags set, the frame's PC the next one.
        let mut cpu = prepared(&[0x4290]);
        cpu.registers.set_address(0, RAM_END);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::BUS_ERROR));
        assert_eq!(peek_long(&cpu, STACK - FAULT_FRAME_BYTES + 2), CODE + 2);
        assert_eq!(
            peek_word(&cpu, STACK - FAULT_FRAME_BYTES) as u16 & status::CCR,
            status::Z
        );
        // ADD.L D1, (A1): its read faults, so it restarts.
        let mut cpu = prepared(&[0xd391]);
        cpu.registers.set_address(1, RAM_END);
        step(&mut cpu, 1);
        assert_eq!(taken(&cpu), Some(Vector::BUS_ERROR));
        assert_eq!(peek_long(&cpu, STACK - FAULT_FRAME_BYTES + 2), CODE);
    }

    #[test]
    fn illegal_combinations_are_not_claimed() {
        // ADD.B A0, D0; ADDQ.B #1, A0; CMPI.W #0, #0; NEG.W A0;
        // CLR.W (2, PC); ADDA.W with a mode-7 register 5.
        for opcode in [0xd008, 0x5208, 0x0c7c, 0x4448, 0x427a, 0xd0fd] {
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
