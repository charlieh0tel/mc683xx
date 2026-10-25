//! Instruction timing: each opcode's head, tail and cycles from the
//! timing tables (CPU32RM 8.3), the effective address cost included.
//! The run loop charges an instruction its cycles less the overlap
//! with the last one's tail (CPU32RM 8.1.6), plus its bus cycles'
//! stall and what the instruction adds itself.
//!
//! The table holds what the opcode determines.  What only the operands
//! or the outcome determine (a branch taken, a shift count, a MOVEM
//! register list, a second opcode word) the handler adds with
//! [`Core::charge`](crate::cpu::Core::charge) from the functions here;
//! exception processing charges the costs in [`exception`].

mod decode;
pub(crate) mod exception;
mod rows;

use std::sync::OnceLock;

use crate::cycles::Cycles;
use crate::effective_address::kind;
use crate::instructions::OPCODES;
use crate::size::Size;

/// One opcode's timing (CPU32RM 8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cost {
    /// Clocks at its start that can overlap the last instruction.
    pub(crate) head: u8,
    /// Clocks at its end the next instruction can overlap.
    pub(crate) tail: u8,
    /// Its clocks on a two-clock bus.
    pub(crate) cycles: u16,
}

/// Every opcode's cost, by opcode.  Built once: about 15 ms in a
/// debug build, 3 ms optimized.  An array, so an opcode indexes it
/// unchecked.
pub(crate) fn table() -> &'static [Cost; OPCODES] {
    static TABLE: OnceLock<Box<[Cost; OPCODES]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=u16::MAX)
            .map(decode::cost)
            .collect::<Box<[Cost]>>()
            .try_into()
            .expect("a cost for every opcode")
    })
}

/// Clocks a long operand's second bus cycle adds, to the tail and the
/// cycles, for each access a table marks "X" (CPU32RM 8.3.1, 8.3).
const LONG_CLOCKS: i8 = 2;

/// One row of a timing table (CPU32RM 8.3), as printed: for byte and
/// word operands on a two-clock bus.  Tails may be negative (8.1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    /// The head column.
    head: i8,
    /// The tail column.
    tail: i8,
    /// The cycles column; where it gives a range, its maximum.
    cycles: u16,
    /// The operand accesses it marks "X": one bus cycle each for a
    /// byte or word, two for a long.
    long_accesses: u8,
}

impl Row {
    /// A row with no "X" access.
    const fn new(head: i8, tail: i8, cycles: u16) -> Self {
        Self {
            head,
            tail,
            cycles,
            long_accesses: 0,
        }
    }

    /// The row with `accesses` of its bus cycles marked "X".
    const fn with_x(self, accesses: u8) -> Self {
        Self {
            long_accesses: accesses,
            ..self
        }
    }

    /// The row for an operand of `size`: for a long, two clocks more
    /// to the tail, and two more cycles for each "X" (CPU32RM 8.3.1;
    /// 8.3: "for long bus cycles, usually add a value of two to the
    /// tail").  A row with several "X" adds to its tail once.
    const fn sized(self, size: Size) -> Self {
        match (size, self.long_accesses) {
            (Size::Byte | Size::Word, _) | (Size::Long, 0) => self,
            (Size::Long, accesses) => Self {
                head: self.head,
                tail: self.tail + LONG_CLOCKS,
                cycles: self.cycles + (LONG_CLOCKS as u16) * accesses as u16,
                long_accesses: 0,
            },
        }
    }

    /// The row alone as a cost.
    const fn cost(self) -> Cost {
        Sequence::EMPTY.then(self).cost()
    }

    /// The clocks `self` takes beyond `base`, for a charge.
    const fn beyond(self, base: Self) -> Cycles {
        Cycles(self.cycles.saturating_sub(base.cycles) as u64)
    }

    /// The clocks `self` takes more or fewer than `base`.
    const fn against(self, base: Self) -> Adjustment {
        Adjustment::of(self.cycles as i32 - base.cycles as i32)
    }
}

/// What a handler changes its opcode's table cost by, for what only
/// its operands decide, where that can be less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Adjustment {
    /// Clocks more, a charge.
    More(Cycles),
    /// Clocks fewer, a refund.
    Fewer(Cycles),
}

impl Adjustment {
    /// `clocks` more, or fewer if negative.
    const fn of(clocks: i32) -> Self {
        if clocks >= 0 {
            Self::More(Cycles(clocks.unsigned_abs() as u64))
        } else {
            Self::Fewer(Cycles(clocks.unsigned_abs() as u64))
        }
    }
}

/// An instruction's parts, combined in order as if each were an
/// instruction (CPU32RM 8.1.6): a save step, its effective addresses,
/// its operation.  The head is the first part's, the tail the last's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sequence {
    /// The first part's head.
    head: i8,
    /// The last part's tail.
    tail: i8,
    /// The parts' cycles less their overlaps.
    cycles: i32,
    /// Whether it has no part yet.
    empty: bool,
}

impl Sequence {
    /// No part.
    const EMPTY: Self = Self {
        head: 0,
        tail: 0,
        cycles: 0,
        empty: true,
    };

    /// `row` after the parts so far, overlapping min(tail, head)
    /// (CPU32RM 8.1.6); a negative tail or head overlaps nothing
    /// (8.1.7).
    const fn then(self, row: Row) -> Self {
        if self.empty {
            return Self {
                head: row.head,
                tail: row.tail,
                cycles: row.cycles as i32,
                empty: false,
            };
        }
        let tail = if self.tail > 0 { self.tail } else { 0 };
        let head = if row.head > 0 { row.head } else { 0 };
        let overlap = if tail < head { tail } else { head };
        Self {
            head: self.head,
            tail: row.tail,
            cycles: self.cycles - overlap as i32 + row.cycles as i32,
            empty: false,
        }
    }

    /// `row`, if there is one: an effective address in a register
    /// takes no part ("-" in CPU32RM 8.3.1, 8.3.2).
    const fn then_some(self, row: Option<Row>) -> Self {
        match row {
            Some(row) => self.then(row),
            None => self,
        }
    }

    /// The cost, negative head and tail counted as zero (CPU32RM 8.1.7:
    /// on a two-clock bus a zero is used instead of a negative number).
    const fn cost(self) -> Cost {
        Cost {
            head: if self.head > 0 {
                self.head.cast_unsigned()
            } else {
                0
            },
            tail: if self.tail > 0 {
                self.tail.cast_unsigned()
            } else {
                0
            },
            cycles: (self.cycles as i16).cast_unsigned(),
        }
    }
}

/// Fields of second opcode words the costs depend on.
mod extension {
    /// MUL.L and DIV.L, bit 11: signed (CPU32RM 4.5, MULS, DIVS long
    /// forms).
    pub(super) const SIGNED: u16 = 1 << 11;
    /// MUL.L bit 10: a quad word product (CPU32RM 4.5, MULU long).
    pub(super) const QUAD: u16 = 1 << 10;
    /// MOVES bit 11, dr: register to memory (CPU32RM 4.5, MOVES).
    pub(super) const REGISTER_TO_MEMORY: u16 = 1 << 11;
    /// TBL bit 10, R: unrounded, the "N" forms (CPU32RM 4.5, TBLS).
    pub(super) const UNROUNDED: u16 = 1 << 10;
    /// Where TBL's size field, bits 7:6, starts (CPU32RM 4.5, TBLS).
    pub(super) const TABLE_SIZE_SHIFT: u16 = 6;
    /// Full format BS: base suppressed (CPU32RM 3.4.4, Figure 3-2).
    pub(super) const BASE_SUPPRESS: u16 = 1 << 7;
    /// Full format IS: index suppressed.
    pub(super) const INDEX_SUPPRESS: u16 = 1 << 6;
    /// Where full format BD SIZE starts.
    pub(super) const BASE_DISPLACEMENT_SHIFT: u16 = 4;
    /// BD SIZE 01: null.
    pub(super) const NULL_DISPLACEMENT: u16 = 0b01;
    /// BD SIZE 10: a word.
    pub(super) const WORD_DISPLACEMENT: u16 = 0b10;
}

/// A conditional branch taken, beyond the not-taken cost the table
/// charges (CPU32RM 8.3.11), for a displacement of `size`: byte, word
/// or long.  For Bcc; BRA is always taken and its table entry says so.
/// The head stays the not-taken one's, as it does for every charge
/// here: a charge adds cycles only.
pub(crate) const fn branch_taken(size: Size) -> Cycles {
    rows::branch::TAKEN.beyond(decode::branch_not_taken(size))
}

/// How a DBcc ends (CPU32RM 8.3.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decrement {
    /// The condition is true: no decrement, no branch.
    ConditionTrue,
    /// The condition false and the count expired at -1: no branch.
    Expired,
    /// The condition false and the count not -1: the branch taken.
    Taken,
}

/// DBcc beyond the condition-true cost the table charges (CPU32RM
/// 8.3.11), out of loop mode.
pub(crate) const fn decrement_and_branch(outcome: Decrement) -> Cycles {
    let row = match outcome {
        Decrement::ConditionTrue => rows::branch::DBCC_TRUE,
        Decrement::Expired => rows::branch::DBCC_EXPIRED,
        Decrement::Taken => rows::branch::DBCC_TAKEN,
    };
    row.beyond(rows::branch::DBCC_TRUE)
}

/// DBcc in loop mode, its starred rows (CPU32RM 8.3.11; 1.1.2), against
/// the condition-true cost the table charges, after an instruction
/// whose tail was `last_tail`: the row overlaps that tail by its own
/// head.  Its tail is zero, as the run loop takes it.
pub(crate) fn decrement_and_branch_looping(outcome: Decrement, last_tail: u8) -> Adjustment {
    let row = match outcome {
        Decrement::ConditionTrue => rows::branch::DBCC_LOOP_TRUE,
        Decrement::Expired => rows::branch::DBCC_LOOP_EXPIRED,
        Decrement::Taken => rows::branch::DBCC_LOOP_TAKEN,
    };
    in_place_of(row, rows::branch::DBCC_TRUE, last_tail)
}

/// A shift or rotate of a data register by a count in a register,
/// beyond the count-zero cost the table charges (CPU32RM 8.3.9, notes
/// 1 and 2).  `opcode` is the instruction's, `count` the count the
/// instruction uses (Dn modulo 64).
pub(crate) const fn shift_count(opcode: u16, count: u32) -> Cycles {
    let clocks = if decode::is_rotate_extend(opcode) {
        rows::shift::rotate_extend_register_clocks(count)
    } else {
        rows::shift::register_clocks(count)
    };
    Cycles((clocks - rows::shift::MINIMUM_CLOCKS) as u64)
}

/// MOVEM beyond its no-register cost: four clocks a register in each
/// of its rows (CPU32RM 8.3.4, "n * 4").
pub(crate) const fn movem(registers: u32) -> Cycles {
    Cycles(registers as u64 * rows::special::MOVEM_CLOCKS_PER_REGISTER)
}

/// MUL.L beyond the quad-product cost the table charges, by its
/// second word: a long product's maximum is six clocks more (CPU32RM
/// 8.3.5).  Signed and unsigned take the same time.
pub(crate) const fn multiply_long(extension: u16) -> Cycles {
    if extension & extension::QUAD != 0 {
        Cycles::ZERO
    } else {
        rows::arithmetic::MULTIPLY_LONG.beyond(rows::arithmetic::MULTIPLY_QUAD)
    }
}

/// DIV.L beyond the unsigned cost the table charges, by its second
/// word: DIVS.L's maximum is 16 clocks more (CPU32RM 8.3.5).
pub(crate) const fn divide_long(extension: u16) -> Cycles {
    if extension & extension::SIGNED != 0 {
        rows::arithmetic::DIVIDE_SIGNED_LONG.beyond(rows::arithmetic::DIVIDE_UNSIGNED_LONG)
    } else {
        Cycles::ZERO
    }
}

/// MOVES beyond the memory-to-register cost the table charges, by its
/// second word (CPU32RM 8.3.4, MOVES (Op)).
pub(crate) const fn moves(extension: u16) -> Cycles {
    if extension & extension::REGISTER_TO_MEMORY != 0 {
        rows::special::MOVES_TO_MEMORY.beyond(rows::special::MOVES_TO_REGISTER)
    } else {
        Cycles::ZERO
    }
}

/// TBLS, TBLU, TBLSN and TBLUN beyond the rounded byte or word cost
/// the table charges, by the opcode and its second word: the
/// unrounded forms, and long table entries, cost more (CPU32RM 8.3.5).
pub(crate) const fn table_lookup(opcode: u16, extension: u16) -> Cycles {
    let register = decode::is_table_register_form(opcode);
    let signed = extension & extension::SIGNED != 0;
    let row = match (extension & extension::UNROUNDED != 0, signed, register) {
        (false, _, true) => rows::arithmetic::TABLE_REGISTERS,
        (false, _, false) => rows::arithmetic::TABLE,
        (true, true, true) => rows::arithmetic::TABLE_SIGNED_UNROUNDED_REGISTERS,
        (true, true, false) => rows::arithmetic::TABLE_SIGNED_UNROUNDED,
        (true, false, true) => rows::arithmetic::TABLE_UNSIGNED_UNROUNDED_REGISTERS,
        (true, false, false) => rows::arithmetic::TABLE_UNSIGNED_UNROUNDED,
    };
    let base = if register {
        rows::arithmetic::TABLE_REGISTERS
    } else {
        rows::arithmetic::TABLE
    };
    let row = match Size::from_field(extension >> extension::TABLE_SIZE_SHIFT) {
        Some(size) if !register => row.sized(size),
        _ => row,
    };
    row.beyond(base)
}

/// CHK, CHK2, TRAPcc or TRAPV trapping, beyond the no-trap cost the
/// table charges for `opcode` (CPU32RM 8.3.12, 8.3.13: the "(ex)" and
/// "(trap)" rows); zero for any other opcode.  The exception's
/// stacking is in these rows: exception processing charges nothing
/// more for it.
pub(crate) const fn trap_taken(opcode: u16) -> Cycles {
    match decode::trap(opcode) {
        Some((trap, no_trap)) => trap.beyond(no_trap),
        None => Cycles::ZERO,
    }
}

/// RTE beyond the four-word frame cost the table charges, by the
/// frame's format, bits 15:12 of its format word (CPU32RM 8.3.14,
/// 6.4).  Format $2 is the six-word frame, $C the bus error frame
/// ("BERR on instruction").  Any other format takes a format error,
/// which exception processing charges.
pub(crate) const fn return_from_exception(format: u8) -> Cycles {
    let row = match format {
        exception::SIX_WORD_FORMAT => rows::save::RTE_SIX_WORD,
        exception::BUS_ERROR_FORMAT => rows::save::RTE_BUS_ERROR,
        _ => rows::save::RTE_FOUR_WORD,
    };
    row.beyond(rows::save::RTE_FOUR_WORD)
}

/// The effective address table an instruction's address is charged
/// by (CPU32RM 8.3.1, 8.3.2).  The handler names it, for what only its
/// extension words tell: a full format extension word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressTable {
    /// Fetch effective address (8.3.1): the address and the operand's
    /// read.
    Fetch,
    /// Calculate effective address (8.3.2): the address alone, for
    /// LEA, PEA, JMP, JSR, MOVEM, MOVES, CLR, Scc, TAS, MOVE from SR
    /// and CCR, TBL's table form and MOVE's <CEA> (8.3.3 to 8.3.12).
    Calculate,
}

/// A full format extension word against the brief format the table
/// charges for the indexed modes, (d8, An, Xn) and (d8, PC, Xn), by
/// `table` (CPU32RM 8.3.1, 8.3.2): a 32-bit base displacement one
/// clock more, an index alone the same, the other forms fewer.  The
/// cycles alone: the head and tail stay the brief form's.
pub(crate) const fn full_extension(extension: u16, table: AddressTable) -> Adjustment {
    match table {
        AddressTable::Fetch => rows::FETCH_FULL.row(extension).against(rows::FETCH.index),
        AddressTable::Calculate => rows::CALCULATE_FULL
            .row(extension)
            .against(rows::CALCULATE.index),
    }
}

/// The tables MOVE's source and destination addresses are charged by,
/// as the first MOVE row matching both operands takes them (CPU32RM
/// 8.3.3): the source calculated when the destination is a <CEA> (MOVE
/// <CEA>, <FEA> and its note), else fetched; the destination
/// calculated after a register or immediate source (MOVE Rn, <CEA>,
/// MOVE #, <CEA>), else fetched.
/// `source` and `destination` are the fields' kinds
/// (`effective_address::kind`).
pub(crate) const fn move_tables(source: u8, destination: u8) -> (AddressTable, AddressTable) {
    let source_table = match destination {
        kind::DISPLACEMENT | kind::INDEX | kind::ABSOLUTE_SHORT | kind::ABSOLUTE_LONG => {
            AddressTable::Calculate
        }
        _ => AddressTable::Fetch,
    };
    let destination_table = match source {
        kind::DATA_REGISTER | kind::ADDRESS_REGISTER | kind::IMMEDIATE => AddressTable::Calculate,
        _ => AddressTable::Fetch,
    };
    (source_table, destination_table)
}

/// LPSTOP against the TBL cost the table charges its first word,
/// $F800 (CPU32RM 8.3.5, TBL Dym:Dyn), after an instruction whose tail
/// was `last_tail`: LPSTOP's own row (8.3.13) overlaps that tail by its
/// head, not TBL's.  Both tails count as zero (8.1.7), so the next
/// instruction overlaps neither.
pub(crate) fn lpstop(last_tail: u8) -> Adjustment {
    in_place_of(
        rows::exception::LPSTOP,
        rows::arithmetic::TABLE_REGISTERS,
        last_tail,
    )
}

/// `row` against `base`, the row the table charges, each overlapping
/// `last_tail` by its own head (CPU32RM 8.1.6).
fn in_place_of(row: Row, base: Row, last_tail: u8) -> Adjustment {
    let after = |row: Row| {
        Sequence::EMPTY
            .then(Row::new(0, last_tail.cast_signed(), 0))
            .then(row)
            .cycles
    };
    Adjustment::of(after(row) - after(base))
}

/// Whether the timing decoder takes `opcode` for an instruction, not
/// an illegal, line A or line F opcode, for the test against the
/// handler table.
#[cfg(test)]
pub(crate) fn is_instruction(opcode: u16) -> bool {
    decode::is_instruction(opcode)
}

#[cfg(test)]
mod tests;
