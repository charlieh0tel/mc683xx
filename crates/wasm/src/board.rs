//! A board from its board file's text and its images' bytes, run from
//! the page's clock (`performance.now()`) a frame at a time, its console
//! as bytes.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::time::Duration;

use js_sys::Array;
use js_sys::BigInt;
use js_sys::Function;
use js_sys::Map;
use js_sys::Object;
use js_sys::Reflect;
use js_sys::Symbol;
use js_sys::Uint8Array;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::pacing::Pacing;
use mc683xx_board::registry::Environment;
use mc683xx_board::signal::Serial;
use mc683xx_board::signal::Signal;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::qsm::sci::Frame;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::error::Error;

/// The radix a `bigint` prints in, in a message.
const DECIMAL: u8 = 10;

/// Milliseconds a second, for the page's clock readings.
const MILLISECONDS_PER_SECOND: f64 = 1000.0;

/// A board built around the MC68331 (Rust's `Board<Mc68331>`).
#[wasm_bindgen]
#[derive(Debug)]
pub struct Board {
    /// The board.
    board: mc683xx_board::board::Board<Mc68331>,
}

impl Board {
    /// The board file `text` describes, its images' bytes, its memories'
    /// and its peripherals' (an AT28C64B's `image`), in `images` by the
    /// paths the file names, with the options `enabled`; its types the
    /// built-in ones and the stock peripherals'.
    ///
    /// # Errors
    ///
    /// [`Error::Load`] for a file or board that does not load, an image
    /// `images` lacks among them.
    pub fn with_images(
        text: &str,
        images: &BTreeMap<String, Vec<u8>>,
        enabled: &[String],
    ) -> Result<Self, Error> {
        let mut options = Options::default();
        options.enabled = enabled.iter().cloned().collect();
        let read = |path: &Path| {
            path.to_str()
                .and_then(|name| images.get(name))
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        };
        let description = Description::parse_with(text, Path::new(""), &options, &read)?;
        let board = mc683xx_board::board::Board::load_with(
            &description,
            &mc683xx_peripherals::registry(),
            Environment::default(),
            read,
        )?;
        Ok(Self { board })
    }

    /// [`runFor`](Self::run_for_js), its picoseconds a `u64`.
    ///
    /// # Errors
    ///
    /// As [`run_for_js`](Self::run_for_js).
    pub fn run_for(&mut self, picoseconds: u64) -> Result<BoardRun, Error> {
        Ok(self.board.run_for(picoseconds)?.into())
    }

    /// [`runFrame`](Self::run_frame_js), its picoseconds a `u64`.
    ///
    /// # Errors
    ///
    /// As [`run_frame_js`](Self::run_frame_js).
    pub fn run_frame(&mut self, wall: f64, picoseconds: u64) -> Result<BoardRun, Error> {
        let wall = Duration::try_from_secs_f64(wall / MILLISECONDS_PER_SECOND)
            .map_err(|_| Error::Wall(wall))?;
        Ok(self.board.run_frame(wall, picoseconds)?.into())
    }

    /// [`send`](Self::send_js), its data bytes.
    ///
    /// # Errors
    ///
    /// As [`send_js`](Self::send_js).
    pub fn send(&mut self, name: &str, data: &[u8]) -> Result<(), Error> {
        let serial = |byte: u8| Signal::Serial(Serial::untimed(Frame::Data(byte.into())));
        // the endpoint checked even for no data
        self.board.check_send(name, &serial(0))?;
        for &byte in data {
            self.board.send(name, serial(byte))?;
        }
        Ok(())
    }

    /// The board, to reach what the bindings do not.
    #[must_use]
    pub const fn board(&self) -> &mc683xx_board::board::Board<Mc68331> {
        &self.board
    }

    /// The board, to change.
    pub const fn board_mut(&mut self) -> &mut mc683xx_board::board::Board<Mc68331> {
        &mut self.board
    }
}

#[wasm_bindgen]
impl Board {
    /// The board file `text` describes, its images' bytes in `images`, a
    /// `Map` or an object from the path the file names to a
    /// `Uint8Array` (none for `null` or `undefined`), with the options
    /// `enabled` names.
    ///
    /// # Errors
    ///
    /// [`Error::Images`], [`Error::ImageName`] and [`Error::Image`] for
    /// images of another shape, and as [`with_images`](Self::with_images).
    #[wasm_bindgen(constructor)]
    pub fn new(
        text: &str,
        #[wasm_bindgen(
            unchecked_param_type = "Map<string, Uint8Array> | Record<string, Uint8Array> | null | undefined"
        )]
        images: &JsValue,
        enabled: Option<Vec<String>>,
    ) -> Result<Self, Error> {
        Self::with_images(text, &image_bytes(images)?, &enabled.unwrap_or_default())
    }

    /// Board time in picoseconds.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn now(&self) -> u64 {
        self.board.now().0
    }

    /// The pacing [`run_frame`](Self::run_frame_js) keeps: `unpaced`,
    /// `realtime` or a factor, board seconds a wall second.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn pacing(&self) -> String {
        self.board.control().pacing().to_string()
    }

    /// Sets the pacing from its text, as [`pacing`](Self::pacing) gives
    /// it.
    ///
    /// # Errors
    ///
    /// [`Error::ParsePacing`] for text that names no pacing.
    #[wasm_bindgen(js_name = setPacing)]
    pub fn set_pacing(&mut self, pacing: &str) -> Result<(), Error> {
        let pacing: Pacing = pacing.parse()?;
        Ok(self.board.control().set_pacing(pacing)?)
    }

    /// Runs `picoseconds` of board time from now, unpaced (Rust's
    /// `Board::run_for`).
    ///
    /// # Errors
    ///
    /// [`Error::PicosecondsType`] for picoseconds not a `bigint`,
    /// [`Error::Picoseconds`] for a `bigint` out of a `u64`'s range, and
    /// [`Error::Run`] for a run that fails.
    #[wasm_bindgen(js_name = runFor)]
    pub fn run_for_js(
        &mut self,
        #[wasm_bindgen(unchecked_param_type = "bigint")] picoseconds: JsValue,
    ) -> Result<BoardRun, Error> {
        self.run_for(picoseconds_of(picoseconds)?)
    }

    /// Runs a frame (Rust's `Board::run_frame`): to the board time due by
    /// the page's clock reading `wall`, in milliseconds
    /// (`performance.now()`), under the pacing, but no more than
    /// `picoseconds` of board time, a frame's bound however far behind the
    /// page is (a hidden tab), which drops the time the page missed.
    /// Unpaced it runs `picoseconds`.  Paced, the first frame, and the first
    /// after a [`set_pacing`](Self::set_pacing) to another pacing (not to
    /// the one it has), a [`run_for`](Self::run_for_js) (even of no time)
    /// or a successful [`restore`](Self::restore), runs nothing: it
    /// starts the pacing.
    ///
    /// # Errors
    ///
    /// [`Error::Wall`] for a reading that is not finite or below 0,
    /// [`Error::PicosecondsType`] and [`Error::Picoseconds`] as for
    /// [`run_for_js`](Self::run_for_js), and [`Error::Run`] for a run
    /// that fails.
    #[wasm_bindgen(js_name = runFrame)]
    pub fn run_frame_js(
        &mut self,
        wall: f64,
        #[wasm_bindgen(unchecked_param_type = "bigint")] picoseconds: JsValue,
    ) -> Result<BoardRun, Error> {
        self.run_frame(wall, picoseconds_of(picoseconds)?)
    }

    /// Sends `data`, a `Uint8Array`, to the run-time endpoint `name`
    /// now, a serial frame a byte.
    ///
    /// # Errors
    ///
    /// [`Error::Data`] for data not a `Uint8Array` (wasm-bindgen would
    /// take a string or an array of numbers as bytes), and
    /// [`Error::Send`] for an endpoint that drives no serial net.
    #[wasm_bindgen(js_name = send)]
    pub fn send_js(
        &mut self,
        name: &str,
        #[wasm_bindgen(unchecked_param_type = "Uint8Array")] data: &JsValue,
    ) -> Result<(), Error> {
        let data = uint8_array(data).ok_or_else(|| Error::Data(described(data)))?;
        self.send(name, &data.to_vec())
    }

    /// The data bytes of the serial frames that reached the run-time
    /// endpoint `name` since the last call: a frame's low eight bits,
    /// breaks and other values left out.
    ///
    /// # Errors
    ///
    /// [`Error::Receive`] for an endpoint no net drives.
    #[wasm_bindgen(js_name = takeReceived)]
    pub fn take_received(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        Ok(self
            .board
            .take_received(name)?
            .into_iter()
            .filter_map(|(_, signal)| match signal {
                Signal::Serial(serial) => serial.frame.data().map(|data| data.to_le_bytes()[0]),
                _ => None,
            })
            .collect())
    }

    /// The memory `name`'s bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Memory`] for a memory the board lacks.
    pub fn memory(&self, name: &str) -> Result<Vec<u8>, Error> {
        self.board
            .memory(name)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| Error::Memory(name.to_owned()))
    }

    /// The board's state, as Rust's `Board::save`.
    ///
    /// # Errors
    ///
    /// [`Error::State`] for a peripheral that cannot save.
    pub fn save(&self) -> Result<Vec<u8>, Error> {
        Ok(self.board.save()?)
    }

    /// Puts back the state `snapshot` holds, as Rust's `Board::restore`.
    ///
    /// # Errors
    ///
    /// [`Error::State`] for a snapshot the board refuses.
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), Error> {
        Ok(self.board.restore(snapshot)?)
    }
}

/// How a run ended (Rust's `BoardRun`).
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardRun {
    /// Why it ended.
    stop: Stop,
    /// Board time at its end, in picoseconds.
    now: u64,
}

#[wasm_bindgen]
impl BoardRun {
    /// Why it ended, the kind of Rust's `Stop`: `budget` (its time ran
    /// out), `stopped`, `halted`, `hook`, `requested`, `background` or
    /// `fault`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn stop(&self) -> String {
        stop_name(self.stop).to_owned()
    }

    /// Board time at its end, in picoseconds.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn now(&self) -> u64 {
        self.now
    }
}

/// JavaScript's name for the kind of `stop`: the bindings' own, not
/// `Stop`'s `Display`, so a change there does not change what a page
/// branches on.  `Stop` is `#[non_exhaustive]`; a kind these bindings do
/// not name yet is `unknown`.
const fn stop_name(stop: Stop) -> &'static str {
    match stop {
        Stop::Budget => "budget",
        Stop::Stopped => "stopped",
        Stop::Halted(_) => "halted",
        Stop::Hook(_) => "hook",
        Stop::Requested => "requested",
        Stop::Background(_) => "background",
        Stop::Fault => "fault",
        _ => "unknown",
    }
}

impl From<mc683xx_board::board::BoardRun> for BoardRun {
    fn from(run: mc683xx_board::board::BoardRun) -> Self {
        Self {
            stop: run.stop,
            now: run.now.0,
        }
    }
}

/// `value` as picoseconds of board time: a `bigint` from 0 to 2 ** 64 - 1.
/// wasm-bindgen would take a `bigint` modulo 2 ** 64 (so `-1n` would run
/// for 213 days), and a `number` is no `bigint`, however exact.
fn picoseconds_of(value: JsValue) -> Result<u64, Error> {
    let Some(bigint) = value.dyn_ref::<BigInt>() else {
        return Err(Error::PicosecondsType(described(&value)));
    };
    let text = bigint
        .to_string(DECIMAL)
        .map_or_else(|_| String::new(), String::from);
    u64::try_from(value).map_err(|_| Error::Picoseconds(text))
}

/// The images in `images`, a `Map` or an object, by path; none for
/// `undefined` or `null`.
fn image_bytes(images: &JsValue) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let entries = if images.is_undefined() || images.is_null() {
        return Ok(BTreeMap::new());
    } else if let Some(map) = map(images) {
        Array::from(&map.entries())
    } else if images.is_object() {
        Object::entries(images.unchecked_ref())
    } else {
        return Err(Error::Images);
    };
    entries
        .iter()
        .map(|entry| {
            let entry: Array = entry.unchecked_into();
            let name = entry.get(0);
            let name = name
                .as_string()
                .ok_or_else(|| Error::ImageName(described(&name)))?;
            let bytes = entry.get(1);
            let bytes = uint8_array(&bytes).ok_or_else(|| Error::Image(name.clone()))?;
            Ok((name, bytes.to_vec()))
        })
        .collect()
}

/// `value` if it is a `Uint8Array`, from this realm or another (an
/// iframe's, a `vm` context's), where `instanceof`, which compares
/// constructors, fails: by the getter of
/// `%TypedArray%.prototype[%Symbol.toStringTag%]` (ECMA-262, Properties
/// of the `%TypedArray%` Intrinsic Object), the name of a typed array
/// of any realm, undefined for anything else.
fn uint8_array(value: &JsValue) -> Option<&Uint8Array> {
    let typed_array =
        Object::get_prototype_of(&Object::get_prototype_of(&Uint8Array::new_with_length(0)));
    let name = slot_getter(&typed_array, &Symbol::to_string_tag(), value)?;
    (name.as_string()? == "Uint8Array").then(|| value.unchecked_ref())
}

/// `value` if it is a `Map`, of any realm, as [`uint8_array`]: by the
/// getter `Map.prototype.size` (ECMA-262, Properties of the Map
/// Prototype Object), which throws for anything else.
fn map(value: &JsValue) -> Option<&Map> {
    let prototype = Object::get_prototype_of(&Map::new());
    slot_getter(&prototype, &JsValue::from_str("size"), value)?;
    Some(value.unchecked_ref())
}

/// `prototype`'s own getter `key` called on `value`, `None` if it
/// throws: a built-in's getter checks an internal slot of `value`, which
/// any realm's object of its kind has.
fn slot_getter(prototype: &Object, key: &JsValue, value: &JsValue) -> Option<JsValue> {
    let descriptor = Object::get_own_property_descriptor(prototype, key);
    let getter = Reflect::get(&descriptor, &JsValue::from_str("get")).ok()?;
    getter.dyn_ref::<Function>()?.call0(value).ok()
}

/// What `value` is, for a message: `null`, `undefined`, a primitive's
/// type (`a string`, `a number`), or an object's or a function's tag (`an
/// Array`, `an ArrayBuffer`, `an Object`), as
/// `Object.prototype.toString` gives it.
fn described(value: &JsValue) -> String {
    if value.is_null() {
        return "null".to_owned();
    }
    if value.is_undefined() {
        return "undefined".to_owned();
    }
    let kind = if value.is_object() || value.is_function() {
        tag(value).unwrap_or_else(|| "object".to_owned())
    } else {
        value.js_typeof().as_string().unwrap_or_default()
    };
    let article = if kind.starts_with(VOWELS) { "an" } else { "a" };
    format!("{article} {kind}")
}

/// Letters that take `an`, in [`described`].
const VOWELS: [char; 10] = ['A', 'E', 'I', 'O', 'U', 'a', 'e', 'i', 'o', 'u'];

/// What `Object.prototype.toString` gives an object, `[object Array]`,
/// around its tag.
const TAG_PREFIX: &str = "[object ";
/// See [`TAG_PREFIX`].
const TAG_SUFFIX: &str = "]";

/// `value`'s tag (`Array`), as `Object.prototype.toString` gives it,
/// `None` if it throws (a revoked proxy).
fn tag(value: &JsValue) -> Option<String> {
    let prototype = Object::get_prototype_of(&Object::new());
    let to_string = Reflect::get(&prototype, &JsValue::from_str("toString")).ok()?;
    let text = to_string
        .dyn_ref::<Function>()?
        .call0(value)
        .ok()?
        .as_string()?;
    Some(
        text.strip_prefix(TAG_PREFIX)?
            .strip_suffix(TAG_SUFFIX)?
            .to_owned(),
    )
}
