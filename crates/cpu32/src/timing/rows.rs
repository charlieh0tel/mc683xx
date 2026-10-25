//! The timing tables (CPU32RM 8.3), transcribed: head, tail and
//! cycles as printed.  Where the cycles column gives a range or a "<"
//! maximum, the row holds the maximum.

use crate::timing::Row;
use crate::timing::extension;

/// The effective address rows of one table, brief formats (CPU32RM
/// 8.3.1, 8.3.2).  The PC-relative modes share the An rows (note 3).
#[derive(Debug, Clone, Copy)]
pub(super) struct Modes {
    /// (An).
    pub(super) indirect: Row,
    /// (An)+.
    pub(super) postincrement: Row,
    /// -(An).
    pub(super) predecrement: Row,
    /// (d16, An) and (d16, PC).
    pub(super) displacement: Row,
    /// (xxx).W.
    pub(super) absolute_short: Row,
    /// (xxx).L.
    pub(super) absolute_long: Row,
    /// (d8, An, Xn.Sz*Sc) and (d8, PC, Xn.Sz*Sc).
    pub(super) index: Row,
}

/// The full format extension word rows of one table (CPU32RM 8.3.1,
/// 8.3.2), named for what the word leaves unsuppressed.  Size and scale
/// of the index do not matter (note 2).
#[derive(Debug, Clone, Copy)]
pub(super) struct FullFormat {
    /// (0): all suppressed.
    suppressed: Row,
    /// (d16).
    displacement_16: Row,
    /// (d32).
    displacement_32: Row,
    /// (An).
    base: Row,
    /// (Xm.Sz*Sc).
    index: Row,
    /// (An, Xm.Sz*Sc).
    base_index: Row,
    /// (d16, An).
    base_displacement_16: Row,
    /// (d32, An).
    base_displacement_32: Row,
    /// (d16, An, Xm) and (d16, An, Xm.Sz*Sc).
    base_displacement_16_index: Row,
    /// (d32, An, Xm) and (d32, An, Xm.Sz*Sc).
    base_displacement_32_index: Row,
}

impl FullFormat {
    /// The row for full format extension word `extension`.  The tables
    /// list no (d16, Xm) or (d32, Xm), base suppressed and indexed with
    /// a displacement; they take the (d16, An, Xm) and (d32, An, Xm)
    /// rows.  BD SIZE 00 is reserved: the long displacement row.
    pub(super) const fn row(&self, extension: u16) -> Row {
        let base = extension & extension::BASE_SUPPRESS == 0;
        let index = extension & extension::INDEX_SUPPRESS == 0;
        let displacement = extension >> extension::BASE_DISPLACEMENT_SHIFT & 0b11;
        match (base, index, displacement) {
            (false, false, extension::NULL_DISPLACEMENT) => self.suppressed,
            (false, false, extension::WORD_DISPLACEMENT) => self.displacement_16,
            (false, false, _) => self.displacement_32,
            (true, false, extension::NULL_DISPLACEMENT) => self.base,
            (true, false, extension::WORD_DISPLACEMENT) => self.base_displacement_16,
            (true, false, _) => self.base_displacement_32,
            (false, true, extension::NULL_DISPLACEMENT) => self.index,
            (true, true, extension::NULL_DISPLACEMENT) => self.base_index,
            (_, true, extension::WORD_DISPLACEMENT) => self.base_displacement_16_index,
            (_, true, _) => self.base_displacement_32_index,
        }
    }
}

/// Fetch effective address (CPU32RM 8.3.1): calculate the address and
/// read the operand, "X" for its read.
pub(super) const FETCH: Modes = Modes {
    indirect: Row::new(1, 1, 3).with_x(1),
    postincrement: Row::new(1, 1, 3).with_x(1),
    predecrement: Row::new(2, 2, 4).with_x(1),
    displacement: Row::new(1, 3, 5).with_x(1),
    absolute_short: Row::new(1, 3, 5).with_x(1),
    absolute_long: Row::new(1, 5, 7).with_x(1),
    index: Row::new(4, 2, 8).with_x(1),
};

/// Fetch effective address, full format (CPU32RM 8.3.1).
pub(super) const FETCH_FULL: FullFormat = FullFormat {
    suppressed: Row::new(2, 2, 6).with_x(1),
    displacement_16: Row::new(1, 3, 7).with_x(1),
    displacement_32: Row::new(1, 5, 9).with_x(1),
    base: Row::new(1, 1, 5).with_x(1),
    index: Row::new(4, 2, 8).with_x(1),
    base_index: Row::new(4, 2, 8).with_x(1),
    base_displacement_16: Row::new(1, 3, 7).with_x(1),
    base_displacement_32: Row::new(1, 5, 9).with_x(1),
    base_displacement_16_index: Row::new(2, 2, 8).with_x(1),
    base_displacement_32_index: Row::new(1, 3, 9).with_x(1),
};

/// Fetch effective address, #<data>.B and #<data>.W (CPU32RM 8.3.1).
pub(super) const FETCH_IMMEDIATE: Row = Row::new(1, 1, 3);

/// Fetch effective address, #<data>.L (CPU32RM 8.3.1).
pub(super) const FETCH_IMMEDIATE_LONG: Row = Row::new(1, 3, 5);

/// Calculate effective address (CPU32RM 8.3.2): the address alone.
pub(super) const CALCULATE: Modes = Modes {
    indirect: Row::new(1, 0, 2),
    postincrement: Row::new(1, 0, 2),
    predecrement: Row::new(2, 0, 2),
    displacement: Row::new(1, 1, 3),
    absolute_short: Row::new(1, 1, 3),
    absolute_long: Row::new(1, 3, 5),
    index: Row::new(4, 0, 6),
};

/// Calculate effective address, full format (CPU32RM 8.3.2).
pub(super) const CALCULATE_FULL: FullFormat = FullFormat {
    suppressed: Row::new(2, 0, 4),
    displacement_16: Row::new(1, 1, 5),
    displacement_32: Row::new(1, 3, 7),
    base: Row::new(1, 0, 4),
    index: Row::new(4, 0, 6),
    base_index: Row::new(4, 0, 6),
    base_displacement_16: Row::new(1, 1, 5),
    base_displacement_32: Row::new(1, 3, 7),
    base_displacement_16_index: Row::new(2, 0, 6),
    base_displacement_32_index: Row::new(1, 1, 7),
};

/// MOVE and MOVEA (CPU32RM 8.3.3), "x" for the write.  The decoder
/// takes the first row matching both operands, as the table says.
pub(super) mod move_ {
    use crate::timing::Row;

    /// MOVE Rn, Rn.
    pub(in crate::timing) const REGISTER_TO_REGISTER: Row = Row::new(0, 0, 2);
    /// MOVE <FEA>, Rn.
    pub(in crate::timing) const FETCH_TO_REGISTER: Row = Row::new(0, 0, 2);
    /// MOVE Rn, (Am).
    pub(in crate::timing) const REGISTER_TO_INDIRECT: Row = Row::new(0, 2, 4).with_x(1);
    /// MOVE Rn, (Am)+.
    pub(in crate::timing) const REGISTER_TO_POSTINCREMENT: Row = Row::new(1, 1, 5).with_x(1);
    /// MOVE Rn, -(Am).
    pub(in crate::timing) const REGISTER_TO_PREDECREMENT: Row = Row::new(2, 2, 6).with_x(1);
    /// MOVE Rn, <CEA>.
    pub(in crate::timing) const REGISTER_TO_CALCULATE: Row = Row::new(1, 3, 5).with_x(1);
    /// MOVE <FEA>, (An), (An)+ and -(An).
    pub(in crate::timing) const FETCH_TO_INCREMENT: Row = Row::new(2, 2, 6).with_x(1);
    /// MOVE #, <CEA>, after the # fetch effective address.
    pub(in crate::timing) const IMMEDIATE_TO_CALCULATE: Row = Row::new(2, 2, 6).with_x(1);
    /// MOVE <CEA>, <FEA>: the source by the calculate table, the
    /// destination by the fetch table.
    pub(in crate::timing) const CALCULATE_TO_FETCH: Row = Row::new(2, 2, 6).with_x(1);
}

/// Special-purpose MOVE (CPU32RM 8.3.4).
pub(super) mod special {
    use crate::timing::Row;

    /// EXG Rn, Rm.
    pub(in crate::timing) const EXG: Row = Row::new(2, 0, 4);
    /// MOVEC Cr, Rn.
    pub(in crate::timing) const MOVEC_FROM_CONTROL: Row = Row::new(10, 0, 14);
    /// MOVEC Rn, Cr: 14-16, its maximum.
    pub(in crate::timing) const MOVEC_TO_CONTROL: Row = Row::new(12, 0, 16);
    /// MOVE CCR, Dn.
    pub(in crate::timing) const CCR_TO_DATA: Row = Row::new(2, 0, 4);
    /// MOVE CCR, <CEA>.
    pub(in crate::timing) const CCR_TO_CALCULATE: Row = Row::new(0, 2, 4);
    /// MOVE Dn, CCR.
    pub(in crate::timing) const DATA_TO_CCR: Row = Row::new(2, 0, 4);
    /// MOVE <FEA>, CCR.
    pub(in crate::timing) const FETCH_TO_CCR: Row = Row::new(0, 0, 4);
    /// MOVE SR, Dn.
    pub(in crate::timing) const SR_TO_DATA: Row = Row::new(2, 0, 4);
    /// MOVE SR, <CEA>.
    pub(in crate::timing) const SR_TO_CALCULATE: Row = Row::new(0, 2, 4);
    /// MOVE Dn, SR.
    pub(in crate::timing) const DATA_TO_SR: Row = Row::new(4, -2, 10);
    /// MOVE <FEA>, SR.
    pub(in crate::timing) const FETCH_TO_SR: Row = Row::new(0, -2, 10);
    /// MOVEM.W <CEA>, RL, no register: 8 + n * 4.
    pub(in crate::timing) const MOVEM_WORD_TO_REGISTERS: Row = Row::new(1, 0, 8);
    /// MOVEM.W RL, <CEA>, no register: 8 + n * 4.
    pub(in crate::timing) const MOVEM_WORD_TO_MEMORY: Row = Row::new(1, 0, 8);
    /// MOVEM.L <CEA>, RL, no register: 12 + n * 4.
    pub(in crate::timing) const MOVEM_LONG_TO_REGISTERS: Row = Row::new(1, 0, 12);
    /// MOVEM.L RL, <CEA>, no register: 10 + n * 4.
    pub(in crate::timing) const MOVEM_LONG_TO_MEMORY: Row = Row::new(1, 2, 10);
    /// MOVEM's n * 4: clocks a register.
    pub(in crate::timing) const MOVEM_CLOCKS_PER_REGISTER: u64 = 4;
    /// MOVEP.W Dn, (d16, An).
    pub(in crate::timing) const MOVEP_WORD_TO_MEMORY: Row = Row::new(2, 0, 10);
    /// MOVEP.W (d16, An), Dn.
    pub(in crate::timing) const MOVEP_WORD_TO_REGISTER: Row = Row::new(1, 2, 11);
    /// MOVEP.L Dn, (d16, An).
    pub(in crate::timing) const MOVEP_LONG_TO_MEMORY: Row = Row::new(2, 0, 14);
    /// MOVEP.L (d16, An), Dn.
    pub(in crate::timing) const MOVEP_LONG_TO_REGISTER: Row = Row::new(1, 2, 19);
    /// MOVES (Save), either direction.
    pub(in crate::timing) const MOVES_SAVE: Row = Row::new(1, 1, 3);
    /// MOVES (Op) <CEA>, Rn.
    pub(in crate::timing) const MOVES_TO_REGISTER: Row = Row::new(7, 1, 11).with_x(1);
    /// MOVES (Op) Rn, <CEA>.
    pub(in crate::timing) const MOVES_TO_MEMORY: Row = Row::new(9, 2, 12).with_x(1);
    /// MOVE USP, An and MOVE An, USP.
    pub(in crate::timing) const MOVE_USP: Row = Row::new(0, 0, 2);
    /// SWAP Dn.
    pub(in crate::timing) const SWAP: Row = Row::new(4, 0, 6);
}

/// Arithmetic and logic (CPU32RM 8.3.5).
pub(super) mod arithmetic {
    use crate::timing::Row;

    /// ADD(A), AND, OR, SUB(A), CMP(A) Rn, Rm and <FEA>, Rn; EOR Dn, Dm.
    pub(in crate::timing) const TO_REGISTER: Row = Row::new(0, 0, 2);
    /// ADD, AND, EOR, OR, SUB Dn, <FEA>.
    pub(in crate::timing) const TO_MEMORY: Row = Row::new(0, 3, 5).with_x(1);
    /// The (Save) rows of CMP2, MUL.L, DIV.L and TBL <CEA>, which CHK2
    /// (CPU32RM 8.3.12) and the F-line second word (8.3.13) share.
    pub(in crate::timing) const SAVE: Row = Row::new(1, 1, 3);
    /// CMP2 (Op): 16-18, its maximum, which is CHK2 (Op)'s (no ex,
    /// CPU32RM 8.3.12).
    pub(in crate::timing) const COMPARE_BOUNDS: Row = Row::new(2, 0, 18).with_x(1);
    /// MUL(S/U).W.
    pub(in crate::timing) const MULTIPLY_WORD: Row = Row::new(0, 0, 26);
    /// MUL(S/U).L (Op) <FEA>, Dl: 46-52, its maximum.
    pub(in crate::timing) const MULTIPLY_LONG: Row = Row::new(2, 0, 52);
    /// MUL(S/U).L (Op) <FEA>, Dn:Dl.
    pub(in crate::timing) const MULTIPLY_QUAD: Row = Row::new(2, 0, 46);
    /// DIVU.W.
    pub(in crate::timing) const DIVIDE_UNSIGNED_WORD: Row = Row::new(0, 0, 32);
    /// DIVS.W.
    pub(in crate::timing) const DIVIDE_SIGNED_WORD: Row = Row::new(0, 0, 42);
    /// DIVU.L (Op): <46.
    pub(in crate::timing) const DIVIDE_UNSIGNED_LONG: Row = Row::new(2, 0, 46);
    /// DIVS.L (Op): <62.
    pub(in crate::timing) const DIVIDE_SIGNED_LONG: Row = Row::new(2, 0, 62);
    /// TBL(S/U) Dn:Dm, Dp: 28-30.
    pub(in crate::timing) const TABLE_REGISTERS: Row = Row::new(26, 0, 30);
    /// TBL(S/U) (Op) <CEA>, Dn: 33-35, two "X" reads.
    pub(in crate::timing) const TABLE: Row = Row::new(6, 0, 35).with_x(2);
    /// TBLSN Dn:Dm, Dp: 30-34.
    pub(in crate::timing) const TABLE_SIGNED_UNROUNDED_REGISTERS: Row = Row::new(30, 0, 34);
    /// TBLSN (Op) <CEA>, Dn: 35-39.
    pub(in crate::timing) const TABLE_SIGNED_UNROUNDED: Row = Row::new(6, 0, 39).with_x(2);
    /// TBLUN Dn:Dm, Dp: 34-40.
    pub(in crate::timing) const TABLE_UNSIGNED_UNROUNDED_REGISTERS: Row = Row::new(30, 0, 40);
    /// TBLUN (Op) <CEA>, Dn: 39-45.
    pub(in crate::timing) const TABLE_UNSIGNED_UNROUNDED: Row = Row::new(6, 0, 45).with_x(2);
}

/// Immediate arithmetic and logic (CPU32RM 8.3.6).  ADDI, ANDI, CMPI,
/// EORI, ORI and SUBI add the # fetch effective address first.
pub(super) mod immediate {
    use crate::timing::Row;

    /// MOVEQ #, Dn.
    pub(in crate::timing) const MOVEQ: Row = Row::new(0, 0, 2);
    /// ADDQ and SUBQ #, Rn.
    pub(in crate::timing) const QUICK_TO_REGISTER: Row = Row::new(0, 0, 2);
    /// ADDQ and SUBQ #, <FEA>.
    pub(in crate::timing) const QUICK_TO_MEMORY: Row = Row::new(0, 3, 5).with_x(1);
    /// ADDI, ANDI, CMPI, EORI, ORI and SUBI #, Rn.
    pub(in crate::timing) const TO_REGISTER: Row = Row::new(0, 0, 2);
    /// ADDI, ANDI, CMPI, EORI, ORI and SUBI #, <FEA>.
    pub(in crate::timing) const TO_MEMORY: Row = Row::new(0, 3, 5).with_x(1);
}

/// Binary-coded decimal and extended (CPU32RM 8.3.7).  The table marks
/// no "X"; ADDX, SUBX and CMPM take a long operand, so each of their
/// operand accesses is charged as an "X" (8.1.5: a long access adds
/// its second bus cycle).
pub(super) mod extended {
    use crate::timing::Row;

    /// ABCD and SBCD Dn, Dm.
    pub(in crate::timing) const DECIMAL_REGISTER: Row = Row::new(2, 0, 4);
    /// ABCD and SBCD -(An), -(Am).
    pub(in crate::timing) const DECIMAL_MEMORY: Row = Row::new(2, 2, 12);
    /// ADDX and SUBX Dn, Dm.
    pub(in crate::timing) const REGISTER: Row = Row::new(0, 0, 2);
    /// ADDX and SUBX -(An), -(Am): two reads and a write.
    pub(in crate::timing) const MEMORY: Row = Row::new(2, 2, 10).with_x(3);
    /// CMPM (An)+, (Am)+: two reads.
    pub(in crate::timing) const CMPM: Row = Row::new(1, 0, 8).with_x(2);
}

/// Single operand (CPU32RM 8.3.8).
pub(super) mod single {
    use crate::timing::Row;

    /// CLR Dn.
    pub(in crate::timing) const CLR_DATA: Row = Row::new(0, 0, 2);
    /// CLR <CEA>.
    pub(in crate::timing) const CLR_CALCULATE: Row = Row::new(0, 2, 4).with_x(1);
    /// NEG, NEGX and NOT Dn.
    pub(in crate::timing) const NEGATE_DATA: Row = Row::new(0, 0, 2);
    /// NEG, NEGX and NOT <FEA>.
    pub(in crate::timing) const NEGATE_MEMORY: Row = Row::new(0, 3, 5).with_x(1);
    /// EXT Dn, and EXTB.
    pub(in crate::timing) const EXT: Row = Row::new(0, 0, 2);
    /// NBCD Dn.
    pub(in crate::timing) const NBCD_DATA: Row = Row::new(2, 0, 4);
    /// NBCD <FEA>.
    pub(in crate::timing) const NBCD_MEMORY: Row = Row::new(0, 2, 6);
    /// Scc Dn.
    pub(in crate::timing) const SCC_DATA: Row = Row::new(2, 0, 4);
    /// Scc <CEA>.
    pub(in crate::timing) const SCC_CALCULATE: Row = Row::new(2, 2, 6);
    /// TAS Dn.
    pub(in crate::timing) const TAS_DATA: Row = Row::new(4, 0, 6);
    /// TAS <CEA>.
    pub(in crate::timing) const TAS_CALCULATE: Row = Row::new(1, 0, 10);
    /// TST <FEA>.
    pub(in crate::timing) const TST: Row = Row::new(0, 0, 2);
}

/// Shift and rotate (CPU32RM 8.3.9).  Rows whose head the table prints
/// as -2 compute head and cycles by its notes; the head is read as the
/// cycles less two, as the #, Dm rows' 4 and 6 are.
pub(super) mod shift {
    use crate::timing::Row;

    /// The fewest clocks any of the computed rows takes: the notes'
    /// max (..., 6).
    pub(in crate::timing) const MINIMUM_CLOCKS: u16 = 6;
    /// What the computed rows' head is short of their cycles: the
    /// head column's -2.
    const HEAD_SHORT: u16 = 2;
    /// The register count shift count's bits: modulo 64 (CPU32RM 4.4,
    /// LSL, LSR).
    const COUNT_MASK: u32 = 63;

    /// `LSd`, `ASd`, `ROd` #, Dm.
    pub(in crate::timing) const IMMEDIATE: Row = Row::new(4, 0, 6);
    /// `LSd`, `ASd`, `ROd`, `ROXd` <FEA>.
    pub(in crate::timing) const MEMORY: Row = Row::new(0, 2, 6);

    /// A computed row: the head two clocks short of `clocks`.
    pub(in crate::timing) const fn computed(clocks: u16) -> Row {
        Row::new((clocks - HEAD_SHORT) as i8, 0, clocks)
    }

    /// `LSd`, `ASd`, `ROd` Dn, Dm for count `count` (note 1):
    /// max(3 + n/4 + mod(n, 4) + mod(n/4 + mod(n, 4) + 1, 2), 6).
    pub(in crate::timing) const fn register_clocks(count: u32) -> u16 {
        let count = count & COUNT_MASK;
        let steps = count / 4 + count % 4;
        let clocks = (3 + steps + (steps + 1) % 2) as u16;
        if clocks > MINIMUM_CLOCKS {
            clocks
        } else {
            MINIMUM_CLOCKS
        }
    }

    /// `ROXd` Dn, Dm for count `count` (note 2): max(3 + n + mod(n + 1,
    /// 2), 6).
    pub(in crate::timing) const fn rotate_extend_register_clocks(count: u32) -> u16 {
        let count = count & COUNT_MASK;
        let clocks = (3 + count + (count + 1) % 2) as u16;
        if clocks > MINIMUM_CLOCKS {
            clocks
        } else {
            MINIMUM_CLOCKS
        }
    }

    /// `ROXd` #, Dm for count `count`, 1 to 8 (note 3): max(2 + n +
    /// mod(n, 2), 6).
    pub(in crate::timing) const fn rotate_extend_immediate_clocks(count: u16) -> u16 {
        let clocks = 2 + count + count % 2;
        if clocks > MINIMUM_CLOCKS {
            clocks
        } else {
            MINIMUM_CLOCKS
        }
    }
}

/// Bit manipulation (CPU32RM 8.3.10).  The # forms add the # fetch
/// effective address first.
pub(super) mod bit {
    use crate::timing::Row;

    /// BCHG, BCLR, BSET #, Dn.
    pub(in crate::timing) const CHANGE_IMMEDIATE_REGISTER: Row = Row::new(2, 0, 6);
    /// BCHG, BCLR, BSET Dn, Dm.
    pub(in crate::timing) const CHANGE_REGISTER: Row = Row::new(4, 0, 6);
    /// BCHG, BCLR, BSET #, <FEA>.
    pub(in crate::timing) const CHANGE_IMMEDIATE_MEMORY: Row = Row::new(1, 2, 8);
    /// BCHG, BCLR, BSET Dn, <FEA>.
    pub(in crate::timing) const CHANGE_MEMORY: Row = Row::new(2, 2, 8);
    /// BTST #, Dn.
    pub(in crate::timing) const TEST_IMMEDIATE_REGISTER: Row = Row::new(2, 0, 4);
    /// BTST Dn, Dm.
    pub(in crate::timing) const TEST_REGISTER: Row = Row::new(2, 0, 4);
    /// BTST #, <FEA>.
    pub(in crate::timing) const TEST_IMMEDIATE_MEMORY: Row = Row::new(1, 0, 4);
    /// BTST Dn, <FEA>.
    pub(in crate::timing) const TEST_MEMORY: Row = Row::new(2, 0, 8);
}

/// Conditional branch (CPU32RM 8.3.11).
pub(super) mod branch {
    use crate::timing::Row;

    /// Bcc taken, any displacement.
    pub(in crate::timing) const TAKEN: Row = Row::new(2, -2, 8);
    /// Bcc.B not taken.
    pub(in crate::timing) const NOT_TAKEN_BYTE: Row = Row::new(2, 0, 4);
    /// Bcc.W not taken.
    pub(in crate::timing) const NOT_TAKEN_WORD: Row = Row::new(0, 0, 4);
    /// Bcc.L not taken.
    pub(in crate::timing) const NOT_TAKEN_LONG: Row = Row::new(0, 0, 6);
    /// DBcc (T, not taken).
    pub(in crate::timing) const DBCC_TRUE: Row = Row::new(1, 1, 4);
    /// DBcc (F, -1, not taken).
    pub(in crate::timing) const DBCC_EXPIRED: Row = Row::new(2, 0, 6);
    /// DBcc (F, not -1, taken).
    pub(in crate::timing) const DBCC_TAKEN: Row = Row::new(6, -2, 10);
    /// DBcc (T, not taken), in loop mode.
    pub(in crate::timing) const DBCC_LOOP_TRUE: Row = Row::new(4, 0, 6);
    /// DBcc (F, -1, not taken), in loop mode.
    pub(in crate::timing) const DBCC_LOOP_EXPIRED: Row = Row::new(6, 0, 8);
    /// DBcc (F, not -1, taken), in loop mode.
    pub(in crate::timing) const DBCC_LOOP_TAKEN: Row = Row::new(6, 0, 10);
}

/// Control (CPU32RM 8.3.12).
pub(super) mod control {
    use crate::timing::Row;

    /// ANDI, EORI, ORI #, SR.
    pub(in crate::timing) const IMMEDIATE_TO_SR: Row = Row::new(0, -2, 12);
    /// ANDI, EORI, ORI #, CCR.
    pub(in crate::timing) const IMMEDIATE_TO_CCR: Row = Row::new(2, 0, 6);
    /// BSR.B and BSR.W.
    pub(in crate::timing) const BSR: Row = Row::new(3, -2, 13);
    /// BSR.L.
    pub(in crate::timing) const BSR_LONG: Row = Row::new(1, -2, 13);
    /// CHK <FEA>, Dn (no ex).
    pub(in crate::timing) const CHK: Row = Row::new(2, 0, 8);
    /// CHK <FEA>, Dn (ex).
    pub(in crate::timing) const CHK_TRAP: Row = Row::new(2, -2, 42);
    /// CHK2 (Op) <FEA>, Dn (ex): x + 2 reads.
    pub(in crate::timing) const CHK2_TRAP: Row = Row::new(2, -2, 52).with_x(1);
    /// JMP <CEA>.
    pub(in crate::timing) const JMP: Row = Row::new(0, -2, 6);
    /// JSR <CEA>.
    pub(in crate::timing) const JSR: Row = Row::new(3, -2, 13);
    /// LEA <CEA>, An.
    pub(in crate::timing) const LEA: Row = Row::new(0, 0, 2);
    /// LINK.W An, #.
    pub(in crate::timing) const LINK: Row = Row::new(2, 0, 10);
    /// LINK.L An, #.
    pub(in crate::timing) const LINK_LONG: Row = Row::new(0, 0, 10);
    /// NOP.
    pub(in crate::timing) const NOP: Row = Row::new(0, 0, 2);
    /// PEA <CEA>.
    pub(in crate::timing) const PEA: Row = Row::new(0, 0, 8);
    /// RTD #.
    pub(in crate::timing) const RTD: Row = Row::new(1, -2, 12);
    /// RTR.
    pub(in crate::timing) const RTR: Row = Row::new(1, -2, 14);
    /// RTS.
    pub(in crate::timing) const RTS: Row = Row::new(1, -2, 12);
    /// UNLK An.
    pub(in crate::timing) const UNLK: Row = Row::new(1, 0, 9);
}

/// Exception-related instructions and operations (CPU32RM 8.3.13).
pub(super) mod exception {
    use crate::timing::Row;

    /// BKPT (Acknowledged).
    pub(in crate::timing) const BKPT: Row = Row::new(0, 0, 14);
    /// BKPT (Bus Error).
    pub(in crate::timing) const BKPT_BUS_ERROR: Row = Row::new(0, -2, 35);
    /// Breakpoint (Acknowledged): the hardware breakpoint.
    pub(in crate::timing) const BREAKPOINT: Row = Row::new(0, 0, 10);
    /// Breakpoint (Bus Error).
    pub(in crate::timing) const BREAKPOINT_BUS_ERROR: Row = Row::new(0, -2, 42);
    /// Interrupt, with a three-clock interrupt acknowledge cycle.
    pub(in crate::timing) const INTERRUPT: Row = Row::new(0, -2, 30);
    /// RESET.
    pub(in crate::timing) const RESET: Row = Row::new(0, 0, 518);
    /// STOP.
    pub(in crate::timing) const STOP: Row = Row::new(2, 0, 12);
    /// LPSTOP.
    pub(in crate::timing) const LPSTOP: Row = Row::new(3, -2, 25);
    /// Divide-by-Zero.
    pub(in crate::timing) const DIVIDE_BY_ZERO: Row = Row::new(0, -2, 36);
    /// Trace.
    pub(in crate::timing) const TRACE: Row = Row::new(0, -2, 36);
    /// TRAP #.
    pub(in crate::timing) const TRAP: Row = Row::new(4, -2, 29);
    /// ILLEGAL.
    pub(in crate::timing) const ILLEGAL: Row = Row::new(0, -2, 25);
    /// A-line.
    pub(in crate::timing) const LINE_A: Row = Row::new(0, -2, 25);
    /// F-line (First word illegal).
    pub(in crate::timing) const LINE_F: Row = Row::new(0, -2, 25);
    /// F-line (Second word illegal) ea = Rn.
    pub(in crate::timing) const LINE_F_SECOND_WORD_REGISTER: Row = Row::new(1, -2, 31);
    /// F-line (Second word illegal) ea != Rn (Op), after the (Save)
    /// row and the effective address.
    pub(in crate::timing) const LINE_F_SECOND_WORD: Row = Row::new(4, -2, 29);
    /// Privileged.
    pub(in crate::timing) const PRIVILEGED: Row = Row::new(0, -2, 25);
    /// TRAPcc (trap).
    pub(in crate::timing) const TRAPCC_TRAP: Row = Row::new(2, -2, 38);
    /// TRAPcc (no trap).
    pub(in crate::timing) const TRAPCC: Row = Row::new(2, 0, 4);
    /// TRAPcc.W (trap).
    pub(in crate::timing) const TRAPCC_WORD_TRAP: Row = Row::new(2, -2, 38);
    /// TRAPcc.W (no trap).
    pub(in crate::timing) const TRAPCC_WORD: Row = Row::new(0, 0, 4);
    /// TRAPcc.L (trap).
    pub(in crate::timing) const TRAPCC_LONG_TRAP: Row = Row::new(0, -2, 38);
    /// TRAPcc.L (no trap).
    pub(in crate::timing) const TRAPCC_LONG: Row = Row::new(0, 0, 6);
    /// TRAPV (trap).
    pub(in crate::timing) const TRAPV_TRAP: Row = Row::new(2, -2, 38);
    /// TRAPV (no trap).
    pub(in crate::timing) const TRAPV: Row = Row::new(2, 0, 4);
}

/// Save and restore (CPU32RM 8.3.14).
pub(super) mod save {
    use crate::timing::Row;

    /// BERR on instruction: <58.
    pub(in crate::timing) const BUS_ERROR_ON_INSTRUCTION: Row = Row::new(0, -2, 58);
    /// BERR on exception.
    pub(in crate::timing) const BUS_ERROR_ON_EXCEPTION: Row = Row::new(0, -2, 48);
    /// RTE (four-word frame).
    pub(in crate::timing) const RTE_FOUR_WORD: Row = Row::new(1, -2, 24);
    /// RTE (six-word frame).
    pub(in crate::timing) const RTE_SIX_WORD: Row = Row::new(1, -2, 26);
    /// RTE (BERR on instruction): of a bus error frame.
    pub(in crate::timing) const RTE_BUS_ERROR: Row = Row::new(1, -2, 50);
    /// RTE (BERR on four-word frame): a bus error reading it.
    pub(in crate::timing) const RTE_BUS_ERROR_ON_FOUR_WORD: Row = Row::new(1, -2, 66);
    /// RTE (BERR on six-word frame).
    pub(in crate::timing) const RTE_BUS_ERROR_ON_SIX_WORD: Row = Row::new(1, -2, 70);
}
