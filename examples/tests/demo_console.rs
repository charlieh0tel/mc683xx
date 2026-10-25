//! The demo board's console (`boards/demo.toml`, firmware
//! `firmware/demo`) against a golden: a script of every command fed as
//! `mc683xx-run --bind console=stdio --input-after 0.01` feeds piped
//! input, a byte a 1 ms turn, and what comes back compared with
//! `tests/golden/demo-console.golden`, its CR LF line ends as LF (every
//! LF the firmware sends follows a CR).  Only the text is pinned, not the
//! cycles or instructions; but the `t` lines' uptimes count PIT ticks,
//! so they move with instruction timing (and with the firmware): a
//! timing change shows here as a golden diff, on purpose.  After a
//! change that is meant, `UPDATE_GOLDEN=1 cargo test` rewrites the
//! golden (the convention of the snapshot goldens, `snapshot-golden`).

use std::env;
use std::fs;
use std::path::Path;

use mc683xx_board::board::Board;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::qsm::sci::Frame;

/// What is typed: every command.
const SCRIPT: &[u8] = b"h\nm\np\nt\nc\nx 0\nf\nt\n";
/// The console's run-time endpoint.
const CONSOLE: &str = "console";
/// Board time between turns, a byte of input a turn: `mc683xx-run`'s
/// default `--turn-us`, 1 ms.
const TURN: u64 = PICOSECONDS_PER_SECOND / 1000;
/// Board time before input goes in, past the firmware's enabling its
/// receiver: 10 ms.
const INPUT_AFTER: Time = Time(PICOSECONDS_PER_SECOND / 100);
/// Board time the run ends at, past the last command's end (about
/// 2.15 s).
const END: Time = Time::from_seconds(3);
/// The variable that asks a run to write the golden.
const UPDATE: &str = "UPDATE_GOLDEN";
/// The golden, from the crate's directory.
const GOLDEN: &str = "tests/golden/demo-console.golden";

/// The console's text from power-on to `END`, with `SCRIPT` typed.
fn console() -> Vec<u8> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry).unwrap();
    let mut board = Board::<Mc68331>::read(
        &directory.join("boards/demo.toml"),
        &Options::default(),
        &registry,
        Environment::default(),
    )
    .unwrap();
    let mut typed = SCRIPT.iter();
    let mut text = Vec::new();
    while board.now() < END {
        if board.now() >= INPUT_AFTER
            && let Some(&byte) = typed.next()
        {
            let frame = Serial::untimed(Frame::Data(byte.into()));
            board.send(CONSOLE, Signal::Serial(frame)).unwrap();
        }
        let run = board.run_until((board.now() + TURN).min(END)).unwrap();
        assert_eq!(run.stop, Stop::Budget, "at {}", run.now);
        for (_, signal) in board.take_received(CONSOLE).unwrap() {
            if let Signal::Serial(Serial {
                frame: Frame::Data(data),
                ..
            }) = signal
            {
                text.push(u8::try_from(data).expect("8-bit frames"));
            }
        }
    }
    assert_eq!(typed.len(), 0, "the script was not all typed");
    text
}

#[test]
fn the_demo_console_answers_every_command_as_its_golden() {
    let text = String::from_utf8(console()).expect("ASCII");
    assert_eq!(
        text.matches('\n').count(),
        text.matches("\r\n").count(),
        "a line ends without a CR"
    );
    let text = text.replace("\r\n", "\n");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if env::var_os(UPDATE).is_some() {
        fs::write(&path, &text).expect("writes the golden");
        return;
    }
    let golden = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no golden {}: run {UPDATE}=1 cargo test", path.display()));
    assert!(
        text == golden,
        "the demo console differs from {}; if the change is meant, \
         regenerate it with {UPDATE}=1 cargo test\n--- found ---\n{text}",
        path.display()
    );
}
