//! A call run on an oracle, as a firmware harness runs one on the core:
//! from registers and memory until it reaches a return address, a trap
//! whose handler it does not run, a branch to itself, a fault or an
//! exception, or runs too long.

use crate::memory::Fault;
use crate::memory::RegionMemory;
use crate::state::State;

/// TRAP #n (CPU32RM 4.4, TRAP): the opcode bits, and the number's.
pub const TRAP: u16 = 0x4e40;
/// The bits of TRAP's opcode that are not the number.
pub const TRAP_MASK: u16 = 0xfff0;
/// BRA.S and BRA.W to themselves (CPU32RM 4.4, BRA): a spin a call
/// never leaves.
const SELF_BRANCHES: [(u16, Option<u16>); 2] = [(0x60fe, None), (0x6000, Some(0xfffe))];

/// TRAP #0's vector; #n's is n more (CPU32RM 6.1.1, Table 6-1).
pub const TRAP_VECTOR: u8 = 32;
/// Bytes of a vector in the table (CPU32RM 6.1.1).
pub const VECTOR_BYTES: u32 = 4;
/// SR's S bit (CPU32RM 2.3.1.3, Figure 2-3).
pub const SUPERVISOR: u16 = 1 << 13;
/// SR's T1 and T0, which exception processing clears (CPU32RM 6.1.3).
pub const TRACE_BITS: u16 = 0b11 << 14;
/// Bytes of the format $0 four-word frame: SR, the PC, and the
/// format/vector word (CPU32RM 6.4.1, Figure 6-3).
pub const SHORT_FRAME_BYTES: u32 = 8;
/// Where the format/vector word is in a frame (CPU32RM 6.1.4, Figure
/// 6-1).
pub const FORMAT_WORD_OFFSET: u32 = 6;
/// The format/vector word's vector offset field (CPU32RM 6.1.4).
pub const VECTOR_OFFSET_MASK: u16 = 0x0fff;
/// Where the format/vector word's format field starts (CPU32RM 6.1.4).
pub const FORMAT_SHIFT: u16 = 12;

/// The format $0 frame of an exception through `vector` from SR `sr`,
/// returning to `pc` (CPU32RM 6.4.1, Figure 6-3), as it lies on the
/// stack from its top.
#[must_use]
pub fn short_frame(sr: u16, pc: u32, vector: u8) -> [u8; SHORT_FRAME_BYTES as usize] {
    let [sr_high, sr_low] = sr.to_be_bytes();
    let [pc_3, pc_2, pc_1, pc_0] = pc.to_be_bytes();
    let [format_high, format_low] = (u16::from(vector) * VECTOR_BYTES as u16).to_be_bytes();
    [
        sr_high,
        sr_low,
        pc_3,
        pc_2,
        pc_1,
        pc_0,
        format_high,
        format_low,
    ]
}

/// What a call is run from and until.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    /// The registers it starts with.
    pub state: State,
    /// The vector base it takes exceptions through.
    pub vbr: u32,
    /// The return address that ends it, never executed.
    pub until: u32,
    /// The traps whose handlers it runs; it ends at any other.
    pub handled_traps: &'a [u8],
    /// The most instructions it runs.
    pub step_limit: usize,
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// It reached `Call::until`.
    Returned,
    /// It reached a trap whose handler it does not run: the number.
    Trap(u8),
    /// It reached a branch to itself.
    Spin,
    /// It ran `Call::step_limit` instructions.
    StepLimit,
    /// An access no memory answers.
    Fault(Fault),
    /// The oracle took an exception: its vector, at the instruction at
    /// `pc`.
    Exception {
        /// The vector number.
        vector: u8,
        /// The instruction's address.
        pc: u32,
    },
    /// The oracle cannot be trusted with the call: why.
    Unsupported(&'static str),
}

/// A call's end on an oracle.
#[derive(Debug, Clone)]
pub struct CallEnd {
    /// How it ended.
    pub ending: Ending,
    /// The registers then.
    pub state: State,
    /// Instructions run.
    pub steps: usize,
    /// Memory then.
    pub memory: RegionMemory,
}

/// Whether the instruction at `pc` ends a call before it runs, given
/// the words of memory: a trap whose handler it does not run, or a
/// branch to itself.
pub fn stop_before(
    word: impl Fn(u32) -> Option<u16>,
    pc: u32,
    handled_traps: &[u8],
) -> Option<Ending> {
    let opcode = word(pc)?;
    if opcode & TRAP_MASK == TRAP {
        let number = (opcode & !TRAP_MASK) as u8;
        return (!handled_traps.contains(&number)).then_some(Ending::Trap(number));
    }
    spins(word, pc).then_some(Ending::Spin)
}

/// Whether the instruction at `pc` is a branch to itself, given the
/// words of memory.
pub fn spins(word: impl Fn(u32) -> Option<u16>, pc: u32) -> bool {
    word(pc).is_some_and(|opcode| {
        SELF_BRANCHES.iter().any(|&(first, second)| {
            opcode == first && second.is_none_or(|second| word(pc + 2) == Some(second))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::Ending;
    use super::stop_before;

    #[test]
    fn traps_and_spins_end_a_call() {
        let words = |at: u32| match at {
            0 => Some(0x4e43),
            2 => Some(0x4e41),
            4 => Some(0x60fe),
            6 => Some(0x6000),
            8 => Some(0xfffe),
            _ => Some(0x4e71),
        };
        assert_eq!(stop_before(words, 0, &[1]), Some(Ending::Trap(3)));
        assert_eq!(stop_before(words, 2, &[1]), None);
        assert_eq!(stop_before(words, 4, &[]), Some(Ending::Spin));
        assert_eq!(stop_before(words, 6, &[]), Some(Ending::Spin));
        assert_eq!(stop_before(words, 10, &[]), None);
    }
}

/// A call every oracle's tests run: `MOVEQ #5, D0; TRAP #1; MOVE.W D0,
/// ($2000).W; RTS`, trap #1's handler `ADDQ.L #1, D0; RTE` (CPU32RM
/// 4.4), from SSP $3000 holding the return address, `UNTIL`.
#[cfg(all(test, any(feature = "musashi", feature = "unicorn")))]
pub(crate) mod sample {
    use std::sync::Arc;

    use super::Call;
    use super::SUPERVISOR;
    use crate::memory::Contents;
    use crate::memory::Region;
    use crate::memory::RegionMemory;
    use crate::state::State;

    /// Where the code is.
    pub(crate) const CODE: u32 = 0x1000;
    /// Where trap #1's handler is.
    const HANDLER: u32 = 0x1100;
    /// TRAP #1's vector's address, VBR zero.
    const TRAP_1_VECTOR: usize = 33 * 4;
    /// The RAM the code writes and the stack.
    pub(crate) const RAM: u32 = 0x2000;
    /// The stack's top.
    pub(crate) const STACK: u32 = 0x3000;
    /// The return address.
    pub(crate) const UNTIL: u32 = 0x4000;
    /// The RTS's address.
    const RTS_AT: usize = 0x1008;
    /// The sample's store: MOVE.W D0, ($2000).W.
    pub(crate) const STORE: [u16; 2] = [0x31c0, RAM as u16];
    /// A store to ROM in its place: MOVE.W D0, ($0100).W.
    pub(crate) const STORE_TO_ROM: [u16; 2] = [0x31c0, 0x0100];

    /// The sample's memory, `store` the MOVE's two words.
    pub(crate) fn memory(store: [u16; 2]) -> RegionMemory {
        let mut rom = vec![0; RAM as usize];
        let mut put = |at: usize, words: &[u16]| {
            for (index, word) in words.iter().enumerate() {
                rom[at + 2 * index..at + 2 * index + 2].copy_from_slice(&word.to_be_bytes());
            }
        };
        put(CODE as usize, &[0x7005, 0x4e41, store[0], store[1]]);
        put(RTS_AT, &[0x4e75]);
        put(HANDLER as usize, &[0x5280, 0x4e73]);
        put(TRAP_1_VECTOR, &[0, HANDLER as u16]);
        let mut ram = vec![0; (UNTIL - RAM) as usize];
        let stack = (STACK - RAM) as usize;
        ram[stack..stack + 4].copy_from_slice(&UNTIL.to_be_bytes());
        RegionMemory::new(vec![
            Region {
                start: 0,
                contents: Contents::ReadOnly(Arc::from(rom)),
            },
            Region {
                start: RAM,
                contents: Contents::Writable(ram),
            },
            Region {
                start: UNTIL,
                contents: Contents::ReadOnly(Arc::from(vec![0; 0x1000])),
            },
        ])
    }

    /// The sample call, its handled traps `handled_traps`.
    pub(crate) fn call(handled_traps: &[u8]) -> Call<'_> {
        Call {
            state: State {
                sr: SUPERVISOR | 0x0700,
                pc: CODE,
                ssp: STACK,
                ..State::default()
            },
            vbr: 0,
            until: UNTIL,
            handled_traps,
            step_limit: 100,
        }
    }
}
