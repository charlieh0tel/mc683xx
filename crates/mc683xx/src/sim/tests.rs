use crate::pins::PortLevels;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;
use mc683xx_cpu32::interrupt::InterruptLevel;
use mc683xx_cpu32::size::Size;

use super::Configuration;
use super::ResetCause;
use super::Sim;
use super::clock::ClockSource;
use super::ports::Change;
use super::ports::PortName;
use crate::chip_select::ChipSelect;
use crate::module::RegisterAccess;
use crate::peripheral::Timed;

/// System clocks in an EXTAL period at SYNCR's reset value.
const RESET_CLOCKS_PER_TICK: u64 = 256;

fn sim() -> Sim {
    Sim::new(Configuration::default(), Cycles::ZERO)
}

fn access(offset: u32, size: Size, now: Cycles) -> RegisterAccess {
    RegisterAccess {
        offset,
        size,
        supervisor: true,
        now,
    }
}

fn read(sim: &mut Sim, offset: u32, size: Size) -> u32 {
    sim.read(access(offset, size, Cycles::ZERO)).unwrap()
}

fn write(sim: &mut Sim, offset: u32, size: Size, value: u32, now: Cycles) {
    sim.write(access(offset, size, now), value).unwrap();
}

#[test]
fn a_snapshot_s_sim_must_fit_the_chip_and_be_one_a_run_could_leave() {
    let current = sim();
    assert!(sim().check_snapshot(&current).is_ok());
    let external = Configuration {
        clock: ClockSource::external(Frequency::from_hz(16_000_000)).unwrap(),
        ..Configuration::default()
    };
    assert!(matches!(
        Sim::new(external, Cycles::ZERO).check_snapshot(&current),
        Err(StateError::Mismatch(reason)) if reason == "SIM configuration"
    ));
    let mut stopped = sim();
    stopped.clock.set_multiplier_unchecked(0);
    let mut port_c = sim();
    port_c.port_c = 0xff;
    let mut low_power = sim();
    low_power.low_power = Some(8);
    for (snapshot, what) in [
        (stopped, "SYNCR"),
        (port_c, "SIM registers"),
        (low_power, "LPSTOP's interrupt mask"),
    ] {
        assert!(matches!(
            snapshot.check_snapshot(&current),
            Err(StateError::Invalid(reason)) if reason == what
        ));
    }
}

#[test]
fn registers_out_of_reset() {
    let mut sim = sim();
    assert_eq!(read(&mut sim, 0x00, Size::Word), 0x00cf);
    assert_eq!(read(&mut sim, 0x04, Size::Word), 0x3f08);
    assert_eq!(read(&mut sim, 0x07, Size::Byte), 0x40, "POW");
    assert_eq!(read(&mut sim, 0x21, Size::Byte), 0x80);
    assert_eq!(read(&mut sim, 0x22, Size::Word), 0x000f);
    assert_eq!(read(&mut sim, 0x24, Size::Word), 0x0000);
    assert_eq!(read(&mut sim, 0x41, Size::Byte), 0x7f);
    assert_eq!(read(&mut sim, 0x44, Size::Word), 0x3fff);
    assert_eq!(read(&mut sim, 0x46, Size::Word), 0x03ff);
    assert_eq!(read(&mut sim, 0x48, Size::Word), 0x0007);
    assert_eq!(read(&mut sim, 0x4a, Size::Word), 0x7b70);
    assert_eq!(read(&mut sim, 0x4e, Size::Word), 0x0000);
    assert_eq!(read(&mut sim, 0x17, Size::Byte), 0xff, "PEPAR");
    assert_eq!(read(&mut sim, 0x1f, Size::Byte), 0xff, "PFPAR");
    assert_eq!(sim.module_base(), 0xff_f000);
    assert_eq!(sim.arbitration(), 0xf);
}

#[test]
fn modclk_low_prescales_the_timers() {
    let configuration = Configuration {
        data: !(1 << 11),
        clock: ClockSource::external(Frequency::from_hz(16_000_000)).unwrap(),
    };
    let mut sim = Sim::new(configuration, Cycles::ZERO);
    assert_eq!(read(&mut sim, 0x21, Size::Byte), 0xc0, "SWE, SWP");
    assert_eq!(read(&mut sim, 0x24, Size::Word), 0x0100, "PTP");
    assert_eq!(read(&mut sim, 0x00, Size::Word), 0x08cf, "SLVEN");
    // 2^18 system clocks: EXTAL is the system clock
    assert_eq!(sim.next_event(), Some(Cycles(1 << 18)));
}

#[test]
fn mm_is_written_once() {
    let mut sim = sim();
    let written = sim.write(access(0x00, Size::Word, Cycles::ZERO), 0x008f);
    assert!(written.unwrap().decode);
    assert_eq!(sim.module_base(), 0x7f_f000);
    write(&mut sim, 0x00, Size::Word, 0x00cf, Cycles::ZERO);
    assert_eq!(sim.module_base(), 0x7f_f000);
    assert_eq!(read(&mut sim, 0x00, Size::Word), 0x008f);
}

#[test]
fn sypcr_is_written_once() {
    let mut sim = sim();
    write(&mut sim, 0x21, Size::Byte, 0xcc, Cycles::ZERO);
    write(&mut sim, 0x21, Size::Byte, 0x00, Cycles::ZERO);
    assert_eq!(read(&mut sim, 0x21, Size::Byte), 0xcc);
    assert_eq!(sim.bus_monitor(), Some(64));
    assert!(sim.halt_monitor());
}

#[test]
fn byte_writes_keep_the_other_half() {
    let mut sim = sim();
    write(&mut sim, 0x4c, Size::Word, 0x1003, Cycles::ZERO);
    write(&mut sim, 0x4d, Size::Byte, 0x05, Cycles::ZERO);
    assert_eq!(read(&mut sim, 0x4c, Size::Word), 0x1005);
    assert_eq!(read(&mut sim, 0x4c, Size::Byte), 0x10);
    // CSPAR0 bit 1 always reads one
    write(&mut sim, 0x44, Size::Word, 0, Cycles::ZERO);
    assert_eq!(read(&mut sim, 0x44, Size::Word), 0x0002);
}

#[test]
fn user_mode_reaches_only_unrestricted_registers() {
    let mut sim = sim();
    let user = |offset| RegisterAccess {
        supervisor: false,
        ..access(offset, Size::Byte, Cycles::ZERO)
    };
    assert_eq!(sim.read(user(0x11)), None, "SUPV set");
    write(&mut sim, 0x00, Size::Word, 0x004f, Cycles::ZERO);
    assert!(sim.read(user(0x11)).is_some(), "PORTE0 is S/U");
    assert_eq!(sim.read(user(0x21)), None, "SYPCR is S");
    assert_eq!(sim.write(user(0x4c), 0), None);
}

#[test]
fn ports_read_their_pins_on_inputs() {
    let mut sim = sim();
    sim.set_port_f_pins(0x5a);
    write(&mut sim, 0x19, Size::Byte, 0x0f, Cycles::ZERO);
    write(&mut sim, 0x1d, Size::Byte, 0x03, Cycles::ZERO);
    assert_eq!(read(&mut sim, 0x19, Size::Byte), 0x5b);
    assert_eq!(read(&mut sim, 0x1b, Size::Byte), 0x5b, "PORTF1 is PORTF0");
    write(&mut sim, 0x13, Size::Byte, 0x40, Cycles::ZERO);
    assert_eq!(sim.port_e().latch, 0x40);
}

#[test]
fn chip_select_writes_change_the_decode() {
    let mut sim = sim();
    let written = sim
        .write(access(0x48, Size::Word, Cycles::ZERO), 0x0005)
        .unwrap();
    assert!(written.decode);
    assert_eq!(sim.chip_selects().base(ChipSelect::Boot), 0x0005);
    let written = sim
        .write(access(0x22, Size::Word, Cycles::ZERO), 0x0542)
        .unwrap();
    assert!(!written.decode);
}

/// PICR $0542 and PITR $0008: level 5, vector $42, 1024 ticks a
/// second at any clock.
#[test]
fn the_timer_ticks_on_extal() {
    let mut sim = sim();
    write(&mut sim, 0x04, Size::Word, 0xcf00, Cycles::ZERO);
    write(&mut sim, 0x22, Size::Word, 0x0542, Cycles::ZERO);
    write(&mut sim, 0x24, Size::Word, 0x0008, Cycles::ZERO);
    let period = Cycles(16_777_216 / 1024);
    assert_eq!(sim.next_event(), Some(period));
    sim.advance(Cycles(period.0 - 1));
    assert_eq!(sim.timer_request(), None);
    sim.advance(period);
    assert_eq!(sim.timer_request(), InterruptLevel::new(5));
    assert_eq!(sim.acknowledge_timer(), 0x42);
    assert_eq!(sim.timer_request(), None);
    assert_eq!(sim.next_event(), Some(Cycles(2 * period.0)));
}

/// SYNCR `from`, `from_clocks` system clocks an EXTAL period, written out
/// of reset, then `to`, `to_clocks` a period, half an EXTAL period on:
/// the running period's half left takes its time at the new rate, and
/// the PIT's and the watchdog's periods count on from its end
/// (MC68331UM 4.2.10, 4.2.11, 4.3.2).
fn timers_across_a_syncr_write(from: (u32, u64), to: (u32, u64)) {
    let ((from, from_clocks), (to, to_clocks)) = (from, to);
    let half = Cycles(from_clocks / 2);
    let at = |tick: u64| Cycles(half.0 + to_clocks / 2 + (tick - 1) * to_clocks);
    let sim = || {
        let mut sim = sim();
        write(&mut sim, 0x04, Size::Word, from, Cycles::ZERO);
        sim
    };
    // PITR 1: four EXTAL periods (MC68331UM 4.2.11)
    let mut timer = sim();
    write(&mut timer, 0x22, Size::Word, 0x0542, Cycles::ZERO);
    write(&mut timer, 0x24, Size::Word, 0x0001, Cycles::ZERO);
    write(&mut timer, 0x04, Size::Word, to, half);
    assert_eq!(timer.clock().tick(half), 0);
    assert_eq!(timer.next_event(), Some(at(4)));
    timer.advance(Cycles(at(4).0 - 1));
    assert_eq!(timer.timer_request(), None);
    timer.advance(at(4));
    assert_eq!(timer.timer_request(), InterruptLevel::new(5));
    // the watchdog's 2^9 periods at reset (MC68331UM 4.2.10)
    let mut watchdog = sim();
    write(&mut watchdog, 0x04, Size::Word, to, half);
    assert_eq!(watchdog.next_event(), Some(at(512)));
    watchdog.advance(Cycles(at(512).0 - 1));
    assert_eq!(watchdog.take_reset(), None);
    watchdog.advance(at(512));
    assert_eq!(watchdog.take_reset(), Some(ResetCause::Watchdog));
}

/// SYNCR out of reset: 256 clocks a period.
const RESET: (u32, u64) = (0x3f00, RESET_CLOCKS_PER_TICK);
/// Y 0: 4 clocks a period.
const SLOWEST: (u32, u64) = (0x0000, 4);

#[test]
fn a_falling_multiplier_keeps_the_running_period_s_time() {
    timers_across_a_syncr_write(RESET, SLOWEST);
}

#[test]
fn a_rising_multiplier_keeps_the_running_period_s_time() {
    timers_across_a_syncr_write(SLOWEST, RESET);
    // W, X, Y 15: 512 clocks a period (MC68331UM Table 4-7)
    timers_across_a_syncr_write(RESET, (0xcf00, 512));
}

#[test]
fn the_watchdog_resets_unless_serviced() {
    let mut sim = sim();
    // 2^9 EXTAL periods at reset
    let timeout = Cycles(512 * RESET_CLOCKS_PER_TICK);
    sim.advance(Cycles(timeout.0 - 1));
    assert_eq!(sim.take_reset(), None);
    write(&mut sim, 0x27, Size::Byte, 0x55, Cycles(timeout.0 - 1));
    write(&mut sim, 0x27, Size::Byte, 0xaa, Cycles(timeout.0 - 1));
    sim.advance(timeout);
    assert_eq!(sim.take_reset(), None);
    sim.advance(Cycles(2 * timeout.0));
    assert_eq!(sim.take_reset(), Some(ResetCause::Watchdog));
    assert_eq!(sim.next_event(), None);
    sim.reset(ResetCause::Watchdog, Cycles(2 * timeout.0));
    assert_eq!(sim.reset_status(), 0x20);
}

/// SYPCR $CC (SWP set, SWT 00), written at once out of
/// reset: SWP's prescaler takes effect at once (MC68331UM 4.2.10,
/// Figure 4-3), stretching the reset time-out's 2^9 EXTAL periods to
/// 2^18 (Table 4-4) without a service.
#[test]
fn a_new_swp_takes_effect_at_once() {
    let mut sim = sim();
    let tick = |n: u64| Cycles(n * RESET_CLOCKS_PER_TICK);
    write(&mut sim, 0x21, Size::Byte, 0xcc, Cycles::ZERO);
    assert_eq!(sim.next_event(), Some(tick(1 << 18)));
    sim.advance(tick((1 << 18) - 1));
    assert_eq!(sim.take_reset(), None);
    sim.advance(tick(1 << 18));
    assert_eq!(sim.take_reset(), Some(ResetCause::Watchdog));
}

#[test]
fn lpstop_stops_the_watchdog() {
    let mut sim = sim();
    let tick = |n: u64| Cycles(n * RESET_CLOCKS_PER_TICK);
    sim.low_power_stop(0, tick(100));
    assert_eq!(sim.next_event(), None);
    sim.wake(tick(1000));
    assert_eq!(sim.next_event(), Some(tick(1000 + 412)));
}

#[test]
fn a_reset_keeps_the_port_latches_and_the_reset_instruction_all() {
    let mut sim = sim();
    write(&mut sim, 0x11, Size::Byte, 0x81, Cycles::ZERO);
    write(&mut sim, 0x4c, Size::Word, 0x1003, Cycles::ZERO);
    sim.system_reset();
    assert_eq!(sim.reset_status(), 0x02);
    assert_eq!(read(&mut sim, 0x4c, Size::Word), 0x1003);
    sim.reset(ResetCause::External, Cycles(10));
    assert_eq!(sim.reset_status(), 0x80);
    assert_eq!(read(&mut sim, 0x4c, Size::Word), 0);
    assert_eq!(sim.port_e().latch, 0x81);
}

/// RSR: EXT POW SW HLT, 0, LOC SYS TST (MC68331UM D.3.4).
#[test]
fn every_cause_has_its_rsr_bit() {
    let bits = ResetCause::ALL.map(ResetCause::status);
    assert_eq!(bits, [0x80, 0x40, 0x20, 0x10, 0x04, 0x02, 0x01]);
    let mut sim = sim();
    for cause in ResetCause::ALL {
        sim.reset(cause, Cycles::ZERO);
        assert_eq!(sim.reset_status(), cause.status());
    }
}

#[test]
fn port_c_drives_the_selects_assigned_as_discrete_outputs() {
    let mut sim = sim();
    // PORTC out of reset (SIMRM Table 8-4), but no pin a discrete
    // output: reset makes each a select or its other function
    // (MC68331UM 4.8.1.1)
    assert_eq!(read(&mut sim, 0x41, Size::Byte), 0x7f);
    assert_eq!(
        sim.outputs(PortName::C),
        PortLevels {
            driven: 0,
            levels: 0
        }
    );
    // CSPAR0: CS3 to CS5 %00; CSPAR1: CS6 to CS9 %00, CS10 a select
    write(&mut sim, 0x44, Size::Word, 0x00ff, Cycles(5));
    write(&mut sim, 0x46, Size::Word, 0x0300, Cycles(6));
    assert_eq!(
        sim.outputs(PortName::C),
        PortLevels {
            driven: 0x7f,
            levels: 0x7f
        }
    );
    write(&mut sim, 0x41, Size::Byte, 0xc1, Cycles(7));
    assert_eq!(sim.port_c(), 0x41, "bit 7 not implemented");
    assert_eq!(
        sim.outputs(PortName::C),
        PortLevels {
            driven: 0x7f,
            levels: 0x41
        }
    );
    let change = |driven, levels| Change {
        port: PortName::C,
        outputs: PortLevels { driven, levels },
    };
    assert_eq!(
        sim.take_output_changes(),
        [
            (Cycles(5), change(0x07, 0x07)),
            (Cycles(6), change(0x7f, 0x7f)),
            (Cycles(7), change(0x7f, 0x41)),
        ]
    );
    assert!(sim.take_output_changes().is_empty());
}

#[test]
fn a_pulse_between_quanta_is_two_changes() {
    let mut sim = sim();
    // PE0 a port pin, an output, low
    write(&mut sim, 0x17, Size::Byte, 0xfe, Cycles(1));
    write(&mut sim, 0x15, Size::Byte, 0x01, Cycles(2));
    write(&mut sim, 0x11, Size::Byte, 0x01, Cycles(10));
    write(&mut sim, 0x11, Size::Byte, 0x00, Cycles(13));
    // an unchanged level is no change
    write(&mut sim, 0x13, Size::Byte, 0x00, Cycles(14));
    assert_eq!(
        sim.outputs(PortName::E),
        PortLevels {
            driven: 0x01,
            levels: 0x00
        }
    );
    let change = |driven, levels| Change {
        port: PortName::E,
        outputs: PortLevels { driven, levels },
    };
    assert_eq!(
        sim.take_output_changes(),
        [
            (Cycles(2), change(0x01, 0x00)),
            (Cycles(10), change(0x01, 0x01)),
            (Cycles(13), change(0x01, 0x00)),
        ]
    );
    // a reset makes the pins inputs again (SIMRM 8.8)
    write(&mut sim, 0x1f, Size::Byte, 0x00, Cycles(20));
    write(&mut sim, 0x1d, Size::Byte, 0x80, Cycles(20));
    write(&mut sim, 0x19, Size::Byte, 0x80, Cycles(21));
    sim.reset(ResetCause::External, Cycles(30));
    assert_eq!(
        sim.take_output_changes(),
        [
            (
                Cycles(20),
                Change {
                    port: PortName::F,
                    outputs: PortLevels {
                        driven: 0x80,
                        levels: 0
                    },
                }
            ),
            (
                Cycles(21),
                Change {
                    port: PortName::F,
                    outputs: PortLevels {
                        driven: 0x80,
                        levels: 0x80
                    },
                }
            ),
            (Cycles(30), change(0, 0)),
            (
                Cycles(30),
                Change {
                    port: PortName::F,
                    outputs: PortLevels {
                        driven: 0,
                        levels: 0
                    },
                }
            ),
        ]
    );
}
