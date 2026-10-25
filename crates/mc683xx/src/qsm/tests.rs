use crate::pins::PortLevels;
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::Stateless;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::size::Size;

use super::PORTQS;
use super::PQSPAR_DDRQS;
use super::QILR_QIVR;
use super::Qsm;
use super::RAM;
use super::SPCR0;
use super::SPCR1;
use super::SPCR2;
use super::SPCR3_SPSR;
use super::STATE_VERSION;
use super::qspi::Shifted;
use super::sci::Frame;
use crate::module::Module;
use crate::module::RegisterAccess;
use crate::peripheral::ClockPhase;
use crate::peripheral::ClockPolarity;
use crate::peripheral::QspiClock;
use crate::peripheral::QspiTransfer;
use crate::peripheral::SpiPeripheral;
use crate::peripheral::Timed;

/// The 16.78-MHz system clock of QSMRM's tables, 2^24 Hz.
const SYSTEM_HZ: u64 = 1 << 24;
/// System clocks in an SCI frame at SCBR 55, 10 bits of 32 x 55.
const FRAME_55: u64 = 10 * 32 * 55;

fn access(offset: u32, size: Size, now: u64) -> RegisterAccess {
    RegisterAccess {
        offset,
        size,
        supervisor: true,
        now: Cycles(now),
    }
}

fn read(qsm: &mut Qsm, offset: u32, size: Size, now: u64) -> u32 {
    qsm.read(access(offset, size, now)).unwrap()
}

fn read_word(qsm: &mut Qsm, offset: u32, now: u64) -> u32 {
    read(qsm, offset, Size::Word, now)
}

fn write(qsm: &mut Qsm, offset: u32, size: Size, value: u32, now: u64) {
    assert!(qsm.write(access(offset, size, now), value));
}

fn write_word(qsm: &mut Qsm, offset: u32, value: u32, now: u64) {
    write(qsm, offset, Size::Word, value, now);
}

/// What a peripheral saw: a transfer, or PCS\[3:0\] changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// PCS levels, bits, the word out, and when.
    Transfer(u8, u8, u16, u64),
    /// PCS levels, and when.
    Selects(u8, u64),
    /// The serial clock, and when.
    Clock(QspiClock, u64),
}

/// A peripheral that answers each word with its complement and
/// records what it saw where the test can read it.
#[derive(Debug, Default)]
struct Echo(Rc<RefCell<Vec<Seen>>>);

// Its records are the test's, not its state.
impl Stateless for Echo {}

impl SpiPeripheral for Echo {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.0.borrow_mut().push(Seen::Transfer(
            transfer.chip_selects,
            transfer.bits,
            transfer.out,
            transfer.now.0,
        ));
        !transfer.out
    }

    fn chip_selects(&mut self, levels: u8, now: Cycles) {
        self.0.borrow_mut().push(Seen::Selects(levels, now.0));
    }

    fn clock(&mut self, clock: QspiClock, now: Cycles) {
        self.0.borrow_mut().push(Seen::Clock(clock, now.0));
    }
}

/// The serial clock at SPBR `spbr` and the test's system clock.
const fn sck(polarity: ClockPolarity, phase: ClockPhase, spbr: u64) -> QspiClock {
    QspiClock {
        polarity,
        phase,
        period: Cycles(2 * spbr),
        system_clock: Frequency::from_hz(SYSTEM_HZ),
    }
}

/// The QSM with IARB 9, the SCI at level 3 and the QSPI at 4, vectors
/// $40 and $41, the SCI at SCBR 55 (9532 baud at 16.78 MHz, QSMRM
/// Table 5-2).
fn qsm() -> Qsm {
    let mut qsm = Qsm::new(Frequency::from_hz(SYSTEM_HZ));
    write_word(&mut qsm, 0x00, 0x0009, 0);
    write_word(&mut qsm, 0x04, 0x2340, 0);
    write_word(&mut qsm, 0x08, 55, 0);
    qsm
}

#[test]
fn registers_out_of_reset() {
    let mut qsm = Qsm::new(Frequency::from_hz(SYSTEM_HZ));
    // QSMRM Table 3-1; MC68331UM D.4
    for (offset, value) in [
        (0x00, 0x0080),
        (0x02, 0x0000),
        (0x04, 0x000f),
        (0x08, 0x0004),
        (0x0a, 0x0000),
        (0x0c, 0x0180),
        (0x16, 0x0000),
        (0x18, 0x0104),
        (0x1a, 0x0404),
        (0x1c, 0x0000),
        (0x1e, 0x0000),
    ] {
        assert_eq!(read_word(&mut qsm, offset, 0), value, "{offset:#x}");
    }
    // PORTQS reads the pins: every one an input the board holds high
    assert_eq!(read(&mut qsm, 0x15, Size::Byte, 0), 0xff);
    assert_eq!(qsm.request(), None);
    assert_eq!(qsm.next_event(), None);
}

#[test]
fn byte_writes_reach_their_register() {
    let mut qsm = qsm();
    write(&mut qsm, 0x05, Size::Byte, 0x51, 0);
    assert_eq!(read_word(&mut qsm, 0x04, 0), 0x2351, "INTV0 reads one");
    write(&mut qsm, 0x16, Size::Byte, 0xff, 0);
    write(&mut qsm, 0x17, Size::Byte, 0xfe, 0);
    assert_eq!(read_word(&mut qsm, 0x16, 0), 0x7bfe);
    write(&mut qsm, 0x1e, Size::Byte, 0xff, 0);
    assert_eq!(read_word(&mut qsm, 0x1e, 0), 0x0700);
    // QSPI RAM is plain RAM to the CPU
    write_word(&mut qsm, 0x14e, 0xbeef, 0);
    write(&mut qsm, 0x100, Size::Byte, 0x12, 0);
    assert_eq!(read_word(&mut qsm, 0x14e, 0), 0xbeef);
    assert_eq!(read(&mut qsm, 0x100, Size::Byte, 0), 0x12);
    assert_eq!(read_word(&mut qsm, 0x150, 0), 0, "past the RAM");
}

#[test]
fn supv_moves_the_registers_to_supervisor_space() {
    let mut qsm = qsm();
    let user = |offset| RegisterAccess {
        supervisor: false,
        ..access(offset, Size::Word, 0)
    };
    // SUPV set: not acknowledged, so the cycle goes external (QSMRM
    // 3.2.1)
    write_word(&mut qsm, 0x00, 0x0089, 0);
    assert_eq!(qsm.read(user(0x08)), None);
    assert!(!qsm.write(user(0x08), 1));
    // SUPV clear: the assignable registers open, the global ones read
    // zero and ignore writes
    write_word(&mut qsm, 0x00, 0x0009, 0);
    assert_eq!(qsm.read(user(0x08)), Some(55));
    assert_eq!(qsm.read(user(0x04)), Some(0));
    assert!(qsm.write(user(0x00), 0x0080));
    assert_eq!(read_word(&mut qsm, 0x00, 0), 0x0009);
}

/// The frames sent since the last call: each frame, start, end and bit
/// time.
fn sent_frames(qsm: &mut Qsm) -> Vec<(Frame, u64, u64, u64)> {
    qsm.take_transmitted()
        .into_iter()
        .map(|sent| (sent.frame, sent.start.0, sent.end.0, sent.bit.0))
        .collect()
}

/// The SCI transmitting, TE set at `now`: a preamble first.
fn transmitting(qsm: &mut Qsm, now: u64) {
    write_word(qsm, 0x0a, 0x0008, now);
}

#[test]
fn a_character_goes_a_frame_after_the_preamble() {
    let mut qsm = qsm();
    transmitting(&mut qsm, 100);
    assert_eq!(read_word(&mut qsm, 0x0c, 100) & 0x0180, 0x0100, "TC clear");
    write_word(&mut qsm, 0x0e, u32::from(b'A'), 100);
    assert_eq!(read_word(&mut qsm, 0x0c, 100) & 0x0100, 0, "TDRE clear");
    assert_eq!(qsm.next_event(), Some(Cycles(100 + FRAME_55)));
    assert!(
        qsm.take_transmitted().is_empty(),
        "the preamble is no frame"
    );
    // the preamble ends, TDR moves to the shifter: its start bit
    let start = 100 + FRAME_55;
    assert_eq!(read_word(&mut qsm, 0x0c, start) & 0x0180, 0x0100);
    let sent = 100 + 2 * FRAME_55;
    assert_eq!(
        sent_frames(&mut qsm),
        [(Frame::Data(u16::from(b'A')), start, sent, 32 * 55)]
    );
    assert_eq!(read_word(&mut qsm, 0x0c, sent - 1) & 0x0080, 0);
    assert_eq!(read_word(&mut qsm, 0x0c, sent) & 0x0180, 0x0180);
    assert!(qsm.take_transmitted().is_empty());
}

#[test]
fn the_format_matches_its_golden() {
    let mut qsm = qsm();
    transmitting(&mut qsm, 100);
    read_word(&mut qsm, 0x0c, 100);
    write_word(&mut qsm, 0x0e, u32::from(b'A'), 100);
    qsm.advance(Cycles(100 + FRAME_55 + 1));
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "qsm",
        &[("qsm", STATE_VERSION)],
        &qsm.save().unwrap(),
    );
}

#[test]
fn a_state_no_run_could_save_is_refused() {
    let mut qsm = qsm();
    transmitting(&mut qsm, 100);
    let snapshot = qsm.save().unwrap();
    let edited = |change: fn(&mut Qsm)| {
        let mut saved: Qsm = decode(STATE_VERSION, &snapshot).unwrap();
        change(&mut saved);
        encode(STATE_VERSION, &saved)
    };
    let invalid: [(Vec<u8>, &str); 3] = [
        (edited(|saved| saved.qilr = 0xff), "QSM registers"),
        (edited(|saved| saved.sci.set_sccr0_unmasked(0xe000)), "SCI"),
        (edited(|saved| saved.qspi.halt_past_the_queue()), "QSPI"),
    ];
    let mut other = self::qsm();
    let before = other.save().unwrap();
    for (bytes, what) in invalid {
        assert!(matches!(
            other.restore(&bytes),
            Err(StateError::Invalid(reason)) if reason == what
        ));
    }
    assert_eq!(other.save().unwrap(), before, "unchanged by the refusals");
}

#[test]
fn tdr_written_without_reading_scsr_is_not_sent() {
    let mut qsm = qsm();
    transmitting(&mut qsm, 0);
    qsm.advance(Cycles(FRAME_55));
    write_word(&mut qsm, 0x0e, 0x55, FRAME_55);
    qsm.advance(Cycles(3 * FRAME_55));
    assert!(qsm.take_transmitted().is_empty());
    assert_eq!(read_word(&mut qsm, 0x0c, 3 * FRAME_55) & 0x0180, 0x0180);
    // a byte write to SCDR's upper half starts nothing either
    write(&mut qsm, 0x0e, Size::Byte, 0x01, 3 * FRAME_55);
    assert_eq!(read_word(&mut qsm, 0x0c, 3 * FRAME_55) & 0x0100, 0x0100);
}

#[test]
fn tdre_and_tc_interrupt_through_the_sci_vector() {
    let mut qsm = qsm();
    // TIE, TE
    write_word(&mut qsm, 0x0a, 0x0088, 0);
    let three = InterruptLevel::new(3).unwrap();
    assert_eq!(qsm.request().unwrap().level, three);
    assert_eq!(qsm.request().unwrap().arbitration, 9);
    assert_eq!(qsm.acknowledge(three, Cycles::ZERO), Some(0x40));
    read_word(&mut qsm, 0x0c, 0);
    write_word(&mut qsm, 0x0e, 0x31, 0);
    assert_eq!(qsm.request(), None);
    // TCIE: TC once the frame is gone
    write_word(&mut qsm, 0x0a, 0x0048, 0);
    qsm.advance(Cycles(2 * FRAME_55));
    assert_eq!(qsm.request().unwrap().level, three);
}

#[test]
fn parity_and_nine_bit_frames_go_as_on_the_line() {
    let mut qsm = qsm();
    // M, PE, odd parity, TE: eight data bits and a parity bit, eleven
    // bit times
    write_word(&mut qsm, 0x0a, 0x0e08, 0);
    let frame = 11 * 32 * 55;
    read_word(&mut qsm, 0x0c, 0);
    // three ones: odd parity needs a zero
    write_word(&mut qsm, 0x0e, 0x107, 0);
    qsm.advance(Cycles(2 * frame));
    assert_eq!(
        sent_frames(&mut qsm),
        [(Frame::Data(0x007), frame, 2 * frame, 32 * 55)]
    );
    // a break while SBK is set
    write_word(&mut qsm, 0x0a, 0x0e09, 2 * frame);
    qsm.advance(Cycles(3 * frame));
    // the next break starts as the first ends, SBK still set
    assert_eq!(
        sent_frames(&mut qsm),
        [
            (Frame::Break, 2 * frame, 3 * frame, 32 * 55),
            (Frame::Break, 3 * frame, 4 * frame, 32 * 55)
        ]
    );
}

/// The SCI receiving, RIE and RE set.
fn receiving(qsm: &mut Qsm) {
    write_word(qsm, 0x0a, 0x0024, 0);
}

#[test]
fn a_character_arriving_sets_rdrf_a_frame_later() {
    let mut qsm = qsm();
    receiving(&mut qsm);
    qsm.receive(Frame::Data(u16::from(b'*')), Cycles(10));
    assert_eq!(read_word(&mut qsm, 0x0c, 11) & 0x0060, 0x0020, "RAF");
    assert_eq!(qsm.next_event(), Some(Cycles(10 + FRAME_55)));
    qsm.advance(Cycles(10 + FRAME_55));
    let three = InterruptLevel::new(3).unwrap();
    assert_eq!(qsm.request().unwrap().level, three);
    assert_eq!(read_word(&mut qsm, 0x0c, 10 + FRAME_55) & 0x0060, 0x0040);
    assert_eq!(read_word(&mut qsm, 0x0e, 10 + FRAME_55), u32::from(b'*'));
    assert_eq!(read_word(&mut qsm, 0x0c, 10 + FRAME_55) & 0x0040, 0);
    assert_eq!(qsm.request(), None);
}

#[test]
fn frames_arrive_back_to_back_and_overrun() {
    let mut qsm = qsm();
    receiving(&mut qsm);
    qsm.receive(Frame::Data(1), Cycles::ZERO);
    qsm.receive(Frame::Data(2), Cycles::ZERO);
    qsm.receive(Frame::Data(3), Cycles::ZERO);
    qsm.advance(Cycles(2 * FRAME_55));
    let status = read_word(&mut qsm, 0x0c, 2 * FRAME_55);
    assert_eq!(status & 0x0048, 0x0048, "RDRF and OR");
    // the read of SCDR clears both; the first character stays
    assert_eq!(read_word(&mut qsm, 0x0e, 2 * FRAME_55), 1);
    assert_eq!(read_word(&mut qsm, 0x0c, 2 * FRAME_55) & 0x0048, 0);
    qsm.advance(Cycles(3 * FRAME_55));
    assert_eq!(read_word(&mut qsm, 0x0e, 3 * FRAME_55), 3);
}

#[test]
fn a_flag_set_after_scsr_is_read_stays() {
    let mut qsm = qsm();
    receiving(&mut qsm);
    read_word(&mut qsm, 0x0c, 0);
    qsm.receive(Frame::Data(7), Cycles::ZERO);
    qsm.advance(Cycles(FRAME_55));
    read_word(&mut qsm, 0x0e, FRAME_55);
    assert_eq!(read_word(&mut qsm, 0x0c, FRAME_55) & 0x0040, 0x0040);
    // a byte read of SCDR's upper half clears nothing
    read(&mut qsm, 0x0e, Size::Byte, FRAME_55);
    assert_eq!(read_word(&mut qsm, 0x0c, FRAME_55) & 0x0040, 0x0040);
}

#[test]
fn errors_arrive_with_the_character() {
    let mut qsm = qsm();
    // PE, even parity, RE
    write_word(&mut qsm, 0x0a, 0x0404, 0);
    // seven data bits 0x01 and a parity bit of zero: odd ones
    qsm.receive(Frame::Data(0x01), Cycles::ZERO);
    qsm.advance(Cycles(FRAME_55));
    assert_eq!(read_word(&mut qsm, 0x0c, FRAME_55) & 0x0043, 0x0041, "PF");
    assert_eq!(read_word(&mut qsm, 0x0e, FRAME_55), 0x01);
    qsm.receive(Frame::Break, Cycles(FRAME_55));
    qsm.advance(Cycles(2 * FRAME_55));
    assert_eq!(
        read_word(&mut qsm, 0x0c, 2 * FRAME_55) & 0x0043,
        0x0042,
        "FE"
    );
}

#[test]
fn an_idle_line_follows_the_last_frame() {
    let mut qsm = qsm();
    // ILT long, ILIE, RE
    write_word(&mut qsm, 0x0a, 0x1014, 0);
    qsm.receive(Frame::Data(0x7f), Cycles::ZERO);
    qsm.advance(Cycles(FRAME_55));
    assert_eq!(qsm.next_event(), Some(Cycles(2 * FRAME_55)));
    qsm.advance(Cycles(2 * FRAME_55));
    assert_eq!(read_word(&mut qsm, 0x0c, 2 * FRAME_55) & 0x0010, 0x0010);
    assert!(qsm.request().is_some());
    // short idle-line detect counts the stop bit and the trailing ones:
    // 0xf0's four and the stop bit leave five bit times
    let mut qsm = qsm_with_sccr1(0x0014);
    qsm.receive(Frame::Data(0xf0), Cycles::ZERO);
    qsm.advance(Cycles(FRAME_55));
    assert_eq!(qsm.next_event(), Some(Cycles(FRAME_55 + 5 * 32 * 55)));
}

fn qsm_with_sccr1(sccr1: u32) -> Qsm {
    let mut qsm = qsm();
    write_word(&mut qsm, 0x0a, sccr1, 0);
    qsm
}

#[test]
fn wakeup_ignores_frames_until_an_address_mark() {
    // WAKE, RE, RWU
    let mut qsm = qsm_with_sccr1(0x0106);
    qsm.receive(Frame::Data(0x12), Cycles::ZERO);
    qsm.receive(Frame::Data(0x81), Cycles::ZERO);
    qsm.advance(Cycles(FRAME_55));
    assert_eq!(read_word(&mut qsm, 0x0c, FRAME_55) & 0x0040, 0);
    qsm.advance(Cycles(2 * FRAME_55));
    assert_eq!(read_word(&mut qsm, 0x0a, 2 * FRAME_55), 0x0104, "RWU clear");
    assert_eq!(read_word(&mut qsm, 0x0e, 2 * FRAME_55), 0x81);
}

#[test]
fn loop_mode_feeds_the_receiver() {
    // LOOPS, TE, RE
    let mut qsm = qsm_with_sccr1(0x400c);
    read_word(&mut qsm, 0x0c, 0);
    write_word(&mut qsm, 0x0e, 0x5a, 0);
    qsm.advance(Cycles(2 * FRAME_55));
    assert!(qsm.take_transmitted().is_empty(), "TXD stays at mark");
    assert_eq!(read_word(&mut qsm, 0x0e, 2 * FRAME_55), 0x5a);
}

#[test]
fn a_stopped_baud_generator_moves_nothing() {
    let mut qsm = qsm();
    write_word(&mut qsm, 0x08, 0, 0);
    transmitting(&mut qsm, 0);
    read_word(&mut qsm, 0x0c, 0);
    write_word(&mut qsm, 0x0e, 0x41, 0);
    assert_eq!(qsm.next_event(), None);
    // SCBR written: the preamble starts
    write_word(&mut qsm, 0x08, 1, 1000);
    assert_eq!(qsm.next_event(), Some(Cycles(1000 + 10 * 32)));
}

/// The QSM with an [`Echo`] on the QSPI, PCS\[3:0\], SCK and MOSI the
/// QSPI's outputs, PORTQS holding PCS high: the queue's log.
fn qspi(qsm: &mut Qsm) -> Rc<RefCell<Vec<Seen>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    qsm.attach(Box::new(Echo(Rc::clone(&seen))));
    write(qsm, 0x15, Size::Byte, 0x78, 0);
    write_word(qsm, 0x16, 0x7b7e, 0);
    seen.borrow_mut().clear();
    seen
}

#[test]
fn a_queue_runs_with_each_entrys_pcs_and_timing() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    // three entries: 8 bits to PCS 1110; BITSE (12 bits), DSCK, DT to
    // 1101 with CONT; 8 bits to 1101
    for (entry, (command, out)) in [(0x0e, 0x00a5), (0xfd, 0x0abc), (0x0d, 0x0042)]
        .into_iter()
        .enumerate()
    {
        let entry = entry as u32;
        write(&mut qsm, 0x140 + entry, Size::Byte, command, 0);
        write_word(&mut qsm, 0x120 + 2 * entry, out, 0);
    }
    // master, 12 bits, SPBR 4; DSCKL 10, DTL 3; ENDQP 2, NEWQP 0,
    // SPIFIE
    write_word(&mut qsm, 0x18, 0xb004, 0);
    write_word(&mut qsm, 0x1c, 0x8200, 0);
    write_word(&mut qsm, 0x1a, 0x8a03, 100);
    assert_eq!(read_word(&mut qsm, 0x1a, 100) & 0x8000, 0x8000);
    // entry 0: half an SCK period (4), 8 bits of 8 clocks, 17 after
    let first = 100 + 4 + 64;
    // entry 1: DSCKL 10, 12 bits, DTL 3 x 32 after
    let second = first + 17 + 10 + 96;
    let third = second + 96 + 4 + 64;
    qsm.advance(Cycles(10_000));
    assert_eq!(
        seen.borrow()[..],
        [
            Seen::Clock(
                sck(ClockPolarity::IdleLow, ClockPhase::CaptureLeading, 4),
                100
            ),
            Seen::Selects(0b1110, 100),
            Seen::Transfer(0b1110, 8, 0x00a5, first),
            Seen::Selects(0b1111, first),
            Seen::Selects(0b1101, first + 17),
            Seen::Transfer(0b1101, 12, 0x0abc, second),
            Seen::Transfer(0b1101, 8, 0x0042, third),
            Seen::Selects(0b1111, third),
        ]
    );
    assert_eq!(read_word(&mut qsm, 0x100, 10_000), 0x5a);
    assert_eq!(read_word(&mut qsm, 0x102, 10_000), 0x543, "12 bits");
    assert_eq!(read_word(&mut qsm, 0x104, 10_000), 0xbd);
    assert_eq!(read_word(&mut qsm, 0x1a, 10_000) & 0x8000, 0, "SPE clear");
    assert_eq!(
        read(&mut qsm, 0x1f, Size::Byte, 10_000),
        0x82,
        "SPIF, CPTQP"
    );
    let four = InterruptLevel::new(4).unwrap();
    assert_eq!(qsm.request().unwrap().level, four);
    assert_eq!(qsm.acknowledge(four, Cycles(10_000)), Some(0x41));
    write(&mut qsm, 0x1f, Size::Byte, 0x00, 10_000);
    assert_eq!(qsm.request(), None);
}

/// Port QS's output changes as PCS\[3:0\] and the pins driven, after
/// the queue of `a_queue_runs_with_each_entrys_pcs_and_timing` with no
/// peripheral, the QSM brought to 10,000 clocks `step` at a time.
fn queue_output_changes(step: u64) -> Vec<(u64, u8, u8)> {
    let mut qsm = qsm();
    write(&mut qsm, 0x15, Size::Byte, 0x78, 0);
    write_word(&mut qsm, 0x16, 0x7b7e, 0);
    for (entry, (command, out)) in [(0x0e, 0x00a5), (0xfd, 0x0abc), (0x0d, 0x0042)]
        .into_iter()
        .enumerate()
    {
        let entry = entry as u32;
        write(&mut qsm, 0x140 + entry, Size::Byte, command, 0);
        write_word(&mut qsm, 0x120 + 2 * entry, out, 0);
    }
    write_word(&mut qsm, 0x18, 0xb004, 0);
    write_word(&mut qsm, 0x1c, 0x8200, 0);
    write_word(&mut qsm, 0x1a, 0x8a03, 100);
    for now in (step..=10_000).step_by(step as usize) {
        qsm.advance(Cycles(now));
    }
    qsm.take_output_changes()
        .into_iter()
        .map(|(at, outputs)| (at.0, outputs.driven, outputs.levels >> 3 & 0xf))
        .collect()
}

#[test]
fn port_qs_output_changes_come_at_their_times_however_the_qsm_is_advanced() {
    let first = 100 + 4 + 64;
    let second = first + 17 + 10 + 96;
    let third = second + 96 + 4 + 64;
    // DDRQS $7E at 0 drives PORTQS's PCS 1111; the queue's PCS codes,
    // PCS held through entry 1's CONT
    let expected = [
        (0, 0x7e, 0b1111),
        (100, 0x7e, 0b1110),
        (first, 0x7e, 0b1111),
        (first + 17, 0x7e, 0b1101),
        (third, 0x7e, 0b1111),
    ];
    assert_eq!(queue_output_changes(10_000), expected);
    assert_eq!(queue_output_changes(1), expected);
}

#[test]
fn a_polled_queue_stops_without_a_quantum_unless_pcs_moves() {
    for (command, stop) in [(0x0e, None), (0x8e, Some(100 + 4 + 64 + 17))] {
        let mut qsm = qsm();
        let seen = qspi(&mut qsm);
        // one 8-bit entry to PCS 1110, CONT or not; master, SPBR 4;
        // ENDQP 0, no SPIFIE: polled
        write(&mut qsm, 0x140, Size::Byte, command, 0);
        write_word(&mut qsm, 0x18, 0x8004, 0);
        write_word(&mut qsm, 0x1a, 0x8000, 100);
        let transfer = 100 + 4 + 64;
        assert_eq!(qsm.next_event(), Some(Cycles(transfer)));
        qsm.advance(Cycles(transfer));
        // without CONT PCS returned at the transfer, so the stop calls
        // nothing and nothing sees it before a read
        assert_eq!(qsm.next_event(), stop.map(Cycles), "{command:#x}");
        qsm.advance(Cycles(10_000));
        assert_eq!(
            seen.borrow().last(),
            Some(&Seen::Selects(0b1111, stop.unwrap_or(transfer)))
        );
        assert_eq!(read_word(&mut qsm, 0x1a, 10_000) & 0x8000, 0, "SPE clear");
    }
}

#[test]
fn the_qspi_vector_wins_over_the_scis_at_one_level() {
    let mut qsm = qsm();
    qspi(&mut qsm);
    write_word(&mut qsm, 0x04, 0x1240, 0);
    // TIE, TE: the SCI requests at level 2
    write_word(&mut qsm, 0x0a, 0x0088, 0);
    write_word(&mut qsm, 0x18, 0x8004, 0);
    write_word(&mut qsm, 0x1c, 0x8000, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    qsm.advance(Cycles(1000));
    let two = InterruptLevel::new(2).unwrap();
    assert_eq!(qsm.acknowledge(two, Cycles(1000)), Some(0x41));
    read(&mut qsm, 0x1f, Size::Byte, 1000);
    write(&mut qsm, 0x1f, Size::Byte, 0, 1000);
    assert_eq!(qsm.acknowledge(two, Cycles(1000)), Some(0x40));
}

/// One 8-bit entry's time at SPBR 2 with the standard delays: half an
/// SCK period, 8 bits of 4 clocks, 17 after.
const ENTRY_2: u64 = 2 + 32 + 17;

/// The serial clocks a peripheral was told, and when.
fn clocks(seen: &RefCell<Vec<Seen>>) -> Vec<(QspiClock, u64)> {
    seen.borrow()
        .iter()
        .filter_map(|seen| match seen {
            Seen::Clock(clock, now) => Some((*clock, *now)),
            Seen::Transfer(..) | Seen::Selects(..) => None,
        })
        .collect()
}

#[test]
fn the_reset_clock_is_spcr0_out_of_reset() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    let spcr0 = read(&mut qsm, 0x18, Size::Word, 0);
    // MSTR on SPCR0 out of reset; ENDQP 1
    write_word(&mut qsm, 0x18, 0x8000 | spcr0, 0);
    write_word(&mut qsm, 0x1c, 0x0100, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    qsm.advance(Cycles(10_000));
    let reset = QspiClock::reset(Frequency::from_hz(SYSTEM_HZ));
    assert_eq!(clocks(&seen), [(reset, 0)]);
    assert_eq!(
        reset,
        sck(ClockPolarity::IdleLow, ClockPhase::ChangeLeading, 4)
    );
}

#[test]
fn the_peripheral_learns_the_mode_and_baud_when_they_change() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    // CPOL, CPHA, SPBR 2: 4.19 MHz (QSMRM Table 4-3); ENDQP 1
    write_word(&mut qsm, 0x18, 0x8302, 0);
    write_word(&mut qsm, 0x1c, 0x0100, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    qsm.advance(Cycles(2 * ENTRY_2));
    let fast = sck(ClockPolarity::IdleHigh, ClockPhase::ChangeLeading, 2);
    assert_eq!(clocks(&seen), [(fast, 0)], "told once");
    assert_eq!(fast.baud().hz(), 4_194_304);
    // the same clock again: not told
    write_word(&mut qsm, 0x1a, 0x8000, 2 * ENTRY_2);
    qsm.advance(Cycles(4 * ENTRY_2));
    assert_eq!(clocks(&seen).len(), 1);
    // CPOL and CPHA clear, SPBR 8, the system clock halved
    let now = 4 * ENTRY_2;
    qsm.set_system_clock(Frequency::from_hz(SYSTEM_HZ / 2));
    write_word(&mut qsm, 0x18, 0x8008, now);
    write_word(&mut qsm, 0x1a, 0x8000, now);
    let slow = QspiClock {
        system_clock: Frequency::from_hz(SYSTEM_HZ / 2),
        ..sck(ClockPolarity::IdleLow, ClockPhase::CaptureLeading, 8)
    };
    assert_eq!(clocks(&seen), [(fast, 0), (slow, now)]);
    assert_eq!(slow.baud().hz(), 524_288);
}

#[test]
fn wraparound_repeats_the_queue_from_newqp() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    for entry in 0..16 {
        write(&mut qsm, 0x140 + entry, Size::Byte, entry & 0xf, 0);
    }
    // WREN, WRTO, ENDQP 5, NEWQP 4
    write_word(&mut qsm, 0x18, 0x8002, 0);
    write_word(&mut qsm, 0x1c, 0x6504, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    qsm.advance(Cycles(5 * ENTRY_2));
    let codes: Vec<u8> = seen
        .borrow()
        .iter()
        .filter_map(|seen| match seen {
            Seen::Transfer(code, ..) => Some(*code),
            Seen::Selects(..) | Seen::Clock(..) => None,
        })
        .collect();
    assert_eq!(codes, [4, 5, 4, 5, 4]);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 5 * ENTRY_2) & 0x80, 0x80);
    assert_eq!(read_word(&mut qsm, 0x1a, 5 * ENTRY_2) & 0x8000, 0x8000);
    // WREN cleared: the queue ends at ENDQP and clears SPE
    write_word(&mut qsm, 0x1c, 0x0504, 5 * ENTRY_2);
    qsm.advance(Cycles(8 * ENTRY_2));
    assert_eq!(read_word(&mut qsm, 0x1a, 8 * ENTRY_2) & 0x8000, 0);
}

#[test]
fn a_queue_past_f_wraps_to_0() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    // ENDQP 1, NEWQP $E: entries E, F, 0, 1
    write_word(&mut qsm, 0x18, 0x8002, 0);
    write_word(&mut qsm, 0x1c, 0x010e, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    qsm.advance(Cycles(10 * ENTRY_2));
    let transfers = seen
        .borrow()
        .iter()
        .filter(|seen| matches!(seen, Seen::Transfer(..)))
        .count();
    assert_eq!(transfers, 4);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 10 * ENTRY_2), 0x81);
}

#[test]
fn halt_stops_at_a_queue_boundary_and_resumes() {
    let mut qsm = qsm();
    qspi(&mut qsm);
    // ENDQP 3; HMIE, HALT during the first entry
    write_word(&mut qsm, 0x18, 0x8002, 0);
    write_word(&mut qsm, 0x1c, 0x0300, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    write(&mut qsm, 0x1e, Size::Byte, 0x03, 1);
    qsm.advance(Cycles(10 * ENTRY_2));
    assert_eq!(
        read(&mut qsm, 0x1f, Size::Byte, 10 * ENTRY_2),
        0x20,
        "HALTA"
    );
    assert_eq!(qsm.next_event(), None);
    assert!(qsm.request().is_some(), "HMIE");
    // HALT cleared: entries 1 to 3 follow
    write(&mut qsm, 0x1f, Size::Byte, 0x00, 10 * ENTRY_2);
    write(&mut qsm, 0x1e, Size::Byte, 0x02, 10 * ENTRY_2);
    qsm.advance(Cycles(20 * ENTRY_2));
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 20 * ENTRY_2), 0x83);
}

#[test]
fn newqp_waits_for_the_entry_in_progress() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    for entry in 0..16 {
        write(&mut qsm, 0x140 + entry, Size::Byte, entry & 0xf, 0);
    }
    // ENDQP 9, NEWQP 0; at the first entry NEWQP 8 is written
    write_word(&mut qsm, 0x18, 0x8002, 0);
    write_word(&mut qsm, 0x1c, 0x0900, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    write_word(&mut qsm, 0x1c, 0x0908, 1);
    assert_eq!(read_word(&mut qsm, 0x1c, 1), 0x0900, "reads the register");
    qsm.advance(Cycles(10 * ENTRY_2));
    let codes: Vec<u8> = seen
        .borrow()
        .iter()
        .filter_map(|seen| match seen {
            Seen::Transfer(code, ..) => Some(*code),
            Seen::Selects(..) | Seen::Clock(..) => None,
        })
        .collect();
    assert_eq!(codes, [0, 8, 9]);
}

#[test]
fn spe_cleared_aborts_the_transfer() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    write_word(&mut qsm, 0x18, 0x8002, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    write_word(&mut qsm, 0x1a, 0x0000, 10);
    qsm.advance(Cycles(1000));
    assert!(
        !seen
            .borrow()
            .iter()
            .any(|seen| matches!(seen, Seen::Transfer(..)))
    );
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 1000), 0);
}

#[test]
fn ss_held_low_is_a_mode_fault() {
    let mut qsm = qsm();
    qspi(&mut qsm);
    // PCS0/SS an input
    write_word(&mut qsm, 0x16, 0x7b76, 0);
    qsm.set_port_pins(0xf7, Cycles(0));
    write_word(&mut qsm, 0x18, 0x8002, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 0), 0x40, "MODF");
    assert_eq!(read_word(&mut qsm, 0x1a, 0) & 0x8000, 0);
    assert_eq!(qsm.next_event(), None);
}

#[test]
fn loopq_receives_what_it_sends() {
    let mut qsm = qsm();
    qspi(&mut qsm);
    write_word(&mut qsm, 0x120, 0x00c3, 0);
    write(&mut qsm, 0x1e, Size::Byte, 0x04, 0);
    write_word(&mut qsm, 0x18, 0x8002, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    qsm.advance(Cycles(ENTRY_2));
    assert_eq!(read_word(&mut qsm, 0x100, ENTRY_2), 0xc3);
}

#[test]
fn a_stopped_baud_generator_waits() {
    let mut qsm = qsm();
    qspi(&mut qsm);
    // SPBR 1 stops SCK (QSMRM 4.3.1)
    write_word(&mut qsm, 0x18, 0x8001, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 0);
    assert_eq!(qsm.next_event(), None);
    write_word(&mut qsm, 0x18, 0x8002, 10);
    assert_eq!(qsm.next_event(), Some(Cycles(10 + 2 + 32)));
}

#[test]
fn a_reset_keeps_the_ram_and_the_peripheral() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    write_word(&mut qsm, 0x120, 0x1234, 0);
    transmitting(&mut qsm, 0);
    qsm.reset(Cycles(5));
    assert_eq!(read_word(&mut qsm, 0x120, 5), 0x1234);
    assert_eq!(read_word(&mut qsm, 0x0a, 5), 0);
    assert_eq!(read_word(&mut qsm, 0x00, 5), 0x0080);
    assert!(qsm.peripheral::<Echo>().is_some());
    // the port's outputs go: PCS floats to the board's high
    assert_eq!(seen.borrow().last(), None);
}

#[test]
fn woms_makes_txd_open_drain() {
    let mut qsm = qsm();
    // TE: TXD at mark, driven high
    write_word(&mut qsm, 0x0a, 0x0008, 0);
    assert_eq!(
        qsm.port_outputs(),
        PortLevels {
            driven: 0x80,
            levels: 0x80
        }
    );
    // WOMS: released, to the board's level (QSMRM 5.2.2)
    write_word(&mut qsm, 0x0a, 0x2008, 0);
    assert_eq!(
        qsm.port_outputs(),
        PortLevels {
            driven: 0x00,
            levels: 0x00
        }
    );
    qsm.set_port_pins(0x7f, Cycles::ZERO);
    assert_eq!(read(&mut qsm, 0x15, Size::Byte, 0) & 0x80, 0);
}

#[test]
fn lpstop_holds_the_qspi_and_the_sci_until_the_wake() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    // one 8-bit entry to PCS 1110; master, SPBR 4; ENDQP 0
    write(&mut qsm, 0x140, Size::Byte, 0x0e, 0);
    write_word(&mut qsm, 0x18, 0x8004, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 100);
    let transfer = 100 + 4 + 64;
    // RE: a frame on RXD
    write_word(&mut qsm, 0x0a, 0x0004, 100);
    qsm.receive(Frame::Data(0x55), Cycles(100));
    // the clock off 10 clocks in, for 1000 (MC68331UM 4.3.4)
    qsm.stop_clock(Cycles(110));
    assert_eq!(qsm.next_event(), None);
    qsm.advance(Cycles(1110));
    qsm.receive(Frame::Data(0x33), Cycles(1110));
    assert!(
        !seen
            .borrow()
            .iter()
            .any(|seen| matches!(seen, Seen::Transfer(..)))
    );
    qsm.start_clock(Cycles(1110));
    qsm.advance(Cycles(1_000_000));
    assert!(
        seen.borrow()
            .contains(&Seen::Transfer(0b1110, 8, 0x0000, transfer + 1000)),
        "{:?}",
        seen.borrow()
    );
    // the frame arriving goes on; the one sent while stopped is unseen
    assert_eq!(read_word(&mut qsm, 0x0e, 1_000_000), 0x55);
    assert_eq!(
        read_word(&mut qsm, 0x0c, 1_000_000) & 0x0048,
        0x0040,
        "RDRF, no OR"
    );
}

#[test]
fn a_reset_first_makes_the_transfers_already_due() {
    let mut qsm = qsm();
    let seen = qspi(&mut qsm);
    // one 8-bit entry to PCS 1110; master, SPBR 4; ENDQP 0
    write(&mut qsm, 0x140, Size::Byte, 0x0e, 0);
    write_word(&mut qsm, 0x120, 0x00a5, 0);
    write_word(&mut qsm, 0x18, 0x8004, 0);
    write_word(&mut qsm, 0x1a, 0x8000, 100);
    let transfer = 100 + 4 + 64;
    qsm.reset(Cycles(transfer + 10));
    assert!(
        seen.borrow()
            .contains(&Seen::Transfer(0b1110, 8, 0x00a5, transfer)),
        "{:?}",
        seen.borrow()
    );
    assert_eq!(read_word(&mut qsm, 0x100, transfer + 10), 0x5a);
}

/// The QSM as a slave with an [`Echo`] that must see nothing: MISO the
/// QSPI's output, MOSI and PCS0/SS its inputs, SCK an input
/// (MC68331UM 6.3.5.3); BITS 8, ENDQP `end`, NEWQP 0, SPIFIE, and the
/// transmit RAM `out`.  SPE is left to the test.
fn slave(qsm: &mut Qsm, end: u32, out: &[u16]) -> Rc<RefCell<Vec<Seen>>> {
    let seen = qspi(qsm);
    write_word(qsm, 0x16, 0x0b01, 0);
    for (entry, &word) in out.iter().enumerate() {
        write_word(qsm, 0x120 + 2 * entry as u32, word.into(), 0);
    }
    write_word(qsm, 0x18, 0x2000, 0);
    write_word(qsm, 0x1c, 0x8000 | end << 8, 0);
    seen
}

/// SPE set at `now`.
fn enable(qsm: &mut Qsm, now: u64) {
    write_word(qsm, 0x1a, 0x8000, now);
}

/// The host's master shifts `bits` bits of `mosi` at `now`, most
/// significant first.
fn shift(qsm: &mut Qsm, mosi: u16, bits: u8, now: u64) -> Shifted {
    let mut shifted = Shifted::default();
    for bit in (0..bits).rev() {
        shifted.record(bit, qsm.clock_slave(mosi >> bit & 1 != 0, Cycles(now)));
    }
    shifted
}

/// What MISO carried for `bits` bits, all driven.
const fn driven(miso: u16, bits: u8) -> Shifted {
    Shifted {
        miso,
        driven: u16::MAX >> (16 - bits),
    }
}

#[test]
fn a_slave_takes_the_masters_bits_entry_by_entry() {
    let mut qsm = qsm();
    let seen = slave(&mut qsm, 2, &[0x00a5, 0x000f, 0x0042]);
    enable(&mut qsm, 0);
    // not selected yet: the bits go nowhere
    assert_eq!(shift(&mut qsm, 0xff, 8, 0), Shifted::default());
    qsm.set_slave_select(true, Cycles(10));
    assert_eq!(shift(&mut qsm, 0x3c, 8, 10), driven(0xa5, 8));
    assert_eq!(read_word(&mut qsm, 0x100, 10), 0x3c);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 10), 0x00, "CPTQP 0");
    // entry 1 in two halves, SS negated between: it resumes
    assert_eq!(shift(&mut qsm, 0x9, 4, 20), driven(0x0, 4));
    qsm.set_slave_select(false, Cycles(30));
    assert_eq!(shift(&mut qsm, 0xf, 4, 35), Shifted::default());
    qsm.set_slave_select(true, Cycles(40));
    assert_eq!(read_word(&mut qsm, 0x102, 40), 0, "not yet stored");
    assert_eq!(shift(&mut qsm, 0x6, 4, 40), driven(0xf, 4));
    assert_eq!(read_word(&mut qsm, 0x102, 40), 0x96);
    // entry 2, ENDQP: SPIF, SPE clears, the bits past it go nowhere
    assert_eq!(
        shift(&mut qsm, 0x81ff, 16, 50),
        Shifted {
            miso: 0x4200,
            driven: 0xff00
        }
    );
    assert_eq!(read_word(&mut qsm, 0x104, 50), 0x81);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 50), 0x82, "SPIF, CPTQP 2");
    assert_eq!(read_word(&mut qsm, 0x1a, 50) & 0x8000, 0, "SPE clear");
    assert_eq!(
        qsm.request().unwrap().level,
        InterruptLevel::new(4).unwrap()
    );
    // a slave drives no PCS and starts no master transfer: the
    // peripheral sees only the master's SS on PCS0
    assert_eq!(
        seen.borrow()[..],
        [
            Seen::Selects(0b1110, 10),
            Seen::Selects(0b1111, 30),
            Seen::Selects(0b1110, 40),
        ]
    );
    assert_eq!(qsm.next_event(), None);
}

#[test]
fn a_slave_word_is_bits_long() {
    let mut qsm = qsm();
    slave(&mut qsm, 0, &[0x0abc]);
    // BITS 12; BITS 0 is 16 (QSMRM Table 4-2)
    write_word(&mut qsm, 0x18, 0x3000, 0);
    enable(&mut qsm, 0);
    qsm.set_slave_select(true, Cycles(0));
    assert_eq!(shift(&mut qsm, 0x0123, 12, 0), driven(0xabc, 12));
    assert_eq!(read_word(&mut qsm, 0x100, 0), 0x123);
    write_word(&mut qsm, 0x18, 0x0000, 0);
    enable(&mut qsm, 0);
    assert_eq!(shift(&mut qsm, 0xbeef, 16, 0), driven(0x0abc, 16));
    assert_eq!(read_word(&mut qsm, 0x100, 0), 0xbeef);
}

#[test]
fn slave_wraparound_repeats_the_queue() {
    let mut qsm = qsm();
    slave(&mut qsm, 1, &[0x0011, 0x0022]);
    // WREN to $0, ENDQP 1, NEWQP 1
    write_word(&mut qsm, 0x1c, 0x4101, 0);
    enable(&mut qsm, 0);
    qsm.set_slave_select(true, Cycles(0));
    let out: Vec<u16> = [0xa1, 0xb0, 0xb1, 0xc0]
        .into_iter()
        .map(|mosi| shift(&mut qsm, mosi, 8, 0).miso)
        .collect();
    assert_eq!(out, [0x22, 0x11, 0x22, 0x11]);
    assert_eq!(read_word(&mut qsm, 0x100, 0), 0xc0);
    assert_eq!(read_word(&mut qsm, 0x102, 0), 0xb1);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 0), 0x80, "SPIF, CPTQP 0");
    assert_eq!(read_word(&mut qsm, 0x1a, 0) & 0x8000, 0x8000, "SPE stays");
    // WRTO: from ENDQP to NEWQP, entry 1 again
    write_word(&mut qsm, 0x1c, 0x6101, 0);
    assert_eq!(shift(&mut qsm, 0xd1, 8, 0).miso, 0x22);
    assert_eq!(shift(&mut qsm, 0xd1, 8, 0).miso, 0x22);
}

#[test]
fn a_slave_halts_after_the_word_in_progress() {
    let mut qsm = qsm();
    slave(&mut qsm, 3, &[0x0011, 0x0022]);
    enable(&mut qsm, 0);
    qsm.set_slave_select(true, Cycles(0));
    shift(&mut qsm, 0x5, 4, 0);
    // HMIE, HALT mid-word: the word completes, then the queue halts
    write(&mut qsm, 0x1e, Size::Byte, 0x03, 0);
    assert_eq!(shift(&mut qsm, 0xa, 4, 0), driven(0x1, 4));
    assert_eq!(read_word(&mut qsm, 0x100, 0), 0x5a);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 0), 0x20, "HALTA");
    assert!(qsm.request().is_some(), "HMIE");
    assert_eq!(shift(&mut qsm, 0xff, 8, 0), Shifted::default(), "halted");
    // HALT cleared: entry 1 follows
    write(&mut qsm, 0x1f, Size::Byte, 0x00, 0);
    write(&mut qsm, 0x1e, Size::Byte, 0x00, 0);
    assert_eq!(shift(&mut qsm, 0x77, 8, 0), driven(0x22, 8));
    assert_eq!(read_word(&mut qsm, 0x102, 0), 0x77);
}

#[test]
fn newqp_moves_a_slave_only_between_words() {
    let mut qsm = qsm();
    slave(&mut qsm, 5, &[0x0011, 0x0022, 0x0033, 0x0044]);
    enable(&mut qsm, 0);
    qsm.set_slave_select(true, Cycles(0));
    // at a word boundary: entry 2 at once
    write_word(&mut qsm, 0x1c, 0x8502, 0);
    assert_eq!(shift(&mut qsm, 0, 8, 0).miso, 0x33);
    // mid-word: after it
    shift(&mut qsm, 0, 4, 0);
    write_word(&mut qsm, 0x1c, 0x8501, 0);
    assert_eq!(shift(&mut qsm, 0, 4, 0).miso, 0x4);
    assert_eq!(shift(&mut qsm, 0, 8, 0).miso, 0x22);
}

#[test]
fn a_selected_slave_is_no_mode_fault_but_a_master_is() {
    let mut qsm = qsm();
    slave(&mut qsm, 0, &[]);
    enable(&mut qsm, 0);
    qsm.set_slave_select(true, Cycles(0));
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 0), 0);
    // MSTR set with SS still low (QSMRM 4.3.5)
    write_word(&mut qsm, 0x18, 0xa004, 0);
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 0), 0x40, "MODF");
    assert_eq!(read_word(&mut qsm, 0x1a, 0) & 0x8000, 0, "SPE clear");
}

#[test]
fn the_slaves_pins_follow_ddrqs_and_womq() {
    let mut qsm = qsm();
    slave(&mut qsm, 15, &[0x00f0, 0x00f0, 0x00f0]);
    enable(&mut qsm, 0);
    qsm.set_slave_select(true, Cycles(0));
    // WOMQ: MISO's high bits float (QSMRM 4.3.1)
    write_word(&mut qsm, 0x18, 0x6000, 0);
    assert_eq!(
        shift(&mut qsm, 0x12, 8, 0),
        Shifted {
            miso: 0,
            driven: 0x0f
        }
    );
    write_word(&mut qsm, 0x18, 0x2000, 0);
    // MOSI an output: data input disabled, zeros received; MISO an
    // input: no data out (MC68331UM Table 6-1)
    write_word(&mut qsm, 0x16, 0x0b02, 0);
    assert_eq!(shift(&mut qsm, 0xff, 8, 0), Shifted::default());
    assert_eq!(read_word(&mut qsm, 0x102, 0), 0);
    // SCK an output: no clock in, nothing moves
    write_word(&mut qsm, 0x16, 0x0b05, 0);
    assert_eq!(shift(&mut qsm, 0xff, 8, 0), driven(0xff, 8));
    assert_eq!(read(&mut qsm, 0x1f, Size::Byte, 0), 0x01, "CPTQP 1");
    // SS an output: not selected
    write_word(&mut qsm, 0x16, 0x0b09, 0);
    assert_eq!(shift(&mut qsm, 0xff, 8, 0), Shifted::default());
}

/// A generator for the randomized tests: xorshift64*, seeded, so a run
/// is the same every time.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// True `percent` percent of the time.
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// How a model ends a run's quanta: at every end of a queue entry's
/// phase, as the QSPI did before it batched entries, or at the QSM's
/// [`Timed::next_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syncing {
    PerPhase,
    Seen,
}

/// The QSM's next event as `syncing` has it.
fn next_event(qsm: &Qsm, syncing: Syncing) -> Option<Cycles> {
    match syncing {
        Syncing::PerPhase => [qsm.sci.next_event(), qsm.qspi.next_phase_end()]
            .into_iter()
            .flatten()
            .min(),
        Syncing::Seen => qsm.next_event(),
    }
}

/// A peripheral that answers each word with its complement and logs
/// what it saw with the instruction it was called in.
struct Logged {
    /// What it saw, and the instruction count then.
    seen: Rc<RefCell<Vec<(Seen, u64)>>>,
    /// The instructions run so far.
    instructions: Rc<Cell<u64>>,
    /// What `deferrable` returns.
    deferrable: bool,
}

impl Logged {
    fn log(&self, seen: Seen) {
        self.seen.borrow_mut().push((seen, self.instructions.get()));
    }
}

impl Stateless for Logged {}

impl SpiPeripheral for Logged {
    fn transfer(&mut self, transfer: QspiTransfer) -> u16 {
        self.log(Seen::Transfer(
            transfer.chip_selects,
            transfer.bits,
            transfer.out,
            transfer.now.0,
        ));
        !transfer.out
    }

    fn chip_selects(&mut self, levels: u8, now: Cycles) {
        self.log(Seen::Selects(levels, now.0));
    }

    fn clock(&mut self, clock: QspiClock, now: Cycles) {
        self.log(Seen::Clock(clock, now.0));
    }

    fn deferrable(&self) -> bool {
        self.deferrable
    }
}

/// An access an instruction makes.
#[derive(Debug, Clone, Copy)]
enum Operation {
    /// A read at the offset.
    Read(u32, Size),
    /// A write of the value at the offset.
    Write(u32, Size, u32),
}

/// An instruction: its clocks, and an access so many clocks in.
#[derive(Debug, Clone, Copy)]
struct Instruction {
    clocks: u64,
    access: Option<(u64, Operation)>,
}

/// A random program for the QSPI, and the runs the host cuts it into.
struct Program {
    /// The instructions, run in order, round again at the end.
    instructions: Vec<Instruction>,
    /// Each run's budget, and the port QS pins the host drives after
    /// it.
    runs: Vec<(u64, Option<u8>)>,
}

/// A random write to the QSPI's registers or RAM, or port QS's: mostly
/// settings that keep a queue running fast.
fn random_write(random: &mut Random) -> Operation {
    let spbr = [0, 1, 2, 2, 3, 4, 8][random.below(7) as usize];
    match random.below(12) {
        0 => Operation::Write(
            SPCR0,
            Size::Word,
            u32::from(random.chance(90)) << 15
                | (random.below(16) as u32) << 10
                | (random.below(4) as u32) << 8
                | spbr,
        ),
        1 | 2 => Operation::Write(
            SPCR1,
            Size::Word,
            u32::from(random.chance(70)) << 15
                | (random.below(5) as u32) << 8
                | random.below(4) as u32,
        ),
        3 | 4 => Operation::Write(SPCR2, Size::Word, random.next() as u32 & 0xffff),
        // HALT sometimes, LOOPQ and HMIE
        5 => Operation::Write(
            SPCR3_SPSR,
            Size::Byte,
            u32::from(random.chance(15)) | (random.below(4) as u32) << 1,
        ),
        6 => Operation::Write(SPCR3_SPSR + 1, Size::Byte, 0),
        // command RAM, DT rarely: its delays are long
        7 | 8 => Operation::Write(
            RAM + 0x40 + random.below(16) as u32,
            Size::Byte,
            random.next() as u32 & if random.chance(20) { 0xff } else { 0xdf },
        ),
        9 => Operation::Write(
            RAM + 0x20 + 2 * random.below(16) as u32,
            Size::Word,
            random.next() as u32 & 0xffff,
        ),
        10 => Operation::Write(
            PQSPAR_DDRQS,
            Size::Word,
            if random.chance(80) {
                0x7b7e
            } else {
                random.next() as u32 & 0x7bff
            },
        ),
        _ => Operation::Write(PORTQS + 1, Size::Byte, random.next() as u32 & 0xff),
    }
}

/// A random read of the QSPI's registers or RAM, or port QS's.
fn random_read(random: &mut Random) -> Operation {
    match random.below(4) {
        0 => Operation::Read(SPCR3_SPSR + 1, Size::Byte),
        1 => Operation::Read(RAM + 2 * random.below(0x28) as u32, Size::Word),
        2 => Operation::Read(SPCR0 + 2 * random.below(4) as u32, Size::Word),
        _ => Operation::Read(PORTQS, Size::Word),
    }
}

impl Program {
    fn new(random: &mut Random) -> Self {
        // the QSPI at level 4; PCS[3:0], SCK and MOSI its outputs; a
        // random queue running at once
        let mut setup = vec![
            Operation::Write(QILR_QIVR, Size::Word, 0x2340),
            Operation::Write(PQSPAR_DDRQS, Size::Word, 0x7b7e),
            Operation::Write(PORTQS + 1, Size::Byte, 0x78),
        ];
        setup.extend((0..16).map(|entry| {
            Operation::Write(RAM + 0x40 + entry, Size::Byte, random.next() as u32 & 0xdf)
        }));
        setup.extend([
            Operation::Write(SPCR0, Size::Word, 0x8002 | (random.below(16) as u32) << 10),
            Operation::Write(SPCR2, Size::Word, random.next() as u32 & 0xffff),
            Operation::Write(SPCR1, Size::Word, 0x8000 | random.below(3) as u32),
        ]);
        let mut instructions: Vec<Instruction> = setup
            .into_iter()
            .map(|operation| Instruction {
                clocks: 12,
                access: Some((4, operation)),
            })
            .collect();
        instructions.extend((0..4000).map(|_| {
            let clocks = 2 + random.below(23);
            let access = random.chance(10).then(|| {
                let operation = if random.chance(30) {
                    random_write(random)
                } else {
                    random_read(random)
                };
                (random.below(clocks), operation)
            });
            Instruction { clocks, access }
        }));
        let runs = (0..20)
            .map(|_| {
                let budget = 1 + random.below(4000);
                // PCS0/SS low sometimes: a mode fault
                let pins = random
                    .chance(20)
                    .then(|| !(u8::from(random.chance(30)) << 3));
                (budget, pins)
            })
            .collect();
        Self { instructions, runs }
    }
}

/// What a model showed: the request at each instruction's start, each
/// value read, and the state at each run's end; and what the
/// peripheral saw, with the instruction it was called in.
#[derive(Debug, PartialEq, Eq)]
struct Shown {
    requests: Vec<Option<(InterruptLevel, u8)>>,
    reads: Vec<u32>,
    states: Vec<(Vec<u8>, PortLevels, u8)>,
    seen: Vec<(Seen, u64)>,
}

/// Runs `program` on the QSM, syncing as `syncing` says and as the
/// chip's run loop does (an access that moves the next event before
/// the sync point ends the quantum after its instruction), with a
/// [`Logged`] peripheral, deferrable or not, or none.
fn run_program(program: &Program, syncing: Syncing, peripheral: Option<bool>) -> (Shown, u64) {
    let mut qsm = Qsm::new(Frequency::from_hz(SYSTEM_HZ));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let instructions = Rc::new(Cell::new(0));
    if let Some(deferrable) = peripheral {
        qsm.attach(Box::new(Logged {
            seen: Rc::clone(&seen),
            instructions: Rc::clone(&instructions),
            deferrable,
        }));
    }
    let mut shown = Shown {
        requests: Vec::new(),
        reads: Vec::new(),
        states: Vec::new(),
        seen: Vec::new(),
    };
    let mut now = Cycles::ZERO;
    let mut quanta = 0;
    let mut code = program.instructions.iter().cycle();
    for &(budget, pins) in &program.runs {
        let end = now + Cycles(budget);
        while now < end {
            quanta += 1;
            let next = next_event(&qsm, syncing).map_or(end, |next| next.min(end));
            let quantum_end = now + now.until(next).max(Cycles(1));
            loop {
                let request = qsm.request();
                shown
                    .requests
                    .push(request.map(|request| (request.level, request.arbitration)));
                let instruction = code.next().expect("a program");
                instructions.set(instructions.get() + 1);
                let mut sync = false;
                if let Some((offset, operation)) = instruction.access {
                    let at = now + Cycles(offset);
                    let access = |offset, size| RegisterAccess {
                        offset,
                        size,
                        supervisor: true,
                        now: at,
                    };
                    match operation {
                        Operation::Read(offset, size) => {
                            shown.reads.push(qsm.read(access(offset, size)).unwrap());
                        }
                        Operation::Write(offset, size, value) => {
                            assert!(qsm.write(access(offset, size), value));
                        }
                    }
                    sync = next_event(&qsm, syncing).is_some_and(|next| next < quantum_end);
                }
                now += Cycles(instruction.clocks);
                if now >= quantum_end || sync {
                    break;
                }
            }
            qsm.advance(now);
        }
        shown
            .states
            .push((qsm.save().unwrap(), qsm.port_outputs(), qsm.chip_selects()));
        if let Some(pins) = pins {
            qsm.set_port_pins(pins, now);
        }
    }
    shown.seen = seen.take();
    (shown, quanta)
}

#[test]
fn batched_entries_show_what_entry_by_entry_does() {
    let mut random = Random(0x6833_1000_5eed);
    let mut quanta = [[0; 2]; 3];
    let (mut requested, mut transfers) = (0, 0);
    for case in 0..300 {
        let program = Program::new(&mut random);
        for (index, peripheral) in [None, Some(false), Some(true)].into_iter().enumerate() {
            let (reference, per_phase) = run_program(&program, Syncing::PerPhase, peripheral);
            let (mut batched, seen) = run_program(&program, Syncing::Seen, peripheral);
            if peripheral == Some(true) {
                // deferred: the same calls in the same order, later
                for ((_, at), &(_, reference_at)) in batched.seen.iter_mut().zip(&reference.seen) {
                    assert!(*at >= reference_at, "case {case}: called early");
                    *at = reference_at;
                }
            }
            assert_eq!(batched, reference, "case {case}, {peripheral:?}");
            quanta[index][0] += per_phase;
            quanta[index][1] += seen;
            requested += reference.requests.iter().flatten().count();
            transfers += reference
                .seen
                .iter()
                .filter(|(seen, _)| matches!(seen, Seen::Transfer(..)))
                .count();
        }
    }
    eprintln!("quanta {quanta:?}, requested {requested}, transfers {transfers}");
    // the programs interrupt and transfer, and batching saves quanta
    assert!(requested > 10_000 && transfers > 10_000);
    for [per_phase, seen] in quanta {
        assert!(seen < per_phase, "{quanta:?}");
    }
}
