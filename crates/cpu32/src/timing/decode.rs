//! Each opcode's cost from its encoding (CPU32RM 4.5) and the timing
//! tables (8.3): which instruction, size and effective address modes
//! it is, and so which rows it adds up.  Decoded here, independent of
//! the instruction handlers; an opcode no encoding admits costs what
//! its exception does.

use crate::effective_address::Category;
use crate::effective_address::MODE_SHIFT;
use crate::effective_address::THREE_BITS;
use crate::effective_address::mode_of;
use crate::size::Size;
use crate::timing::Cost;
use crate::timing::Row;
use crate::timing::Sequence;
use crate::timing::rows;
use crate::timing::rows::arithmetic;
use crate::timing::rows::bit;
use crate::timing::rows::branch;
use crate::timing::rows::control;
use crate::timing::rows::exception;
use crate::timing::rows::extended;
use crate::timing::rows::immediate;
use crate::timing::rows::move_;
use crate::timing::rows::shift;
use crate::timing::rows::single;
use crate::timing::rows::special;

/// Bits 15:12, the line (CPU32RM Table 4-11).
const LINE_SHIFT: u16 = 12;
/// Line 1010, unassigned (CPU32RM Table 4-11, 6.2.8).
const LINE_A: u16 = 0xa;
/// Line 1111 (CPU32RM Table 4-11, 6.2.8).
const LINE_F: u16 = 0xf;
/// Bits 5:0: an effective address field, mode then register (CPU32RM
/// 3.4).
const FIELD_MASK: u16 = 0x3f;
/// A two-bit field.
const TWO_BITS: u16 = 0b11;
/// Bits 11:9: a register, or an operation (CPU32RM 4.5).
const REGISTER_SHIFT: u16 = 9;
/// Bits 7:6: the common size field (CPU32RM 4.5).
const SIZE_SHIFT: u16 = 6;
/// Bits 8:6: the operation mode (CPU32RM 4.5).
const OPMODE_SHIFT: u16 = 6;
/// Bit 8: Dn, <ea> rather than <ea>, Dn; signed for DIVS and MULS; L
/// rather than W for ADDA, CMPA and SUBA (CPU32RM 4.5).
const BIT_8: u16 = 1 << 8;
/// Bit 3: R/M, memory to memory, in ABCD, SBCD, ADDX, SUBX (CPU32RM
/// 4.5).
const MEMORY_TO_MEMORY: u16 = 1 << 3;
/// Bit 10: MOVEM dr, memory to register (CPU32RM 4.5).
const MOVEM_TO_REGISTERS: u16 = 1 << 10;
/// Bit 6: MOVEM size, long (CPU32RM 4.5).
const MOVEM_LONG: u16 = 1 << 6;
/// Bit 5: i/r, a shift count in a register (CPU32RM 4.5, ASL, ASR
/// (Register)).
const COUNT_IN_REGISTER: u16 = 1 << 5;
/// Where a register shift's type starts, bits 4:3 (CPU32RM 4.5).
const SHIFT_TYPE_SHIFT: u16 = 3;
/// Shift type 10: `ROXd` (CPU32RM 4.5, ROXL, ROXR (Register)).
const ROTATE_EXTEND: u16 = 0b10;
/// A register shift's count field 000: a count of eight (CPU32RM 4.4,
/// ASL).
const IMMEDIATE_COUNT_EIGHT: u16 = 8;
/// The Bcc, BRA and BSR displacement bits (CPU32RM 4.5, Bcc).
const DISPLACEMENT_MASK: u16 = 0xff;
/// Displacement $00: a 16-bit displacement follows.
const WORD_DISPLACEMENT: u16 = 0x00;
/// Displacement $FF: a 32-bit displacement follows.
const LONG_DISPLACEMENT: u16 = 0xff;
/// Data alterable modes (CPU32RM Table 3-1).
const DATA_ALTERABLE: Category = Category::DATA.and(Category::ALTERABLE);
/// Memory alterable modes (CPU32RM Table 3-1).
const MEMORY_ALTERABLE: Category = Category::MEMORY.and(Category::ALTERABLE);
/// Control alterable modes (CPU32RM Table 3-1).
const CONTROL_ALTERABLE: Category = Category::CONTROL.and(Category::ALTERABLE);

/// An effective address field's mode (CPU32RM 3.4, 3.4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Dn.
    DataRegister,
    /// An.
    AddressRegister,
    /// (An).
    Indirect,
    /// (An)+.
    Postincrement,
    /// -(An).
    Predecrement,
    /// (d16, An) and (d16, PC).
    Displacement,
    /// (d8, An, Xn) and (bd, An, Xn); with PC too.
    Index,
    /// (xxx).W.
    AbsoluteShort,
    /// (xxx).L.
    AbsoluteLong,
    /// #<data>.
    Immediate,
}

impl Mode {
    /// The mode of six-bit field `field`, none if it is no mode
    /// (CPU32RM 3.4.4, mode and register encodings).
    const fn of(field: u16) -> Option<Self> {
        let mode = match (mode_of(field), field & THREE_BITS) {
            (0b000, _) => Self::DataRegister,
            (0b001, _) => Self::AddressRegister,
            (0b010, _) => Self::Indirect,
            (0b011, _) => Self::Postincrement,
            (0b100, _) => Self::Predecrement,
            (0b101, _) | (0b111, 0b010) => Self::Displacement,
            (0b110, _) | (0b111, 0b011) => Self::Index,
            (0b111, 0b000) => Self::AbsoluteShort,
            (0b111, 0b001) => Self::AbsoluteLong,
            (0b111, 0b100) => Self::Immediate,
            _ => return None,
        };
        Some(mode)
    }

    /// Dn or An.
    const fn is_register(self) -> bool {
        matches!(self, Self::DataRegister | Self::AddressRegister)
    }

    /// The fetch effective address row (CPU32RM 8.3.1) for an operand
    /// of `size`; none for a register.
    const fn fetch(self, size: Size) -> Option<Row> {
        let modes = rows::FETCH;
        let row = match self {
            Self::DataRegister | Self::AddressRegister => return None,
            Self::Indirect => modes.indirect,
            Self::Postincrement => modes.postincrement,
            Self::Predecrement => modes.predecrement,
            Self::Displacement => modes.displacement,
            Self::Index => modes.index,
            Self::AbsoluteShort => modes.absolute_short,
            Self::AbsoluteLong => modes.absolute_long,
            Self::Immediate => match size {
                Size::Long => rows::FETCH_IMMEDIATE_LONG,
                Size::Byte | Size::Word => rows::FETCH_IMMEDIATE,
            },
        };
        Some(row.sized(size))
    }

    /// The calculate effective address row (CPU32RM 8.3.2); none for a
    /// register or an immediate, which have no address.
    const fn calculate(self) -> Option<Row> {
        let modes = rows::CALCULATE;
        let row = match self {
            Self::DataRegister | Self::AddressRegister | Self::Immediate => return None,
            Self::Indirect => modes.indirect,
            Self::Postincrement => modes.postincrement,
            Self::Predecrement => modes.predecrement,
            Self::Displacement => modes.displacement,
            Self::Index => modes.index,
            Self::AbsoluteShort => modes.absolute_short,
            Self::AbsoluteLong => modes.absolute_long,
        };
        Some(row)
    }
}

/// An encoding: the opcodes with `opcode & mask == pattern`, and how
/// to decode one into its rows, [`None`] if its fields admit no
/// instruction.
#[derive(Debug, Clone, Copy)]
struct Encoding {
    /// The fixed bits.
    mask: u16,
    /// Their values.
    pattern: u16,
    /// The rows of one such opcode.
    decode: fn(u16) -> Option<Sequence>,
    /// For an instruction that can trap, its trap and no-trap rows of
    /// the operation (CPU32RM 8.3.12, 8.3.13).
    trap: Option<(Row, Row)>,
}

/// The CPU32's encodings (CPU32RM 4.5), the first match winning, so a
/// specific encoding comes before the general one it is carved from.
/// Line A, and line F but TBL, have none.
const ENCODINGS: &[Encoding] = &[
    // Line 0: bit manipulation, MOVEP, immediate.
    encoding(0xffff, 0x003c, |_| fixed(control::IMMEDIATE_TO_CCR)), // ORI to CCR
    encoding(0xffff, 0x007c, |_| fixed(control::IMMEDIATE_TO_SR)),  // ORI to SR
    encoding(0xffff, 0x023c, |_| fixed(control::IMMEDIATE_TO_CCR)), // ANDI to CCR
    encoding(0xffff, 0x027c, |_| fixed(control::IMMEDIATE_TO_SR)),  // ANDI to SR
    encoding(0xffff, 0x0a3c, |_| fixed(control::IMMEDIATE_TO_CCR)), // EORI to CCR
    encoding(0xffff, 0x0a7c, |_| fixed(control::IMMEDIATE_TO_SR)),  // EORI to SR
    encoding(0xf138, 0x0108, movep),                                // MOVEP
    encoding(0xf100, 0x0100, bit_dynamic),                          // BTST, BCHG, BCLR, BSET Dn
    encoding(0xff00, 0x0800, bit_static),                           // BTST, BCHG, BCLR, BSET #
    encoding(0xff00, 0x0e00, moves),                                // MOVES
    trapping(
        0xf9c0,
        0x00c0,
        compare_bounds,
        control::CHK2_TRAP,
        arithmetic::COMPARE_BOUNDS,
    ), // CMP2, CHK2
    encoding(0xff00, 0x0000, immediate_alterable),                  // ORI
    encoding(0xff00, 0x0200, immediate_alterable),                  // ANDI
    encoding(0xff00, 0x0400, immediate_alterable),                  // SUBI
    encoding(0xff00, 0x0600, immediate_alterable),                  // ADDI
    encoding(0xff00, 0x0a00, immediate_alterable),                  // EORI
    encoding(0xff00, 0x0c00, compare_immediate),                    // CMPI
    // Lines 1, 2, 3: MOVE and MOVEA, byte, long, word.
    encoding(0xf000, 0x1000, |opcode| move_(opcode, Size::Byte)),
    encoding(0xf000, 0x2000, |opcode| move_(opcode, Size::Long)),
    encoding(0xf000, 0x3000, |opcode| move_(opcode, Size::Word)),
    // Line 4: miscellaneous.
    encoding(0xffff, 0x4e70, |_| fixed(exception::RESET)), // RESET
    encoding(0xffff, 0x4e71, |_| fixed(control::NOP)),     // NOP
    encoding(0xffff, 0x4e72, |_| fixed(exception::STOP)),  // STOP
    encoding(0xffff, 0x4e73, |_| fixed(rows::save::RTE_FOUR_WORD)), // RTE
    encoding(0xffff, 0x4e74, |_| fixed(control::RTD)),     // RTD
    encoding(0xffff, 0x4e75, |_| fixed(control::RTS)),     // RTS
    trapping(
        0xffff,
        0x4e76,
        |_| fixed(exception::TRAPV),
        exception::TRAPV_TRAP,
        exception::TRAPV,
    ), // TRAPV
    encoding(0xffff, 0x4e77, |_| fixed(control::RTR)),     // RTR
    encoding(0xffff, 0x4e7a, |_| fixed(special::MOVEC_FROM_CONTROL)), // MOVEC Cr, Rn
    encoding(0xffff, 0x4e7b, |_| fixed(special::MOVEC_TO_CONTROL)), // MOVEC Rn, Cr
    encoding(0xfff0, 0x4e40, |_| fixed(exception::TRAP)),  // TRAP
    encoding(0xfff8, 0x4e50, |_| fixed(control::LINK)),    // LINK.W
    encoding(0xfff8, 0x4e58, |_| fixed(control::UNLK)),    // UNLK
    encoding(0xfff0, 0x4e60, |_| fixed(special::MOVE_USP)), // MOVE USP
    encoding(0xfff8, 0x4808, |_| fixed(control::LINK_LONG)), // LINK.L
    encoding(0xfff8, 0x4840, |_| fixed(special::SWAP)),    // SWAP
    encoding(0xfff8, 0x4848, |_| fixed(exception::BKPT)),  // BKPT
    encoding(0xfff8, 0x4880, |_| fixed(single::EXT)),      // EXT.W
    encoding(0xfff8, 0x48c0, |_| fixed(single::EXT)),      // EXT.L
    encoding(0xfff8, 0x49c0, |_| fixed(single::EXT)),      // EXTB.L
    encoding(0xffc0, 0x40c0, |opcode| {
        store_status(opcode, special::SR_TO_DATA, special::SR_TO_CALCULATE)
    }), // MOVE from SR
    encoding(0xffc0, 0x42c0, |opcode| {
        store_status(opcode, special::CCR_TO_DATA, special::CCR_TO_CALCULATE)
    }), // MOVE from CCR
    encoding(0xffc0, 0x44c0, |opcode| {
        load_status(opcode, special::DATA_TO_CCR, special::FETCH_TO_CCR)
    }), // MOVE to CCR
    encoding(0xffc0, 0x46c0, |opcode| {
        load_status(opcode, special::DATA_TO_SR, special::FETCH_TO_SR)
    }), // MOVE to SR
    encoding(0xffc0, 0x4800, nbcd),                        // NBCD
    encoding(0xffc0, 0x4840, |opcode| {
        control_operand(opcode, control::PEA)
    }), // PEA
    // ILLEGAL, and BGND, which with background mode disabled takes the
    // illegal instruction exception (CPU32RM 4.4, ILLEGAL; 7.2.2.2):
    // both are ILLEGAL's row (8.3.13).  Carved from TAS's encoding.
    encoding(0xffff, 0x4afc, |_| fixed(exception::ILLEGAL)), // ILLEGAL
    encoding(0xffff, 0x4afa, |_| fixed(exception::ILLEGAL)), // BGND
    encoding(0xffc0, 0x4ac0, tas),                           // TAS
    encoding(0xffc0, 0x4c00, |opcode| {
        long_save(opcode, arithmetic::MULTIPLY_QUAD)
    }), // MULS.L, MULU.L
    encoding(0xffc0, 0x4c40, |opcode| {
        long_save(opcode, arithmetic::DIVIDE_UNSIGNED_LONG)
    }), // DIVS.L, DIVU.L
    encoding(0xffc0, 0x4e80, |opcode| {
        control_operand(opcode, control::JSR)
    }), // JSR
    encoding(0xffc0, 0x4ec0, |opcode| {
        control_operand(opcode, control::JMP)
    }), // JMP
    encoding(0xfb80, 0x4880, movem),                         // MOVEM
    encoding(0xf1c0, 0x41c0, |opcode| {
        control_operand(opcode, control::LEA)
    }), // LEA
    trapping(
        0xf1c0,
        0x4180,
        |opcode| chk(opcode, Size::Word),
        control::CHK_TRAP,
        control::CHK,
    ), // CHK.W
    trapping(
        0xf1c0,
        0x4100,
        |opcode| chk(opcode, Size::Long),
        control::CHK_TRAP,
        control::CHK,
    ), // CHK.L
    encoding(0xff00, 0x4000, negate),                        // NEGX
    encoding(0xff00, 0x4200, clr),                           // CLR
    encoding(0xff00, 0x4400, negate),                        // NEG
    encoding(0xff00, 0x4600, negate),                        // NOT
    encoding(0xff00, 0x4a00, tst),                           // TST
    // Line 5: ADDQ, SUBQ, Scc, DBcc, TRAPcc.
    encoding(0xf0f8, 0x50c8, |_| fixed(branch::DBCC_TRUE)), // DBcc
    trapping(
        0xf0ff,
        0x50fa,
        |_| fixed(exception::TRAPCC_WORD),
        exception::TRAPCC_WORD_TRAP,
        exception::TRAPCC_WORD,
    ), // TRAPcc.W
    trapping(
        0xf0ff,
        0x50fb,
        |_| fixed(exception::TRAPCC_LONG),
        exception::TRAPCC_LONG_TRAP,
        exception::TRAPCC_LONG,
    ), // TRAPcc.L
    trapping(
        0xf0ff,
        0x50fc,
        |_| fixed(exception::TRAPCC),
        exception::TRAPCC_TRAP,
        exception::TRAPCC,
    ), // TRAPcc
    encoding(0xf0c0, 0x50c0, scc),                          // Scc
    encoding(0xf000, 0x5000, quick),                        // ADDQ, SUBQ
    // Line 6: Bcc, BRA, BSR.
    encoding(0xff00, 0x6000, |_| fixed(branch::TAKEN)), // BRA
    encoding(0xff00, 0x6100, bsr),                      // BSR
    encoding(0xf000, 0x6000, |opcode| {
        fixed(branch_not_taken(displacement(opcode)))
    }), // Bcc
    // Line 7: MOVEQ.
    encoding(0xf100, 0x7000, |_| fixed(immediate::MOVEQ)),
    // Line 8: OR, DIV, SBCD.
    encoding(0xf1f0, 0x8100, decimal),     // SBCD
    encoding(0xf0c0, 0x80c0, divide_word), // DIVS.W, DIVU.W
    encoding(0xf000, 0x8000, |opcode| logical(opcode, Category::DATA)), // OR
    // Line 9: SUB, SUBA, SUBX.
    encoding(0xf0c0, 0x90c0, address_arithmetic), // SUBA
    encoding(0xf130, 0x9100, extended),           // SUBX
    encoding(0xf000, 0x9000, arithmetic),         // SUB
    // Line B: CMP, CMPA, CMPM, EOR.
    encoding(0xf0c0, 0xb0c0, address_arithmetic), // CMPA
    encoding(0xf138, 0xb108, cmpm),               // CMPM
    encoding(0xf100, 0xb100, eor),                // EOR
    encoding(0xf100, 0xb000, compare),            // CMP
    // Line C: AND, MUL, ABCD, EXG.
    encoding(0xf0c0, 0xc0c0, multiply_word), // MULS.W, MULU.W
    encoding(0xf1f0, 0xc100, decimal),       // ABCD
    encoding(0xf1f8, 0xc140, |_| fixed(special::EXG)), // EXG Dx, Dy
    encoding(0xf1f8, 0xc148, |_| fixed(special::EXG)), // EXG Ax, Ay
    encoding(0xf1f8, 0xc188, |_| fixed(special::EXG)), // EXG Dx, Ay
    encoding(0xf000, 0xc000, |opcode| logical(opcode, Category::DATA)), // AND
    // Line D: ADD, ADDA, ADDX.
    encoding(0xf0c0, 0xd0c0, address_arithmetic), // ADDA
    encoding(0xf130, 0xd100, extended),           // ADDX
    encoding(0xf000, 0xd000, arithmetic),         // ADD
    // Line E: shift and rotate; the bit field encodings, 1110 1xxx
    // 11, are the 68020's.
    encoding(0xf8c0, 0xe0c0, shift_memory), // ASd, LSd, ROXd, ROd <ea>
    encoding(0xf000, 0xe000, shift_register), // ASd, LSd, ROXd, ROd Dn
    // Line F: TBL, and LPSTOP, which shares $F800 with TBL Dym = D0.
    encoding(0xffc0, 0xf800, table_lookup),
];

/// An encoding, for [`ENCODINGS`].
const fn encoding(mask: u16, pattern: u16, decode: fn(u16) -> Option<Sequence>) -> Encoding {
    Encoding {
        mask,
        pattern,
        decode,
        trap: None,
    }
}

/// An encoding of an instruction that can trap, with its `trap` and
/// `no_trap` operation rows.
const fn trapping(
    mask: u16,
    pattern: u16,
    decode: fn(u16) -> Option<Sequence>,
    trap: Row,
    no_trap: Row,
) -> Encoding {
    Encoding {
        trap: Some((trap, no_trap)),
        ..encoding(mask, pattern, decode)
    }
}

/// The cost of `opcode`.  One no encoding admits takes its exception:
/// line A, line F (first word illegal), or illegal (CPU32RM 8.3.13).
pub(super) fn cost(opcode: u16) -> Cost {
    find(opcode)
        .and_then(|encoding| (encoding.decode)(opcode))
        .map_or_else(
            || match opcode >> LINE_SHIFT {
                LINE_A => exception::LINE_A.cost(),
                LINE_F => exception::LINE_F.cost(),
                _ => exception::ILLEGAL.cost(),
            },
            Sequence::cost,
        )
}

/// Whether an encoding admits `opcode`.
#[cfg(test)]
pub(super) fn is_instruction(opcode: u16) -> bool {
    find(opcode).is_some_and(|encoding| (encoding.decode)(opcode).is_some())
}

/// A single row.
const fn fixed(row: Row) -> Option<Sequence> {
    Some(Sequence::EMPTY.then(row))
}

/// The effective address field.
const fn field(opcode: u16) -> u16 {
    opcode & FIELD_MASK
}

/// The common size field, bits 7:6.
const fn size(opcode: u16) -> Option<Size> {
    Size::from_field(opcode >> SIZE_SHIFT)
}

/// The mode of the field, if `category` admits it.
const fn mode_in(opcode: u16, category: Category) -> Option<Mode> {
    if category.admits(field(opcode)) {
        Mode::of(field(opcode))
    } else {
        None
    }
}

/// The mode of the field if it is any mode, An not for a byte.
const fn any_mode(opcode: u16, size: Size) -> Option<Mode> {
    match Mode::of(field(opcode)) {
        Some(Mode::AddressRegister) if matches!(size, Size::Byte) => None,
        mode => mode,
    }
}

/// An operand read: in a register, `register`; in memory, its fetch
/// effective address then `memory`, both of `size`.
const fn operand(mode: Mode, size: Size, register: Row, memory: Row) -> Option<Sequence> {
    if mode.is_register() {
        fixed(register)
    } else {
        Some(
            Sequence::EMPTY
                .then_some(mode.fetch(size))
                .then(memory.sized(size)),
        )
    }
}

/// An operand addressed only: in a data register, `register`;
/// in memory, its calculate effective address then `memory`.
const fn addressed(mode: Mode, size: Size, register: Row, memory: Row) -> Option<Sequence> {
    if mode.is_register() {
        fixed(register)
    } else {
        Some(
            Sequence::EMPTY
                .then_some(mode.calculate())
                .then(memory.sized(size)),
        )
    }
}

/// ORI, ANDI, SUBI, ADDI, EORI: a data alterable destination.
const fn immediate_alterable(opcode: u16) -> Option<Sequence> {
    immediate_to(opcode, DATA_ALTERABLE)
}

/// CMPI: any data destination but an immediate (CPU32RM 4.4, CMPI).
const fn compare_immediate(opcode: u16) -> Option<Sequence> {
    match Mode::of(field(opcode)) {
        Some(Mode::Immediate) => None,
        _ => immediate_to(opcode, Category::DATA),
    }
}

/// The immediate instructions: the # fetch effective address, then
/// the #, Rn or #, <FEA> row (CPU32RM 8.3.6).
const fn immediate_to(opcode: u16, category: Category) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, category) else {
        return None;
    };
    let Some(operation) = operand(mode, size, immediate::TO_REGISTER, immediate::TO_MEMORY) else {
        return None;
    };
    Some(then_all(
        Sequence::EMPTY.then_some(Mode::Immediate.fetch(size)),
        operation,
    ))
}

/// `first`, then the parts of `second` as one part: their combined
/// head, cycles and tail.  Equal to adding them part by part, since
/// overlap only joins neighbors (CPU32RM 8.1.6).
const fn then_all(first: Sequence, second: Sequence) -> Sequence {
    first.then(Row::new(
        second.head,
        second.tail,
        (second.cycles as i16).cast_unsigned(),
    ))
}

/// MOVEP (CPU32RM 8.3.4), by its opmode.
const fn movep(opcode: u16) -> Option<Sequence> {
    match opcode >> OPMODE_SHIFT & THREE_BITS {
        0b100 => fixed(special::MOVEP_WORD_TO_REGISTER),
        0b101 => fixed(special::MOVEP_LONG_TO_REGISTER),
        0b110 => fixed(special::MOVEP_WORD_TO_MEMORY),
        0b111 => fixed(special::MOVEP_LONG_TO_MEMORY),
        _ => None,
    }
}

/// Whether a bit instruction's type, bits 7:6, is BTST (CPU32RM 4.5).
const fn is_bit_test(opcode: u16) -> bool {
    opcode >> SIZE_SHIFT & TWO_BITS == 0b00
}

/// BTST, BCHG, BCLR, BSET Dn, <ea> (CPU32RM 8.3.10): BTST reads any
/// data operand, the others a data alterable one; a byte in memory.
const fn bit_dynamic(opcode: u16) -> Option<Sequence> {
    let (category, register, memory) = if is_bit_test(opcode) {
        (Category::DATA, bit::TEST_REGISTER, bit::TEST_MEMORY)
    } else {
        (DATA_ALTERABLE, bit::CHANGE_REGISTER, bit::CHANGE_MEMORY)
    };
    let Some(mode) = mode_in(opcode, category) else {
        return None;
    };
    operand(mode, Size::Byte, register, memory)
}

/// BTST, BCHG, BCLR, BSET #, <ea> (CPU32RM 8.3.10): the # fetch
/// effective address first; BTST takes no immediate operand.
const fn bit_static(opcode: u16) -> Option<Sequence> {
    let (category, register, memory) = if is_bit_test(opcode) {
        (
            Category::DATA,
            bit::TEST_IMMEDIATE_REGISTER,
            bit::TEST_IMMEDIATE_MEMORY,
        )
    } else {
        (
            DATA_ALTERABLE,
            bit::CHANGE_IMMEDIATE_REGISTER,
            bit::CHANGE_IMMEDIATE_MEMORY,
        )
    };
    let Some(mode) = mode_in(opcode, category) else {
        return None;
    };
    if matches!(mode, Mode::Immediate) {
        return None;
    }
    let Some(operation) = operand(mode, Size::Byte, register, memory) else {
        return None;
    };
    Some(then_all(
        Sequence::EMPTY.then_some(Mode::Immediate.fetch(Size::Byte)),
        operation,
    ))
}

/// MOVES (CPU32RM 8.3.4): save, calculate, then the operation, charged
/// memory to register; `timing::moves` adds the other direction.
const fn moves(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, MEMORY_ALTERABLE) else {
        return None;
    };
    Some(
        Sequence::EMPTY
            .then(special::MOVES_SAVE)
            .then_some(mode.calculate())
            .then(special::MOVES_TO_REGISTER.sized(size)),
    )
}

/// CMP2 and CHK2 (CPU32RM 8.3.5, 8.3.12): save, fetch, operation.
/// Their size is in bits 10:9.
const fn compare_bounds(opcode: u16) -> Option<Sequence> {
    let Some(size) = Size::from_field(opcode >> REGISTER_SHIFT) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, Category::CONTROL) else {
        return None;
    };
    Some(
        Sequence::EMPTY
            .then(arithmetic::SAVE)
            .then_some(mode.fetch(size))
            .then(arithmetic::COMPARE_BOUNDS.sized(size)),
    )
}

/// MOVE's destination field, its mode and register swapped back to
/// the usual order (CPU32RM 4.5, MOVE).
const fn move_destination(opcode: u16) -> u16 {
    (opcode >> MODE_SHIFT & FIELD_MASK & !THREE_BITS) | (opcode >> REGISTER_SHIFT & THREE_BITS)
}

/// MOVE and MOVEA (CPU32RM 8.3.3): the first row that matches both
/// operands.
const fn move_(opcode: u16, size: Size) -> Option<Sequence> {
    let Some(source) = any_mode(opcode, size) else {
        return None;
    };
    let destination_field = move_destination(opcode);
    let Some(destination) = Mode::of(destination_field) else {
        return None;
    };
    let admitted = match destination {
        Mode::AddressRegister => !matches!(size, Size::Byte),
        _ => DATA_ALTERABLE.admits(destination_field),
    };
    if !admitted {
        return None;
    }
    let sequence = match (source, destination) {
        (_, Mode::DataRegister | Mode::AddressRegister) => {
            if source.is_register() {
                Sequence::EMPTY.then(move_::REGISTER_TO_REGISTER)
            } else {
                Sequence::EMPTY
                    .then_some(source.fetch(size))
                    .then(move_::FETCH_TO_REGISTER)
            }
        }
        (Mode::DataRegister | Mode::AddressRegister, Mode::Indirect) => {
            Sequence::EMPTY.then(move_::REGISTER_TO_INDIRECT.sized(size))
        }
        (Mode::DataRegister | Mode::AddressRegister, Mode::Postincrement) => {
            Sequence::EMPTY.then(move_::REGISTER_TO_POSTINCREMENT.sized(size))
        }
        (Mode::DataRegister | Mode::AddressRegister, Mode::Predecrement) => {
            Sequence::EMPTY.then(move_::REGISTER_TO_PREDECREMENT.sized(size))
        }
        (Mode::DataRegister | Mode::AddressRegister, _) => Sequence::EMPTY
            .then_some(destination.calculate())
            .then(move_::REGISTER_TO_CALCULATE.sized(size)),
        (_, Mode::Indirect | Mode::Postincrement | Mode::Predecrement) => Sequence::EMPTY
            .then_some(source.fetch(size))
            .then(move_::FETCH_TO_INCREMENT.sized(size)),
        (Mode::Immediate, _) => Sequence::EMPTY
            .then_some(source.fetch(size))
            .then_some(destination.calculate())
            .then(move_::IMMEDIATE_TO_CALCULATE.sized(size)),
        _ => Sequence::EMPTY
            .then_some(source.calculate())
            .then_some(destination.fetch(size))
            .then(move_::CALCULATE_TO_FETCH.sized(size)),
    };
    Some(sequence)
}

/// MOVE from SR and from CCR (CPU32RM 8.3.4): a data alterable
/// destination, a word.
const fn store_status(opcode: u16, register: Row, memory: Row) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, DATA_ALTERABLE) else {
        return None;
    };
    addressed(mode, Size::Word, register, memory)
}

/// MOVE to SR and to CCR (CPU32RM 8.3.4): any data source, a word.
const fn load_status(opcode: u16, register: Row, memory: Row) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, Category::DATA) else {
        return None;
    };
    operand(mode, Size::Word, register, memory)
}

/// NBCD (CPU32RM 8.3.8): a data alterable byte.
const fn nbcd(opcode: u16) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, DATA_ALTERABLE) else {
        return None;
    };
    operand(mode, Size::Byte, single::NBCD_DATA, single::NBCD_MEMORY)
}

/// TAS (CPU32RM 8.3.8): a data alterable byte, addressed by the
/// calculate table.
const fn tas(opcode: u16) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, DATA_ALTERABLE) else {
        return None;
    };
    addressed(mode, Size::Byte, single::TAS_DATA, single::TAS_CALCULATE)
}

/// LEA, PEA, JMP, JSR (CPU32RM 8.3.12): a control address, calculated.
const fn control_operand(opcode: u16, operation: Row) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, Category::CONTROL) else {
        return None;
    };
    Some(Sequence::EMPTY.then_some(mode.calculate()).then(operation))
}

/// MUL.L and DIV.L (CPU32RM 8.3.5): save, fetch, operation, but no
/// save for a data register source.  The table charges the cheaper of
/// each pair of forms; `timing::multiply_long` and
/// `timing::divide_long` add the rest from the second word.
const fn long_save(opcode: u16, operation: Row) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, Category::DATA) else {
        return None;
    };
    if mode.is_register() {
        return fixed(operation);
    }
    Some(
        Sequence::EMPTY
            .then(arithmetic::SAVE)
            .then_some(mode.fetch(Size::Long))
            .then(operation),
    )
}

/// MOVEM (CPU32RM 8.3.4) with no register: `timing::movem` adds each.
/// To memory, control alterable or -(An); to registers, control or
/// (An)+.
const fn movem(opcode: u16) -> Option<Sequence> {
    let to_registers = opcode & MOVEM_TO_REGISTERS != 0;
    let long = opcode & MOVEM_LONG != 0;
    let mode = Mode::of(field(opcode));
    let admitted = if to_registers {
        Category::CONTROL.admits(field(opcode)) || matches!(mode, Some(Mode::Postincrement))
    } else {
        CONTROL_ALTERABLE.admits(field(opcode)) || matches!(mode, Some(Mode::Predecrement))
    };
    let Some(mode) = mode else {
        return None;
    };
    if !admitted {
        return None;
    }
    let operation = match (to_registers, long) {
        (true, false) => special::MOVEM_WORD_TO_REGISTERS,
        (true, true) => special::MOVEM_LONG_TO_REGISTERS,
        (false, false) => special::MOVEM_WORD_TO_MEMORY,
        (false, true) => special::MOVEM_LONG_TO_MEMORY,
    };
    Some(Sequence::EMPTY.then_some(mode.calculate()).then(operation))
}

/// CHK (CPU32RM 8.3.12), no exception: any data bound.
const fn chk(opcode: u16, size: Size) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, Category::DATA) else {
        return None;
    };
    operand(mode, size, control::CHK, control::CHK)
}

/// NEGX, NEG, NOT (CPU32RM 8.3.8): data alterable.
const fn negate(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, DATA_ALTERABLE) else {
        return None;
    };
    operand(mode, size, single::NEGATE_DATA, single::NEGATE_MEMORY)
}

/// CLR (CPU32RM 8.3.8): data alterable, addressed by the calculate
/// table.
const fn clr(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, DATA_ALTERABLE) else {
        return None;
    };
    addressed(mode, size, single::CLR_DATA, single::CLR_CALCULATE)
}

/// TST (CPU32RM 8.3.8): any mode, An not for a byte (CPU32RM 4.4,
/// TST).
const fn tst(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = any_mode(opcode, size) else {
        return None;
    };
    operand(mode, size, single::TST, single::TST)
}

/// Scc (CPU32RM 8.3.8): a data alterable byte.
const fn scc(opcode: u16) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, DATA_ALTERABLE) else {
        return None;
    };
    addressed(mode, Size::Byte, single::SCC_DATA, single::SCC_CALCULATE)
}

/// ADDQ and SUBQ (CPU32RM 8.3.6): alterable, An not for a byte.
const fn quick(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, Category::ALTERABLE) else {
        return None;
    };
    if matches!((mode, size), (Mode::AddressRegister, Size::Byte)) {
        return None;
    }
    operand(
        mode,
        size,
        immediate::QUICK_TO_REGISTER,
        immediate::QUICK_TO_MEMORY,
    )
}

/// A branch's displacement size, from its low byte (CPU32RM 4.4, Bcc).
const fn displacement(opcode: u16) -> Size {
    match opcode & DISPLACEMENT_MASK {
        WORD_DISPLACEMENT => Size::Word,
        LONG_DISPLACEMENT => Size::Long,
        _ => Size::Byte,
    }
}

/// Bcc not taken, by displacement size (CPU32RM 8.3.11).
pub(super) const fn branch_not_taken(size: Size) -> Row {
    match size {
        Size::Byte => branch::NOT_TAKEN_BYTE,
        Size::Word => branch::NOT_TAKEN_WORD,
        Size::Long => branch::NOT_TAKEN_LONG,
    }
}

/// BSR, by displacement size (CPU32RM 8.3.12).
const fn bsr(opcode: u16) -> Option<Sequence> {
    match displacement(opcode) {
        Size::Byte | Size::Word => fixed(control::BSR),
        Size::Long => fixed(control::BSR_LONG),
    }
}

/// ABCD and SBCD (CPU32RM 8.3.7).
const fn decimal(opcode: u16) -> Option<Sequence> {
    if opcode & MEMORY_TO_MEMORY != 0 {
        fixed(extended::DECIMAL_MEMORY)
    } else {
        fixed(extended::DECIMAL_REGISTER)
    }
}

/// DIVS.W and DIVU.W (CPU32RM 8.3.5): any data source, a word.
const fn divide_word(opcode: u16) -> Option<Sequence> {
    let operation = if opcode & BIT_8 != 0 {
        arithmetic::DIVIDE_SIGNED_WORD
    } else {
        arithmetic::DIVIDE_UNSIGNED_WORD
    };
    let Some(mode) = mode_in(opcode, Category::DATA) else {
        return None;
    };
    operand(mode, Size::Word, operation, operation)
}

/// MULS.W and MULU.W (CPU32RM 8.3.5): any data source, a word.
const fn multiply_word(opcode: u16) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, Category::DATA) else {
        return None;
    };
    operand(
        mode,
        Size::Word,
        arithmetic::MULTIPLY_WORD,
        arithmetic::MULTIPLY_WORD,
    )
}

/// Dn, <ea> to memory (CPU32RM 8.3.5): memory alterable.
const fn to_memory(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, MEMORY_ALTERABLE) else {
        return None;
    };
    operand(mode, size, arithmetic::TO_MEMORY, arithmetic::TO_MEMORY)
}

/// OR and AND (CPU32RM 8.3.5): <ea>, Dn from a `source` operand, or
/// Dn, <ea> to memory.
const fn logical(opcode: u16, source: Category) -> Option<Sequence> {
    if opcode & BIT_8 != 0 {
        return to_memory(opcode);
    }
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, source) else {
        return None;
    };
    operand(mode, size, arithmetic::TO_REGISTER, arithmetic::TO_REGISTER)
}

/// <ea>, Dn from any operand, An not for a byte (CPU32RM 8.3.5).
const fn from_any(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = any_mode(opcode, size) else {
        return None;
    };
    operand(mode, size, arithmetic::TO_REGISTER, arithmetic::TO_REGISTER)
}

/// ADD and SUB (CPU32RM 8.3.5).
const fn arithmetic(opcode: u16) -> Option<Sequence> {
    if opcode & BIT_8 != 0 {
        to_memory(opcode)
    } else {
        from_any(opcode)
    }
}

/// CMP (CPU32RM 8.3.5).
const fn compare(opcode: u16) -> Option<Sequence> {
    from_any(opcode)
}

/// EOR Dn, <ea> (CPU32RM 8.3.5): data alterable.
const fn eor(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    let Some(mode) = mode_in(opcode, DATA_ALTERABLE) else {
        return None;
    };
    operand(mode, size, arithmetic::TO_REGISTER, arithmetic::TO_MEMORY)
}

/// ADDA, CMPA, SUBA (CPU32RM 8.3.5): any source; bit 8 says long.
const fn address_arithmetic(opcode: u16) -> Option<Sequence> {
    let size = if opcode & BIT_8 != 0 {
        Size::Long
    } else {
        Size::Word
    };
    let Some(mode) = any_mode(opcode, size) else {
        return None;
    };
    operand(mode, size, arithmetic::TO_REGISTER, arithmetic::TO_REGISTER)
}

/// ADDX and SUBX (CPU32RM 8.3.7).
const fn extended(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    if opcode & MEMORY_TO_MEMORY != 0 {
        fixed(extended::MEMORY.sized(size))
    } else {
        fixed(extended::REGISTER)
    }
}

/// CMPM (CPU32RM 8.3.7).
const fn cmpm(opcode: u16) -> Option<Sequence> {
    let Some(size) = size(opcode) else {
        return None;
    };
    fixed(extended::CMPM.sized(size))
}

/// Shift and rotate memory by one (CPU32RM 8.3.9): memory alterable,
/// a word.
const fn shift_memory(opcode: u16) -> Option<Sequence> {
    let Some(mode) = mode_in(opcode, MEMORY_ALTERABLE) else {
        return None;
    };
    operand(mode, Size::Word, shift::MEMORY, shift::MEMORY)
}

/// Whether a register shift is `ROXd`.
pub(super) const fn is_rotate_extend(opcode: u16) -> bool {
    opcode >> SHIFT_TYPE_SHIFT & TWO_BITS == ROTATE_EXTEND
}

/// Shift and rotate a data register (CPU32RM 8.3.9).  A count in the
/// opcode is costed here; a count in a register at zero, and
/// `timing::shift_count` adds the rest.
const fn shift_register(opcode: u16) -> Option<Sequence> {
    if size(opcode).is_none() {
        return None;
    }
    if opcode & COUNT_IN_REGISTER != 0 {
        return fixed(shift::computed(shift::MINIMUM_CLOCKS));
    }
    if !is_rotate_extend(opcode) {
        return fixed(shift::IMMEDIATE);
    }
    let count = match opcode >> REGISTER_SHIFT & THREE_BITS {
        0 => IMMEDIATE_COUNT_EIGHT,
        count => count,
    };
    fixed(shift::computed(shift::rotate_extend_immediate_clocks(
        count,
    )))
}

/// Whether a TBL opcode is the data register interpolate form (CPU32RM
/// 4.5, TBLS): mode 000, Dym in the register field.
pub(super) const fn is_table_register_form(opcode: u16) -> bool {
    matches!(Mode::of(field(opcode)), Some(Mode::DataRegister))
}

/// TBLS, TBLU, TBLSN, TBLUN (CPU32RM 8.3.5), charged as rounded, byte
/// or word, as the second word decides: `timing::table_lookup` adds
/// the rest.  The table lookup form takes a control address: save,
/// calculate, operation.  $F800 is also LPSTOP's first word, charged
/// here as TBL; `timing::lpstop` takes LPSTOP's difference back.
const fn table_lookup(opcode: u16) -> Option<Sequence> {
    if is_table_register_form(opcode) {
        return fixed(arithmetic::TABLE_REGISTERS);
    }
    let Some(mode) = mode_in(opcode, Category::CONTROL) else {
        return None;
    };
    Some(
        Sequence::EMPTY
            .then(arithmetic::SAVE)
            .then_some(mode.calculate())
            .then(arithmetic::TABLE),
    )
}

/// The F-line exception for an illegal second opcode word in a $F8xx
/// opcode (CPU32RM 8.3.13, F-line (Second word illegal)): with ea = Rn
/// one row, else save, the effective address, and the operation.
pub(super) const fn line_f_second_word(opcode: u16) -> Cost {
    match Mode::of(field(opcode)) {
        Some(mode) if !mode.is_register() => Sequence::EMPTY
            .then(arithmetic::SAVE)
            .then_some(mode.calculate())
            .then(exception::LINE_F_SECOND_WORD)
            .cost(),
        _ => exception::LINE_F_SECOND_WORD_REGISTER.cost(),
    }
}

/// The trap and no-trap rows of a CHK, CHK2, TRAPcc or TRAPV opcode
/// (CPU32RM 8.3.12, 8.3.13).
pub(super) const fn trap(opcode: u16) -> Option<(Row, Row)> {
    match find(opcode) {
        Some(encoding) => encoding.trap,
        None => None,
    }
}

/// The first encoding that matches `opcode`.
const fn find(opcode: u16) -> Option<Encoding> {
    let mut index = 0;
    while index < ENCODINGS.len() {
        let encoding = ENCODINGS[index];
        if opcode & encoding.mask == encoding.pattern {
            return Some(encoding);
        }
        index += 1;
    }
    None
}
