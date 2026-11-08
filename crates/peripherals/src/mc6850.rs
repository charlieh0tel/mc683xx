//! `mc6850`: Motorola's MC6850 ACIA (MC6850 data sheet DS9493R4, cited
//! by heading): one asynchronous serial channel, double buffered both
//! ways, on the bus through RS and D7-D0 with an interrupt output and
//! no vector.
//!
//! The board reaches it only through its attachments and ports
//! ([docs/peripherals.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md)):
//! `RxData` and `TxData` are serial nets, `CTS` and `DCD` logic inputs,
//! low (asserted) until driven, `RTS` a logic output, and `IRQ` the
//! interrupt output's level; a peripheral `irq` wires the same output
//! to an IRQ pin.  Tx CLK and Rx CLK are the parameters `tx_clk_hz` and
//! `rx_clk_hz`, which the control register's counter divide divides
//! into the bit rates.  The bus's E clock is the select's cycle.

#[cfg(test)]
mod tests;

use std::collections::VecDeque;

use mc683xx_board::inspect::Inspection;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::PeripheralAccess;
use mc683xx_board::peripheral::Role;
use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::registry::RegistryError;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::frequency::NANOHERTZ_PER_HERTZ;
use mc683xx_modules::qsm::sci::Frame;
use serde::Deserialize;
use serde::Serialize;

use crate::character::Format;
use crate::character::Parity;
use crate::character::SIXTEENTHS;

/// Its registered name.
pub(crate) const NAME: &str = "mc6850";
/// The parameter giving Tx CLK's frequency in hertz.
const TX_CLK_HZ: &str = "tx_clk_hz";
/// The parameter giving Rx CLK's.
const RX_CLK_HZ: &str = "rx_clk_hz";
/// The format of its snapshots.
const VERSION: u32 = 1;

/// The ports, by index: the serial lines, the modem inputs, RTS, then
/// IRQ.
const RX_DATA: usize = 0;
/// `TxData`.
const TX_DATA: usize = 1;
/// `CTS`.
const CTS: usize = 2;
/// `DCD`.
const DCD: usize = 3;
/// `RTS`.
const RTS: usize = 4;
/// `IRQ`.
const IRQ: usize = 5;
/// The bits of RTS and IRQ in the levels last driven.
const RTS_BIT: u8 = 1 << 0;
/// IRQ's.
const IRQ_BIT: u8 = 1 << 1;

/// RS, the peripheral's one address input: high the data registers, low
/// control and status (ACIA Interface Signals for MPU, Register Select;
/// Table 1).
const RS: u32 = 1;

/// CR1:0, the counter divide select (Control Register, Counter Divide
/// Select Bits).
const CR_DIVIDE_MASK: u8 = 0b11;
/// CR1:0 `11`: master reset.
const MASTER_RESET: u8 = 0b11;
/// The divide ratios of CR1:0 `00`, `01` and `10`.
const DIVIDES: [u64; 3] = [1, 16, 64];
/// CR4:2, the word select (Word Select Bits).
const CR_WORD_SHIFT: u8 = 2;
/// The word select field, unshifted.
const CR_WORD_MASK: u8 = 0b111;
/// CR6:5, the transmitter control (Transmitter Control Bits).
const CR_TRANSMIT_SHIFT: u8 = 5;
/// The transmitter control field, unshifted.
const CR_TRANSMIT_MASK: u8 = 0b11;
/// CR6:5 `01`: RTS low, the transmitter's interrupt enabled.
const TRANSMIT_INTERRUPT: u8 = 0b01;
/// CR6:5 `10`: RTS high, the transmitter's interrupt disabled.
const TRANSMIT_RTS_HIGH: u8 = 0b10;
/// CR6:5 `11`: RTS low, a break level on Tx Data, the transmitter's
/// interrupt disabled.
const TRANSMIT_BREAK: u8 = 0b11;
/// CR7, the receive interrupt enable (Receive Interrupt Enable Bit).
const CR_RIE: u8 = 0x80;

/// SR0, receive data register full (Status Register).
const SR_RDRF: u8 = 0x01;
/// SR1, transmit data register empty.
const SR_TDRE: u8 = 0x02;
/// SR2, data carrier detect.
const SR_DCD: u8 = 0x04;
/// SR3, clear-to-send.
const SR_CTS: u8 = 0x08;
/// SR4, framing error.
const SR_FE: u8 = 0x10;
/// SR5, receiver overrun.
const SR_OVRN: u8 = 0x20;
/// SR6, parity error.
const SR_PE: u8 = 0x40;
/// SR7, the IRQ output's state.
const SR_IRQ: u8 = 0x80;

/// The word select's formats, by CR4:2: data bits, parity (odd if
/// true) and stop bits (Word Select Bits).
const WORDS: [(u8, Option<bool>, u64); 8] = [
    (7, Some(false), 2),
    (7, Some(true), 2),
    (7, Some(false), 1),
    (7, Some(true), 1),
    (8, None, 2),
    (8, None, 1),
    (8, Some(false), 1),
    (8, Some(true), 1),
];

/// `mask` if `set`, else none of it.
const fn bits(set: bool, mask: u8) -> u8 {
    if set { mask } else { 0 }
}

/// The format control word `control` selects.
const fn format(control: u8) -> Format {
    let (data_bits, parity, stop_bits) = WORDS[(control >> CR_WORD_SHIFT & CR_WORD_MASK) as usize];
    Format {
        data_bits,
        parity: match parity {
            Some(odd) => Parity::With(odd),
            None => Parity::None,
        },
        stop: SIXTEENTHS * stop_bits,
    }
}

/// The divide ratio control word `control` selects, `None` in master
/// reset.
fn divide(control: u8) -> Option<u64> {
    DIVIDES.get(usize::from(control & CR_DIVIDE_MASK)).copied()
}

/// Whether control word `control` is a master reset.
const fn in_master_reset(control: u8) -> bool {
    control & CR_DIVIDE_MASK == MASTER_RESET
}

/// CR6:5 of control word `control`.
const fn transmit_control(control: u8) -> u8 {
    control >> CR_TRANSMIT_SHIFT & CR_TRANSMIT_MASK
}

/// A data clock, Tx CLK or Rx CLK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Clock {
    /// Its frequency, hertz.
    hz: u64,
}

impl Clock {
    /// The picoseconds `sixteenths` sixteenths of a bit take at its
    /// rate divided by `divide`, rounded down.
    fn span(self, sixteenths: u64, divide: u64) -> u64 {
        let picoseconds =
            u128::from(sixteenths) * u128::from(divide) * u128::from(PICOSECONDS_PER_SECOND)
                / (u128::from(SIXTEENTHS) * u128::from(self.hz));
        picoseconds as u64
    }

    /// The bit rate at its rate divided by `divide`.
    fn baud(self, divide: u64) -> Frequency {
        let nanohertz = u128::from(self.hz) * u128::from(NANOHERTZ_PER_HERTZ) / u128::from(divide);
        Frequency(nanohertz as u64)
    }
}

/// The overrun (Status Register, Receiver Overrun).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Overrun {
    /// None.
    #[default]
    None,
    /// A character was lost while the receive data register was full;
    /// OVRN waits for that register's character to be read.
    Pending,
    /// OVRN is set, and RDRF with it, until the data register is read.
    Shown,
}

/// A frame on the receive line, waiting for the line to be free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Arrival {
    /// When it reached the receiver.
    at: Time,
    /// The frame.
    serial: Serial,
}

/// The frame the receiver is on, from its start bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Incoming {
    /// The frame.
    frame: Frame,
    /// The center of its first stop bit, where it is transferred.
    sample: Time,
    /// Whether the receiver took its start bit and has been neither
    /// master reset nor inhibited by DCD since: else it is lost.
    taken: bool,
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

/// The ACIA's state: what its snapshot holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct State {
    /// The control register.
    control: u8,
    /// The first master reset since power-on holds: RTS and IRQ high
    /// whatever CR6:5 say (Device Operation, Master Reset).
    first_reset: bool,
    /// The transmit data register, while it holds a character not yet
    /// transferred to the shift register.
    holding: Option<u8>,
    /// The frame shifting out.
    shifting: Option<Shifting>,
    /// When the next frame may start: the last frame's end, a bit time
    /// later after a break.
    transmit_free: Time,
    /// Frames on the receive line not yet started.
    line: VecDeque<Arrival>,
    /// When the receive line is free: the last frame's end.
    line_free: Time,
    /// The frame being received.
    incoming: Option<Incoming>,
    /// The end of the last break on the line, while it is the last
    /// frame: a break frame that starts by then continues it.
    break_end: Option<Time>,
    /// The receive data register, kept after it is read (Receive Data
    /// Register).
    data: u8,
    /// Whether the receive data register's character is current: RDRF
    /// but for DCD.
    full: bool,
    /// FE and PE of the character last transferred to the receive data
    /// register.
    errors: u8,
    /// The overrun.
    overrun: Overrun,
    /// The level on CTS, high until driven low: low until driven.
    cts_high: bool,
    /// The level on DCD.
    dcd_high: bool,
    /// The DCD status bit's latch: set by DCD's rise, held until a
    /// status read then a data read, or a master reset.
    dcd_latched: bool,
    /// A status read showed the latched DCD; the next data read clears
    /// it.
    dcd_read: bool,
    /// The time it was last brought to.
    now: Time,
    /// The levels last driven on RTS (`RTS_BIT`) and IRQ (`IRQ_BIT`),
    /// none before the first drive.
    driven: Option<u8>,
}

impl Default for State {
    /// Powered on in its first master reset: the data sheet gives no
    /// power-on state, but asks for a master reset before anything else
    /// (Device Operation, Master Reset; Counter Divide Select Bits).
    fn default() -> Self {
        Self {
            control: MASTER_RESET,
            first_reset: true,
            holding: None,
            shifting: None,
            transmit_free: Time::ZERO,
            line: VecDeque::new(),
            line_free: Time::ZERO,
            incoming: None,
            break_end: None,
            data: 0,
            full: false,
            errors: 0,
            overrun: Overrun::None,
            cts_high: false,
            dcd_high: false,
            dcd_latched: false,
            dcd_read: false,
            now: Time::ZERO,
            driven: None,
        }
    }
}

impl State {
    /// Whether a master reset holds it.
    const fn in_master_reset(&self) -> bool {
        in_master_reset(self.control)
    }

    /// Refuses what no run holds: an overrun with the data register not
    /// full, a DCD read without the latch, a first reset outside master
    /// reset, anything a master reset clears set during one, an error
    /// bit not FE or PE, or levels past IRQ's.
    fn check(&self) -> Result<(), StateError> {
        let invalid = |what| Err(StateError::invalid(what));
        if self.overrun != Overrun::None && !self.full {
            return invalid("an MC6850 overrun with its data register empty");
        }
        if self.dcd_read && !self.dcd_latched {
            return invalid("an MC6850 DCD read without its latch");
        }
        if self.first_reset && !self.in_master_reset() {
            return invalid("an MC6850 first reset outside master reset");
        }
        if self.in_master_reset()
            && (self.full || self.holding.is_some() || self.dcd_latched || self.errors != 0)
        {
            return invalid("an MC6850 status a master reset clears");
        }
        if self.errors & !(SR_FE | SR_PE) != 0 {
            return invalid("an MC6850 error bit not FE or PE");
        }
        if self
            .driven
            .is_some_and(|driven| driven & !(RTS_BIT | IRQ_BIT) != 0)
        {
            return invalid("an MC6850 output past IRQ");
        }
        Ok(())
    }
}

/// An MC6850 on the board.  It has no RESET input: only a master reset
/// resets it, so a board reset changes nothing.
#[derive(Debug)]
struct Acia {
    /// Tx CLK.
    tx_clock: Clock,
    /// Rx CLK.
    rx_clock: Clock,
    /// Its state.
    state: State,
    /// Frames to drive on `TxData`: their start and frame.
    transmitted: Vec<(Time, Serial)>,
}

impl Acia {
    /// From `tx_clk_hz` and `rx_clk_hz`.
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        let clock = |key| {
            let hz: u64 = context.parameters.required(key)?;
            if hz == 0 {
                return Err(ParameterError::invalid(key, "the clock must run"));
            }
            Ok(Clock { hz })
        };
        Ok(Self::new(clock(TX_CLK_HZ)?, clock(RX_CLK_HZ)?))
    }

    /// Powered on, its clocks `tx_clock` and `rx_clock`.
    fn new(tx_clock: Clock, rx_clock: Clock) -> Self {
        Self {
            tx_clock,
            rx_clock,
            state: State::default(),
            transmitted: Vec::new(),
        }
    }

    /// Its ports, in index order, by the data sheet's pin names.
    fn ports() -> Vec<PortSpec> {
        vec![
            PortSpec::input("RxData", Kind::Serial),
            PortSpec::output("TxData", Kind::Serial),
            PortSpec::input("CTS", Kind::Logic),
            PortSpec::input("DCD", Kind::Logic),
            PortSpec::output("RTS", Kind::Logic),
            PortSpec::output("IRQ", Kind::Logic),
        ]
    }

    /// RDRF: the data register current, and DCD low (Status Register,
    /// Receive Data Register Full).
    const fn receive_full(&self) -> bool {
        self.state.full && !self.state.dcd_high
    }

    /// TDRE: the data register empty, inhibited by CTS high or a master
    /// reset (Status Register, Transmit Data Register Empty; Interrupt
    /// Request).
    const fn transmit_empty(&self) -> bool {
        let state = &self.state;
        state.holding.is_none() && !state.cts_high && !state.in_master_reset()
    }

    /// Whether IRQ is asserted: with CR7, RDRF, OVRN or DCD's latch;
    /// with CR6:5 `01`, TDRE; held high in master reset (ACIA Interface
    /// Signals for MPU, Interrupt Request; Device Operation, Master
    /// Reset).
    fn interrupting(&self) -> bool {
        let state = &self.state;
        let receive = state.control & CR_RIE != 0
            && (self.receive_full() || state.overrun == Overrun::Shown || state.dcd_latched);
        let transmit =
            transmit_control(state.control) == TRANSMIT_INTERRUPT && self.transmit_empty();
        !state.in_master_reset() && (receive || transmit)
    }

    /// The status register (Status Register).
    fn status(&self) -> u8 {
        let state = &self.state;
        [
            (self.receive_full(), SR_RDRF),
            (self.transmit_empty(), SR_TDRE),
            (state.dcd_latched || state.dcd_high, SR_DCD),
            (state.cts_high, SR_CTS),
            (state.overrun == Overrun::Shown, SR_OVRN),
            (self.interrupting(), SR_IRQ),
        ]
        .into_iter()
        .fold(state.errors, |status, (set, bit)| status | bits(set, bit))
    }

    /// RTS's level: high with CR6:5 `10`, and through the first master
    /// reset (Peripheral/Modem Control, Request-to-Send; Device
    /// Operation, Master Reset).
    const fn rts_high(&self) -> bool {
        let state = &self.state;
        (state.first_reset && state.in_master_reset())
            || transmit_control(state.control) == TRANSMIT_RTS_HIGH
    }

    /// When it next changes on its own.
    fn next_internal(&self) -> Option<Time> {
        let state = &self.state;
        let receiving = match (state.incoming, state.line.front()) {
            (Some(incoming), _) => Some(incoming.sample),
            (None, Some(arrival)) => Some(arrival.at.max(state.line_free)),
            (None, None) => None,
        };
        let starting = self.would_transmit().then_some(state.transmit_free);
        [
            state.shifting.map(|shifting| shifting.end),
            starting,
            receiving,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Brings it to `now`, each event at its own time.
    fn run_until(&mut self, now: Time) {
        let now = now.max(self.state.now);
        while let Some(at) = self.next_internal().filter(|&at| at <= now) {
            self.step(at);
        }
        self.state.now = now;
    }

    /// Everything due at `at`, which `next_internal` gave.
    fn step(&mut self, at: Time) {
        if let Some(shifting) = self.state.shifting
            && shifting.end <= at
        {
            self.state.shifting = None;
            self.state.transmit_free = self.state.transmit_free.max(shifting.end);
        }
        self.try_transmit(at);
        match self.state.incoming {
            Some(incoming) if incoming.sample <= at => {
                self.state.incoming = None;
                self.transfer(incoming);
            }
            Some(_) => {}
            None => {
                if let Some(arrival) = self.state.line.front().copied() {
                    let start = arrival.at.max(self.state.line_free);
                    if start <= at {
                        self.state.line.pop_front();
                        self.start(arrival.serial, start);
                    }
                }
            }
        }
    }

    /// Whether the transmitter would start a frame once its line is
    /// free: a character in the data register, or else a break the
    /// control register asks for, outside master reset.
    fn would_transmit(&self) -> bool {
        let state = &self.state;
        !state.in_master_reset()
            && state.shifting.is_none()
            && (state.holding.is_some() || transmit_control(state.control) == TRANSMIT_BREAK)
    }

    /// Starts the next frame at `now` if the transmitter would and its
    /// line is free: the data register's character, transferred to the
    /// shift register (Transmit Data Register (TDR)), or a break frame a
    /// character long.
    fn try_transmit(&mut self, now: Time) {
        if now < self.state.transmit_free || !self.would_transmit() {
            return;
        }
        let Some(divide) = divide(self.state.control) else {
            return;
        };
        let format = format(self.state.control);
        let frame = match self.state.holding.take() {
            Some(data) => Frame::Data(format.encode(data)),
            None => Frame::Break,
        };
        let length = self.tx_clock.span(format.length(), divide);
        self.state.shifting = Some(Shifting {
            end: now + length,
            breaking: frame == Frame::Break,
        });
        let serial = Serial::timed(frame, self.tx_clock.baud(divide), length);
        self.transmitted.push((now, serial));
    }

    /// A frame's start bit at `start`: the receiver times it at its own
    /// rate.
    fn start(&mut self, serial: Serial, start: Time) {
        let state = &mut self.state;
        let format = format(state.control);
        let divide = divide(state.control);
        let own_length = divide.map(|divide| self.rx_clock.span(format.length(), divide));
        let length = match serial.length {
            0 => own_length.unwrap_or(0),
            length => length,
        };
        let end = start + length;
        state.line_free = end;
        let breaking = serial.frame == Frame::Break;
        let continues_break = breaking && state.break_end.is_some_and(|at| at >= start);
        state.break_end = breaking.then_some(end);
        let sample = match divide {
            Some(divide) => {
                (start + self.rx_clock.span(format.to_stop_center(), divide)).min(end.max(start))
            }
            None => end,
        };
        state.incoming = Some(Incoming {
            frame: serial.frame,
            sample,
            taken: divide.is_some() && !state.dcd_high,
            continues_break,
        });
    }

    /// A frame at its first stop bit's center: its character into the
    /// receive data register, or lost to an overrun while that register
    /// is full (Receive Data Register (RDR); Status Register).
    fn transfer(&mut self, incoming: Incoming) {
        if !incoming.taken || incoming.continues_break {
            return;
        }
        let state = &mut self.state;
        if state.full {
            if state.overrun == Overrun::None {
                state.overrun = Overrun::Pending;
            }
            return;
        }
        // a word length or parity change is effective at once (Word
        // Select Bits): the character is taken in the format of now
        let format = format(state.control);
        let (line, framing) = match incoming.frame {
            Frame::Data(line) => (line, 0),
            // all zeros, no stop bit (Status Register, Framing Error)
            Frame::Break => (0, SR_FE),
        };
        let decoded = format.decode(line);
        state.data = decoded.data;
        state.errors = framing | bits(decoded.parity_bit_set, SR_PE);
        state.full = true;
    }

    /// The character on the line is lost to the receiver.
    const fn lose_incoming(&mut self) {
        if let Some(incoming) = &mut self.state.incoming {
            incoming.taken = false;
        }
    }

    /// The receiver initialized: its data register not current, its
    /// errors and overrun clear, the character on the line lost.
    fn initialize_receiver(&mut self) {
        let state = &mut self.state;
        state.full = false;
        state.errors = 0;
        state.overrun = Overrun::None;
        self.lose_incoming();
    }

    /// The control register written `value` at `now`.
    fn write_control(&mut self, value: u8, now: Time) {
        let old = self.state.control;
        self.state.control = value;
        if transmit_control(old) == TRANSMIT_BREAK
            && transmit_control(value) != TRANSMIT_BREAK
            && let Some(shifting) = self.state.shifting.filter(|shifting| shifting.breaking)
            && let Some(divide) = divide(old)
        {
            // Tx Data high a bit time before the next character
            self.state.transmit_free = shifting.end + self.tx_clock.span(SIXTEENTHS, divide);
        }
        if in_master_reset(value) {
            self.master_reset();
        } else {
            self.state.first_reset = false;
            self.try_transmit(now);
        }
    }

    /// A master reset: the status register clear but for CTS and DCD,
    /// the receiver and transmitter initialized; the other control bits
    /// stay as written (Counter Divide Select Bits).  A frame already on
    /// Tx Data has been driven: it ends.
    fn master_reset(&mut self) {
        self.initialize_receiver();
        let state = &mut self.state;
        state.holding = None;
        state.dcd_latched = false;
        state.dcd_read = false;
    }

    /// The status register read: a latched DCD shown, for the data read
    /// that clears it.
    fn read_status(&mut self) -> u8 {
        let status = self.status();
        if self.state.dcd_latched {
            self.state.dcd_read = true;
        }
        status
    }

    /// The receive data register read: it keeps its character but RDRF
    /// clears, or a pending overrun shows with RDRF held; a shown one
    /// clears; a DCD shown by a status read clears (Status Register).
    fn read_data(&mut self) -> u8 {
        let state = &mut self.state;
        match state.overrun {
            Overrun::Pending => state.overrun = Overrun::Shown,
            Overrun::Shown => {
                state.overrun = Overrun::None;
                state.full = false;
            }
            Overrun::None => state.full = false,
        }
        if state.dcd_read {
            state.dcd_latched = false;
            state.dcd_read = false;
        }
        state.data
    }

    /// The transmit data register written `value` at `now`: ignored in
    /// master reset, which holds the transmitter initialized.
    fn write_data(&mut self, value: u8, now: Time) {
        if !self.state.in_master_reset() {
            self.state.holding = Some(value);
            self.try_transmit(now);
        }
    }

    /// DCD driven `high`: a rise latches the DCD bit outside master
    /// reset, and DCD high initializes the receiver (Peripheral/Modem
    /// Control, Data Carrier Detect).
    fn set_dcd(&mut self, high: bool) {
        if high && !self.state.dcd_high && !self.state.in_master_reset() {
            self.state.dcd_latched = true;
            self.state.dcd_read = false;
        }
        self.state.dcd_high = high;
        if high {
            self.initialize_receiver();
        }
    }

    /// Drives what changed: frames started on `TxData`, at their start
    /// bits, and RTS and IRQ if their levels changed, now.
    fn sync(&mut self, drive: &mut Drive<'_>) {
        for (at, serial) in self.transmitted.drain(..) {
            drive.drive_at(at, TX_DATA, Signal::Serial(serial));
        }
        let levels = bits(self.rts_high(), RTS_BIT) | bits(!self.interrupting(), IRQ_BIT);
        let changed = self
            .state
            .driven
            .map_or(RTS_BIT | IRQ_BIT, |driven| driven ^ levels);
        for (bit, port) in [(RTS_BIT, RTS), (IRQ_BIT, IRQ)] {
            if changed & bit != 0 {
                drive.drive(port, Signal::Level(levels & bit != 0));
            }
        }
        self.state.driven = Some(levels);
    }
}

impl Peripheral for Acia {
    /// RS low reads the status register, high the receive data register.
    /// The MC6850 gives no vector: an acknowledge through an `iack`
    /// attachment ends in BERR.
    fn read(&mut self, access: PeripheralAccess, drive: &mut Drive<'_>) -> Result<u32, BusError> {
        if access.role == Role::Iack {
            return Err(BusError);
        }
        self.run_until(access.now);
        let value = if access.address & RS == 0 {
            self.read_status()
        } else {
            self.read_data()
        };
        self.sync(drive);
        Ok(access.read_byte(value))
    }

    /// RS low writes the control register, high the transmit data
    /// register.
    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        if access.role == Role::Iack {
            return Err(BusError);
        }
        self.run_until(access.now);
        let value = access.written_byte(value);
        if access.address & RS == 0 {
            self.write_control(value, access.now);
        } else {
            self.write_data(value, access.now);
        }
        self.sync(drive);
        Ok(())
    }

    fn wait_states(&mut self, access: PeripheralAccess) -> Option<u8> {
        (access.role != Role::Iack).then_some(0)
    }

    fn interrupt_request(&self) -> bool {
        self.interrupting()
    }

    fn power_on(&mut self, drive: &mut Drive<'_>) {
        self.sync(drive);
    }

    fn input(&mut self, port: usize, signal: Signal, now: Time, drive: &mut Drive<'_>) {
        match (port, signal) {
            (RX_DATA, Signal::Serial(serial)) => {
                self.state.line.push_back(Arrival { at: now, serial });
                self.run_until(now);
            }
            (CTS, Signal::Level(high)) => {
                self.run_until(now);
                self.state.cts_high = high;
            }
            (DCD, Signal::Level(high)) => {
                self.run_until(now);
                self.set_dcd(high);
            }
            _ => {}
        }
        self.sync(drive);
    }

    fn next_event(&self) -> Option<Time> {
        self.next_internal()
    }

    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {
        self.run_until(now);
        self.sync(drive);
    }

    /// Its registers and lines, as of its last call.
    fn inspect(&self) -> Inspection {
        let state = &self.state;
        let shown = Inspection::default()
            .register("control", state.control)
            .register("status", self.status())
            .register("rx_data", state.data);
        let shown = match state.holding {
            Some(byte) => shown.register("tx_data", byte),
            None => shown.text("tx_data", "empty"),
        };
        shown
            .boolean("tx_shifting", state.shifting.is_some())
            .boolean("cts", state.cts_high)
            .boolean("dcd", state.dcd_high)
            .boolean("rts", self.rts_high())
            .boolean("irq", self.interrupting())
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(
            VERSION,
            &(self.tx_clock, self.rx_clock, &self.state),
        ))
    }

    /// Refuses a snapshot of other clocks and one `State::check`
    /// refuses.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (tx_clock, rx_clock, state): (Clock, Clock, State) = decode(VERSION, snapshot)?;
        if (tx_clock, rx_clock) != (self.tx_clock, self.rx_clock) {
            return Err(StateError::mismatch("an MC6850 of other clocks"));
        }
        state.check()?;
        self.state = state;
        self.transmitted.clear();
        Ok(())
    }
}

/// Registers `mc6850`.
pub(crate) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(NAME, Acia::ports(), |context| {
        Ok(Box::new(Acia::from_parameters(context)?))
    })
}
