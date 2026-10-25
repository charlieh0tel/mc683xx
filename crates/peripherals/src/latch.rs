//! The octal D latch on the bus, 74x374 and 74x373: eight flip-flops
//! the CPU writes through a select, their Q outputs on nets
//! ([docs/peripherals.md, 74x374 and
//! 74x373](https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md#74x374-and-74x373-octal-latch)).
//!
//! The '374 (SN74HC374, SCLS141G) takes D on CLK's rising edge, the
//! '373 (SN74HC373, SCLS140G) passes D while LE is high and holds it
//! when LE falls (SN74HC374 Table 7-1, SN74HC373 Table 6-1).  Strobed
//! by a write cycle, both hold the byte the cycle wrote from its end:
//! one model serves both.  Their D inputs are on DATA\[15:8\], as an
//! 8-bit port's are (SIMRM 5.3.3), so a word cycle through `lane =
//! "both"` writes its upper byte.  A read drives nothing onto the bus,
//! reads 0 and leaves Q alone: wire the peripheral `role = "write"`.  No
//! reset pin: RESET leaves Q as it was.
//!
//! OE (active low) puts Q in the high-impedance state without
//! touching the flip-flops (SN74HC374 7.1).  A logic net carries no
//! high impedance, so a disabled Q drives the level its pull gives.

use mc683xx_board::inspect::Inspection;
use mc683xx_board::inspect::Radix;
use mc683xx_board::peripheral::Drive;
use mc683xx_board::peripheral::Peripheral;
use mc683xx_board::peripheral::PeripheralAccess;
use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;
use mc683xx_board::registry::PortSpec;
use mc683xx_board::registry::Registry;
use mc683xx_board::registry::RegistryError;
use mc683xx_board::signal::Kind;
use mc683xx_board::signal::Signal;
use mc683xx_board::time::Time;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cpu::state::decode;
use mc683xx_cpu32::cpu::state::encode;
use serde::Deserialize;
use serde::Serialize;

use crate::octal::LEVELS;
use crate::octal::LINES;
use crate::octal::PULLS;
use crate::octal::level;

/// The edge-triggered register's registered name.
pub(crate) const NAME_374: &str = "74x374";
/// The transparent latch's.
pub(crate) const NAME_373: &str = "74x373";

/// The format of a latch's snapshot.
const VERSION: u32 = 1;
/// Its OE input's port, after Q0 to Q7.
const OE: usize = LINES;

/// The flip-flops' contents out of power-on (the datasheets give it
/// as unknown: SN74HC373 Table 6-1 note 3).
const INITIAL: &str = "initial";
/// OE's level until a net drives it: `"low"` (tied low, enabled) or
/// `"high"` (tied high, Q released).
const OE_LEVEL: &str = "oe";
/// What a released Q reads as on its net: `"up"` (high) or `"down"`.
const PULL: &str = "pull";

/// Registers the '374.
pub(crate) fn register_374(registry: &mut Registry) -> Result<(), RegistryError> {
    register(registry, NAME_374)
}

/// Registers the '373.
pub(crate) fn register_373(registry: &mut Registry) -> Result<(), RegistryError> {
    register(registry, NAME_373)
}

/// Registers the latch as `name`: ports `Q0` to `Q7`, logic outputs,
/// and `OE`, a logic input.
fn register(registry: &mut Registry, name: &str) -> Result<(), RegistryError> {
    let ports = (0..LINES)
        .map(|bit| PortSpec::output(format!("Q{bit}"), Kind::Logic))
        .chain([PortSpec::input("OE", Kind::Logic)])
        .collect();
    registry.register(name, ports, |context| {
        Ok(Box::new(Latch::from_parameters(context)?))
    })
}

/// An octal latch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Latch {
    /// The flip-flops.
    held: u8,
    /// OE's level: high releases Q.
    oe: bool,
    /// A released Q's level.
    pull: bool,
    /// Q as last driven, none before the first drive.
    driven: Option<u8>,
}

impl Latch {
    /// From `initial` (0), `oe` (`"low"`) and `pull` (`"up"`).
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        Ok(Self {
            held: context.parameters.integer(INITIAL)?.unwrap_or(0),
            oe: level(context, OE_LEVEL, LEVELS, false)?,
            pull: level(context, PULL, PULLS, true)?,
            driven: None,
        })
    }

    /// The levels on Q0 to Q7.
    const fn outputs(&self) -> u8 {
        match (self.oe, self.pull) {
            (false, _) => self.held,
            (true, true) => u8::MAX,
            (true, false) => 0,
        }
    }

    /// Drives each Q whose level changed.
    fn sync(&mut self, drive: &mut Drive<'_>) {
        let outputs = self.outputs();
        let changed = self.driven.map_or(u8::MAX, |driven| driven ^ outputs);
        for bit in (0..LINES).filter(|bit| changed & 1 << bit != 0) {
            drive.drive(bit, Signal::Level(outputs & 1 << bit != 0));
        }
        self.driven = Some(outputs);
    }
}

impl Peripheral for Latch {
    fn read(&mut self, _: PeripheralAccess, _: &mut Drive<'_>) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(
        &mut self,
        access: PeripheralAccess,
        value: u32,
        drive: &mut Drive<'_>,
    ) -> Result<(), BusError> {
        self.held = access.written_byte(value);
        self.sync(drive);
        Ok(())
    }

    fn power_on(&mut self, drive: &mut Drive<'_>) {
        self.sync(drive);
    }

    fn input(&mut self, port: usize, signal: Signal, _: Time, drive: &mut Drive<'_>) {
        if let (OE, Signal::Level(level)) = (port, signal) {
            self.oe = level;
            self.sync(drive);
        }
    }

    /// The flip-flops, OE and the levels on Q0 to Q7.
    fn inspect(&self) -> Inspection {
        Inspection::default()
            .integer_in("held", self.held, Radix::Binary)
            .boolean("oe", self.oe)
            .integer_in("outputs", self.outputs(), Radix::Binary)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(VERSION, self))
    }

    /// Refuses another `pull`'s snapshot, and levels last driven that
    /// are not the outputs the rest gives.
    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        let restored: Self = decode(VERSION, snapshot)?;
        if restored.pull != self.pull {
            return Err(StateError::mismatch("a latch of another pull"));
        }
        if restored
            .driven
            .is_some_and(|driven| driven != restored.outputs())
        {
            return Err(StateError::invalid("a latch's Q not as it drove them"));
        }
        *self = restored;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_board::inspect::Value;
    use mc683xx_board::peripheral::Drive;
    use mc683xx_board::peripheral::Peripheral;
    use mc683xx_board::signal::Signal;
    use mc683xx_board::time::Time;
    use mc683xx_cpu32::cpu::state::StateError;
    use mc683xx_cpu32::size::Size;

    use super::Latch;
    use super::OE;
    use super::VERSION;
    use crate::octal::tests::access;
    use crate::octal::tests::levels;

    /// A latch holding `held`, enabled, pulled up, not yet driven.
    const fn latch(held: u8) -> Latch {
        Latch {
            held,
            oe: false,
            pull: true,
            driven: None,
        }
    }

    #[test]
    fn power_on_drives_every_q_and_a_write_only_those_that_change() {
        let mut latch = latch(0x0f);
        let mut drive = Drive::new(Time::ZERO);
        latch.power_on(&mut drive);
        assert_eq!(
            levels(drive),
            (0..8).map(|bit| (bit, bit < 4)).collect::<Vec<_>>()
        );
        let mut drive = Drive::new(Time(5));
        latch
            .write(access(Size::Byte, Time(5)), 0x1e, &mut drive)
            .unwrap();
        assert_eq!(levels(drive), [(0, false), (4, true)]);
    }

    #[test]
    fn a_word_writes_its_upper_byte_and_a_read_drives_nothing() {
        let mut latch = latch(0);
        let mut drive = Drive::new(Time::ZERO);
        latch
            .write(access(Size::Word, Time::ZERO), 0xa55a, &mut drive)
            .unwrap();
        assert_eq!(latch.held, 0xa5);
        let read = latch
            .read(access(Size::Byte, Time::ZERO), &mut drive)
            .unwrap();
        assert_eq!(read, 0);
        assert_eq!(latch.held, 0xa5, "a read leaves Q alone");
    }

    #[test]
    fn oe_releases_q_to_its_pull_and_keeps_the_flip_flops() {
        let mut latch = latch(0x81);
        latch.power_on(&mut Drive::new(Time::ZERO));
        let mut drive = Drive::new(Time(1));
        latch.input(OE, Signal::Level(true), Time(1), &mut drive);
        assert_eq!(levels(drive).len(), 6, "the six low bits go high");
        latch
            .write(access(Size::Byte, Time(2)), 0x42, &mut Drive::new(Time(2)))
            .unwrap();
        let mut drive = Drive::new(Time(3));
        latch.input(OE, Signal::Level(false), Time(3), &mut drive);
        assert_eq!(
            levels(drive),
            (0..8)
                .filter(|bit| 0x42 & 1 << bit == 0)
                .map(|bit| (bit, false))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn it_shows_its_flip_flops_and_outputs() {
        let mut latch = latch(0x81);
        latch.input(OE, Signal::Level(true), Time(1), &mut Drive::new(Time(1)));
        let inspection = latch.inspect();
        assert_eq!(inspection.get("held"), Some(&Value::Integer(0x81)));
        assert_eq!(inspection.get("oe"), Some(&Value::Boolean(true)));
        assert_eq!(
            inspection.get("outputs"),
            Some(&Value::Integer(0xff)),
            "pulled up"
        );
    }

    #[test]
    fn it_restores_what_it_saved() {
        let mut latch = latch(0x5a);
        latch.power_on(&mut Drive::new(Time::ZERO));
        let mut restored = Latch {
            held: 0,
            oe: true,
            pull: true,
            driven: None,
        };
        restored.restore(&latch.save().unwrap()).unwrap();
        assert_eq!(restored, latch);
    }

    #[test]
    fn it_refuses_another_pull_and_outputs_it_did_not_drive() {
        let mut latch = latch(0x5a);
        latch.power_on(&mut Drive::new(Time::ZERO));
        let before = latch;
        let other = Latch {
            pull: false,
            ..latch
        };
        assert!(matches!(
            latch.restore(&other.save().unwrap()),
            Err(StateError::Mismatch(reason)) if reason == "a latch of another pull"
        ));
        let undriven = Latch {
            driven: Some(0x5b),
            ..latch
        };
        assert!(matches!(
            latch.restore(&undriven.save().unwrap()),
            Err(StateError::Invalid(_))
        ));
        assert_eq!(latch, before);
    }

    #[test]
    fn the_format_matches_its_golden() {
        let mut latch = latch(0x3c);
        latch.power_on(&mut Drive::new(Time::ZERO));
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "74x374",
            &[("74x374", VERSION)],
            &latch.save().unwrap(),
        );
    }
}
