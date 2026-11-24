//! Save and restore: the core's whole state as bytes.
//!
//! A format version comes first, so the same snapshot and the same
//! inputs give the same run ([PLAN.md, State]).  Hooks are the host's,
//! not state: a restore keeps the CPU's own.  The format is shared by
//! every layer that saves ([`encode`], [`decode`]), each with its own
//! version.  A snapshot a host restores whole (the core's, a bare
//! machine's, a chip's, a board's) starts with a magic and its
//! [`Layer`] before the version, so that one handed to the wrong
//! layer's `restore` says so ([`StateError::Layer`]).
//!
//! [PLAN.md, State]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#interface

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use serde::de::DeserializeOwned;

#[cfg(doc)]
use crate::Cpu;

#[cfg(test)]
mod tests;

/// The format of [`Cpu::save`]'s bytes.  A change to what it saves
/// changes it.
pub const VERSION: u32 = 11;

/// Bytes of the version that starts every snapshot, little-endian.
const VERSION_BYTES: usize = size_of::<u32>();

/// What starts a [`Layer`]'s snapshot, before its tag.
pub(crate) const MAGIC: [u8; 4] = *b"M683";

/// A format a host restores whole, which its snapshot names after a
/// magic: so a snapshot handed to another layer's `restore` is refused
/// as such, not as a version mismatch.  The parts inside one (modules,
/// peripherals) do not name theirs: each is placed by the layer that
/// holds it, and a host's peripheral's format is the host's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Layer {
    /// The core's ([`Cpu::save`]).
    Cpu,
    /// A bare machine's ([`Machine::save`](crate::flat::Machine::save)).
    Machine,
    /// A chip's (`Mc68331::save`).
    Chip,
    /// A board's (`Board::save`).
    Board,
}

impl Layer {
    /// Every layer.
    const ALL: [Self; 4] = [Self::Cpu, Self::Machine, Self::Chip, Self::Board];

    /// The byte that names it in a snapshot.
    const fn tag(self) -> u8 {
        match self {
            Self::Cpu => 1,
            Self::Machine => 2,
            Self::Chip => 3,
            Self::Board => 4,
        }
    }

    /// The layer `tag` names, if any.
    fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|layer| layer.tag() == tag)
    }

    /// Its name.
    const fn name(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Machine => "machine",
            Self::Chip => "chip",
            Self::Board => "board",
        }
    }
}

/// Its name: `CPU`, `machine`, `chip`, `board`.
impl fmt::Display for Layer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The layer named so, as `Display` writes it, in any case.
impl FromStr for Layer {
    type Err = ParseLayerError;

    fn from_str(name: &str) -> Result<Self, ParseLayerError> {
        Self::ALL
            .into_iter()
            .find(|layer| layer.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| ParseLayerError(name.to_owned()))
    }
}

/// Text that names no snapshot layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no snapshot layer {0:?}")]
pub struct ParseLayerError(String);

/// Why a snapshot could not be restored, or a state saved.
///
/// Neither `Clone` nor `PartialEq`, so that a later variant may hold an
/// error that is neither (an `io::Error`): a test matches it
/// (`matches!`).  The reasons are text a host's model may build at run
/// time; [`mismatch`](Self::mismatch), [`invalid`](Self::invalid) and
/// [`unsupported`](Self::unsupported) take a `&'static str` or a
/// `String`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StateError {
    /// Bytes that do not start as a layer's snapshot does: not a
    /// snapshot, or one older than the header.
    #[error("not a snapshot")]
    NotSnapshot,
    /// Another layer's snapshot.
    #[error("a {found} snapshot, not a {expected} snapshot")]
    Layer {
        /// The layer it is.
        found: Layer,
        /// The layer restoring it.
        expected: Layer,
    },
    /// A snapshot of another format.
    #[error("snapshot format {found}, not {expected}")]
    Version {
        /// Its version.
        found: u32,
        /// The version this build reads.
        expected: u32,
    },
    /// Too short to hold a version.
    #[error("snapshot truncated")]
    Truncated,
    /// Its bytes do not decode.
    #[error(transparent)]
    Decode(DecodeError),
    /// A snapshot of other hardware: its memory or modules differ from
    /// the machine's.
    #[error("snapshot of other hardware: {0}")]
    Mismatch(Cow<'static, str>),
    /// A snapshot that decodes but that no run could have saved: a
    /// register bit the part does not implement, an index past what it
    /// indexes, a value its own registers contradict.
    #[error("snapshot inconsistent: {0}")]
    Invalid(Cow<'static, str>),
    /// A part that cannot save its state.
    #[error("{0} cannot save its state")]
    Unsupported(Cow<'static, str>),
    /// A named peripheral in a larger snapshot (a board's peripheral,
    /// a chip's or a bare machine's mapped peripheral) that would not
    /// save or restore.
    #[error("{name}: {error}")]
    Peripheral {
        /// Its name.
        name: String,
        /// Why.
        error: Box<StateError>,
    },
}

/// Why a snapshot's bytes do not decode.  Opaque: the encoding is the
/// snapshot's, not the API's, so its decoder's errors are not exposed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("snapshot does not decode: {0}")]
pub struct DecodeError(postcard::Error);

impl StateError {
    /// [`Self::Mismatch`], for `what`.
    #[must_use]
    pub fn mismatch(what: impl Into<Cow<'static, str>>) -> Self {
        Self::Mismatch(what.into())
    }

    /// [`Self::Invalid`], for `what`.
    #[must_use]
    pub fn invalid(what: impl Into<Cow<'static, str>>) -> Self {
        Self::Invalid(what.into())
    }

    /// [`Self::Unsupported`], of `what`.
    #[must_use]
    pub fn unsupported(what: impl Into<Cow<'static, str>>) -> Self {
        Self::Unsupported(what.into())
    }

    /// This error, of the peripheral named `name`.
    #[must_use]
    pub fn of_peripheral(self, name: &str) -> Self {
        Self::Peripheral {
            name: name.to_owned(),
            error: Box::new(self),
        }
    }
}

/// The format of a [`Stateless`] peripheral's snapshot, which holds nothing.
pub const STATELESS_VERSION: u32 = 1;

/// Whether and how a peripheral saves its state: every peripheral attached to
/// a chip (`Builder::peripheral` and `unwired_peripheral`) or a bare machine
/// ([`Machine::add_peripheral`](crate::flat::Machine::add_peripheral)) says,
/// as the board's peripherals do, so that their snapshots hold every
/// peripheral; and a peripheral model a board adapts (`mc683xx-board`'s
/// `MappedAdapter` and `SpiAdapter`).
///
/// Both methods or neither: a model that saves gives both, its bytes in
/// its own format, which it versions itself ([`encode`] and [`decode`]
/// do).  One with no state is [`Stateless`].  One that cannot save is
/// [`unsavable!`](crate::unsavable): both its methods refuse, and so
/// does the snapshot that holds it, naming it.  Neither method has a
/// default, so that no model saves what its own `restore` refuses.
pub trait Save {
    /// Its state.
    ///
    /// # Errors
    ///
    /// [`StateError::Unsupported`] if the model cannot save.
    fn save(&self) -> Result<Vec<u8>, StateError>;

    /// Restores what `save` gave.  It must take what its own `save`
    /// gives, and leave the model unchanged when it refuses: a snapshot
    /// that holds it checks its bytes by restoring them, and puts its
    /// own back if a later part refuses theirs ([`restore_each`]).
    ///
    /// # Errors
    ///
    /// A [`StateError`] if `snapshot` is not one `save` gave, or does
    /// not fit this model; the model is then unchanged.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError>;
}

/// Implements [`Save`] for a model that cannot save, `$role` naming it
/// (`"a test stub"`): its `save` and `restore` both refuse with
/// [`StateError::Unsupported`]`($role)`, and so does the snapshot that
/// holds it, naming its place.  A macro, not a marker trait beside
/// [`Stateless`]: two blanket impls of [`Save`] would conflict.
///
/// ```
/// /// A model that cannot save.
/// struct Stub;
///
/// mc683xx_cpu32::unsavable!(Stub, "a stub");
/// ```
#[macro_export]
macro_rules! unsavable {
    ($model:ty, $role:expr $(,)?) => {
        impl $crate::state::Save for $model {
            fn save(
                &self,
            ) -> ::core::result::Result<::std::vec::Vec<u8>, $crate::state::StateError> {
                ::core::result::Result::Err($crate::state::StateError::unsupported($role))
            }

            fn restore(
                &mut self,
                _: &[u8],
            ) -> ::core::result::Result<(), $crate::state::StateError> {
                ::core::result::Result::Err($crate::state::StateError::unsupported($role))
            }
        }
    };
}

/// A peripheral with no state of its own to save, a fixed responder say:
/// `impl Stateless for Model {}` gives it a [`Save`] whose snapshot holds
/// nothing ([`STATELESS_VERSION`]).
pub trait Stateless {}

impl<T: Stateless> Save for T {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(STATELESS_VERSION, &()))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        decode::<()>(STATELESS_VERSION, snapshot)
    }
}

/// Restores parts in order, each from its snapshot in `snapshots`:
/// `restore(index, bytes)` restores part `index`.  If one refuses, the
/// parts before it get their `backups` back (the refusing part is
/// unchanged by [`Save::restore`]'s contract, and the parts after it
/// are untouched), and its error is returned.  A layer that holds
/// parts (a chip's peripherals, a board's peripherals) restores them so,
/// a snapshot it holds checked only by restoring it.
///
/// # Errors
///
/// The first error `restore` gives.
///
/// Not part of the semver API: plumbing the chip and the board restore
/// their parts with.
#[doc(hidden)]
pub fn restore_each<B: AsRef<[u8]>>(
    snapshots: &[impl AsRef<[u8]>],
    backups: &[B],
    mut restore: impl FnMut(usize, &[u8]) -> Result<(), StateError>,
) -> Result<(), StateError> {
    for (index, snapshot) in snapshots.iter().enumerate() {
        if let Err(error) = restore(index, snapshot.as_ref()) {
            put_back(&backups[..index], restore);
            return Err(error);
        }
    }
    Ok(())
}

/// Restores each part `restore(index, bytes)` names from its own
/// snapshot in `backups`, in order: the rollback of a restore a later
/// part refused.  A part that refuses its own snapshot breaks
/// [`Save::restore`]'s contract; it is left as it is, the rest still
/// put back, and the error the rollback follows is the one returned.
fn put_back(
    backups: &[impl AsRef<[u8]>],
    mut restore: impl FnMut(usize, &[u8]) -> Result<(), StateError>,
) {
    for (index, backup) in backups.iter().enumerate() {
        // A part's own snapshot back, which its contract says it takes.
        let _ = restore(index, backup.as_ref());
    }
}

/// `value` as a snapshot of format `version`.
///
/// # Panics
///
/// If `value`'s serializer fails, which none of the crates' does:
/// writing to a vector cannot fail.
#[must_use]
pub fn encode<T: Serialize>(version: u32, value: &T) -> Vec<u8> {
    encode_after(Vec::new(), version, value)
}

/// `bytes`, then [`encode`]'s, written into them.
fn encode_after<T: Serialize>(mut bytes: Vec<u8>, version: u32, value: &T) -> Vec<u8> {
    bytes.extend(version.to_le_bytes());
    // Writing to a vector cannot fail; nor can our types' serializers.
    postcard::to_extend(value, bytes).expect("serializes")
}

/// `value` as `layer`'s snapshot of format `version`: [`encode`]'s,
/// after the magic and `layer`'s tag.
///
/// Not part of the semver API: what the layers a host restores whole
/// save with.
///
/// # Panics
///
/// As [`encode`].
#[doc(hidden)]
#[must_use]
pub fn encode_layer<T: Serialize>(layer: Layer, version: u32, value: &T) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.push(layer.tag());
    encode_after(bytes, version, value)
}

/// The value `layer`'s snapshot of format `version` holds.
///
/// Not part of the semver API: what the layers a host restores whole
/// restore with.
///
/// # Errors
///
/// [`StateError::Truncated`] if it is too short to hold the magic and
/// a tag, [`StateError::NotSnapshot`] if it does not start with the
/// magic, [`StateError::Layer`] if it is another layer's,
/// [`StateError::Invalid`] if it names no layer, and [`decode`]'s.
#[doc(hidden)]
pub fn decode_layer<T: DeserializeOwned>(
    layer: Layer,
    version: u32,
    bytes: &[u8],
) -> Result<T, StateError> {
    let (magic, rest) = bytes
        .split_first_chunk::<{ MAGIC.len() }>()
        .ok_or(StateError::Truncated)?;
    if *magic != MAGIC {
        return Err(StateError::NotSnapshot);
    }
    let (&tag, body) = rest.split_first().ok_or(StateError::Truncated)?;
    match Layer::from_tag(tag) {
        Some(found) if found == layer => decode(version, body),
        Some(found) => Err(StateError::Layer {
            found,
            expected: layer,
        }),
        None => Err(StateError::invalid(format!("snapshot layer {tag}"))),
    }
}

/// The value a snapshot of format `version` holds.
///
/// # Errors
///
/// [`StateError::Truncated`] if it is too short to hold a version,
/// [`StateError::Version`] if it is of another format,
/// [`StateError::Decode`] if its bytes do not decode, and
/// [`StateError::Invalid`] if bytes are left past the value.
pub fn decode<T: DeserializeOwned>(version: u32, bytes: &[u8]) -> Result<T, StateError> {
    let (head, body) = bytes
        .split_first_chunk::<VERSION_BYTES>()
        .ok_or(StateError::Truncated)?;
    let found = u32::from_le_bytes(*head);
    if found != version {
        return Err(StateError::Version {
            found,
            expected: version,
        });
    }
    let (value, rest) =
        postcard::take_from_bytes(body).map_err(|error| StateError::Decode(DecodeError(error)))?;
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(StateError::invalid("bytes past the end of the snapshot"))
    }
}

/// Serde for a byte array longer than serde's own arrays (32), as a
/// state field's `#[serde(with = "mc683xx_cpu32::state::byte_array")]`.
///
/// Not part of the semver API: serde plumbing for the workspace's snapshots.
#[doc(hidden)]
pub mod byte_array {
    use serde::Deserialize;
    use serde::Deserializer;
    use serde::Serialize;
    use serde::Serializer;
    use serde::de::Error;

    /// The bytes, as a sequence.
    ///
    /// # Errors
    ///
    /// The serializer's.
    pub fn serialize<S: Serializer, const N: usize>(
        bytes: &[u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        bytes.as_slice().serialize(serializer)
    }

    /// The bytes, which must be `N`.
    ///
    /// # Errors
    ///
    /// The deserializer's, and an invalid length if there are not `N`
    /// bytes.
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<[u8; N], D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let length = bytes.len();
        bytes
            .try_into()
            .map_err(|_| D::Error::invalid_length(length, &"the array's length"))
    }
}
