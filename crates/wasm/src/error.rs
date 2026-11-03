//! What a JavaScript host can get wrong, before it becomes a JavaScript
//! exception.

use mc683xx_board::error::LoadError;
use mc683xx_board::error::ReceiveError;
use mc683xx_board::error::RunError;
use mc683xx_board::error::SendError;
use mc683xx_board::pacing::PacingError;
use mc683xx_board::pacing::ParsePacingError;
use mc683xx_cpu32::cpu::state::StateError;
use wasm_bindgen::JsError;
use wasm_bindgen::JsValue;

/// A call the bindings or the board refuse; it is thrown as a JavaScript
/// `Error` with its message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The board file, or a board it describes, that does not load.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// An image that is not a `Uint8Array`.
    #[error("image {0:?} is not a Uint8Array")]
    Image(String),
    /// An image's name, a `Map`'s key, that is not a string: what it
    /// is (`a number`).
    #[error("an image's name is not a string but {0}")]
    ImageName(String),
    /// Images that are neither a `Map` nor an object.
    #[error("images are neither a Map nor an object")]
    Images,
    /// Text that names no pacing.
    #[error(transparent)]
    ParsePacing(#[from] ParsePacingError),
    /// A pacing the control refuses.
    #[error(transparent)]
    Pacing(#[from] PacingError),
    /// Picoseconds that are not a `bigint`: what they are (`a number`).
    #[error("picoseconds are not a bigint but {0}")]
    PicosecondsType(String),
    /// Picoseconds that are a `bigint` out of a `u64`'s range.
    #[error("{0}n picoseconds is out of range: give 0n to 2n ** 64n - 1n")]
    Picoseconds(String),
    /// Data to send that is not a `Uint8Array`: what it is (`an
    /// Array`, `null`).
    #[error("data to send is not a Uint8Array but {0}")]
    Data(String),
    /// A wall clock reading that is no time.
    #[error("{0} ms is no clock reading: give a finite time, 0 or more")]
    Wall(f64),
    /// A run that failed.
    #[error(transparent)]
    Run(#[from] RunError),
    /// A send no net takes.
    #[error(transparent)]
    Send(#[from] SendError),
    /// A take from an endpoint no net drives.
    #[error(transparent)]
    Receive(#[from] ReceiveError),
    /// A board that cannot save, or a snapshot it refuses.
    #[error(transparent)]
    State(#[from] StateError),
    /// A memory the board lacks.
    #[error("no memory {0:?}")]
    Memory(String),
}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        JsError::new(&error.to_string()).into()
    }
}
