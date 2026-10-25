//! A board file read and checked against everything but the registry.
//!
//! The chip's section, the memories with their contents, the
//! decoders, the peripherals' wiring and the nets' endpoints, every name
//! resolved ([`docs/board-files.md`][board-files]).
//!
//! [board-files]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::memory::PAGE_BYTES;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::chip_selects::Termination;
use mc683xx_modules::sim::reset::ResetScope;

use crate::endpoint::Endpoint;
use crate::error::LoadError;
use crate::error::SyntaxError;
use crate::peripheral::Lane;
use crate::peripheral::Role;
use crate::registry::ParameterTable;
use crate::registry::ParameterValue;
use crate::registry::table_from_toml;
use crate::schema;
use crate::schema::Attachment;
use crate::schema::Cause;
use crate::schema::Dsack;
use crate::schema::Endpoints;
use crate::schema::FileRole;
use crate::schema::Input;
use crate::schema::Lines;
use crate::schema::Tied;
use crate::schema::Undecoded;
use crate::signal::Kind;

/// The address lines of the SIM's parts, ADDR\[23:0\] (SIMRM 5.1.1).
const ADDRESS_LINES: u8 = 24;
/// The bytes ADDR\[23:0\] reach, 16 Mbytes: the most a memory holds.
pub(crate) const ADDRESS_SPACE: usize = 1 << ADDRESS_LINES;
/// The most inputs a decoder may have: a 74AC154's four, and more.
const DECODER_INPUTS: usize = 8;
/// PCS\[3:0\], the QSPI's chip-select pins (QSMRM 4.3.6.3).
pub(crate) const PCS_MASK: u8 = 0xf;
/// The key every table takes and the loader ignores, a peripheral's
/// `parameters` too.
const NOTE: &str = "note";

/// What the run decides about the file: its options, image
/// replacements and parameters.
///
/// It may gain fields: build one from [`Options::default()`] and set
/// those it needs (`let mut x = Options::default(); x.firmware = ...;`).
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Options {
    /// The options enabled: entries `when` names are on the board only
    /// if their option is here.
    pub enabled: BTreeSet<String>,
    /// The firmware image to run, one of `[board] images` or another:
    /// it replaces every memory's image that is one of them.
    pub firmware: Option<PathBuf>,
    /// Images by memory name, replacing the file's and `firmware`.
    pub images: BTreeMap<String, PathBuf>,
    /// Parameters by peripheral name, each key replacing the file's or
    /// added to them, and checked as the file's are: by the type's
    /// constructor, a key it does not read an error.  A peripheral the file
    /// does not name is an error; one an option leaves off is not.
    pub parameters: BTreeMap<String, ParameterTable>,
}

impl Options {
    /// Sets parameter `key` of `peripheral` to `value`.
    pub fn set_parameter(
        &mut self,
        peripheral: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<ParameterValue>,
    ) {
        self.parameters
            .entry(peripheral.into())
            .or_default()
            .insert(key.into(), value.into());
    }

    /// Sets a parameter from `assignment`, `PERIPHERAL.KEY=VALUE` (the
    /// runner's `--set`): `VALUE` a TOML value (`3`, `0x10`, `true`,
    /// `"text"`, `[1, 2]`), or else taken as a bare string.
    ///
    /// # Errors
    ///
    /// An `assignment` not of that form, its peripheral or key empty
    /// ([`LoadError::Setting`]).
    pub fn set(&mut self, assignment: &str) -> Result<(), LoadError> {
        let malformed = || LoadError::Setting(assignment.to_owned());
        let (path, text) = assignment.split_once('=').ok_or_else(malformed)?;
        let (peripheral, key) = path.split_once('.').ok_or_else(malformed)?;
        if peripheral.is_empty() || key.is_empty() {
            return Err(malformed());
        }
        let value = toml::from_str::<toml::Table>(&format!("value = {text}"))
            .ok()
            .and_then(|mut table| table.remove("value"))
            .map_or_else(|| ParameterValue::from(text), ParameterValue::from_toml);
        self.set_parameter(peripheral, key, value);
        Ok(())
    }
}

/// The board's firmware images and the run's choice of one.
#[derive(Debug, Clone, Copy)]
struct Firmware<'a> {
    /// `[board] images`, as the file writes them.
    images: &'a [PathBuf],
    /// The run's choice.
    chosen: Option<&'a Path>,
}

/// A memory, its contents read.
#[derive(Debug, Clone)]
pub(crate) struct MemorySpec {
    /// Its name.
    pub(crate) name: String,
    /// Its contents.
    pub(crate) bytes: Vec<u8>,
    /// Whether writes reach it.
    pub(crate) writable: bool,
    /// Its selects; none at a fixed address.
    pub(crate) pins: Vec<ChipSelect>,
    /// Its fixed address, emulator-only, in place of selects.
    pub(crate) base: Option<u32>,
    /// A fixed memory's port and termination; a select's come from its
    /// option register.
    pub(crate) timing: (PortSize, Termination),
    /// The resets that reload it.
    pub(crate) reload: ResetScope,
}

/// A decoder input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoderInput {
    /// ADDR`n`.
    Line(u8),
    /// A tied level.
    Tied(bool),
}

/// A decoder.
#[derive(Debug, Clone)]
pub(crate) struct DecoderSpec {
    /// Its name.
    pub(crate) name: String,
    /// Its enable.
    pub(crate) select: ChipSelect,
    /// Its inputs, least significant first.
    pub(crate) inputs: Vec<DecoderInput>,
    /// The output each code selects, by code.
    pub(crate) outputs: Vec<u32>,
    /// A code with nothing attached.
    pub(crate) undecoded: Undecoded,
}

/// Where an attachment reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// A chip-select pin.
    Select(ChipSelect),
    /// A decoder's output.
    Decoder {
        /// The decoder, by index.
        decoder: usize,
        /// The output.
        output: u32,
    },
    /// The QSPI, under a PCS code.
    Qspi {
        /// The levels.
        code: u8,
        /// The pins compared.
        mask: u8,
    },
    /// No select: the cycles the peripheral's own decode takes.
    Bus(BusDecode),
}

/// A peripheral's attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachmentSpec {
    /// Where.
    pub(crate) target: Target,
    /// Its role.
    pub(crate) role: Role,
    /// Its lane.
    pub(crate) lane: Lane,
    /// The ADDR lines on the peripheral's A0 up.
    pub(crate) lines: Vec<u8>,
    /// Added to the address.
    pub(crate) offset: u32,
}

/// A peripheral.
#[derive(Debug, Clone)]
pub(crate) struct PeripheralSpec {
    /// Its name.
    pub(crate) name: String,
    /// Its type.
    pub(crate) kind: String,
    /// The chip's marking or part number as fitted.
    pub(crate) marking: Option<String>,
    /// Its parameters.
    pub(crate) parameters: ParameterTable,
    /// Its attachments.
    pub(crate) attachments: Vec<AttachmentSpec>,
    /// Its IRQ pin.
    pub(crate) irq: Option<IrqPin>,
    /// The resets that reach it.
    pub(crate) reset: ResetScope,
    /// The DSACK pins it drives, if known.
    pub(crate) dsack: Option<mc683xx_modules::sim::chip_selects::Dsack>,
}

/// A net.
#[derive(Debug, Clone)]
pub(crate) struct NetSpec {
    /// Its name.
    pub(crate) name: String,
    /// Its type.
    pub(crate) kind: Kind,
    /// Open drain.
    pub(crate) wired: bool,
    /// Its drivers.
    pub(crate) from: Vec<Endpoint>,
    /// Its sinks.
    pub(crate) to: Vec<Endpoint>,
}

/// A board file, checked: what [`Board::load`] builds from.
///
/// [`Board::load`]: crate::board::Board::load
#[derive(Debug, Clone)]
pub struct Description {
    /// The file's directory.
    pub(crate) directory: PathBuf,
    /// `[board] name`.
    name: Option<String>,
    /// `[board] title`.
    title: Option<String>,
    /// The firmware images it takes, relative to the working directory.
    images: Vec<PathBuf>,
    /// The options a run may enable.
    options: Vec<String>,
    /// `[chip] type`.
    chip_type: String,
    /// The file's text, for the chip to read its `[chip]` table from
    /// with its own keys, their lines kept.
    text: String,
    /// The memories.
    pub(crate) memories: Vec<MemorySpec>,
    /// The decoders.
    pub(crate) decoders: Vec<DecoderSpec>,
    /// The peripherals.
    pub(crate) peripherals: Vec<PeripheralSpec>,
    /// The nets.
    pub(crate) nets: Vec<NetSpec>,
}

impl Description {
    /// Sets parameter `key` of `peripheral` to `value`, as
    /// [`Options::set_parameter`] does before the file is read: checked
    /// by the type's constructor when the board loads.
    ///
    /// # Errors
    ///
    /// A description with no peripheral `peripheral` ([`LoadError::Peripheral`]).
    pub fn set_parameter(
        &mut self,
        peripheral: &str,
        key: impl Into<String>,
        value: impl Into<ParameterValue>,
    ) -> Result<(), LoadError> {
        let spec = self
            .peripherals
            .iter_mut()
            .find(|spec| spec.name == peripheral)
            .ok_or_else(|| LoadError::Peripheral(peripheral.to_owned()))?;
        spec.parameters.insert(key.into(), value.into());
        Ok(())
    }

    /// The board file at `path`, its images read and every name
    /// resolved.
    ///
    /// # Errors
    ///
    /// A file that cannot be read ([`LoadError::Read`]), or what
    /// [`Description::parse`] fails with.
    pub fn read(path: &Path, options: &Options) -> Result<Self, LoadError> {
        let text = fs::read_to_string(path).map_err(|error| LoadError::Read {
            path: path.to_owned(),
            error,
        })?;
        let directory = path.parent().map(Path::to_owned).unwrap_or_default();
        Self::parse(&text, &directory, options)
    }

    /// A board file's `text`, its paths relative to `directory`.
    ///
    /// # Errors
    ///
    /// TOML that does not parse or is not the schema's
    /// ([`LoadError::Syntax`]), a version other than this build's
    /// ([`LoadError::Version`]), an image that cannot be read
    /// ([`LoadError::Image`]), `options` the file does not declare or
    /// name ([`LoadError::Option`], [`LoadError::Peripheral`]), or anything
    /// else wrong with the board the file describes: each a
    /// [`LoadError`] naming it.
    pub fn parse(text: &str, directory: &Path, options: &Options) -> Result<Self, LoadError> {
        let versioned: schema::Versioned = toml::from_str(text).map_err(SyntaxError::from_toml)?;
        match versioned.version {
            Some(toml::Value::Integer(schema::VERSION)) => {}
            Some(other) => return Err(LoadError::Version(other.to_string())),
            None => return Err(LoadError::Version("missing".to_owned())),
        }
        let file: schema::File = toml::from_str(text).map_err(SyntaxError::from_toml)?;
        let board = file.board;
        let declared: BTreeSet<&str> = board.options.iter().map(String::as_str).collect();
        if let Some(option) = options
            .enabled
            .iter()
            .find(|option| !declared.contains(option.as_str()))
        {
            return Err(LoadError::Option(option.clone()));
        }
        let entries = [
            file.memory
                .iter()
                .map(|entry| &entry.option)
                .collect::<Vec<_>>(),
            file.decoder.iter().map(|entry| &entry.option).collect(),
            file.peripheral.iter().map(|entry| &entry.option).collect(),
            file.unconnected.iter().map(|entry| &entry.option).collect(),
            file.net.iter().map(|entry| &entry.option).collect(),
        ];
        if let Some(option) = entries
            .iter()
            .flatten()
            .filter_map(|option| option.as_ref())
            .find(|option| !declared.contains(option.as_str()))
        {
            return Err(LoadError::Option(option.clone()));
        }
        let on = |option: &Option<String>| {
            option
                .as_ref()
                .is_none_or(|option| options.enabled.contains(option))
        };
        let memory: Vec<_> = file
            .memory
            .into_iter()
            .filter(|entry| on(&entry.option))
            .collect();
        let decoder: Vec<_> = file
            .decoder
            .into_iter()
            .filter(|entry| on(&entry.option))
            .collect();
        let (peripheral, absent): (Vec<_>, Vec<_>) = file
            .peripheral
            .into_iter()
            .partition(|entry| on(&entry.option));
        let absent: BTreeSet<String> = absent.into_iter().map(|entry| entry.name).collect();
        let unconnected: Vec<_> = file
            .unconnected
            .into_iter()
            .filter(|entry| on(&entry.option))
            .collect();
        let net: Vec<_> = file
            .net
            .into_iter()
            .filter(|entry| on(&entry.option))
            .collect();
        let mut names = BTreeSet::new();
        for name in memory
            .iter()
            .map(|entry| &entry.name)
            .chain(decoder.iter().map(|entry| &entry.name))
            .chain(peripheral.iter().map(|entry| &entry.name))
            .chain(net.iter().map(|entry| &entry.name))
        {
            if name.is_empty() || name.contains('.') || name == "chip" || name == "external" {
                return Err(LoadError::Name(name.clone()));
            }
            if !names.insert(name.as_str()) {
                return Err(LoadError::Duplicate(name.clone()));
            }
        }
        let chip_type = match file.chip.get(schema::TYPE) {
            Some(toml::Value::String(kind)) => kind.clone(),
            Some(other) => other.to_string(),
            None => return Err(LoadError::NoChip),
        };
        let mut users = Users::default();
        for entry in &unconnected {
            let select = select("unconnected", &entry.select)?;
            users.add(Point::Select(select), "unconnected".to_owned(), None)?;
        }
        let firmware = Firmware {
            images: &board.images,
            chosen: options.firmware.as_deref(),
        };
        let memories = memory
            .iter()
            .map(|entry| read_memory(entry, directory, options, &firmware, &mut users))
            .collect::<Result<Vec<_>, _>>()?;
        check_fixed(&memories)?;
        let decoders = decoder
            .iter()
            .map(|entry| read_decoder(entry, &mut users))
            .collect::<Result<Vec<_>, _>>()?;
        let mut peripherals = peripheral
            .into_iter()
            .map(|entry| read_peripheral(entry, &decoders, &mut users))
            .collect::<Result<Vec<_>, _>>()?;
        for (name, parameters) in &options.parameters {
            if let Some(peripheral) = peripherals
                .iter_mut()
                .find(|peripheral| peripheral.name == *name)
            {
                peripheral.parameters.extend(parameters.clone());
            } else if !absent.contains(name) {
                return Err(LoadError::Peripheral(name.clone()));
            }
        }
        let peripheral_names: BTreeSet<&str> = peripherals
            .iter()
            .map(|peripheral| peripheral.name.as_str())
            .collect();
        let mut seen = Seen::default();
        let nets = net
            .into_iter()
            .map(|entry| read_net(entry, &peripheral_names, &absent, &mut seen))
            .filter_map(Result::transpose)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            directory: directory.to_owned(),
            name: board.name,
            title: board.title,
            images: board
                .images
                .iter()
                .map(|image| directory.join(image))
                .collect(),
            options: board.options,
            chip_type,
            text: text.to_owned(),
            memories,
            decoders,
            peripherals,
            nets,
        })
    }

    /// `[chip] type`, the chip the board is built around: what a host
    /// chooses the [`Chip`] to load the board with by.
    ///
    /// [`Chip`]: crate::chip::Chip
    #[must_use]
    pub fn chip_type(&self) -> &str {
        &self.chip_type
    }

    /// The `[chip]` table as chip `expected`'s section, its own keys
    /// checked by its type, with the line of any that is wrong.  An
    /// absent table is an empty one.
    pub(crate) fn chip_section<S: DeserializeOwned>(
        &self,
        expected: &'static str,
    ) -> Result<S, LoadError> {
        if self.chip_type != expected {
            return Err(LoadError::ChipType {
                found: self.chip_type.clone(),
                expected,
            });
        }
        /// The file as the chip reads it: its table alone.
        #[derive(Deserialize)]
        struct ChipOnly<S> {
            /// `[chip]`.
            chip: Option<S>,
        }
        let only: ChipOnly<S> = toml::from_str(&self.text).map_err(SyntaxError::from_toml)?;
        match only.chip {
            Some(section) => Ok(section),
            None => toml::Value::Table(toml::Table::new())
                .try_into()
                .map_err(SyntaxError::from_toml),
        }
    }

    /// `[board] name`, if given.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// `[board] title`, if given.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The firmware images the board takes, the default first.
    #[must_use]
    pub fn images(&self) -> &[PathBuf] {
        &self.images
    }

    /// The options a run may enable.
    #[must_use]
    pub fn options(&self) -> &[String] {
        &self.options
    }

    /// The memories' names, in file order.
    pub fn memories(&self) -> impl Iterator<Item = &str> {
        self.memories.iter().map(|memory| memory.name.as_str())
    }

    /// The peripherals' names and types, in file order.
    pub fn peripherals(&self) -> impl Iterator<Item = (&str, &str)> {
        self.peripherals
            .iter()
            .map(|peripheral| (peripheral.name.as_str(), peripheral.kind.as_str()))
    }

    /// The marking of `peripheral`, if the file gives one.
    #[must_use]
    pub fn marking(&self, peripheral: &str) -> Option<&str> {
        self.peripherals
            .iter()
            .find(|spec| spec.name == peripheral)?
            .marking
            .as_deref()
    }
}

/// IRQ pin `level`, 1 to 7.
fn irq_pin(owner: &str, level: u8) -> Result<IrqPin, LoadError> {
    IrqPin::new(level).ok_or_else(|| LoadError::Level {
        owner: owner.to_owned(),
        level,
    })
}

/// The select named `pin`.
fn select(owner: &str, pin: &str) -> Result<ChipSelect, LoadError> {
    pin.parse().map_err(|_| LoadError::Pin {
        owner: owner.to_owned(),
        pin: pin.to_owned(),
    })
}

/// A select, or a decoder's output: a place on the bus users share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Point {
    /// A chip-select pin.
    Select(ChipSelect),
    /// A decoder's output: the decoder's index and the output.
    Output(usize, u32),
}

/// Who uses each point: a name, and a peripheral's role and lane, or
/// [`None`] for a user that takes the point whole.
#[derive(Debug, Default)]
struct Users(BTreeMap<Point, Vec<User>>);

/// A user of a point: its name, and a peripheral's role and lane.
type User = (String, Option<(Role, Lane)>);

impl Users {
    /// `owner` uses `point`, with a peripheral's `split`, if it shares it
    /// with no one who takes the same cycles.
    fn add(
        &mut self,
        point: Point,
        owner: String,
        split: Option<(Role, Lane)>,
    ) -> Result<(), LoadError> {
        let users = self.0.entry(point).or_default();
        if let Some((first, _)) = users.iter().find(|(name, other)| {
            *name != owner
                && match (split, other) {
                    (Some(mine), Some(theirs)) => overlaps(mine, *theirs),
                    _ => true,
                }
        }) {
            return Err(LoadError::Shared {
                point: match point {
                    Point::Select(select) => select.to_string(),
                    Point::Output(_, output) => format!("decoder output {output}"),
                },
                first: first.clone(),
                second: owner,
            });
        }
        users.push((owner, split));
        Ok(())
    }
}

/// Whether two attachments on one point take some cycle on one lane.
fn overlaps((role, lane): (Role, Lane), (other_role, other_lane): (Role, Lane)) -> bool {
    let roles = match (role, other_role) {
        (Role::Read, Role::Write) | (Role::Write, Role::Read) => false,
        (Role::Iack, other) | (other, Role::Iack) => other == Role::Iack,
        _ => true,
    };
    let lanes = lane == Lane::Both || other_lane == Lane::Both || lane == other_lane;
    roles && lanes
}

/// An attachment's error.
fn attachment_error(owner: &str, index: usize, reason: impl Into<String>) -> LoadError {
    LoadError::Attachment {
        owner: owner.to_owned(),
        index,
        reason: reason.into(),
    }
}

/// A memory, its image read.
fn read_memory(
    entry: &schema::Memory,
    directory: &Path,
    options: &Options,
    firmware: &Firmware<'_>,
    users: &mut Users,
) -> Result<MemorySpec, LoadError> {
    let name = &entry.name;
    let image = match (options.images.get(name), &entry.image) {
        (Some(path), _) => Some(path.clone()),
        (None, Some(path)) => Some(
            firmware
                .chosen
                .filter(|_| firmware.images.contains(path))
                .map_or_else(|| directory.join(path), Path::to_owned),
        ),
        (None, None) => None,
    };
    let size_error = |reason: String| LoadError::Size {
        memory: name.clone(),
        reason,
    };
    if let Some(size) = entry.size
        && size > ADDRESS_SPACE as u64
    {
        return Err(size_error(format!(
            "{size:#x} bytes is more than ADDR[23:0] reach"
        )));
    }
    let image = image
        .map(|path| {
            fs::read(&path).map_err(|error| LoadError::Image {
                memory: name.clone(),
                path,
                error,
            })
        })
        .transpose()?;
    let image = match (image, entry.image_offset) {
        (Some(image), Some(offset)) => {
            let start = usize::try_from(offset)
                .ok()
                .filter(|&start| start <= image.len())
                .ok_or_else(|| size_error(format!("its image has no offset {offset:#x}")))?;
            let end = entry.size.map_or(image.len(), |size| {
                usize::try_from(size).map_or(image.len(), |size| image.len().min(start + size))
            });
            Some(image[start..end].to_vec())
        }
        (None, Some(_)) => return Err(size_error("image_offset needs an image".to_owned())),
        (image, None) => image,
    };
    let size = match (entry.size, &image) {
        (Some(size), _) => {
            usize::try_from(size).map_err(|_| size_error(format!("{size:#x} bytes is too many")))?
        }
        (None, Some(image)) if image.len() <= ADDRESS_SPACE => image.len(),
        (None, Some(image)) => {
            return Err(size_error(format!(
                "its image's {:#x} bytes are more than ADDR[23:0] reach",
                image.len()
            )));
        }
        (None, None) => return Err(size_error("give an image or a size".to_owned())),
    };
    if size == 0 || !size.is_multiple_of(PAGE_BYTES as usize) {
        return Err(size_error(format!(
            "{size:#x} bytes is not a whole number of {PAGE_BYTES:#x}-byte pages"
        )));
    }
    let mut bytes = image.unwrap_or_default();
    if bytes.len() > size {
        return Err(size_error(format!(
            "its image's {:#x} bytes do not fit {size:#x}",
            bytes.len()
        )));
    }
    bytes.resize(size, entry.fill);
    let reload = entry.reload.as_deref().map_or(ResetScope::NONE, scope);
    if let Some(base) = entry.base {
        if !entry.attach.is_empty() {
            return Err(attachment_error(
                name,
                0,
                "a memory takes a select or a base, not both",
            ));
        }
        let base_error = |reason: &str| LoadError::Base {
            memory: name.clone(),
            reason: format!("base {base:#x} {reason}"),
        };
        if !base.is_multiple_of(PAGE_BYTES.into()) {
            return Err(base_error("is not on a 2-Kbyte page boundary"));
        }
        let past = || base_error("puts it past ADDR[23:0]");
        if base
            .checked_add(size as u64)
            .is_none_or(|end| end > ADDRESS_SPACE as u64)
        {
            return Err(past());
        }
        let base = u32::try_from(base).map_err(|_| past())?;
        let timing_error = |reason: String| LoadError::Base {
            memory: name.clone(),
            reason,
        };
        let port = match entry.port {
            None | Some(16) => PortSize::Bits16,
            Some(8) => PortSize::Bits8,
            Some(other) => return Err(timing_error(format!("port {other} is not 8 or 16"))),
        };
        let termination = match entry.wait_states {
            None => Termination::Fast,
            Some(wait) if wait <= Termination::MOST_WAIT_STATES => Termination::WaitStates(wait),
            Some(wait) => {
                return Err(timing_error(format!(
                    "wait_states {wait} is more than {}",
                    Termination::MOST_WAIT_STATES
                )));
            }
        };
        return Ok(MemorySpec {
            name: name.clone(),
            bytes,
            writable: entry.writable,
            pins: Vec::new(),
            base: Some(base),
            timing: (port, termination),
            reload,
        });
    }
    if entry.port.is_some() || entry.wait_states.is_some() {
        return Err(attachment_error(
            name,
            0,
            "port and wait_states are a fixed memory's: a select's come from its option register",
        ));
    }
    if entry.attach.is_empty() {
        return Err(attachment_error(
            name,
            0,
            "a memory needs a select or a base",
        ));
    }
    let mut pins = Vec::new();
    for (index, attachment) in entry.attach.iter().enumerate() {
        let Attachment {
            select: Some(pin),
            decoder: None,
            output: None,
            qspi: None,
            mask: None,
            lines: None,
            offset: None,
            ..
        } = attachment
        else {
            return Err(attachment_error(
                name,
                index,
                "a memory takes only select, role and lane",
            ));
        };
        let (role, _) = attachment
            .role
            .map_or((Role::ReadWrite, None), FileRole::split);
        if role == Role::Iack {
            return Err(attachment_error(
                name,
                index,
                "a memory answers no acknowledge: wire a peripheral",
            ));
        }
        let select = select(name, pin)?;
        users.add(Point::Select(select), name.clone(), None)?;
        if !pins.contains(&select) {
            pins.push(select);
        }
    }
    Ok(MemorySpec {
        name: name.clone(),
        bytes,
        writable: entry.writable,
        pins,
        base: None,
        timing: (PortSize::Bits16, Termination::Fast),
        reload,
    })
}

/// Whether the fixed memories are each clear of the others.
fn check_fixed(memories: &[MemorySpec]) -> Result<(), LoadError> {
    let span = |memory: &MemorySpec| {
        memory
            .base
            .map(|base| (u64::from(base), u64::from(base) + memory.bytes.len() as u64))
    };
    for (index, memory) in memories.iter().enumerate() {
        let Some((start, end)) = span(memory) else {
            continue;
        };
        if let Some(other) = memories[..index]
            .iter()
            .find(|other| span(other).is_some_and(|(from, to)| start < to && from < end))
        {
            return Err(LoadError::Base {
                memory: memory.name.clone(),
                reason: format!("overlaps {}", other.name),
            });
        }
    }
    Ok(())
}

/// The resets `causes` name.
fn scope(causes: &[Cause]) -> ResetScope {
    causes.iter().fold(ResetScope::NONE, |scope, cause| {
        scope.with(match cause {
            Cause::External => ResetCause::External,
            Cause::PowerOn => ResetCause::PowerOn,
            Cause::Watchdog => ResetCause::Watchdog,
            Cause::HaltMonitor => ResetCause::HaltMonitor,
            Cause::System => ResetCause::System,
            Cause::LossOfClock => ResetCause::LossOfClock,
            Cause::Test => ResetCause::Test,
        })
    })
}

/// A decoder.
fn read_decoder(entry: &schema::Decoder, users: &mut Users) -> Result<DecoderSpec, LoadError> {
    let error = |reason: String| LoadError::Decoder {
        decoder: entry.name.clone(),
        reason,
    };
    if entry.address_bits.is_empty() || entry.address_bits.len() > DECODER_INPUTS {
        return Err(error(format!("it needs 1 to {DECODER_INPUTS} inputs")));
    }
    let inputs = entry
        .address_bits
        .iter()
        .map(|input| match *input {
            Input::Line(line) if line < ADDRESS_LINES => Ok(DecoderInput::Line(line)),
            Input::Line(line) => Err(error(format!("no ADDR{line}"))),
            Input::Tied(tied) => Ok(DecoderInput::Tied(tied == Tied::High)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let codes = 1_u32 << inputs.len();
    let mut outputs: Vec<u32> = (0..codes).collect();
    for (code, &output) in &entry.alias.codes {
        let code = parse_integer(code)
            .filter(|&code| code < codes)
            .ok_or_else(|| error(format!("no code {code:?} to alias")))?;
        if output >= codes {
            return Err(error(format!("no output {output}")));
        }
        outputs[code as usize] = output;
    }
    let select = select(&entry.name, &entry.select)?;
    users.add(Point::Select(select), entry.name.clone(), None)?;
    Ok(DecoderSpec {
        name: entry.name.clone(),
        select,
        inputs,
        outputs,
        undecoded: entry.undecoded,
    })
}

/// An integer as TOML writes one in a key: decimal, `0x` or `0b`.
fn parse_integer(text: &str) -> Option<u32> {
    if let Some(hex) = text.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).ok()
    } else if let Some(binary) = text.strip_prefix("0b") {
        u32::from_str_radix(binary, 2).ok()
    } else {
        text.parse().ok()
    }
}

/// A peripheral's attachment.
fn read_attachment(
    owner: &str,
    index: usize,
    attachment: &Attachment,
    decoders: &[DecoderSpec],
    users: &mut Users,
) -> Result<AttachmentSpec, LoadError> {
    let error = |reason: &str| attachment_error(owner, index, reason);
    let (role, implied) = attachment
        .role
        .map_or((Role::ReadWrite, None), FileRole::split);
    let lane = match (implied, attachment.lane) {
        (Some(_), Some(_)) => return Err(error("role upper or lower is a lane: give one")),
        (implied, lane) => implied.or(lane).unwrap_or_default(),
    };
    let lines = match &attachment.lines {
        None => (0..ADDRESS_LINES).collect(),
        Some(Lines::Count(count)) if *count <= ADDRESS_LINES => (0..*count).collect(),
        Some(Lines::Count(_)) => return Err(error("there are 24 address lines")),
        Some(Lines::List(lines)) => {
            if lines.iter().any(|&line| line >= ADDRESS_LINES) {
                return Err(error("there are 24 address lines, ADDR0 to ADDR23"));
            }
            if lines.len() > usize::from(ADDRESS_LINES) {
                return Err(error("a peripheral has at most 24 address lines"));
            }
            lines.clone()
        }
    };
    let offset = attachment.offset.unwrap_or(0);
    let on_bus =
        attachment.base.is_some() || attachment.size.is_some() || attachment.iack.is_some();
    let target = match (
        &attachment.select,
        &attachment.decoder,
        attachment.output,
        attachment.qspi,
    ) {
        (None, None, None, None) if on_bus && attachment.mask.is_none() => {
            bus_decode(attachment, role).map_err(&error)?
        }
        _ if on_bus => {
            return Err(error(
                "base and size, or iack, put it on no select: give no select, decoder or qspi",
            ));
        }
        (Some(pin), None, None, None) if attachment.mask.is_none() => {
            Target::Select(select(owner, pin)?)
        }
        (None, Some(decoder), Some(output), None) if attachment.mask.is_none() => {
            let (index, spec) = decoders
                .iter()
                .enumerate()
                .find(|(_, spec)| &spec.name == decoder)
                .ok_or_else(|| error(&format!("no decoder {decoder:?}")))?;
            if output >= 1 << spec.inputs.len() {
                return Err(error(&format!("decoder {decoder} has no output {output}")));
            }
            Target::Decoder {
                decoder: index,
                output,
            }
        }
        (None, None, None, Some(code)) => {
            if attachment.role.is_some()
                || attachment.lane.is_some()
                || attachment.lines.is_some()
                || attachment.offset.is_some()
            {
                return Err(error("a QSPI attachment takes only qspi and mask"));
            }
            let mask = attachment.mask.unwrap_or(PCS_MASK);
            if mask & !PCS_MASK != 0 || code & !mask != 0 {
                return Err(error(
                    "PCS codes and masks are 4 bits, the code within the mask",
                ));
            }
            Target::Qspi { code, mask }
        }
        _ => {
            return Err(error(
                "give one of select, decoder and output, or qspi and mask",
            ));
        }
    };
    // an acknowledge's attachment answers acknowledges alone
    let role = if matches!(target, Target::Bus(BusDecode::Acknowledge(_))) {
        Role::Iack
    } else {
        role
    };
    let point = match target {
        Target::Select(select) => Some(Point::Select(select)),
        Target::Decoder { decoder, output } => Some(Point::Output(decoder, output)),
        Target::Qspi { .. } | Target::Bus(_) => None,
    };
    if let Some(point) = point {
        users.add(point, owner.to_owned(), Some((role, lane)))?;
    }
    Ok(AttachmentSpec {
        target,
        role,
        lane,
        lines,
        offset,
    })
}

/// The cycles a no-select attachment of `role` takes: a window of
/// `base` and `size`, or the acknowledges of `iack`'s level.
fn bus_decode(attachment: &Attachment, role: Role) -> Result<Target, &'static str> {
    let decode = match (attachment.base, attachment.size, attachment.iack) {
        (Some(base), Some(size), None) => {
            if role == Role::Iack {
                return Err("a window takes no iack role: give iack = <level>");
            }
            if size == 0 || u64::from(base) + u64::from(size) > 1 << ADDRESS_LINES {
                return Err("its window must be inside ADDR[23:0] and not empty");
            }
            BusDecode::Window {
                base: Address(base),
                length: size,
            }
        }
        (None, None, Some(level)) => {
            if attachment
                .role
                .is_some_and(|role| role.split().0 != Role::Iack)
            {
                return Err("an iack attachment's role is iack");
            }
            BusDecode::Acknowledge(IrqPin::new(level).ok_or("iack is a level, 1 to 7")?)
        }
        _ => return Err("give base and size, or iack, not both"),
    };
    Ok(Target::Bus(decode))
}

/// A peripheral.
fn read_peripheral(
    entry: schema::Peripheral,
    decoders: &[DecoderSpec],
    users: &mut Users,
) -> Result<PeripheralSpec, LoadError> {
    let attachments: Vec<AttachmentSpec> = entry
        .attach
        .iter()
        .enumerate()
        .map(|(index, attachment)| read_attachment(&entry.name, index, attachment, decoders, users))
        .collect::<Result<_, _>>()?;
    let irq = entry
        .irq
        .map(|level| irq_pin(&entry.name, level))
        .transpose()?;
    let reset = entry.reset.as_deref().map_or(ResetScope::ALL, scope);
    let dsack = entry.dsack.and_then(|dsack| match dsack {
        Dsack::Dsack0 => Some(mc683xx_modules::sim::chip_selects::Dsack::Dsack0),
        Dsack::Dsack1 => Some(mc683xx_modules::sim::chip_selects::Dsack::Dsack1),
        Dsack::Both => Some(mc683xx_modules::sim::chip_selects::Dsack::Both),
        Dsack::Open => None,
    });
    if dsack.is_none()
        && attachments
            .iter()
            .any(|attachment| matches!(attachment.target, Target::Bus(_)))
    {
        return Err(attachment_error(
            &entry.name,
            0,
            "a peripheral on no select drives DSACK itself: give dsack",
        ));
    }
    let mut parameters = entry.parameters;
    if parameters.get(NOTE).is_some_and(toml::Value::is_str) {
        parameters.remove(NOTE);
    }
    Ok(PeripheralSpec {
        name: entry.name,
        kind: entry.kind,
        marking: entry.marking,
        parameters: table_from_toml(parameters),
        attachments,
        irq,
        reset,
        dsack,
    })
}

/// The endpoints already on a net, as drivers and as sinks: each is on
/// at most one net in each role, but external endpoints, which the
/// runner multiplexes.
#[derive(Debug, Default)]
struct Seen {
    /// Drivers, with their nets.
    drivers: BTreeMap<Endpoint, String>,
    /// Sinks, with their nets.
    sinks: BTreeMap<Endpoint, String>,
}

/// A net, without the endpoints of peripherals an option leaves off the
/// board; [`None`] if that leaves it no driver or no sink.
fn read_net(
    entry: schema::Net,
    peripherals: &BTreeSet<&str>,
    absent: &BTreeSet<String>,
    seen: &mut Seen,
) -> Result<Option<NetSpec>, LoadError> {
    let net = entry.name;
    let (kind, wired) = entry.kind.kind();
    let net_error = |reason: &str| LoadError::Net {
        net: net.clone(),
        reason: reason.to_owned(),
    };
    let from = match entry.from {
        Endpoints::One(one) => vec![one],
        Endpoints::Many(many) => many,
    };
    if from.is_empty() || entry.to.is_empty() {
        return Err(net_error("a net needs a driver and a sink"));
    }
    let present = |name: &String| !matches!(Endpoint::parse(name), Some(Endpoint::Peripheral { peripheral, .. }) if absent.contains(&peripheral));
    let from: Vec<String> = from.into_iter().filter(present).collect();
    let sinks: Vec<String> = entry.to.into_iter().filter(present).collect();
    if from.is_empty() || sinks.is_empty() {
        return Ok(None);
    }
    if from.len() > 1 && !wired {
        return Err(net_error("several drivers need a wired-and net"));
    }
    let resolve = |name: &String| -> Result<Endpoint, LoadError> {
        Endpoint::parse(name)
            .filter(|endpoint| match endpoint {
                Endpoint::Peripheral { peripheral, .. } => {
                    peripherals.contains(peripheral.as_str())
                }
                _ => true,
            })
            .ok_or_else(|| LoadError::Endpoint {
                net: net.clone(),
                endpoint: name.clone(),
            })
    };
    let from = from.iter().map(resolve).collect::<Result<Vec<_>, _>>()?;
    let to = sinks.iter().map(resolve).collect::<Result<Vec<_>, _>>()?;
    for (endpoints, taken) in [(&from, &mut seen.drivers), (&to, &mut seen.sinks)] {
        for endpoint in endpoints {
            if matches!(endpoint, Endpoint::External(_)) {
                continue;
            }
            if let Some(other) = taken.insert(endpoint.clone(), net.clone()) {
                return Err(net_error(&format!(
                    "{endpoint} is already on net {other} that way"
                )));
            }
        }
    }
    Ok(Some(NetSpec {
        name: net,
        kind,
        wired,
        from,
        to,
    }))
}

#[cfg(test)]
mod tests;
