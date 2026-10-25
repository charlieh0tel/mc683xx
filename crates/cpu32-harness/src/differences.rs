//! Where the CPU32 differs from the oracles by design: cases excluded,
//! each with its reason.  An instruction family's author adds its own
//! here (undefined flags, for one) with the manual's section.

use mc683xx_cpu32::exception::Exception;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::registers::status;

use crate::decode::Case;
use crate::single_step::PREFETCH_BYTES;

/// SR's S bit (CPU32RM 2.3.1.3, Figure 2-3).
const SUPERVISOR: u16 = 1 << 13;
/// SR's T1 bit, the 68000's T (CPU32RM 2.3.1.3, Figure 2-3).
pub(crate) const TRACE: u16 = 1 << 15;
/// SR's T0 bit, trace on change of flow: the CPU32's, and no bit on
/// the 68000, which reads it as zero (CPU32RM 2.3.1.3).
pub(crate) const TRACE_ON_CHANGE_OF_FLOW: u16 = 1 << 14;
/// Files whose instruction reads SR as data, T1 with it.
const READS_SR: [&str; 4] = ["MOVEfromSR", "ANDItoSR", "ORItoSR", "EORItoSR"];
/// Files whose instruction replaces SR, T1 with it.
pub(crate) const REPLACES_SR: [&str; 2] = ["MOVEtoSR", "STOP"];
/// LINK.W A7 (CPU32RM 4.4, LINK).
const LINK_A7: u16 = 0x4e57;
/// MOVEM register to memory with -(An): 0100 1000 1s10 0rrr (CPU32RM
/// 4.4, MOVEM).
const MOVEM_PREDECREMENT_MASK: u16 = 0xffb8;
/// Its fixed bits.
const MOVEM_PREDECREMENT: u16 = 0x48a0;
/// A7's mask bit with -(An), where bit 0 is A7 and bit 7 A0.
const MOVEM_PREDECREMENT_A7: u16 = 1;
/// Why the CPU32's MOVEM to -(An) stores An differently.
const MOVEM_STORES_DECREMENTED: &str = "MOVEM to -(An) with An in the list stores An less the operand \
     size on the CPU32 (CPU32RM 4.4, MOVEM; M68000PRM 4, MOVEM), An unchanged on the 68000 and \
     in Musashi";

/// Opcodes the CPU32 has and the 68020 does not, as a mask and the bits
/// it keeps, with their reason.
const CPU32_ONLY: [(u16, u16, &str); 2] = [
    (
        0xffc0,
        0xf800,
        "LPSTOP and TBL are the CPU32's own (CPU32RM 4.4, LPSTOP, TBLS, TBLU)",
    ),
    (
        0xffff,
        0x4afa,
        "BGND is the CPU32's own (CPU32RM 4.4, BGND)",
    ),
];

/// BKPT #n, $4848 to $484F (CPU32RM 4.4, BKPT).
const BKPT_MASK: u16 = 0xfff8;
/// BKPT's fixed bits.
const BKPT: u16 = 0x4848;
/// Why BKPT differs.  The differential bus answers every cycle,
/// the breakpoint acknowledge read in CPU space included, so the CPU32
/// executes the word it returns in BKPT's place (CPU32RM 4.4, BKPT;
/// 6.2.5; MC68331UM 4.5.4.1.1).  Musashi runs no acknowledge cycle
/// and always takes the illegal instruction exception.
const BKPT_REASON: &str = "BKPT executes the word its breakpoint acknowledge returns (CPU32RM 6.2.5); \
     Musashi's 68020 always takes illegal instruction";

/// A branch's 8-bit displacement field (CPU32RM 4.4, Bcc).
const DISPLACEMENT: u16 = 0xff;
/// The 8-bit displacement that selects a 32-bit one.
const LONG_DISPLACEMENT: u16 = 0xff;

/// A case's initial word `offset` bytes into its instruction.
fn word(case: &Case, offset: u32) -> u16 {
    let at = case
        .initial
        .registers
        .pc
        .wrapping_sub(PREFETCH_BYTES)
        .wrapping_add(offset);
    case.initial
        .ram
        .iter()
        .find_map(|&(address, word)| (address == at).then_some(word))
        .unwrap_or_default()
}

/// A case's opcode: the initial word at its instruction's address.
pub(crate) fn opcode(case: &Case) -> u16 {
    word(case, 0)
}

/// Whether `opcode` with register list `mask` is a MOVEM to -(An) that
/// stores An itself.
const fn movem_stores_its_address_register(opcode: u16, mask: u16) -> bool {
    let register = opcode & 0b111;
    opcode & MOVEM_PREDECREMENT_MASK == MOVEM_PREDECREMENT
        && mask & MOVEM_PREDECREMENT_A7 << (7 - register) != 0
}

/// The BCD instructions, which Musashi runs otherwise than the 68000 on
/// a digit above 9, as random operands mostly have.  SingleStepTests
/// checks them, digits above 9 included.
const MUSASHI_DECIMAL: [(u16, u16, &str); 4] = [
    (
        0xb1f0,
        0x8100,
        "ABCD and SBCD of a digit above 9 are undefined (CPU32RM 4.4, ABCD, SBCD); the core follows the 68000's SingleStepTests, Musashi does not",
    ),
    // NBCD's modes but An, $4808-$480F being LINK.L.
    (0xfff8, 0x4800, NBCD_UNDEFINED),
    (0xfff0, 0x4810, NBCD_UNDEFINED),
    (0xffe0, 0x4820, NBCD_UNDEFINED),
];
/// Why NBCD is excluded.
const NBCD_UNDEFINED: &str = "NBCD of a digit above 9 is undefined (CPU32RM 4.4, NBCD); the core follows the 68000's SingleStepTests, Musashi does not";

/// Condition codes the manuals leave undefined, by opcode mask and the
/// bits it keeps, with their section.  The core's choice (PLAN.md,
/// Where the manuals are silent) is not compared.
const UNDEFINED_FLAGS: [(u16, u16, u16); 7] = [
    // ABCD and SBCD: N and V (CPU32RM 4.4, ABCD, SBCD; Table 4-1).
    (0xf1f0, 0xc100, status::N | status::V),
    (0xf1f0, 0x8100, status::N | status::V),
    // NBCD's Dn and memory modes (CPU32RM 4.4, NBCD); $4808-$480F is
    // LINK.L, its flags untouched.
    (0xfff8, 0x4800, status::N | status::V),
    (0xfff0, 0x4810, status::N | status::V),
    (0xffe0, 0x4820, status::N | status::V),
    // CHK: Z, V and C, and N when it does not trap, the only case
    // compared (CPU32RM 4.4, CHK).
    (
        0xf140,
        0x4100,
        status::N | status::Z | status::V | status::C,
    ),
    // CHK2 and CMP2: N and V (CPU32RM 4.4, CHK2, CMP2).
    (COMPARE_BOUNDS.0, COMPARE_BOUNDS.1, status::N | status::V),
];
/// DIVS and DIVU, word and long, whose N and Z are undefined after an
/// overflow (CPU32RM 4.4, DIVS, DIVU): masks and bits.
const DIVIDES: [(u16, u16); 2] = [(0xf0c0, 0x80c0), (0xffc0, 0x4c40)];

/// The condition codes the manuals leave undefined after `opcode`, for
/// a run that ended with SR `sr`: compared on neither side.
#[must_use]
pub fn undefined_flags(opcode: u16, sr: u16) -> u16 {
    let fixed = UNDEFINED_FLAGS
        .iter()
        .filter(|(mask, bits, _)| opcode & mask == *bits)
        .fold(0, |flags, (_, _, undefined)| flags | undefined);
    let overflowed =
        sr & status::V != 0 && DIVIDES.iter().any(|(mask, bits)| opcode & mask == *bits);
    if overflowed {
        fixed | status::N | status::Z
    } else {
        fixed
    }
}

/// Why the CPU32 does a SingleStepTests case from `file` (its name, such
/// as `MOVE.w`) differently from the 68000, if it does.
#[must_use]
pub fn single_step(file: &str, case: &Case) -> Option<&'static str> {
    match file {
        "RTE" => Some(
            "the 68000's frames have no format word, which the CPU32's RTE reads (CPU32RM 6.4)",
        ),
        "MOVEfromSR" if case.initial.registers.sr & SUPERVISOR == 0 => Some(
            "MOVE from SR is privileged on the CPU32 (CPU32RM 4.4, MOVE from SR), not on the 68000",
        ),
        "Bcc" | "BRA" | "BSR" if opcode(case) & DISPLACEMENT == LONG_DISPLACEMENT => Some(
            "an 8-bit displacement of $FF selects a 32-bit one on the CPU32 (CPU32RM 4.4, Bcc); \
             the 68000 has none and branches by -1",
        ),
        name if case.initial.registers.sr & TRACE != 0 && READS_SR.contains(&name) => Some(
            "the cases end before the trace T1 calls for, which the CPU32 takes (CPU32RM 6.2.10); \
             run untraced, an instruction that reads SR would read T1 clear",
        ),
        "LINK" if opcode(case) == LINK_A7 => Some(
            "LINK A7 pushes A7 after its decrement, as CPU32RM 4.4 LINK's operation orders it \
             (SP - 4 -> SP; An -> (SP)); the 68000 pushes it before",
        ),
        "MOVEM.w" | "MOVEM.l" if movem_stores_its_address_register(opcode(case), word(case, 2)) => {
            Some(MOVEM_STORES_DECREMENTED)
        }
        _ => None,
    }
}

/// Why the CPU32 runs `opcode`, its next word `extension`, differently
/// from Musashi's 68020, if it does: an opcode the 68020 lacks, or how
/// the core ended it.
#[must_use]
pub fn differential(
    opcode: u16,
    extension: u16,
    exception: Option<&Exception>,
) -> Option<&'static str> {
    if let Some((_, _, reason)) = CPU32_ONLY
        .iter()
        .chain(&MUSASHI_DECIMAL)
        .find(|(mask, bits, _)| opcode & mask == *bits)
    {
        return Some(reason);
    }
    if opcode & BKPT_MASK == BKPT {
        return Some(BKPT_REASON);
    }
    if movem_stores_its_address_register(opcode, extension) {
        return Some(MOVEM_STORES_DECREMENTED);
    }
    let (mask, bits) = COMPARE_BOUNDS;
    if opcode & mask == bits && extension & COMPARE_BOUNDS_ADDRESS_REGISTER != 0 {
        return Some(COMPARE_BOUNDS_REASON);
    }
    match exception {
        Some(Exception::Fault(fault)) if fault.vector == Vector::ADDRESS_ERROR => Some(
            "the 68020 allows a misaligned operand; the CPU32 takes an address error (CPU32RM 6.2.3)",
        ),
        _ => None,
    }
}

/// The 68020's M bit, SR bit 12, which selects its master stack; the
/// CPU32 has none, and its bit 12 reads zero (CPU32RM 2.3.1.3).
pub(crate) const MASTER: u16 = 1 << 12;

/// CHK2 and CMP2 (CPU32RM 4.4, CHK2): mask and bits.
const COMPARE_BOUNDS: (u16, u16) = (0xf9c0, 0x00c0);
/// CHK2 and CMP2's second word D/A bit: an address register.
const COMPARE_BOUNDS_ADDRESS_REGISTER: u16 = 1 << 15;

/// Why CHK2 and CMP2 of an address register are excluded.
const COMPARE_BOUNDS_REASON: &str = "CHK2 and CMP2 of an address register: Musashi's C is not CPU32RM Table 4-1's against the sign-extended bounds (CPU32RM 4.4, CHK2)";

/// Why the CPU32 ends a divide `opcode` differently from Musashi's
/// 68020, by the SR the core ended with, if it does.
#[must_use]
pub fn differential_divide(opcode: u16, sr: u16) -> Option<&'static str> {
    (sr & status::V != 0 && DIVIDES.iter().any(|(mask, bits)| opcode & mask == *bits)).then_some(
        "a divide overflow, after which the CPU32 clears C (CPU32RM 4.4, DIVS, DIVU) and Musashi's 68020 keeps it: not compared on any oracle",
    )
}

/// Why the CPU32 ends a completed instruction differently from
/// Musashi's 68020, by the SR Musashi ended with, if it does.
#[must_use]
pub fn differential_result(expected_sr: u16) -> Option<&'static str> {
    (expected_sr & MASTER != 0)
        .then_some("the instruction set the 68020's M bit, which the CPU32 lacks (CPU32RM 2.3.1.3)")
}

#[cfg(test)]
mod tests {
    use super::movem_stores_its_address_register;

    #[test]
    fn movem_to_predecrement_storing_its_register() {
        // MOVEM.L A2, -(A2): A2 is bit 5 with -(An).
        assert!(movem_stores_its_address_register(0x48e2, 1 << 5));
        assert!(!movem_stores_its_address_register(0x48e2, 1 << 4));
        // MOVEM.L A2, (A2) is not predecrement.
        assert!(!movem_stores_its_address_register(0x48d2, 1 << 5));
    }
}
