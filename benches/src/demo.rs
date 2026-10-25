//! The example demo board (`examples/boards/demo.toml`) and its C
//! firmware: a console command typed at its prompt and run to the next
//! prompt, from a snapshot taken at the first.  Compiled code, not a
//! kernel: nearly all the command's own work, the SCI at its fastest
//! rate leaving `put_char` little to wait for TDRE.

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

/// The board file, from this crate's directory.
const BOARD: &str = "../examples/boards/demo.toml";
/// The console's run-time endpoint.
const CONSOLE: &str = "console";
/// Board time a typed byte takes, as `mc683xx-run` types piped input:
/// a byte a 1 ms turn.
const TURN: u64 = PICOSECONDS_PER_SECOND / 1000;
/// The longest the boot or a command may take to its prompt: 2 s.
const LONGEST: u64 = 2 * PICOSECONDS_PER_SECOND;
/// What ends a command's output: the next prompt.
const PROMPT: &[u8] = b"> ";
/// The Mandelbrot command, its line as typed.
pub const MANDELBROT: &[u8] = b"m\n";

/// The board at its first prompt, and a command's run from there.
#[derive(Debug)]
pub struct Demo {
    /// The board.
    board: Board<Mc68331>,
    /// Its snapshot at the first prompt.
    prompt: Vec<u8>,
    /// The command line typed.
    line: &'static [u8],
    /// Board time from the prompt to the command's next prompt, in
    /// whole turns, picoseconds.
    pub span: u64,
    /// Instructions the command runs, its typing among them.
    pub instructions: u64,
    /// What the console showed: the echo, the output, the prompt.
    pub output: Vec<u8>,
}

impl Demo {
    /// The board booted to its prompt and `line` calibrated: run once,
    /// turn by turn, to its next prompt.
    ///
    /// # Panics
    ///
    /// If the board does not load or save, the CPU halts, or the boot
    /// or the command does not reach a prompt within 2 s.
    #[must_use]
    pub fn new(line: &'static [u8]) -> Self {
        let mut board = Board::<Mc68331>::read(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(BOARD),
            &Options::default(),
            &Registry::with_builtins(),
            Environment::default(),
        )
        .expect("the demo board");
        Self::to_prompt(&mut board, b"");
        let prompt = board.save().expect("a snapshot");
        let start = board.now();
        let before = board.chip().instructions();
        let output = Self::to_prompt(&mut board, line);
        Self {
            span: board.now().since(start),
            instructions: board.chip().instructions() - before,
            board,
            prompt,
            line,
            output,
        }
    }

    /// One iteration: the board back at the prompt, the line typed, a
    /// byte a turn, and the run to the end of its span; what the
    /// console showed.
    ///
    /// # Panics
    ///
    /// If the snapshot does not restore or the CPU halts.
    pub fn iteration(&mut self) -> Vec<u8> {
        self.board.restore(&self.prompt).expect("the snapshot");
        let start = self.board.now();
        for (turn, &byte) in (0..).zip(self.line) {
            Self::type_byte(&mut self.board, byte);
            Self::run(&mut self.board, start + turn * TURN + TURN);
        }
        Self::run(&mut self.board, start + self.span);
        Self::received(&mut self.board)
    }

    /// What the console shows as `line` is typed, a byte a turn, turn
    /// by turn to the next prompt.
    fn to_prompt(board: &mut Board<Mc68331>, line: &[u8]) -> Vec<u8> {
        let start = board.now();
        let mut output = Vec::new();
        let mut typed = line.iter();
        while !(typed.len() == 0 && output.ends_with(PROMPT)) {
            assert!(
                board.now().since(start) < LONGEST,
                "no prompt after {:?}",
                String::from_utf8_lossy(line)
            );
            if let Some(&byte) = typed.next() {
                Self::type_byte(board, byte);
            }
            let until = board.now() + TURN;
            Self::run(board, until);
            output.extend(Self::received(board));
        }
        output
    }

    /// `byte` typed on the console.
    fn type_byte(board: &mut Board<Mc68331>, byte: u8) {
        let frame = Serial::untimed(Frame::Data(byte.into()));
        board
            .send(CONSOLE, Signal::Serial(frame))
            .expect("the console takes input");
    }

    /// The board run to `until`, the CPU not halting.
    fn run(board: &mut Board<Mc68331>, until: Time) {
        let run = board.run_until(until).expect("a run");
        assert_eq!(run.stop, Stop::Budget, "at {}", run.now);
    }

    /// The bytes the console received since the last call.
    fn received(board: &mut Board<Mc68331>) -> Vec<u8> {
        board
            .take_received(CONSOLE)
            .expect("the console receives")
            .into_iter()
            .filter_map(|(_, signal)| match signal {
                Signal::Serial(Serial {
                    frame: Frame::Data(data),
                    ..
                }) => u8::try_from(data).ok(),
                _ => None,
            })
            .collect()
    }
}
