//! Run-time endpoints bound to the host ([PLAN.md, Boards](https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards)): the board
//! file names them, `--bind name=how` binds them.  Serial frames go
//! out as bytes, their low eight bits, and bytes come in as frames; any
//! other value goes to standard error as a line.

#[cfg(not(unix))]
use std::convert::Infallible;
use std::fs::File;
use std::io;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;

use mc683xx_board::control::BoardControl;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
#[cfg(unix)]
use nix::fcntl::FcntlArg;
#[cfg(unix)]
use nix::fcntl::OFlag;
#[cfg(unix)]
use nix::fcntl::fcntl;
#[cfg(unix)]
use nix::pty::openpty;
#[cfg(unix)]
use nix::sys::termios::OutputFlags;
#[cfg(unix)]
use nix::sys::termios::SetArg;
#[cfg(unix)]
use nix::sys::termios::Termios;
#[cfg(unix)]
use nix::sys::termios::cfmakeraw;
#[cfg(unix)]
use nix::sys::termios::tcgetattr;
#[cfg(unix)]
use nix::sys::termios::tcsetattr;
#[cfg(unix)]
use nix::unistd::ttyname;

use crate::CHIP;
use crate::error::RunError;

/// The key that ends a run from a `tty` binding: Ctrl-], as telnet's.
pub(crate) const ESCAPE: u8 = 0x1d;

/// How an endpoint is bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum How {
    /// Standard output, and standard input.
    Stdio,
    /// Standard output only: standard input stays free for another
    /// binding.
    Stdout,
    /// A new pseudo-terminal, its name on standard error.
    Pty,
    /// The terminal on standard input, raw, and standard output:
    /// keystrokes go in as typed, [`ESCAPE`] ends the run.
    Tty,
    /// Nothing: what comes out is dropped, nothing goes in.
    Null,
    /// A file what comes out is written to.
    File(PathBuf),
}

impl FromStr for How {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "stdio" => Ok(Self::Stdio),
            "stdout" => Ok(Self::Stdout),
            "pty" => Ok(Self::Pty),
            "tty" => Ok(Self::Tty),
            "null" => Ok(Self::Null),
            _ => text
                .strip_prefix("file:")
                .map(|path| Self::File(PathBuf::from(path)))
                .ok_or_else(|| {
                    format!("{text:?}: give stdio, stdout, tty, pty, null or file:PATH")
                }),
        }
    }
}

/// Where an endpoint's input comes from.
enum Input {
    /// None.
    Nothing,
    /// Standard input, a file or a pipe, not read yet: it is read whole
    /// when input first goes in, so `--gdb` says where it waits first.
    Piped,
    /// Bytes read whole, from a file or a pipe: one is sent each turn,
    /// so a run is the same each time.
    Fed(Vec<u8>, usize),
    /// Bytes from a terminal as they are typed: a run with them is not
    /// reproducible.
    Live(Receiver<u8>),
    /// Bytes from the terminal on standard input, raw, as they are typed;
    /// not reproducible.  The terminal is put back as it was when this
    /// goes.
    Raw(Receiver<u8>, RawTerminal),
    /// A pseudo-terminal's master, read without blocking: not
    /// reproducible either.  The slave stays open with it, so the master
    /// reads no hangup before a client opens the slave.
    Pty {
        /// The master.
        master: File,
        /// The slave.
        _slave: PtySlave,
    },
}

/// A pseudo-terminal's slave.
#[cfg(unix)]
type PtySlave = OwnedFd;
/// None: pseudo-terminals are Unix's, and `pty` refuses one elsewhere.
#[cfg(not(unix))]
type PtySlave = Infallible;

/// Where an endpoint's output goes.
enum Output {
    /// Dropped.
    Nothing,
    /// Standard output.
    Stdout,
    /// A file.
    File(File),
    /// A pseudo-terminal's master, which does not block: what its
    /// buffer cannot take, with nobody reading the slave, is dropped,
    /// as a UART's frames are with nothing listening on the line.
    Pty(File),
}

/// How a `tty` binding's [`ESCAPE`] ends the run: as Ctrl-C does
/// (`interruption` in `main.rs`).
#[derive(Clone)]
pub(crate) struct Escape {
    /// The board's control, to stop its run.
    pub(crate) control: BoardControl,
    /// The flag Ctrl-C sets.
    pub(crate) interrupted: Arc<AtomicBool>,
}

impl Escape {
    /// Ends the run.
    fn end_run(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
        self.control.stop();
    }
}

/// An endpoint bound.
pub(crate) struct Binding {
    /// The endpoint's name.
    pub(crate) name: String,
    /// Its input.
    input: Input,
    /// Its output.
    output: Output,
}

impl Binding {
    /// `name` bound `how`; `stdin` says whether a stdio or tty binding
    /// takes standard input, which a tty binding must; `escape` is how a
    /// tty binding ends the run.
    pub(crate) fn new(
        name: String,
        how: &How,
        stdin: bool,
        escape: &Escape,
    ) -> Result<Self, RunError> {
        let (input, output) = match how {
            How::Null => (Input::Nothing, Output::Nothing),
            How::Stdout => (Input::Nothing, Output::Stdout),
            How::File(path) => (
                Input::Nothing,
                Output::File(File::create(path).map_err(|error| RunError::Io {
                    path: path.clone(),
                    error,
                })?),
            ),
            How::Stdio => {
                let input = if stdin { stdin_input() } else { Input::Nothing };
                (input, Output::Stdout)
            }
            How::Tty if !stdin => return Err(RunError::StdinTaken(name)),
            How::Tty if !io::stdin().is_terminal() => return Err(RunError::NotATerminal(name)),
            How::Tty => {
                let raw = RawTerminal::new()?;
                eprintln!("{CHIP}: bound name={name} tty; Ctrl-] ends the run");
                (Input::Raw(raw_input(escape.clone()), raw), Output::Stdout)
            }
            How::Pty => {
                let (master, slave) = pty(&name)?;
                let reader = master.try_clone().map_err(RunError::Pty)?;
                (
                    Input::Pty {
                        master: reader,
                        _slave: slave,
                    },
                    Output::Pty(master),
                )
            }
        };
        Ok(Self {
            name,
            input,
            output,
        })
    }

    /// Whether it has input to send: standard input or a pty's.
    pub(crate) const fn takes_input(&self) -> bool {
        !matches!(self.input, Input::Nothing)
    }

    /// The bytes to send now: one fed byte, or all a terminal has.
    /// Piped input is read whole the first time, blocking until its
    /// end.
    pub(crate) fn take_input(&mut self) -> Result<Vec<u8>, RunError> {
        if matches!(self.input, Input::Piped) {
            self.input = Input::Fed(read_stdin()?, 0);
        }
        Ok(match &mut self.input {
            Input::Nothing | Input::Piped => Vec::new(),
            Input::Fed(bytes, next) => {
                let byte = bytes.get(*next).copied();
                *next += usize::from(byte.is_some());
                byte.into_iter().collect()
            }
            Input::Live(receiver) | Input::Raw(receiver, _) => receiver.try_iter().collect(),
            Input::Pty { master, .. } => {
                let mut buffer = [0; 256];
                match master.read(&mut buffer) {
                    Ok(count) => buffer[..count].to_vec(),
                    Err(_) => Vec::new(),
                }
            }
        })
    }

    /// Writes what reached the endpoint.
    pub(crate) fn put_output(&mut self, received: &[(Time, Signal)]) -> io::Result<()> {
        let mut bytes = Vec::new();
        for (at, signal) in received {
            match signal {
                Signal::Serial(serial) => bytes.extend(serial.frame.data().map(|data| data as u8)),
                other => eprintln!("{CHIP}: external name={} time={} {other}", self.name, at.0),
            }
        }
        if bytes.is_empty() {
            return Ok(());
        }
        match &mut self.output {
            Output::Nothing => Ok(()),
            Output::Stdout => {
                let mut stdout = io::stdout().lock();
                stdout.write_all(&bytes)?;
                stdout.flush()
            }
            Output::File(file) => file.write_all(&bytes),
            Output::Pty(master) => write_or_drop(master, &bytes),
        }
    }
}

/// Writes what `master` takes of `bytes` without blocking and drops the
/// rest: the pseudo-terminal's buffer is the line's only buffer.
fn write_or_drop(master: &mut File, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match master.write(bytes) {
            Ok(0) => return Ok(()),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Standard input: a file or a pipe to read whole later, or a reader
/// thread for the terminal.
fn stdin_input() -> Input {
    if io::stdin().is_terminal() {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for byte in io::stdin().lock().bytes() {
                let Ok(byte) = byte else { break };
                if sender.send(byte).is_err() {
                    break;
                }
            }
        });
        return Input::Live(receiver);
    }
    Input::Piped
}

/// The raw terminal's keystrokes, from a reader thread that ends the run
/// at [`ESCAPE`], which goes no further.
fn raw_input(escape: Escape) -> Receiver<u8> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for byte in io::stdin().lock().bytes() {
            let Ok(byte) = byte else { break };
            if byte == ESCAPE {
                escape.end_run();
                break;
            }
            if sender.send(byte).is_err() {
                break;
            }
        }
    });
    receiver
}

/// The terminal on standard input as it was before [`RawTerminal`] made
/// it raw, for [`restore_terminal`], which a forced exit calls too.
#[cfg(unix)]
static SAVED: Mutex<Option<Termios>> = Mutex::new(None);

/// The terminal on standard input made raw, put back as it was when
/// this goes.  Input is raw (no echo, no line editing, no signals from
/// keys: Ctrl-C and Ctrl-Z go to the endpoint); output keeps its
/// newline translation, so standard error's lines stay lines.
pub(crate) struct RawTerminal(());

impl RawTerminal {
    #[cfg(unix)]
    fn new() -> Result<Self, RunError> {
        let stdin = io::stdin();
        let saved = tcgetattr(&stdin).map_err(|error| RunError::Terminal(error.into()))?;
        let mut raw = saved.clone();
        cfmakeraw(&mut raw);
        raw.output_flags |= OutputFlags::OPOST | OutputFlags::ONLCR;
        *SAVED.lock().unwrap_or_else(PoisonError::into_inner) = Some(saved);
        tcsetattr(&stdin, SetArg::TCSANOW, &raw).map_err(|error| {
            restore_terminal();
            RunError::Terminal(error.into())
        })?;
        Ok(Self(()))
    }

    /// None: raw terminals here are Unix's.
    #[cfg(not(unix))]
    fn new() -> Result<Self, RunError> {
        Err(RunError::Terminal(io::Error::new(
            io::ErrorKind::Unsupported,
            "a raw terminal needs a Unix host",
        )))
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Puts the terminal on standard input back as it was before it was made
/// raw, if it was.
#[cfg(unix)]
pub(crate) fn restore_terminal() {
    let saved = SAVED.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(saved) = saved {
        let _ = tcsetattr(io::stdin(), SetArg::TCSANOW, &saved);
    }
}

/// Nothing: no terminal is made raw off Unix.
#[cfg(not(unix))]
pub(crate) const fn restore_terminal() {}

/// Standard input, a file or a pipe, read to its end.
fn read_stdin() -> Result<Vec<u8>, RunError> {
    let mut bytes = Vec::new();
    io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|error| RunError::Io {
            path: PathBuf::from("-"),
            error,
        })?;
    Ok(bytes)
}

/// A raw pseudo-terminal for `name`, its slave's path on standard
/// error: its master, which does not block, and its slave.
#[cfg(unix)]
fn pty(name: &str) -> Result<(File, PtySlave), RunError> {
    let pair = openpty(None, None).map_err(|error| RunError::Pty(error.into()))?;
    let mut termios = tcgetattr(&pair.slave).map_err(|error| RunError::Pty(error.into()))?;
    cfmakeraw(&mut termios);
    tcsetattr(&pair.slave, SetArg::TCSANOW, &termios)
        .map_err(|error| RunError::Pty(error.into()))?;
    fcntl(&pair.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
        .map_err(|error| RunError::Pty(error.into()))?;
    let path = ttyname(&pair.slave).map_err(|error| RunError::Pty(error.into()))?;
    eprintln!("{CHIP}: bound name={name} pty={}", path.display());
    Ok((File::from(pair.master), pair.slave))
}

/// No pseudo-terminal: they are Unix's.
#[cfg(not(unix))]
fn pty(_: &str) -> Result<(File, PtySlave), RunError> {
    Err(RunError::Pty(io::Error::new(
        io::ErrorKind::Unsupported,
        "pseudo-terminals need a Unix host",
    )))
}
