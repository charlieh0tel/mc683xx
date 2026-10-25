//! Observers on nets (`Board::observe`): what they see is what the
//! net's peripherals see, when they see it, attaching one changes no
//! run, and a restore tells each its net's restored value.

use std::cell::RefCell;
use std::env;
use std::rc::Rc;

use mc683xx_board::board::Board;

use mc683xx_board::builtin::EdgeCounter;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::error::NetError;
use mc683xx_board::observe::Observer;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;

/// The probe's ports' nets, by port.
const PROBED: [&str; 2] = ["pps", "switch"];
/// How long the runs go: ten periods of the divided clock.
const RUN_PS: u64 = 1_000_000_000;

/// A change as a net's sink or observer sees it: the net, the time and
/// the value.
type Seen = (String, Time, Signal);

/// A peripheral that keeps each change on its inputs.
#[derive(Debug, Default)]
struct Probe {
    /// The changes, in order.
    seen: Vec<Seen>,
}

impl Peripheral for Probe {
    fn input(&mut self, port: usize, signal: Signal, now: Time, _drive: &mut Drive<'_>) {
        self.seen.push((PROBED[port].to_owned(), now, signal));
    }
}

/// A 10 MHz clock divided by 1000 onto `pps`, to PGP0 and an edge
/// counter, and `switch` from the host; with a probe on both if
/// `probed`, which cannot save.
fn board(probed: bool) -> Board<Mc68331> {
    let probe = if probed {
        r#"
[[peripheral]]
name = "probe"
type = "probe"
"#
    } else {
        ""
    };
    let sinks = |port: &str| {
        if probed {
            format!(", \"probe.{port}\"")
        } else {
            String::new()
        }
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
to = ["chip.PGP0", "counter.in"{pps}]
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["external.lamp"{switch}]
"#,
        pps = sinks("pps"),
        switch = sinks("switch"),
    );
    let description = Description::parse(&text, &env::temp_dir(), &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    registry
        .register(
            "probe",
            vec![
                PortSpec::input("pps", Kind::Edge),
                PortSpec::input("switch", Kind::Logic),
            ],
            |_| Ok(Box::new(Probe::default())),
        )
        .unwrap();
    Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap()
}

/// A log observers share.
#[derive(Debug, Default)]
struct Log(Rc<RefCell<Vec<Seen>>>);

impl Log {
    /// An observer that appends to it.
    fn observer(&self) -> impl Observer {
        let log = Rc::clone(&self.0);
        move |net: &str, at: Time, signal: Signal| {
            log.borrow_mut().push((net.to_owned(), at, signal))
        }
    }

    /// What it holds.
    fn seen(&self) -> Vec<Seen> {
        self.0.borrow().clone()
    }
}

/// What a [`Resyncing`] observer is told of a restore: the net, the time
/// and the value.
type Restored = (String, Time, Option<Signal>);

/// An observer that keeps what it is told of restores, and counts
/// changes.
#[derive(Debug, Default)]
struct Resyncing {
    /// The changes it was told.
    changes: Rc<RefCell<usize>>,
    /// The restores, in order.
    restored: Rc<RefCell<Vec<Restored>>>,
}

impl Observer for Resyncing {
    fn changed(&mut self, _: &str, _: Time, _: Signal) {
        *self.changes.borrow_mut() += 1;
    }

    fn restored(&mut self, net: &str, at: Time, signal: Option<Signal>) {
        self.restored
            .borrow_mut()
            .push((net.to_owned(), at, signal));
    }
}

/// Runs `board` to `RUN_PS`, the switch toggled along the way.
fn run(board: &mut Board<Mc68331>) {
    for (step, level) in [false, true, false].into_iter().enumerate() {
        board
            .run_until(Time(RUN_PS / 4 * (step as u64 + 1)))
            .unwrap();
        board.send("switch", Signal::Level(level)).unwrap();
    }
    board.run_until(Time(RUN_PS)).unwrap();
}

#[test]
fn an_observer_sees_what_a_sink_sees_when_it_sees_it() {
    let mut board = board(true);
    let log = Log::default();
    for net in PROBED {
        board.observe(net, log.observer()).unwrap();
    }
    run(&mut board);
    let probed = board.peripheral::<Probe>("probe").unwrap().seen.clone();
    assert!(probed.len() > 20, "{probed:?}");
    assert_eq!(log.seen(), probed);
    // the first rise: input edge 1000, the clock's phase after it
    assert_eq!(
        log.seen()
            .iter()
            .find(|(_, _, signal)| *signal == Signal::Level(true)),
        Some(&("pps".to_owned(), Time(100_000_037), Signal::Level(true)))
    );
}

#[test]
fn an_observer_changes_no_run() {
    let mut observed = board(false);
    let mut plain = board(false);
    let log = Log::default();
    observed.observe("pps", log.observer()).unwrap();
    run(&mut observed);
    run(&mut plain);
    assert!(!log.seen().is_empty());
    assert_eq!(observed.save().unwrap(), plain.save().unwrap());
    let count =
        |board: &Board<Mc68331>| board.peripheral::<EdgeCounter>("counter").unwrap().count();
    assert_eq!(count(&observed), count(&plain));
}

#[test]
fn an_observer_is_detached_by_its_id() {
    let mut board = board(false);
    let log = Log::default();
    let pps = board.observe("pps", log.observer()).unwrap();
    let switch = board.observe("switch", log.observer()).unwrap();
    assert_ne!(pps, switch);
    assert!(board.unobserve(pps).is_some());
    assert!(board.unobserve(pps).is_none());
    run(&mut board);
    assert!(log.seen().iter().all(|(net, _, _)| net == "switch"));
    assert_eq!(log.seen().len(), 3);
    assert!(matches!(
        board.observe("nowhere", log.observer()),
        Err(NetError::Unknown(name)) if name == "nowhere"
    ));
}

#[test]
fn a_restore_tells_each_observer_its_net_s_restored_value() {
    let mut board = board(false);
    let restored: Rc<RefCell<Vec<Restored>>> = Rc::default();
    let changes: Rc<RefCell<usize>> = Rc::default();
    for net in ["switch", "pps"] {
        board
            .observe(
                net,
                Resyncing {
                    changes: Rc::clone(&changes),
                    restored: Rc::clone(&restored),
                },
            )
            .unwrap();
    }
    let log = Log::default();
    board.observe("pps", log.observer()).unwrap();
    board.run_until(Time(RUN_PS / 4)).unwrap();
    let snapshot = board.save().unwrap();
    let (at, pps) = (board.now(), board.net_value("pps").unwrap());
    assert!(pps.is_some());
    run(&mut board);
    let seen = log.seen().len();
    let changed = *changes.borrow();
    assert!(board.restore(b"not a snapshot").is_err());
    assert!(
        restored.borrow().is_empty(),
        "a failed restore tells nothing"
    );
    board.restore(&snapshot).unwrap();
    assert_eq!(
        *restored.borrow(),
        [("pps".to_owned(), at, pps), ("switch".to_owned(), at, None)],
        "nets in file order"
    );
    assert_eq!(*changes.borrow(), changed, "a restore is no change");
    assert_eq!(log.seen().len(), seen, "a closure is told nothing");
}

#[test]
fn a_net_s_value_is_what_it_last_carried() {
    let mut board = board(false);
    assert_eq!(board.net_value("switch"), Ok(None));
    run(&mut board);
    assert_eq!(board.net_value("switch"), Ok(Some(Signal::Level(false))));
    assert!(board.net_value("nowhere").is_err());
    let nets: Vec<(&str, Kind)> = board.nets().collect();
    assert_eq!(
        nets,
        [
            ("ten-megahertz", Kind::Clock),
            ("pps", Kind::Edge),
            ("switch", Kind::Logic)
        ]
    );
}
