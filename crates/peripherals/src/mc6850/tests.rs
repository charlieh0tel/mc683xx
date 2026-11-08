//! The ACIA's registers and lines, each against its data sheet heading.

use mc683xx_board::inspect::Value;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Driven;
use mc683xx_board::peripheral::Lane;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::PeripheralAccess;
use mc683xx_board::peripheral::Role;
use mc683xx_board::registry::Context;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::Parameters;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Direction;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::qsm::sci::Frame;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::size::Size;

use super::Acia;
use super::CTS;
use super::Clock;
use super::DCD;
use super::IRQ;
use super::NAME;
use super::Overrun;
use super::RTS;
use super::RX_DATA;
use super::State;
use super::TX_DATA;
use super::VERSION;

/// RS low: status read, control written (Table 1).
const STATUS: u8 = 0;
/// RS high: receive data read, transmit data written.
const DATA: u8 = 1;

/// Tx CLK and Rx CLK: 16 times 9600 baud.
const CLOCK_HZ: u64 = 153_600;
/// Master reset (Counter Divide Select Bits).
const RESET: u8 = 0x03;
/// Divide by 16, 8 bits, no parity, 1 stop bit, RTS low, no transmit
/// interrupt: the 58503B's control word.
const RUN: u8 = 0x15;
/// CR7, the receive interrupt enable.
const RIE: u8 = 0x80;
/// CR6:5 `01`, the transmit interrupt enable.
const TIE: u8 = 0x20;
/// CR6:5 `10`: RTS high.
const RTS_HIGH: u8 = 0x40;
/// CR6:5 `11`: a break.
const BREAK: u8 = 0x60;

/// Status bits (Status Register).
const RDRF: u8 = 0x01;
/// TDRE.
const TDRE: u8 = 0x02;
/// DCD.
const DCD_BIT: u8 = 0x04;
/// CTS.
const CTS_BIT: u8 = 0x08;
/// FE.
const FE: u8 = 0x10;
/// OVRN.
const OVRN: u8 = 0x20;
/// PE.
const PE: u8 = 0x40;
/// IRQ.
const IRQ_BIT: u8 = 0x80;

/// Picoseconds in a bit at 9600 baud.
const BIT: u64 = 104_166_666;
/// Ten bits: start, eight data, one stop.
const CHARACTER: u64 = 1_041_666_666;
/// Start bit to the center of the stop bit: nine and a half bits.
const TO_STOP_CENTER: u64 = 989_583_333;

/// An ACIA under test.
struct Bench {
    /// The peripheral.
    acia: Acia,
}

impl Bench {
    /// Powered on, both clocks at `CLOCK_HZ`.
    fn new() -> Self {
        let clock = Clock { hz: CLOCK_HZ };
        let mut acia = Acia::new(clock, clock);
        acia.power_on(&mut Drive::new(Time::ZERO));
        Self { acia }
    }

    /// Master reset, then `control`, as the 58503B starts it.
    fn started(control: u8) -> Self {
        let mut bench = Self::new();
        bench.write(STATUS, RESET, Time::ZERO);
        bench.write(STATUS, control, Time::ZERO);
        bench
    }

    /// An access to `register` at `now` through `role`.
    fn access(register: u8, role: Role, now: Time) -> PeripheralAccess {
        let select = ChipSelect::Numbered(Number::new(8).unwrap());
        PeripheralAccess::new(
            select,
            u32::from(register),
            Size::Byte,
            FunctionCode::SUPERVISOR_DATA,
            now,
        )
        .through(0, role, Lane::Both)
    }

    /// `register` written `value` at `now`: what it drove.
    fn write(&mut self, register: u8, value: u8, now: Time) -> Vec<Driven> {
        let mut drive = Drive::new(now);
        self.acia
            .write(
                Self::access(register, Role::ReadWrite, now),
                u32::from(value),
                &mut drive,
            )
            .unwrap();
        drive.finish().driven
    }

    /// `register` read at `now`.
    fn read(&mut self, register: u8, now: Time) -> u8 {
        let mut drive = Drive::new(now);
        self.acia
            .read(Self::access(register, Role::ReadWrite, now), &mut drive)
            .unwrap() as u8
    }

    /// `signal` into `port` at `now`: what it drove.
    fn input(&mut self, port: usize, signal: Signal, now: Time) -> Vec<Driven> {
        let mut drive = Drive::new(now);
        self.acia.input(port, signal, now, &mut drive);
        drive.finish().driven
    }

    /// Advances through every event up to `until`: what it drove.
    fn run(&mut self, until: Time) -> Vec<Driven> {
        let mut driven = Vec::new();
        while let Some(next) = self.acia.next_event().filter(|&next| next <= until) {
            let mut drive = Drive::new(next);
            self.acia.advance(next, &mut drive);
            driven.extend(drive.finish().driven);
        }
        let mut drive = Drive::new(until);
        self.acia.advance(until, &mut drive);
        driven.extend(drive.finish().driven);
        driven
    }
}

/// The frames among `driven`, with their times.
fn frames(driven: &[Driven]) -> Vec<(Time, Serial)> {
    driven
        .iter()
        .filter(|driven| driven.port == TX_DATA)
        .filter_map(|driven| match driven.signal {
            Signal::Serial(serial) => Some((driven.at, serial)),
            _ => None,
        })
        .collect()
}

/// The levels among `driven`, by port.
fn levels(driven: &[Driven]) -> Vec<(usize, bool)> {
    driven
        .iter()
        .filter_map(|driven| match driven.signal {
            Signal::Level(level) => Some((driven.port, level)),
            _ => None,
        })
        .collect()
}

/// An 8N1 character at 9600 baud from the line.
fn character(byte: u8) -> Signal {
    Signal::Serial(Serial::new(
        Frame::Data(u16::from(byte)),
        Frequency::from_hz(9600),
        10,
    ))
}

/// The peripheral built from the parameters `table`.
fn construct(table: &str) -> Result<Box<dyn Peripheral>, ParameterError> {
    let mut registry = Registry::new();
    crate::register(&mut registry).unwrap();
    let parameters = Parameters::parse(table).unwrap();
    let environment = Environment::default();
    let context = Context::new("acia", &parameters, &environment);
    let peripheral = registry.get(NAME).unwrap().construct(&context)?.peripheral;
    if let Some(key) = parameters.unread() {
        return Err(ParameterError::Unknown(key));
    }
    Ok(peripheral)
}

#[test]
fn it_declares_the_pins_by_their_names() {
    let mut registry = Registry::new();
    crate::register(&mut registry).unwrap();
    let ports: Vec<(String, Kind, Direction)> = registry
        .get(NAME)
        .unwrap()
        .ports()
        .unwrap()
        .iter()
        .map(|port| (port.name.clone(), port.kind, port.direction))
        .collect();
    let expected = [
        ("RxData", Kind::Serial, Direction::In),
        ("TxData", Kind::Serial, Direction::Out),
        ("CTS", Kind::Logic, Direction::In),
        ("DCD", Kind::Logic, Direction::In),
        ("RTS", Kind::Logic, Direction::Out),
        ("IRQ", Kind::Logic, Direction::Out),
    ]
    .map(|(name, kind, direction)| (name.to_owned(), kind, direction));
    assert_eq!(ports, expected);
    assert_eq!([RX_DATA, TX_DATA, CTS, DCD, RTS, IRQ], [0, 1, 2, 3, 4, 5]);
}

#[test]
fn the_clocks_are_required_parameters() {
    assert!(construct("tx_clk_hz = 153600\nrx_clk_hz = 153600").is_ok());
    assert!(matches!(
        construct("tx_clk_hz = 153600"),
        Err(ParameterError::Missing(_))
    ));
    assert!(matches!(
        construct("tx_clk_hz = 0\nrx_clk_hz = 153600"),
        Err(ParameterError::Invalid { .. })
    ));
    assert!(matches!(
        construct("tx_clk_hz = 1\nrx_clk_hz = 1\nclock_hz = 1"),
        Err(ParameterError::Unknown(_))
    ));
}

/// Power-on is the first master reset: RTS and IRQ high, TDRE
/// inhibited, nothing sent (Device Operation, Master Reset).
#[test]
fn it_powers_on_in_its_first_master_reset() {
    let clock = Clock { hz: CLOCK_HZ };
    let mut acia = Acia::new(clock, clock);
    let mut drive = Drive::new(Time::ZERO);
    acia.power_on(&mut drive);
    assert_eq!(levels(&drive.finish().driven), [(RTS, true), (IRQ, true)]);
    let mut bench = Bench { acia };
    assert_eq!(bench.read(STATUS, Time::ZERO), 0);
    assert!(frames(&bench.write(DATA, b'x', Time::ZERO)).is_empty());
    // CR6:5 00 asks for RTS low: the first master reset holds it high
    assert!(levels(&bench.write(STATUS, RESET, Time::ZERO)).is_empty());
    assert_eq!(
        levels(&bench.write(STATUS, RUN, Time::ZERO)),
        [(RTS, false)]
    );
    assert_eq!(bench.read(STATUS, Time::ZERO), TDRE);
}

/// A later master reset clears the status but for CTS and DCD,
/// initializes both sections, holds IRQ high and sets RTS as CR6:5 say
/// (Counter Divide Select Bits; Device Operation, Master Reset).
#[test]
fn a_master_reset_clears_the_status_and_holds_irq_high() {
    let mut bench = Bench::started(RUN | RIE);
    bench.input(RX_DATA, character(b'a'), Time::ZERO);
    bench.input(RX_DATA, character(b'b'), Time::ZERO);
    bench.input(CTS, Signal::Level(true), Time::ZERO);
    let now = Time(3 * CHARACTER);
    bench.run(now);
    assert!(bench.acia.interrupt_request());
    assert_eq!(bench.read(STATUS, now), IRQ_BIT | CTS_BIT | RDRF);
    // a master reset with CR6:5 10: RTS high, IRQ released
    let driven = bench.write(STATUS, RESET | RTS_HIGH | RIE, now);
    assert_eq!(levels(&driven), [(RTS, true), (IRQ, true)]);
    assert_eq!(bench.read(STATUS, now), CTS_BIT);
    assert_eq!(bench.acia.state.overrun, Overrun::None);
    assert_eq!(bench.acia.state.control, RESET | RTS_HIGH | RIE);
    // CR6:5 00 with the next one: RTS low, the first reset over
    assert_eq!(levels(&bench.write(STATUS, RESET, now)), [(RTS, false)]);
    // the transmit data register is cleared and ignored
    bench.input(CTS, Signal::Level(false), now);
    bench.write(STATUS, RUN, now);
    bench.write(DATA, b'x', now);
    bench.write(DATA, b'y', now);
    bench.write(STATUS, RESET, now);
    assert!(frames(&bench.write(DATA, b'z', now)).is_empty());
    let after = now + CHARACTER;
    assert!(frames(&bench.run(after)).is_empty());
    let sent = frames(&bench.write(STATUS, RUN, after));
    assert!(sent.is_empty(), "nothing held across the reset: {sent:?}");
}

/// The word select's eight formats (Word Select Bits): the data bits,
/// the parity bit after them, and one or two stop bits.
#[test]
fn each_word_select_frames_its_character() {
    // CR4:2, bits on the line for $C1, and frame bits
    let cases: [(u8, u16, u64); 8] = [
        (0b000, 0x41, 11),         // 7E2: $41 has two ones, parity 0
        (0b001, 0x41 | 0x80, 11),  // 7O2
        (0b010, 0x41, 10),         // 7E1
        (0b011, 0xc1, 10),         // 7O1
        (0b100, 0xc1, 11),         // 8N2
        (0b101, 0xc1, 10),         // 8N1
        (0b110, 0xc1 | 0x100, 11), // 8E1: three ones, parity 1
        (0b111, 0xc1, 11),         // 8O1
    ];
    for (word, line, bits) in cases {
        let mut bench = Bench::started(0x01 | word << 2);
        let sent = frames(&bench.write(DATA, 0xc1, Time::ZERO));
        let [(at, serial)] = sent[..] else {
            panic!("one frame for {word:03b}: {sent:?}");
        };
        assert_eq!(at, Time::ZERO);
        assert_eq!(serial.frame, Frame::Data(line), "{word:03b}");
        assert_eq!(serial.length, bits * 1_000_000_000_000 / 9600, "{word:03b}");
        assert_eq!(serial.baud, Frequency::from_hz(9600));
    }
}

/// CR1:0 divide Tx CLK by 1, 16 or 64 (Counter Divide Select Bits).
#[test]
fn the_counter_divide_gives_the_bit_rate() {
    for (divide, baud) in [(0b00, 153_600), (0b01, 9600), (0b10, 2400)] {
        let mut bench = Bench::started(0x14 | divide);
        let sent = frames(&bench.write(DATA, b'd', Time::ZERO));
        assert_eq!(sent[0].1.baud, Frequency::from_hz(baud));
        assert_eq!(sent[0].1.length, 10_000_000_000_000 / baud);
    }
}

/// A character goes out at once if the transmitter is idle and TDRE
/// sets as it moves to the shift register; the next waits in the data
/// register for the first's end (Transmit; Transmit Data Register).
#[test]
fn the_transmitter_is_double_buffered() {
    let mut bench = Bench::started(RUN);
    assert_eq!(frames(&bench.write(DATA, b'1', Time::ZERO)).len(), 1);
    assert_eq!(bench.read(STATUS, Time::ZERO) & TDRE, TDRE);
    assert!(frames(&bench.write(DATA, b'2', Time::ZERO)).is_empty());
    assert_eq!(bench.read(STATUS, Time::ZERO) & TDRE, 0);
    let sent = frames(&bench.run(Time(2 * CHARACTER)));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, Time(CHARACTER));
    assert_eq!(sent[0].1.frame, Frame::Data(u16::from(b'2')));
    assert_eq!(bench.read(STATUS, Time(2 * CHARACTER)) & TDRE, TDRE);
}

/// A character is transferred at its stop bit's center; reading the
/// data register clears RDRF and keeps the character (Receive Data
/// Register).
#[test]
fn a_character_is_received_at_its_stop_bit_and_read_once() {
    let mut bench = Bench::started(RUN);
    bench.input(RX_DATA, character(b'r'), Time::ZERO);
    bench.run(Time(TO_STOP_CENTER - 1));
    assert_eq!(bench.read(STATUS, Time(TO_STOP_CENTER - 1)) & RDRF, 0);
    bench.run(Time(TO_STOP_CENTER));
    let now = Time(TO_STOP_CENTER);
    assert_eq!(bench.read(STATUS, now), RDRF | TDRE);
    assert_eq!(bench.read(DATA, now), b'r');
    assert_eq!(bench.read(STATUS, now), TDRE);
    assert_eq!(bench.read(DATA, now), b'r', "a non-destructive read");
}

/// The receiver takes a frame at its own rate whatever the sender's,
/// as the MC68681 does: a 4800-baud frame is still its bits.
#[test]
fn the_receiver_does_not_check_the_senders_baud() {
    let mut bench = Bench::started(RUN);
    let slow = Serial::new(Frame::Data(u16::from(b's')), Frequency::from_hz(4800), 10);
    bench.input(RX_DATA, Signal::Serial(slow), Time::ZERO);
    bench.run(Time(TO_STOP_CENTER));
    assert_eq!(bench.read(DATA, Time(TO_STOP_CENTER)), b's');
    // an untimed frame is a character long at the receiver's rate
    let mut bench = Bench::started(RUN);
    let untimed = Serial::untimed(Frame::Data(u16::from(b'u')));
    bench.input(RX_DATA, Signal::Serial(untimed), Time::ZERO);
    bench.input(RX_DATA, Signal::Serial(untimed), Time::ZERO);
    bench.run(Time(CHARACTER + TO_STOP_CENTER - 1));
    assert_eq!(bench.read(DATA, Time(CHARACTER)), b'u');
    let now = Time(CHARACTER + TO_STOP_CENTER - 1);
    assert_eq!(bench.read(STATUS, now) & RDRF, 0, "the second not yet");
    bench.run(Time(CHARACTER + TO_STOP_CENTER));
    assert_eq!(
        bench.read(STATUS, Time(CHARACTER + TO_STOP_CENTER)) & RDRF,
        RDRF
    );
}

/// PE and FE go with the character in the data register, until the
/// next is transferred (Status Register, Framing Error, Parity Error);
/// a seven-bit character reads with bit 7 zero (Receive).
#[test]
fn errors_go_with_their_character() {
    // 7E1
    let mut bench = Bench::started(0x09);
    let wrong = Serial::new(Frame::Data(0x41 | 0x80), Frequency::from_hz(9600), 10);
    bench.input(RX_DATA, Signal::Serial(wrong), Time::ZERO);
    let now = Time(CHARACTER);
    bench.run(now);
    assert_eq!(bench.read(STATUS, now), RDRF | TDRE | PE);
    assert_eq!(bench.read(DATA, now), 0x41);
    assert_eq!(bench.read(STATUS, now), TDRE | PE, "PE stays with it");
    bench.input(
        RX_DATA,
        Signal::Serial(Serial::untimed(Frame::Data(0x41))),
        now,
    );
    let now = Time(2 * CHARACTER);
    bench.run(now);
    assert_eq!(bench.read(STATUS, now), RDRF | TDRE);
    // a break: all zeros and no stop bit; back to back, one character
    let mut bench = Bench::started(RUN);
    let brk = Signal::Serial(Serial::new(Frame::Break, Frequency::from_hz(9600), 10));
    bench.input(RX_DATA, brk, Time::ZERO);
    bench.input(RX_DATA, brk, Time::ZERO);
    let now = Time(3 * CHARACTER);
    bench.run(now);
    assert_eq!(bench.read(STATUS, now), RDRF | TDRE | FE);
    assert_eq!(bench.read(DATA, now), 0);
    assert_eq!(bench.read(STATUS, now) & (RDRF | OVRN), 0);
}

/// The second character received without a read is lost; OVRN shows
/// once the first is read, with RDRF held, and the next data read
/// clears both (Status Register, Receiver Overrun).
#[test]
fn an_overrun_shows_after_the_character_before_it_is_read() {
    let mut bench = Bench::started(RUN | RIE);
    for byte in *b"abc" {
        bench.input(RX_DATA, character(byte), Time::ZERO);
    }
    let now = Time(3 * CHARACTER);
    bench.run(now);
    assert_eq!(bench.read(STATUS, now), IRQ_BIT | RDRF | TDRE);
    assert_eq!(bench.read(DATA, now), b'a');
    assert_eq!(bench.read(STATUS, now), IRQ_BIT | OVRN | RDRF | TDRE);
    assert!(bench.acia.interrupt_request());
    // characters keep being lost while it shows
    bench.input(RX_DATA, character(b'd'), now);
    let now = now + CHARACTER;
    bench.run(now);
    assert_eq!(bench.read(DATA, now), b'a', "the lost ones never arrive");
    assert_eq!(bench.read(STATUS, now), TDRE);
    assert!(!bench.acia.interrupt_request());
    bench.input(RX_DATA, character(b'e'), now);
    let now = now + CHARACTER;
    bench.run(now);
    assert_eq!(bench.read(DATA, now), b'e');
}

/// DCD's rise latches its bit and interrupts with CR7; a status read
/// then a data read clear the latch, after which the bit follows the
/// input; DCD high inhibits and initializes the receiver
/// (Peripheral/Modem Control, Data Carrier Detect; Status Register,
/// Data Carrier Detect).
#[test]
fn dcd_latches_its_rise_until_a_status_and_a_data_read() {
    let mut bench = Bench::started(RUN | RIE);
    bench.input(RX_DATA, character(b'k'), Time::ZERO);
    let now = Time(CHARACTER);
    bench.run(now);
    let driven = bench.input(DCD, Signal::Level(true), now);
    assert!(levels(&driven).is_empty(), "IRQ already low for RDRF");
    // the receiver initialized: RDRF empty, the character gone
    assert_eq!(bench.read(STATUS, now), IRQ_BIT | DCD_BIT | TDRE);
    bench.input(DCD, Signal::Level(false), now);
    assert_eq!(bench.read(STATUS, now) & DCD_BIT, DCD_BIT, "latched");
    // a data read alone does not clear it
    let mut other = Bench::started(RUN | RIE);
    other.input(DCD, Signal::Level(true), Time::ZERO);
    other.input(DCD, Signal::Level(false), Time::ZERO);
    other.read(DATA, Time::ZERO);
    assert!(other.acia.interrupt_request());
    // after the status read, the data read clears it
    let driven_read = {
        let mut drive = Drive::new(now);
        bench
            .acia
            .read(Bench::access(DATA, Role::ReadWrite, now), &mut drive)
            .unwrap();
        drive.finish().driven
    };
    assert_eq!(levels(&driven_read), [(IRQ, true)]);
    assert_eq!(bench.read(STATUS, now), TDRE);
    // held high through the clearing reads: the bit follows the input,
    // and no interrupt
    bench.input(DCD, Signal::Level(true), now);
    bench.read(STATUS, now);
    bench.read(DATA, now);
    assert_eq!(bench.read(STATUS, now), DCD_BIT | TDRE);
    assert!(!bench.acia.interrupt_request());
    // frames are lost while it is high
    bench.input(RX_DATA, character(b'l'), now);
    let later = now + CHARACTER;
    bench.run(later);
    assert_eq!(bench.read(STATUS, later) & RDRF, 0);
    bench.input(DCD, Signal::Level(false), later);
    assert_eq!(bench.read(STATUS, later), TDRE);
    // a master reset clears the latch
    bench.input(DCD, Signal::Level(true), later);
    bench.input(DCD, Signal::Level(false), later);
    assert!(bench.acia.interrupt_request());
    bench.write(STATUS, RESET, later);
    bench.write(STATUS, RUN | RIE, later);
    assert_eq!(bench.read(STATUS, later), TDRE);
}

/// CTS high inhibits TDRE and shows in its status bit, masking the
/// transmit interrupt; it does not stop the transmitter (Peripheral/
/// Modem Control, Clear-to-Send; Status Register, Clear-to-Send).
#[test]
fn cts_high_inhibits_tdre() {
    let mut bench = Bench::started(RUN | TIE);
    assert!(bench.acia.interrupt_request());
    let driven = bench.input(CTS, Signal::Level(true), Time::ZERO);
    assert_eq!(levels(&driven), [(IRQ, true)]);
    assert_eq!(bench.read(STATUS, Time::ZERO), CTS_BIT);
    assert_eq!(frames(&bench.write(DATA, b'c', Time::ZERO)).len(), 1);
    let driven = bench.input(CTS, Signal::Level(false), Time::ZERO);
    assert_eq!(levels(&driven), [(IRQ, false)]);
    assert_eq!(bench.read(STATUS, Time::ZERO), IRQ_BIT | TDRE);
}

/// IRQ: CR7 with RDRF; CR6:5 `01` with TDRE, cleared by a data write
/// with the shift register busy; low while asserted, SR7 its state
/// (Interrupt Request; Transmitter Control Bits).
#[test]
fn the_interrupts_follow_their_enables() {
    let mut quiet = Bench::started(RUN);
    assert!(!quiet.acia.interrupt_request());
    for control in [RUN | RTS_HIGH, RUN | BREAK] {
        quiet.write(STATUS, control, Time::ZERO);
        assert!(!quiet.acia.interrupt_request(), "{control:#04x}");
    }
    let mut bench = Bench::started(RUN);
    let driven = bench.write(STATUS, RUN | TIE, Time::ZERO);
    assert_eq!(levels(&driven), [(IRQ, false)]);
    assert_eq!(bench.read(STATUS, Time::ZERO), IRQ_BIT | TDRE);
    // the first goes to the shift register, the second clears TDRE
    bench.write(DATA, b'1', Time::ZERO);
    assert!(bench.acia.interrupt_request());
    let driven = bench.write(DATA, b'2', Time::ZERO);
    assert_eq!(levels(&driven), [(IRQ, true)]);
    let driven = bench.run(Time(CHARACTER));
    assert_eq!(levels(&driven), [(IRQ, false)]);
    bench.write(STATUS, RUN, Time(CHARACTER));
    assert!(!bench.acia.interrupt_request());
    // the receiver's, cleared by the data read
    bench.write(STATUS, RUN | RIE, Time(CHARACTER));
    bench.input(RX_DATA, character(b'i'), Time(CHARACTER));
    let driven = bench.run(Time(2 * CHARACTER));
    assert_eq!(levels(&driven), [(IRQ, false)]);
    assert_eq!(
        bench.read(STATUS, Time(2 * CHARACTER)),
        IRQ_BIT | RDRF | TDRE
    );
    bench.read(DATA, Time(2 * CHARACTER));
    assert!(!bench.acia.interrupt_request());
    // the ACIA gives no vector
    let iack = Bench::access(0, Role::Iack, Time::ZERO);
    assert_eq!(
        bench.acia.read(iack, &mut Drive::new(Time::ZERO)),
        Err(BusError)
    );
    assert_eq!(bench.acia.wait_states(iack), None);
}

/// CR6:5: RTS low but with `10`; `11` sends a break, frames a character
/// long back to back after what the transmitter holds, until it is
/// cleared; then Tx Data is high a bit time (Transmitter Control Bits).
#[test]
fn the_transmitter_control_sets_rts_and_sends_a_break() {
    let mut bench = Bench::started(RUN);
    assert_eq!(
        levels(&bench.write(STATUS, RUN | RTS_HIGH, Time::ZERO)),
        [(RTS, true)]
    );
    assert_eq!(
        levels(&bench.write(STATUS, RUN | BREAK, Time::ZERO)),
        [(RTS, false)]
    );
    let sent = frames(&bench.run(Time(2 * CHARACTER + 1)));
    let breaks: Vec<_> = sent
        .iter()
        .map(|(at, serial)| (at.0, serial.frame))
        .collect();
    // the first break went out at the control write
    assert_eq!(
        breaks,
        [(CHARACTER, Frame::Break), (2 * CHARACTER, Frame::Break)]
    );
    bench.write(STATUS, RUN, Time(2 * CHARACTER + 1));
    bench.write(DATA, b'z', Time(2 * CHARACTER + 1));
    let sent = frames(&bench.run(Time(4 * CHARACTER)));
    let [(at, serial)] = sent[..] else {
        panic!("one character: {sent:?}");
    };
    assert_eq!(at, Time(3 * CHARACTER + BIT));
    assert_eq!(serial.frame, Frame::Data(u16::from(b'z')));
}

#[test]
fn it_shows_its_registers_and_lines() {
    let mut bench = Bench::started(RUN | RIE);
    bench.write(DATA, b'1', Time::ZERO);
    bench.write(DATA, b'2', Time::ZERO);
    let inspection = bench.acia.inspect();
    assert_eq!(
        inspection.get("control"),
        Some(&Value::Integer(i64::from(RUN | RIE)))
    );
    assert_eq!(
        inspection.get("tx_data"),
        Some(&Value::Integer(i64::from(b'2')))
    );
    assert_eq!(inspection.get("tx_shifting"), Some(&Value::Boolean(true)));
    assert_eq!(inspection.get("rts"), Some(&Value::Boolean(false)));
}

#[test]
fn a_snapshot_restores_mid_character() {
    let mut bench = Bench::started(RUN | RIE);
    bench.input(RX_DATA, character(b's'), Time::ZERO);
    let snapshot = bench.acia.save().unwrap();
    let mut other = Bench::new();
    other.acia.restore(&snapshot).unwrap();
    other.run(Time(CHARACTER));
    assert!(other.acia.interrupt_request());
    assert_eq!(other.read(DATA, Time(CHARACTER)), b's');
    let mut wrong = snapshot;
    wrong[0] ^= 1;
    assert!(other.acia.restore(&wrong).is_err());
}

#[test]
fn a_restore_refuses_other_clocks_and_a_state_no_run_holds() {
    let mut bench = Bench::started(RUN);
    bench.input(RX_DATA, character(b's'), Time::ZERO);
    let before = bench.acia.state.clone();
    let slow = Clock { hz: CLOCK_HZ / 2 };
    let fast = Clock { hz: CLOCK_HZ };
    for other in [Acia::new(slow, fast), Acia::new(fast, slow)] {
        assert!(matches!(
            bench.acia.restore(&other.save().unwrap()),
            Err(StateError::Mismatch(_))
        ));
    }
    let invalid = [
        State {
            overrun: Overrun::Pending,
            ..before.clone()
        },
        State {
            dcd_read: true,
            ..before.clone()
        },
        State {
            first_reset: true,
            ..before.clone()
        },
        State {
            control: RESET,
            holding: Some(1),
            ..before.clone()
        },
        State {
            full: true,
            errors: 0x01,
            ..before.clone()
        },
        State {
            driven: Some(0x04),
            ..before.clone()
        },
    ];
    for state in invalid {
        let snapshot = encode(VERSION, &(fast, fast, &state));
        assert!(
            matches!(bench.acia.restore(&snapshot), Err(StateError::Invalid(_))),
            "{state:?}"
        );
    }
    assert_eq!(bench.acia.state, before);
}

#[test]
fn the_format_matches_its_golden() {
    let mut bench = Bench::started(RUN | RIE | TIE);
    bench.write(DATA, b'g', Time::ZERO);
    bench.write(DATA, b'h', Time::ZERO);
    for byte in *b"ij" {
        bench.input(
            RX_DATA,
            Signal::Serial(Serial::untimed(Frame::Data(u16::from(byte)))),
            Time(3),
        );
    }
    bench.input(DCD, Signal::Level(true), Time(5));
    bench.input(DCD, Signal::Level(false), Time(6));
    bench.input(RX_DATA, character(b'k'), Time(7));
    bench.run(Time(CHARACTER + 5));
    let snapshot = bench.acia.save().unwrap();
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "mc6850",
        &[("mc6850", VERSION)],
        &snapshot,
    );
}

#[test]
fn a_word_cycle_carries_the_register_on_data_15_8() {
    let mut bench = Bench::new();
    let access = PeripheralAccess::new(
        ChipSelect::Boot,
        u32::from(STATUS),
        Size::Word,
        FunctionCode::SUPERVISOR_DATA,
        Time::ZERO,
    );
    let mut drive = Drive::new(Time::ZERO);
    bench.acia.write(access, 0x1500, &mut drive).unwrap();
    assert_eq!(
        bench.acia.state.control, RUN,
        "the upper byte (SIMRM 5.3.3)"
    );
    assert_eq!(
        bench.acia.read(access, &mut drive),
        Ok(u32::from(TDRE) << 8)
    );
}
