//! smartclock's board files (github.com/charlieh0tel/smartclock), its
//! six models, as test inputs: each must parse and load with the stock
//! peripherals (`mc683xx-peripherals`), so a stock part's parameters
//! and ports are checked as a run checks them, and every other peripheral
//! type registered as a stub with the ports the file's nets use.  The files are smartclock's, not in this
//! repository: `SMARTCLOCK_BOARDS` names their directory (smartclock's
//! `boards`).  The tests are ignored, so that a run without it passes
//! none vacuously: `make smartclock-boards` runs them, and they fail
//! if it is unset.  Synthetic images stand
//! in for HP's firmware.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process;

use mc683xx_board::board::Board;

use mc683xx_board::builtin::Recorder;
use mc683xx_board::description::Description;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::signal::Direction;
use mc683xx_board::signal::Kind;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_peripherals::register;

/// The variable naming the directory.
const BOARDS: &str = "SMARTCLOCK_BOARDS";
/// Bytes of a synthetic image for a memory with no size.
const IMAGE_BYTES: usize = 0x8_0000;

/// The board files in the directory the variable names.
fn boards() -> Vec<PathBuf> {
    let directory = env::var_os(BOARDS).unwrap_or_else(|| panic!("{BOARDS} is not set"));
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .expect("the boards directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no board files in {BOARDS}");
    files
}

/// A directory for the synthetic images.
fn images_directory(test: &str) -> PathBuf {
    let path = env::temp_dir().join(format!("smartclock-drafts-{test}-{}", process::id()));
    fs::create_dir_all(&path).expect("the images' directory");
    path
}

/// An integer key of a TOML table.
fn integer(table: &toml::Value, key: &str) -> Option<usize> {
    table
        .get(key)?
        .as_integer()
        .and_then(|value| usize::try_from(value).ok())
}

/// Every memory's image in `path` replaced by a synthetic one in
/// `directory` that fills it, and the declared options on if
/// `enabled`.
fn options(path: &Path, directory: &Path, enabled: bool) -> Options {
    let text = fs::read_to_string(path).expect("the board file");
    let table: toml::Table = toml::from_str(&text).expect("TOML");
    let mut images = BTreeMap::new();
    for memory in table
        .get("memory")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|memory| memory.get("image").is_some())
    {
        let name = memory["name"].as_str().expect("a name");
        let bytes = integer(memory, "image_offset").unwrap_or(0)
            + integer(memory, "size").unwrap_or(IMAGE_BYTES);
        let image = directory.join(format!("{name}-{bytes:x}.bin"));
        fs::write(&image, vec![0xff; bytes]).expect("the image");
        images.insert(name.to_owned(), image);
    }
    let declared = table
        .get("board")
        .and_then(|board| board.get("options"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(str::to_owned);
    let mut options = Options::default();
    if enabled {
        options.enabled = declared.collect();
    }
    options.images = images;
    options
}

#[test]
#[ignore = "needs SMARTCLOCK_BOARDS: make smartclock-boards"]
fn every_draft_parses_with_and_without_its_options() {
    let boards = boards();
    let directory = images_directory("parse");
    for path in &boards {
        for enabled in [false, true] {
            if let Err(error) = Description::read(path, &options(path, &directory, enabled)) {
                panic!("{}: {error}", path.display());
            }
        }
    }
    fs::remove_dir_all(directory).expect("the images' directory");
}

/// A port as the stubs declare it: its kind, and whether nets drive it
/// and are driven by it.
type Use = (Kind, bool, bool);

/// The kind a net of `name` carries.
fn kind(name: &str) -> Kind {
    match name {
        "logic" | "wired-and" => Kind::Logic,
        "edge" => Kind::Edge,
        "clock" => Kind::Clock,
        "serial" => Kind::Serial,
        "analog" => Kind::Analog,
        "bus" => Kind::Bus,
        other => panic!("net type {other}"),
    }
}

/// A registry of the built-in and stock types and, for each type `path`
/// names that it lacks, a recording stub with the ports its nets use.
fn registry(path: &Path) -> Registry {
    let text = fs::read_to_string(path).expect("the board file");
    let table: toml::Table = toml::from_str(&text).expect("TOML");
    let array = |key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let types: BTreeMap<String, String> = array("peripheral")
        .iter()
        .map(|peripheral| {
            (
                peripheral["name"].as_str().expect("a name").to_owned(),
                peripheral["type"].as_str().expect("a type").to_owned(),
            )
        })
        .collect();
    let mut ports: BTreeMap<String, BTreeMap<String, Use>> = BTreeMap::new();
    for net in array("net") {
        let kind = kind(net["type"].as_str().expect("a type"));
        let from: Vec<toml::Value> = match &net["from"] {
            toml::Value::Array(many) => many.clone(),
            one => vec![one.clone()],
        };
        let to = net["to"].as_array().cloned().unwrap_or_default();
        for (endpoints, driving) in [(from, true), (to, false)] {
            for endpoint in endpoints {
                let endpoint = endpoint.as_str().expect("an endpoint");
                let Some((peripheral, port)) = endpoint.split_once('.') else {
                    continue;
                };
                let Some(type_name) = types.get(peripheral) else {
                    continue;
                };
                let entry = ports
                    .entry(type_name.clone())
                    .or_default()
                    .entry(port.to_owned())
                    .or_insert((kind, false, false));
                if driving {
                    entry.1 = true;
                } else {
                    entry.2 = true;
                }
            }
        }
    }
    let mut registry = Registry::with_builtins();
    register(&mut registry).expect("the stock types");
    for type_name in types.values() {
        if registry.get(type_name).is_some() {
            continue;
        }
        let specs: Vec<PortSpec> = ports
            .get(type_name)
            .into_iter()
            .flatten()
            .map(|(name, &(kind, drives, sinks))| PortSpec {
                name: name.clone(),
                kind,
                direction: match (drives, sinks) {
                    (true, true) => Direction::InOut,
                    (true, false) => Direction::Out,
                    _ => Direction::In,
                },
            })
            .collect();
        registry
            .register(type_name.clone(), specs, |context| {
                let _ = context.parameters.table();
                Ok(Box::new(Recorder::default()))
            })
            .expect("a new type");
    }
    registry
}

/// The board `path` loaded, its options on if `enabled`, and run; why
/// it failed, if it did.  Paths of the board's
/// peripherals (an EEPROM's working file) resolve in `directory`, so the
/// boards' own directory is only read.
fn load_and_run(path: &Path, directory: &Path, enabled: bool) -> Result<(), String> {
    let description = Description::read(path, &options(path, directory, enabled))
        .map_err(|error| error.to_string())?;
    let mut environment = Environment::default();
    environment.work_dir = Some(directory.to_owned());
    environment.board_dir = directory.to_owned();
    let mut board = Board::<Mc68331>::load(&description, &registry(path), environment)
        .map_err(|error| error.to_string())?;
    board
        .run_until(Time(1_000_000_000))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[test]
#[ignore = "needs SMARTCLOCK_BOARDS: make smartclock-boards"]
fn every_draft_loads_with_the_stock_peripherals_and_runs() {
    let boards = boards();
    let directory = images_directory("load");
    let mut failures = Vec::new();
    for path in &boards {
        for enabled in [false, true] {
            let options = if enabled { "options on" } else { "options off" };
            match load_and_run(path, &directory, enabled) {
                Ok(()) => println!("{} ({options}): loads and runs", path.display()),
                Err(error) => {
                    println!("{} ({options}): {error}", path.display());
                    failures.push(format!("{} ({options}): {error}", path.display()));
                }
            }
        }
    }
    fs::remove_dir_all(directory).expect("the images' directory");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
