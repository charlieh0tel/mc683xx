//! The DUART's registers and functions, each against its datasheet
//! section.

use mc683xx_board::inspect::Inspection;
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
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::qsm::sci::Frame;

use super::Duart;
use super::IP0;
use super::IRQ;
use super::MC68681;
use super::OP0;
use super::RXDA;
use super::RXDB;
use super::SCC68692;
use super::State;
use super::TXDA;
use super::TXDB;
use super::VERSION;
use super::Variant;

/// Registers by number (MC68681 Table 4-1).
const MRA: u8 = 0x0;
/// SRA / CSRA.
const SRA: u8 = 0x1;
/// CRA.
const CRA: u8 = 0x2;
/// RBA / TBA.
const RBA: u8 = 0x3;
/// IPCR / ACR.
const ACR: u8 = 0x4;
/// ISR / IMR.
const ISR: u8 = 0x5;
/// CUR / CTUR.
const CTUR: u8 = 0x6;
/// CLR / CTLR.
const CTLR: u8 = 0x7;
/// MRB.
const MRB: u8 = 0x8;
/// SRB / CSRB.
const SRB: u8 = 0x9;
/// CRB.
const CRB: u8 = 0xa;
/// RBB / TBB.
const RBB: u8 = 0xb;
/// IVR.
const IVR: u8 = 0xc;
/// The input port / OPCR.
const OPCR: u8 = 0xd;
/// Start counter / set OPR bits.
const START: u8 = 0xe;
/// Stop counter / reset OPR bits.
const STOP: u8 = 0xf;

/// MR1: no parity, eight bits (Table 4-5).
const MR1_8N: u8 = 0x13;
/// MR2: normal, one stop bit.
const MR2_1_STOP: u8 = 0x07;
/// CSR: 9600 baud both ways, either set.
const CSR_9600: u8 = 0xbb;
/// CR: enable the transmitter and receiver.
const ENABLE: u8 = 0x05;

/// Picoseconds in a bit at 9600 baud from 3.6864 MHz: 384 ticks.
const BIT: u64 = 104_166_666;
/// Ten bits: start, eight data, one stop.
const CHARACTER: u64 = 1_041_666_666;
/// Start bit to the center of the stop bit: nine and a half bits.
const TO_STOP_CENTER: u64 = 989_583_333;

/// A DUART under test.
struct Bench {
    /// The peripheral.
    duart: Duart,
}

impl Bench {
    /// An MC68681.
    fn new() -> Self {
        Self::of(Variant::Mc68681)
    }

    /// A `variant` at 3.6864 MHz, powered on.
    fn of(variant: Variant) -> Self {
        let mut duart = Duart::new(variant, super::X1_DEFAULT_HZ);
        duart.power_on(&mut Drive::new(Time::ZERO));
        Self { duart }
    }

    /// The channel whose registers start at `base` set up: 8N1 at 9600,
    /// both enabled.
    fn channel(&mut self, base: u8) -> &mut Self {
        for (register, value) in [
            (CRA, 0x10),
            (MRA, MR1_8N),
            (MRA, MR2_1_STOP),
            (SRA, CSR_9600),
            (CRA, ENABLE),
        ] {
            self.write(base + register, value, Time::ZERO);
        }
        self
    }

    /// An access to `register` at `now` through `role`.
    fn access(register: u8, role: Role, now: Time) -> PeripheralAccess {
        let select = ChipSelect::Numbered(Number::new(8).unwrap());
        let function_code = if role == Role::Iack {
            FunctionCode::CPU_SPACE
        } else {
            FunctionCode::SUPERVISOR_DATA
        };
        PeripheralAccess::new(select, u32::from(register), Size::Byte, function_code, now).through(
            0,
            role,
            Lane::Lower,
        )
    }

    /// `register` written `value` at `now`: what it drove.
    fn write(&mut self, register: u8, value: u8, now: Time) -> Vec<Driven> {
        let mut drive = Drive::new(now);
        self.duart
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
        self.read_driving(register, now).0
    }

    /// `register` read at `now`, and what the read drove.
    fn read_driving(&mut self, register: u8, now: Time) -> (u8, Vec<Driven>) {
        let mut drive = Drive::new(now);
        let value = self
            .duart
            .read(Self::access(register, Role::ReadWrite, now), &mut drive)
            .unwrap();
        (value as u8, drive.finish().driven)
    }

    /// An interrupt acknowledge at `now`.
    fn acknowledge(&mut self, now: Time) -> Result<u32, BusError> {
        let access = Self::access(0, Role::Iack, now);
        self.duart.read(access, &mut Drive::new(now))
    }

    /// `signal` into `port` at `now`: what it drove.
    fn input(&mut self, port: usize, signal: Signal, now: Time) -> Vec<Driven> {
        let mut drive = Drive::new(now);
        self.duart.input(port, signal, now, &mut drive);
        drive.finish().driven
    }

    /// Advances through every event up to `until`: what it drove.
    fn run(&mut self, until: Time) -> Vec<Driven> {
        let mut driven = Vec::new();
        while let Some(next) = self.duart.next_event().filter(|&next| next <= until) {
            let mut drive = Drive::new(next);
            self.duart.advance(next, &mut drive);
            driven.extend(drive.finish().driven);
        }
        let mut drive = Drive::new(until);
        self.duart.advance(until, &mut drive);
        driven.extend(drive.finish().driven);
        driven
    }
}

/// The frames among `driven` on `port`, with their times.
fn frames(driven: &[Driven], port: usize) -> Vec<(Time, Serial)> {
    driven
        .iter()
        .filter(|driven| driven.port == port)
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

#[test]
fn both_peripherals_declare_the_pins_by_their_names() {
    let mut registry = Registry::new();
    crate::register(&mut registry).unwrap();
    for name in [MC68681, SCC68692] {
        let ports: Vec<(String, Kind, Direction)> = registry
            .get(name)
            .unwrap()
            .ports()
            .unwrap()
            .iter()
            .map(|port| (port.name.clone(), port.kind, port.direction))
            .collect();
        assert_eq!(
            ports[RXDA],
            ("RxDA".to_owned(), Kind::Serial, Direction::In)
        );
        assert_eq!(
            ports[TXDB],
            ("TxDB".to_owned(), Kind::Serial, Direction::Out)
        );
        assert_eq!(
            ports[IP0 + 5],
            ("IP5".to_owned(), Kind::Logic, Direction::In)
        );
        assert_eq!(
            ports[OP0 + 7],
            ("OP7".to_owned(), Kind::Logic, Direction::Out)
        );
        assert_eq!(ports[IRQ], ("IRQ".to_owned(), Kind::Logic, Direction::Out));
        assert_eq!(ports.len(), IRQ + 1);
    }
    assert!(crate::names().any(|name| name == SCC68692));
}

/// The peripheral `name` builds from `parameters`.
fn construct(name: &str, parameters: &str) -> Result<Box<dyn Peripheral>, ParameterError> {
    let mut registry = Registry::new();
    crate::register(&mut registry).unwrap();
    let parameters = Parameters::parse(parameters).unwrap();
    let environment = Environment::default();
    let context = Context::new("duart", &parameters, &environment);
    let peripheral = registry.get(name).unwrap().construct(&context)?.peripheral;
    if let Some(key) = parameters.unread() {
        return Err(ParameterError::Unknown(key));
    }
    Ok(peripheral)
}

#[test]
fn x1_clk_is_a_parameter_that_scales_every_rate() {
    assert!(construct(MC68681, "").is_ok());
    assert!(matches!(
        construct(MC68681, "x1_hz = 0"),
        Err(ParameterError::Invalid { .. })
    ));
    assert!(matches!(
        construct(SCC68692, "clock = 1"),
        Err(ParameterError::Unknown(_))
    ));
    // twice the crystal: code $B is 19200 baud
    let mut bench = Bench {
        duart: Duart::new(Variant::Mc68681, 2 * super::X1_DEFAULT_HZ),
    };
    bench.channel(0);
    let sent = frames(&bench.write(RBA, b'2', Time::ZERO), TXDA);
    assert_eq!(sent[0].1.baud, Frequency::from_hz(19_200));
    assert_eq!(sent[0].1.length, CHARACTER / 2);
}

#[test]
fn the_ivr_is_0f_out_of_reset_and_answers_an_acknowledge_only_while_interrupting() {
    let mut bench = Bench::new();
    assert_eq!(bench.read(IVR, Time::ZERO), 0x0f);
    bench.write(IVR, 0x40, Time::ZERO);
    assert_eq!(bench.read(IVR, Time::ZERO), 0x40);
    // no interrupt pending: the DUART ignores IACK (2.11)
    assert_eq!(bench.acknowledge(Time::ZERO), Err(BusError));
    let iack = Bench::access(0, Role::Iack, Time::ZERO);
    assert_eq!(bench.duart.wait_states(iack), None);
    bench.channel(0);
    bench.write(ISR, 0x01, Time::ZERO);
    assert_eq!(bench.acknowledge(Time::ZERO), Ok(0x40));
    assert_eq!(bench.duart.wait_states(iack), Some(0));
}

#[test]
fn the_mode_register_pointer_moves_to_mr2_and_back_on_its_command() {
    let mut bench = Bench::new();
    bench.write(MRB, 0x93, Time::ZERO);
    bench.write(MRB, 0x17, Time::ZERO);
    bench.write(MRB, 0x27, Time::ZERO);
    // the pointer stays at MR2 (4.1)
    assert_eq!(bench.read(MRB, Time::ZERO), 0x27);
    bench.write(CRB, 0x10, Time::ZERO);
    assert_eq!(bench.read(MRB, Time::ZERO), 0x93);
    assert_eq!(bench.read(MRB, Time::ZERO), 0x27);
    // channel A's is its own
    assert_eq!(bench.read(MRA, Time::ZERO), 0x00);
}

#[test]
fn a_character_goes_out_at_its_write_and_the_next_waits_in_the_holding_register() {
    let mut bench = Bench::new();
    bench.channel(MRB);
    assert_eq!(bench.read(SRB, Time::ZERO) & 0x0c, 0x0c, "TxRDY and TxEMT");
    let sent = frames(&bench.write(RBB, b'p', Time(1000)), TXDB);
    assert_eq!(
        sent,
        [(
            Time(1000),
            Serial::timed(
                Frame::Data(u16::from(b'p')),
                Frequency::from_hz(9600),
                CHARACTER
            )
        )]
    );
    assert_eq!(bench.read(SRB, Time(1000)) & 0x0c, 0x04, "TxRDY, not TxEMT");
    bench.write(RBB, b'q', Time(2000));
    assert_eq!(bench.read(SRB, Time(2000)) & 0x0c, 0x00, "holding full");
    let sent = frames(&bench.run(Time(1000 + 2 * CHARACTER)), TXDB);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, Time(1000 + CHARACTER), "back to back");
    assert_eq!(
        bench.read(SRB, Time(1000 + 2 * CHARACTER)) & 0x0c,
        0x0c,
        "empty"
    );
}

#[test]
fn a_disabled_transmitter_takes_no_character_but_finishes_its_own() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.write(RBA, b'a', Time::ZERO);
    bench.write(RBA, b'b', Time::ZERO);
    bench.write(CRA, 0x08, Time::ZERO);
    assert_eq!(
        bench.read(SRA, Time::ZERO) & 0x0c,
        0,
        "TxRDY and TxEMT clear"
    );
    bench.write(RBA, b'c', Time::ZERO);
    let sent = frames(&bench.run(Time(4 * CHARACTER)), TXDA);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.frame, Frame::Data(u16::from(b'b')));
}

#[test]
fn parity_and_character_length_shape_the_frame() {
    let mut bench = Bench::new();
    bench.channel(0);
    // seven bits, odd parity; 1.5 stop bits (MR2 code 8, 25/16 bits)
    bench.write(CRA, 0x10, Time::ZERO);
    bench.write(MRA, 0x06, Time::ZERO);
    bench.write(MRA, 0x08, Time::ZERO);
    let sent = frames(&bench.write(RBA, 0x41, Time::ZERO), TXDA);
    // 0x41 has two ones: odd parity sets bit 7
    assert_eq!(sent[0].1.frame, Frame::Data(0xc1));
    // start, seven data, parity: 144 sixteenths; and 25: 4056 ticks
    assert_eq!(sent[0].1.length, 1_100_260_416);
    // forced low parity, five bits: 1 1/16 stop bits at code 0
    bench.write(CRA, 0x10, Time::ZERO);
    bench.write(MRA, 0x08, Time::ZERO);
    bench.write(MRA, 0x00, Time::ZERO);
    let sent = frames(&bench.run(Time(10 * CHARACTER)), TXDA);
    assert!(sent.is_empty());
    let sent = frames(&bench.write(RBA, 0xff, Time(10 * CHARACTER)), TXDA);
    assert_eq!(sent[0].1.frame, Frame::Data(0x1f));
}

#[test]
fn the_receiver_checks_parity() {
    let mut bench = Bench::new();
    bench.channel(0);
    // eight bits, even parity
    bench.write(CRA, 0x10, Time::ZERO);
    bench.write(MRA, 0x03, Time::ZERO);
    let frame = |bits| Signal::Serial(Serial::new(Frame::Data(bits), Frequency::from_hz(9600), 11));
    bench.input(RXDA, frame(0x101), Time::ZERO);
    bench.run(Time(2 * CHARACTER));
    assert_eq!(
        bench.read(SRA, Time(2 * CHARACTER)) & 0x21,
        0x01,
        "even: no error"
    );
    assert_eq!(bench.read(RBA, Time(2 * CHARACTER)), 0x01);
    bench.input(RXDA, frame(0x001), Time(2 * CHARACTER));
    bench.run(Time(4 * CHARACTER));
    assert_eq!(
        bench.read(SRA, Time(4 * CHARACTER)) & 0x21,
        0x21,
        "parity error"
    );
    // reset error status clears it (4.2.7.2)
    bench.write(CRA, 0x40, Time(4 * CHARACTER));
    assert_eq!(bench.read(SRA, Time(4 * CHARACTER)) & 0x21, 0x01);
}

#[test]
fn a_character_arrives_at_the_center_of_its_stop_bit() {
    let mut bench = Bench::new();
    bench.channel(MRB);
    let start = Time(5000);
    bench.input(RXDB, character(b'x'), start);
    assert_eq!(bench.duart.next_event(), Some(start + TO_STOP_CENTER));
    bench.run(start + TO_STOP_CENTER - 1);
    assert_eq!(bench.read(SRB, start) & 0x01, 0, "on the line");
    bench.run(start + TO_STOP_CENTER);
    assert_eq!(
        bench.read(SRB, start + TO_STOP_CENTER) & 0x01,
        0x01,
        "RxRDY"
    );
    assert_eq!(bench.read(RBB, start + TO_STOP_CENTER), b'x');
    assert_eq!(bench.read(SRB, start + TO_STOP_CENTER) & 0x01, 0);
}

/// The group `name` of `inspection`.
fn group<'a>(inspection: &'a Inspection, name: &str) -> &'a Inspection {
    match inspection.get(name) {
        Some(Value::Group(group)) => group,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn it_shows_each_channel_s_fifo_and_its_registers() {
    let mut bench = Bench::new();
    bench.channel(MRB);
    bench.write(ISR, 0x20, Time::ZERO);
    let start = Time(5000);
    bench.input(RXDB, character(b'x'), start);
    bench.run(start + TO_STOP_CENTER);
    let inspection = bench.duart.inspect();
    assert_eq!(
        inspection.get("variant"),
        Some(&Value::Text(MC68681.to_owned()))
    );
    let b = group(&inspection, "b");
    assert_eq!(b.get("rx_fifo"), Some(&Value::Bytes(b"x".to_vec())));
    assert_eq!(b.get("receiving"), Some(&Value::Boolean(true)));
    assert_eq!(b.get("mr1"), Some(&Value::Integer(i64::from(MR1_8N))));
    let a = group(&inspection, "a");
    assert_eq!(a.get("rx_fifo"), Some(&Value::Bytes(Vec::new())));
    assert_eq!(inspection.get("imr"), Some(&Value::Integer(0x20)));
    assert_eq!(inspection.get("irq"), Some(&Value::Boolean(true)), "RxRDYB");
    let counter = group(&inspection, "counter");
    assert_eq!(counter.get("preload"), Some(&Value::Integer(0)));
    assert_eq!(group(&inspection, "outputs").fields.len(), 8);
}

#[test]
fn a_character_past_the_fifo_and_the_shift_register_overruns() {
    let mut bench = Bench::new();
    bench.channel(MRB);
    for byte in *b"abcdef" {
        bench.input(
            RXDB,
            Signal::Serial(Serial::untimed(Frame::Data(u16::from(byte)))),
            Time::ZERO,
        );
    }
    let end = Time(6 * CHARACTER);
    bench.run(end);
    // a, b and c in the FIFO; d held in the shift register and lost to
    // e's start bit, e to f's: f held (3.2)
    assert_eq!(bench.read(SRB, end) & 0x13, 0x13, "RxRDY, FFULL and OE");
    let read: Vec<u8> = (0..4).map(|_| bench.read(RBB, end)).collect();
    assert_eq!(read, b"abcf");
    assert_eq!(bench.read(SRB, end) & 0x10, 0x10, "OE until reset");
    bench.write(CRB, 0x40, end);
    assert_eq!(bench.read(SRB, end) & 0x10, 0);
    // an empty FIFO gives the last character again
    assert_eq!(bench.read(RBB, end), b'f');
}

#[test]
fn a_character_arriving_while_the_receiver_is_disabled_is_lost() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.write(CRA, 0x02, Time::ZERO);
    bench.input(RXDA, character(b'x'), Time::ZERO);
    bench.write(CRA, 0x01, Time(BIT));
    bench.run(Time(4 * CHARACTER));
    assert_eq!(bench.read(SRA, Time(4 * CHARACTER)) & 0x01, 0);
    assert_eq!(bench.duart.next_event(), None);
}

#[test]
fn local_loopback_receives_with_the_receiver_disabled_and_sends_nothing() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.write(CRA, 0x10, Time::ZERO);
    let _ = bench.read(MRA, Time::ZERO);
    bench.write(MRA, 0x87, Time::ZERO);
    bench.write(CRA, 0x02, Time::ZERO);
    let mut driven = bench.write(RBA, b'L', Time::ZERO);
    // the line's characters are ignored (4.2.2.1)
    driven.extend(bench.input(RXDA, character(b'x'), Time::ZERO));
    driven.extend(bench.run(Time(3 * CHARACTER)));
    assert!(frames(&driven, TXDA).is_empty(), "TxDA held high");
    assert_eq!(bench.read(SRA, Time(3 * CHARACTER)) & 0x01, 0x01);
    assert_eq!(bench.read(RBA, Time(3 * CHARACTER)), b'L');
    assert_eq!(
        bench.read(SRA, Time(3 * CHARACTER)) & 0x01,
        0,
        "nothing else"
    );
}

#[test]
fn automatic_echo_retransmits_what_it_receives_and_cuts_off_the_cpu() {
    let mut bench = Bench::new();
    bench.channel(MRB);
    bench.write(CRB, 0x10, Time::ZERO);
    let _ = bench.read(MRB, Time::ZERO);
    bench.write(MRB, 0x47, Time::ZERO);
    assert_eq!(
        bench.read(SRB, Time::ZERO) & 0x0c,
        0,
        "TxRDY and TxEMT inactive"
    );
    let echoed = frames(&bench.input(RXDB, character(b'e'), Time(7)), TXDB);
    assert_eq!(
        echoed,
        [(
            Time(7),
            Serial::timed(
                Frame::Data(u16::from(b'e')),
                Frequency::from_hz(9600),
                CHARACTER
            )
        )]
    );
    bench.run(Time(2 * CHARACTER));
    assert_eq!(bench.read(RBB, Time(2 * CHARACTER)), b'e', "received too");
    // remote loopback: echoed, not received
    let _ = bench.read(MRB, Time::ZERO);
    bench.write(MRB, 0xc7, Time(2 * CHARACTER));
    let echoed = frames(
        &bench.input(RXDB, character(b'r'), Time(2 * CHARACTER)),
        TXDB,
    );
    assert_eq!(echoed.len(), 1);
    bench.run(Time(4 * CHARACTER));
    assert_eq!(bench.read(SRB, Time(4 * CHARACTER)) & 0x01, 0);
}

#[test]
fn the_mask_passes_status_to_irq_which_is_driven_low() {
    let mut bench = Bench::new();
    bench.channel(MRB);
    assert!(!bench.duart.interrupt_request());
    let driven = levels(&bench.write(ISR, 0x10, Time::ZERO));
    assert_eq!(driven, [(IRQ, false)], "TxRDYB passed");
    assert_eq!(bench.read(ISR, Time::ZERO), 0x10, "TxRDYB; A disabled");
    assert!(bench.duart.interrupt_request());
    let driven = levels(&bench.write(RBB, b'p', Time::ZERO));
    assert!(driven.is_empty(), "the shifter took it: TxRDY still");
    let driven = levels(&bench.write(RBB, b'q', Time::ZERO));
    assert_eq!(driven, [(IRQ, true)]);
    // receiver ready, or FIFO full as MR1[6] selects (4.2.1.2)
    bench.write(ISR, 0x20, Time::ZERO);
    bench.write(CRB, 0x10, Time::ZERO);
    bench.write(MRB, 0x53, Time::ZERO);
    for byte in *b"xy" {
        bench.input(
            RXDB,
            Signal::Serial(Serial::untimed(Frame::Data(u16::from(byte)))),
            Time::ZERO,
        );
    }
    bench.run(Time(3 * CHARACTER));
    assert_eq!(
        bench.read(ISR, Time(3 * CHARACTER)) & 0x20,
        0,
        "two: not full"
    );
    bench.input(RXDB, character(b'z'), Time(3 * CHARACTER));
    bench.run(Time(5 * CHARACTER));
    assert_eq!(bench.read(ISR, Time(5 * CHARACTER)) & 0x20, 0x20, "FFULLB");
    assert!(bench.duart.interrupt_request());
}

#[test]
fn a_break_is_one_zero_character_and_a_change_at_each_end() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.write(ISR, 0x04, Time::ZERO);
    let brk = Signal::Serial(Serial::new(Frame::Break, Frequency::from_hz(9600), 10));
    // three character times of break: one character
    for at in 0..3 {
        bench.input(RXDA, brk, Time(at * CHARACTER));
        bench.run(Time(at * CHARACTER + TO_STOP_CENTER));
    }
    let now = Time(2 * CHARACTER + TO_STOP_CENTER);
    assert_eq!(bench.read(SRA, now) & 0x81, 0x81, "RB and RxRDY");
    assert_eq!(bench.read(ISR, now) & 0x04, 0x04, "change in break");
    bench.write(CRA, 0x50, now);
    assert_eq!(bench.read(ISR, now) & 0x04, 0, "reset");
    bench.run(Time(3 * CHARACTER));
    assert_eq!(bench.read(ISR, Time(3 * CHARACTER)) & 0x04, 0x04, "the end");
    assert_eq!(bench.read(RBA, Time(3 * CHARACTER)), 0);
    assert_eq!(
        bench.read(SRA, Time(3 * CHARACTER)) & 0x01,
        0,
        "one character"
    );
}

#[test]
fn a_commanded_break_goes_out_as_break_frames_until_stopped() {
    let mut bench = Bench::new();
    bench.channel(0);
    let mut driven = bench.write(CRA, 0x60, Time::ZERO);
    driven.extend(bench.run(Time(2 * CHARACTER)));
    driven.extend(bench.write(CRA, 0x70, Time(2 * CHARACTER)));
    driven.extend(bench.write(RBA, b'z', Time(2 * CHARACTER)));
    driven.extend(bench.run(Time(5 * CHARACTER)));
    let sent: Vec<(Time, Frame)> = frames(&driven, TXDA)
        .into_iter()
        .map(|(at, serial)| (at, serial.frame))
        .collect();
    assert_eq!(
        sent,
        [
            (Time(0), Frame::Break),
            (Time(CHARACTER), Frame::Break),
            (Time(2 * CHARACTER), Frame::Break),
            // a bit time high after the break (4.2.7.2)
            (Time(3 * CHARACTER + BIT), Frame::Data(u16::from(b'z'))),
        ]
    );
}

/// The count registers read at `now`.
fn count(bench: &mut Bench, now: Time) -> u16 {
    u16::from_be_bytes([bench.read(CTUR, now), bench.read(CTLR, now)])
}

#[test]
fn the_counter_counts_down_from_its_start_to_terminal_count() {
    let mut bench = Bench::new();
    // counter mode, X1/16: 230.4 kHz (Table 4-4)
    bench.write(ACR, 0x30, Time::ZERO);
    bench.write(CTUR, 0x00, Time::ZERO);
    bench.write(CTLR, 0x64, Time::ZERO);
    bench.write(ISR, 0x08, Time::ZERO);
    let _ = bench.read(START, Time::ZERO);
    // 16 ticks a count: 40 counts
    let tick = 1_000_000_000_000 / 3_686_400;
    assert_eq!(count(&mut bench, Time(640 * tick + 300)), 60);
    let terminal = bench.duart.next_event().unwrap();
    assert_eq!(
        terminal,
        Time((1600 * 1_000_000_000_000_u64).div_ceil(3_686_400))
    );
    bench.run(terminal);
    assert_eq!(bench.read(ISR, terminal) & 0x08, 0x08, "ISR[3]");
    assert!(bench.duart.interrupt_request());
    // it rolls over to $FFFF and counts on (3.5.1)
    assert_eq!(count(&mut bench, terminal + 16 * tick + 300), 0xffff);
    let _ = bench.read(STOP, terminal + 32 * tick + 300);
    assert_eq!(bench.read(ISR, terminal) & 0x08, 0, "stop clears it");
    assert_eq!(
        count(&mut bench, Time(1_000_000_000_000)),
        0xfffe,
        "stopped"
    );
}

#[test]
fn the_timer_sets_its_ready_bit_every_second_terminal_count_and_squares_op3() {
    let mut bench = Bench::new();
    // timer mode on X1/16, preload 4: a terminal count every 64 ticks
    bench.write(ACR, 0x70, Time::ZERO);
    bench.write(CTLR, 4, Time::ZERO);
    bench.write(OPCR, 0x04, Time::ZERO);
    bench.write(ISR, 0x08, Time::ZERO);
    let (_, mut driven) = bench.read_driving(START, Time::ZERO);
    let tick = |n: u64| Time((n * 1_000_000_000_000).div_ceil(3_686_400));
    driven.extend(bench.run(tick(64 * 4)));
    let edges: Vec<(Time, bool)> = driven
        .into_iter()
        .filter(|driven| driven.port == OP0 + 3)
        .map(|driven| (driven.at, driven.signal == Signal::Level(true)))
        .collect();
    // the start inverted it low; each terminal count inverts it
    assert_eq!(
        edges,
        [
            (Time::ZERO, false),
            (tick(64), true),
            (tick(128), false),
            (tick(192), true),
            (tick(256), false)
        ]
    );
    assert_eq!(bench.read(ISR, tick(256)) & 0x08, 0x08);
    let _ = bench.read(STOP, tick(256));
    assert_eq!(
        bench.read(ISR, tick(256)) & 0x08,
        0,
        "cleared; the timer runs on"
    );
    bench.run(tick(64 * 5));
    assert_eq!(
        bench.read(ISR, tick(64 * 5)) & 0x08,
        0,
        "the first terminal count"
    );
    bench.run(tick(64 * 6));
    assert_eq!(bench.read(ISR, tick(64 * 6)) & 0x08, 0x08, "the second");
}

#[test]
fn the_timer_is_a_16x_clock_for_a_channel() {
    let mut bench = Bench::new();
    bench.channel(0);
    // timer on X1, preload 12: a 16X clock of X1 / 24, 9600 baud
    bench.write(ACR, 0x60, Time::ZERO);
    bench.write(CTLR, 12, Time::ZERO);
    bench.write(SRA, 0xdd, Time::ZERO);
    assert!(
        frames(&bench.write(RBA, b't', Time::ZERO), TXDA).is_empty(),
        "not started"
    );
    let _ = bench.read(START, Time::ZERO);
    let sent = frames(&bench.run(Time(1)), TXDA);
    assert_eq!(sent[0].1.baud, Frequency::from_hz(9600));
}

#[test]
fn the_output_port_shows_the_complement_of_its_register() {
    let mut bench = Bench::new();
    // power-on drove every pin high and IRQ high
    let mut fresh = Duart::new(Variant::Mc68681, super::X1_DEFAULT_HZ);
    let mut drive = Drive::new(Time::ZERO);
    fresh.power_on(&mut drive);
    let first = levels(&drive.finish().driven);
    assert_eq!(first.len(), 9);
    assert!(first.iter().all(|&(_, level)| level));
    assert_eq!(
        levels(&bench.write(START, 0x05, Time::ZERO)),
        [(OP0, false), (OP0 + 2, false)]
    );
    assert_eq!(levels(&bench.write(STOP, 0x01, Time::ZERO)), [(OP0, true)]);
    // OP4 as channel A's receiver interrupt: open drain, low while
    // RxRDYA, whatever the mask (4.2.11.4)
    bench.channel(0);
    assert!(levels(&bench.write(OPCR, 0x10, Time::ZERO)).is_empty());
    bench.input(RXDA, character(b'4'), Time::ZERO);
    let driven = levels(&bench.run(Time(CHARACTER)));
    assert_eq!(driven, [(OP0 + 4, false)]);
    assert_eq!(
        levels(&bench.write(OPCR, 0x50, Time(CHARACTER))),
        [(OP0 + 6, false)]
    );
}

#[test]
fn rts_follows_the_receivers_fifo_and_the_transmitters_disable() {
    let mut bench = Bench::new();
    bench.channel(0);
    // MR1[7], RxRTS; MR2[5], TxRTS
    bench.write(CRA, 0x10, Time::ZERO);
    bench.write(MRA, 0x93, Time::ZERO);
    bench.write(MRA, 0x27, Time::ZERO);
    bench.write(START, 0x01, Time::ZERO);
    for byte in *b"abcd" {
        bench.input(
            RXDA,
            Signal::Serial(Serial::untimed(Frame::Data(u16::from(byte)))),
            Time::ZERO,
        );
    }
    let driven = bench.run(Time(3 * CHARACTER + BIT));
    assert_eq!(levels(&driven), [(OP0, true)], "negated at d's start bit");
    let (_, driven) = bench.read_driving(RBA, Time(4 * CHARACTER));
    assert!(levels(&driven).is_empty(), "d took the room");
    let _ = bench.read(RBA, Time(4 * CHARACTER));
    assert_eq!(bench.read(SRA, Time(4 * CHARACTER)) & 0x02, 0);
    // the transmitter: OPR[0] cleared a bit time after its last character
    let at = Time(10 * CHARACTER);
    bench.write(RBA, b'!', at);
    bench.write(CRA, 0x08, at);
    let driven = levels(&bench.run(at + 2 * CHARACTER));
    assert_eq!(driven, [(OP0, true)]);
    assert_eq!(bench.duart.state.opr & 1, 0);
}

#[test]
fn cts_holds_the_transmitter_until_it_goes_low() {
    let mut bench = Bench::new();
    bench.channel(MRB);
    bench.write(CRB, 0x10, Time::ZERO);
    let _ = bench.read(MRB, Time::ZERO);
    bench.write(MRB, 0x17, Time::ZERO);
    // IP1 is CTSB; high from power-on
    assert!(frames(&bench.write(RBB, b'c', Time::ZERO), TXDB).is_empty());
    assert_eq!(bench.duart.next_event(), None, "nothing to wait for");
    let sent = frames(&bench.input(IP0 + 1, Signal::Level(false), Time(77)), TXDB);
    assert_eq!(sent[0].0, Time(77));
}

#[test]
fn the_input_port_reads_its_pins_and_its_detectors_take_25_to_50_microseconds() {
    let mut bench = Bench::new();
    assert_eq!(
        bench.read(OPCR, Time::ZERO),
        0xff,
        "D7, IACK high, pins high"
    );
    bench.write(ACR, 0x04, Time::ZERO);
    bench.write(ISR, 0x80, Time::ZERO);
    bench.input(IP0 + 2, Signal::Level(false), Time(1_000_000));
    assert_eq!(bench.read(OPCR, Time(1_000_000)), 0xfb, "unlatched");
    assert_eq!(bench.read(ACR, Time(1_000_000)), 0x0b, "not yet a change");
    let recognized = bench.duart.next_event().unwrap();
    // two samples 96 ticks apart, 26.04 microseconds
    assert!((26_000_000..=52_100_000).contains(&(recognized.0 - 1_000_000)));
    bench.run(recognized);
    assert!(bench.duart.interrupt_request(), "ISR[7]");
    assert_eq!(bench.read(ACR, recognized), 0x4b, "IP2 changed");
    assert!(
        !bench.duart.interrupt_request(),
        "reading IPCR clears ISR[7]"
    );
    assert_eq!(bench.read(ACR, recognized), 0x0b);
    // a pulse shorter than a sample period is not seen
    bench.input(IP0 + 2, Signal::Level(true), Time(100_000_000));
    bench.input(IP0 + 2, Signal::Level(false), Time(101_000_000));
    assert_eq!(bench.duart.next_event(), None);
    // an IP4 change has no detector
    bench.input(IP0 + 4, Signal::Level(false), Time(200_000_000));
    assert_eq!(bench.duart.next_event(), None);
    assert_eq!(bench.read(OPCR, Time(200_000_000)), 0xeb);
}

#[test]
fn the_scc68692_reads_iack_not_ip2_in_bit_6() {
    let mut bench = Bench::of(Variant::Scc68692);
    assert_eq!(bench.read(OPCR, Time::ZERO), 0xff);
    bench.input(IP0 + 2, Signal::Level(false), Time::ZERO);
    assert_eq!(bench.read(OPCR, Time::ZERO), 0xfb, "IACK high, IP2 low");
}

#[test]
fn reset_clears_the_mask_the_output_port_and_the_channels() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.write(ISR, 0x01, Time::ZERO);
    bench.write(IVR, 0x55, Time::ZERO);
    bench.write(START, 0xff, Time::ZERO);
    bench.write(ACR, 0x30, Time::ZERO);
    let mut drive = Drive::new(Time(10));
    bench.duart.reset(Time(10), &mut drive);
    let driven = levels(&drive.finish().driven);
    assert_eq!(driven.len(), 9, "every pin high, IRQ negated");
    assert!(driven.iter().all(|&(_, level)| level));
    assert_eq!(bench.read(IVR, Time(10)), 0x0f);
    assert_eq!(bench.read(SRA, Time(10)), 0, "inactive");
    assert_eq!(bench.read(ISR, Time(10)), 0);
    assert_eq!(
        bench.read(MRA, Time(10)),
        MR1_8N,
        "the pointer at MR1; modes kept"
    );
    // timer mode (2.4)
    assert_eq!(bench.duart.state.acr, 0x70);
}

#[test]
fn the_scc68692s_extra_commands_are_the_mc68681s_with_bit_7_ignored() {
    // $80: assert RTSN on the SCC68692; no command on the MC68681
    let mut scc = Bench::of(Variant::Scc68692);
    assert_eq!(levels(&scc.write(CRA, 0x80, Time::ZERO)), [(OP0, false)]);
    assert_eq!(
        levels(&scc.write(CRB, 0x80, Time::ZERO)),
        [(OP0 + 1, false)]
    );
    assert_eq!(levels(&scc.write(CRA, 0x90, Time::ZERO)), [(OP0, true)]);
    let mut mc = Bench::new();
    assert!(levels(&mc.write(CRA, 0x80, Time::ZERO)).is_empty());
    // $A0: timeout mode on the SCC68692, reset receiver on the MC68681
    mc.channel(0);
    mc.input(RXDA, character(b'k'), Time::ZERO);
    mc.run(Time(CHARACTER));
    mc.write(CRA, 0xa0, Time(CHARACTER));
    assert_eq!(mc.read(SRA, Time(CHARACTER)) & 0x01, 0, "the FIFO reset");
}

#[test]
fn the_scc68692s_timeout_mode_restarts_the_counter_at_each_character() {
    let mut bench = Bench::of(Variant::Scc68692);
    bench.channel(0);
    // counter on X1/16, preload 300: 1.302 ms, past a character time
    bench.write(ACR, 0x30, Time::ZERO);
    bench.write(CTUR, 0x01, Time::ZERO);
    bench.write(CTLR, 0x2c, Time::ZERO);
    bench.write(ISR, 0x08, Time::ZERO);
    bench.write(CRA, 0xa0, Time::ZERO);
    assert_eq!(bench.duart.next_event(), None, "stopped until a character");
    bench.input(RXDA, character(b'1'), Time::ZERO);
    bench.input(RXDA, character(b'2'), Time::ZERO);
    let second = Time(CHARACTER + TO_STOP_CENTER);
    bench.run(second);
    assert!(!bench.duart.interrupt_request());
    let timeout = bench.duart.next_event().unwrap();
    let span = timeout.0 - second.0;
    assert!((1_301_000_000..1_303_000_000).contains(&span), "{span}");
    bench.run(timeout);
    assert!(bench.duart.interrupt_request(), "ISR[3]: the line paused");
    // start and stop commands are disabled
    let _ = bench.read(STOP, timeout);
    assert!(bench.duart.interrupt_request());
    bench.write(CRA, 0xc0, timeout);
    let _ = bench.read(STOP, timeout);
    assert!(!bench.duart.interrupt_request());
}

#[test]
fn multidrop_takes_address_characters_with_the_receiver_disabled() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.write(CRA, 0x10, Time::ZERO);
    bench.write(MRA, 0x1b, Time::ZERO);
    bench.write(CRA, 0x02, Time::ZERO);
    let frame = |bits| Signal::Serial(Serial::untimed(Frame::Data(bits)));
    // data, then an address: the A/D bit after the eight data bits
    bench.input(RXDA, frame(0x033), Time::ZERO);
    bench.input(RXDA, frame(0x1a5), Time::ZERO);
    bench.run(Time(3 * CHARACTER));
    assert_eq!(
        bench.read(SRA, Time(3 * CHARACTER)) & 0x21,
        0x21,
        "A/D in PE"
    );
    assert_eq!(bench.read(RBA, Time(3 * CHARACTER)), 0xa5);
    assert_eq!(bench.read(SRA, Time(3 * CHARACTER)) & 0x01, 0);
    // the transmitter sends MR1[2] as the A/D bit
    bench.write(CRA, 0x10, Time(3 * CHARACTER));
    bench.write(MRA, 0x1f, Time(3 * CHARACTER));
    bench.write(CRA, 0x05, Time(3 * CHARACTER));
    let sent = frames(&bench.write(RBA, 0x42, Time(3 * CHARACTER)), TXDA);
    assert_eq!(sent[0].1.frame, Frame::Data(0x142));
}

#[test]
fn block_error_mode_keeps_every_characters_status_until_reset() {
    let mut bench = Bench::new();
    bench.channel(0);
    // block mode, even parity
    bench.write(CRA, 0x10, Time::ZERO);
    bench.write(MRA, 0x23, Time::ZERO);
    let frame = |bits| Signal::Serial(Serial::untimed(Frame::Data(bits)));
    bench.input(RXDA, frame(0x001), Time::ZERO);
    bench.input(RXDA, frame(0x101), Time::ZERO);
    bench.run(Time(3 * CHARACTER));
    let _ = bench.read(RBA, Time(3 * CHARACTER));
    assert_eq!(
        bench.read(SRA, Time(3 * CHARACTER)) & 0x21,
        0x21,
        "the first's error"
    );
    bench.write(CRA, 0x40, Time(3 * CHARACTER));
    assert_eq!(bench.read(SRA, Time(3 * CHARACTER)) & 0x21, 0x01);
}

#[test]
fn a_snapshot_restores_mid_character() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.write(ISR, 0x02, Time::ZERO);
    bench.input(RXDA, character(b's'), Time::ZERO);
    let snapshot = bench.duart.save().unwrap();
    let mut other = Bench::new();
    other.duart.restore(&snapshot).unwrap();
    other.run(Time(CHARACTER));
    assert!(other.duart.interrupt_request());
    assert_eq!(other.read(RBA, Time(CHARACTER)), b's');
    let mut wrong = snapshot;
    wrong[0] ^= 1;
    assert!(other.duart.restore(&wrong).is_err());
}

#[test]
fn a_restore_refuses_another_peripheral_and_a_state_no_run_holds() {
    let mut bench = Bench::new();
    bench.channel(0);
    bench.input(RXDA, character(b's'), Time::ZERO);
    let before = bench.duart.state.clone();
    let others = [
        Duart::new(Variant::Scc68692, super::X1_DEFAULT_HZ),
        Duart::new(Variant::Mc68681, 2 * super::X1_DEFAULT_HZ),
    ];
    for other in others {
        assert!(matches!(
            bench.duart.restore(&other.save().unwrap()),
            Err(StateError::Mismatch(_))
        ));
    }
    let timeout = State {
        timeout: Some(0),
        ..before.clone()
    };
    let past_ip5 = State {
        inputs: 0x7f,
        ..before.clone()
    };
    let unchanged = State {
        pending: [Some(Time(1)), None, None, None],
        ..before.clone()
    };
    for state in [timeout, past_ip5, unchanged] {
        let snapshot = encode(VERSION, &(Variant::Mc68681, super::X1_DEFAULT_HZ, &state));
        assert!(matches!(
            bench.duart.restore(&snapshot),
            Err(StateError::Invalid(_))
        ));
    }
    assert_eq!(bench.duart.state, before);
}

#[test]
fn the_format_matches_its_golden() {
    let mut bench = Bench::of(Variant::Scc68692);
    bench.channel(0).channel(MRB);
    bench.write(ACR, 0x70, Time::ZERO);
    bench.write(CTLR, 9, Time::ZERO);
    bench.write(IVR, 0x41, Time::ZERO);
    bench.write(ISR, 0x22, Time::ZERO);
    bench.write(START, 0x0c, Time::ZERO);
    let _ = bench.read(START, Time::ZERO);
    bench.write(RBA, b'g', Time::ZERO);
    bench.write(RBA, b'h', Time::ZERO);
    for byte in *b"ijkl" {
        bench.input(
            RXDB,
            Signal::Serial(Serial::untimed(Frame::Data(u16::from(byte)))),
            Time(3),
        );
    }
    bench.input(IP0, Signal::Level(false), Time(5));
    bench.run(Time(2 * CHARACTER));
    let snapshot = bench.duart.save().unwrap();
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "mc68681",
        &[("mc68681", VERSION)],
        &snapshot,
    );
}

#[test]
fn a_word_cycle_through_both_lanes_carries_the_register_on_data_15_8() {
    let mut bench = Bench::new();
    let access = PeripheralAccess::new(
        ChipSelect::Boot,
        u32::from(IVR),
        Size::Word,
        FunctionCode::SUPERVISOR_DATA,
        Time::ZERO,
    );
    let mut drive = Drive::new(Time::ZERO);
    bench.duart.write(access, 0x4142, &mut drive).unwrap();
    assert_eq!(bench.duart.state.ivr, 0x41, "the upper byte (SIMRM 5.3.3)");
    assert_eq!(bench.duart.read(access, &mut drive), Ok(0x4100));
}

/// RESET puts the MC68681's counter/timer in timer mode, which runs
/// continuously (2.4, 3.5); the SCC68692's is stopped until a start
/// command (RESETN; Counter/Timer).
#[test]
fn reset_runs_the_mc68681s_timer_and_stops_the_scc68692s() {
    let tick = |n: u64| Time((n * 1_000_000_000_000).div_ceil(3_686_400));
    for (variant, acr, running) in [
        (Variant::Mc68681, 0x30, true),
        (Variant::Scc68692, 0x70, false),
    ] {
        let mut bench = Bench::of(variant);
        // X1/16, preload 4: a terminal count every 64 ticks
        bench.write(ACR, acr, Time::ZERO);
        bench.write(CTLR, 4, Time::ZERO);
        bench.duart.reset(tick(32), &mut Drive::new(tick(32)));
        assert_eq!(bench.duart.state.acr, 0x70, "{variant:?}: timer mode");
        // the second terminal count from RESET
        let second = tick(32 + 2 * 64);
        bench.run(second);
        assert_eq!(
            bench.read(ISR, second) & 0x08 != 0,
            running,
            "{variant:?}: ISR[3]"
        );
        let _ = bench.read(START, second);
        let later = tick(32 + 4 * 64);
        bench.run(later);
        assert_eq!(
            bench.read(ISR, later) & 0x08,
            0x08,
            "{variant:?}: timing from the start"
        );
    }
}
