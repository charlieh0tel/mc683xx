//! The registry of peripheral types: each type's name, the ports it declares,
//! and its constructor from the peripheral's parameter table.
//!
//! A board file names a type by its name alone, so a host's own types
//! (and, later, a plugin's) are registered here and no file format
//! changes ([PLAN.md, Boards][plan-boards]).
//!
//! [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use crate::builtin;
use crate::error::LoadError;
use crate::error::SyntaxError;
use crate::peripheral::Peripheral;
use crate::signal::Direction;
use crate::signal::Kind;

/// A port a peripheral type declares: a net endpoint `peripheral.name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortSpec {
    /// Its name.
    pub name: String,
    /// What it carries.
    pub kind: Kind,
    /// Which way it faces.
    pub direction: Direction,
}

impl PortSpec {
    /// An input `name` of `kind`.
    #[must_use]
    pub fn input(name: impl Into<String>, kind: Kind) -> Self {
        Self {
            name: name.into(),
            kind,
            direction: Direction::In,
        }
    }

    /// An output `name` of `kind`.
    #[must_use]
    pub fn output(name: impl Into<String>, kind: Kind) -> Self {
        Self {
            name: name.into(),
            kind,
            direction: Direction::Out,
        }
    }
}

/// What a constructor refuses in a peripheral's parameters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParameterError {
    /// A parameter the type needs is missing.
    #[error("parameter {0:?} is missing")]
    Missing(String),
    /// A parameter of the wrong TOML type.
    #[error("parameter {key:?} is not {expected}")]
    Type {
        /// The parameter.
        key: String,
        /// What it should be.
        expected: &'static str,
    },
    /// An integer out of the parameter's range.
    #[error("parameter {key:?} is out of range: {value}")]
    Range {
        /// The parameter.
        key: String,
        /// Its value.
        value: i64,
    },
    /// A parameter the type does not know: found after construction,
    /// as one the constructor never read.
    #[error("unknown parameter {0:?}")]
    Unknown(String),
    /// A value the type refuses for its own reason.
    #[error("parameter {key:?}: {reason}")]
    Invalid {
        /// The parameter.
        key: String,
        /// Why.
        reason: String,
    },
}

impl ParameterError {
    /// The type refuses parameter `key`'s value for `reason`
    /// ([`ParameterError::Invalid`]).
    #[must_use]
    pub fn invalid(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid {
            key: key.into(),
            reason: reason.into(),
        }
    }
}

/// A parameter's value, as a board file's TOML gives it
/// ([`docs/board-files.md`][board-files]): the board's own type, so the
/// file format's parser is not part of the API.
///
/// [board-files]: https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ParameterValue {
    /// A string.
    String(String),
    /// An integer.
    Integer(i64),
    /// A float.
    Float(f64),
    /// A boolean.
    Boolean(bool),
    /// A date or time, as its TOML text.
    Datetime(String),
    /// An array.
    Array(Vec<ParameterValue>),
    /// A table.
    Table(ParameterTable),
}

/// A table of parameters: values by key.
pub type ParameterTable = BTreeMap<String, ParameterValue>;

impl ParameterValue {
    /// The integer, if it is one.
    #[must_use]
    pub const fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(integer) => Some(*integer),
            _ => None,
        }
    }

    /// The float, if it is one.
    #[must_use]
    pub const fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(float) => Some(*float),
            _ => None,
        }
    }

    /// The boolean, if it is one.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(boolean) => Some(*boolean),
            _ => None,
        }
    }

    /// The string, if it is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    /// The array, if it is one.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }

    /// The table, if it is one.
    #[must_use]
    pub const fn as_table(&self) -> Option<&ParameterTable> {
        match self {
            Self::Table(table) => Some(table),
            _ => None,
        }
    }

    /// `value`, from the file's TOML.
    pub(crate) fn from_toml(value: toml::Value) -> Self {
        match value {
            toml::Value::String(text) => Self::String(text),
            toml::Value::Integer(integer) => Self::Integer(integer),
            toml::Value::Float(float) => Self::Float(float),
            toml::Value::Boolean(boolean) => Self::Boolean(boolean),
            toml::Value::Datetime(datetime) => Self::Datetime(datetime.to_string()),
            toml::Value::Array(values) => {
                Self::Array(values.into_iter().map(Self::from_toml).collect())
            }
            toml::Value::Table(table) => Self::Table(table_from_toml(table)),
        }
    }
}

/// `table`, from the file's TOML.
pub(crate) fn table_from_toml(table: toml::Table) -> ParameterTable {
    table
        .into_iter()
        .map(|(key, value)| (key, ParameterValue::from_toml(value)))
        .collect()
}

impl From<i64> for ParameterValue {
    fn from(integer: i64) -> Self {
        Self::Integer(integer)
    }
}

impl From<i32> for ParameterValue {
    fn from(integer: i32) -> Self {
        Self::Integer(integer.into())
    }
}

impl From<u32> for ParameterValue {
    fn from(integer: u32) -> Self {
        Self::Integer(integer.into())
    }
}

impl From<f64> for ParameterValue {
    fn from(float: f64) -> Self {
        Self::Float(float)
    }
}

impl From<bool> for ParameterValue {
    fn from(boolean: bool) -> Self {
        Self::Boolean(boolean)
    }
}

impl From<String> for ParameterValue {
    fn from(text: String) -> Self {
        Self::String(text)
    }
}

impl From<&str> for ParameterValue {
    fn from(text: &str) -> Self {
        Self::String(text.to_owned())
    }
}

impl From<Vec<Self>> for ParameterValue {
    fn from(values: Vec<Self>) -> Self {
        Self::Array(values)
    }
}

impl From<ParameterTable> for ParameterValue {
    fn from(table: ParameterTable) -> Self {
        Self::Table(table)
    }
}

/// A peripheral's parameter table, read by its type's constructor.  Every
/// key read is noted: a key no read reached is an unknown parameter,
/// an error naming the peripheral, so no constructor checks for strays.
#[derive(Debug, Default)]
pub struct Parameters {
    /// The table from the file.
    table: ParameterTable,
    /// The keys read.
    read: RefCell<BTreeSet<String>>,
}

impl Parameters {
    /// The parameters in `table`.
    #[must_use]
    pub fn new(table: ParameterTable) -> Self {
        Self {
            table,
            read: RefCell::default(),
        }
    }

    /// The parameters a TOML table's `text` gives, as a board file's
    /// `parameters` would: for a host's tests of its constructors, and,
    /// through [`Parameters::into_table`], for a host holding TOML that
    /// sets a peripheral's parameters (`Options::parameters`).  The TOML
    /// parser stays out of the API: a host with its own parsed TOML
    /// passes its text.
    ///
    /// # Errors
    ///
    /// A `text` that is not a TOML table ([`LoadError::Syntax`]).
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        let table: toml::Table = toml::from_str(text).map_err(SyntaxError::from_toml)?;
        Ok(Self::new(table_from_toml(table)))
    }

    /// The value of `key`, if there is one, noted as read.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&ParameterValue> {
        self.read.borrow_mut().insert(key.to_owned());
        self.table.get(key)
    }

    /// The whole table, every key noted as read: for a constructor
    /// that passes it on (Python's, as keyword arguments).
    #[must_use]
    pub fn table(&self) -> &ParameterTable {
        self.read.borrow_mut().extend(self.table.keys().cloned());
        &self.table
    }

    /// The whole table, owned: [`Parameters::parse`]'s values for
    /// `Options::parameters`.
    #[must_use]
    pub fn into_table(self) -> ParameterTable {
        self.table
    }

    /// An integer `key`, if given, that fits `T`.
    ///
    /// # Errors
    ///
    /// A `key` that is not an integer ([`ParameterError::Type`]), or that
    /// does not fit `T` ([`ParameterError::Range`]).
    pub fn integer<T: TryFrom<i64>>(&self, key: &str) -> Result<Option<T>, ParameterError> {
        let Some(value) = self.value(key) else {
            return Ok(None);
        };
        let value = value.as_integer().ok_or_else(|| ParameterError::Type {
            key: key.to_owned(),
            expected: "an integer",
        })?;
        T::try_from(value)
            .map(Some)
            .map_err(|_| ParameterError::Range {
                key: key.to_owned(),
                value,
            })
    }

    /// An integer `key` the type needs.
    ///
    /// # Errors
    ///
    /// A `key` not given ([`ParameterError::Missing`]), or what
    /// [`Parameters::integer`] fails with.
    pub fn required<T: TryFrom<i64>>(&self, key: &str) -> Result<T, ParameterError> {
        self.integer(key)?
            .ok_or_else(|| ParameterError::Missing(key.to_owned()))
    }

    /// A boolean `key`, if given.
    ///
    /// # Errors
    ///
    /// A `key` that is not a boolean ([`ParameterError::Type`]).
    pub fn boolean(&self, key: &str) -> Result<Option<bool>, ParameterError> {
        self.typed(key, ParameterValue::as_bool, "a boolean")
    }

    /// A number `key`, if given: a float, or an integer as one.
    ///
    /// # Errors
    ///
    /// A `key` that is not a number ([`ParameterError::Type`]).
    pub fn float(&self, key: &str) -> Result<Option<f64>, ParameterError> {
        self.typed(
            key,
            |value| {
                value
                    .as_float()
                    .or_else(|| value.as_integer().map(|integer| integer as f64))
            },
            "a number",
        )
    }

    /// A string `key`, if given.
    ///
    /// # Errors
    ///
    /// A `key` that is not a string ([`ParameterError::Type`]).
    pub fn string(&self, key: &str) -> Result<Option<&str>, ParameterError> {
        self.typed(key, ParameterValue::as_str, "a string")
    }

    /// `key` as `convert` reads it, if given.
    fn typed<'a, T>(
        &'a self,
        key: &str,
        convert: impl FnOnce(&'a ParameterValue) -> Option<T>,
        expected: &'static str,
    ) -> Result<Option<T>, ParameterError> {
        self.value(key)
            .map(|value| {
                convert(value).ok_or_else(|| ParameterError::Type {
                    key: key.to_owned(),
                    expected,
                })
            })
            .transpose()
    }

    /// The first key no read reached, in key order: the board's
    /// unknown-parameter error, public so a host's tests of its
    /// constructors find strays as a load would.
    #[must_use]
    pub fn unread(&self) -> Option<String> {
        let read = self.read.borrow();
        self.table.keys().find(|key| !read.contains(*key)).cloned()
    }
}

/// What the run gives every peripheral: run-time choices that stay out of the
/// board file ([PLAN.md, Boards][plan-boards]).
///
/// It may gain fields: build one from [`Environment::default()`] and set
/// those it needs (`let mut x = Environment::default(); x.seed = ...;`).
///
/// [plan-boards]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#boards
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Environment {
    /// The one seed every noise model draws from.
    pub seed: u64,
    /// Where peripherals keep working copies (an EEPROM's), if the run gives
    /// one.
    pub work_dir: Option<PathBuf>,
    /// The board file's directory, which its paths are relative to.
    pub board_dir: PathBuf,
}

impl Environment {
    /// The seed of the peripheral `name`: its own stream from the one seed,
    /// so adding a peripheral does not change another's.
    #[must_use]
    pub fn seed_for(&self, name: &str) -> u64 {
        /// FNV-1a's offset basis and prime, 64-bit.
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        /// FNV-1a's prime.
        const PRIME: u64 = 0x0100_0000_01b3;
        let hash = name.bytes().fold(OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(PRIME)
        });
        mix(self.seed ^ hash)
    }
}

/// `SplitMix64`'s finalizer: spreads a seed's bits.
const fn mix(seed: u64) -> u64 {
    let mixed = (seed ^ (seed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

/// What a constructor is given.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Context<'a> {
    /// The peripheral's name in the board file.
    pub name: &'a str,
    /// Its parameters.
    pub parameters: &'a Parameters,
    /// The run's choices.
    pub environment: &'a Environment,
}

impl<'a> Context<'a> {
    /// What a constructor of the peripheral `name` is given: its
    /// `parameters` and the run's `environment`.  A load builds it; a
    /// host builds one to test a constructor.
    #[must_use]
    pub const fn new(
        name: &'a str,
        parameters: &'a Parameters,
        environment: &'a Environment,
    ) -> Self {
        Self {
            name,
            parameters,
            environment,
        }
    }

    /// A path parameter `key`, if given, relative to the board file's
    /// directory: a flash's or an FPGA design's image.
    ///
    /// # Errors
    ///
    /// A `key` that is not a string ([`ParameterError::Type`]).
    pub fn path(&self, key: &str) -> Result<Option<PathBuf>, ParameterError> {
        Ok(self
            .parameters
            .string(key)?
            .map(|path| self.environment.board_dir.join(path)))
    }
}

/// A peripheral and the ports it has, as a constructor builds them.
#[non_exhaustive]
pub struct Constructed {
    /// The peripheral.
    pub peripheral: Box<dyn Peripheral>,
    /// Its ports, which it knows by index: the type's, or for a type
    /// whose ports depend on its parameters (an FPGA's design, a
    /// latch's width), its own.
    pub ports: Vec<PortSpec>,
}

impl Constructed {
    /// `peripheral` with `ports`.
    #[must_use]
    pub fn new(peripheral: Box<dyn Peripheral>, ports: Vec<PortSpec>) -> Self {
        Self { peripheral, ports }
    }
}

impl fmt::Debug for Constructed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Constructed")
            .field("ports", &self.ports)
            .finish_non_exhaustive()
    }
}

/// A type's constructor, of a peripheral alone.
type Constructor = Box<dyn Fn(&Context<'_>) -> Result<Box<dyn Peripheral>, ParameterError>>;
/// A type's constructor, of a peripheral and its ports.
type PortedConstructor = Box<dyn Fn(&Context<'_>) -> Result<Constructed, ParameterError>>;

/// How a type builds its peripherals.
enum Builds {
    /// With the type's fixed ports.
    Fixed(Vec<PortSpec>, Constructor),
    /// With ports from the parameters.
    Ported(PortedConstructor),
}

/// A registered peripheral type.
pub struct PeripheralType {
    /// How it builds its peripherals.
    builds: Builds,
}

impl PeripheralType {
    /// A peripheral of this type, with its ports.
    ///
    /// # Errors
    ///
    /// What the type's constructor refuses in `context`'s parameters.
    pub fn construct(&self, context: &Context<'_>) -> Result<Constructed, ParameterError> {
        match &self.builds {
            Builds::Fixed(ports, constructor) => Ok(Constructed {
                peripheral: constructor(context)?,
                ports: ports.clone(),
            }),
            Builds::Ported(constructor) => constructor(context),
        }
    }

    /// Its ports, if they do not depend on its parameters.
    #[must_use]
    pub fn ports(&self) -> Option<&[PortSpec]> {
        match &self.builds {
            Builds::Fixed(ports, _) => Some(ports),
            Builds::Ported(_) => None,
        }
    }
}

impl fmt::Debug for PeripheralType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeripheralType")
            .field("ports", &self.ports())
            .finish_non_exhaustive()
    }
}

/// The first port name in `ports` used twice.
pub(crate) fn repeated_port(ports: &[PortSpec]) -> Option<&str> {
    ports
        .iter()
        .enumerate()
        .find(|(index, port)| ports[..*index].iter().any(|other| other.name == port.name))
        .map(|(_, port)| port.name.as_str())
}

/// What registering can get wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// A type name already registered.
    #[error("peripheral type {0:?} is already registered")]
    Duplicate(String),
    /// Two ports of one type with one name.
    #[error("peripheral type {kind:?} declares port {port:?} twice")]
    Port {
        /// The type.
        kind: String,
        /// The port.
        port: String,
    },
}

/// Peripheral types by name.
#[derive(Debug, Default)]
pub struct Registry {
    /// The types.
    types: BTreeMap<String, PeripheralType>,
}

impl Registry {
    /// A registry with no types.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with the built-in generic types (`fixed`,
    /// `registers`, `recorder`, `clock`, `divider`, `edge-counter`).
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        builtin::register(&mut registry);
        registry
    }

    /// Registers the type `name`, whose peripherals have `ports`, built by
    /// `constructor`.
    ///
    /// # Errors
    ///
    /// A `name` already registered ([`RegistryError::Duplicate`]), or
    /// `ports` that repeat a name ([`RegistryError::Port`]).
    pub fn register(
        &mut self,
        name: impl Into<String>,
        ports: Vec<PortSpec>,
        constructor: impl Fn(&Context<'_>) -> Result<Box<dyn Peripheral>, ParameterError> + 'static,
    ) -> Result<(), RegistryError> {
        let name = name.into();
        if let Some(port) = repeated_port(&ports) {
            return Err(RegistryError::Port {
                kind: name,
                port: port.to_owned(),
            });
        }
        self.insert(name, Builds::Fixed(ports, Box::new(constructor)))
    }

    /// Registers the type `name`, whose `constructor` gives each peripheral
    /// its ports from its parameters; the loader checks them as it
    /// would a fixed list.
    ///
    /// # Errors
    ///
    /// A `name` already registered ([`RegistryError::Duplicate`]).
    pub fn register_ported(
        &mut self,
        name: impl Into<String>,
        constructor: impl Fn(&Context<'_>) -> Result<Constructed, ParameterError> + 'static,
    ) -> Result<(), RegistryError> {
        self.insert(name.into(), Builds::Ported(Box::new(constructor)))
    }

    /// Adds the type `name`.
    fn insert(&mut self, name: String, builds: Builds) -> Result<(), RegistryError> {
        if self.types.contains_key(&name) {
            return Err(RegistryError::Duplicate(name));
        }
        self.types.insert(name, PeripheralType { builds });
        Ok(())
    }

    /// The type `name`, if registered.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PeripheralType> {
        self.types.get(name)
    }

    /// The registered names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.types.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::Environment;
    use super::ParameterError;
    use super::ParameterValue;
    use super::Parameters;
    use super::PortSpec;
    use super::Registry;
    use super::RegistryError;
    use crate::builtin::Fixed;
    use crate::signal::Kind;

    #[test]
    fn a_table_parses_into_the_board_s_own_values() {
        let parameters =
            Parameters::parse("a = [1, 2.5]\nb = { c = true }\nd = 1979-05-27").unwrap();
        let table = parameters.table();
        assert_eq!(
            table["a"].as_array(),
            Some([ParameterValue::Integer(1), ParameterValue::Float(2.5)].as_slice())
        );
        let inner = table["b"].as_table().unwrap();
        assert_eq!(inner["c"].as_bool(), Some(true));
        assert_eq!(
            table["d"],
            ParameterValue::Datetime("1979-05-27".to_owned())
        );
        assert_eq!(ParameterValue::from("x").as_str(), Some("x"));
        assert_eq!(ParameterValue::from(3).as_integer(), Some(3));
        assert_eq!(ParameterValue::from(3).as_float(), None);
        assert!(Parameters::parse("a = ").is_err());
    }

    #[test]
    fn a_host_s_toml_text_gives_a_table_it_owns() {
        let table = Parameters::parse("initial = 0x8000\ncs = \"low\"")
            .unwrap()
            .into_table();
        assert_eq!(table["initial"], ParameterValue::Integer(0x8000));
        assert_eq!(table["cs"], ParameterValue::from("low"));
    }

    #[test]
    fn parameters_are_typed_and_strays_found() {
        let parameters = Parameters::parse("a = 3\nb = \"x\"\nc = 300\nd = 1").unwrap();
        assert_eq!(parameters.integer::<u8>("a"), Ok(Some(3)));
        assert_eq!(
            parameters.integer::<u8>("b"),
            Err(ParameterError::Type {
                key: "b".to_owned(),
                expected: "an integer"
            })
        );
        assert_eq!(
            parameters.integer::<u8>("c"),
            Err(ParameterError::Range {
                key: "c".to_owned(),
                value: 300
            })
        );
        assert_eq!(
            parameters.required::<u8>("e"),
            Err(ParameterError::Missing("e".to_owned()))
        );
        assert_eq!(parameters.unread(), Some("d".to_owned()));
    }

    #[test]
    fn names_and_ports_are_unique() {
        let mut registry = Registry::with_builtins();
        let fixed = || Ok(Box::new(Fixed::default()) as _);
        assert_eq!(
            registry.register("fixed", Vec::new(), move |_| fixed()),
            Err(RegistryError::Duplicate("fixed".to_owned()))
        );
        let ports = vec![
            PortSpec::input("a", Kind::Logic),
            PortSpec::output("a", Kind::Logic),
        ];
        assert_eq!(
            registry.register("twice", ports, move |_| fixed()),
            Err(RegistryError::Port {
                kind: "twice".to_owned(),
                port: "a".to_owned()
            })
        );
        assert!(registry.names().any(|name| name == "divider"));
    }

    #[test]
    fn each_peripheral_has_its_own_seed() {
        let environment = Environment {
            seed: 7,
            ..Environment::default()
        };
        assert_ne!(environment.seed_for("ocxo"), environment.seed_for("gps"));
        assert_eq!(environment.seed_for("ocxo"), environment.seed_for("ocxo"));
    }
}
