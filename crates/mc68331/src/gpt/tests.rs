use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_modules::module::Module;
use mc683xx_modules::module::RegisterAccess;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::pins::PortLevels;
use mc683xx_target::size::Size;

use super::Gpt;
use super::Input;
use super::PortGpPin;
use super::PwmChange;
use super::STATE_VERSION;
use super::pwm::Channel;
use super::pwm::Output;
use super::timer::Counter;
use crate::port::OutputChange;
use crate::port::Port;
use crate::port::PortChange;

fn access(offset: u32, size: Size, now: u64) -> RegisterAccess {
    RegisterAccess {
        offset,
        size,
        supervisor: true,
        now: Cycles::from_clocks(now),
    }
}

fn read(gpt: &mut Gpt, offset: u32, now: u64) -> u32 {
    gpt.read(access(offset, Size::Word, now)).unwrap()
}

fn read_byte(gpt: &mut Gpt, offset: u32, now: u64) -> u32 {
    gpt.read(access(offset, Size::Byte, now)).unwrap()
}

fn write(gpt: &mut Gpt, offset: u32, value: u32, now: u64) {
    assert!(gpt.write(access(offset, Size::Word, now), value));
}

fn write_byte(gpt: &mut Gpt, offset: u32, value: u32, now: u64) {
    assert!(gpt.write(access(offset, Size::Byte, now), value));
}

/// The GPT with IARB 7, level 6 and vectors from $50: GPTMCR $0007,
/// ICR $0650.
fn gpt() -> Gpt {
    let mut gpt = Gpt::new(Cycles::ZERO);
    write(&mut gpt, 0x00, 0x0007, 0);
    write(&mut gpt, 0x04, 0x0650, 0);
    gpt
}

#[test]
fn registers_out_of_reset() {
    let mut gpt = Gpt::new(Cycles::ZERO);
    // MC68331UM D.2; GPTRM 3.2.1, 3.3.2; PAIS and PCLKS show the pins
    for (offset, value) in [
        (0x00, 0x0080),
        (0x02, 0x0000),
        (0x04, 0x0000),
        (0x06, 0x00ff),
        (0x08, 0x0000),
        (0x0a, 0x0000),
        (0x0c, 0x8800),
        (0x0e, 0xffff),
        (0x10, 0xffff),
        (0x12, 0xffff),
        (0x14, 0xffff),
        (0x1a, 0xffff),
        (0x1c, 0xffff),
        (0x1e, 0x0000),
        (0x20, 0x0000),
        (0x22, 0x0000),
        (0x24, 0x0000),
        (0x26, 0x0000),
        (0x28, 0x0000),
        (0x2a, 0x0000),
        (0x2c, 0x0000),
        (0x2e, 0x0000),
    ] {
        assert_eq!(read(&mut gpt, offset, 0), value, "{offset:#x}");
    }
    assert_eq!(gpt.request(), None);
    assert_eq!(gpt.next_event(), None);
}

#[test]
fn tcnt_counts_the_prescaler_tap() {
    let mut gpt = gpt();
    // the system clock / 4 out of reset (GPTRM 3.1)
    assert_eq!(read(&mut gpt, 0x0a, 4 * 0x1234 + 3), 0x1234);
    assert_eq!(read(&mut gpt, 0x2c, 0x345), 0x145, "PRESCL's 9 bits");
    assert_eq!(read(&mut gpt, 0x28, 1001), 500, "PWMCNT at / 2");
    // CPR 3, / 32, from TCNT $1234 on; a second write changes nothing
    let now = 4 * 0x1234;
    write_byte(&mut gpt, 0x21, 0x03, now);
    write_byte(&mut gpt, 0x21, 0x00, now);
    assert_eq!(read(&mut gpt, 0x20, now), 0x0003);
    assert_eq!(read(&mut gpt, 0x0a, now + 32 * 10), 0x1234 + 10);
    // TCNT is read-only
    write(&mut gpt, 0x0a, 0, now);
    assert_eq!(read(&mut gpt, 0x0a, now), 0x1234);
}

#[test]
fn the_format_matches_its_golden() {
    let mut gpt = gpt();
    // TOC3 $100, OC3I, past the match
    write(&mut gpt, 0x18, 0x0100, 0);
    write_byte(&mut gpt, 0x20, 0x20, 0);
    gpt.advance(Cycles::from_clocks(4 * 0x100 + 1));
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "gpt",
        &[("gpt", STATE_VERSION)],
        &gpt.save().unwrap(),
    );
}

#[test]
fn a_state_no_run_could_save_is_refused() {
    let mut gpt = gpt();
    write(&mut gpt, 0x18, 0x0100, 0);
    let snapshot = gpt.save().unwrap();
    let edited = |change: fn(&mut Gpt)| {
        let mut saved: Gpt = decode(STATE_VERSION, &snapshot).unwrap();
        change(&mut saved);
        encode(STATE_VERSION, &saved)
    };
    let invalid: [(Vec<u8>, &str); 9] = [
        (edited(|saved| saved.icr = 0xffff), "GPT registers"),
        // CPR 1 with TCNT still on the reset tap
        (edited(|saved| saved.tmsk = 1), "GPT prescaler taps"),
        // PPR 7, PCLK, with PWMCNT still on the reset tap
        (
            edited(|saved| saved.pwm.control = 0x70),
            "GPT prescaler taps",
        ),
        (
            edited(|saved| {
                saved
                    .scheduled
                    .push_back((Cycles::from_clocks(20), Input::Pai, true));
                saved
                    .scheduled
                    .push_back((Cycles::from_clocks(10), Input::Pai, false));
            }),
            "GPT scheduled inputs",
        ),
        (edited(|saved| saved.tick = u64::MAX), "GPT counts"),
        (
            edited(|saved| saved.time = Cycles::from_clocks(u64::MAX)),
            "GPT counts",
        ),
        // TCNT synchronized at a prescaler count still to come
        (
            edited(|saved| saved.counter = Counter::new(saved.counter.divisor(), 1 << 40)),
            "GPT counts",
        ),
        (edited(|saved| saved.gated = u64::MAX), "GPT counts"),
        (
            edited(|saved| saved.pclk_rise = Some(Cycles::from_clocks(1 << 40))),
            "GPT counts",
        ),
    ];
    let mut other = self::gpt();
    let before = other.save().unwrap();
    for (bytes, what) in invalid {
        assert!(matches!(
            other.restore(&bytes),
            Err(StateError::Invalid(reason)) if reason == what
        ));
    }
    assert_eq!(other.save().unwrap(), before, "unchanged by the refusals");
    let pin = |bit: u8| decode::<PortGpPin>(STATE_VERSION, &encode(STATE_VERSION, &bit));
    assert_eq!(pin(7).unwrap(), PortGpPin::new(7).unwrap());
    assert!(matches!(pin(8), Err(StateError::Decode(_))));
}

#[test]
fn a_restore_finds_the_next_event_again() {
    let mut gpt = gpt();
    // TOC3 $100, OC3I: the snapshot leaves the cached event out
    write(&mut gpt, 0x18, 0x0100, 0);
    write_byte(&mut gpt, 0x20, 0x20, 0);
    let snapshot = gpt.save().unwrap();
    let mut restored = Gpt::new(Cycles::ZERO);
    assert_eq!(restored.next_event(), None);
    restored.restore(&snapshot).unwrap();
    assert_eq!(restored.next_event(), Some(Cycles::from_clocks(4 * 0x100)));
}

#[test]
fn output_compare_3_interrupts_when_tcnt_reaches_it() {
    let mut gpt = gpt();
    // TOC3 $100, OC3I
    write(&mut gpt, 0x18, 0x0100, 0);
    write_byte(&mut gpt, 0x20, 0x20, 0);
    assert_eq!(gpt.next_event(), Some(Cycles::from_clocks(4 * 0x100)));
    gpt.advance(Cycles::from_clocks(4 * 0x100 - 1));
    assert_eq!(gpt.request(), None);
    gpt.advance(Cycles::from_clocks(4 * 0x100));
    let six = InterruptLevel::new(6).unwrap();
    let request = gpt.request().unwrap();
    assert_eq!((request.level, request.arbitration), (six, 7));
    assert_eq!(
        gpt.acknowledge(six, Cycles::from_clocks(4 * 0x100)),
        Some(Vector(0x56))
    );
    assert_eq!(
        gpt.acknowledge(InterruptLevel::new(5).unwrap(), Cycles::ZERO),
        None
    );
    // a write of zero without the read clears nothing (GPTRM 7.1.2)
    write_byte(&mut gpt, 0x22, 0x00, 4 * 0x100);
    assert!(gpt.request().is_some());
    assert_eq!(read_byte(&mut gpt, 0x22, 4 * 0x100), 0x20);
    write_byte(&mut gpt, 0x22, 0xdf, 4 * 0x100);
    assert_eq!(gpt.request(), None);
    // the next match a TCNT period on
    assert_eq!(gpt.next_event(), Some(Cycles::from_clocks(4 * 0x1_0100)));
}

#[test]
fn a_flag_set_after_the_read_stays() {
    let mut gpt = gpt();
    write(&mut gpt, 0x16, 0x0010, 0);
    write(&mut gpt, 0x18, 0x0020, 0);
    gpt.advance(Cycles::from_clocks(4 * 0x10));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x10), 0x1000, "OC2F");
    // OC3F sets between the read and the write
    gpt.advance(Cycles::from_clocks(4 * 0x20));
    write(&mut gpt, 0x22, 0x0000, 4 * 0x20);
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x20), 0x2000);
}

#[test]
fn compares_drive_their_pins_and_oc1_wins() {
    let mut gpt = gpt();
    // OC2 toggles, OC3 sets; OC1 clears OC2's and OC3's pins
    write(&mut gpt, 0x1e, 0x0d00, 0);
    write(&mut gpt, 0x14, 0x0040, 0);
    write(&mut gpt, 0x16, 0x0010, 0);
    write(&mut gpt, 0x18, 0x0040, 0);
    write(&mut gpt, 0x08, 0x3000, 0);
    assert_eq!(gpt.port_outputs(Cycles::ZERO), PortLevels::new(0x30, 0x00));
    // the pin actions show, so a run syncs at them
    assert_eq!(gpt.next_event(), Some(Cycles::from_clocks(4 * 0x10)));
    gpt.advance(Cycles::from_clocks(4 * 0x10));
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels::new(0x30, 0x10),
        "OC2 toggled"
    );
    gpt.advance(Cycles::from_clocks(4 * 0x40));
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels::new(0x30, 0x00),
        "OC1 over OC3"
    );
    // CFORC: OC3's action, no flag
    let flags = read(&mut gpt, 0x22, 4 * 0x40);
    write_byte(&mut gpt, 0x24, 0x20, 4 * 0x40);
    assert_eq!(gpt.port_outputs(Cycles::ZERO), PortLevels::new(0x30, 0x20));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x40), flags);
    assert_eq!(read_byte(&mut gpt, 0x24, 4 * 0x40), 0, "FOC reads zero");
    // a disconnected compare's pin is PORTGP's again
    write(&mut gpt, 0x1e, 0x0000, 4 * 0x40);
    write(&mut gpt, 0x08, 0x0000, 4 * 0x40);
    write(&mut gpt, 0x06, 0x30a0, 4 * 0x40);
    assert_eq!(gpt.port_outputs(Cycles::ZERO), PortLevels::new(0x30, 0x20));
    assert_eq!(read_byte(&mut gpt, 0x07, 4 * 0x40), 0xef);
}

#[test]
fn a_pin_edge_is_captured() {
    let mut gpt = gpt();
    // EDGE1 rising, IC1I
    write(&mut gpt, 0x1e, 0x0001, 0);
    write_byte(&mut gpt, 0x20, 0x01, 0);
    gpt.set_port_pins(0xfe, u8::MAX, Cycles::from_clocks(4 * 0x100));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x100), 0, "a falling edge");
    gpt.set_port_pins(0xff, u8::MAX, Cycles::from_clocks(4 * 0x1234 + 2));
    assert_eq!(read(&mut gpt, 0x0e, 4 * 0x1234 + 2), 0x1234);
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x1234 + 2), 0x0100, "IC1F");
    let six = InterruptLevel::new(6).unwrap();
    assert_eq!(gpt.acknowledge(six, Cycles::ZERO), Some(Vector(0x51)));
    // input capture 4 on any edge, on PGP7
    write(&mut gpt, 0x0c, 0x0400, 4 * 0x2000);
    write(&mut gpt, 0x1e, 0x00c0, 4 * 0x2000);
    gpt.set_port_pins(0x7f, u8::MAX, Cycles::from_clocks(4 * 0x2000));
    assert_eq!(read(&mut gpt, 0x1c, 4 * 0x2000), 0x2000);
    write(&mut gpt, 0x1c, 0, 4 * 0x2000);
    assert_eq!(
        read(&mut gpt, 0x1c, 4 * 0x2000),
        0x2000,
        "TIC4 is read-only"
    );
}

#[test]
fn a_write_to_an_output_pin_is_captured() {
    let mut gpt = gpt();
    // PGP1 an output; EDGE2 falling (GPTRM 8.1)
    write(&mut gpt, 0x06, 0x0202, 0);
    write(&mut gpt, 0x1e, 0x0008, 0);
    write(&mut gpt, 0x06, 0x0200, 4 * 9);
    assert_eq!(read(&mut gpt, 0x10, 4 * 9), 9);
    assert_eq!(read(&mut gpt, 0x22, 4 * 9), 0x0200, "IC2F");
}

#[test]
fn a_byte_write_changes_only_its_lane_of_ddrgp_and_portgp() {
    let mut gpt = gpt();
    // the board holds every pin low; PORTGP reads the pins, not its
    // latch (GPTRM 8.1)
    gpt.set_port_pins(0x00, u8::MAX, Cycles::ZERO);
    write_byte(&mut gpt, 0x07, 0xff, 0);
    assert_eq!(read_byte(&mut gpt, 0x07, 0), 0x00);
    write_byte(&mut gpt, 0x06, 0x0f, 0);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels::new(0x0f, 0x0f),
        "the latch kept"
    );
    write_byte(&mut gpt, 0x07, 0x05, 0);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels::new(0x0f, 0x05),
        "DDRGP kept"
    );
    // OC2 clears its pin, PGP4, at its compare (GPTRM 3.3.1); the latch
    // under it is PORTGP's, not the pin's level
    write_byte(&mut gpt, 0x07, 0xff, 0);
    write(&mut gpt, 0x1e, 0x0200, 0);
    write_byte(&mut gpt, 0x06, 0x1f, 0);
    assert_eq!(gpt.port_outputs(Cycles::ZERO), PortLevels::new(0x1f, 0x0f));
    assert_eq!(read_byte(&mut gpt, 0x07, 0), 0x0f);
    write_byte(&mut gpt, 0x06, 0x1f, 0);
    write(&mut gpt, 0x1e, 0x0000, 0);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels::new(0x1f, 0x1f),
        "the latch again"
    );
}

#[test]
fn lpstop_holds_tcnt_and_the_inputs_until_the_wake() {
    let mut gpt = gpt();
    // OC3 at $100 with OC3I; EDGE1 falling, IC1I
    write(&mut gpt, 0x18, 0x0100, 0);
    write(&mut gpt, 0x1e, 0x0002, 0);
    write(&mut gpt, 0x20, 0x2100, 0);
    gpt.set_low_power(true, Cycles::from_clocks(4 * 0x10));
    assert_eq!(gpt.next_event(), None);
    gpt.advance(Cycles::from_clocks(4 * 0x1000));
    assert_eq!(read(&mut gpt, 0x0a, 4 * 0x1000), 0x10, "TCNT holds");
    // a level the board drives while stopped is seen at the wake
    // (MC68331UM 4.3.4)
    gpt.set_port_pins(0xfe, u8::MAX, Cycles::from_clocks(4 * 0x1000));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x1000), 0, "no capture");
    gpt.set_low_power(false, Cycles::from_clocks(4 * 0x2000));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x2000), 0x0100, "IC1F at the wake");
    assert_eq!(read(&mut gpt, 0x0e, 4 * 0x2000), 0x10);
    // TCNT runs on from $10: OC3 a $F0 counts later
    assert_eq!(
        gpt.next_event(),
        Some(Cycles::from_clocks(4 * (0x2000 + 0xf0)))
    );
}

#[test]
fn tcnt_overflow_sets_tof() {
    let mut gpt = gpt();
    // TOI
    write_byte(&mut gpt, 0x21, 0x80, 0);
    assert_eq!(gpt.next_event(), Some(Cycles::from_clocks(4 << 16)));
    gpt.advance(Cycles::from_clocks(4 << 16));
    // the compares at their reset value $FFFF matched on the way
    assert_eq!(read(&mut gpt, 0x22, 4 << 16), 0xf880);
    let six = InterruptLevel::new(6).unwrap();
    assert_eq!(gpt.acknowledge(six, Cycles::ZERO), Some(Vector(0x59)));
}

#[test]
fn pab_moves_a_source_to_the_top() {
    let mut gpt = gpt();
    // OC1 and OC4 pending; PAB 7 puts OC4 first, as vector $X0
    write(&mut gpt, 0x14, 0x0001, 0);
    write(&mut gpt, 0x1a, 0x0001, 0);
    write_byte(&mut gpt, 0x20, 0x48, 0);
    gpt.advance(Cycles::from_clocks(4));
    let six = InterruptLevel::new(6).unwrap();
    assert_eq!(
        gpt.acknowledge(six, Cycles::from_clocks(4)),
        Some(Vector(0x54))
    );
    write(&mut gpt, 0x04, 0x7650, 4);
    assert_eq!(
        gpt.acknowledge(six, Cycles::from_clocks(4)),
        Some(Vector(0x50))
    );
}

#[test]
fn the_pulse_accumulator_counts_events_and_overflows() {
    let mut gpt = gpt();
    // PAEN, rising edges; PACNT $FE
    write(&mut gpt, 0x0c, 0x50fe, 0);
    gpt.set_input(Input::Pai, false, Cycles::from_clocks(1));
    gpt.set_input(Input::Pai, true, Cycles::from_clocks(2));
    assert_eq!(read(&mut gpt, 0x0c, 2), 0xd8ff, "PAIS, PCLKS");
    assert_eq!(read(&mut gpt, 0x22, 2), 0x0010, "PAIF");
    gpt.set_input(Input::Pai, false, Cycles::from_clocks(3));
    gpt.set_input(Input::Pai, true, Cycles::from_clocks(4));
    assert_eq!(read_byte(&mut gpt, 0x0d, 4), 0);
    assert_eq!(read(&mut gpt, 0x22, 4), 0x0030, "PAOVF");
}

#[test]
fn gated_accumulation_counts_while_pai_is_high() {
    let mut gpt = gpt();
    // PAEN, PAMOD, PEDGE clear: a zero inhibits; the system clock / 512
    gpt.set_input(Input::Pai, false, Cycles::ZERO);
    write(&mut gpt, 0x0c, 0x6000, 0);
    gpt.set_input(Input::Pai, true, Cycles::from_clocks(512 * 10 + 100));
    gpt.set_input(Input::Pai, false, Cycles::from_clocks(512 * 30));
    assert_eq!(read_byte(&mut gpt, 0x0d, 512 * 40), 20);
    assert_eq!(read(&mut gpt, 0x22, 512 * 40), 0x0010, "PAIF at the end");
    // PAOVI: the overflow is an event
    write(&mut gpt, 0x0c, 0x60f0, 512 * 40);
    write_byte(&mut gpt, 0x21, 0x20, 512 * 40);
    gpt.set_input(Input::Pai, true, Cycles::from_clocks(512 * 40));
    assert_eq!(gpt.next_event(), Some(Cycles::from_clocks(512 * 56)));
}

#[test]
fn pclk_can_clock_tcnt() {
    let mut gpt = gpt();
    write_byte(&mut gpt, 0x21, 0x07, 0);
    write(&mut gpt, 0x14, 0x0002, 0);
    for edge in 1..=2 {
        gpt.set_input(Input::Pclk, false, Cycles::from_clocks(edge * 10));
        gpt.set_input(Input::Pclk, true, Cycles::from_clocks(edge * 10 + 5));
    }
    assert_eq!(read(&mut gpt, 0x0a, 1000), 2);
    assert_eq!(read(&mut gpt, 0x22, 1000), 0x0800, "OC1F");
    assert_eq!(gpt.next_event(), None, "no time for PCLK's edges");
}

#[test]
fn stopp_freezes_and_incp_steps() {
    let mut gpt = gpt();
    write(&mut gpt, 0x00, 0x1007, 40);
    assert_eq!(read(&mut gpt, 0x0a, 4000), 10);
    write(&mut gpt, 0x00, 0x1807, 4000);
    write(&mut gpt, 0x00, 0x1807, 4000);
    write(&mut gpt, 0x00, 0x1807, 4000);
    write(&mut gpt, 0x00, 0x1807, 4000);
    assert_eq!(read(&mut gpt, 0x0a, 4000), 11);
    assert_eq!(read(&mut gpt, 0x00, 4000), 0x1007, "INCP negates itself");
    // the pins are ignored until STOPP clears
    write(&mut gpt, 0x1e, 0x0001, 4000);
    gpt.set_port_pins(0xfe, u8::MAX, Cycles::from_clocks(4000));
    gpt.set_port_pins(0xff, u8::MAX, Cycles::from_clocks(4001));
    assert_eq!(read(&mut gpt, 0x22, 4001), 0);
    gpt.set_port_pins(0xfe, u8::MAX, Cycles::from_clocks(4002));
    write(&mut gpt, 0x00, 0x0007, 5000);
    assert_eq!(read(&mut gpt, 0x06, 5000), 0x00fe);
    assert_eq!(read(&mut gpt, 0x0a, 5000 + 4), 12);
}

/// What stops the input synchronizers in a test (GPTRM 9.3, 9.4).
#[derive(Debug, Clone, Copy)]
enum Hold {
    /// GPTMCR's STOPP.
    Stopp,
    /// FREEZE with FRZ0.
    Freeze,
}

impl Hold {
    /// Both.
    const BOTH: [Self; 2] = [Self::Stopp, Self::Freeze];

    /// Stops the GPT at `now` if `held`, or starts it again.
    fn set(self, gpt: &mut Gpt, held: bool, now: u64) {
        match self {
            Self::Stopp => write(gpt, 0x00, if held { 0x1007 } else { 0x0007 }, now),
            Self::Freeze => {
                write(gpt, 0x00, 0x2007, now);
                gpt.set_freeze(held, Cycles::from_clocks(now));
            }
        }
    }
}

#[test]
fn a_forced_compare_is_not_captured_while_held() {
    for hold in Hold::BOTH {
        let mut gpt = gpt();
        // OC1M7 drives PGP7 low, then input capture 4 on any edge
        write(&mut gpt, 0x08, 0x8000, 0);
        write(&mut gpt, 0x0c, 0x0400, 0);
        write(&mut gpt, 0x1e, 0x00c0, 0);
        hold.set(&mut gpt, true, 4 * 0x10);
        // FOC1 drives OC1D7 high, then low again (GPTRM 3.3.4, 9.3)
        write(&mut gpt, 0x08, 0x8080, 4 * 0x10);
        write_byte(&mut gpt, 0x24, 0x08, 4 * 0x10);
        assert_eq!(
            gpt.port_outputs(Cycles::from_clocks(4 * 0x10)).levels(),
            0x80,
            "{hold:?}"
        );
        write(&mut gpt, 0x08, 0x8000, 4 * 0x10);
        write_byte(&mut gpt, 0x24, 0x08, 4 * 0x10);
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x10), 0, "{hold:?}: no I4O5F");
        hold.set(&mut gpt, false, 4 * 0x20);
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x20), 0, "{hold:?}: no edge left");
        assert_eq!(read(&mut gpt, 0x1c, 4 * 0x20), 0xffff, "{hold:?}");
    }
    // FREEZE without FRZ0 is ignored: the forced compare is captured
    let mut gpt = gpt();
    write(&mut gpt, 0x08, 0x8080, 0);
    write(&mut gpt, 0x0c, 0x0400, 0);
    write(&mut gpt, 0x1e, 0x00c0, 0);
    gpt.set_freeze(true, Cycles::from_clocks(4 * 0x10));
    write_byte(&mut gpt, 0x24, 0x08, 4 * 0x10);
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x10), 0x8000, "I4O5F");
    assert_eq!(read(&mut gpt, 0x1c, 4 * 0x10), 0x10);
}

#[test]
fn a_portgp_write_is_seen_only_as_the_synchronizers_are_clocked() {
    for hold in Hold::BOTH {
        let mut gpt = gpt();
        // PGP1 an output, high; EDGE2 falling
        write(&mut gpt, 0x06, 0x0202, 0);
        write(&mut gpt, 0x1e, 0x0008, 0);
        hold.set(&mut gpt, true, 4 * 0x10);
        write(&mut gpt, 0x06, 0x0200, 4 * 0x10);
        write(&mut gpt, 0x06, 0x0202, 4 * 0x10);
        write(&mut gpt, 0x06, 0x0200, 4 * 0x10);
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x10), 0, "{hold:?}: no IC2F");
        // the level that changed is an edge once they are clocked
        hold.set(&mut gpt, false, 4 * 0x20);
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x20), 0x0200, "{hold:?}: IC2F");
        assert_eq!(read(&mut gpt, 0x10, 4 * 0x20), 0x10, "{hold:?}: TCNT held");
    }
}

#[test]
fn incp_clocks_the_synchronizers_once() {
    let mut gpt = gpt();
    // PGP1 an output, high; EDGE2 falling; STOPP
    write(&mut gpt, 0x06, 0x0202, 0);
    write(&mut gpt, 0x1e, 0x0008, 0);
    write(&mut gpt, 0x00, 0x1007, 4 * 0x10);
    write(&mut gpt, 0x06, 0x0200, 4 * 0x10);
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x10), 0);
    // INCP (GPTRM 9.4): TCNT's tap is 4, so the capture holds $10
    write(&mut gpt, 0x00, 0x1807, 4 * 0x10 + 2);
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x10 + 2), 0x0200, "IC2F");
    assert_eq!(read(&mut gpt, 0x10, 4 * 0x10 + 2), 0x10);
}

#[test]
fn an_external_edge_is_not_captured_while_held() {
    for hold in Hold::BOTH {
        let mut gpt = gpt();
        // EDGE1 falling
        write(&mut gpt, 0x1e, 0x0002, 0);
        hold.set(&mut gpt, true, 4 * 0x10);
        // a pulse is lost
        gpt.set_port_pins(0xfe, u8::MAX, Cycles::from_clocks(4 * 0x10 + 1));
        gpt.set_port_pins(0xff, u8::MAX, Cycles::from_clocks(4 * 0x10 + 2));
        hold.set(&mut gpt, false, 4 * 0x20);
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x20), 0, "{hold:?}: pulse lost");
        // a level changed while held is an edge once they are clocked
        hold.set(&mut gpt, true, 4 * 0x30);
        gpt.set_port_pins(0xfe, u8::MAX, Cycles::from_clocks(4 * 0x30 + 1));
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x40), 0, "{hold:?}");
        hold.set(&mut gpt, false, 4 * 0x40);
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x40), 0x0100, "{hold:?}: IC1F");
        // TCNT held at $30, then counted on from it
        assert_eq!(read(&mut gpt, 0x0e, 4 * 0x40), 0x20, "{hold:?}");
    }
}

#[test]
fn the_pulse_accumulator_ignores_pai_while_held() {
    for hold in Hold::BOTH {
        let mut gpt = gpt();
        // PAEN, rising edges; PAI low
        write(&mut gpt, 0x0c, 0x5000, 0);
        gpt.set_input(Input::Pai, false, Cycles::from_clocks(1));
        hold.set(&mut gpt, true, 4 * 0x10);
        gpt.set_input(Input::Pai, true, Cycles::from_clocks(4 * 0x10 + 1));
        gpt.set_input(Input::Pai, false, Cycles::from_clocks(4 * 0x10 + 2));
        gpt.set_input(Input::Pai, true, Cycles::from_clocks(4 * 0x10 + 3));
        assert_eq!(read_byte(&mut gpt, 0x0d, 4 * 0x20), 0, "{hold:?}: PACNT");
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x20), 0, "{hold:?}: no PAIF");
        // the level that rose while held is an edge once they are clocked
        hold.set(&mut gpt, false, 4 * 0x20);
        assert_eq!(read_byte(&mut gpt, 0x0d, 4 * 0x20), 1, "{hold:?}: PACNT");
        assert_eq!(read(&mut gpt, 0x22, 4 * 0x20), 0x0010, "{hold:?}: PAIF");
    }
}

#[test]
fn freeze_lets_tcnt_be_written_whatever_frz0() {
    let mut gpt = gpt();
    // read-only while FREEZE is negated (GPTRM 3.1)
    write(&mut gpt, 0x0a, 0x1000, 100);
    assert_eq!(read(&mut gpt, 0x0a, 100), 25);
    // FRZ0 clear: writable all the same, counting on (GPTRM 9.3); the
    // compares at their reset value $FFFF do not match the write
    gpt.set_freeze(true, Cycles::from_clocks(100));
    write(&mut gpt, 0x0a, 0xffff, 100);
    assert_eq!(read(&mut gpt, 0x0a, 100), 0xffff);
    assert_eq!(read(&mut gpt, 0x22, 103), 0);
    assert_eq!(read(&mut gpt, 0x0a, 104), 0);
    assert_eq!(read(&mut gpt, 0x22, 104), 0x0080, "TOF as it rolls over");
    // FRZ0: TCNT holds what is written
    write(&mut gpt, 0x00, 0x2007, 200);
    write(&mut gpt, 0x0a, 0x1234, 200);
    assert_eq!(read(&mut gpt, 0x0a, 10_000), 0x1234);
    gpt.set_freeze(false, Cycles::from_clocks(10_000));
    assert_eq!(read(&mut gpt, 0x0a, 10_003), 0x1234);
    assert_eq!(read(&mut gpt, 0x0a, 10_004), 0x1235);
    write(&mut gpt, 0x0a, 0, 10_004);
    assert_eq!(read(&mut gpt, 0x0a, 10_004), 0x1235, "read-only again");
}

#[test]
fn freeze_lets_the_captures_be_written() {
    let mut gpt = gpt();
    // TIC2, and TI4/O5 as input capture 4, are read-only (GPTRM 3.2.1,
    // 3.4)
    write_byte(&mut gpt, 0x0c, 0x04, 0);
    write(&mut gpt, 0x10, 0x1234, 0);
    write(&mut gpt, 0x1c, 0x5678, 0);
    assert_eq!(read(&mut gpt, 0x10, 0), 0xffff);
    assert_eq!(read(&mut gpt, 0x1c, 0), 0xffff);
    gpt.set_freeze(true, Cycles::from_clocks(0));
    write(&mut gpt, 0x10, 0x1234, 0);
    write(&mut gpt, 0x1c, 0x5678, 0);
    assert_eq!(read(&mut gpt, 0x10, 0), 0x1234);
    assert_eq!(read(&mut gpt, 0x1c, 0), 0x5678);
}

#[test]
fn freeze_lets_cpr_be_written_again() {
    let mut gpt = gpt();
    // CPR 3, then 1 while FREEZE is asserted (GPTRM 5.1, 9.3)
    write_byte(&mut gpt, 0x21, 0x03, 0);
    gpt.set_freeze(true, Cycles::from_clocks(0));
    write_byte(&mut gpt, 0x21, 0x01, 0);
    assert_eq!(read(&mut gpt, 0x20, 0), 0x0001);
    assert_eq!(read(&mut gpt, 0x0a, 80), 10, "the system clock / 8");
    gpt.set_freeze(false, Cycles::from_clocks(80));
    write_byte(&mut gpt, 0x21, 0x02, 80);
    assert_eq!(read(&mut gpt, 0x20, 80), 0x0001, "written once");
    // a write while FREEZE is asserted is the one write
    let mut gpt = self::gpt();
    gpt.set_freeze(true, Cycles::from_clocks(0));
    write_byte(&mut gpt, 0x21, 0x01, 0);
    gpt.set_freeze(false, Cycles::from_clocks(0));
    write_byte(&mut gpt, 0x21, 0x02, 0);
    assert_eq!(read(&mut gpt, 0x20, 0), 0x0001);
}

#[test]
fn freeze_lets_the_prescaler_be_written() {
    let mut gpt = gpt();
    // read-only while FREEZE is negated (GPTRM 5.1)
    write(&mut gpt, 0x2c, 0, 500);
    assert_eq!(read(&mut gpt, 0x2c, 500), 0x1f4);
    // FRZ0: TCNT 250 and PWMCNT 500 hold, the prescaler $1FD
    write(&mut gpt, 0x00, 0x2007, 500);
    gpt.set_freeze(true, Cycles::from_clocks(1000));
    write(&mut gpt, 0x2c, 0xfffd, 1000);
    assert_eq!(read(&mut gpt, 0x2c, 2000), 0x1fd);
    assert_eq!(read(&mut gpt, 0x0a, 2000), 250);
    assert_eq!(read(&mut gpt, 0x28, 2000), 500);
    // the taps clock their counters as the new value counts to them:
    // PWMCNT at $1FE, TCNT at $200
    gpt.set_freeze(false, Cycles::from_clocks(2000));
    assert_eq!(read(&mut gpt, 0x28, 2001), 501);
    assert_eq!(read(&mut gpt, 0x0a, 2002), 250);
    assert_eq!(read(&mut gpt, 0x0a, 2003), 251);
    assert_eq!(read(&mut gpt, 0x2c, 2003), 0);
}

#[test]
fn freeze_lets_pwmcnt_be_written_and_a_falling_top_bit_starts_a_period() {
    let mut gpt = gpt();
    // FRZ0; PWMA $80 from the next period, fast at the system clock / 2
    write(&mut gpt, 0x00, 0x2007, 0);
    write(&mut gpt, 0x26, 0x8000, 0);
    write(&mut gpt, 0x28, 0x1234, 0);
    assert_eq!(read(&mut gpt, 0x28, 0), 0, "read-only (GPTRM 6.1)");
    gpt.set_freeze(true, Cycles::from_clocks(0x180));
    assert_eq!(read(&mut gpt, 0x28, 0x180), 0xc0);
    assert_eq!(read(&mut gpt, 0x2a, 0x180), 0);
    // bit 7 from one to zero is a zero detect: PWMA's period starts
    // (GPTRM 6.1)
    write(&mut gpt, 0x28, 0x0005, 0x180);
    assert_eq!(read(&mut gpt, 0x28, 0x180), 5);
    assert_eq!(read(&mut gpt, 0x2a, 0x180), 0x8000);
    assert!(gpt.pwm_level(Channel::A, Cycles::from_clocks(0x180)));
    // bit 7 from zero, to zero or one, is none, whatever the count
    // passes
    write(&mut gpt, 0x26, 0x4000, 0x180);
    write(&mut gpt, 0x28, 0x0040, 0x180);
    write(&mut gpt, 0x28, 0x01c3, 0x180);
    assert_eq!(read(&mut gpt, 0x2a, 0x180), 0x8000);
    assert!(!gpt.pwm_level(Channel::A, Cycles::from_clocks(0x180)));
    // PWMA $40 from the period that starts as PWMCNT counts to $200
    gpt.set_freeze(false, Cycles::from_clocks(0x1000));
    assert_eq!(read(&mut gpt, 0x2a, 0x1000 + 2 * 0x3d - 1), 0x8000);
    assert_eq!(read(&mut gpt, 0x2a, 0x1000 + 2 * 0x3d), 0x4000);
}

#[test]
fn freeze_with_frz0_lets_compares_be_forced_and_pins_written() {
    let mut gpt = gpt();
    write(&mut gpt, 0x00, 0x2007, 0);
    gpt.set_freeze(true, Cycles::from_clocks(10));
    // OC3 sets PGP5, FOC3 forces it; PORTGP drives PGP0 (GPTRM 9.3)
    write(&mut gpt, 0x1e, 0x0c00, 10);
    write_byte(&mut gpt, 0x24, 0x20, 10);
    write(&mut gpt, 0x06, 0x0101, 10);
    assert_eq!(
        gpt.port_outputs(Cycles::from_clocks(10)),
        PortLevels::new(0x21, 0x21)
    );
}

#[test]
fn a_snapshot_keeps_what_freeze_mode_wrote() {
    let mut gpt = gpt();
    gpt.set_freeze(true, Cycles::from_clocks(100));
    write(&mut gpt, 0x0a, 0x8000, 100);
    write(&mut gpt, 0x2c, 0x01fd, 100);
    write(&mut gpt, 0x28, 0x1234, 100);
    let mut restored = Gpt::new(Cycles::ZERO);
    restored.restore(&gpt.save().unwrap()).unwrap();
    for offset in [0x0a, 0x28, 0x2c] {
        assert_eq!(
            read(&mut restored, offset, 150),
            read(&mut gpt, offset, 150),
            "{offset:#x}"
        );
    }
}

#[test]
fn pwm_duty_takes_effect_at_the_end_of_a_period() {
    let mut gpt = gpt();
    // PWMA $80 in fast mode at the system clock / 2: 512 clocks a
    // period
    write(&mut gpt, 0x26, 0x8040, 10);
    assert_eq!(
        gpt.pwm(Channel::A, Cycles::from_clocks(10)),
        Output::Level(false)
    );
    let next = 512 + 10;
    assert_eq!(
        gpt.pwm(Channel::A, Cycles::from_clocks(next)),
        Output::Wave {
            period: 256,
            high: 128,
            clocks: Some(Cycles::from_clocks(2))
        }
    );
    assert!(gpt.pwm_level(Channel::A, Cycles::from_clocks(512 + 255)));
    assert!(!gpt.pwm_level(Channel::A, Cycles::from_clocks(512 + 256)));
    assert!(gpt.pwm_level(Channel::B, Cycles::from_clocks(512 + 127)));
    assert!(!gpt.pwm_level(Channel::B, Cycles::from_clocks(512 + 128)));
    assert_eq!(read(&mut gpt, 0x2a, 600), 0x8040, "PWMBUFA and B");
    // slow mode for B at the system clock / 128: 128 x 32768 clocks
    write_byte(&mut gpt, 0x25, 0x64, 600);
    assert_eq!(
        gpt.pwm(Channel::B, Cycles::from_clocks(600)),
        Output::Wave {
            period: 32768,
            high: 64 * 128,
            clocks: Some(Cycles::from_clocks(128))
        }
    );
    // F1A: high from the next period; FPWMB: B a discrete output of F1B
    write_byte(&mut gpt, 0x25, 0x06, 600);
    write_byte(&mut gpt, 0x24, 0x01, 600);
    assert_eq!(
        gpt.pwm(Channel::B, Cycles::from_clocks(600)),
        Output::Level(false)
    );
    assert!(gpt.pwm_level(Channel::A, Cycles::from_clocks(1024 + 300)));
    assert_eq!(
        read_byte(&mut gpt, 0x25, 1024 + 300),
        0x06,
        "F1x read the pins"
    );
    // PPROUT: A carries PWMCNT's clock
    write_byte(&mut gpt, 0x25, 0x80, 2000);
    assert_eq!(
        gpt.pwm(Channel::A, Cycles::from_clocks(2000)),
        Output::Clock {
            clocks: Some(Cycles::from_clocks(2))
        }
    );
}

#[test]
fn supv_moves_the_registers_to_supervisor_space() {
    let mut gpt = Gpt::new(Cycles::ZERO);
    let user = |offset| RegisterAccess {
        supervisor: false,
        ..access(offset, Size::Word, 0)
    };
    assert_eq!(gpt.read(user(0x0a)), None, "SUPV set out of reset");
    write(&mut gpt, 0x00, 0x0007, 0);
    write(&mut gpt, 0x04, 0x0650, 0);
    assert_eq!(gpt.read(user(0x0a)), Some(0));
    assert_eq!(gpt.read(user(0x04)), Some(0), "ICR is supervisor-only");
    assert!(gpt.write(user(0x04), 0x0700));
    assert_eq!(read(&mut gpt, 0x04, 0), 0x0650);
}

#[test]
fn a_reset_returns_the_registers_and_keeps_the_pins() {
    let mut gpt = gpt();
    gpt.set_port_pins(0x0f, u8::MAX, Cycles::from_clocks(5));
    write(&mut gpt, 0x14, 0x1234, 5);
    gpt.reset(Cycles::from_clocks(100));
    assert_eq!(read(&mut gpt, 0x14, 100), 0xffff);
    assert_eq!(read(&mut gpt, 0x0a, 100 + 8), 2, "TCNT from the reset");
    assert_eq!(read(&mut gpt, 0x06, 100), 0x000f);
}

/// A change the GPT drove, without its time, as the tests compare them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// Port GP's pins it drives and their levels.
    PortGp(PortLevels),
    /// A PWM pin's level.
    Pwm {
        /// The pin.
        channel: Channel,
        /// Its level.
        level: bool,
    },
}

/// Port GP driving `driven` at `levels`.
const fn gp(driven: u8, levels: u8) -> Change {
    Change::PortGp(PortLevels::new(driven, levels))
}

/// PWMA at `level`.
const fn pwma(level: bool) -> Change {
    Change::Pwm {
        channel: Channel::A,
        level,
    }
}

/// Every change, port GP's and the PWM pins' together, in time order,
/// each with its time.
fn take_output_changes(gpt: &mut Gpt) -> Vec<(Cycles, Change)> {
    gpt.take_output_changes()
        .into_iter()
        .map(|change| match change {
            OutputChange::Port(change) => (change.now, Change::PortGp(change.outputs)),
            OutputChange::Pwm(change) => (
                change.now,
                Change::Pwm {
                    channel: change.channel,
                    level: change.level,
                },
            ),
        })
        .collect()
}

#[test]
fn port_and_pwm_changes_are_taken_apart_each_in_time_order() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    // PWMA $80, fast: high at 512 (GPTRM 6.2); OC3 sets PGP5 by CFORC
    write(&mut gpt, 0x26, 0x8000, 10);
    write(&mut gpt, 0x1e, 0x0c00, 20);
    write_byte(&mut gpt, 0x24, 0x20, 1000);
    gpt.advance(Cycles::from_clocks(1100));
    let port = |at, levels| PortChange {
        now: Cycles::from_clocks(at),
        port: Port::Gp,
        outputs: PortLevels::new(0x20, levels),
    };
    assert_eq!(gpt.take_port_changes(), [port(20, 0x00), port(1000, 0x20)]);
    assert_eq!(
        gpt.take_pwm_changes(),
        [
            PwmChange {
                now: Cycles::from_clocks(512),
                channel: Channel::A,
                level: true,
            },
            PwmChange {
                now: Cycles::from_clocks(768),
                channel: Channel::A,
                level: false,
            },
            PwmChange {
                now: Cycles::from_clocks(1024),
                channel: Channel::A,
                level: true,
            },
        ]
    );
    assert_eq!(gpt.take_output_changes(), []);
    assert_eq!(gpt.take_pwm_changes(), []);
    assert_eq!(gpt.take_port_changes(), []);
    assert!(gpt.edges.port_queue.is_empty() && gpt.edges.pwm_queue.is_empty());
}

#[test]
fn port_and_pwm_changes_at_one_time_keep_their_order() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    // FPWMA with F1A: PWMA high at once (GPTRM 6.3.1); then PORTGP drives
    // PGP0 low, then PWMA low, all at one time
    write(&mut gpt, 0x24, 0x0202, 100);
    write(&mut gpt, 0x06, 0x0100, 100);
    write(&mut gpt, 0x24, 0x0200, 100);
    let pwm = |level| {
        OutputChange::Pwm(PwmChange {
            now: Cycles::from_clocks(100),
            channel: Channel::A,
            level,
        })
    };
    let port = OutputChange::Port(PortChange {
        now: Cycles::from_clocks(100),
        port: Port::Gp,
        outputs: PortLevels::new(0x01, 0x00),
    });
    assert_eq!(gpt.take_output_changes(), [pwm(true), port, pwm(false)]);
    assert_eq!(gpt.take_port_changes(), []);
    assert_eq!(gpt.take_pwm_changes(), []);
}

#[test]
fn each_take_drains_what_it_gives_and_the_rest_keeps_its_order() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    let now = Cycles::from_clocks(100);
    let pwm = |level| PwmChange {
        now,
        channel: Channel::A,
        level,
    };
    let port = |levels| PortChange {
        now,
        port: Port::Gp,
        outputs: PortLevels::new(0x01, levels),
    };
    // PWMA high, PGP0 low, then, the PWM edge taken, PWMA low and PGP0
    // high, all at one time: the port change left queued keeps its
    // place before the edges found after it
    write(&mut gpt, 0x24, 0x0202, 100);
    write(&mut gpt, 0x06, 0x0100, 100);
    assert_eq!(gpt.take_pwm_changes(), [pwm(true)]);
    write(&mut gpt, 0x24, 0x0200, 100);
    write(&mut gpt, 0x06, 0x0101, 100);
    assert_eq!(
        gpt.take_output_changes(),
        [
            OutputChange::Port(port(0x00)),
            OutputChange::Pwm(pwm(false)),
            OutputChange::Port(port(0x01)),
        ]
    );
    // ports alone, then the PWM pins' alone: nothing left behind
    write(&mut gpt, 0x24, 0x0202, 100);
    write(&mut gpt, 0x06, 0x0100, 100);
    assert_eq!(gpt.take_port_changes(), [port(0x00)]);
    assert_eq!(gpt.edges.port_queue, []);
    assert_eq!(gpt.take_pwm_changes(), [pwm(true)]);
    assert_eq!(gpt.edges.pwm_queue, []);
    assert_eq!(gpt.take_output_changes(), []);
}

#[test]
fn compare_edges_come_at_their_own_times() {
    let mut gpt = gpt();
    // OC2 toggles on PGP4 at TCNT $10, once a TCNT period
    write(&mut gpt, 0x1e, 0x0100, 0);
    write(&mut gpt, 0x16, 0x0010, 0);
    gpt.advance(Cycles::from_clocks(100));
    let period = 4 * 0x1_0000;
    gpt.advance(Cycles::from_clocks(period + 4 * 0x10 + 50));
    assert_eq!(
        take_output_changes(&mut gpt),
        [
            (Cycles::from_clocks(0), gp(0x10, 0x00)),
            (Cycles::from_clocks(4 * 0x10), gp(0x10, 0x10)),
            (Cycles::from_clocks(period + 4 * 0x10), gp(0x10, 0x00)),
        ]
    );
    assert_eq!(take_output_changes(&mut gpt), []);
}

#[test]
fn cforc_and_a_reset_make_edges_at_once() {
    let mut gpt = gpt();
    // OC3 sets PGP5; FOC3 forces it (GPTRM 3.3.4)
    write(&mut gpt, 0x1e, 0x0c00, 10);
    write_byte(&mut gpt, 0x24, 0x20, 1000);
    // PORTGP drives PGP0 high
    write(&mut gpt, 0x06, 0x0101, 1200);
    gpt.reset(Cycles::from_clocks(2000));
    assert_eq!(
        take_output_changes(&mut gpt),
        [
            (Cycles::from_clocks(10), gp(0x20, 0x00)),
            (Cycles::from_clocks(1000), gp(0x20, 0x20)),
            (Cycles::from_clocks(1200), gp(0x21, 0x21)),
            (Cycles::from_clocks(2000), gp(0x00, 0x00)),
        ]
    );
}

#[test]
fn pwm_edges_follow_the_duty_a_period_late() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    // PWMA $80 and PWMB $40, fast at the system clock / 2: 512 clocks a
    // period, the duty from the next period (GPTRM 6.2); B unreported
    write(&mut gpt, 0x26, 0x8040, 10);
    gpt.advance(Cycles::from_clocks(700));
    gpt.advance(Cycles::from_clocks(1100));
    write_byte(&mut gpt, 0x26, 0x40, 1100);
    gpt.advance(Cycles::from_clocks(1700));
    // $00: low from 2048 on, which is no edge
    write_byte(&mut gpt, 0x26, 0x00, 1700);
    gpt.advance(Cycles::from_clocks(1 << 24));
    assert_eq!(
        take_output_changes(&mut gpt),
        [
            (Cycles::from_clocks(512), pwma(true)),
            (Cycles::from_clocks(768), pwma(false)),
            (Cycles::from_clocks(1024), pwma(true)),
            (Cycles::from_clocks(1280), pwma(false)),
            (Cycles::from_clocks(1536), pwma(true)),
            (Cycles::from_clocks(1664), pwma(false)),
        ]
    );
    // FPWMA with F1A: high at once
    let now = (1 << 24) + 3;
    write(&mut gpt, 0x24, 0x0202, now);
    assert_eq!(
        take_output_changes(&mut gpt),
        [(Cycles::from_clocks(now), pwma(true))]
    );
    gpt.set_report_pwm_edges(Channel::A, false, Cycles::from_clocks(now));
    write(&mut gpt, 0x24, 0x0200, now);
    assert_eq!(take_output_changes(&mut gpt), []);
}

/// The clock edges of a tap of `divisor` from `first` to `last`, each a
/// rise where the tap clocks its counter and a fall a system clock later
/// (GPTRM 5.1), as `change` makes them.
fn clock_edges<T>(
    divisor: u64,
    first: u64,
    last: u64,
    change: impl Fn(bool) -> T,
) -> Vec<(Cycles, T)> {
    (first.div_ceil(divisor) * divisor..=last)
        .filter(|&count| count % divisor <= 1)
        .map(|count| (Cycles::from_clocks(count), change(count % divisor == 0)))
        .collect()
}

#[test]
fn cprout_drives_tcnts_clock_on_oc1() {
    let mut gpt = gpt();
    // an input out of reset: the board's level
    assert_eq!(gpt.oc1(Cycles::from_clocks(10)), Output::Level(true));
    // CPROUT, CPR %001: TCNT at the system clock / 8 (GPTRM Table 5-1)
    write(&mut gpt, 0x20, 0x0009, 10);
    assert_eq!(
        gpt.oc1(Cycles::from_clocks(10)),
        Output::Clock {
            clocks: Some(Cycles::from_clocks(8))
        }
    );
    assert_eq!(gpt.next_event(), None);
    gpt.advance(Cycles::from_clocks(30));
    assert_eq!(
        gpt.port_outputs(Cycles::from_clocks(32)),
        PortLevels::new(0x08, 0x08)
    );
    assert_eq!(
        gpt.port_outputs(Cycles::from_clocks(33)),
        PortLevels::new(0x08, 0x00)
    );
    // unreported, the pin shows low
    assert_eq!(
        take_output_changes(&mut gpt),
        [(Cycles::from_clocks(10), gp(0x08, 0x00))]
    );
    gpt.set_report_oc1_clock_edges(true, Cycles::from_clocks(30));
    assert_eq!(
        read(&mut gpt, 0x06, 56) & 0x08,
        0x08,
        "PORTGP reads the pin"
    );
    gpt.advance(Cycles::from_clocks(60));
    assert_eq!(gpt.next_event(), None, "the clock does not sync");
    // CPROUT off: PGP3 is an input again
    write(&mut gpt, 0x20, 0x0000, 60);
    gpt.advance(Cycles::from_clocks(100));
    let mut expected = clock_edges(8, 31, 59, |high| gp(0x08, u8::from(high) << 3));
    expected.push((Cycles::from_clocks(60), gp(0x00, 0x00)));
    assert_eq!(take_output_changes(&mut gpt), expected);
}

#[test]
fn cprout_edges_come_before_a_compare_at_their_time() {
    let mut gpt = gpt();
    gpt.set_report_oc1_clock_edges(true, Cycles::ZERO);
    // OC2 toggles PGP4 at TCNT $10; CPROUT on the system clock / 4
    write(&mut gpt, 0x1e, 0x0100, 0);
    write(&mut gpt, 0x16, 0x0010, 0);
    write(&mut gpt, 0x20, 0x0008, 0);
    gpt.advance(Cycles::from_clocks(4 * 0x10 + 1));
    let changes = take_output_changes(&mut gpt);
    assert_eq!(
        changes[changes.len() - 3..],
        [
            (Cycles::from_clocks(4 * 0x10), gp(0x18, 0x08)),
            (Cycles::from_clocks(4 * 0x10), gp(0x18, 0x18)),
            (Cycles::from_clocks(4 * 0x10 + 1), gp(0x18, 0x10)),
        ]
    );
    // OC1M3 gives OC1's pin to the compare (GPTRM Table B-1)
    write(&mut gpt, 0x08, 0x0800, 4 * 0x10 + 2);
    gpt.advance(Cycles::from_clocks(100));
    assert_eq!(take_output_changes(&mut gpt), []);
    assert_eq!(
        gpt.port_outputs(Cycles::from_clocks(100)),
        PortLevels::new(0x18, 0x10)
    );
    assert_eq!(gpt.oc1(Cycles::from_clocks(100)), Output::Level(false));
    // OC1D3 sets the pin at OC1's next match, TOC1 $0000
    write(&mut gpt, 0x08, 0x0808, 100);
    gpt.advance(Cycles::from_clocks(4 * 0x10000 + 1));
    assert_eq!(
        gpt.oc1(Cycles::from_clocks(4 * 0x10000 + 1)),
        Output::Level(true)
    );
}

#[test]
fn pprout_drives_pwmcnts_clock_on_pwma() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    // PPROUT, PPR %010: PWMCNT at the system clock / 8 (GPTRM Table 6-2)
    write_byte(&mut gpt, 0x25, 0xa0, 10);
    assert_eq!(
        gpt.pwm(Channel::A, Cycles::from_clocks(10)),
        Output::Clock {
            clocks: Some(Cycles::from_clocks(8))
        }
    );
    assert!(gpt.pwm_level(Channel::A, Cycles::from_clocks(16)));
    assert!(!gpt.pwm_level(Channel::A, Cycles::from_clocks(17)));
    gpt.advance(Cycles::from_clocks(30));
    // PPR %000 mid-run, the system clock / 2
    write_byte(&mut gpt, 0x25, 0x80, 30);
    gpt.advance(Cycles::from_clocks(40));
    assert_eq!(gpt.next_event(), None, "the clock does not sync");
    // PPROUT off: the waveform, low at duty 0
    write_byte(&mut gpt, 0x25, 0x00, 41);
    gpt.advance(Cycles::from_clocks(100));
    let mut expected = clock_edges(8, 11, 29, pwma);
    expected.extend(clock_edges(2, 30, 41, pwma));
    assert_eq!(take_output_changes(&mut gpt), expected);
    // FPWMA drives F1A whatever PPROUT (GPTRM 6.3.1)
    write(&mut gpt, 0x24, 0x0282, 100);
    gpt.advance(Cycles::from_clocks(200));
    assert_eq!(
        take_output_changes(&mut gpt),
        [(Cycles::from_clocks(100), pwma(true))]
    );
}

#[test]
fn pprout_on_pclk_is_high_a_system_clock_from_each_rise() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    // PPROUT, PPR %111: PCLK
    write_byte(&mut gpt, 0x25, 0xf0, 10);
    gpt.schedule_input(Cycles::from_clocks(100), Input::Pclk, false);
    gpt.schedule_input(Cycles::from_clocks(200), Input::Pclk, true);
    gpt.schedule_input(Cycles::from_clocks(300), Input::Pclk, false);
    gpt.schedule_input(Cycles::from_clocks(400), Input::Pclk, true);
    gpt.advance(Cycles::from_clocks(250));
    assert_eq!(gpt.next_event(), Some(Cycles::from_clocks(300)));
    gpt.advance(Cycles::from_clocks(500));
    assert_eq!(
        take_output_changes(&mut gpt),
        [
            (Cycles::from_clocks(200), pwma(true)),
            (Cycles::from_clocks(201), pwma(false)),
            (Cycles::from_clocks(400), pwma(true)),
            (Cycles::from_clocks(401), pwma(false)),
        ]
    );
}

#[test]
fn a_scheduled_capture_edge_latches_tcnt_at_its_time() {
    let mut gpt = gpt();
    // EDGE1 rising, IC1I
    write(&mut gpt, 0x1e, 0x0001, 0);
    write_byte(&mut gpt, 0x20, 0x01, 0);
    let pgp0 = Input::PortGp(PortGpPin::new(0).unwrap());
    let at = 4 * 0x1234 + 2;
    gpt.schedule_input(Cycles::from_clocks(at), pgp0, true);
    gpt.schedule_input(Cycles::from_clocks(at), pgp0, false);
    gpt.schedule_input(Cycles::from_clocks(at), pgp0, true);
    assert_eq!(
        gpt.next_event(),
        Some(Cycles::from_clocks(at)),
        "a quantum ends there"
    );
    assert_eq!(read(&mut gpt, 0x22, at - 1), 0);
    let later = at + 4000;
    gpt.advance(Cycles::from_clocks(later));
    assert_eq!(read(&mut gpt, 0x0e, later), 0x1234, "TIC1");
    assert_eq!(read(&mut gpt, 0x22, later), 0x0100, "IC1F");
    assert_eq!(gpt.next_event(), None);
    // one in the past takes effect at once
    gpt.schedule_input(Cycles::from_clocks(at), pgp0, false);
    gpt.schedule_input(Cycles::from_clocks(at), pgp0, true);
    assert_eq!(gpt.next_event(), Some(Cycles::from_clocks(later)));
    gpt.advance(Cycles::from_clocks(later));
    assert_eq!(read(&mut gpt, 0x0e, later), 0x1234 + 1000);
}

#[test]
fn scheduled_pai_edges_count_at_their_times() {
    let mut gpt = gpt();
    // PAEN, rising edges, as a GPS engine's 1 PPS
    write(&mut gpt, 0x0c, 0x5000, 0);
    for second in 1..=3 {
        gpt.schedule_input(Cycles::from_clocks(second * 1000), Input::Pai, false);
        gpt.schedule_input(Cycles::from_clocks(second * 1000 + 100), Input::Pai, true);
    }
    gpt.advance(Cycles::from_clocks(2050));
    assert_eq!(read_byte(&mut gpt, 0x0d, 2050), 1);
    assert_eq!(read_byte(&mut gpt, 0x0d, 2100), 2);
    assert_eq!(read_byte(&mut gpt, 0x0d, 5000), 3);
    assert_eq!(read(&mut gpt, 0x22, 5000), 0x0010, "PAIF");
}

#[test]
fn unscheduled_inputs_past_now_never_come() {
    let mut gpt = gpt();
    write(&mut gpt, 0x0c, 0x5000, 0);
    for second in 1..=3 {
        gpt.schedule_input(Cycles::from_clocks(second * 1000), Input::Pai, false);
        gpt.schedule_input(Cycles::from_clocks(second * 1000 + 100), Input::Pai, true);
    }
    // the first second's edges have come by 1500; the rest are withdrawn
    gpt.unschedule_inputs(Cycles::from_clocks(1500));
    assert_eq!(gpt.next_event(), None);
    assert_eq!(read_byte(&mut gpt, 0x0d, 5000), 1);
}

#[test]
fn scheduled_pclk_edges_clock_and_show_in_pclks() {
    let mut gpt = gpt();
    // TCNT on PCLK; OC1 at 2
    write_byte(&mut gpt, 0x21, 0x07, 0);
    write(&mut gpt, 0x14, 0x0002, 0);
    for edge in 1..=2 {
        gpt.schedule_input(Cycles::from_clocks(edge * 10), Input::Pclk, false);
        gpt.schedule_input(Cycles::from_clocks(edge * 10 + 5), Input::Pclk, true);
    }
    // PCLKS reads the pin
    assert_eq!(read_byte(&mut gpt, 0x0c, 12) & 0x08, 0);
    assert_eq!(read(&mut gpt, 0x22, 24), 0, "TCNT 1");
    assert_eq!(read(&mut gpt, 0x22, 25), 0x0800, "OC1F at TCNT 2");
    assert_eq!(read_byte(&mut gpt, 0x0c, 25) & 0x08, 0x08);
    assert_eq!(read(&mut gpt, 0x0a, 1000), 2);
}

#[test]
fn inputs_are_named_as_their_pins_in_any_case() {
    for name in ["PGP0", "PGP7", "PAI", "PCLK"] {
        let input: Input = name.parse().unwrap();
        assert_eq!(input.to_string(), name);
        assert_eq!(name.to_lowercase().parse(), Ok(input));
    }
    assert_eq!(
        "pGp3".parse(),
        Ok(Input::PortGp(PortGpPin::new(3).unwrap()))
    );
    for bad in ["PGP8", "PGP07", "PG", "PG\u{e9}0", "OC1"] {
        assert!(bad.parse::<Input>().is_err(), "{bad}");
    }
}
