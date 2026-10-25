//! A board's control handle (`Board::control`) and paced runs
//! (`Board::run_until_paced`): a stop and inputs from another thread, taken
//! at sync points, and pacing that holds board time to wall time and
//! changes nothing the board does.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use mc683xx_board::board::Board;

use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::error::SendError;
use mc683xx_board::pacing::Pacing;
use mc683xx_board::pacing::PacingError;
use mc683xx_board::pacing::QUANTUM;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_mc68331::chip::Mc68331;

/// A millisecond in picoseconds.
const MILLISECOND: u64 = PICOSECONDS_PER_SECOND / 1000;

/// A 10 MHz clock divided by 1000 onto `tick`, to PGP0, and `switch`
/// from the host to PGP1.
const BOARD: &str = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "reference"
type = "clock"
parameters = { hz = 10_000_000 }
[[peripheral]]
name = "divider"
type = "divider"
parameters = { divide = 1000 }
[[net]]
name = "ten-megahertz"
type = "clock"
from = "reference.out"
to = ["divider.in"]
[[net]]
name = "tick"
type = "edge"
from = "divider.out"
to = ["chip.PGP0"]
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["chip.PGP1"]
"#;

/// The board.
fn board() -> Board<Mc68331> {
    let description = Description::parse(BOARD, Path::new("."), &Options::default()).unwrap();
    Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap()
}

/// A change a net carried: its net, time and value.
type Seen = (String, Time, Signal);

/// Observes `nets` of `board`: what they carry from now on.
fn observed(board: &mut Board<Mc68331>, nets: &[&str]) -> Rc<RefCell<Vec<Seen>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    for net in nets {
        let kept = Rc::clone(&seen);
        board
            .observe(net, move |net: &str, at: Time, signal: Signal| {
                kept.borrow_mut().push((net.to_owned(), at, signal));
            })
            .unwrap();
    }
    seen
}

#[test]
fn inputs_queued_between_runs_are_driven_at_the_next_runs_start_in_order() {
    let mut board = board();
    board.run_until(Time(MILLISECOND)).unwrap();
    let seen = observed(&mut board, &["switch"]);
    let control = board.control();
    control.send("switch", Signal::Level(false)).unwrap();
    control.send("switch", Signal::Level(true)).unwrap();
    assert_eq!(
        control.send("lamp", Signal::Level(true)),
        Err(SendError::Endpoint("lamp".to_owned()))
    );
    assert_eq!(
        control.send("switch", Signal::Analog(1.0)),
        Err(SendError::Kind {
            endpoint: "switch".to_owned(),
            kinds: vec![Kind::Logic],
        })
    );
    assert!(seen.borrow().is_empty(), "nothing before the run");
    board.run_until(Time(2 * MILLISECOND)).unwrap();
    let at = Time(MILLISECOND);
    assert_eq!(
        *seen.borrow(),
        [
            ("switch".to_owned(), at, Signal::Level(false)),
            ("switch".to_owned(), at, Signal::Level(true)),
        ]
    );
}

#[test]
fn a_direct_send_lands_after_inputs_queued_before_it() {
    let mut board = board();
    let seen = observed(&mut board, &["switch"]);
    board.control().send("switch", Signal::Level(true)).unwrap();
    board.send("switch", Signal::Level(false)).unwrap();
    board.run_until(Time(MILLISECOND)).unwrap();
    let values: Vec<Signal> = seen.borrow().iter().map(|seen| seen.2).collect();
    assert_eq!(values, [Signal::Level(true), Signal::Level(false)]);
}

#[test]
fn a_stop_asked_between_runs_stops_the_next_at_its_start_once() {
    let mut board = board();
    board.control().stop();
    let outcome = board.run_until(Time(MILLISECOND)).unwrap();
    assert_eq!((outcome.stop, outcome.now), (Stop::Requested, Time::ZERO));
    let outcome = board.run_until(Time(MILLISECOND)).unwrap();
    assert_eq!(
        (outcome.stop, outcome.now),
        (Stop::Budget, Time(MILLISECOND))
    );
}

#[test]
fn a_bad_factor_is_refused_and_the_pacing_kept() {
    let control = board().control();
    assert_eq!(control.pacing(), Pacing::Unpaced);
    control.set_pacing(Pacing::Scaled(0.5)).unwrap();
    assert_eq!(
        control.set_pacing(Pacing::Scaled(-1.0)),
        Err(PacingError::Factor(-1.0))
    );
    assert_eq!(control.pacing(), Pacing::Scaled(0.5));
}

#[test]
fn another_thread_stops_a_paced_run_and_its_input_lands_at_a_sync_point() {
    let mut board = board();
    let seen = observed(&mut board, &["switch", "tick"]);
    let control = board.control();
    // slow enough that the run is still going when the thread acts
    control.set_pacing(Pacing::Scaled(0.1)).unwrap();
    let front_end = control.clone();
    let thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        front_end.send("switch", Signal::Level(true)).unwrap();
        thread::sleep(Duration::from_millis(50));
        front_end.stop();
    });
    let until = Time(10 * PICOSECONDS_PER_SECOND);
    let outcome = board.run_until_paced(until, QUANTUM).unwrap();
    thread.join().unwrap();
    assert_eq!(outcome.stop, Stop::Requested);
    assert!(outcome.now < until);
    let seen = seen.borrow();
    let switched: Vec<&Seen> = seen.iter().filter(|(net, ..)| net == "switch").collect();
    let [(_, at, signal)] = switched[..] else {
        panic!("{switched:?}");
    };
    assert_eq!(*signal, Signal::Level(true));
    assert!(Time::ZERO < *at && *at <= outcome.now, "{at:?}");
    // a sync point: a quantum's end, or a quantum's, at a tick's edge
    assert!(
        at.0 % QUANTUM.get() == 0
            || seen
                .iter()
                .any(|(net, edge, _)| net == "tick" && edge == at),
        "{at:?}"
    );
    assert!(!control.stopping(), "the stop taken");
}

#[test]
fn a_paced_run_is_the_unpaced_run() {
    let total = Time(50 * MILLISECOND);
    let run = |pacing: Pacing| {
        let mut board = board();
        let seen = observed(&mut board, &["tick", "switch"]);
        let control = board.control();
        control.set_pacing(pacing).unwrap();
        control.send("switch", Signal::Level(false)).unwrap();
        let outcome = board.run_until_paced(total, QUANTUM).unwrap();
        let seen = seen.borrow().clone();
        (outcome, seen, board.save().unwrap())
    };
    let unpaced = run(Pacing::Unpaced);
    assert_eq!(unpaced.0.stop, Stop::Budget);
    assert!(unpaced.1.len() > 100, "{} changes", unpaced.1.len());
    assert!(run(Pacing::Scaled(4.0)) == unpaced);
    let mut board = board();
    board
        .control()
        .send("switch", Signal::Level(false))
        .unwrap();
    board.run_until(total).unwrap();
    assert!(board.save().unwrap() == unpaced.2, "one run");
}

#[test]
fn real_time_pacing_keeps_board_time_with_wall_time() {
    let total = Time(200 * MILLISECOND);
    let started = Instant::now();
    board().run_until(total).unwrap();
    let unpaced = started.elapsed();
    let mut board = board();
    board.control().set_pacing(Pacing::RealTime).unwrap();
    let started = Instant::now();
    let outcome = board.run_until_paced(total, QUANTUM).unwrap();
    let paced = started.elapsed();
    assert_eq!(outcome.now, total);
    let board_time = Duration::from_millis(200);
    // never ahead of wall time, its last quantum waited out too
    assert!(paced >= board_time, "{paced:?}");
    // generous above, for a loaded host
    assert!(
        paced <= board_time + unpaced + Duration::from_secs(2),
        "{paced:?}, unpaced {unpaced:?}"
    );
}

#[test]
fn short_paced_runs_in_a_loop_are_paced() {
    let mut board = board();
    board.control().set_pacing(Pacing::Scaled(0.1)).unwrap();
    let started = Instant::now();
    for _ in 0..20 {
        let until = board.now() + 5 * MILLISECOND;
        board.run_until_paced(until, QUANTUM).unwrap();
    }
    // 100 ms of board time at a tenth of real time
    let paced = started.elapsed();
    assert!(paced >= Duration::from_secs(1), "{paced:?}");
}
