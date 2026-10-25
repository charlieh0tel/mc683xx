//! The octal buffer the CPU reads, 74x244: eight inputs from nets or
//! fixed levels (a DIP switch bank), driven onto the bus while the
//! select enables it ([docs/peripherals.md,
//! 74x244](https://github.com/charlieh0tel/mc683xx/blob/main/docs/peripherals.md#74x244-octal-buffer)).
//!
//! The SN74HC244 (SCLS130I) is two 4-bit buffers, each passing its A
//! inputs to its Y outputs while its OE is low (SN74HC244 7.1, Table
//! 7-1).  On the bus both OEs are the read strobe and the Y outputs
//! DATA\[15:8\], as an 8-bit port's (SIMRM 5.3.3): a read gives the byte
//! on the A inputs, on DATA\[15:8\] for a word, the lower byte not
//! driven.  A write reaches nothing: wire the peripheral `role = "read"`.
//!
//! An input takes its level from `levels` until a net drives it, so a
//! board ties the inputs it does not wire.  `A0` to `A3` are 1A1 to
//! 1A4 and `A4` to `A7` 2A1 to 2A4, `An` bit n of the byte read; `A`,
//! a bus input, sets all eight at once from its low byte (a host's
//! switch bank through one external).

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

use crate::octal::LINES;

/// The buffer's registered name.
pub(crate) const NAME: &str = "74x244";

/// The format of a buffer's snapshot.
const VERSION: u32 = 1;
/// Its bus input's port, after A0 to A7.
const BUS: usize = LINES;
/// The inputs' levels until nets drive them, a one high.
const LEVELS: &str = "levels";
/// Their default: every input high, a switch bank's open positions on
/// pull-ups.
const OPEN: u8 = 0xff;

/// Registers the buffer: ports `A0` to `A7`, logic inputs, and `A`, a
/// bus input.
pub(crate) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    let ports = (0..LINES)
        .map(|bit| PortSpec::input(format!("A{bit}"), Kind::Logic))
        .chain([PortSpec::input("A", Kind::Bus)])
        .collect();
    registry.register(NAME, ports, |context| {
        Ok(Box::new(Buffer::from_parameters(context)?))
    })
}

/// An octal buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Buffer {
    /// The levels on A0 to A7, a one high.
    inputs: u8,
}

impl Buffer {
    /// From `levels` (all high).
    fn from_parameters(context: &Context<'_>) -> Result<Self, ParameterError> {
        Ok(Self {
            inputs: context.parameters.integer(LEVELS)?.unwrap_or(OPEN),
        })
    }
}

impl Peripheral for Buffer {
    fn read(&mut self, access: PeripheralAccess, _: &mut Drive<'_>) -> Result<u32, BusError> {
        Ok(access.read_byte(self.inputs))
    }

    fn write(&mut self, _: PeripheralAccess, _: u32, _: &mut Drive<'_>) -> Result<(), BusError> {
        Ok(())
    }

    fn input(&mut self, port: usize, signal: Signal, _: Time, _: &mut Drive<'_>) {
        match (port, signal) {
            (BUS, Signal::Bus(value)) => self.inputs = value as u8,
            (bit, Signal::Level(high)) if bit < LINES => {
                let mask = 1 << bit;
                self.inputs = if high {
                    self.inputs | mask
                } else {
                    self.inputs & !mask
                };
            }
            _ => {}
        }
    }

    /// The levels on A0 to A7.
    fn inspect(&self) -> Inspection {
        Inspection::default().integer_in("inputs", self.inputs, Radix::Binary)
    }

    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(VERSION, self))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        *self = decode(VERSION, snapshot)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mc683xx_board::inspect::Radix;
    use mc683xx_board::inspect::Value;
    use mc683xx_board::peripheral::Drive;
    use mc683xx_board::peripheral::Peripheral;
    use mc683xx_board::signal::Signal;
    use mc683xx_board::time::Time;
    use mc683xx_cpu32::size::Size;

    use super::BUS;
    use super::Buffer;
    use super::OPEN;
    use super::VERSION;
    use crate::octal::tests::access;

    /// What a read of `size` gives.
    fn read(buffer: &mut Buffer, size: Size) -> u32 {
        buffer
            .read(access(size, Time::ZERO), &mut Drive::new(Time::ZERO))
            .unwrap()
    }

    #[test]
    fn it_reads_its_inputs_and_ignores_writes() {
        let mut buffer = Buffer { inputs: OPEN };
        assert_eq!(read(&mut buffer, Size::Byte), 0xff);
        buffer.input(3, Signal::Level(false), Time(1), &mut Drive::new(Time(1)));
        assert_eq!(read(&mut buffer, Size::Byte), 0xf7);
        buffer
            .write(access(Size::Byte, Time(2)), 0, &mut Drive::new(Time(2)))
            .unwrap();
        assert_eq!(
            read(&mut buffer, Size::Byte),
            0xf7,
            "a write reaches nothing"
        );
        assert_eq!(read(&mut buffer, Size::Word), 0xf700, "on DATA[15:8]");
    }

    #[test]
    fn it_shows_its_inputs_in_binary() {
        let inspection = Buffer { inputs: 0xa5 }.inspect();
        assert_eq!(inspection.get("inputs"), Some(&Value::Integer(0xa5)));
        assert_eq!(inspection.fields[0].radix, Radix::Binary);
    }

    #[test]
    fn its_bus_input_sets_every_line() {
        let mut buffer = Buffer { inputs: OPEN };
        buffer.input(BUS, Signal::Bus(0x1a5), Time(1), &mut Drive::new(Time(1)));
        assert_eq!(read(&mut buffer, Size::Byte), 0xa5);
        buffer.input(0, Signal::Level(false), Time(2), &mut Drive::new(Time(2)));
        assert_eq!(read(&mut buffer, Size::Byte), 0xa4);
    }

    #[test]
    fn it_restores_what_it_saved() {
        let buffer = Buffer { inputs: 0x3c };
        let mut restored = Buffer { inputs: OPEN };
        restored.restore(&buffer.save().unwrap()).unwrap();
        assert_eq!(restored, buffer);
    }

    /// Every byte is a level a run could have, and it has no parameter
    /// a snapshot could differ in: only bytes that do not decode are
    /// refused.
    #[test]
    fn it_refuses_only_what_does_not_decode() {
        let mut buffer = Buffer { inputs: 0x3c };
        let snapshot = Buffer { inputs: 0xc3 }.save().unwrap();
        for refused in [&snapshot[..4], &[2, 0, 0, 0, 0xc3]] {
            assert!(buffer.restore(refused).is_err());
        }
        assert_eq!(buffer, Buffer { inputs: 0x3c });
    }

    #[test]
    fn the_format_matches_its_golden() {
        snapshot_golden::check(
            env!("CARGO_MANIFEST_DIR"),
            "74x244",
            &[("74x244", VERSION)],
            &Buffer { inputs: 0x5a }.save().unwrap(),
        );
    }
}
