//! Brief extension words, where the 68000 and the CPU32 part.  The
//! 68000 ignores SCALE (bits 10:9) and does not decode bit 8 (M68000PRM
//! 2.4, Figure 2-3); the CPU32 scales the index and takes bit 8 as the
//! full format (CPU32RM 3.4.4, Figure 3-2).  SingleStepTests' words have
//! random bits there, so a case is normalized to what the 68000 did
//! before the CPU32 runs it: those bits cleared, which changes nothing
//! on the 68000.

/// SCALE and the format bit: what the 68000 ignores.
const IGNORED_BITS: u16 = 0x0700;

/// Effective address modes (M68000PRM 2.2), the mode field.
mod mode {
    /// (d16, An)
    pub(super) const DISPLACEMENT: u16 = 0b101;
    /// (d8, An, Xn)
    pub(super) const INDEX: u16 = 0b110;
    /// Mode 7: the register field picks the mode.
    pub(super) const SPECIAL: u16 = 0b111;
}

/// Mode 7's register field (M68000PRM 2.2).
mod special {
    /// (xxx).W
    pub(super) const ABSOLUTE_SHORT: u16 = 0b000;
    /// (xxx).L
    pub(super) const ABSOLUTE_LONG: u16 = 0b001;
    /// (d16, PC)
    pub(super) const PC_DISPLACEMENT: u16 = 0b010;
    /// (d8, PC, Xn)
    pub(super) const PC_INDEX: u16 = 0b011;
    /// #data
    pub(super) const IMMEDIATE: u16 = 0b100;
}

/// The bits of a six-bit effective address field.
const FIELD_MASK: u16 = 0x3f;
/// Where an instruction's register field at bits 11:9 starts.
const REGISTER_SHIFT: u16 = 9;
/// MOVE's lines, 1 to 3 (byte, long, word), in bits 15:12.
const MOVE_LINES: [u16; 3] = [0x1, 0x2, 0x3];
/// Where the line field starts.
const LINE_SHIFT: u16 = 12;
/// MOVE's long size, line 2.
const MOVE_LONG: u16 = 0x2;
/// ORI, ANDI, SUBI, ADDI, EORI, CMPI: 0000 ooo0 ss, the operation in
/// bits 11:9 (M68000PRM 8).
const IMMEDIATE_MASK: u16 = 0xf100;
/// The immediate instructions' fixed bits.
const IMMEDIATE: u16 = 0x0000;
/// Their operations: not 100 (static bit operations) or 111.
const IMMEDIATE_OPERATIONS: [u16; 6] = [0b000, 0b001, 0b010, 0b011, 0b101, 0b110];
/// Static BTST, BCHG, BCLR, BSET #bit: 0000 1000 (M68000PRM 8).
const STATIC_BIT_MASK: u16 = 0xff00;
/// Their fixed bits.
const STATIC_BIT: u16 = 0x0800;
/// MOVEM: 0100 1d00 1s (M68000PRM 8).
const MOVEM_MASK: u16 = 0xfb80;
/// Its fixed bits.
const MOVEM: u16 = 0x4880;
/// The common size field's long, bits 7:6 = 10.
const SIZE_LONG: u16 = 0b10;
/// The common size field's no size, 11.
const SIZE_NONE: u16 = 0b11;
/// Where the common size field starts.
const SIZE_SHIFT: u16 = 6;

/// The extension words a six-bit effective address field takes, and
/// whether they are a brief extension word.
const fn extension(field: u16, long: bool) -> (usize, bool) {
    match (field >> 3 & 0b111, field & 0b111) {
        (mode::DISPLACEMENT, _) => (1, false),
        (mode::INDEX, _) => (1, true),
        (mode::SPECIAL, special::ABSOLUTE_SHORT | special::PC_DISPLACEMENT) => (1, false),
        (mode::SPECIAL, special::ABSOLUTE_LONG) => (2, false),
        (mode::SPECIAL, special::PC_INDEX) => (1, true),
        (mode::SPECIAL, special::IMMEDIATE) => (if long { 2 } else { 1 }, false),
        _ => (0, false),
    }
}

/// The offsets, in words from the opcode, of a 68000 instruction's
/// brief extension words, for the layouts the 68000 has: MOVE's two
/// effective addresses, or one in bits 5:0 after any immediate data.
/// For an opcode whose bits 5:0 are not an effective address the answer
/// is meaningless, so callers ask only of cases with an indexed mode.
#[must_use]
pub fn brief_extension_words(opcode: u16) -> Vec<usize> {
    let line = opcode >> LINE_SHIFT;
    let source = opcode & FIELD_MASK;
    let (fields, leading) = if MOVE_LINES.contains(&line) {
        // MOVE's destination has its mode and register swapped.
        let destination = (opcode >> 3 & 0b111_000) | (opcode >> REGISTER_SHIFT & 0b111);
        (vec![(source, line == MOVE_LONG), (destination, false)], 0)
    } else {
        let size = opcode >> SIZE_SHIFT & 0b11;
        let leading = if opcode & IMMEDIATE_MASK == IMMEDIATE
            && IMMEDIATE_OPERATIONS.contains(&(opcode >> REGISTER_SHIFT & 0b111))
            && size != SIZE_NONE
        {
            if size == SIZE_LONG { 2 } else { 1 }
        } else if opcode & STATIC_BIT_MASK == STATIC_BIT || opcode & MOVEM_MASK == MOVEM {
            1
        } else {
            0
        };
        (vec![(source, false)], leading)
    };
    let mut offset = 1 + leading;
    let mut brief = Vec::new();
    for (field, long) in fields {
        let (words, indexed) = extension(field, long);
        if indexed {
            brief.push(offset);
        }
        offset += words;
    }
    brief
}

/// `word` as the 68000 reads it, if it is a brief extension word.
#[must_use]
pub const fn as_68000_reads(word: u16) -> u16 {
    word & !IGNORED_BITS
}

#[cfg(test)]
mod tests {
    use super::as_68000_reads;
    use super::brief_extension_words;

    #[test]
    fn move_counts_the_source_words_first() {
        // MOVE.L (d8, A0, Xn), (d8, A1, Xn)
        assert_eq!(brief_extension_words(0x23b0), [1, 2]);
        // MOVE.L #imm, (d8, A1, Xn): the long immediate is two words.
        assert_eq!(brief_extension_words(0x23bc), [3]);
        // MOVE.W (d16, A3), (d8, A2, Xn)
        assert_eq!(brief_extension_words(0x35ab), [2]);
    }

    #[test]
    fn immediate_data_comes_before_the_effective_address() {
        // ORI.L #imm, (d8, A0, Xn)
        assert_eq!(brief_extension_words(0x00b0), [3]);
        // BTST #bit, (d8, A0, Xn)
        assert_eq!(brief_extension_words(0x0830), [2]);
        // ADD.W (d8, A0, Xn), D0
        assert_eq!(brief_extension_words(0xd070), [1]);
    }

    #[test]
    fn the_68000_ignores_scale_and_bit_8() {
        assert_eq!(as_68000_reads(0xa7f0), 0xa0f0);
    }
}
