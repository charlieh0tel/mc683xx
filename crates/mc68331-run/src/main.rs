//! `mc683xx-run`: runs a board file ([PLAN.md, Boards](https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards);
//! [`docs/board-files.md`](https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md)).
//! A thin command on `mc683xx_board::board::Board`'s run loop: it loads
//! the file with the built-in peripheral types and the stock
//! peripherals (`mc683xx_peripherals`), binds its run-time
//! endpoints, runs it for a time, traces it ([`docs/tracing.md`](https://github.com/charlieh0tel/mc683xx/blob/main/docs/tracing.md)), and
//! says why it stopped.  With `--gdb` it waits at reset for gdb,
//! which runs the board until it detaches ([docs/gdb.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md)).
//!
//! Exit status: 0 the run spent its time; 1 the board or a file could
//! not be used; 2 the command line is wrong; 3 the CPU halted; 4 a
//! hook or a peripheral stopped the run, gdb killed it, or Ctrl-C
//! (SIGINT) did; 130 a second Ctrl-C ended the process at once.
// The published crates also hold these; test-only crates do not.
#![warn(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc
)]

mod binding;
mod error;
mod gdb;

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::num::ParseIntError;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use clap::Parser;
use clap::ValueEnum;
use mc683xx_board::board::Board;
use mc683xx_board::board::BoardRun;
use mc683xx_board::board::BoardTracing;
use mc683xx_board::chip::Chip;
use mc683xx_board::description::Options;
use mc683xx_board::pacing::Pacer;
use mc683xx_board::pacing::Pacing;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_board::trace::Line;
use mc683xx_board::trace::header;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_gdb::stub::Ending;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::qsm::sci::Frame;

use crate::binding::Binding;
use crate::binding::How;
use crate::error::RunError;

/// The chip the runner runs, which names its trace's lines.
const CHIP: &str = <Mc68331 as Chip>::TYPE;
/// Board seconds a run takes unless told otherwise.
const DEFAULT_SECONDS: f64 = 1.0;
/// Picoseconds in a microsecond, `--turn-us`'s unit.
const PICOSECONDS_PER_MICROSECOND: u64 = PICOSECONDS_PER_SECOND / 1_000_000;
/// The longest turn, in microseconds, whose picoseconds a `u64` holds.
const MAX_TURN_US: u64 = u64::MAX / PICOSECONDS_PER_MICROSECOND;
/// Exit status: the board or a file could not be used.
const EXIT_ERROR: u8 = 1;
/// Exit status: the CPU halted.
const EXIT_HALTED: u8 = 3;
/// Exit status: a hook or a peripheral stopped the run.
const EXIT_STOPPED: u8 = 4;
/// Exit status: a second Ctrl-C, the shell's for death by SIGINT
/// (128 + 2).
const EXIT_INTERRUPTED: i32 = 130;
/// What `--help` says after the options.
const AFTER_HELP: &str = "Ctrl-C stops the run at its next sync point: the `stopped` record is \
written and --save still saves (exit status 4).  A second Ctrl-C ends the process at once, \
saving nothing (exit status 130).

Exit status: 0 the run spent its time; 1 the board or a file could not be used; 2 the \
command line is wrong; 3 the CPU halted; 4 a hook, a peripheral, gdb or Ctrl-C stopped it.";

/// What `--trace` traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Traced {
    /// Bus cycles board peripherals answer.
    BusCycles,
    /// Exceptions entered.
    Exceptions,
    /// Chip pins on nets.
    Pins,
    /// QSPI transfers.
    Qspi,
    /// Every value a net carries.
    Nets,
}

/// Runs an MC68331 board file.
#[derive(Debug, Parser)]
#[command(version, about, after_help = AFTER_HELP)]
struct Arguments {
    /// The board file.
    board: PathBuf,
    /// Run until the chip has run this many system clocks.
    #[arg(long, conflicts_with = "seconds")]
    cycles: Option<u64>,
    /// Run this many seconds of board time (1 by default).
    #[arg(long, value_parser = seconds)]
    seconds: Option<f64>,
    /// The firmware image: replaces each memory image that is one of
    /// the file's `[board] images`.
    #[arg(long)]
    firmware: Option<PathBuf>,
    /// A memory's image, NAME=PATH; may be given more than once.
    #[arg(long = "image", value_parser = pair::<PathBuf>)]
    images: Vec<(String, PathBuf)>,
    /// Enable a board option the file declares; may be given more than
    /// once.
    #[arg(long = "option")]
    options: Vec<String>,
    /// Set a peripheral's parameter, PERIPHERAL.KEY=VALUE, the value TOML (a
    /// bare word a string), over the file's; may be given more than
    /// once.
    #[arg(long = "set")]
    settings: Vec<String>,
    /// Bind a run-time endpoint, NAME=stdio|stdout|pty|null|file:PATH
    /// (pty on Unix only); may be given more than once.  Unbound
    /// endpoints are null.  stdout writes what stdio writes and reads
    /// nothing, leaving standard input to another binding; bindings
    /// that share standard output each write to it, and the first
    /// stdio on the command line takes standard input.  An endpoint that drives no serial
    /// net takes no input: stdio (with standard input) and pty are
    /// refused there, and stdout, a file or null serves.  Output a
    /// pty's buffer cannot take, with nobody reading it, is dropped, as
    /// a UART's is with nothing on the line.  With stdio, standard
    /// input that is not a terminal (a file or a pipe) is read to its
    /// end when input first goes in, and fed a byte a turn, so the run
    /// is the same each time: the run waits there for the pipe to
    /// close.  A terminal's is live.
    #[arg(long = "bind", value_parser = pair::<How>)]
    bindings: Vec<(String, How)>,
    /// What to trace on standard error, comma-separated.
    #[arg(long, value_enum, value_delimiter = ',')]
    trace: Vec<Traced>,
    /// Trace each time the instruction at this address, hexadecimal,
    /// runs; may be given more than once.
    #[arg(long = "watch-code", value_parser = hexadecimal)]
    watch_code: Vec<u32>,
    /// Trace where the CPU is every this many seconds of board time.
    #[arg(long, value_parser = seconds)]
    position: Option<f64>,
    /// The seed every noise model draws from.
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// Where peripherals keep working copies.
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Restore this snapshot before running.
    #[arg(long)]
    restore: Option<PathBuf>,
    /// Save a snapshot here after running.  A board with a peripheral
    /// that cannot save, or a file that will not open, is refused before
    /// the run; the file is replaced only at its end, whole: the
    /// snapshot is written beside it and renamed over it.
    #[arg(long)]
    save: Option<PathBuf>,
    /// Microseconds of board time between the runner's turns, where
    /// input goes in (one fed byte a turn), output comes out and
    /// positions are taken.
    #[arg(long, default_value_t = 1000,
          value_parser = clap::value_parser!(u64).range(1..=MAX_TURN_US))]
    turn_us: u64,
    /// Seconds of board time before input goes in: a frame that
    /// arrives before the firmware enables its receiver is lost, as on
    /// the board.
    #[arg(long, default_value_t = 0.0, value_parser = seconds)]
    input_after: f64,
    /// Wait at reset for gdb at this address, HOST:PORT (port 0 picks
    /// one, which standard error names), and let it run the board; when
    /// gdb detaches the run goes on to its end.
    #[arg(long, value_name = "HOST:PORT")]
    gdb: Option<String>,
    /// How fast board time goes against wall time: unpaced (as fast as
    /// the host runs it), realtime, or a factor, board seconds a wall
    /// second (2 twice real time, 0.5 half).  The run is the same
    /// whatever the pacing: wall time decides only how long the runner
    /// sleeps between turns.
    #[arg(long, default_value_t = Pacing::Unpaced)]
    pace: Pacing,
}

/// `NAME=VALUE`.
fn pair<T: FromStr>(text: &str) -> Result<(String, T), String>
where
    T::Err: Display,
{
    let (name, value) = text
        .split_once('=')
        .ok_or_else(|| format!("{text:?}: give NAME=VALUE"))?;
    let value = value
        .parse()
        .map_err(|error| format!("{text:?}: {error}"))?;
    Ok((name.to_owned(), value))
}

/// Seconds of board time: a number, finite and not negative.
fn seconds(text: &str) -> Result<f64, String> {
    let seconds: f64 = text.parse().map_err(|error| format!("{text:?}: {error}"))?;
    if seconds.is_finite() && seconds >= 0.0 {
        Ok(seconds)
    } else {
        Err(format!(
            "{text:?}: give a finite number of seconds, 0 or more"
        ))
    }
}

/// An address, hexadecimal, `0x` or not.
fn hexadecimal(text: &str) -> Result<u32, ParseIntError> {
    u32::from_str_radix(text.trim_start_matches("0x"), 16)
}

/// Seconds as board time, picoseconds.  Floats only parse the command
/// line; the run's time is integers.
#[expect(
    clippy::cast_sign_loss,
    reason = "the flags refuse negative and non-finite seconds"
)]
fn picoseconds(seconds: f64) -> u64 {
    (seconds * PICOSECONDS_PER_SECOND as f64) as u64
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    match run(&arguments) {
        Ok(outcome) => match outcome.stop {
            Stop::Budget | Stop::Stopped => ExitCode::SUCCESS,
            Stop::Halted(_) => ExitCode::from(EXIT_HALTED),
            _ => ExitCode::from(EXIT_STOPPED),
        },
        Err(error) => {
            eprintln!("mc683xx-run: {error}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

/// Loads, binds and runs the board as `arguments` say.
fn run(arguments: &Arguments) -> Result<BoardRun, RunError> {
    let mut options = Options::default();
    options.enabled = arguments.options.iter().cloned().collect();
    options.firmware.clone_from(&arguments.firmware);
    options.images = arguments.images.iter().cloned().collect();
    for setting in &arguments.settings {
        options.set(setting)?;
    }
    let mut environment = Environment::default();
    environment.seed = arguments.seed;
    environment.work_dir = arguments.work_dir.clone();
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry)?;
    let mut board = Board::<Mc68331>::read(&arguments.board, &options, &registry, environment)?;
    if let Some(path) = &arguments.restore {
        let snapshot = fs::read(path).map_err(|error| RunError::Io {
            path: path.clone(),
            error,
        })?;
        board.restore(&snapshot)?;
    }
    let saving = arguments
        .save
        .as_ref()
        .map(|path| saveable(&board, path))
        .transpose()?;
    let senders: BTreeSet<String> = board
        .externals()
        .into_iter()
        .filter(|external| external.kind == Kind::Serial && external.direction.drives())
        .map(|external| external.name)
        .collect();
    let mut externals: Vec<String> = board
        .externals()
        .into_iter()
        .map(|external| external.name)
        .collect();
    externals.dedup();
    let asked: BTreeMap<&str, &How> = arguments
        .bindings
        .iter()
        .map(|(name, how)| (name.as_str(), how))
        .collect();
    if let Some(name) = asked
        .keys()
        .find(|name| !externals.iter().any(|external| external == *name))
    {
        return Err(RunError::Unbound((*name).to_owned()));
    }
    // standard input goes to the first endpoint the command line binds stdio
    let stdin = arguments
        .bindings
        .iter()
        .map(|(name, _)| name.as_str())
        .find(|name| asked.get(name) == Some(&&How::Stdio));
    let bindings = externals
        .into_iter()
        .map(|name| {
            let how = asked.get(name.as_str()).copied().unwrap_or(&How::Null);
            let takes_stdin = stdin == Some(name.as_str());
            let binding = Binding::new(name, how, takes_stdin)?;
            // input to an endpoint that drives no serial net would be lost
            if binding.takes_input() && !senders.contains(&binding.name) {
                return Err(RunError::NoSerialNet(binding.name));
            }
            Ok(binding)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let traced = |traced| arguments.trace.contains(&traced);
    let mut tracing = BoardTracing::default();
    tracing.nets = traced(Traced::Nets);
    tracing.bus_cycles = traced(Traced::BusCycles);
    tracing.exceptions = traced(Traced::Exceptions);
    tracing.pins = traced(Traced::Pins);
    tracing.qspi = traced(Traced::Qspi);
    board.set_tracing(tracing);
    for &pc in &arguments.watch_code {
        board.watch_code(pc);
    }
    let tracing = !arguments.trace.is_empty()
        || !arguments.watch_code.is_empty()
        || arguments.position.is_some();
    if tracing {
        eprintln!("{}", header(CHIP));
    }
    let end = match arguments.cycles {
        Some(_) => Time::MAX,
        None => board.now() + picoseconds(arguments.seconds.unwrap_or(DEFAULT_SECONDS)),
    };
    let turn = arguments.turn_us * PICOSECONDS_PER_MICROSECOND;
    let every = arguments
        .position
        .map(picoseconds)
        .filter(|&every| every > 0);
    let mut turns = Turns {
        bindings,
        input_after: board.now() + picoseconds(arguments.input_after),
        next_position: every.map(|every| (board.now() + every, every)),
    };
    // Ctrl-C, under gdb too, ends the run at the next sync point (or
    // gdb's session at its next wait), and `--save` still saves; a
    // second ends the process, for a run that does not stop
    let control = board.control();
    control.set_pacing(arguments.pace)?;
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let control = control.clone();
        let interrupted = Arc::clone(&interrupted);
        let partial = saving.as_ref().map(|saving| saving.temporary.clone());
        ctrlc::set_handler(move || {
            if interrupted.swap(true, Ordering::SeqCst) {
                if let Some(partial) = &partial {
                    let _ = fs::remove_file(partial);
                }
                process::exit(EXIT_INTERRUPTED);
            }
            control.stop();
        })
        .map_err(RunError::Interrupt)?;
    }
    if let Some(address) = &arguments.gdb {
        let ending = gdb::serve(&mut board, &mut turns, address, &interrupted)?;
        let killed = ending == Ending::Killed;
        if killed || interrupted.load(Ordering::SeqCst) {
            let outcome = BoardRun::new(Stop::Requested, board.now());
            eprintln!("{}", Line::new(CHIP, &board.stopped(outcome)));
            if let Some(saving) = saving.filter(|_| !killed) {
                saving.save(&board)?;
            }
            return Ok(outcome);
        }
    }
    let mut pacer = Pacer::new();
    let outcome = loop {
        pacer.wait(board.now(), &control);
        turns.before(&mut board)?;
        let until = (board.now() + turn).min(end);
        // `--cycles`: the last turn ends at the board time of the cycle,
        // as the clock now runs (a SYNCR write moves it), and a turn
        // always moves time on.
        let until = arguments.cycles.map_or(until, |cycles| {
            let at = board.system_clock().time(Cycles(cycles));
            until.min(at.max(board.now() + 1))
        });
        let outcome = board.run_until(until)?;
        turns.after(&mut board)?;
        let done = board.now() >= end
            || arguments
                .cycles
                .is_some_and(|cycles| board.chip().now() >= Cycles(cycles));
        if outcome.stop != Stop::Budget || done {
            break outcome;
        }
    };
    eprintln!("{}", Line::new(CHIP, &board.stopped(outcome)));
    if let Some(saving) = saving {
        saving.save(&board)?;
    }
    Ok(outcome)
}

/// A `--save` snapshot's file while it is written: a temporary file
/// beside its target, renamed over it once whole, so an old snapshot is
/// replaced whole or not at all.  Dropped unsaved, the temporary file
/// goes.
struct Saving {
    /// Where the snapshot goes.
    target: PathBuf,
    /// The temporary file's path.
    temporary: PathBuf,
    /// The temporary file.
    file: File,
}

/// Checks before the run that `--save` can save `board` to `path`: that
/// every peripheral saves, and that a file beside `path` opens for
/// writing.  `path` is not touched yet, so a run that fails leaves an
/// old snapshot there.
fn saveable(board: &Board<Mc68331>, path: &Path) -> Result<Saving, RunError> {
    board.save()?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}.partial", process::id()));
    let file = File::create(&temporary).map_err(|error| RunError::Io {
        path: path.to_owned(),
        error,
    })?;
    Ok(Saving {
        target: path.to_owned(),
        temporary,
        file,
    })
}

impl Saving {
    /// Saves `board`: the snapshot written whole to the temporary file,
    /// then renamed over the target.
    fn save(mut self, board: &Board<Mc68331>) -> Result<(), RunError> {
        let snapshot = board.save()?;
        self.file
            .write_all(&snapshot)
            .and_then(|()| self.file.sync_all())
            .and_then(|()| fs::rename(&self.temporary, &self.target))
            .map_err(|error| RunError::Io {
                path: self.target.clone(),
                error,
            })
    }
}

impl Drop for Saving {
    fn drop(&mut self) {
        // gone already once renamed
        let _ = fs::remove_file(&self.temporary);
    }
}

/// What the runner does between turns of the board's run: input in
/// before one, output, traces and positions out after it.
struct Turns {
    /// The run-time endpoints' bindings, those with input on endpoints
    /// that drive a serial net.
    bindings: Vec<Binding>,
    /// Board time before which no input goes in.
    input_after: Time,
    /// When the next position is taken, and how often.
    next_position: Option<(Time, u64)>,
}

impl Turns {
    /// Before a turn: a byte of each binding's input in, once input
    /// opens.
    fn before(&mut self, board: &mut Board<Mc68331>) -> Result<(), RunError> {
        if board.now() < self.input_after {
            return Ok(());
        }
        for binding in &mut self.bindings {
            for byte in binding.take_input()? {
                let frame = Serial::untimed(Frame::Data(byte.into()));
                board.send(&binding.name, Signal::Serial(frame))?;
            }
        }
        Ok(())
    }

    /// After a turn: what reached each binding out, then the trace and
    /// a position if one is due, on standard error.
    fn after(&mut self, board: &mut Board<Mc68331>) -> Result<(), RunError> {
        for binding in &mut self.bindings {
            // an endpoint that only drives receives nothing; the bindings
            // are the board's own endpoints
            let Ok(received) = board.take_received(&binding.name) else {
                continue;
            };
            binding.put_output(&received).map_err(RunError::Output)?;
        }
        let trace = board.take_trace();
        for record in &trace.records {
            eprintln!("{}", Line::new(CHIP, record));
        }
        for change in &trace.nets {
            eprintln!("{}", Line::new(CHIP, change));
        }
        if let Some((at, every)) = self.next_position
            && board.now() >= at
        {
            eprintln!("{}", Line::new(CHIP, &board.position()));
            self.next_position = Some((at + every, every));
        }
        Ok(())
    }
}
