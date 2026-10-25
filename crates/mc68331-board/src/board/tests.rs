use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::frequency::Frequency;

use super::Board;
use super::BoardState;
use super::Change;
use super::VERSION;
use crate::builtin::Divider;
use crate::builtin::EdgeCounter;
use crate::description::Description;
use crate::description::Options;
use crate::mc68331::Pin;
use crate::peripheral::Peripheral;
use crate::registry::Environment;
use crate::registry::Registry;
use crate::signal::Signal;
use crate::time::Time;
use crate::time::TimeBase;
use crate::wiring::QspiState;
use mc683xx_mc68331::chip::Mc68331;

/// A board of a clock divided onto an edge counter, its ROM empty;
/// the counter restores before the divider.
const DIVIDED: &str = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
size = 0x10000
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "reference"
type = "clock"
parameters = { hz = 10_000_000 }
[[peripheral]]
name = "counter"
type = "edge-counter"
[[peripheral]]
name = "pps"
type = "divider"
parameters = { divide = 1000 }
[[net]]
name = "ten-megahertz"
type = "clock"
from = "reference.out"
to = ["pps.in"]
[[net]]
name = "one-pps"
type = "edge"
from = "pps.out"
to = ["counter.in"]
"#;

/// The board of `DIVIDED`, run a while.
fn divided() -> Board<Mc68331> {
    let description = Description::parse(DIVIDED, Path::new("."), &Options::default()).unwrap();
    let mut board = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap();
    board.run_until(Time(1_000_000)).unwrap();
    board
}

/// A board of a peripheral on the QSPI selected by PCS0 low, its ROM empty.
const ON_THE_QSPI: &str = r#"
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
size = 0x10000
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "peripheral"
type = "recorder"
attach = [{ qspi = 0b1110 }]
"#;

/// `snapshot` with `change` made to the board's own state.
fn edited(snapshot: &[u8], change: impl FnOnce(&mut BoardState<Pin>)) -> Vec<u8> {
    let mut state: BoardState<Pin> = decode(VERSION, snapshot).unwrap();
    change(&mut state);
    encode(VERSION, &state)
}

/// A change on net `net` by driver `driver`, numbered `sequence`.
fn waiting(net: usize, driver: usize, sequence: u64) -> ((Time, u64), Change) {
    (
        (Time(2_000_000), sequence),
        Change {
            net,
            driver,
            signal: Signal::Level(true),
        },
    )
}

#[test]
fn a_state_no_run_could_save_is_refused() {
    let mut board = divided();
    let snapshot = board.save().unwrap();
    let invalid = [
        (
            edited(&snapshot, |state| state.last.push(None)),
            StateError::mismatch("peripherals, nets or memories"),
        ),
        (
            edited(&snapshot, |state| {
                state.outputs.push((Pin::PortGp(3), true));
            }),
            StateError::mismatch("chip output pins"),
        ),
        (
            edited(&snapshot, |state| state.wired[0].0.push(true)),
            StateError::invalid("a wired net's drivers"),
        ),
        (
            edited(&snapshot, |state| {
                state.queue.push(waiting(99, 0, state.sequence - 1));
            }),
            StateError::invalid("a change waiting"),
        ),
        (
            edited(&snapshot, |state| {
                state.queue.push(waiting(0, 5, state.sequence - 1));
            }),
            StateError::invalid("a change waiting"),
        ),
        (
            edited(&snapshot, |state| {
                state.queue.push(waiting(0, 0, state.sequence));
            }),
            StateError::invalid("a change waiting"),
        ),
        (
            edited(&snapshot, |state| {
                let change = waiting(0, 0, state.sequence - 1);
                state.queue.extend([change, change]);
            }),
            StateError::invalid("a change waiting"),
        ),
        (
            edited(&snapshot, |state| state.takers[0] = Some(99)),
            StateError::invalid("a memory taken by no peripheral"),
        ),
        (
            edited(&snapshot, |state| {
                state.base = TimeBase::new(Cycles::ZERO, Time::ZERO, Frequency(0));
            }),
            StateError::invalid("the system clock's frequency"),
        ),
    ];
    board.run_until(Time(2_000_000)).unwrap();
    let before = board.save().unwrap();
    for (bytes, error) in invalid {
        assert_eq!(
            board.restore(&bytes).unwrap_err().to_string(),
            error.to_string()
        );
    }
    assert_eq!(board.save().unwrap(), before, "unchanged by the refusals");
    board.restore(&snapshot).unwrap();
}

#[test]
fn qspi_levels_past_pcs_3_0_are_refused() {
    let description = Description::parse(ON_THE_QSPI, Path::new("."), &Options::default()).unwrap();
    let mut board = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap();
    board.run_until(Time(1_000_000)).unwrap();
    let snapshot = board.save().unwrap();
    let with_levels = |levels| {
        edited(&snapshot, |state| {
            state.qspi = Some(QspiState::new(levels, 1));
        })
    };
    assert!(matches!(
        board.restore(&with_levels(0x1f)),
        Err(StateError::Invalid(reason)) if reason == "the QSPI's PCS levels"
    ));
    board.restore(&with_levels(0x0e)).unwrap();
}

#[test]
fn a_refused_snapshot_leaves_the_board_as_it_was() {
    let mut board = divided();
    let snapshot = board.save().unwrap();
    board.run_until(Time(2_000_000)).unwrap();
    let count = |board: &mut Board<Mc68331>| {
        board
            .peripheral::<EdgeCounter>("counter")
            .map(|counter| counter.count())
            .unwrap()
    };
    let counted = count(&mut board);
    let before = board.save().unwrap();
    // the counter's snapshot taken, then the divider's refused
    let other_ratio = edited(&snapshot, |state| {
        state.peripherals[2] = Divider::with_ratio(3, 1).save().unwrap();
    });
    assert_eq!(
        board.restore(&other_ratio).unwrap_err().to_string(),
        StateError::mismatch("a divider of another ratio")
            .of_peripheral("pps")
            .to_string()
    );
    assert_eq!(count(&mut board), counted);
    assert_eq!(board.save().unwrap(), before);
    // every peripheral's taken, then the chip's refused
    let no_chip = edited(&snapshot, |state| state.chip.clear());
    assert!(matches!(
        board.restore(&no_chip),
        Err(StateError::Truncated)
    ));
    assert_eq!(count(&mut board), counted);
    assert_eq!(board.save().unwrap(), before);
}

/// A peripheral that saves but refuses every snapshot, its own too,
/// which breaks [`Peripheral::restore`]'s contract; it counts the
/// restores it is asked for.
#[derive(Debug, Default)]
struct Refuser(Rc<Cell<u32>>);

impl Peripheral for Refuser {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(1, &()))
    }

    fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
        self.0.set(self.0.get() + 1);
        Err(StateError::invalid("a refuser"))
    }
}

#[test]
fn a_refusing_peripheral_is_not_rolled_back_and_the_ones_before_it_are() {
    let restores = Rc::new(Cell::new(0));
    let text = format!("{DIVIDED}[[peripheral]]\nname = \"refuser\"\ntype = \"refuser\"\n");
    let description = Description::parse(&text, Path::new("."), &Options::default()).unwrap();
    let mut registry = Registry::with_builtins();
    let counted = Rc::clone(&restores);
    registry
        .register("refuser", Vec::new(), move |_| {
            Ok(Box::new(Refuser(Rc::clone(&counted))))
        })
        .unwrap();
    let mut board =
        Board::<Mc68331>::load(&description, &registry, Environment::default()).unwrap();
    board.run_until(Time(1_000_000)).unwrap();
    let snapshot = board.save().unwrap();
    board.run_until(Time(2_000_000)).unwrap();
    let before = board.save().unwrap();
    assert_eq!(
        board.restore(&snapshot).unwrap_err().to_string(),
        StateError::invalid("a refuser")
            .of_peripheral("refuser")
            .to_string()
    );
    assert_eq!(restores.get(), 1, "asked once, not to take its backup");
    assert_eq!(board.save().unwrap(), before);
}
