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
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;

use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_modules::qsm::sci::Frame;
#[cfg(unix)]
use nix::fcntl::FcntlArg;
#[cfg(unix)]
use nix::fcntl::OFlag;
#[cfg(unix)]
use nix::fcntl::fcntl;
#[cfg(unix)]
use nix::pty::openpty;
#[cfg(unix)]
use nix::sys::termios::SetArg;
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
            "null" => Ok(Self::Null),
            _ => text
                .strip_prefix("file:")
                .map(|path| Self::File(PathBuf::from(path)))
                .ok_or_else(|| format!("{text:?}: give stdio, stdout, pty, null or file:PATH")),
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
    /// `name` bound `how`; `stdin` says whether a stdio binding takes
    /// standard input.
    pub(crate) fn new(name: String, how: &How, stdin: bool) -> Result<Self, RunError> {
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
            Input::Live(receiver) => receiver.try_iter().collect(),
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
                Signal::Serial(serial) => {
                    if let Frame::Data(data) = serial.frame {
                        bytes.push(data as u8);
                    }
                }
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
