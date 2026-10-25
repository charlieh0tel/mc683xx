//! One of the DUART's two channels: a transmitter with its holding and
//! shift registers, a receiver with its three-character FIFO and shift
//! register, their modes and status (MC68681 3.1-3.4, 4.2.1-4.2.10).

use std::collections::VecDeque;
use std::mem;

use mc683xx_board::inspect::Inspection;
use mc683xx_board::signal::Serial;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_modules::qsm::sci::Frame;
use serde::Deserialize;
use serde::Serialize;

use super::registers::COMMAND_RESET_BREAK_CHANGE;
use super::registers::COMMAND_RESET_ERROR;
use super::registers::COMMAND_RESET_POINTER;
use super::registers::COMMAND_RESET_RECEIVER;
use super::registers::COMMAND_RESET_TRANSMITTER;
use super::registers::COMMAND_START_BREAK;
use super::registers::COMMAND_STOP_BREAK;
use super::registers::CR_RX_DISABLE;
use super::registers::CR_RX_ENABLE;
use super::registers::CR_TX_DISABLE;
use super::registers::CR_TX_ENABLE;
use super::registers::CSR_FIELD_MASK;
use super::registers::CSR_RX_SHIFT;
use super::registers::CSR_TIMER;
use super::registers::FIFO_DEPTH;
use super::registers::MR1_ERROR_BLOCK;
use super::registers::MR1_RX_INTERRUPT_FFULL;
use super::registers::MR1_RX_RTS;
use super::registers::MR2_CTS;
use super::registers::MR2_MODE_SHIFT;
use super::registers::MR2_TX_RTS;
use super::registers::SIXTEENTHS;
use super::registers::SR_FFULL;
use super::registers::SR_FIFO_STATUS;
use super::registers::SR_OVERRUN;
use super::registers::SR_PARITY_ERROR;
use super::registers::SR_RECEIVED_BREAK;
use super::registers::SR_RXRDY;
use super::registers::SR_TXEMT;
use super::registers::SR_TXRDY;
use super::timing::Crystal;
use super::timing::Format;
use super::timing::generator;

/// The clocks a channel can select (MC68681 Table 4-5, CSRA).
#[derive(Debug, Clone, Copy)]
pub(super) struct Clocks {
    /// X1/CLK.
    pub(super) crystal: Crystal,
    /// ACR\[7\]: baud rate set 2.
    pub(super) set_2: bool,
    /// The counter/timer's output as a 16X clock, its divisor of
    /// X1/CLK, while it runs as a timer on X1/CLK.
    pub(super) timer: Option<u64>,
}

impl Clocks {
    /// The 16X clock's divisor of X1/CLK for clock select `code`, or
    /// `None` for one the model does not have (an external clock on an
    /// input pin, or the timer on IP2 or stopped).
    fn divisor(&self, code: u8) -> Option<u64> {
        if code == CSR_TIMER {
            self.timer
        } else {
            generator(code, self.set_2)
        }
    }
}

/// MR2\[7:6\], the channel mode (4.2.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The transmitter and receiver independent.
    Normal,
    /// Received characters retransmitted; the CPU's transmitter cut off.
    Echo,
    /// The transmitter looped to the receiver; `TxD` held high, `RxD`
    /// ignored.
    LocalLoopback,
    /// Received characters retransmitted and not given to the CPU.
    RemoteLoopback,
}

/// A character in the receive FIFO, with the status bits that go with
/// it (3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Received {
    /// The data.
    data: u8,
    /// Its received break, framing error and parity error, SR's bits.
    status: u8,
}

/// A frame on the receive line, waiting for the line to be free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Arrival {
    /// When it reached the receiver.
    at: Time,
    /// The frame.
    serial: Serial,
    /// The 16X clock divisor it was sent with, for one the transmitter
    /// looped back (local loopback: the transmit clock is the
    /// receiver's, 4.2.2.1); `None` for the line's.
    looped: Option<u64>,
}

/// The frame the receiver is on, from its start bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Incoming {
    /// The frame.
    frame: Frame,
    /// The center of its first stop bit, where it is assembled.
    sample: Time,
    /// Whether the receiver took its start bit and has been neither
    /// disabled nor reset since: else the character is lost (4.2.7.4).
    taken: bool,
    /// The format it is received in.
    mr1: u8,
    /// Whether it continues a break the line is already in.
    continues_break: bool,
}

/// The frame the transmitter is shifting out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Shifting {
    /// When its last stop bit ends.
    end: Time,
    /// Whether it is a break.
    breaking: bool,
}

/// What a channel did that reaches outside it, taken by the DUART after
/// each call.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Effects {
    /// Frames put on `TxD`, from their start bits.
    pub(super) transmitted: Vec<(Time, Serial)>,
    /// When the transmitter cleared its RTS bit, OPR\[0\] or OPR\[1\]
    /// (MR2\[5\], 4.2.2.2).
    pub(super) clear_rts: bool,
    /// When the receiver last moved a character into the FIFO, for the
    /// SCC68692's timeout mode.
    pub(super) transferred: Option<Time>,
}

/// A channel's state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Channel {
    /// MR1 and MR2.
    modes: [u8; 2],
    /// Which of them the mode register reaches next (4.1).
    pointer: usize,
    /// CSR: the receiver's clock select high, the transmitter's low.
    clock_select: u8,
    /// Whether the receiver is enabled.
    receiving: bool,
    /// Whether the transmitter is enabled.
    transmitting: bool,
    /// The transmit holding register.
    holding: Option<u8>,
    /// The frame shifting out.
    shifting: Option<Shifting>,
    /// When the next frame may start: the line's last frame's end, a
    /// bit time later after a break (4.2.7.2, stop break).
    transmit_free: Time,
    /// A start break command is in force.
    breaking: bool,
    /// The transmitter was disabled with characters still to send; it
    /// sends them first (4.2.2.2, 4.2.7.3).
    disable_pending: bool,
    /// When the transmitter clears its RTS bit, a bit time after its
    /// last character (MR2\[5\], 4.2.2.2).
    clear_rts_at: Option<Time>,
    /// Frames on the receive line not yet started.
    line: VecDeque<Arrival>,
    /// When the receive line is free: the last frame's end.
    line_free: Time,
    /// The frame being received.
    incoming: Option<Incoming>,
    /// The end of a received break, when ISR's change in break is set
    /// again (4.2.9.1).
    break_end: Option<Time>,
    /// The receive FIFO.
    fifo: VecDeque<Received>,
    /// A character assembled while the FIFO was full, waiting in the
    /// receive shift register (3.2).
    held: Option<Received>,
    /// The last character read, which an empty FIFO gives again.
    last_read: u8,
    /// The overrun error, SR\[4\] (4.2.9.4).
    overrun: bool,
    /// In block error mode, the status of every character that came to
    /// the top of the FIFO since the last reset error status (4.2.1.3).
    block_status: u8,
    /// ISR's change in break (4.2.15.6).
    break_change: bool,
    /// The receiver negated RTS: its FIFO was full at a start bit
    /// (MR1\[7\], 4.2.1.1).
    rts_negated: bool,
    /// What reaches outside since the DUART last took it.
    #[serde(skip)]
    effects: Effects,
}

impl Channel {
    /// MR1.
    const fn mr1(&self) -> u8 {
        self.modes[0]
    }

    /// MR2.
    const fn mr2(&self) -> u8 {
        self.modes[1]
    }

    /// The channel mode.
    const fn mode(&self) -> Mode {
        match self.mr2() >> MR2_MODE_SHIFT {
            0 => Mode::Normal,
            1 => Mode::Echo,
            2 => Mode::LocalLoopback,
            _ => Mode::RemoteLoopback,
        }
    }

    /// The transmitter's format.
    const fn format(&self) -> Format {
        Format::of(self.mr1(), self.mr2())
    }

    /// Whether the CPU reaches the transmitter: not in the echo modes
    /// (4.2.2.1).
    const fn cpu_transmits(&self) -> bool {
        matches!(self.mode(), Mode::Normal | Mode::LocalLoopback)
    }

    /// The receiver's 16X clock divisor.
    fn receive_divisor(&self, clocks: &Clocks) -> Option<u64> {
        clocks.divisor(self.clock_select >> CSR_RX_SHIFT)
    }

    /// The transmitter's.
    pub(super) fn transmit_divisor(&self, clocks: &Clocks) -> Option<u64> {
        clocks.divisor(self.clock_select & CSR_FIELD_MASK)
    }

    /// RESET (2.4): both inactive, the status and FIFO clear, the mode
    /// register pointer at MR1 (4.1).  The modes, the clock select and
    /// what is on the lines stay.
    pub(super) fn reset(&mut self) {
        let line = mem::take(&mut self.line);
        *self = Self {
            modes: self.modes,
            clock_select: self.clock_select,
            line,
            line_free: self.line_free,
            incoming: self.incoming,
            effects: mem::take(&mut self.effects),
            ..Self::default()
        };
        self.lose_incoming();
    }

    /// Refuses what no run of a channel holds: the mode register
    /// pointer past MR2, more characters than the FIFO holds, or one in
    /// the shift register while the FIFO has room (3.2).
    pub(super) fn check(&self) -> Result<(), StateError> {
        if self.pointer >= self.modes.len() {
            return Err(StateError::invalid(
                "a DUART mode register pointer past MR2",
            ));
        }
        if self.fifo.len() > FIFO_DEPTH {
            return Err(StateError::invalid("a DUART FIFO past its depth"));
        }
        if self.held.is_some() && self.fifo.len() < FIFO_DEPTH {
            return Err(StateError::invalid(
                "a DUART character held while its FIFO has room",
            ));
        }
        Ok(())
    }

    /// The mode register read: MR1 or MR2, the pointer then at MR2.
    pub(super) fn read_mode(&mut self) -> u8 {
        let mode = self.modes[self.pointer];
        self.pointer = 1;
        mode
    }

    /// The mode register written.
    pub(super) fn write_mode(&mut self, value: u8) {
        self.modes[self.pointer] = value;
        self.pointer = 1;
    }

    /// CSR written.
    pub(super) const fn write_clock_select(&mut self, value: u8) {
        self.clock_select = value;
    }

    /// SR (4.2.9).
    pub(super) fn status(&self) -> u8 {
        let mut status = 0;
        if let Some(top) = self.fifo.front() {
            status |= SR_RXRDY;
            status |= if self.mr1() & MR1_ERROR_BLOCK != 0 {
                self.block_status
            } else {
                top.status
            };
        }
        if self.fifo.len() >= FIFO_DEPTH {
            status |= SR_FFULL;
        }
        if self.overrun {
            status |= SR_OVERRUN;
        }
        if self.transmitting && self.cpu_transmits() && self.holding.is_none() {
            status |= SR_TXRDY;
            if self.shifting.is_none() {
                status |= SR_TXEMT;
            }
        }
        status
    }

    /// ISR's `TxRDY` for the channel.
    pub(super) fn transmitter_ready(&self) -> bool {
        self.status() & SR_TXRDY != 0
    }

    /// ISR's `RxRDY` or FFULL, as MR1\[6\] selects (4.2.15.7).
    pub(super) fn receiver_ready(&self) -> bool {
        let wanted = if self.mr1() & MR1_RX_INTERRUPT_FFULL != 0 {
            SR_FFULL
        } else {
            SR_RXRDY
        };
        self.status() & wanted != 0
    }

    /// ISR's change in break.
    pub(super) const fn break_change(&self) -> bool {
        self.break_change
    }

    /// Whether the receiver holds RTS negated (MR1\[7\], 4.2.1.1).
    pub(super) const fn rts_negated(&self) -> bool {
        self.rts_negated
    }

    /// Its modes, clock select, status, enables, receive FIFO and
    /// transmit holding register, for
    /// [`Peripheral::inspect`](mc683xx_board::peripheral::Peripheral::inspect).
    pub(super) fn inspect(&self) -> Inspection {
        let fifo: Vec<u8> = self.fifo.iter().map(|received| received.data).collect();
        let shown = Inspection::default()
            .register("mr1", self.mr1())
            .register("mr2", self.mr2())
            .register("csr", self.clock_select)
            .register("sr", self.status())
            .boolean("receiving", self.receiving)
            .boolean("transmitting", self.transmitting)
            .bytes("rx_fifo", fifo);
        let shown = match self.holding {
            Some(byte) => shown.register("tx_holding", byte),
            None => shown.text("tx_holding", "empty"),
        };
        shown.boolean("tx_shifting", self.shifting.is_some())
    }

    /// What reached outside since the last call.
    pub(super) fn take_effects(&mut self) -> Effects {
        mem::take(&mut self.effects)
    }

    /// CR written at `now` with miscellaneous command `command` (already
    /// cut to the peripheral's field) and the enable bits of `value`.
    pub(super) fn command(
        &mut self,
        value: u8,
        command: u8,
        now: Time,
        clocks: &Clocks,
        cts: bool,
    ) {
        match command {
            COMMAND_RESET_POINTER => self.pointer = 0,
            COMMAND_RESET_RECEIVER => {
                self.receiving = false;
                self.fifo.clear();
                self.held = None;
                self.overrun = false;
                self.block_status = 0;
                self.rts_negated = false;
                self.lose_incoming();
            }
            COMMAND_RESET_TRANSMITTER => {
                // a frame already on the line has been driven: it stays
                self.transmitting = false;
                self.holding = None;
                self.shifting = None;
                self.breaking = false;
                self.disable_pending = false;
                self.clear_rts_at = None;
            }
            COMMAND_RESET_ERROR => {
                self.overrun = false;
                self.block_status = 0;
                for received in self.fifo.iter_mut().chain(self.held.as_mut()) {
                    received.status = 0;
                }
            }
            COMMAND_RESET_BREAK_CHANGE => self.break_change = false,
            COMMAND_START_BREAK if self.transmitting => self.breaking = true,
            COMMAND_STOP_BREAK => {
                self.breaking = false;
                if let Some(shifting) = self.shifting.filter(|shifting| shifting.breaking) {
                    // TxD high for a bit time before the next character
                    let bit = self.bit_time(clocks).unwrap_or(0);
                    self.transmit_free = shifting.end + bit;
                }
            }
            _ => {}
        }
        if value & CR_RX_ENABLE != 0 {
            self.receiving = true;
        }
        if value & CR_RX_DISABLE != 0 {
            self.receiving = false;
            if !self.receives_disabled() {
                self.lose_incoming();
            }
        }
        if value & CR_TX_ENABLE != 0 {
            self.transmitting = true;
            self.disable_pending = false;
        }
        if value & CR_TX_DISABLE != 0 {
            self.transmitting = false;
            self.disable_pending = self.holding.is_some() || self.shifting.is_some();
        }
        self.try_transmit(now, clocks, cts);
    }

    /// The transmit holding register written at `now`: ignored while the
    /// transmitter is disabled (3.1) or cut off by an echo mode.
    pub(super) fn write_data(&mut self, value: u8, now: Time, clocks: &Clocks, cts: bool) {
        if self.transmitting && self.cpu_transmits() {
            self.holding = Some(value);
            self.try_transmit(now, clocks, cts);
        }
    }

    /// The receive FIFO read: its top, popped, a character waiting in the
    /// shift register taking the room (3.2).  An empty FIFO gives the
    /// last character again.
    pub(super) fn read_data(&mut self, now: Time) -> u8 {
        let Some(top) = self.fifo.pop_front() else {
            return self.last_read;
        };
        self.last_read = top.data;
        if let Some(held) = self.held.take() {
            self.fifo.push_back(held);
            self.effects.transferred = Some(now);
        }
        if let Some(next) = self.fifo.front() {
            self.block_status |= next.status;
        }
        if self.fifo.len() < FIFO_DEPTH {
            self.rts_negated = false;
        }
        top.data
    }

    /// A frame arrives on `RxD` at `at`.
    pub(super) fn receive(&mut self, serial: Serial, at: Time) {
        self.line.push_back(Arrival {
            at,
            serial,
            looped: None,
        });
    }

    /// Whether the receiver takes characters while disabled: in local
    /// loopback and multidrop modes (4.2.7.4).
    const fn receives_disabled(&self) -> bool {
        matches!(self.mode(), Mode::LocalLoopback) || self.format().multidrop()
    }

    /// The character on the line is lost to the receiver.
    fn lose_incoming(&mut self) {
        if let Some(incoming) = &mut self.incoming {
            incoming.taken = false;
        }
    }

    /// The transmitter's bit time in picoseconds, if it has a clock.
    fn bit_time(&self, clocks: &Clocks) -> Option<u64> {
        let divisor = self.transmit_divisor(clocks)?;
        Some(clocks.crystal.span(SIXTEENTHS * divisor))
    }

    /// When the transmitter has a character and its line is free, it
    /// starts sending it at `now`: or a break, with a break commanded and
    /// nothing left to send (4.2.7.2).  With CTS enabled, CTS (`cts`
    /// true while low) must be asserted (4.2.2.3).
    pub(super) fn try_transmit(&mut self, now: Time, clocks: &Clocks, cts: bool) {
        if now < self.transmit_free || !self.would_transmit(clocks, cts) {
            return;
        }
        let Some(divisor) = self.transmit_divisor(clocks) else {
            return;
        };
        let format = self.format();
        let crystal = clocks.crystal;
        let baud = crystal.baud(divisor);
        let length = crystal.span(format.length() * divisor);
        let frame = match self.holding.take() {
            Some(data) => Frame::Data(format.encode(data)),
            None => Frame::Break,
        };
        self.shifting = Some(Shifting {
            end: now + length,
            breaking: frame == Frame::Break,
        });
        let serial = Serial::timed(frame, baud, length);
        if self.mode() == Mode::LocalLoopback {
            self.line.push_back(Arrival {
                at: now,
                serial,
                looped: Some(divisor),
            });
        } else {
            self.effects.transmitted.push((now, serial));
        }
    }

    /// Whether the transmitter would start a frame once its line is
    /// free, as `try_transmit` decides.
    fn would_transmit(&self, clocks: &Clocks, cts: bool) -> bool {
        let cts_allows = self.mr2() & MR2_CTS == 0 || cts;
        self.shifting.is_none()
            && self.transmit_divisor(clocks).is_some()
            && match self.holding {
                Some(_) => cts_allows && self.cpu_transmits(),
                None => self.breaking && self.transmitting,
            }
    }

    /// When the channel next changes on its own.
    pub(super) fn next_event(&self, clocks: &Clocks, cts: bool) -> Option<Time> {
        let arriving = match (self.incoming, self.line.front()) {
            (Some(incoming), _) => Some(incoming.sample),
            (None, Some(arrival)) => Some(arrival.at.max(self.line_free)),
            (None, None) => None,
        };
        let starting = self
            .would_transmit(clocks, cts)
            .then_some(self.transmit_free);
        [
            self.shifting.map(|shifting| shifting.end),
            starting,
            self.clear_rts_at,
            arriving,
            self.break_end,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Everything due at `now`, which `next_event` gave.
    pub(super) fn step(&mut self, now: Time, clocks: &Clocks, cts: bool) {
        loop {
            let before = (
                self.shifting,
                self.incoming,
                self.line.len(),
                self.break_end,
            );
            self.step_transmitter(now, clocks, cts);
            self.step_receiver(now, clocks);
            if before
                == (
                    self.shifting,
                    self.incoming,
                    self.line.len(),
                    self.break_end,
                )
            {
                break;
            }
        }
    }

    /// The transmitter's events due at `now`.
    fn step_transmitter(&mut self, now: Time, clocks: &Clocks, cts: bool) {
        if let Some(shifting) = self.shifting
            && shifting.end <= now
        {
            self.shifting = None;
            self.transmit_free = self.transmit_free.max(shifting.end);
            self.try_transmit(shifting.end, clocks, cts);
            let idle = self.shifting.is_none() && self.holding.is_none();
            if idle && self.disable_pending {
                self.disable_pending = false;
                if self.mr2() & MR2_TX_RTS != 0 {
                    let bit = self.bit_time(clocks).unwrap_or(0);
                    self.clear_rts_at = Some(shifting.end + bit);
                }
            }
        }
        self.try_transmit(now, clocks, cts);
        if let Some(at) = self.clear_rts_at
            && at <= now
        {
            self.clear_rts_at = None;
            self.effects.clear_rts = true;
        }
    }

    /// The receiver's events due at `now`.
    fn step_receiver(&mut self, now: Time, clocks: &Clocks) {
        if let Some(end) = self.break_end
            && end <= now
            && !self.line_continues_break(end)
        {
            self.break_end = None;
            self.break_change = true;
        }
        match self.incoming {
            Some(incoming) if incoming.sample <= now => {
                self.incoming = None;
                self.assemble(incoming);
            }
            Some(_) => {}
            None => {
                if let Some(arrival) = self.line.front().copied() {
                    let start = arrival.at.max(self.line_free);
                    if start <= now {
                        self.line.pop_front();
                        self.start(arrival, start, clocks);
                    }
                }
            }
        }
    }

    /// Whether the next frame on the line is a break that starts as the
    /// break ending at `end` ends: the line has stayed low.
    fn line_continues_break(&self, end: Time) -> bool {
        self.line
            .front()
            .is_some_and(|arrival| arrival.serial.frame == Frame::Break && arrival.at <= end)
    }

    /// A frame's start bit at `start`: the receiver times it, and an echo
    /// mode sends it on (4.2.2.1).
    fn start(&mut self, arrival: Arrival, start: Time, clocks: &Clocks) {
        let crystal = clocks.crystal;
        let format = Format::of(self.mr1(), self.mr2());
        let mode = self.mode();
        let looped = arrival.looped.is_some();
        let divisor = arrival.looped.or_else(|| self.receive_divisor(clocks));
        let own_length = divisor.map(|divisor| crystal.span(format.length() * divisor));
        let length = match arrival.serial.length {
            0 => own_length.unwrap_or(0),
            length => length,
        };
        let end = start + length;
        self.line_free = end;
        if matches!(mode, Mode::Echo | Mode::RemoteLoopback) && !looped {
            // retransmitted bit by bit at the receive clock, parity and
            // stop bits as received
            let echoed = match divisor {
                Some(divisor) => Serial::timed(arrival.serial.frame, crystal.baud(divisor), length),
                None => arrival.serial,
            };
            self.effects.transmitted.push((start, echoed));
        }
        let Some(divisor) = divisor else {
            return;
        };
        let reaches_receiver = match mode {
            Mode::LocalLoopback => looped,
            Mode::RemoteLoopback => false,
            Mode::Normal | Mode::Echo => true,
        };
        let taken = reaches_receiver && (self.receiving || self.receives_disabled());
        let continues_break =
            arrival.serial.frame == Frame::Break && self.break_end.is_some_and(|at| at >= start);
        if taken && !continues_break {
            // a character waiting for room is lost to this one's start
            // bit (3.2), and RTS negated if the FIFO is full (4.2.1.1)
            if self.held.take().is_some() {
                self.overrun = true;
            }
            if self.mr1() & MR1_RX_RTS != 0 && self.fifo.len() >= FIFO_DEPTH {
                self.rts_negated = true;
            }
        }
        let sample = (start + crystal.span(format.to_stop_center() * divisor)).min(end.max(start));
        if continues_break {
            self.break_end = Some(end);
        }
        self.incoming = Some(Incoming {
            frame: arrival.serial.frame,
            sample,
            taken,
            mr1: self.mr1(),
            continues_break,
        });
    }

    /// A frame assembled at its stop bit's center: into the FIFO, or the
    /// shift register while the FIFO is full (3.2).
    fn assemble(&mut self, incoming: Incoming) {
        if !incoming.taken || incoming.continues_break {
            return;
        }
        let format = Format::of(incoming.mr1, self.mr2());
        let received = match incoming.frame {
            Frame::Data(bits) => {
                let decoded = format.decode(bits);
                // multidrop with the receiver disabled: address characters
                // only (3.4)
                if format.multidrop() && !self.receiving && !decoded.address {
                    return;
                }
                Received {
                    data: decoded.data,
                    status: if decoded.parity_bit_set {
                        SR_PARITY_ERROR
                    } else {
                        0
                    },
                }
            }
            Frame::Break => {
                // one all-zero character; the change in break set at its
                // start and again at its end (4.2.9.1)
                self.break_change = true;
                self.break_end = Some(self.line_free);
                Received {
                    data: 0,
                    status: SR_RECEIVED_BREAK,
                }
            }
        };
        if self.fifo.len() < FIFO_DEPTH {
            if self.fifo.is_empty() {
                self.block_status |= received.status & SR_FIFO_STATUS;
            }
            self.fifo.push_back(received);
            self.effects.transferred = Some(incoming.sample);
        } else {
            self.held = Some(received);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Channel;
    use super::FIFO_DEPTH;
    use super::Received;

    #[test]
    fn a_check_refuses_a_channel_no_run_holds() {
        let received = Received { data: 0, status: 0 };
        assert!(Channel::default().check().is_ok());
        let past_mr2 = Channel {
            pointer: 2,
            ..Channel::default()
        };
        let deep = Channel {
            fifo: [received; FIFO_DEPTH + 1].into(),
            ..Channel::default()
        };
        let held = Channel {
            held: Some(received),
            ..Channel::default()
        };
        for channel in [past_mr2, deep, held] {
            assert!(channel.check().is_err(), "{channel:?}");
        }
    }
}
