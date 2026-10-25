//! A board file as TOML holds it, version 1
//! ([`docs/board-files.md`][board-files]): the shapes serde reads, checked
//! no further.  `description` checks and resolves them.  Every table takes
//! a `note` the loader ignores.
//!
//! [board-files]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::peripheral::Lane;
use crate::peripheral::Role;
use crate::signal::Kind;

/// The version this build reads.
pub(crate) const VERSION: i64 = 1;
/// The key of `[chip]` that names the chip.
pub(crate) const TYPE: &str = "type";

/// Only the version, read first, so a file of another version is
/// refused for that and not for keys this build does not know.
#[derive(Debug, Deserialize)]
pub(crate) struct Versioned {
    /// `version`.
    pub(crate) version: Option<toml::Value>,
}

/// The whole file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct File {
    /// `version`, checked already.
    #[serde(rename = "version")]
    pub(crate) _version: i64,
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// `[board]`.
    #[serde(default)]
    pub(crate) board: Board,
    /// `[chip]`: `type`, read here, and the chip's own keys, read by the
    /// chip (`Description::chip_section`).
    #[serde(default)]
    pub(crate) chip: toml::Table,
    /// `[[memory]]`.
    #[serde(default)]
    pub(crate) memory: Vec<Memory>,
    /// `[[decoder]]`.
    #[serde(default)]
    pub(crate) decoder: Vec<Decoder>,
    /// `[[peripheral]]`.
    #[serde(default)]
    pub(crate) peripheral: Vec<Peripheral>,
    /// `[[unconnected]]`.
    #[serde(default)]
    pub(crate) unconnected: Vec<Unconnected>,
    /// `[[net]]`.
    #[serde(default)]
    pub(crate) net: Vec<Net>,
}

/// `[board]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Board {
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// A short name.
    pub(crate) name: Option<String>,
    /// A title.
    pub(crate) title: Option<String>,
    /// The firmware images it takes, the first by default.
    #[serde(default)]
    pub(crate) images: Vec<PathBuf>,
    /// The options a run may enable.
    #[serde(default)]
    pub(crate) options: Vec<String>,
}

/// `[[memory]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Memory {
    /// Its name.
    pub(crate) name: String,
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// The option it needs.
    pub(crate) option: Option<String>,
    /// Its contents' file.
    pub(crate) image: Option<PathBuf>,
    /// Where in the image its contents start.
    pub(crate) image_offset: Option<u64>,
    /// Its bytes.
    pub(crate) size: Option<u64>,
    /// The bytes past the image.
    #[serde(default)]
    pub(crate) fill: u8,
    /// Whether writes reach it.
    #[serde(default)]
    pub(crate) writable: bool,
    /// Its selects; none at a fixed address.
    #[serde(default)]
    pub(crate) attach: Vec<Attachment>,
    /// Its fixed address, emulator-only, in place of `attach`.
    pub(crate) base: Option<u64>,
    /// A fixed memory's port width in bits, 8 or 16 (16 if absent).
    pub(crate) port: Option<u8>,
    /// A fixed memory's wait states, 0 to 13; fast termination if
    /// absent.
    pub(crate) wait_states: Option<u8>,
    /// The resets that reload its contents, none if absent.
    pub(crate) reload: Option<Vec<Cause>>,
}

/// The address lines of an attachment: a count from ADDR0, or a list,
/// the peripheral's A0 first.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum Lines {
    /// ADDR0 to ADDR`n - 1`.
    Count(u8),
    /// These, in order.
    List(Vec<u8>),
}

/// A role as the file writes it: a role, or a lane alone, which is
/// that lane read and written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FileRole {
    /// `read`.
    Read,
    /// `write`.
    Write,
    /// `read-write`.
    ReadWrite,
    /// `iack`.
    Iack,
    /// `upper`: `lane = "upper"`.
    Upper,
    /// `lower`: `lane = "lower"`.
    Lower,
}

impl FileRole {
    /// The role, and the lane it implies.
    pub(crate) const fn split(self) -> (Role, Option<Lane>) {
        match self {
            Self::Read => (Role::Read, None),
            Self::Write => (Role::Write, None),
            Self::ReadWrite => (Role::ReadWrite, None),
            Self::Iack => (Role::Iack, None),
            Self::Upper => (Role::ReadWrite, Some(Lane::Upper)),
            Self::Lower => (Role::ReadWrite, Some(Lane::Lower)),
        }
    }
}

/// One attachment: exactly one of `select`, `decoder` and `qspi`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Attachment {
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// A chip-select pin.
    pub(crate) select: Option<String>,
    /// A decoder's name.
    pub(crate) decoder: Option<String>,
    /// The decoder's output.
    pub(crate) output: Option<u32>,
    /// A QSPI PCS code.
    pub(crate) qspi: Option<u8>,
    /// The PCS pins the code compares.
    pub(crate) mask: Option<u8>,
    /// Its role.
    pub(crate) role: Option<FileRole>,
    /// Its lane.
    pub(crate) lane: Option<Lane>,
    /// Address lines wired.
    pub(crate) lines: Option<Lines>,
    /// Added to the address.
    pub(crate) offset: Option<u32>,
    /// On no select: the first address of the window its own decode
    /// takes.
    pub(crate) base: Option<u32>,
    /// The window's bytes.
    pub(crate) size: Option<u32>,
    /// On no select: the interrupt level whose acknowledges it takes.
    pub(crate) iack: Option<u8>,
}

/// What a decoder does with a code nothing is attached to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Undecoded {
    /// Reads 0; writes go nowhere.
    #[default]
    OpenBus,
    /// BERR.
    BusError,
}

/// A level an input is tied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Tied {
    /// Low: 0.
    Low,
    /// High: 1.
    High,
}

/// A decoder input: an ADDR line or a tied level.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
pub(crate) enum Input {
    /// ADDR`n`.
    Line(u8),
    /// A level.
    Tied(Tied),
}

/// `[[decoder]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decoder {
    /// Its name.
    pub(crate) name: String,
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// The option it needs.
    pub(crate) option: Option<String>,
    /// Its enable.
    pub(crate) select: String,
    /// Its inputs, least significant first.
    pub(crate) address_bits: Vec<Input>,
    /// A code nothing is attached to.
    #[serde(default)]
    pub(crate) undecoded: Undecoded,
    /// Codes that select another code's output.
    #[serde(default)]
    pub(crate) alias: Alias,
}

/// A decoder's aliases, with its note.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Alias {
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// Each code that selects another code's output, and that output.
    #[serde(flatten)]
    pub(crate) codes: BTreeMap<String, u32>,
}

/// A reset cause as the file names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Cause {
    /// External.
    External,
    /// Power-on.
    PowerOn,
    /// Watchdog.
    Watchdog,
    /// Halt monitor.
    HaltMonitor,
    /// The `reset` instruction.
    System,
    /// Loss of clock.
    LossOfClock,
    /// The test submodule.
    Test,
}

/// The DSACK pins an externally terminated peripheral drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum Dsack {
    /// DSACK0: an 8-bit port.
    #[serde(rename = "DSACK0")]
    Dsack0,
    /// DSACK1: a 16-bit port.
    #[serde(rename = "DSACK1")]
    Dsack1,
    /// Both.
    #[serde(rename = "both")]
    Both,
    /// Not known.
    #[serde(rename = "open")]
    Open,
}

/// `[[peripheral]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Peripheral {
    /// Its name.
    pub(crate) name: String,
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// The option it needs.
    pub(crate) option: Option<String>,
    /// Its type.
    #[serde(rename = "type")]
    pub(crate) kind: String,
    /// The chip's marking or part number as fitted, for people; no
    /// peripheral reads it.
    pub(crate) marking: Option<String>,
    /// Its parameters.
    #[serde(default)]
    pub(crate) parameters: toml::Table,
    /// Its attachments.
    #[serde(default)]
    pub(crate) attach: Vec<Attachment>,
    /// Its IRQ pin.
    pub(crate) irq: Option<u8>,
    /// The resets that reach it.
    pub(crate) reset: Option<Vec<Cause>>,
    /// The DSACK pins it drives.
    pub(crate) dsack: Option<Dsack>,
}

/// `[[unconnected]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Unconnected {
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// The option it needs.
    pub(crate) option: Option<String>,
    /// The select.
    pub(crate) select: String,
}

/// A net's type: a kind of signal with one driver, or open drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum NetType {
    /// `logic`.
    Logic,
    /// `edge`.
    Edge,
    /// `clock`.
    Clock,
    /// `serial`.
    Serial,
    /// `analog`.
    Analog,
    /// `bus`.
    Bus,
    /// Open drain, pulled up: a logic net with several drivers.
    WiredAnd,
}

impl NetType {
    /// The kind it carries, and whether it is wired.
    pub(crate) const fn kind(self) -> (Kind, bool) {
        match self {
            Self::Logic => (Kind::Logic, false),
            Self::Edge => (Kind::Edge, false),
            Self::Clock => (Kind::Clock, false),
            Self::Serial => (Kind::Serial, false),
            Self::Analog => (Kind::Analog, false),
            Self::Bus => (Kind::Bus, false),
            Self::WiredAnd => (Kind::Logic, true),
        }
    }
}

/// One endpoint or several.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Endpoints {
    /// One.
    One(String),
    /// Several.
    Many(Vec<String>),
}

/// `[[net]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Net {
    /// Its name.
    pub(crate) name: String,
    /// Ignored.
    #[serde(rename = "note")]
    pub(crate) _note: Option<String>,
    /// The option it needs.
    pub(crate) option: Option<String>,
    /// Its type.
    #[serde(rename = "type")]
    pub(crate) kind: NetType,
    /// Its drivers.
    pub(crate) from: Endpoints,
    /// Its sinks.
    pub(crate) to: Vec<String>,
}
