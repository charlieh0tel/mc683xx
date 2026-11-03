//! The bindings on the host, through the methods JavaScript calls: the
//! demo board (`examples/boards/demo.toml`) booted and typed at, paced
//! frames, snapshots and the errors.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::Cpu;
use mc683xx_cpu32::cpu::Halt;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::memory::Memory;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;

use crate::board::Board;
use crate::board::BoardRun;
use crate::error::Error;

/// The demo board file.
const TEXT: &str = include_str!("../../../examples/boards/demo.toml");
/// Its firmware image.
const IMAGE: &[u8] = include_bytes!("../../../examples/boards/demo.bin");
/// The console's endpoint.
const CONSOLE: &str = "console";
/// Board time the boot runs for, past the banner and the prompt.
const BOOT: u64 = PICOSECONDS_PER_SECOND / 10;
/// The most board time a frame runs.
const SLICE: u64 = PICOSECONDS_PER_SECOND / 10;
/// A frame's wall time in milliseconds, about 60 a second.
const FRAME: f64 = 16.0;
/// Its board time in real time.
const FRAME_PICOSECONDS: u64 = PICOSECONDS_PER_SECOND / 1000 * 16;
/// How far a frame's board time may be from its wall time's, the page's
/// clock being a float: a nanosecond.
const ROUNDING: u64 = 1000;

/// The demo board, booted, its console's text since power-on.
fn booted() -> (Board, String) {
    let images = BTreeMap::from([("demo.bin".to_owned(), IMAGE.to_vec())]);
    let mut board = Board::with_images(TEXT, &images, &[]).unwrap();
    let run = board.run_for(BOOT).unwrap();
    assert_eq!(run.stop(), "budget");
    assert_eq!(run.now(), BOOT);
    let text = String::from_utf8(board.take_received(CONSOLE).unwrap()).unwrap();
    (board, text)
}

#[test]
fn the_demo_boots_and_answers_help() {
    let (mut board, text) = booted();
    assert!(text.contains("MC68331 demo"), "{text:?}");
    assert!(text.ends_with("> "), "{text:?}");
    board.send(CONSOLE, b"h\n").unwrap();
    board.run_for(BOOT).unwrap();
    let help = String::from_utf8(board.take_received(CONSOLE).unwrap()).unwrap();
    assert!(help.starts_with("h\r\n"), "{help:?}");
    assert!(help.ends_with("h, ?    this help\r\n> "), "{help:?}");
}

#[test]
fn frames_keep_to_the_clock_and_the_slice() {
    let (mut board, _) = booted();
    assert_eq!(board.pacing(), "unpaced");
    let start = board.now();
    assert_eq!(board.run_frame(0.0, SLICE).unwrap().now(), start + SLICE);

    board.set_pacing("realtime").unwrap();
    assert_eq!(board.pacing(), "realtime");
    let start = board.now();
    let wall = 5000.0;
    assert_eq!(board.run_frame(wall, SLICE).unwrap().now(), start, "starts");
    let run = board.run_frame(wall + FRAME, SLICE).unwrap();
    let paced = run.now() - start;
    assert!(paced.abs_diff(FRAME_PICOSECONDS) <= ROUNDING, "{paced}");
    // an hour behind runs one slice
    let behind = board.now();
    let run = board.run_frame(wall + 3_600_000.0, SLICE).unwrap();
    assert_eq!(run.now(), behind + SLICE);

    board.set_pacing("2").unwrap();
    assert_eq!(board.pacing(), "2");
}

#[test]
fn a_run_of_no_time_starts_the_frames_pacing_again() {
    let (mut board, _) = booted();
    board.set_pacing("realtime").unwrap();
    let wall = 5000.0;
    board.run_frame(wall, SLICE).unwrap();
    let before = board.run_frame(wall + FRAME, SLICE).unwrap().now();
    board.run_for(0).unwrap();
    let run = board.run_frame(wall + 2.0 * FRAME, SLICE).unwrap();
    assert_eq!(run.now(), before, "starts");
}

#[test]
fn a_restore_goes_back() {
    let (mut board, _) = booted();
    let snapshot = board.save().unwrap();
    let saved = board.now();
    board.send(CONSOLE, b"h\n").unwrap();
    board.run_for(BOOT).unwrap();
    board.restore(&snapshot).unwrap();
    assert_eq!(board.now(), saved);
    board.run_for(BOOT).unwrap();
    assert!(board.take_received(CONSOLE).unwrap().is_empty());
    assert_eq!(board.memory("rom").unwrap()[..IMAGE.len()], *IMAGE);
}

#[test]
fn mistakes_are_errors() {
    let images = BTreeMap::new();
    let error = Board::with_images(TEXT, &images, &[]).unwrap_err();
    assert!(matches!(error, Error::Load(_)), "{error}");
    assert!(error.to_string().contains("demo.bin"), "{error}");
    let (mut board, _) = booted();
    assert!(matches!(
        board.set_pacing("fast"),
        Err(Error::ParsePacing(_))
    ));
    assert!(matches!(
        board.run_frame(f64::NAN, SLICE),
        Err(Error::Wall(_))
    ));
    assert!(matches!(board.run_frame(-1.0, SLICE), Err(Error::Wall(_))));
    assert!(matches!(board.send("nowhere", b"h"), Err(Error::Send(_))));
    assert!(matches!(board.send("nowhere", b""), Err(Error::Send(_))));
    assert!(matches!(
        board.take_received("nowhere"),
        Err(Error::Receive(_))
    ));
    assert!(matches!(board.memory("nowhere"), Err(Error::Memory(_))));
    assert!(matches!(board.restore(b"junk"), Err(Error::State(_))));
}

#[test]
fn each_kind_of_stop_has_its_name() {
    let hook = Cpu::new(Memory::new(24).unwrap())
        .add_code_hook(Address(0)..=Address(0), |_| ControlFlow::Continue(()));
    let names = [
        (Stop::Budget, "budget"),
        (Stop::Stopped, "stopped"),
        (Stop::Halted(Halt::DoubleBusFault), "halted"),
        (Stop::Hook(hook), "hook"),
        (Stop::Requested, "requested"),
        (Stop::Background(Entry::Background), "background"),
        (Stop::Fault, "fault"),
    ];
    for (stop, name) in names {
        let run = mc683xx_board::board::BoardRun::new(stop, Time(0));
        assert_eq!(BoardRun::from(run).stop(), name, "{stop:?}");
    }
}
