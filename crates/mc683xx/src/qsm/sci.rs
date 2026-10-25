//! The serial communications interface (QSMRM 5; MC68331UM 6.4): a
//! double-buffered transmitter and receiver at SCCR0's baud.
//!
//! The host sees whole frames, not bits: each one a frame time on TXD
//! or RXD, so the flags set in the order and at the times the part sets
//! them.  A frame's time is its start bit's, both ways: one sent is
//! reported as its start bit goes out, with its end and bit time, and
//! one the host sends starts on RXD when it is handed over.

use std::collections::VecDeque;
use std::mem;

use mc683xx_cpu32::cycles::Cycles;

use super::port::Txd;

/// SCCR0's SCBR, the baud divisor (QSMRM 5.2.1).
const SCBR_MASK: u16 = 0x1fff;
/// SCCR0 out of reset: SCBR 4 (QSMRM 5.2.1; MC68331UM D.4.4).
const SCCR0_RESET: u16 = 0x0004;
/// System clocks in a bit time per unit of SCBR: SCI baud = system
/// clock / (32 x SCBR) (QSMRM 5.2.1, equation 5-1).
const CLOCKS_PER_BIT_PER_SCBR: u64 = 32;

/// SCCR1's implemented bits, 14:0 (QSMRM 5.2.2).
const SCCR1_WRITABLE: u16 = 0x7fff;
/// SCCR1 LOOPS: the transmitter feeds the receiver.
const SCCR1_LOOPS: u16 = 1 << 14;
/// SCCR1 WOMS: TXD is an open-drain output (QSMRM 5.2.2).
const SCCR1_WOMS: u16 = 1 << 13;
/// SCCR1 ILT: long idle-line detect, counted from the stop bit.
const SCCR1_ILT: u16 = 1 << 12;
/// SCCR1 PT: odd parity.
const SCCR1_PT: u16 = 1 << 11;
/// SCCR1 PE: parity in the data field's last bit.
const SCCR1_PE: u16 = 1 << 10;
/// SCCR1 M: nine data bits, an eleven-bit frame.
const SCCR1_M: u16 = 1 << 9;
/// SCCR1 WAKE: wakeup by address mark, else by idle line.
const SCCR1_WAKE: u16 = 1 << 8;
/// SCCR1 TIE: TDRE interrupts.
const SCCR1_TIE: u16 = 1 << 7;
/// SCCR1 TCIE: TC interrupts.
const SCCR1_TCIE: u16 = 1 << 6;
/// SCCR1 RIE: RDRF and OR interrupts (MC68331UM D.4.5).
const SCCR1_RIE: u16 = 1 << 5;
/// SCCR1 ILIE: IDLE interrupts.
const SCCR1_ILIE: u16 = 1 << 4;
/// SCCR1 TE: the transmitter.
const SCCR1_TE: u16 = 1 << 3;
/// SCCR1 RE: the receiver.
const SCCR1_RE: u16 = 1 << 2;
/// SCCR1 RWU: receiver wakeup mode.
const SCCR1_RWU: u16 = 1 << 1;
/// SCCR1 SBK: send break.
const SCCR1_SBK: u16 = 1;

/// SCSR TDRE: TDR is empty (QSMRM 5.2.3).
pub(super) const SCSR_TDRE: u16 = 1 << 8;
/// SCSR TC: the transmitter is idle.
pub(super) const SCSR_TC: u16 = 1 << 7;
/// SCSR RDRF: RDR holds a character.
pub(super) const SCSR_RDRF: u16 = 1 << 6;
/// SCSR RAF: the receiver is receiving a frame.
pub(super) const SCSR_RAF: u16 = 1 << 5;
/// SCSR IDLE: an idle line was detected.
pub(super) const SCSR_IDLE: u16 = 1 << 4;
/// SCSR OR: a character came while RDRF was set.
pub(super) const SCSR_OR: u16 = 1 << 3;
/// SCSR NF: noise; never set here (PLAN.md).
const SCSR_NF: u16 = 1 << 2;
/// SCSR FE: framing error or break.
pub(super) const SCSR_FE: u16 = 1 << 1;
/// SCSR PF: parity error.
pub(super) const SCSR_PF: u16 = 1;
/// SCSR out of reset: TDRE and TC (QSMRM 5.2.3).
const SCSR_RESET: u16 = SCSR_TDRE | SCSR_TC;
/// The receiver's flags, which a read of SCDR clears once armed
/// (QSMRM 5.4.2).
const RECEIVE_FLAGS: u16 = SCSR_RDRF | SCSR_IDLE | SCSR_OR | SCSR_NF | SCSR_FE | SCSR_PF;
/// The transmitter's flags, which a write of SCDR clears once armed.
const TRANSMIT_FLAGS: u16 = SCSR_TDRE | SCSR_TC;

/// Bits in a frame besides the data: a start bit and a stop bit
/// (QSMRM 5.3).
const FRAMING_BITS: u32 = 2;
/// Data bits with M clear and set (QSMRM Table 5-3).
const DATA_BITS: u32 = 8;
/// Data bits with M set.
const DATA_BITS_M: u32 = 9;
/// TDR's and RDR's bits, R8/T8 to R0/T0 (QSMRM 5.2.4).
const DATA_MASK: u16 = (1 << DATA_BITS_M) - 1;

/// A frame on TXD or RXD, as the host sends or receives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Frame {
    /// A start bit, the data bits as on the line, LSB first (eight, or
    /// nine with M; the parity bit with PE), right-aligned, and a stop
    /// bit.
    Data(u16),
    /// A frame of zeros, stop bit included (QSMRM 5.2.2, SBK).
    Break,
}

/// A frame the SCI sent on TXD, as the shifter took it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Transmitted {
    /// The frame.
    pub frame: Frame,
    /// When its start bit begins.
    pub start: Cycles,
    /// When its stop bit ends.
    pub end: Cycles,
    /// System clocks in its bit time, 32 x SCBR (QSMRM 5.2.1, equation
    /// 5-1): the baud is the system clock / `bit`.
    pub bit: Cycles,
}

/// What the transmit shifter is sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Sending {
    /// A frame for the line.
    Frame(Frame),
    /// A preamble, a frame of ones (QSMRM 5.3).
    Preamble,
}

/// A frame on RXD, being received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Arriving {
    /// The frame.
    frame: Frame,
    /// When its stop bit ends.
    end: Cycles,
    /// Whether the receiver was enabled at its start bit: one enabled
    /// later never found it.
    seen: bool,
}

/// The SCI's state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Sci {
    /// SCCR0.
    sccr0: u16,
    /// SCCR1.
    sccr1: u16,
    /// SCSR's flags.
    status: u16,
    /// The flags a read of SCSR armed to clear (QSMRM 5.2.3).
    armed: u16,
    /// TDR.
    transmit_data: u16,
    /// RDR.
    receive_data: u16,
    /// The transmit shifter's frame and when it ends.
    shifting: Option<(Sending, Cycles)>,
    /// A preamble queued by TE going from 0 to 1.
    preamble: bool,
    /// Frames the host sent, waiting for RXD.
    line: VecDeque<Frame>,
    /// The frame on RXD.
    arriving: Option<Arriving>,
    /// When RXD will have been idle long enough for IDLE (QSMRM
    /// 5.4.2.1).
    idle_at: Option<Cycles>,
    /// Whether a frame has been received since IDLE last set: IDLE
    /// sets once per idle line.
    idle_armed: bool,
    /// The frames sent on TXD, each reported at its start bit.
    transmitted: Vec<Transmitted>,
}

impl Sci {
    /// The SCI out of reset.
    pub(super) const fn new() -> Self {
        Self {
            sccr0: SCCR0_RESET,
            sccr1: 0,
            status: SCSR_RESET,
            armed: 0,
            transmit_data: 0,
            receive_data: 0,
            shifting: None,
            preamble: false,
            line: VecDeque::new(),
            arriving: None,
            idle_at: None,
            idle_armed: false,
            transmitted: Vec::new(),
        }
    }

    /// The SCI after a reset: frames already sent stay for the host.
    pub(super) fn reset(&mut self) {
        let transmitted = mem::take(&mut self.transmitted);
        *self = Self {
            transmitted,
            ..Self::new()
        };
    }

    /// Whether a snapshot's SCI holds implemented register bits only.
    pub(super) const fn is_valid(&self) -> bool {
        self.sccr0 & !SCBR_MASK == 0
            && self.sccr1 & !SCCR1_WRITABLE == 0
            && self.transmit_data & !DATA_MASK == 0
    }

    /// Sets every bit of SCCR0, as no write can.
    #[cfg(test)]
    pub(super) const fn set_sccr0_unmasked(&mut self, value: u16) {
        self.sccr0 = value;
    }

    /// SCCR0.
    pub(super) const fn sccr0(&self) -> u16 {
        self.sccr0
    }

    /// SCCR1.
    pub(super) const fn sccr1(&self) -> u16 {
        self.sccr1
    }

    /// SCSR as read; the read arms its set flags to clear.
    pub(super) const fn read_status(&mut self) -> u16 {
        self.armed = self.status;
        self.status
    }

    /// SCSR without the read's side effect.
    pub(super) const fn status(&self) -> u16 {
        self.status
    }

    /// RDR as a read of SCDR's lower byte or word returns it; the read
    /// clears the receiver's armed flags (QSMRM 5.2.4).
    pub(super) const fn read_data(&mut self) -> u16 {
        let cleared = self.armed & RECEIVE_FLAGS;
        self.status &= !cleared;
        self.armed &= !cleared;
        self.receive_data
    }

    /// RDR without the read's side effect.
    pub(super) const fn data(&self) -> u16 {
        self.receive_data
    }

    /// The frames sent on TXD since the last call.
    pub(super) fn take_transmitted(&mut self) -> Vec<Transmitted> {
        mem::take(&mut self.transmitted)
    }

    /// The frames sent on TXD not yet taken.
    pub(super) const fn untaken_frames(&self) -> usize {
        self.transmitted.len()
    }

    /// System clocks in a bit time, if the baud rate generator runs
    /// (SCBR 0 disables it).
    fn bit_time(&self) -> Option<u64> {
        let scbr = u64::from(self.sccr0 & SCBR_MASK);
        (scbr != 0).then_some(CLOCKS_PER_BIT_PER_SCBR * scbr)
    }

    /// Data bits in a frame, by M.
    const fn data_bits(&self) -> u32 {
        if self.sccr1 & SCCR1_M != 0 {
            DATA_BITS_M
        } else {
            DATA_BITS
        }
    }

    /// Bits in a frame: 10, or 11 with M.
    const fn frame_bits(&self) -> u32 {
        self.data_bits() + FRAMING_BITS
    }

    /// The data field's mask.
    const fn data_mask(&self) -> u16 {
        (1 << self.data_bits()) - 1
    }

    /// The parity bit's place, the data field's last bit (QSMRM 5.2.2,
    /// PE).
    const fn parity_bit(&self) -> u16 {
        1 << (self.data_bits() - 1)
    }

    /// The parity bit `data`'s other bits need, by PT: the data and
    /// parity hold an even number of ones, or with PT an odd number.
    const fn parity(&self, data: u16) -> bool {
        let ones = (data & (self.parity_bit() - 1)).count_ones();
        (ones % 2 == 1) != (self.sccr1 & SCCR1_PT != 0)
    }

    /// A frame time, if the baud rate generator runs.
    fn frame_time(&self) -> Option<Cycles> {
        self.bit_time()
            .map(|bit| Cycles(bit * u64::from(self.frame_bits())))
    }

    /// SCCR0 written at `now`.
    pub(super) fn write_sccr0(&mut self, value: u16, now: Cycles) {
        self.sccr0 = value & SCBR_MASK;
        self.shift(now);
    }

    /// SCCR1 written at `now`: TE from 0 to 1 queues a preamble (QSMRM
    /// 5.3).
    pub(super) fn write_sccr1(&mut self, value: u16, now: Cycles) {
        let value = value & SCCR1_WRITABLE;
        if value & SCCR1_TE != 0 && self.sccr1 & SCCR1_TE == 0 {
            self.preamble = true;
        }
        self.sccr1 = value;
        self.shift(now);
    }

    /// TDR written at `now`, by a write of SCDR's lower byte or word:
    /// the write clears TDRE and TC if a read of SCSR armed them; with
    /// TDRE still set the character is not sent (QSMRM 5.2.3).
    pub(super) fn write_data(&mut self, value: u16, now: Cycles) {
        self.transmit_data = value & DATA_MASK;
        let cleared = self.armed & TRANSMIT_FLAGS;
        self.status &= !cleared;
        self.armed &= !cleared;
        self.shift(now);
    }

    /// The shifter, idle at `now`, takes what comes next: a queued
    /// preamble, a break while SBK is set, TDR's character; with
    /// nothing, the transmitter is complete (QSMRM 5.3).
    fn shift(&mut self, now: Cycles) {
        if self.shifting.is_some() {
            return;
        }
        let Some(frame_time) = self.frame_time() else {
            return;
        };
        let enabled = self.sccr1 & SCCR1_TE != 0;
        let sending = if self.preamble {
            self.preamble = false;
            Sending::Preamble
        } else if enabled && self.sccr1 & SCCR1_SBK != 0 {
            Sending::Frame(Frame::Break)
        } else if enabled && self.status & SCSR_TDRE == 0 {
            self.status |= SCSR_TDRE;
            Sending::Frame(Frame::Data(self.with_parity(self.transmit_data)))
        } else {
            self.status |= SCSR_TC;
            return;
        };
        self.status &= !SCSR_TC;
        let end = now + frame_time;
        if let Sending::Frame(frame) = sending
            && self.sccr1 & SCCR1_LOOPS == 0
        {
            self.transmitted.push(Transmitted {
                frame,
                start: now,
                end,
                bit: Cycles(self.bit_time().expect("the baud rate generator runs")),
            });
        }
        self.shifting = Some((sending, end));
    }

    /// TDR's character as the line carries it: the data field, its last
    /// bit the parity bit with PE.
    const fn with_parity(&self, data: u16) -> u16 {
        let data = data & self.data_mask();
        if self.sccr1 & SCCR1_PE == 0 {
            return data;
        }
        let parity = self.parity_bit();
        if self.parity(data) {
            data | parity
        } else {
            data & !parity
        }
    }

    /// The host sends `frame` on RXD at `now`: it follows the frames
    /// already sent, back to back, at the SCI's baud.
    pub(super) fn receive(&mut self, frame: Frame, now: Cycles) {
        self.line.push_back(frame);
        if self.arriving.is_none() {
            self.arrive(now);
        }
    }

    /// The next frame on the line starts on RXD at `now`.  With the
    /// baud rate generator stopped the receiver cannot sample: the line's
    /// frames pass unseen.
    fn arrive(&mut self, now: Cycles) {
        let Some(frame_time) = self.frame_time() else {
            self.line.clear();
            return;
        };
        let Some(frame) = self.line.pop_front() else {
            return;
        };
        let seen = self.sccr1 & SCCR1_RE != 0;
        if seen {
            self.status |= SCSR_RAF;
        }
        self.idle_at = None;
        self.arriving = Some(Arriving {
            frame,
            end: now + frame_time,
            seen,
        });
    }

    /// `frame` ends on RXD, or from the transmitter in loop mode, at
    /// `now`: to RDR with its flags, or OR if RDR is full (QSMRM
    /// 5.4.2); in wakeup mode only an address mark is received
    /// (5.4.2.2).
    fn received(&mut self, frame: Frame, now: Cycles) {
        self.status &= !SCSR_RAF;
        let data = match frame {
            Frame::Data(data) => data & self.data_mask(),
            Frame::Break => 0,
        };
        if self.sccr1 & SCCR1_RE != 0 {
            let address_mark = data & self.parity_bit() != 0;
            if self.sccr1 & SCCR1_RWU != 0 && self.sccr1 & SCCR1_WAKE != 0 && address_mark {
                self.sccr1 &= !SCCR1_RWU;
            }
            if self.sccr1 & SCCR1_RWU == 0 {
                self.store(frame, data);
            }
        }
        self.idle_at = self.bit_time().map(|bit| {
            let ones = if self.sccr1 & SCCR1_ILT != 0 {
                0
            } else {
                // the stop bit and the data's trailing ones count
                // toward the idle line (QSMRM 5.2.2, ILT)
                let shifted = u32::from(data) << (u32::BITS - self.data_bits());
                1 + shifted.leading_ones()
            };
            now + Cycles(bit * u64::from(self.frame_bits() - ones.min(self.frame_bits())))
        });
    }

    /// A received frame's `data` into RDR, or OR.
    fn store(&mut self, frame: Frame, data: u16) {
        self.idle_armed = true;
        if self.status & (SCSR_RDRF | SCSR_OR) != 0 {
            self.status |= SCSR_OR;
            return;
        }
        self.receive_data = data;
        self.status |= SCSR_RDRF;
        if frame == Frame::Break {
            self.status |= SCSR_FE;
        } else if self.sccr1 & SCCR1_PE != 0 && (data & self.parity_bit() != 0) != self.parity(data)
        {
            self.status |= SCSR_PF;
        }
    }

    /// RXD has been idle a frame time at `now`: IDLE, or in idle-line
    /// wakeup mode the receiver wakes (QSMRM 5.4.2.1, 5.4.2.2).
    fn idle(&mut self) {
        self.idle_at = None;
        if self.sccr1 & SCCR1_RE == 0 {
            return;
        }
        if self.sccr1 & SCCR1_RWU != 0 {
            if self.sccr1 & SCCR1_WAKE == 0 {
                self.sccr1 &= !SCCR1_RWU;
            }
        } else if self.idle_armed {
            self.idle_armed = false;
            self.status |= SCSR_IDLE;
        }
    }

    /// Every time it waits for, `off` later: its clock was off that
    /// long.
    pub(super) fn delay(&mut self, off: Cycles) {
        if let Some((_, end)) = &mut self.shifting {
            *end += off;
        }
        if let Some(arriving) = &mut self.arriving {
            arriving.end += off;
        }
        if let Some(idle_at) = &mut self.idle_at {
            *idle_at += off;
        }
    }

    /// When it next changes on its own.
    pub(super) fn next_event(&self) -> Option<Cycles> {
        [
            self.shifting.map(|(_, end)| end),
            self.arriving.map(|arriving| arriving.end),
            self.idle_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Brings it to `now`, its events in order.
    pub(super) fn advance(&mut self, now: Cycles) {
        while let Some(at) = self.next_event().filter(|&at| at <= now) {
            if let Some((sending, end)) = self.shifting
                && end == at
            {
                self.shifting = None;
                if let Sending::Frame(frame) = sending
                    && self.sccr1 & SCCR1_LOOPS != 0
                {
                    self.received(frame, at);
                }
                self.shift(at);
            } else if let Some(arriving) = self.arriving
                && arriving.end == at
            {
                self.arriving = None;
                if arriving.seen && self.sccr1 & SCCR1_LOOPS == 0 {
                    self.received(arriving.frame, at);
                }
                self.arrive(at);
            } else {
                self.idle();
            }
        }
    }

    /// Whether the SCI requests an interrupt (QSMRM 5.2.2).
    pub(super) fn requesting(&self) -> bool {
        [
            (SCCR1_TIE, SCSR_TDRE),
            (SCCR1_TCIE, SCSR_TC),
            (SCCR1_RIE, SCSR_RDRF | SCSR_OR),
            (SCCR1_ILIE, SCSR_IDLE),
        ]
        .into_iter()
        .any(|(enable, flags)| self.sccr1 & enable != 0 && self.status & flags != 0)
    }

    /// What it makes of TXD: whether the transmitter has it, TE set or
    /// a frame still going (QSMRM 5.3), and whether WOMS makes it open
    /// drain (5.2.2).
    pub(super) const fn txd(&self) -> Txd {
        Txd {
            transmitter: self.sccr1 & SCCR1_TE != 0 || self.shifting.is_some(),
            open_drain: self.sccr1 & SCCR1_WOMS != 0,
        }
    }
}
