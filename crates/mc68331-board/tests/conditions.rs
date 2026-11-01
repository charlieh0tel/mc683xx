//! Stop conditions (`Conditions`): a net's change or a peripheral's own
//! condition stops the run where it holds and is named as fired; a
//! disarmed one stops nothing, and a restore keeps them armed.

use std::env;

use mc683xx_board::board::Board;
use mc683xx_board::condition::ArmError;
use mc683xx_board::condition::Condition;
use mc683xx_board::condition::ConditionInfo;
use mc683xx_board::condition::ConditionToken;
use mc683xx_board::condition::Conditions;
use mc683xx_board::condition::NetChange;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::error::ConditionError;
use mc683xx_board::error::NetError;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_mc68331::chip::Mc68331;

/// The divided clock's period: 10 MHz divided by 1000.
const PERIOD_PS: u64 = 100_000_000;
/// The divided clock's first rise: input edge 1000, the clock's phase
/// after it.
const FIRST_RISE: Time = Time(PERIOD_PS + 37);
/// Its first fall, half a period before: its first change.
const FIRST_FALL: Time = Time(PERIOD_PS / 2 + 37);
/// How long the runs go: ten periods.
const RUN_PS: u64 = 10 * PERIOD_PS;

/// The probe's one condition.
const HIGH: ConditionInfo = ConditionInfo {
    name: "high",
    arguments: "",
    help: "its input driven high",
};

/// A peripheral offering one condition: its input driven high.
#[derive(Debug, Default)]
struct Probe {
    /// The tokens armed, the last given the highest.
    armed: Vec<ConditionToken>,
    /// The tokens given so far.
    given: u32,
}

impl Peripheral for Probe {
    fn input(&mut self, _port: usize, signal: Signal, _now: Time, drive: &mut Drive<'_>) {
        if signal == Signal::Level(true) {
            for &token in &self.armed {
                drive.fire(token);
            }
        }
    }

    fn conditions(&self) -> &'static [ConditionInfo] {
        &[HIGH]
    }

    fn arm(&mut self, condition: &str, arguments: &[String]) -> Result<ConditionToken, ArmError> {
        if condition != HIGH.name {
            return Err(ArmError::Unknown(condition.to_owned()));
        }
        if !arguments.is_empty() {
            return Err(ArmError::Arguments("`high` takes none".to_owned()));
        }
        self.given += 1;
        let token = ConditionToken(self.given);
        self.armed.push(token);
        Ok(token)
    }

    fn disarm(&mut self, token: ConditionToken) {
        self.armed.retain(|&armed| armed != token);
    }
}

/// A 10 MHz clock divided by 1000 onto `pps`, to PGP0 and an edge
/// counter, with the probe on it if `probed`, which cannot save.
fn board(probed: bool) -> Board<Mc68331> {
    let (probe, sink) = if probed {
        (
            "[[peripheral]]\nname = \"probe\"\ntype = \"probe\"\n",
            ", \"probe.in\"",
        )
    } else {
        ("", "")
    };
    let text = format!(
        r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{{ select = "CSBOOT" }}]
[[peripheral]]
name = "reference"
type = "clock"
parameters = {{ hz = 10_000_000, phase_ps = 37 }}
[[peripheral]]
name = "divider"
type = "divider"
parameters = {{ divide = 1000 }}
[[peripheral]]
name = "counter"
type = "edge-counter"
{probe}
[[net]]
name = "ten-megahertz"
type = "clock"
from = "reference.out"
to = ["divider.in"]
[[net]]
name = "pps"
type = "edge"
from = "divider.out"
to = ["chip.PGP0", "counter.in"{sink}]
"#
    );
    let description = Description::parse(&text, &env::temp_dir(), &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register("probe", vec![PortSpec::input("in", Kind::Edge)], |_| {
            Ok(Box::new(Probe::default()))
        })
        .unwrap();
    Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
}

/// The net condition `change` on `pps`.
fn pps(change: NetChange) -> Condition {
    format!("net pps {change}").parse().unwrap()
}

#[test]
fn a_net_s_rise_stops_the_run_where_it_rises() {
    let mut board = board(false);
    let id = board.arm(pps(NetChange::Rise)).unwrap();
    let run = board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(run.stop, Stop::Requested);
    assert_eq!(board.now(), FIRST_RISE);
    assert_eq!(board.fired(), vec![id]);
    let run = board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(run.stop, Stop::Requested);
    assert_eq!(board.now(), Time(FIRST_RISE.0 + PERIOD_PS));
    assert_eq!(board.fired(), vec![id]);
}

#[test]
fn a_net_s_fall_and_change_stop_where_they_hold() {
    let mut board = board(false);
    let fall = board.arm(pps(NetChange::Fall)).unwrap();
    board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(board.now(), FIRST_FALL);
    assert_eq!(board.net_value("pps").unwrap(), Some(Signal::Level(false)));
    assert_eq!(board.fired(), vec![fall]);
    assert!(board.disarm(fall));
    let change = board.arm(pps(NetChange::Change)).unwrap();
    board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(board.net_value("pps").unwrap(), Some(Signal::Level(true)));
    assert_eq!(board.fired(), vec![change]);
}

#[test]
fn a_disarmed_condition_stops_nothing() {
    let mut board = board(false);
    let id = board.arm(pps(NetChange::High)).unwrap();
    assert_eq!(board.armed(), vec![(id, pps(NetChange::High))]);
    assert!(board.disarm(id));
    assert!(!board.disarm(id));
    assert!(board.armed().is_empty());
    let run = board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(run.stop, Stop::Budget);
    assert!(board.fired().is_empty());
}

#[test]
fn conditions_the_board_cannot_watch_are_refused() {
    let mut board = board(false);
    assert_eq!(
        board.arm("net nowhere rise".parse().unwrap()),
        Err(ConditionError::Net(NetError::Unknown("nowhere".to_owned())))
    );
    assert_eq!(
        board.arm("net ten-megahertz rise".parse().unwrap()),
        Err(ConditionError::Kind {
            net: "ten-megahertz".to_owned(),
            kind: Kind::Clock,
        })
    );
    assert!(
        board
            .arm("net ten-megahertz change".parse().unwrap())
            .is_ok()
    );
    assert_eq!(
        board.arm("peripheral nobody high".parse().unwrap()),
        Err(ConditionError::Peripheral("nobody".to_owned()))
    );
    assert_eq!(
        board.arm("peripheral counter high".parse().unwrap()),
        Err(ConditionError::Arm {
            peripheral: "counter".to_owned(),
            error: ArmError::Unknown("high".to_owned()),
        })
    );
}

#[test]
fn a_peripheral_s_condition_stops_the_run_and_is_named() {
    let mut board = board(true);
    assert_eq!(board.offered(), vec![("probe".to_owned(), &[HIGH][..])]);
    assert!(matches!(
        board.arm("peripheral probe high now".parse().unwrap()),
        Err(ConditionError::Arm {
            error: ArmError::Arguments(_),
            ..
        })
    ));
    let high = board.arm("peripheral probe high".parse().unwrap()).unwrap();
    let run = board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(run.stop, Stop::Requested);
    assert_eq!(board.now(), FIRST_RISE);
    assert_eq!(board.fired(), vec![high]);
    let net = board.arm(pps(NetChange::Fall)).unwrap();
    assert_ne!(net, high);
    assert!(board.disarm(high));
    board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(board.now(), Time(FIRST_RISE.0 + PERIOD_PS / 2));
    assert_eq!(board.fired(), vec![net]);
    assert!(board.peripheral::<Probe>("probe").unwrap().armed.is_empty());
}

#[test]
fn a_peripheral_s_condition_disarmed_after_it_fired_is_still_named() {
    let mut board = board(true);
    let high = board.arm("peripheral probe high".parse().unwrap()).unwrap();
    board.run_until(Time(RUN_PS)).unwrap();
    assert!(board.disarm(high));
    let again = board.arm("peripheral probe high".parse().unwrap()).unwrap();
    assert_eq!(board.fired(), vec![high]);
    assert_ne!(again, high);
}

#[test]
fn a_restore_keeps_conditions_armed() {
    let mut board = board(false);
    let snapshot = board.save().unwrap();
    let id = board.arm(pps(NetChange::Rise)).unwrap();
    board.run_until(Time(RUN_PS)).unwrap();
    board.restore(&snapshot).unwrap();
    assert_eq!(board.armed(), vec![(id, pps(NetChange::Rise))]);
    let run = board.run_until(Time(RUN_PS)).unwrap();
    assert_eq!(run.stop, Stop::Requested);
    assert_eq!(board.now(), FIRST_RISE);
    assert_eq!(board.fired(), vec![id]);
}
