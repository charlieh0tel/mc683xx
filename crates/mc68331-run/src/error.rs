//! What can stop the runner before or around a run.  The runner is the
//! workspace's one binary, and keeps typed errors as the libraries do.

use std::io;
use std::path::PathBuf;

use mc683xx_board::registry::RegistryError;
use mc683xx_cpu32::cpu::state::StateError;

/// Why the runner could not run, or finish, a board.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RunError {
    /// The board file, or its loading.
    #[error(transparent)]
    Board(#[from] mc683xx_board::error::LoadError),
    /// The stock peripherals' registration (`mc683xx-peripherals`).
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// A run that could not go on: a peripheral stalled the board.
    #[error(transparent)]
    Run(#[from] mc683xx_board::error::RunError),
    /// Input a binding's endpoint could not drive.
    #[error(transparent)]
    Send(#[from] mc683xx_board::error::SendError),
    /// A file the runner reads or writes.
    #[error("{}: {error}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        error: io::Error,
    },
    /// A snapshot that would not save or restore.
    #[error("snapshot: {0}")]
    State(StateError),
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
    #[error("pty: {0}")]
    Pty(io::Error),
    /// Output that could not be written.
    #[error("output: {0}")]
    Output(io::Error),
    /// gdb's address: no listening there, or no connection taken.
    #[error("gdb: {0}")]
    Gdb(io::Error),
    /// `--pace`'s factor, refused.
    #[error(transparent)]
    Pacing(#[from] mc683xx_board::pacing::PacingError),
    /// Ctrl-C's handler, which could not be set.
    #[error("Ctrl-C: {0}")]
    Interrupt(ctrlc::Error),
}

/// Not `#[from]`, which would make the error the source as well as the
/// message's: printed twice by a reporter that walks the chain.
impl From<StateError> for RunError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}
