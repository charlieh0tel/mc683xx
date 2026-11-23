//! What a JavaScript host can get wrong, before it becomes a JavaScript
//! exception.

use js_sys::RangeError;
use js_sys::TypeError;
use mc683xx_board::board::ReceiveError;
use mc683xx_board::board::RunError;
use mc683xx_board::board::SendError;
use mc683xx_board::description::LoadError;
use mc683xx_board::pacing::ParsePacingError;
use mc683xx_cpu32::cpu::state::StateError;
use wasm_bindgen::JsValue;

/// A call the bindings or the board refuse; it is thrown as a JavaScript
/// error with its message: a `TypeError` for an argument of the wrong
/// type, a `RangeError` for one out of its range, and otherwise an
/// `Error` named for its kind (`LoadError`, `RunError`).
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
    /// Picoseconds that are not a `bigint`: what they are (`a number`).
    #[error("picoseconds are not a bigint but {0}")]
    PicosecondsType(String),
    /// Picoseconds that are a `bigint` out of a `u64`'s range.
    #[error("{0}n picoseconds is out of range: give 0n to 2n ** 64n - 1n")]
    Picoseconds(String),
    /// Bytes that are not a `Uint8Array`.
    #[error("{argument} is not a Uint8Array but {kind}")]
    Bytes {
        /// What the bytes are for (`data to send`).
        argument: &'static str,
        /// What they are (`an Array`, `null`).
        kind: String,
    },
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

/// The JavaScript error an [`Error`] is thrown as, which a page may
/// branch on, as it may not on the message's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Thrown {
    /// A `TypeError`: an argument of the wrong type.
    Type,
    /// A `RangeError`: an argument of the right type out of its range.
    Range,
    /// An `Error` whose `name` is this.
    Named(&'static str),
}

impl Error {
    /// The JavaScript error `self` is thrown as.
    pub(crate) const fn thrown(&self) -> Thrown {
        match self {
            Self::Image(_)
            | Self::ImageName(_)
            | Self::Images
            | Self::PicosecondsType(_)
            | Self::Bytes { .. } => Thrown::Type,
            Self::Picoseconds(_) | Self::Wall(_) => Thrown::Range,
            Self::Load(_) => Thrown::Named("LoadError"),
            Self::ParsePacing(_) => Thrown::Named("PacingError"),
            Self::Run(_) => Thrown::Named("RunError"),
            Self::Send(_) => Thrown::Named("SendError"),
            Self::Receive(_) => Thrown::Named("ReceiveError"),
            Self::State(_) => Thrown::Named("StateError"),
            Self::Memory(_) => Thrown::Named("MemoryError"),
        }
    }
}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        let thrown: js_sys::Error = match error.thrown() {
            Thrown::Type => TypeError::new(&message).into(),
            Thrown::Range => RangeError::new(&message).into(),
            Thrown::Named(name) => {
                let named = js_sys::Error::new(&message);
                named.set_name(name);
                named
            }
        };
        thrown.into()
    }
}
