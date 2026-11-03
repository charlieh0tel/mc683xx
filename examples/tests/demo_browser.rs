//! The demo board (`boards/demo.toml`) through the API a browser host
//! uses, run here on the host: no file read, no clock read and no
//! sleep.  The board file's text and its image are handed over as bytes
//! (`Description::parse_with`); the board boots unpaced
//! (`Board::run_until_paced`, which then reads no clock), then runs in
//! frames paced by a clock the test keeps (`Board::run_frame`), as a page's
//! `requestAnimationFrame` loop with `performance.now()` would; a
//! command typed on the console comes back echoed, the help after it.

use std::io;
use std::path::Path;
use std::time::Duration;

use mc683xx_board::board::Board;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::pacing::Pacing;
use mc683xx_board::pacing::QUANTUM;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::qsm::sci::Frame;

/// The board file, as a page would fetch it.
const TEXT: &str = include_str!("../boards/demo.toml");
/// Its firmware image, as a page would fetch it.
const IMAGE: &[u8] = include_bytes!("../boards/demo.bin");
/// The image's name in the board file.
const IMAGE_NAME: &str = "demo.bin";
/// The console's run-time endpoint.
const CONSOLE: &str = "console";
/// Board time the boot runs for, past the banner and the prompt.
const BOOT: Time = Time(PICOSECONDS_PER_SECOND / 10);
/// A frame's wall time, about 60 a second.
const FRAME: Duration = Duration::from_micros(16_667);
/// Board seconds a wall second while paced.
const FACTOR: f64 = 4.0;
/// The most board time a frame runs, however far behind: a page's
/// bound on a slice.
const SLICE: u64 = PICOSECONDS_PER_SECOND / 10;
/// The most frames the help may take to come back.
const FRAMES: u32 = 200;
/// What is typed, a byte a frame: the help command.
const TYPED: &[u8] = b"h\n";
/// The banner the firmware prints at boot.
const BANNER: &str = "MC68331 demo";
/// The help's last line and the prompt after it.
const HELP_END: &str = "h, ?    this help\r\n> ";

/// The image's bytes, for its name alone.
fn read(path: &Path) -> io::Result<Vec<u8>> {
    if path == Path::new(IMAGE_NAME) {
        Ok(IMAGE.to_vec())
    } else {
        Err(io::ErrorKind::NotFound.into())
    }
}

/// The console's text the board sent since the last look, appended to
/// `text`.
fn take_console(board: &mut Board<Mc68331>, text: &mut String) {
    for (_, signal) in board.take_received(CONSOLE).unwrap() {
        if let Signal::Serial(serial) = signal
            && let Some(data) = serial.frame.data()
        {
            text.push(char::from(u8::try_from(data).expect("8-bit frames")));
        }
    }
}

#[test]
fn the_demo_board_runs_from_bytes_and_a_host_clock() {
    let description =
        Description::parse_with(TEXT, Path::new(""), &Options::default(), read).unwrap();
    let mut board = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .unwrap();
    let control = board.control();
    assert_eq!(control.pacing(), Pacing::Unpaced);
    let run = board.run_until_paced(BOOT, QUANTUM).unwrap();
    assert_eq!(run.now, BOOT);
    let mut text = String::new();
    take_console(&mut board, &mut text);
    assert!(text.contains(BANNER), "{text:?}");
    assert!(text.ends_with("> "), "{text:?}");

    control.set_pacing(Pacing::Scaled(FACTOR)).unwrap();
    let mut typed = TYPED.iter();
    let mut wall = Duration::from_secs(1000);
    let start = board.now();
    text.clear();
    let mut frames = 0;
    while !text.ends_with(HELP_END) {
        assert!(frames < FRAMES, "no help after {frames} frames: {text:?}");
        if let Some(&byte) = typed.next() {
            let frame = Serial::untimed(Frame::Data(byte.into()));
            board.send(CONSOLE, Signal::Serial(frame)).unwrap();
        }
        let run = board.run_frame(wall, SLICE).unwrap();
        assert_eq!(run.stop, Stop::Budget);
        take_console(&mut board, &mut text);
        wall += FRAME;
        frames += 1;
    }
    assert!(text.starts_with("h\r\n"), "the echo first: {text:?}");
    // the first frame starts the pacing; each after runs a frame's worth
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a frame's board picoseconds"
    )]
    let frame = (FRAME.as_secs_f64() * FACTOR * PICOSECONDS_PER_SECOND as f64) as u64;
    let paced = board.now().0 - start.0;
    assert!(
        paced.abs_diff(u64::from(frames - 1) * frame) <= u64::from(frames),
        "{paced}"
    );
}
