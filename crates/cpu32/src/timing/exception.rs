//! What exception processing costs (CPU32RM 8.3.13, 8.3.14), charged
//! in place of the instruction that did not run: the whole stacking
//! and vector fetch, with the instruction's own decode where the
//! exception is the instruction's.  Opcodes no instruction claims
//! already cost their exception in the table.  CHK, CHK2, TRAPcc and
//! TRAPV trapping charge `timing::trap_taken` instead, and nothing
//! from here.

use crate::timing::Cost;
use crate::timing::decode;
use crate::timing::rows::exception;
use crate::timing::rows::save;

/// RTE's format word, format $2: the six-word frame (CPU32RM 6.4.2).
pub(super) const SIX_WORD_FORMAT: u8 = 0x2;
/// RTE's format word, format $C: the bus error frame (CPU32RM 6.4.3).
pub(super) const BUS_ERROR_FORMAT: u8 = 0xc;

/// Illegal instruction (CPU32RM 8.3.13, ILLEGAL).
pub(crate) const ILLEGAL: Cost = exception::ILLEGAL.cost();
/// Line 1010 (CPU32RM 8.3.13, A-line).
pub(crate) const LINE_A: Cost = exception::LINE_A.cost();
/// Line 1111, first word illegal (CPU32RM 8.3.13, F-line).
pub(crate) const LINE_F: Cost = exception::LINE_F.cost();
/// Privilege violation (CPU32RM 8.3.13, Privileged).
pub(crate) const PRIVILEGE_VIOLATION: Cost = exception::PRIVILEGED.cost();
/// Trace (CPU32RM 8.3.13).
pub(crate) const TRACE: Cost = exception::TRACE.cost();
/// Zero divide (CPU32RM 8.3.13, Divide-by-Zero): charged in addition
/// to the divide instruction's table entry, which the manual does not
/// say the row excludes.
pub(crate) const ZERO_DIVIDE: Cost = exception::DIVIDE_BY_ZERO.cost();
/// Interrupt (CPU32RM 8.3.13), its acknowledge cycle three clocks;
/// spurious and uninitialized interrupts too.
pub(crate) const INTERRUPT: Cost = exception::INTERRUPT.cost();
/// Hardware breakpoint, acknowledged (CPU32RM 8.3.13, Breakpoint).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the core has no BKPT pin to raise one")
)]
pub(crate) const BREAKPOINT: Cost = exception::BREAKPOINT.cost();
/// Hardware breakpoint, bus error on its acknowledge (CPU32RM 8.3.13).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the core has no BKPT pin to raise one")
)]
pub(crate) const BREAKPOINT_BUS_ERROR: Cost = exception::BREAKPOINT_BUS_ERROR.cost();
/// BKPT, acknowledged (CPU32RM 8.3.13): the table's entry for BKPT,
/// and charged again for each further acknowledge when one returns
/// another BKPT.
pub(crate) const BKPT: Cost = exception::BKPT.cost();
/// BKPT, bus error on its acknowledge (CPU32RM 8.3.13): in place of
/// the acknowledged cost the table charges BKPT.
pub(crate) const BKPT_BUS_ERROR: Cost = exception::BKPT_BUS_ERROR.cost();
/// Bus error in an instruction (CPU32RM 8.3.14, BERR on instruction,
/// its < 58 maximum); an address error too, for its frame is the
/// same (6.2.3), though the tables list none.
pub(crate) const BUS_ERROR: Cost = save::BUS_ERROR_ON_INSTRUCTION.cost();
/// Bus error in exception processing (CPU32RM 8.3.14, BERR on
/// exception).
pub(crate) const BUS_ERROR_IN_EXCEPTION: Cost = save::BUS_ERROR_ON_EXCEPTION.cost();
/// Bus error as RTE reads a four-word frame (CPU32RM 8.3.14), in place
/// of RTE's table entry.
pub(crate) const RTE_BUS_ERROR_FOUR_WORD: Cost = save::RTE_BUS_ERROR_ON_FOUR_WORD.cost();
/// Bus error as RTE reads a six-word frame (CPU32RM 8.3.14).  RTE
/// reads SR, PC and the format word, then nothing more of a six-word
/// frame, so a bus error there is always before the format is known:
/// the four-word cost.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no read of a six-word frame follows its format word"
    )
)]
pub(crate) const RTE_BUS_ERROR_SIX_WORD: Cost = save::RTE_BUS_ERROR_ON_SIX_WORD.cost();
/// Format error: the tables list none.  Our choice: the cost of the
/// other four-word frame exceptions, ILLEGAL's (CPU32RM 8.3.13).
pub(crate) const FORMAT_ERROR: Cost = exception::ILLEGAL.cost();

/// Line 1111 for a $F8xx opcode whose second word is illegal (CPU32RM
/// 8.3.13, F-line (Second word illegal)), in place of the TBL cost the
/// table charges `opcode`.
pub(crate) const fn line_f_second_word(opcode: u16) -> Cost {
    decode::line_f_second_word(opcode)
}
