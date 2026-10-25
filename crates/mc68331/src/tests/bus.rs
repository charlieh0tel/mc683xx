//! Peripherals on no chip select, which take cycles by their own decode
//! of the address bus.

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::cpu::state::Stateless;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::interrupt::Acknowledge;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_cpu32::peripheral::BusCycle;
use mc683xx_cpu32::peripheral::MappedPeripheral;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::peripheral::Timed;
use mc683xx_modules::sim::chip_selects::Dsack;

use super::NOP;
use super::ROM_BYTES;
use super::Recorder;
use super::SIM;
use super::cycle;
use super::cycle_in;
use super::image;
use super::load_word;
use super::write_register;
use crate::chip::BuildError;
use crate::chip::Builder;
use crate::chip::Mc68331;
use crate::event::DsackFault;
use crate::event::Event;
use crate::host::AccessError;

/// Where the bus peripheral's window is: above CSBOOT's reset block,
/// which no select decodes out of reset (MC68331UM Table 4-24).
const WINDOW: u32 = 0x80_0000;
/// Its bytes.
const WINDOW_BYTES: u32 = 0x100;
/// PEPAR (MC68331UM D.3.10).
const PEPAR: u32 = SIM + 0x16;

/// A chip booting from CSBOOT with `peripheral` on no select at
/// `WINDOW`, driving `pins`, and acknowledging IRQ4.
fn decoding<D: MappedPeripheral + Timed + Stateless + 'static>(
    peripheral: D,
    pins: Dsack,
) -> (Mc68331, PeripheralId) {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let id = builder
        .bus_peripheral(
            peripheral,
            &[
                BusDecode::Window {
                    base: Address(WINDOW),
                    length: WINDOW_BYTES,
                },
                BusDecode::Acknowledge(IrqPin::new(4).unwrap()),
            ],
            pins,
        )
        .unwrap();
    builder.interrupt(id, IrqPin::new(4).unwrap());
    (builder.build(), id)
}

#[test]
fn a_bus_peripheral_takes_its_window_and_nothing_else() {
    let (mut chip, id) = decoding(Recorder::answering(0x1234), Dsack::Dsack1);
    assert_eq!(cycle(&mut chip, WINDOW, Size::Word, None), Ok((0x1234, 3)));
    assert_eq!(
        cycle(&mut chip, WINDOW + 0xfe, Size::Word, Some(0xabcd)),
        Ok((0, 3))
    );
    assert_eq!(
        cycle(&mut chip, WINDOW + WINDOW_BYTES, Size::Word, None),
        Err(BusError),
        "past the window nothing ends the cycle"
    );
    let recorder = chip.peripheral::<Recorder>(id).unwrap();
    assert_eq!(recorder.reads.len(), 1);
    assert_eq!(recorder.reads[0].select, None, "on no select");
    assert_eq!(recorder.writes[0].1, 0xabcd);
    // CPU space at the window's address is not its window
    assert_eq!(
        cycle_in(&mut chip, FunctionCode::CPU_SPACE, WINDOW, Size::Byte, None),
        Err(BusError)
    );
    assert_eq!(
        chip.peek(Address(WINDOW), Size::Word, FunctionCode::SUPERVISOR_DATA),
        Err(AccessError::Unpeekable),
        "a peripheral answers there"
    );
}

#[test]
fn a_bus_peripherals_dsack_waits_sizes_and_is_checked_against_pepar() {
    let (mut chip, id) = decoding(
        Recorder {
            wait: Some(2),
            ..Recorder::answering(0x1234)
        },
        Dsack::Dsack0,
    );
    // DSACK0 is an 8-bit port (SIMRM Table 5-2): two byte cycles of
    // two wait states each
    assert_eq!(cycle(&mut chip, WINDOW, Size::Word, None), Ok((0x3434, 10)));
    // BME with BMT 8 clocks (SIMRM 3.2): 2 wait states still end inside
    write_register(&mut chip, SIM + 0x20, 0x0007);
    assert_eq!(cycle(&mut chip, WINDOW, Size::Byte, None), Ok((0x34, 5)));
    chip.peripheral_mut::<Recorder>(id).unwrap().wait = Some(13);
    assert_eq!(cycle(&mut chip, WINDOW, Size::Byte, None), Err(BusError));
    // PEPAR 0: DSACK0 is PE0 (SIMRM 9.1): no DSACK
    let (mut chip, id) = decoding(Recorder::answering(0x1234), Dsack::Dsack0);
    chip.take_events();
    write_register(&mut chip, PEPAR, 0x0000);
    assert_eq!(cycle(&mut chip, WINDOW, Size::Byte, None), Err(BusError));
    assert_eq!(
        chip.take_events(),
        [Event::Dsack(
            Cycles::ZERO,
            DsackFault::BusUnassigned { peripheral: id }
        )]
    );
}

#[test]
fn a_bus_peripheral_gives_the_vector_of_its_acknowledge() {
    let (mut chip, id) = decoding(Recorder::answering(0x48), Dsack::Dsack1);
    chip.peripheral_mut::<Recorder>(id).unwrap().interrupt = true;
    let four = IrqPin::new(4).unwrap().level();
    assert_eq!(chip.board.level(), four);
    assert_eq!(
        chip.board.acknowledge(four, Cycles::ZERO).answer,
        Acknowledge::Vector(0x48)
    );
    let three = IrqPin::new(3).unwrap().level();
    assert_eq!(
        chip.board.acknowledge(three, Cycles::ZERO).answer,
        Acknowledge::Spurious,
        "not its level"
    );
    let reads = &chip.peripheral::<Recorder>(id).unwrap().reads;
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].function_code, FunctionCode::CPU_SPACE);
    assert_eq!(reads[0].address, 0xff_fff9);
}

#[test]
fn a_select_beside_a_bus_peripheral_ends_at_the_first_dsack() {
    let (mut chip, id) = decoding(
        Recorder {
            wait: Some(4),
            ..Recorder::answering(0x00ff)
        },
        Dsack::Dsack1,
    );
    // CS2 at $800000, 2 Kbytes, both bytes, read and write, zero wait
    // states (SIMRM 7.10): nothing wired, its DSACK ends the cycle
    // first, with the peripheral's data
    write_register(&mut chip, SIM + 0x54, 0x8000);
    write_register(&mut chip, SIM + 0x56, 0x7830);
    assert_eq!(cycle(&mut chip, WINDOW, Size::Word, None), Ok((0x00ff, 3)));
    assert_eq!(chip.peripheral::<Recorder>(id).unwrap().reads.len(), 1);
}

#[test]
fn a_bus_peripheral_takes_a_fixed_memorys_cycles() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let fixed = builder
        .fixed_memory(Address(WINDOW), vec![0x5a; 2048], true)
        .unwrap();
    let id = builder
        .bus_peripheral(
            Recorder::answering(0x1234),
            &[BusDecode::Window {
                base: Address(WINDOW + 0x10),
                length: 2,
            }],
            Dsack::Dsack1,
        )
        .unwrap();
    let mut chip = builder.build();
    assert_eq!(
        cycle(&mut chip, WINDOW + 0x10, Size::Word, None),
        Ok((0x1234, 3))
    );
    assert_eq!(
        cycle(&mut chip, WINDOW, Size::Word, None),
        Ok((0x5a5a, 2)),
        "the rest is the fixed memory's"
    );
    assert_eq!(chip.peripheral::<Recorder>(id).unwrap().reads.len(), 1);
    assert_eq!(chip.memory(fixed)[0x10], 0x5a);
}

/// A peripheral whose DSACK is 8 bits at even addresses and 16 at odd.
#[derive(Debug, Default)]
struct Mixed {
    /// Its reads' sizes.
    sizes: Vec<Size>,
}

impl MappedPeripheral for Mixed {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        self.sizes.push(cycle.size);
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }

    fn dsack_port(&mut self, cycle: BusCycle) -> Option<PortSize> {
        Some(if cycle.address & 2 == 0 {
            PortSize::Bits8
        } else {
            PortSize::Bits16
        })
    }
}

impl Stateless for Mixed {}

impl Timed for Mixed {}

#[test]
fn a_peripherals_dsack_port_sizes_each_cycle() {
    let (mut chip, id) = decoding(Mixed::default(), Dsack::Dsack1);
    cycle(&mut chip, WINDOW, Size::Word, None).unwrap();
    cycle(&mut chip, WINDOW + 2, Size::Word, None).unwrap();
    assert_eq!(
        chip.peripheral::<Mixed>(id).unwrap().sizes,
        [Size::Byte, Size::Byte, Size::Word]
    );
}

#[test]
fn a_bus_window_must_fit_the_address_lines() {
    for (base, length) in [(WINDOW, 0), (0xff_ff00, 0x200)] {
        let mut builder = Builder::default();
        assert_eq!(
            builder
                .bus_peripheral(
                    Recorder::default(),
                    &[BusDecode::Window {
                        base: Address(base),
                        length,
                    }],
                    Dsack::Dsack1,
                )
                .err(),
            Some(BuildError::Window {
                base: Address(base),
                length
            })
        );
    }
}

#[test]
fn the_core_leaves_a_bus_window_inside_a_fixed_memory_to_the_peripheral() {
    let program = [load_word(WINDOW + 0x10, 0), load_word(WINDOW, 1)].concat();
    let mut builder = Builder::default();
    builder
        .memory(image(&program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder
        .fixed_memory(Address(WINDOW), vec![0x5a; 2048], true)
        .unwrap();
    builder
        .bus_peripheral(
            Recorder::answering(0x1234),
            &[BusDecode::Window {
                base: Address(WINDOW + 0x10),
                length: 2,
            }],
            Dsack::Dsack1,
        )
        .unwrap();
    let mut chip = builder.build();
    chip.run_for(Cycles(200));
    let data = chip.cpu().registers.data;
    assert_eq!((data[0] & 0xffff, data[1] & 0xffff), (0x1234, 0x5a5a));
}
