//! The Atmel AT28C64B, an 8K x 8 parallel EEPROM.
//!
//! From its datasheet (`third_party/datasheets/AT28C64B.pdf`, Microchip
//! DS20006432B; the sections cited are its).  It reads as a static RAM
//! (6.1).  A write starts a write cycle that loads a page and programs
//! it on the peripheral's own timer (6.2, 6.3), reads poll while it
//! runs (6.4, 6.5), and software data protection guards the cells
//! (6.6.2, 6.18, 6.19).
//!
//! - A write starts a write cycle and loads its byte.  Each write
//!   within tBLC of the one before is loaded too; tBLC after the last,
//!   the peripheral stops taking bytes and programs the bytes loaded, and
//!   only those, for tWC (6.3, Table 6-4).  A byte loaded twice keeps
//!   the second.  Writes while it programs are not taken.
//! - The data bytes of a cycle are on one page, the 64 bytes A6 to A12
//!   select (6.3).  The datasheet forbids another page's byte and does
//!   not say what it does: here it is dropped, though it still restarts
//!   tBLC, as every write pulse does.
//! - From the first write to the end of tWC every read, at any
//!   address, polls (6.2): I/O7 the complement of the last byte written
//!   (DATA polling, 6.4), I/O6 the opposite of what the read before
//!   gave, from 1 (toggle bit, 6.5; "beginning and ending state of I/O6
//!   will vary", 6.24), I/O5 to I/O0 the last byte's own, which the
//!   datasheet does not give.
//! - Software data protection (6.6.2) is off as the peripheral ships, or as
//!   the `protected` parameter says.  A cycle whose first bytes are the
//!   enable sequence (6.18) programs the bytes after it and leaves the
//!   peripheral protected; one that starts with the disable sequence (6.19)
//!   programs the bytes after it and leaves it unprotected.  The
//!   sequences' own bytes are not written.  While protected, a cycle
//!   without a sequence programs nothing but runs its tWC, polling.
//!   Bytes that start as a sequence and stop short of one are data.
//!
//! Not modeled: the extra 64 bytes and chip erase, which take 12 V on
//! A9 or OE (6.7, 6.17), and the hardware protection (6.6.1), of
//! supply levels and pulses a board model does not have.
//!
//! The cells may live in a host file, `file`, which every programmed
//! byte is written through to as its cycle ends, one byte a `write`, so
//! a run's writes carry to the next.  The file itself is locked before
//! it is read and held while the peripheral lives, so a second run cannot
//! share it, nor read it while the first makes it whole; it is never
//! renamed over, so every run locks the same file, and nothing is left
//! beside it.  A new file is made whole, from `image` or erased, in a
//! sibling of a name of its own and linked into place, which never
//! replaces a file another run made first; a short one is padded erased
//! through the locked handle and synced, so a kill leaves it as it was
//! or longer, and the next run pads it the same.  Bytes written through
//! are not synced
//! one by one: a crash of the host may lose the last, as a power loss
//! during a write cycle loses its bytes on the peripheral.  Protection
//! persists, as power transitions do not disable it (6.6.2): a sidecar,
//! `<file>.sdp`, holds it, replaced as it changes; a file without one
//! (a new file, or a bare 8 KiB dump) starts from `protected`.

use fs::File;
use fs::OpenOptions;
use fs::TryLockError;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::mem;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use mc683xx_board::inspect::Inspection;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::PeripheralAccess;
use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::Registry;
use mc683xx_board::registry::RegistryError;
use mc683xx_board::time::PICOSECONDS_PER_SECOND;
use mc683xx_board::time::Time;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use serde::Deserialize;
use serde::Serialize;

/// The type's registered name.
pub(crate) const NAME: &str = "at28c64b";

/// The cells: 8,192 bytes (3, Description).
const SIZE: usize = 0x2000;
/// A0 to A12, the address pins (2, Pin Descriptions).
const ADDRESS_MASK: u32 = SIZE as u32 - 1;
/// A0 to A5 address a byte in its page; A6 to A12 the page (6.3).
const PAGE_BITS: u32 = 6;
/// A cell never written in a run without an image: all ones, as an
/// erased EEPROM cell reads.  The datasheet gives no value, nor the
/// contents a peripheral ships with.
const ERASED: u8 = 0xff;

/// Picoseconds in a microsecond.
const PICOSECONDS_PER_MICROSECOND: u64 = PICOSECONDS_PER_SECOND / 1_000_000;
/// tBLC, the Byte Load Cycle Time, at most (Table 6-4).
const BYTE_LOAD: u64 = 150 * PICOSECONDS_PER_MICROSECOND;
/// tWC, the Write Cycle Time, at most, in microseconds (Table 6-4): a
/// peripheral's own is anything up to it, and the model's by default.
const WRITE_CYCLE_MAX_US: u64 = 10_000;

/// I/O7, DATA polling's bit (6.4).
const DATA_POLLING: u8 = 0x80;
/// I/O6, the toggle bit (6.5).
const TOGGLE_BIT: u8 = 0x40;

/// The addresses software data protection's sequences write (6.18,
/// 6.19: A12 to A0).
const COMMAND_FIRST: Offset = Offset(0x1555);
/// The second.
const COMMAND_SECOND: Offset = Offset(0x0aaa);
/// The enable sequence's bytes (6.18).
const ENABLE: [(Offset, u8); 3] = [
    (COMMAND_FIRST, 0xaa),
    (COMMAND_SECOND, 0x55),
    (COMMAND_FIRST, 0xa0),
];
/// The disable sequence's (6.19).
const DISABLE: [(Offset, u8); 6] = [
    (COMMAND_FIRST, 0xaa),
    (COMMAND_SECOND, 0x55),
    (COMMAND_FIRST, 0x80),
    (COMMAND_FIRST, 0xaa),
    (COMMAND_SECOND, 0x55),
    (COMMAND_FIRST, 0x20),
];

/// The parameters: the cells' first contents, a path.
const IMAGE: &str = "image";
/// The file the cells live in, a path.
const FILE: &str = "file";
/// tWC, in microseconds.
const WRITE_CYCLE_US: &str = "write_cycle_us";
/// Whether software data protection is on at power-on.
const PROTECTED: &str = "protected";
/// What the file in the run's work directory, without `file`, adds to
/// the peripheral's name.
const WORKING_SUFFIX: &str = ".eeprom";
/// What the sibling a file is made in adds to its name, after the
/// process and a count that make it unique.
const PARTIAL: &str = ".partial";
/// What the sidecar holding software data protection adds.
const PROTECTION: &str = ".sdp";
/// The protection sidecar's contents, protected or not.
const PROTECTION_TEXT: [(&str, bool); 2] = [("protected\n", true), ("unprotected\n", false)];

/// Siblings made by this process, so that each has a name of its own.
static SIBLINGS: AtomicU64 = AtomicU64::new(0);

/// The snapshot's format.
const VERSION: u32 = 2;

/// A cell's address on A0 to A12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Offset(u16);

impl Offset {
    /// The cell `address` reaches: its low 13 bits, the peripheral's pins.
    const fn new(address: u32) -> Self {
        Self((address & ADDRESS_MASK) as u16)
    }

    /// Its page.
    const fn page(self) -> u16 {
        self.0 >> PAGE_BITS
    }

    /// The cell's index.
    const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A software data protection sequence (6.18, 6.19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    /// Protect.
    Enable,
    /// Unprotect.
    Disable,
}

/// A write cycle: its bytes loaded, then programmed (6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WriteCycle {
    /// The cycle's first bytes while they are a sequence or the start
    /// of one: a prefix of `ENABLE` or `DISABLE`.
    command: Vec<(Offset, u8)>,
    /// The data bytes loaded, by offset, the last of each kept; on one
    /// page.
    data: BTreeMap<Offset, u8>,
    /// The last byte written, which DATA polling complements (6.4).
    last: u8,
    /// When it was written.
    last_at: Time,
    /// What the toggle bit read last.
    toggle: bool,
}

impl WriteCycle {
    /// A cycle started by `value` written at `offset` at `now`.
    fn new(offset: Offset, value: u8, now: Time) -> Self {
        let mut cycle = Self {
            command: Vec::new(),
            data: BTreeMap::new(),
            last: value,
            last_at: now,
            toggle: false,
        };
        cycle.load(offset, value, now);
        cycle
    }

    /// Loads `value` at `offset`, written at `now`.
    fn load(&mut self, offset: Offset, value: u8, now: Time) {
        self.last = value;
        self.last_at = now;
        if !self.data.is_empty() || self.command().is_some() {
            self.load_data(offset, value);
            return;
        }
        self.command.push((offset, value));
        if !(ENABLE.starts_with(&self.command) || DISABLE.starts_with(&self.command)) {
            self.flush_command();
        }
    }

    /// Loads a data byte, if it is on the page of those before it.
    fn load_data(&mut self, offset: Offset, value: u8) {
        let page = self.data.keys().next().map(|first| first.page());
        if page.is_none_or(|page| page == offset.page()) {
            self.data.insert(offset, value);
        }
    }

    /// The bytes taken for a sequence, now data.
    fn flush_command(&mut self) {
        for (offset, value) in mem::take(&mut self.command) {
            self.load_data(offset, value);
        }
    }

    /// The sequence its first bytes are, if they are one.
    fn command(&self) -> Option<Command> {
        if self.command == ENABLE {
            Some(Command::Enable)
        } else if self.command == DISABLE {
            Some(Command::Disable)
        } else {
            None
        }
    }

    /// When it stops taking bytes and starts programming.
    fn programming_from(&self) -> Time {
        self.last_at + BYTE_LOAD
    }

    /// When its programming ends, `write_cycle` picoseconds later.
    fn done(&self, write_cycle: u64) -> Time {
        self.programming_from() + write_cycle
    }

    /// A read while it runs (6.4, 6.5).
    fn poll(&mut self) -> u8 {
        self.toggle = !self.toggle;
        let toggle = if self.toggle { TOGGLE_BIT } else { 0 };
        (!self.last & DATA_POLLING) | toggle | (self.last & !(DATA_POLLING | TOGGLE_BIT))
    }

    /// Refuses what no write cycle loads: first bytes that are no
    /// sequence's start, data bytes beside a sequence's start, or data
    /// bytes past the cells or on two pages.
    fn check(&self) -> Result<(), StateError> {
        let invalid = |what| Err(StateError::invalid(what));
        if !(ENABLE.starts_with(&self.command) || DISABLE.starts_with(&self.command)) {
            return invalid("an AT28C64B cycle starting with no sequence");
        }
        if !self.data.is_empty() && !self.command.is_empty() && self.command().is_none() {
            return invalid("an AT28C64B cycle loading data within a sequence");
        }
        if self.data.keys().any(|offset| offset.index() >= SIZE) {
            return invalid("an AT28C64B byte past its cells");
        }
        let mut pages = self.data.keys().map(|offset| offset.page());
        if let Some(first) = pages.next()
            && pages.any(|page| page != first)
        {
            return invalid("an AT28C64B cycle on two pages");
        }
        Ok(())
    }

    /// Its sequence, if any, and the bytes it programs.
    fn finish(mut self) -> (Option<Command>, BTreeMap<Offset, u8>) {
        let command = self.command();
        if command.is_none() {
            self.flush_command();
        }
        (command, self.data)
    }
}

/// Why the cells' file or image could not be used.
#[derive(Debug, thiserror::Error)]
enum OpenError {
    /// A file could not be read, made, opened or locked.
    #[error("{what} {}: {error}", path.display())]
    File {
        /// What was being done.
        what: &'static str,
        /// To which file.
        path: PathBuf,
        /// What failed.
        error: io::Error,
    },
    /// A file larger than the peripheral.
    #[error("{} holds {size} bytes, more than the peripheral's {SIZE}", path.display())]
    TooLarge {
        /// The file.
        path: PathBuf,
        /// Its size.
        size: usize,
    },
    /// Another run has the file open.
    #[error("{} is in use by another run", path.display())]
    InUse {
        /// The file.
        path: PathBuf,
    },
    /// A protection sidecar that says neither.
    #[error("{} holds neither \"protected\" nor \"unprotected\"", path.display())]
    Protection {
        /// The sidecar.
        path: PathBuf,
    },
}

/// A failure of `what` on `path`.
fn failed<'a>(what: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> OpenError + 'a {
    move |error| OpenError::File {
        what,
        path: path.to_owned(),
        error,
    }
}

/// `path`'s contents, which must fit the peripheral.
fn contents(path: &Path) -> Result<Vec<u8>, OpenError> {
    let bytes = fs::read(path).map_err(failed("reading", path))?;
    if bytes.len() > SIZE {
        return Err(OpenError::TooLarge {
            path: path.to_owned(),
            size: bytes.len(),
        });
    }
    Ok(bytes)
}

/// `cells` padded erased to the peripheral's size.
fn whole(mut cells: Vec<u8>) -> Vec<u8> {
    cells.resize(SIZE, ERASED);
    cells
}

/// `path` with `suffix` added to its name.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A new sibling of `path` of a name no other file has: the process,
/// a count and `.partial` added to its name.
fn sibling(path: &Path) -> Result<(PathBuf, File), OpenError> {
    loop {
        let count = SIBLINGS.fetch_add(1, Ordering::Relaxed);
        let partial = sidecar(path, &format!(".{}-{count}{PARTIAL}", process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
        {
            Ok(file) => return Ok((partial, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(failed("creating", &partial)(error)),
        }
    }
}

/// Replaces `path` with `bytes`, or makes it, at once: a sibling
/// written and synced, then renamed over it.
fn replace(path: &Path, bytes: &[u8]) -> Result<(), OpenError> {
    let partial = written_sibling(path, bytes)?;
    if let Err(error) = fs::rename(&partial, path) {
        let _ = fs::remove_file(&partial);
        return Err(failed("renaming over", path)(error));
    }
    sync_directory(path)
}

/// Syncs `path`'s directory, so that a name made or replaced in it is
/// durable; a directory opens as a file only on Unix.
fn sync_directory(path: &Path) -> Result<(), OpenError> {
    #[cfg(unix)]
    if let Some(directory) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(failed("syncing", directory))?;
    }
    Ok(())
}

/// A new sibling of `path` holding `bytes`, written and synced.
fn written_sibling(path: &Path, bytes: &[u8]) -> Result<PathBuf, OpenError> {
    let (partial, mut file) = sibling(path)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    match written {
        Ok(()) => Ok(partial),
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(failed("writing", &partial)(error))
        }
    }
}

/// Makes `path` holding `bytes` if no file is there: a sibling written
/// and synced, then linked to `path`.  A link never replaces a file, so
/// one another run made first stays.
fn publish(path: &Path, bytes: &[u8]) -> Result<(), OpenError> {
    let partial = written_sibling(path, bytes)?;
    let linked = fs::hard_link(&partial, path);
    let _ = fs::remove_file(&partial);
    match linked {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
            Err(failed("linking", path)(error))
        }
        _ => sync_directory(path),
    }
}

/// `path` open to read and write and locked, made from `image` (or
/// erased) if it does not exist.
fn open_locked(path: &Path, image: Option<Vec<u8>>) -> Result<File, OpenError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    let file = match options.open(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            publish(path, &whole(image.unwrap_or_default()))?;
            options.open(path)
        }
        opened => opened,
    }
    .map_err(failed("opening", path))?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => OpenError::InUse {
            path: path.to_owned(),
        },
        TryLockError::Error(error) => failed("locking", path)(error),
    })?;
    Ok(file)
}

/// The cells `file`, `path` open and locked, holds: padded erased
/// through it and synced if short, refused if larger than the
/// peripheral.  Padding only appends, so one a kill cut short pads the
/// same at the next run.
fn read_whole(file: &mut File, path: &Path) -> Result<Vec<u8>, OpenError> {
    let mut found = Vec::with_capacity(SIZE);
    file.read_to_end(&mut found)
        .map_err(failed("reading", path))?;
    let length = found.len();
    if length > SIZE {
        return Err(OpenError::TooLarge {
            path: path.to_owned(),
            size: length,
        });
    }
    let cells = whole(found);
    if length < SIZE {
        write_through(file, length as u64, &cells[length..])
            .and_then(|()| file.sync_all())
            .map_err(failed("padding", path))?;
    }
    Ok(cells)
}

/// A file opened: its cells, the protection its sidecar holds, if it
/// has one, and the file held.
type Opened = (Vec<u8>, Option<bool>, Store);

/// The protection `path`'s sidecar holds, if it has one.
fn protection(path: &Path) -> Result<Option<bool>, OpenError> {
    if !path.try_exists().map_err(failed("finding", path))? {
        return Ok(None);
    }
    let text = fs::read(path).map_err(failed("reading", path))?;
    PROTECTION_TEXT
        .iter()
        .find(|(said, _)| said.as_bytes() == text)
        .map(|&(_, protected)| Some(protected))
        .ok_or_else(|| OpenError::Protection {
            path: path.to_owned(),
        })
}

/// The cells in `path`, open to write and locked before it is read:
/// made from `image`, or erased, if it does not exist, and made whole
/// if it is short.  With them the protection its sidecar holds, if it
/// has one, read under the lock.
fn open(path: &Path, image: Option<Vec<u8>>) -> Result<Opened, OpenError> {
    let mut file = open_locked(path, image)?;
    let cells = read_whole(&mut file, path)?;
    let protection_path = sidecar(path, PROTECTION);
    let protected = protection(&protection_path)?;
    Ok((
        cells,
        protected,
        Store {
            file: Some(file),
            protection: protection_path,
        },
    ))
}

/// The cells' host file, held while the peripheral lives.
#[derive(Debug)]
struct Store {
    /// The file, locked and open to write through; dropped, and its
    /// lock released, if a write fails.
    file: Option<File>,
    /// The sidecar holding software data protection.
    protection: PathBuf,
}

/// Writes `bytes` at `offset` in `file`.
fn write_through(file: &mut File, offset: u64, bytes: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)
}

/// An AT28C64B on the bus, on A0 to A12 and I/O0 to I/O7.
///
/// An 8-bit peripheral, each access one byte at the address its
/// attachment gives ([`PeripheralAccess::written_byte`]).
#[derive(Debug)]
pub struct At28c64b {
    /// The cells.
    cells: Vec<u8>,
    /// Whether software data protection is on (6.6.2).
    protected: bool,
    /// The write cycle running, if one is.
    writing: Option<WriteCycle>,
    /// tWC, in picoseconds.
    write_cycle: u64,
    /// The file the cells are written through to, if any.
    store: Option<Store>,
    /// Why writing through to the file failed, if it did; the file is
    /// then dropped, and the run stopped.
    file_error: Option<io::Error>,
}

impl At28c64b {
    /// A peripheral holding `cells`, padded erased, with tWC `write_cycle`
    /// picoseconds and protection `protected`, writing through to
    /// `store`.
    fn new(cells: Vec<u8>, write_cycle: u64, protected: bool, store: Option<Store>) -> Self {
        Self {
            cells: whole(cells),
            protected,
            writing: None,
            write_cycle,
            store,
            file_error: None,
        }
    }

    /// The file the cells are written through to, while it is.
    fn file(&mut self) -> Option<&mut File> {
        self.store.as_mut().and_then(|store| store.file.as_mut())
    }

    /// A peripheral from its parameters: `image`, `file` (or, without it,
    /// `<name>.eeprom` in the run's work directory), `write_cycle_us`
    /// and `protected`.
    fn from_context(context: &Context<'_>) -> Result<Self, ParameterError> {
        let invalid = |key: &str| {
            let key = key.to_owned();
            move |error: OpenError| ParameterError::invalid(key, error.to_string())
        };
        let write_cycle_us = context
            .parameters
            .integer(WRITE_CYCLE_US)?
            .unwrap_or(WRITE_CYCLE_MAX_US);
        if !(1..=WRITE_CYCLE_MAX_US).contains(&write_cycle_us) {
            return Err(ParameterError::invalid(
                WRITE_CYCLE_US,
                format!("{write_cycle_us} is not 1 to {WRITE_CYCLE_MAX_US}"),
            ));
        }
        let protected = context.parameters.boolean(PROTECTED)?.unwrap_or(false);
        let image = context
            .path(IMAGE)?
            .map(|path| contents(&path))
            .transpose()
            .map_err(invalid(IMAGE))?;
        let path = context.path(FILE)?.or_else(|| {
            context
                .environment
                .work_dir
                .as_ref()
                .map(|directory| directory.join(format!("{}{WORKING_SUFFIX}", context.name)))
        });
        let (cells, protected, store) = match path {
            Some(path) => {
                let (cells, found, store) = open(&path, image).map_err(invalid(FILE))?;
                (cells, found.unwrap_or(protected), Some(store))
            }
            None => (image.unwrap_or_default(), protected, None),
        };
        Ok(Self::new(
            cells,
            write_cycle_us * PICOSECONDS_PER_MICROSECOND,
            protected,
            store,
        ))
    }

    /// The cells.
    #[must_use]
    pub fn cells(&self) -> &[u8] {
        &self.cells
    }

    /// Whether software data protection is on.
    #[must_use]
    pub const fn protected(&self) -> bool {
        self.protected
    }

    /// Why writing through to the file failed, if it did.
    #[must_use]
    pub const fn file_error(&self) -> Option<&io::Error> {
        self.file_error.as_ref()
    }

    /// Brings it to `now`: a write cycle whose tWC is over leaves its
    /// bytes in the cells and the file, and its sequence's protection.
    fn settle(&mut self, now: Time, drive: &mut Drive<'_>) {
        let write_cycle = self.write_cycle;
        let Some(cycle) = self.writing.take_if(|cycle| cycle.done(write_cycle) <= now) else {
            return;
        };
        let (command, data) = cycle.finish();
        if !self.protected || command.is_some() {
            for (offset, value) in data {
                self.program(offset, value, drive);
            }
        }
        let protected = match command {
            Some(Command::Enable) => true,
            Some(Command::Disable) => false,
            None => self.protected,
        };
        if protected != self.protected {
            self.protected = protected;
            if let Err(error) = self.persist_protection(protected) {
                self.drop_file(io::Error::other(error));
                drive.stop();
            }
        }
    }

    /// Drops the file, releasing its lock, for `error` writing it
    /// through: the cells live on in memory alone.
    fn drop_file(&mut self, error: io::Error) {
        if let Some(store) = &mut self.store {
            store.file = None;
        }
        self.file_error = Some(error);
    }

    /// Replaces the protection sidecar with `protected`, while the file
    /// is written through.
    fn persist_protection(&self, protected: bool) -> Result<(), OpenError> {
        match &self.store {
            Some(store) if store.file.is_some() => {
                let (text, _) = PROTECTION_TEXT
                    .iter()
                    .find(|&&(_, said)| said == protected)
                    .expect("both protections have a text");
                replace(&store.protection, text.as_bytes())
            }
            _ => Ok(()),
        }
    }

    /// Programs `value` at `offset`, through to the file.  A failure
    /// there drops the file and stops the run.
    fn program(&mut self, offset: Offset, value: u8, drive: &mut Drive<'_>) {
        self.cells[offset.index()] = value;
        let written = self
            .file()
            .map(|file| write_through(file, offset.0.into(), &[value]));
        if let Some(Err(error)) = written {
            self.drop_file(error);
            drive.stop();
        }
    }

    /// A read of `offset`, settled: the cell, or the cycle's polling.
    fn read_byte(&mut self, offset: Offset) -> u8 {
        match &mut self.writing {
            Some(cycle) => cycle.poll(),
            None => self.cells[offset.index()],
        }
    }

    /// A write of `value` to `offset` at `now`, settled: loaded into the
    /// cycle, or a cycle started.
    fn write_byte(&mut self, offset: Offset, value: u8, now: Time) {
        match &mut self.writing {
            None => self.writing = Some(WriteCycle::new(offset, value, now)),
            Some(cycle) if now < cycle.programming_from() => cycle.load(offset, value, now),
            Some(_) => {}
        }
    }
}

impl Peripheral for At28c64b {
    fn read(&mut self, access: PeripheralAccess, drive: &mut Drive<'_>) -> Result<u32, BusError> {
        self.settle(access.now, drive);
        let byte = self.read_byte(Offset::new(access.address));
        Ok(access.read_byte(byte))
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        self.settle(access.now, drive);
        let byte = access.written_byte(value);
        self.write_byte(Offset::new(access.address), byte, access.now);
        Ok(())
    }

    fn next_event(&self) -> Option<Time> {
        self.writing
            .as_ref()
            .map(|cycle| cycle.done(self.write_cycle))
    }

    fn advance(&mut self, now: Time, drive: &mut Drive<'_>) {
        self.settle(now, drive);
    }

    /// A summary of the cells (their count and how many are not
    /// erased; [`At28c64b::cells`] gives them), the protection, the write cycle
    /// running and a failed write-through.
    fn inspect(&self) -> Inspection {
        let programmed = self.cells.iter().filter(|&&cell| cell != ERASED).count();
        let shown = Inspection::default()
            .count("cells", self.cells.len())
            .count("programmed", programmed)
            .boolean("protected", self.protected);
        let shown = match &self.writing {
            Some(cycle) => shown.group(
                "writing",
                Inspection::default()
                    .count("loaded", cycle.data.len() + cycle.command.len())
                    .register("last", cycle.last)
                    .time("done", cycle.done(self.write_cycle)),
            ),
            None => shown.boolean("writing", false),
        };
        match &self.file_error {
            Some(error) => shown.text("file_error", error.to_string()),
            None => shown,
        }
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(
            VERSION,
            &(self.write_cycle, &self.cells, self.protected, &self.writing),
        ))
    }

    /// Restores the snapshot's cells, writing them through to the file,
    /// so that it holds the snapshot's; the file is the loaded peripheral's,
    /// not part of the snapshot, and so is its protection sidecar, written
    /// only when the protection changes.  Refuses another tWC's snapshot,
    /// cells not the peripheral's size and a cycle `WriteCycle::check`
    /// refuses.  The restore itself is in memory and cannot fail: a file
    /// or sidecar that cannot be written drops the file, as a run's
    /// failed write does, and [`At28c64b::file_error`] says why.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let (write_cycle, cells, protected, writing): (u64, Vec<u8>, bool, Option<WriteCycle>) =
            decode(VERSION, snapshot)?;
        if write_cycle != self.write_cycle {
            return Err(StateError::mismatch("an AT28C64B of another tWC"));
        }
        if cells.len() != SIZE {
            return Err(StateError::invalid("AT28C64B cells not 8K"));
        }
        if let Some(cycle) = &writing {
            cycle.check()?;
        }
        let written = self
            .file()
            .map_or(Ok(()), |file| write_through(file, 0, &cells));
        if let Err(error) = written {
            self.drop_file(error);
        }
        if protected != self.protected
            && let Err(error) = self.persist_protection(protected)
        {
            self.drop_file(io::Error::other(error));
        }
        self.cells = cells;
        self.protected = protected;
        self.writing = writing;
        Ok(())
    }
}

/// Registers the type.
pub(crate) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register(NAME, Vec::new(), |context| {
        Ok(Box::new(At28c64b::from_context(context)?) as Box<dyn Peripheral>)
    })
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::Barrier;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::thread;

    use mc683xx_board::inspect::Value;
    use mc683xx_board::peripheral::Drive;
    use mc683xx_board::peripheral::Peripheral;
    use mc683xx_board::peripheral::PeripheralAccess;
    use mc683xx_board::registry::Context;
    use mc683xx_board::registry::Environment;
    use mc683xx_board::registry::ParameterError;
    use mc683xx_board::registry::Parameters;
    use mc683xx_board::time::Time;
    use mc683xx_cpu32::cpu::state::StateError;
    use mc683xx_cpu32::cpu::state::encode;
    use mc683xx_cpu32::function_code::FunctionCode;
    use mc683xx_cpu32::size::Size;
    use mc683xx_modules::chip_select::ChipSelect;

    use super::At28c64b;
    use super::BYTE_LOAD;
    use super::COMMAND_FIRST;
    use super::DISABLE;
    use super::ENABLE;
    use super::ERASED;
    use super::Offset;
    use super::OpenError;
    use super::Opened;
    use super::PICOSECONDS_PER_MICROSECOND;
    use super::SIZE;
    use super::VERSION;
    use super::WRITE_CYCLE_MAX_US;
    use super::WriteCycle;
    use super::open;

    /// tWC in the tests.
    const WRITE_CYCLE: u64 = WRITE_CYCLE_MAX_US * PICOSECONDS_PER_MICROSECOND;

    /// A peripheral holding `cells`, unprotected, with no file.
    fn peripheral(cells: Vec<u8>) -> At28c64b {
        At28c64b::new(cells, WRITE_CYCLE, false, None)
    }

    /// `peripheral` brought to `now`.
    fn at(peripheral: &mut At28c64b, now: Time) {
        peripheral.settle(now, &mut Drive::new(now));
    }

    /// A byte written at `now`, as the bus writes one.
    fn write(peripheral: &mut At28c64b, offset: u16, value: u8, now: Time) {
        at(peripheral, now);
        peripheral.write_byte(Offset(offset), value, now);
    }

    /// Bytes written `BYTE_LOAD / 2` apart from `start`; the last one's
    /// time.
    fn load(peripheral: &mut At28c64b, bytes: &[(Offset, u8)], start: Time) -> Time {
        let step = BYTE_LOAD / 2;
        let mut now = start;
        for (index, (offset, value)) in bytes.iter().enumerate() {
            now = start + step * index as u64;
            write(peripheral, offset.0, *value, now);
        }
        now
    }

    /// A cell read.
    fn read(peripheral: &mut At28c64b, offset: u16) -> u8 {
        peripheral.read_byte(Offset(offset))
    }

    /// When a cycle whose last byte was at `last` is done.
    fn done(last: Time) -> Time {
        last + BYTE_LOAD + WRITE_CYCLE
    }

    /// Scratch directories made, so that tests running at once never
    /// share one.
    static DIRECTORIES: AtomicUsize = AtomicUsize::new(0);

    /// A directory of a test's own, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let unique = DIRECTORIES.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "mc683xx-at28c64b-{name}-{}-{unique}",
                process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A peripheral from the board-file parameters `text`, in `directory`.
    fn construct(text: &str, directory: &Scratch) -> Result<At28c64b, ParameterError> {
        let parameters = Parameters::parse(text).unwrap();
        let mut environment = Environment::default();
        environment.board_dir = directory.0.clone();
        let context = Context::new("eeprom", &parameters, &environment);
        let peripheral = At28c64b::from_context(&context);
        assert_eq!(parameters.unread(), None);
        peripheral
    }

    /// DATA polling and the toggle bit (6.4, 6.5) until tWC is over.
    #[test]
    fn a_byte_polls_until_its_write_cycle_ends() {
        let mut peripheral = peripheral(vec![0; 16]);
        write(&mut peripheral, 0x10c0, 0x5a, Time::ZERO);
        let polls = [read(&mut peripheral, 0x10c0), read(&mut peripheral, 0x10c0)];
        assert_eq!(
            polls[0] & 0x80,
            0x80,
            "I/O7 complemented: {:#04x}",
            polls[0]
        );
        assert_eq!(polls[0] & 0x3f, 0x1a, "the rest the byte's");
        assert_ne!(polls[0] & 0x40, polls[1] & 0x40, "I/O6 toggles");
        assert_eq!(
            read(&mut peripheral, 0x0003) & 0x80,
            0x80,
            "any address polls"
        );
        at(&mut peripheral, done(Time::ZERO) - 1);
        assert_ne!(read(&mut peripheral, 0x10c0), 0x5a, "still programming");
        assert_eq!(peripheral.next_event(), Some(done(Time::ZERO)));
        at(&mut peripheral, done(Time::ZERO));
        assert_eq!(read(&mut peripheral, 0x10c0), 0x5a);
        assert_eq!(read(&mut peripheral, 0x10c0), 0x5a, "settled");
        assert_eq!(peripheral.next_event(), None);
    }

    #[test]
    fn it_shows_its_cells_and_the_cycle_running() {
        let mut peripheral = peripheral(vec![ERASED; SIZE]);
        write(&mut peripheral, 0x10c0, 0x5a, Time::ZERO);
        let inspection = peripheral.inspect();
        assert_eq!(inspection.get("programmed"), Some(&Value::Integer(0)));
        let Some(Value::Group(writing)) = inspection.get("writing") else {
            panic!("a cycle running");
        };
        assert_eq!(writing.get("last"), Some(&Value::Integer(0x5a)));
        let end = i64::try_from(done(Time::ZERO).0).unwrap();
        assert_eq!(writing.get("done"), Some(&Value::Integer(end)));
        at(&mut peripheral, done(Time::ZERO));
        let inspection = peripheral.inspect();
        assert_eq!(inspection.get("cells"), Some(&Value::Integer(8192)));
        assert_eq!(inspection.get("programmed"), Some(&Value::Integer(1)));
        assert_eq!(inspection.get("writing"), Some(&Value::Boolean(false)));
        assert_eq!(inspection.get("protected"), Some(&Value::Boolean(false)));
    }

    /// A page's bytes within tBLC of each other program together, a
    /// byte loaded twice keeping the second; one of another page during
    /// the load, and any during the programming, are not taken (6.3).
    #[test]
    fn a_page_loads_within_tblc_and_programs_at_once() {
        let mut peripheral = peripheral(vec![0; SIZE]);
        let bytes = [(0x40, 1), (0x41, 2), (0x7f, 3), (0x41, 4), (0x80, 9)];
        let last = load(
            &mut peripheral,
            &bytes.map(|(offset, value)| (Offset(offset), value)),
            Time(100),
        );
        write(&mut peripheral, 0x42, 5, last + BYTE_LOAD);
        at(&mut peripheral, done(last));
        let cells = [0x40, 0x41, 0x42, 0x7f, 0x80].map(|offset| read(&mut peripheral, offset));
        assert_eq!(cells, [1, 4, 0, 3, 0]);
    }

    /// A word cycle reaches one cell, on DATA\[15:8\] (SIMRM 5.3.3).
    #[test]
    fn a_word_cycle_is_one_byte_on_data_15_8() {
        let mut peripheral = peripheral(vec![0; SIZE]);
        let access = |now| {
            PeripheralAccess::new(
                ChipSelect::Boot,
                0x40,
                Size::Word,
                FunctionCode::SUPERVISOR_DATA,
                now,
            )
        };
        let mut drive = Drive::new(Time::ZERO);
        peripheral
            .write(access(Time::ZERO), 0x1234, &mut drive)
            .unwrap();
        at(&mut peripheral, done(Time::ZERO));
        assert_eq!(
            [read(&mut peripheral, 0x40), read(&mut peripheral, 0x41)],
            [0x12, 0]
        );
        let now = done(Time::ZERO);
        assert_eq!(
            peripheral.read(access(now), &mut Drive::new(now)),
            Ok(0x1200)
        );
    }

    /// Past tBLC and tWC a byte starts a write cycle of its own.
    #[test]
    fn a_byte_after_the_cycle_is_a_cycle_of_its_own() {
        let mut peripheral = peripheral(vec![0; SIZE]);
        write(&mut peripheral, 0x10, 1, Time::ZERO);
        let first = done(Time::ZERO);
        write(&mut peripheral, 0x11, 2, first);
        assert_eq!(read(&mut peripheral, 0x10) & 0x80, 0x80, "the second polls");
        at(&mut peripheral, done(first));
        assert_eq!(
            [read(&mut peripheral, 0x10), read(&mut peripheral, 0x11)],
            [1, 2]
        );
    }

    /// The enable sequence protects the peripheral at the end of its cycle,
    /// programming the bytes after it and not its own (6.6.2, 6.18).
    #[test]
    fn the_enable_sequence_protects_and_writes_the_bytes_after_it() {
        let mut peripheral = peripheral(vec![0; SIZE]);
        let mut bytes = ENABLE.to_vec();
        bytes.push((Offset(0x0200), 0x77));
        let last = load(&mut peripheral, &bytes, Time::ZERO);
        assert!(!peripheral.protected());
        at(&mut peripheral, done(last));
        assert!(peripheral.protected());
        assert_eq!(read(&mut peripheral, 0x0200), 0x77);
        assert_eq!(read(&mut peripheral, COMMAND_FIRST.0), 0, "not written");
        assert_eq!(read(&mut peripheral, 0x0aaa), 0);
    }

    /// While protected, a write without the sequence programs nothing
    /// but polls for tWC (6.6.2); with it, its bytes are written.
    #[test]
    fn a_protected_peripheral_writes_only_after_the_sequence() {
        let mut peripheral = At28c64b::new(vec![0; SIZE], WRITE_CYCLE, true, None);
        write(&mut peripheral, 0x0300, 0x11, Time::ZERO);
        assert_eq!(read(&mut peripheral, 0x0300) & 0x80, 0x80, "polls");
        at(&mut peripheral, done(Time::ZERO));
        assert_eq!(read(&mut peripheral, 0x0300), 0);
        let mut bytes = ENABLE.to_vec();
        bytes.push((Offset(0x0300), 0x22));
        let start = done(Time::ZERO);
        let last = load(&mut peripheral, &bytes, start);
        at(&mut peripheral, done(last));
        assert_eq!(read(&mut peripheral, 0x0300), 0x22);
        assert!(peripheral.protected());
    }

    /// The disable sequence unprotects, even with no data after it
    /// (6.19, note 3).
    #[test]
    fn the_disable_sequence_unprotects() {
        let mut peripheral = At28c64b::new(vec![0; SIZE], WRITE_CYCLE, true, None);
        let last = load(&mut peripheral, &DISABLE, Time::ZERO);
        at(&mut peripheral, done(last));
        assert!(!peripheral.protected());
        write(&mut peripheral, 0x0400, 0x33, done(last));
        at(&mut peripheral, done(done(last)));
        assert_eq!(read(&mut peripheral, 0x0400), 0x33);
        assert_eq!(read(&mut peripheral, COMMAND_FIRST.0), 0);
    }

    /// Bytes that start as a sequence and stop short of one are data.
    #[test]
    fn a_broken_sequence_is_data() {
        let mut peripheral = peripheral(vec![0; SIZE]);
        let last = load(&mut peripheral, &ENABLE[..1], Time::ZERO);
        at(&mut peripheral, done(last));
        assert_eq!(read(&mut peripheral, COMMAND_FIRST.0), 0xaa);
        assert!(!peripheral.protected());
    }

    #[test]
    fn it_starts_from_its_image_padded_erased() {
        let mut peripheral = peripheral(vec![1, 2]);
        assert_eq!(read(&mut peripheral, 1), 2);
        assert_eq!(read(&mut peripheral, 2), ERASED);
    }

    #[test]
    fn writes_reach_the_file_and_the_next_run() {
        let scratch = Scratch::new("persist");
        fs::write(scratch.file("image.bin"), [7_u8; SIZE]).unwrap();
        let parameters = "image = \"image.bin\"\nfile = \"cells.bin\"";
        let mut peripheral = construct(parameters, &scratch).unwrap();
        write(&mut peripheral, 0xc3, 0x77, Time::ZERO);
        at(&mut peripheral, done(Time::ZERO) - 1);
        let held = fs::read(scratch.file("cells.bin")).unwrap();
        assert_eq!(held[0xc3], 7, "not before its cycle ends");
        at(&mut peripheral, done(Time::ZERO));
        drop(peripheral);
        let held = fs::read(scratch.file("cells.bin")).unwrap();
        assert_eq!(held.len(), SIZE);
        assert_eq!(held[0xc3], 0x77);
        assert_eq!(held[0xc2], 7);
        assert_eq!(fs::read(scratch.file("image.bin")).unwrap(), [7_u8; SIZE]);
        let mut again = construct(parameters, &scratch).unwrap();
        assert_eq!(read(&mut again, 0xc3), 0x77, "kept, not the image again");
    }

    #[test]
    fn without_an_image_the_file_starts_erased_and_a_short_one_is_made_whole() {
        let scratch = Scratch::new("whole");
        let path = scratch.file("cells.bin");
        drop(open(&path, None).unwrap());
        assert_eq!(fs::read(&path).unwrap(), [ERASED; SIZE]);
        fs::write(&path, [1, 2, 3]).unwrap();
        let (cells, _, store) = open(&path, None).unwrap();
        drop(store);
        assert_eq!(&cells[..4], [1, 2, 3, ERASED], "padded erased");
        assert_eq!(fs::read(&path).unwrap().len(), SIZE);
        let names: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["cells.bin"], "nothing beside it");
    }

    /// A run leaves no lock or sibling beside the file once its
    /// peripheral is dropped, protected or not.
    #[test]
    fn a_dropped_peripheral_leaves_only_its_file_and_protection() {
        let scratch = Scratch::new("stray");
        let mut peripheral = construct("file = \"cells.bin\"", &scratch).unwrap();
        let last = load(&mut peripheral, &ENABLE, Time::ZERO);
        at(&mut peripheral, done(last));
        drop(peripheral);
        let mut names: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["cells.bin", "cells.bin.sdp"]);
    }

    /// A short file's padding cut short by a kill pads the same at the
    /// next run.
    #[test]
    fn padding_cut_short_pads_the_same() {
        let scratch = Scratch::new("pad");
        let path = scratch.file("cells.bin");
        let mut cut = vec![1, 2, 3];
        cut.resize(100, ERASED);
        fs::write(&path, &cut).unwrap();
        let (cells, _, _) = open(&path, Some(vec![9; SIZE])).unwrap();
        assert_eq!(&cells[..4], [1, 2, 3, ERASED], "not the image");
        assert!(cells[3..].iter().all(|&cell| cell == ERASED));
    }

    #[test]
    fn an_image_larger_than_the_peripheral_is_refused() {
        let scratch = Scratch::new("large");
        fs::write(scratch.file("image.bin"), vec![0; SIZE + 1]).unwrap();
        let error = construct("image = \"image.bin\"", &scratch).unwrap_err();
        assert!(
            matches!(&error, ParameterError::Invalid { key, .. } if key == "image"),
            "{error}"
        );
    }

    #[test]
    fn a_second_run_cannot_open_the_file() {
        let scratch = Scratch::new("lock");
        let path = scratch.file("cells.bin");
        let first = open(&path, None).unwrap();
        let error = open(&path, None).unwrap_err();
        assert!(matches!(error, OpenError::InUse { .. }), "{error}");
        drop(first);
        open(&path, None).unwrap();
    }

    /// The lock comes first: a run refused the file neither reads nor
    /// replaces it.
    #[test]
    fn a_run_refused_the_lock_leaves_the_file_alone() {
        let scratch = Scratch::new("order");
        let path = scratch.file("cells.bin");
        let first = open(&path, None).unwrap();
        fs::write(&path, [1, 2, 3]).unwrap();
        let error = open(&path, Some(vec![9; SIZE])).unwrap_err();
        assert!(matches!(error, OpenError::InUse { .. }), "{error}");
        assert_eq!(fs::read(&path).unwrap(), [1, 2, 3], "not made whole");
        drop(first);
    }

    /// Runs opening one file at once: one has it, the others are
    /// refused, and the file is whole.
    #[test]
    fn of_runs_opening_the_file_at_once_one_has_it() {
        const RUNS: usize = 8;
        for round in 0..16 {
            let scratch = Scratch::new(&format!("race-{round}"));
            let path = scratch.file("cells.bin");
            let start = Barrier::new(RUNS);
            let opened: Vec<Result<Opened, OpenError>> = thread::scope(|scope| {
                let runs: Vec<_> = (0..RUNS)
                    .map(|_| {
                        scope.spawn(|| {
                            start.wait();
                            open(&path, Some(vec![7; SIZE]))
                        })
                    })
                    .collect();
                runs.into_iter().map(|run| run.join().unwrap()).collect()
            });
            let held: Vec<_> = opened.iter().filter_map(|run| run.as_ref().ok()).collect();
            assert_eq!(held.len(), 1, "round {round}");
            assert_eq!(held[0].0, [7; SIZE]);
            assert!(opened.iter().all(|run| match run {
                Ok(_) => true,
                Err(error) => matches!(error, OpenError::InUse { .. }),
            }));
            assert_eq!(fs::read(&path).unwrap(), [7; SIZE]);
        }
    }

    #[test]
    fn the_work_directory_holds_the_file_without_one() {
        let scratch = Scratch::new("work");
        let parameters = Parameters::default();
        let mut environment = Environment::default();
        environment.work_dir = Some(scratch.0.clone());
        let context = Context::new("u4", &parameters, &environment);
        drop(At28c64b::from_context(&context).unwrap());
        assert_eq!(fs::read(scratch.file("u4.eeprom")).unwrap().len(), SIZE);
    }

    #[test]
    fn the_write_cycle_is_at_most_the_datasheets() {
        let scratch = Scratch::new("range");
        let error = construct("write_cycle_us = 10001", &scratch).unwrap_err();
        assert!(
            matches!(&error, ParameterError::Invalid { key, .. } if key == "write_cycle_us"),
            "{error}"
        );
        let mut peripheral = construct("write_cycle_us = 2000", &scratch).unwrap();
        write(&mut peripheral, 0, 1, Time::ZERO);
        assert_eq!(
            peripheral.next_event(),
            Some(Time::ZERO + BYTE_LOAD + 2000 * PICOSECONDS_PER_MICROSECOND)
        );
    }

    #[test]
    fn a_restore_puts_back_the_cells_the_cycle_and_the_file() {
        let scratch = Scratch::new("restore");
        let mut peripheral = peripheral(vec![0; SIZE]);
        let last = load(&mut peripheral, &ENABLE, Time::ZERO);
        let snapshot = peripheral.save().unwrap();
        let mut copy = construct("file = \"cells.bin\"", &scratch).unwrap();
        copy.restore(&snapshot).unwrap();
        assert_eq!(copy.next_event(), Some(done(last)));
        at(&mut copy, done(last));
        assert!(copy.protected());
        drop(copy);
        assert_eq!(fs::read(scratch.file("cells.bin")).unwrap(), [0; SIZE]);
    }

    /// A sidecar the restore cannot write drops the file, as a run's
    /// failed write does, rather than refusing the snapshot; one that
    /// leaves the protection alone writes no sidecar.
    #[cfg(unix)]
    #[test]
    fn a_restore_that_cannot_write_the_sidecar_drops_the_file() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("read-only");
        let mut peripheral = construct("file = \"cells.bin\"", &scratch).unwrap();
        let unprotected = peripheral.save().unwrap();
        let mut protecting = At28c64b::new(vec![0x5a; SIZE], WRITE_CYCLE, true, None);
        let protected = protecting.save().unwrap();
        let read_only = |mode| {
            fs::set_permissions(&scratch.0, fs::Permissions::from_mode(mode)).unwrap();
        };
        read_only(0o555);
        if fs::write(scratch.file("probe"), []).is_ok() {
            // Permissions do not bind this user (root, say).
            read_only(0o755);
            return;
        }
        peripheral.restore(&unprotected).unwrap();
        assert!(peripheral.file_error().is_none(), "no sidecar written");
        peripheral.restore(&protected).unwrap();
        read_only(0o755);
        assert!(peripheral.file_error().is_some());
        assert!(peripheral.protected());
        assert_eq!(peripheral.cells(), [0x5a; SIZE]);
        assert!(peripheral.store.as_ref().unwrap().file.is_none());
        protecting.restore(&unprotected).unwrap();
        assert!(!protecting.protected());
    }

    /// Power transitions do not disable software data protection
    /// (6.6.2): the next run finds the peripheral as the last left it.
    #[test]
    fn protection_carries_to_the_next_run() {
        let scratch = Scratch::new("sdp");
        let parameters = "file = \"cells.bin\"";
        let mut peripheral = construct(parameters, &scratch).unwrap();
        assert!(!peripheral.protected(), "shipped unprotected");
        let last = load(&mut peripheral, &ENABLE, Time::ZERO);
        at(&mut peripheral, done(last));
        assert!(peripheral.protected());
        drop(peripheral);
        assert_eq!(
            fs::read(scratch.file("cells.bin.sdp")).unwrap(),
            b"protected\n"
        );
        assert_eq!(fs::read(scratch.file("cells.bin")).unwrap().len(), SIZE);
        let mut again = construct(parameters, &scratch).unwrap();
        assert!(again.protected(), "kept across runs");
        let last = load(&mut again, &DISABLE, Time::ZERO);
        at(&mut again, done(last));
        drop(again);
        let protected = construct("file = \"cells.bin\"\nprotected = true", &scratch).unwrap();
        assert!(!protected.protected(), "the file's, not the parameter's");
    }

    /// A file without a sidecar, an 8 KiB dump, starts from `protected`.
    #[test]
    fn a_file_without_its_sidecar_starts_from_the_parameter() {
        let scratch = Scratch::new("bare");
        fs::write(scratch.file("cells.bin"), [0; SIZE]).unwrap();
        let peripheral = construct("file = \"cells.bin\"\nprotected = true", &scratch).unwrap();
        assert!(peripheral.protected());
        drop(peripheral);
        assert!(
            !scratch.file("cells.bin.sdp").exists(),
            "written as it changes"
        );
        fs::write(scratch.file("cells.bin.sdp"), "maybe\n").unwrap();
        let error = construct("file = \"cells.bin\"", &scratch).unwrap_err();
        assert!(
            matches!(&error, ParameterError::Invalid { key, .. } if key == "file"),
            "{error}"
        );
    }

    #[test]
    fn a_restore_refuses_another_twc_and_a_cycle_no_write_loads() {
        let scratch = Scratch::new("refuse");
        let mut peripheral =
            construct("file = \"cells.bin\"\nwrite_cycle_us = 5000", &scratch).unwrap();
        load(&mut peripheral, &[(Offset(0x0040), 0x12)], Time::ZERO);
        let (cells, protected, writing) = (
            peripheral.cells.clone(),
            peripheral.protected,
            peripheral.writing.clone(),
        );
        let other = At28c64b::new(vec![0; SIZE], WRITE_CYCLE, true, None);
        assert!(matches!(
            peripheral.restore(&other.save().unwrap()),
            Err(StateError::Mismatch(_))
        ));
        let cycle = |command: &[(Offset, u8)], data: &[(Offset, u8)]| WriteCycle {
            command: command.to_vec(),
            data: data.iter().copied().collect(),
            last: 0,
            last_at: Time::ZERO,
            toggle: false,
        };
        let invalid = [
            cycle(&[(COMMAND_FIRST, 0x12)], &[]),
            cycle(&ENABLE[..2], &[(Offset(0), 0)]),
            cycle(&[], &[(Offset(SIZE as u16), 0)]),
            cycle(&[], &[(Offset(0), 0), (Offset(0x40), 0)]),
        ];
        for writing in invalid {
            let snapshot = encode(
                VERSION,
                &(
                    peripheral.write_cycle,
                    vec![0_u8; SIZE],
                    true,
                    Some(writing),
                ),
            );
            assert!(matches!(
                peripheral.restore(&snapshot),
                Err(StateError::Invalid(_))
            ));
        }
        let short = encode(
            VERSION,
            &(
                peripheral.write_cycle,
                vec![0_u8; 16],
                true,
                None::<WriteCycle>,
            ),
        );
        assert!(matches!(
            peripheral.restore(&short),
            Err(StateError::Invalid(_))
        ));
        assert_eq!(
            (
                peripheral.cells.clone(),
                peripheral.protected,
                peripheral.writing.clone()
            ),
            (cells, protected, writing)
        );
        drop(peripheral);
        assert_eq!(fs::read(scratch.file("cells.bin")).unwrap(), [ERASED; SIZE]);
        assert!(!scratch.file("cells.bin.sdp").exists());
    }

    #[test]
    fn the_format_matches_its_golden() {
        let mut peripheral = At28c64b::new(
            (0..=u8::MAX).cycle().take(SIZE).collect(),
            WRITE_CYCLE,
            true,
            None,
        );
        let mut bytes = ENABLE.to_vec();
        bytes.extend([(Offset(0x0101), 0x5a), (Offset(0x0100), 0xa5)]);
        load(&mut peripheral, &bytes, Time(1_000));
        read(&mut peripheral, 0);
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "at28c64b",
            &[("at28c64b", VERSION)],
            &peripheral.save().unwrap(),
        );
    }
}
