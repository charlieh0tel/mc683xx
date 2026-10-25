use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::module::Module;
use mc683xx_modules::module::RegisterAccess;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::pins::PortLevels;

use super::Gpt;
use super::Input;
use super::OutputChange;
use super::PortGpPin;
use super::STATE_VERSION;
use super::pwm::Channel;
use super::pwm::Output;

fn access(offset: u32, size: Size, now: u64) -> RegisterAccess {
    RegisterAccess {
        offset,
        size,
        supervisor: true,
        now: Cycles(now),
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
    gpt.advance(Cycles(4 * 0x100 + 1));
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
    let invalid: [(Vec<u8>, &str); 4] = [
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
                saved.scheduled.push_back((Cycles(20), Input::Pai, true));
                saved.scheduled.push_back((Cycles(10), Input::Pai, false));
            }),
            "GPT scheduled inputs",
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
    assert_eq!(restored.next_event(), Some(Cycles(4 * 0x100)));
}

#[test]
fn output_compare_3_interrupts_when_tcnt_reaches_it() {
    let mut gpt = gpt();
    // TOC3 $100, OC3I
    write(&mut gpt, 0x18, 0x0100, 0);
    write_byte(&mut gpt, 0x20, 0x20, 0);
    assert_eq!(gpt.next_event(), Some(Cycles(4 * 0x100)));
    gpt.advance(Cycles(4 * 0x100 - 1));
    assert_eq!(gpt.request(), None);
    gpt.advance(Cycles(4 * 0x100));
    let six = InterruptLevel::new(6).unwrap();
    let request = gpt.request().unwrap();
    assert_eq!((request.level, request.arbitration), (six, 7));
    assert_eq!(gpt.acknowledge(six, Cycles(4 * 0x100)), Some(0x56));
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
    assert_eq!(gpt.next_event(), Some(Cycles(4 * 0x1_0100)));
}

#[test]
fn a_flag_set_after_the_read_stays() {
    let mut gpt = gpt();
    write(&mut gpt, 0x16, 0x0010, 0);
    write(&mut gpt, 0x18, 0x0020, 0);
    gpt.advance(Cycles(4 * 0x10));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x10), 0x1000, "OC2F");
    // OC3F sets between the read and the write
    gpt.advance(Cycles(4 * 0x20));
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
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x30,
            levels: 0x00
        }
    );
    // the pin actions show, so a run syncs at them
    assert_eq!(gpt.next_event(), Some(Cycles(4 * 0x10)));
    gpt.advance(Cycles(4 * 0x10));
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x30,
            levels: 0x10
        },
        "OC2 toggled"
    );
    gpt.advance(Cycles(4 * 0x40));
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x30,
            levels: 0x00
        },
        "OC1 over OC3"
    );
    // CFORC: OC3's action, no flag
    let flags = read(&mut gpt, 0x22, 4 * 0x40);
    write_byte(&mut gpt, 0x24, 0x20, 4 * 0x40);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x30,
            levels: 0x20
        }
    );
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x40), flags);
    assert_eq!(read_byte(&mut gpt, 0x24, 4 * 0x40), 0, "FOC reads zero");
    // a disconnected compare's pin is PORTGP's again
    write(&mut gpt, 0x1e, 0x0000, 4 * 0x40);
    write(&mut gpt, 0x08, 0x0000, 4 * 0x40);
    write(&mut gpt, 0x06, 0x30a0, 4 * 0x40);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x30,
            levels: 0x20
        }
    );
    assert_eq!(read_byte(&mut gpt, 0x07, 4 * 0x40), 0xef);
}

#[test]
fn a_pin_edge_is_captured() {
    let mut gpt = gpt();
    // EDGE1 rising, IC1I
    write(&mut gpt, 0x1e, 0x0001, 0);
    write_byte(&mut gpt, 0x20, 0x01, 0);
    gpt.set_port_pins(0xfe, Cycles(4 * 0x100));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x100), 0, "a falling edge");
    gpt.set_port_pins(0xff, Cycles(4 * 0x1234 + 2));
    assert_eq!(read(&mut gpt, 0x0e, 4 * 0x1234 + 2), 0x1234);
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x1234 + 2), 0x0100, "IC1F");
    let six = InterruptLevel::new(6).unwrap();
    assert_eq!(gpt.acknowledge(six, Cycles::ZERO), Some(0x51));
    // input capture 4 on any edge, on PGP7
    write(&mut gpt, 0x0c, 0x0400, 4 * 0x2000);
    write(&mut gpt, 0x1e, 0x00c0, 4 * 0x2000);
    gpt.set_port_pins(0x7f, Cycles(4 * 0x2000));
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
    gpt.set_port_pins(0x00, Cycles::ZERO);
    write_byte(&mut gpt, 0x07, 0xff, 0);
    assert_eq!(read_byte(&mut gpt, 0x07, 0), 0x00);
    write_byte(&mut gpt, 0x06, 0x0f, 0);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x0f,
            levels: 0x0f
        },
        "the latch kept"
    );
    write_byte(&mut gpt, 0x07, 0x05, 0);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x0f,
            levels: 0x05
        },
        "DDRGP kept"
    );
    // OC2 clears its pin, PGP4, at its compare (GPTRM 3.3.1); the latch
    // under it is PORTGP's, not the pin's level
    write_byte(&mut gpt, 0x07, 0xff, 0);
    write(&mut gpt, 0x1e, 0x0200, 0);
    write_byte(&mut gpt, 0x06, 0x1f, 0);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x1f,
            levels: 0x0f
        }
    );
    assert_eq!(read_byte(&mut gpt, 0x07, 0), 0x0f);
    write_byte(&mut gpt, 0x06, 0x1f, 0);
    write(&mut gpt, 0x1e, 0x0000, 0);
    assert_eq!(
        gpt.port_outputs(Cycles::ZERO),
        PortLevels {
            driven: 0x1f,
            levels: 0x1f
        },
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
    gpt.set_low_power(true, Cycles(4 * 0x10));
    assert_eq!(gpt.next_event(), None);
    gpt.advance(Cycles(4 * 0x1000));
    assert_eq!(read(&mut gpt, 0x0a, 4 * 0x1000), 0x10, "TCNT holds");
    // a level the board drives while stopped is seen at the wake
    // (MC68331UM 4.3.4)
    gpt.set_port_pins(0xfe, Cycles(4 * 0x1000));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x1000), 0, "no capture");
    gpt.set_low_power(false, Cycles(4 * 0x2000));
    assert_eq!(read(&mut gpt, 0x22, 4 * 0x2000), 0x0100, "IC1F at the wake");
    assert_eq!(read(&mut gpt, 0x0e, 4 * 0x2000), 0x10);
    // TCNT runs on from $10: OC3 a $F0 counts later
    assert_eq!(gpt.next_event(), Some(Cycles(4 * (0x2000 + 0xf0))));
}

#[test]
fn tcnt_overflow_sets_tof() {
    let mut gpt = gpt();
    // TOI
    write_byte(&mut gpt, 0x21, 0x80, 0);
    assert_eq!(gpt.next_event(), Some(Cycles(4 << 16)));
    gpt.advance(Cycles(4 << 16));
    // the compares at their reset value $FFFF matched on the way
    assert_eq!(read(&mut gpt, 0x22, 4 << 16), 0xf880);
    let six = InterruptLevel::new(6).unwrap();
    assert_eq!(gpt.acknowledge(six, Cycles::ZERO), Some(0x59));
}

#[test]
fn pab_moves_a_source_to_the_top() {
    let mut gpt = gpt();
    // OC1 and OC4 pending; PAB 7 puts OC4 first, as vector $X0
    write(&mut gpt, 0x14, 0x0001, 0);
    write(&mut gpt, 0x1a, 0x0001, 0);
    write_byte(&mut gpt, 0x20, 0x48, 0);
    gpt.advance(Cycles(4));
    let six = InterruptLevel::new(6).unwrap();
    assert_eq!(gpt.acknowledge(six, Cycles(4)), Some(0x54));
    write(&mut gpt, 0x04, 0x7650, 4);
    assert_eq!(gpt.acknowledge(six, Cycles(4)), Some(0x50));
}

#[test]
fn the_pulse_accumulator_counts_events_and_overflows() {
    let mut gpt = gpt();
    // PAEN, rising edges; PACNT $FE
    write(&mut gpt, 0x0c, 0x50fe, 0);
    gpt.set_pai(false, Cycles(1));
    gpt.set_pai(true, Cycles(2));
    assert_eq!(read(&mut gpt, 0x0c, 2), 0xd8ff, "PAIS, PCLKS");
    assert_eq!(read(&mut gpt, 0x22, 2), 0x0010, "PAIF");
    gpt.set_pai(false, Cycles(3));
    gpt.set_pai(true, Cycles(4));
    assert_eq!(read_byte(&mut gpt, 0x0d, 4), 0);
    assert_eq!(read(&mut gpt, 0x22, 4), 0x0030, "PAOVF");
}

#[test]
fn gated_accumulation_counts_while_pai_is_high() {
    let mut gpt = gpt();
    // PAEN, PAMOD, PEDGE clear: a zero inhibits; the system clock / 512
    gpt.set_pai(false, Cycles::ZERO);
    write(&mut gpt, 0x0c, 0x6000, 0);
    gpt.set_pai(true, Cycles(512 * 10 + 100));
    gpt.set_pai(false, Cycles(512 * 30));
    assert_eq!(read_byte(&mut gpt, 0x0d, 512 * 40), 20);
    assert_eq!(read(&mut gpt, 0x22, 512 * 40), 0x0010, "PAIF at the end");
    // PAOVI: the overflow is an event
    write(&mut gpt, 0x0c, 0x60f0, 512 * 40);
    write_byte(&mut gpt, 0x21, 0x20, 512 * 40);
    gpt.set_pai(true, Cycles(512 * 40));
    assert_eq!(gpt.next_event(), Some(Cycles(512 * 56)));
}

#[test]
fn pclk_can_clock_tcnt() {
    let mut gpt = gpt();
    write_byte(&mut gpt, 0x21, 0x07, 0);
    write(&mut gpt, 0x14, 0x0002, 0);
    for edge in 1..=2 {
        gpt.set_pclk(false, Cycles(edge * 10));
        gpt.set_pclk(true, Cycles(edge * 10 + 5));
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
    gpt.set_port_pins(0xfe, Cycles(4000));
    gpt.set_port_pins(0xff, Cycles(4001));
    assert_eq!(read(&mut gpt, 0x22, 4001), 0);
    gpt.set_port_pins(0xfe, Cycles(4002));
    write(&mut gpt, 0x00, 0x0007, 5000);
    assert_eq!(read(&mut gpt, 0x06, 5000), 0x00fe);
    assert_eq!(read(&mut gpt, 0x0a, 5000 + 4), 12);
}

#[test]
fn pwm_duty_takes_effect_at_the_end_of_a_period() {
    let mut gpt = gpt();
    // PWMA $80 in fast mode at the system clock / 2: 512 clocks a
    // period
    write(&mut gpt, 0x26, 0x8040, 10);
    assert_eq!(gpt.pwm(Channel::A, Cycles(10)), Output::Level(false));
    let next = 512 + 10;
    assert_eq!(
        gpt.pwm(Channel::A, Cycles(next)),
        Output::Wave {
            period: 256,
            high: 128,
            clocks: Some(2)
        }
    );
    assert!(gpt.pwm_level(Channel::A, Cycles(512 + 255)));
    assert!(!gpt.pwm_level(Channel::A, Cycles(512 + 256)));
    assert!(gpt.pwm_level(Channel::B, Cycles(512 + 127)));
    assert!(!gpt.pwm_level(Channel::B, Cycles(512 + 128)));
    assert_eq!(read(&mut gpt, 0x2a, 600), 0x8040, "PWMBUFA and B");
    // slow mode for B at the system clock / 128: 128 x 32768 clocks
    write_byte(&mut gpt, 0x25, 0x64, 600);
    assert_eq!(
        gpt.pwm(Channel::B, Cycles(600)),
        Output::Wave {
            period: 32768,
            high: 64 * 128,
            clocks: Some(128)
        }
    );
    // F1A: high from the next period; FPWMB: B a discrete output of F1B
    write_byte(&mut gpt, 0x25, 0x06, 600);
    write_byte(&mut gpt, 0x24, 0x01, 600);
    assert_eq!(gpt.pwm(Channel::B, Cycles(600)), Output::Level(false));
    assert!(gpt.pwm_level(Channel::A, Cycles(1024 + 300)));
    assert_eq!(
        read_byte(&mut gpt, 0x25, 1024 + 300),
        0x06,
        "F1x read the pins"
    );
    // PPROUT: A carries PWMCNT's clock
    write_byte(&mut gpt, 0x25, 0x80, 2000);
    assert_eq!(
        gpt.pwm(Channel::A, Cycles(2000)),
        Output::Clock { clocks: Some(2) }
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
    gpt.set_port_pins(0x0f, Cycles(5));
    write(&mut gpt, 0x14, 0x1234, 5);
    gpt.reset(Cycles(100));
    assert_eq!(read(&mut gpt, 0x14, 100), 0xffff);
    assert_eq!(read(&mut gpt, 0x0a, 100 + 8), 2, "TCNT from the reset");
    assert_eq!(read(&mut gpt, 0x06, 100), 0x000f);
}

/// Port GP driving `driven` at `levels`.
const fn gp(driven: u8, levels: u8) -> OutputChange {
    OutputChange::PortGp(PortLevels { driven, levels })
}

/// PWMA at `level`.
const fn pwma(level: bool) -> OutputChange {
    OutputChange::Pwm {
        channel: Channel::A,
        level,
    }
}

#[test]
fn compare_edges_come_at_their_own_times() {
    let mut gpt = gpt();
    // OC2 toggles on PGP4 at TCNT $10, once a TCNT period
    write(&mut gpt, 0x1e, 0x0100, 0);
    write(&mut gpt, 0x16, 0x0010, 0);
    gpt.advance(Cycles(100));
    let period = 4 * 0x1_0000;
    gpt.advance(Cycles(period + 4 * 0x10 + 50));
    assert_eq!(
        gpt.take_output_changes(),
        [
            (Cycles(0), gp(0x10, 0x00)),
            (Cycles(4 * 0x10), gp(0x10, 0x10)),
            (Cycles(period + 4 * 0x10), gp(0x10, 0x00)),
        ]
    );
    assert_eq!(gpt.take_output_changes(), []);
}

#[test]
fn cforc_and_a_reset_make_edges_at_once() {
    let mut gpt = gpt();
    // OC3 sets PGP5; FOC3 forces it (GPTRM 3.3.4)
    write(&mut gpt, 0x1e, 0x0c00, 10);
    write_byte(&mut gpt, 0x24, 0x20, 1000);
    // PORTGP drives PGP0 high
    write(&mut gpt, 0x06, 0x0101, 1200);
    gpt.reset(Cycles(2000));
    assert_eq!(
        gpt.take_output_changes(),
        [
            (Cycles(10), gp(0x20, 0x00)),
            (Cycles(1000), gp(0x20, 0x20)),
            (Cycles(1200), gp(0x21, 0x21)),
            (Cycles(2000), gp(0x00, 0x00)),
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
    gpt.advance(Cycles(700));
    gpt.advance(Cycles(1100));
    write_byte(&mut gpt, 0x26, 0x40, 1100);
    gpt.advance(Cycles(1700));
    // $00: low from 2048 on, which is no edge
    write_byte(&mut gpt, 0x26, 0x00, 1700);
    gpt.advance(Cycles(1 << 24));
    assert_eq!(
        gpt.take_output_changes(),
        [
            (Cycles(512), pwma(true)),
            (Cycles(768), pwma(false)),
            (Cycles(1024), pwma(true)),
            (Cycles(1280), pwma(false)),
            (Cycles(1536), pwma(true)),
            (Cycles(1664), pwma(false)),
        ]
    );
    // FPWMA with F1A: high at once
    let now = (1 << 24) + 3;
    write(&mut gpt, 0x24, 0x0202, now);
    assert_eq!(gpt.take_output_changes(), [(Cycles(now), pwma(true))]);
    gpt.set_report_pwm_edges(Channel::A, false, Cycles(now));
    write(&mut gpt, 0x24, 0x0200, now);
    assert_eq!(gpt.take_output_changes(), []);
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
        .map(|count| (Cycles(count), change(count % divisor == 0)))
        .collect()
}

#[test]
fn cprout_drives_tcnts_clock_on_oc1() {
    let mut gpt = gpt();
    // an input out of reset: the board's level
    assert_eq!(gpt.oc1(Cycles(10)), Output::Level(true));
    // CPROUT, CPR %001: TCNT at the system clock / 8 (GPTRM Table 5-1)
    write(&mut gpt, 0x20, 0x0009, 10);
    assert_eq!(gpt.oc1(Cycles(10)), Output::Clock { clocks: Some(8) });
    assert_eq!(gpt.next_event(), None);
    gpt.advance(Cycles(30));
    assert_eq!(
        gpt.port_outputs(Cycles(32)),
        PortLevels {
            driven: 0x08,
            levels: 0x08
        }
    );
    assert_eq!(
        gpt.port_outputs(Cycles(33)),
        PortLevels {
            driven: 0x08,
            levels: 0x00
        }
    );
    // unreported, the pin shows low
    assert_eq!(gpt.take_output_changes(), [(Cycles(10), gp(0x08, 0x00))]);
    gpt.set_report_oc1_clock_edges(true, Cycles(30));
    assert_eq!(
        read(&mut gpt, 0x06, 56) & 0x08,
        0x08,
        "PORTGP reads the pin"
    );
    gpt.advance(Cycles(60));
    assert_eq!(gpt.next_event(), None, "the clock does not sync");
    // CPROUT off: PGP3 is an input again
    write(&mut gpt, 0x20, 0x0000, 60);
    gpt.advance(Cycles(100));
    let mut expected = clock_edges(8, 31, 59, |high| gp(0x08, u8::from(high) << 3));
    expected.push((Cycles(60), gp(0x00, 0x00)));
    assert_eq!(gpt.take_output_changes(), expected);
}

#[test]
fn cprout_edges_come_before_a_compare_at_their_time() {
    let mut gpt = gpt();
    gpt.set_report_oc1_clock_edges(true, Cycles::ZERO);
    // OC2 toggles PGP4 at TCNT $10; CPROUT on the system clock / 4
    write(&mut gpt, 0x1e, 0x0100, 0);
    write(&mut gpt, 0x16, 0x0010, 0);
    write(&mut gpt, 0x20, 0x0008, 0);
    gpt.advance(Cycles(4 * 0x10 + 1));
    let changes = gpt.take_output_changes();
    assert_eq!(
        changes[changes.len() - 3..],
        [
            (Cycles(4 * 0x10), gp(0x18, 0x08)),
            (Cycles(4 * 0x10), gp(0x18, 0x18)),
            (Cycles(4 * 0x10 + 1), gp(0x18, 0x10)),
        ]
    );
    // OC1M3 gives OC1's pin to the compare (GPTRM Table B-1)
    write(&mut gpt, 0x08, 0x0800, 4 * 0x10 + 2);
    gpt.advance(Cycles(100));
    assert_eq!(gpt.take_output_changes(), []);
    assert_eq!(
        gpt.port_outputs(Cycles(100)),
        PortLevels {
            driven: 0x18,
            levels: 0x10
        }
    );
    assert_eq!(gpt.oc1(Cycles(100)), Output::Level(false));
    // OC1D3 sets the pin at OC1's next match, TOC1 $0000
    write(&mut gpt, 0x08, 0x0808, 100);
    gpt.advance(Cycles(4 * 0x10000 + 1));
    assert_eq!(gpt.oc1(Cycles(4 * 0x10000 + 1)), Output::Level(true));
}

#[test]
fn pprout_drives_pwmcnts_clock_on_pwma() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    // PPROUT, PPR %010: PWMCNT at the system clock / 8 (GPTRM Table 6-2)
    write_byte(&mut gpt, 0x25, 0xa0, 10);
    assert_eq!(
        gpt.pwm(Channel::A, Cycles(10)),
        Output::Clock { clocks: Some(8) }
    );
    assert!(gpt.pwm_level(Channel::A, Cycles(16)));
    assert!(!gpt.pwm_level(Channel::A, Cycles(17)));
    gpt.advance(Cycles(30));
    // PPR %000 mid-run, the system clock / 2
    write_byte(&mut gpt, 0x25, 0x80, 30);
    gpt.advance(Cycles(40));
    assert_eq!(gpt.next_event(), None, "the clock does not sync");
    // PPROUT off: the waveform, low at duty 0
    write_byte(&mut gpt, 0x25, 0x00, 41);
    gpt.advance(Cycles(100));
    let mut expected = clock_edges(8, 11, 29, pwma);
    expected.extend(clock_edges(2, 30, 41, pwma));
    assert_eq!(gpt.take_output_changes(), expected);
    // FPWMA drives F1A whatever PPROUT (GPTRM 6.3.1)
    write(&mut gpt, 0x24, 0x0282, 100);
    gpt.advance(Cycles(200));
    assert_eq!(gpt.take_output_changes(), [(Cycles(100), pwma(true))]);
}

#[test]
fn pprout_on_pclk_is_high_a_system_clock_from_each_rise() {
    let mut gpt = gpt();
    gpt.set_report_pwm_edges(Channel::A, true, Cycles::ZERO);
    // PPROUT, PPR %111: PCLK
    write_byte(&mut gpt, 0x25, 0xf0, 10);
    gpt.schedule_input(Cycles(100), Input::Pclk, false);
    gpt.schedule_input(Cycles(200), Input::Pclk, true);
    gpt.schedule_input(Cycles(300), Input::Pclk, false);
    gpt.schedule_input(Cycles(400), Input::Pclk, true);
    gpt.advance(Cycles(250));
    assert_eq!(gpt.next_event(), Some(Cycles(300)));
    gpt.advance(Cycles(500));
    assert_eq!(
        gpt.take_output_changes(),
        [
            (Cycles(200), pwma(true)),
            (Cycles(201), pwma(false)),
            (Cycles(400), pwma(true)),
            (Cycles(401), pwma(false)),
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
    gpt.schedule_input(Cycles(at), pgp0, true);
    gpt.schedule_input(Cycles(at), pgp0, false);
    gpt.schedule_input(Cycles(at), pgp0, true);
    assert_eq!(gpt.next_event(), Some(Cycles(at)), "a quantum ends there");
    assert_eq!(read(&mut gpt, 0x22, at - 1), 0);
    let later = at + 4000;
    gpt.advance(Cycles(later));
    assert_eq!(read(&mut gpt, 0x0e, later), 0x1234, "TIC1");
    assert_eq!(read(&mut gpt, 0x22, later), 0x0100, "IC1F");
    assert_eq!(gpt.next_event(), None);
    // one in the past takes effect at once
    gpt.schedule_input(Cycles(at), pgp0, false);
    gpt.schedule_input(Cycles(at), pgp0, true);
    assert_eq!(gpt.next_event(), Some(Cycles(later)));
    gpt.advance(Cycles(later));
    assert_eq!(read(&mut gpt, 0x0e, later), 0x1234 + 1000);
}

#[test]
fn scheduled_pai_edges_count_at_their_times() {
    let mut gpt = gpt();
    // PAEN, rising edges, as a GPS engine's 1 PPS
    write(&mut gpt, 0x0c, 0x5000, 0);
    for second in 1..=3 {
        gpt.schedule_input(Cycles(second * 1000), Input::Pai, false);
        gpt.schedule_input(Cycles(second * 1000 + 100), Input::Pai, true);
    }
    gpt.advance(Cycles(2050));
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
        gpt.schedule_input(Cycles(second * 1000), Input::Pai, false);
        gpt.schedule_input(Cycles(second * 1000 + 100), Input::Pai, true);
    }
    // the first second's edges have come by 1500; the rest are withdrawn
    gpt.unschedule_inputs(Cycles(1500));
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
        gpt.schedule_input(Cycles(edge * 10), Input::Pclk, false);
        gpt.schedule_input(Cycles(edge * 10 + 5), Input::Pclk, true);
    }
    // PCLKS reads the pin
    assert_eq!(read_byte(&mut gpt, 0x0c, 12) & 0x08, 0);
    assert_eq!(read(&mut gpt, 0x22, 24), 0, "TCNT 1");
    assert_eq!(read(&mut gpt, 0x22, 25), 0x0800, "OC1F at TCNT 2");
    assert_eq!(read_byte(&mut gpt, 0x0c, 25) & 0x08, 0x08);
    assert_eq!(read(&mut gpt, 0x0a, 1000), 2);
}

#[test]
fn inputs_are_named_as_their_pins() {
    for name in ["PGP0", "PGP7", "PAI", "PCLK"] {
        let input: Input = name.parse().unwrap();
        assert_eq!(input.to_string(), name);
    }
    assert_eq!(
        "PGP3".parse(),
        Ok(Input::PortGp(PortGpPin::new(3).unwrap()))
    );
    for bad in ["PGP8", "PGP07", "pai", "OC1"] {
        assert!(bad.parse::<Input>().is_err(), "{bad}");
    }
}
