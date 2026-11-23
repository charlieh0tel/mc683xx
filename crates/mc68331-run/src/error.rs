//! What can stop the runner before or around a run.  The runner is the
//! workspace's one binary, and keeps typed errors as the libraries do.

use std::io;
use std::path::PathBuf;

use mc683xx_cpu32::cpu::state::StateError;

/// Why the runner could not run, or finish, a board.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RunError {
    /// The board file, or its loading.
    #[error(transparent)]
    Board(#[from] mc683xx_board::description::LoadError),
    /// A `--set` not of the form `PERIPHERAL.KEY=VALUE`.
    #[error(transparent)]
    Setting(#[from] mc683xx_board::description::ParseSettingError),
    /// A run that could not go on: a peripheral stalled the board.
    #[error(transparent)]
    Run(#[from] mc683xx_board::board::RunError),
    /// Input a binding's endpoint could not drive.
    #[error(transparent)]
    Send(#[from] mc683xx_board::board::SendError),
    /// A file the runner reads or writes.
    #[error("{}: {error}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        error: io::Error,
    },
    /// A snapshot that would not save or restore.
    #[error("snapshot: {error}")]
    State {
        /// Why.
        error: StateError,
    },
    /// A binding of an endpoint the board does not name.
    #[error("--bind {0}: the board names no external.{0}")]
    Unbound(String),
    /// Input bound to an endpoint that drives no serial net, where it
    /// would be lost.
    #[error(
        "--bind {0}: external.{0} drives no serial net, so input bound to it would be lost; \
         bind it to stdout, file:PATH or null"
    )]
    NoSerialNet(String),
    /// A pseudo-terminal that could not be opened.
    #[error("pty: {error}")]
    Pty {
        /// Why.
        error: io::Error,
    },
    /// A `tty` binding while another binding takes standard input.
    #[error("--bind {0}=tty: standard input is another binding's; tty needs it")]
    StdinTaken(String),
    /// A `tty` binding whose standard input is not a terminal.
    #[error(
        "--bind {0}=tty: standard input is not a terminal; bind stdio to read a file or a pipe"
    )]
    NotATerminal(String),
    /// The terminal on standard input, which would not be made raw.
    #[error("tty: {error}")]
    Terminal {
        /// Why.
        error: io::Error,
    },
    /// Output that could not be written.
    #[error("output: {error}")]
    Output {
        /// Why.
        error: io::Error,
    },
    /// gdb's address: no listening there, or no connection taken.
    #[error("gdb: {error}")]
    Gdb {
        /// Why.
        error: io::Error,
    },
    /// Ctrl-C's handler, which could not be set.
    #[error("Ctrl-C: {error}")]
    Interrupt {
        /// Why.
        error: ctrlc::Error,
    },
}

/// Not `#[from]`, which would make the error the source as well as the
/// message's: printed twice by a reporter that walks the chain.
impl From<StateError> for RunError {
    fn from(error: StateError) -> Self {
        Self::State { error }
    }
}
