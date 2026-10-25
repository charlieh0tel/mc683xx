//! A peripheral's state as a front end shows it: a tree of named fields.
//!
//! What [`Peripheral::inspect`] and [`Board::inspect`] give.  Read-only,
//! outside the snapshot and outside determinism: what it shows is the
//! peripheral's choice, and may change between versions.
//!
//! [`Peripheral::inspect`]: crate::peripheral::Peripheral::inspect
//! [`Board::inspect`]: crate::board::Board::inspect

use crate::time::Time;

/// The unit of a time: board time's picoseconds.
const PICOSECONDS: &str = "ps";

/// How a front end shows an integer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Radix {
    /// In decimal.
    #[default]
    Decimal,
    /// In hexadecimal: a register, an address.
    Hexadecimal,
    /// In binary: a register of bit fields, port levels.
    Binary,
}

/// A field's value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// An integer: a register, a count, a code.
    Integer(i64),
    /// A flag or a level.
    Boolean(bool),
    /// A measure: volts, seconds.
    Real(f64),
    /// Text: a state's name.
    Text(String),
    /// Bytes: a FIFO's contents.
    Bytes(Vec<u8>),
    /// Fields of its own: a channel, a register's bits.
    Group(Inspection),
}

/// A named field.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Its name.
    pub name: String,
    /// Its value.
    pub value: Value,
    /// Its unit, if it has one (`V`, `ps`).
    pub unit: Option<&'static str>,
    /// How an integer is shown.
    pub radix: Radix,
}

impl Field {
    /// `value` named `name`, with no unit, in decimal.
    #[must_use]
    pub fn new(name: impl Into<String>, value: Value) -> Self {
        Self {
            name: name.into(),
            value,
            unit: None,
            radix: Radix::Decimal,
        }
    }
}

/// A peripheral's state: its fields, in the order shown.  Built by
/// chaining, `Inspection::default().integer("count", 3)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inspection {
    /// The fields.
    pub fields: Vec<Field>,
}

impl Inspection {
    /// With `field` added.
    #[must_use]
    pub fn field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }

    /// With the integer `value` named `name`, in decimal.
    #[must_use]
    pub fn integer(self, name: impl Into<String>, value: impl Into<i64>) -> Self {
        self.field(Field::new(name, Value::Integer(value.into())))
    }

    /// With the integer `value` named `name`, in `radix`.
    #[must_use]
    pub fn integer_in(self, name: impl Into<String>, value: impl Into<i64>, radix: Radix) -> Self {
        self.field(Field {
            radix,
            ..Field::new(name, Value::Integer(value.into()))
        })
    }

    /// With the register `value` named `name`, in hexadecimal.
    #[must_use]
    pub fn register(self, name: impl Into<String>, value: impl Into<i64>) -> Self {
        self.integer_in(name, value, Radix::Hexadecimal)
    }

    /// With the count `value` named `name`, in decimal, saturating.
    #[must_use]
    pub fn count(self, name: impl Into<String>, value: usize) -> Self {
        self.integer(name, i64::try_from(value).unwrap_or(i64::MAX))
    }

    /// With the board time `value` named `name`, in picoseconds,
    /// saturating.
    #[must_use]
    pub fn time(self, name: impl Into<String>, value: Time) -> Self {
        let value = i64::try_from(value.0).unwrap_or(i64::MAX);
        self.field(Field {
            unit: Some(PICOSECONDS),
            ..Field::new(name, Value::Integer(value))
        })
    }

    /// With the flag `value` named `name`.
    #[must_use]
    pub fn boolean(self, name: impl Into<String>, value: bool) -> Self {
        self.field(Field::new(name, Value::Boolean(value)))
    }

    /// With the measure `value` in `unit` named `name`.
    #[must_use]
    pub fn real(self, name: impl Into<String>, value: f64, unit: &'static str) -> Self {
        self.field(Field {
            unit: Some(unit),
            ..Field::new(name, Value::Real(value))
        })
    }

    /// With the text `value` named `name`.
    #[must_use]
    pub fn text(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.field(Field::new(name, Value::Text(value.into())))
    }

    /// With the bytes `value` named `name`.
    #[must_use]
    pub fn bytes(self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.field(Field::new(name, Value::Bytes(value.into())))
    }

    /// With the group `value` named `name`.
    #[must_use]
    pub fn group(self, name: impl Into<String>, value: Self) -> Self {
        self.field(Field::new(name, Value::Group(value)))
    }

    /// The value of the field `name`, if there is one: the first of
    /// that name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| &field.value)
    }

    /// Whether it has no fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Inspection;
    use super::Radix;
    use super::Value;

    #[test]
    fn an_inspection_keeps_its_fields_in_order_with_their_hints() {
        let inspection = Inspection::default()
            .register("sr", 0x0c)
            .real("vout", 1.25, "V")
            .group("a", Inspection::default().boolean("ready", true));
        let names: Vec<&str> = inspection
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(names, ["sr", "vout", "a"]);
        assert_eq!(inspection.fields[0].radix, Radix::Hexadecimal);
        assert_eq!(inspection.fields[1].unit, Some("V"));
        assert_eq!(inspection.get("sr"), Some(&Value::Integer(0x0c)));
        let Some(Value::Group(group)) = inspection.get("a") else {
            panic!("a group");
        };
        assert_eq!(group.get("ready"), Some(&Value::Boolean(true)));
        assert!(Inspection::default().is_empty());
    }
}
