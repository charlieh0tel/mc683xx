//! Save and restore: the core's whole state as bytes.
//!
//! A format version comes first, so the same snapshot and the same
//! inputs give the same run ([PLAN.md, State]).  Hooks are the host's,
//! not state: a restore keeps the CPU's own.  The format is shared by
//! every layer that saves ([`encode`], [`decode`]), each with its own
//! version.
//!
//! [PLAN.md, State]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#interface

use std::borrow::Cow;

use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::Cpu;
use super::Halt;
use super::hooks::Resume;
use crate::address::Address;
use crate::cycles::Cycles;
use crate::effective_address::Altered;
use crate::exception::Exception;
use crate::exception::Fault;
use crate::exception::Movem;
use crate::exception::MovemResume;
use crate::interrupt::InterruptLevel;
use crate::memory::MemoryState;
use crate::registers::Registers;
use crate::registers::status;

#[cfg(test)]
mod tests;

/// The format of [`Cpu::save`]'s bytes.  A change to what it saves
/// changes it.
pub const VERSION: u32 = 6;

/// Bytes of the version that starts every snapshot, little-endian.
const VERSION_BYTES: usize = size_of::<u32>();

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
        impl $crate::cpu::state::Save for $model {
            fn save(
                &self,
            ) -> ::core::result::Result<::std::vec::Vec<u8>, $crate::cpu::state::StateError> {
                ::core::result::Result::Err($crate::cpu::state::StateError::unsupported($role))
            }

            fn restore(
                &mut self,
                _: &[u8],
            ) -> ::core::result::Result<(), $crate::cpu::state::StateError> {
                ::core::result::Result::Err($crate::cpu::state::StateError::unsupported($role))
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
pub fn put_back(
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
    let mut bytes = version.to_le_bytes().to_vec();
    // Writing to a vector cannot fail; nor can our types' serializers.
    bytes.extend(postcard::to_allocvec(value).expect("serializes"));
    bytes
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
/// state field's `#[serde(with = "mc683xx_cpu32::cpu::state::byte_array")]`.
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

/// What a snapshot of the core holds: every field of [`Cpu`] but the
/// hooks and what a restore sets (a block starts at the restored PC),
/// and its memory's regions and allocated slabs of page tables and
/// permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CpuState {
    /// The registers.
    registers: Registers,
    /// [`Cpu::instruction_pc`].
    instruction_pc: Address,
    /// [`Cpu::previous_pc`].
    previous_pc: Address,
    /// [`Cpu::last_tail`].
    last_tail: u8,
    /// [`Cpu::stall`].
    stall: Cycles,
    /// [`Cpu::extra`].
    extra: Cycles,
    /// [`Cpu::refund`].
    refund: Cycles,
    /// [`Cpu::stopped`].
    stopped: bool,
    /// [`Cpu::halt`].
    halt: Option<Halt>,
    /// [`Cpu::fault_window`].
    fault_window: bool,
    /// [`Cpu::released`].
    released: Option<Fault>,
    /// [`Cpu::altered`].
    altered: Altered,
    /// [`Cpu::movem`].
    movem: Option<Movem>,
    /// [`Cpu::movem_resume`].
    movem_resume: Option<MovemResume>,
    /// [`Cpu::untraced`].
    untraced: bool,
    /// [`Cpu::sampled_level`].
    sampled_level: InterruptLevel,
    /// [`Cpu::sampled_mask`].
    sampled_mask: u8,
    /// [`Cpu::taken`].
    taken: Option<Exception>,
    /// [`Cpu::instructions`].
    instructions: u64,
    /// [`Cpu::resume`].
    resume: Option<(Address, Resume)>,
    /// [`Cpu::loopable`].
    loopable: Option<Address>,
    /// [`Cpu::loop_mode`].
    loop_mode: Option<Address>,
    /// The memory's address lines, as a mask.
    address_mask: u32,
    /// The memory's regions, page tables and permissions.
    memory: MemoryState,
}

impl CpuState {
    /// Whether it holds what a run could have saved: SR's unimplemented
    /// bits clear (CPU32RM 2.3.1.3), the altered registers address
    /// registers, the sampled mask a level.  Function codes and
    /// interrupt levels are checked as they decode.
    fn validate(&self) -> Result<(), StateError> {
        if self.registers.sr() & !status::IMPLEMENTED != 0 {
            return Err(StateError::invalid("status register"));
        }
        if !self.altered.is_valid() {
            return Err(StateError::invalid("altered address registers"));
        }
        if InterruptLevel::new(self.sampled_mask).is_none() {
            return Err(StateError::invalid("sampled interrupt mask"));
        }
        Ok(())
    }
}

impl Cpu {
    /// The core's whole state, as a snapshot: registers, the run
    /// loop's timing and exception state, and memory (its regions,
    /// ROM's included, page tables and permissions).  Not the hooks.
    #[must_use]
    pub fn save(&self) -> Vec<u8> {
        let memory = &self.memory;
        let state = CpuState {
            registers: self.registers.clone(),
            instruction_pc: self.instruction_pc,
            previous_pc: self.previous_pc,
            last_tail: self.last_tail,
            stall: self.stall,
            extra: self.extra,
            refund: self.refund,
            stopped: self.stopped,
            halt: self.halt,
            fault_window: self.fault_window,
            released: self.released,
            altered: self.altered,
            movem: self.movem,
            movem_resume: self.movem_resume,
            untraced: self.untraced,
            sampled_level: self.sampled_level,
            sampled_mask: self.sampled_mask,
            taken: self.taken,
            instructions: self.instructions,
            resume: self.resume,
            loopable: self.loopable,
            loop_mode: self.loop_mode,
            address_mask: memory.address_mask,
            memory: memory.state(),
        };
        encode(VERSION, &state)
    }

    /// Restores a snapshot [`save`](Self::save) made of a CPU with the same
    /// address lines and regions of the same sizes; its hooks stay.  The
    /// instruction at the restored PC starts a block, as after a jump.  On
    /// an error the CPU is unchanged.
    ///
    /// # Errors
    ///
    /// [`decode`]'s; [`StateError::Mismatch`] for a snapshot of a CPU
    /// with other address lines or regions of other sizes, or page tables
    /// that do not fit them; [`StateError::Invalid`] for one no run could
    /// have saved.
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let state: CpuState = decode(VERSION, snapshot)?;
        if state.address_mask != self.memory.address_mask {
            return Err(StateError::mismatch("address lines"));
        }
        state.validate()?;
        self.memory
            .restore(state.memory)
            .map_err(StateError::mismatch)?;
        self.registers = state.registers;
        self.instruction_pc = state.instruction_pc;
        self.previous_pc = state.previous_pc;
        self.last_tail = state.last_tail;
        self.stall = state.stall;
        self.extra = state.extra;
        self.refund = state.refund;
        self.stopped = state.stopped;
        self.halt = state.halt;
        self.fault_window = state.fault_window;
        self.released = state.released;
        self.altered = state.altered;
        self.movem = state.movem;
        self.movem_resume = state.movem_resume;
        self.untraced = state.untraced;
        self.sampled_level = state.sampled_level;
        self.sampled_mask = state.sampled_mask;
        self.taken = state.taken;
        self.instructions = state.instructions;
        self.block_start = true;
        self.left_pc = Address(self.registers.pc);
        self.resume = state.resume;
        self.loopable = state.loopable;
        self.loop_mode = state.loop_mode;
        self.stop_request = None;
        Ok(())
    }
}
