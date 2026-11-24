use std::cell::Cell;
use std::cell::RefCell;
use std::ops::ControlFlow;
use std::rc::Rc;

use super::Machine;
use super::MapError;
use super::PeekError;
use crate::BusError;
use crate::cpu::Stop;
use crate::cpu::bare::End;
use crate::cpu::bare::FaultCause;
use crate::cpu::bare::Limits;
use crate::cycles::Cycle;
use crate::exception::Vector;
use crate::hooks::Instruction;
use crate::hooks::MemoryAccess;
use crate::interrupt::IrqPin;
use crate::memory::Backing;
use crate::memory::PAGE_BYTES;
use crate::memory::Page;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::memory::Unserved;
use crate::peripheral::BusCycle;
use crate::peripheral::MappedPeripheral;
use crate::registers::status;
use crate::state;
use crate::state::Layer;
use crate::state::Save;
use crate::state::StateError;
use crate::state::Stateless;
use crate::state::decode;
use crate::state::decode_layer;
use crate::state::encode;
use crate::state::encode_layer;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

/// RAM from zero.
const RAM: u32 = 0x1_0000;
/// The supervisor stack's top.
const STACK: u32 = 0x8000;
/// Code far above 24 bits.
const CODE: u32 = 0x3140_0000;
/// A return address nothing maps.
const RETURN: u32 = 0x0080_0000;
/// A peripheral's base.
const PERIPHERAL: u32 = 0x00f0_0001;
/// NOP (CPU32RM 4, NOP).
const NOP: u16 = 0x4e71;
/// RTS.
const RTS: u16 = 0x4e75;
/// MOVEQ #1,D0.
const MOVEQ_1_D0: u16 = 0x7001;
/// MOVEQ #2,D0.
const MOVEQ_2_D0: u16 = 0x7002;
/// MOVE.L (A0),D0.
const MOVE_LONG_A0_TO_D0: u16 = 0x2010;
/// MOVE.L D0,(A0).
const MOVE_LONG_D0_TO_A0: u16 = 0x2080;
/// MOVE.L D0,(A1).
const MOVE_LONG_D0_TO_A1: u16 = 0x2280;
/// MOVE.W (A0),D0.
const MOVE_WORD_A0_TO_D0: u16 = 0x3010;
/// STOP #$2000.
const STOP: [u16; 2] = [0x4e72, 0x2000];
/// An interrupt handler's vector number.
const USER_VECTOR: u8 = 64;
/// Its handler.
const HANDLER: u32 = 0x4000;

/// `words` as bytes.
fn bytes(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_be_bytes()).collect()
}

/// A machine with RAM from zero, `code` at [`CODE`], supervisor state,
/// SP at [`STACK`] with [`RETURN`] pushed, PC at [`CODE`].
fn machine(code: &[u16]) -> Machine {
    let mut machine = Machine::new(super::ADDRESS_BITS).unwrap();
    machine.map(Address(0), RAM, &[]).unwrap();
    machine
        .map(Address(CODE), PAGE_BYTES, &bytes(code))
        .unwrap();
    machine
        .poke_bytes(
            Address(STACK - 4),
            FunctionCode::SupervisorData,
            &RETURN.to_be_bytes(),
        )
        .unwrap();
    machine.cpu.registers.set_sr(status::S);
    machine.cpu.registers.ssp = Address(STACK - 4);
    machine.cpu.registers.pc = Address(CODE);
    machine
}

/// Limits reaching [`RETURN`].
const fn reaching_return() -> Limits {
    Limits {
        cycles: None,
        instructions: None,
        steps: None,
        reach: Some(Address(RETURN)),
        self_loop: false,
    }
}

#[test]
fn a_call_at_a_high_address_runs_until_it_returns() {
    let mut machine = machine(&[MOVEQ_1_D0, RTS]);
    let run = machine.run_limited(reaching_return());
    assert_eq!(run.end, End::Reached);
    assert_eq!(machine.cpu.registers.data[0], 1);
    assert_eq!(machine.cpu.registers.pc.0, RETURN);
    assert_eq!(machine.cpu.instructions(), 2);
    assert!(run.cycles.clocks() > 0);
}

#[test]
fn an_instruction_limit_stops_the_run() {
    let mut machine = machine(&[NOP, NOP, NOP, RTS]);
    let run = machine.run_limited(Limits {
        instructions: Some(2),
        ..reaching_return()
    });
    assert_eq!(run.end, End::Instructions);
    assert_eq!(machine.cpu.registers.pc.0, CODE + 4);
}

#[test]
fn until_is_checked_before_the_code_hooks_there() {
    let mut machine = machine(&[NOP, RTS]);
    let called = Rc::new(RefCell::new(0));
    let count = Rc::clone(&called);
    machine
        .cpu
        .add_code_hook(Address(CODE)..=Address(CODE), move |_: &Instruction| {
            *count.borrow_mut() += 1;
            ControlFlow::Break(())
        });
    let at_code = Limits {
        reach: Some(Address(CODE)),
        ..Limits::default()
    };
    assert_eq!(machine.run_limited(at_code).end, End::Reached);
    assert_eq!(*called.borrow(), 0);
    let run = machine.run_limited(reaching_return());
    assert!(matches!(run.end, End::Stop(crate::cpu::Stop::Hook(_))));
    assert_eq!(*called.borrow(), 1);
    // A resume skips the hook; clearing the skip calls it again.
    machine.cpu.clear_resume();
    machine.run_limited(reaching_return());
    assert_eq!(*called.borrow(), 2);
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    assert_eq!(*called.borrow(), 2);
}

#[test]
fn maps_do_not_overlap_and_unmapped_bytes_are_refused() {
    let mut machine = machine(&[NOP]);
    let mapped = machine.map(Address(RAM - PAGE_BYTES), 2 * PAGE_BYTES, &[]);
    assert_eq!(mapped, Err(MapError::Mapped(Address(RAM - PAGE_BYTES))));
    assert_eq!(
        mapped.unwrap_err().to_string(),
        format!("{} is mapped already", Address(RAM - PAGE_BYTES))
    );
    let mut buffer = [0; 4];
    assert_eq!(
        machine.peek_bytes(Address(RAM - 2), FunctionCode::SupervisorData, &mut buffer),
        Err(PeekError::Unserved(Unserved(Address(RAM))))
    );
    assert_eq!(
        machine.poke_bytes(Address(RAM - 2), FunctionCode::SupervisorData, &[1; 4]),
        Err(PeekError::Unserved(Unserved(Address(RAM))))
    );
    machine
        .peek_bytes(Address(RAM - 4), FunctionCode::SupervisorData, &mut buffer)
        .unwrap();
    assert_eq!(buffer, [0; 4], "a refused write writes nothing");
    machine.unmap(Address(0), RAM).unwrap();
    assert_eq!(
        machine.peek_bytes(Address(0), FunctionCode::SupervisorData, &mut buffer),
        Err(PeekError::Unserved(Unserved(Address(0))))
    );
    machine.map(Address(0), PAGE_BYTES, &[7]).unwrap();
    machine
        .peek_bytes(Address(0), FunctionCode::SupervisorData, &mut buffer[..1])
        .unwrap();
    assert_eq!(buffer[0], 7);
}

/// Where the unmap tests map: away from [`machine`]'s maps.
const SPARE: u32 = 0x0010_0000;
/// Pages the unmap tests map.
const SPARE_PAGES: u32 = 4;

/// The bytes [`machine`]'s maps hold: RAM and a page of code.
const MACHINE_BYTES: usize = (RAM + PAGE_BYTES) as usize;

/// [`machine`] with [`SPARE_PAGES`] pages at [`SPARE`], each filled
/// with its page number.
fn spare() -> Machine {
    let mut machine = machine(&[MOVEQ_1_D0, RTS]);
    let contents: Vec<u8> = (0..SPARE_PAGES)
        .flat_map(|page| [page as u8; PAGE_BYTES as usize])
        .collect();
    machine
        .map(Address(SPARE), SPARE_PAGES * PAGE_BYTES, &contents)
        .unwrap();
    machine
}

/// The first byte of each spare page, [`None`] where it is unmapped.
fn spare_pages(machine: &Machine) -> Vec<Option<u8>> {
    (0..SPARE_PAGES)
        .map(|page| {
            let mut byte = [0];
            let at = Address(SPARE + page * PAGE_BYTES);
            machine
                .peek_bytes(at, FunctionCode::SupervisorData, &mut byte)
                .ok()
                .map(|()| byte[0])
        })
        .collect()
}

#[test]
fn mapping_and_unmapping_again_and_again_holds_no_more() {
    let mut machine = machine(&[MOVEQ_1_D0, RTS]);
    let before = machine.cpu.memory.held();
    assert_eq!(before.regions, 2);
    assert_eq!(before.bytes, MACHINE_BYTES);
    for _ in 0..1000 {
        machine
            .map(Address(SPARE), SPARE_PAGES * PAGE_BYTES, &[1])
            .unwrap();
        // Unmapped in two parts, as a split leaves it.
        machine
            .unmap(Address(SPARE + PAGE_BYTES), PAGE_BYTES)
            .unwrap();
        machine
            .unmap(Address(SPARE), SPARE_PAGES * PAGE_BYTES)
            .unwrap();
    }
    let after = machine.cpu.memory.held();
    assert_eq!(after.regions, before.regions);
    assert_eq!(after.bytes, before.bytes);
    assert_eq!(after.slots, before.slots + 2);
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
}

#[test]
fn a_partial_unmap_splits_or_trims_a_map_and_keeps_the_rest() {
    let mut machine = spare();
    let page = PAGE_BYTES as usize;
    machine
        .unmap(Address(SPARE + PAGE_BYTES), PAGE_BYTES)
        .unwrap();
    assert_eq!(spare_pages(&machine), [Some(0), None, Some(2), Some(3)]);
    let held = machine.cpu.memory.held();
    assert_eq!(held.regions, 4);
    assert_eq!(held.bytes, MACHINE_BYTES + 3 * page);
    // The front of the second part.
    machine
        .unmap(Address(SPARE + 2 * PAGE_BYTES), PAGE_BYTES)
        .unwrap();
    assert_eq!(spare_pages(&machine), [Some(0), None, None, Some(3)]);
    assert_eq!(machine.cpu.memory.held().bytes, MACHINE_BYTES + 2 * page);
    machine
        .poke_bytes(
            Address(SPARE + 3 * PAGE_BYTES),
            FunctionCode::SupervisorData,
            &[7],
        )
        .unwrap();
    assert_eq!(spare_pages(&machine), [Some(0), None, None, Some(7)]);
    // Both parts in one unmap, with the unmapped page between them.
    machine
        .unmap(Address(SPARE), SPARE_PAGES * PAGE_BYTES)
        .unwrap();
    assert_eq!(spare_pages(&machine), [None; SPARE_PAGES as usize]);
    let held = machine.cpu.memory.held();
    assert_eq!(held.regions, 2);
    assert_eq!(held.bytes, MACHINE_BYTES);
}

#[test]
fn a_trimmed_map_keeps_its_permissions_and_serves_the_core() {
    let mut machine = spare();
    let last = Address(SPARE + (SPARE_PAGES - 1) * PAGE_BYTES);
    machine
        .protect(last, PAGE_BYTES, Permissions::READ)
        .unwrap();
    machine
        .unmap(Address(SPARE), (SPARE_PAGES - 1) * PAGE_BYTES)
        .unwrap();
    assert_eq!(
        machine.cpu.memory.permissions(Space::User, last),
        Permissions::READ
    );
    assert_eq!(
        machine.cpu.memory.permissions(Space::User, Address(SPARE)),
        Permissions::ALL
    );
    machine.cpu.registers.set_address(0, last.0);
    machine
        .poke_bytes(
            Address(CODE),
            FunctionCode::SupervisorData,
            &bytes(&[MOVE_WORD_A0_TO_D0, RTS]),
        )
        .unwrap();
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    assert_eq!(machine.cpu.registers.data[0], 0x0303);
}

#[test]
fn code_mapped_again_where_it_was_unmapped_is_fetched_anew() {
    let mut machine = machine(&[MOVEQ_1_D0, RTS]);
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    machine.unmap(Address(CODE), PAGE_BYTES).unwrap();
    machine
        .map(Address(CODE), PAGE_BYTES, &bytes(&[MOVEQ_2_D0, RTS]))
        .unwrap();
    machine
        .poke_bytes(
            Address(STACK - 4),
            FunctionCode::SupervisorData,
            &RETURN.to_be_bytes(),
        )
        .unwrap();
    machine.cpu.registers.ssp = Address(STACK - 4);
    machine.cpu.registers.pc = Address(CODE);
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    assert_eq!(machine.cpu.registers.data[0], 2);
}

/// Spare page `page`'s entry in space `space`.
fn spare_entry(machine: &Machine, space: Space, page: u32) -> Page {
    machine
        .cpu
        .memory
        .page(space, Address(SPARE + page * PAGE_BYTES))
}

#[test]
fn unmap_leaves_the_bytes_of_a_region_the_host_mapped() {
    let mut machine = spare();
    let host_page = |page: u32| Address(SPARE + (SPARE_PAGES + page) * PAGE_BYTES);
    // A host region, mapped twice, the second time from its second page.
    let region = machine
        .cpu
        .memory
        .add_region([5, 6].map(|byte| [byte; PAGE_BYTES as usize]).concat());
    let backing = |offset| Backing { region, offset };
    for (at, offset, length) in [(0, 0, 2), (2, PAGE_BYTES, 1)] {
        let page = Page::BUS.reading(Some(backing(offset)));
        machine
            .cpu
            .memory
            .map(Space::Supervisor, host_page(at), length * PAGE_BYTES, page)
            .unwrap();
    }
    // An alias of a map's last page that the host made.
    let alias = spare_entry(&machine, Space::Supervisor, SPARE_PAGES - 1);
    machine
        .cpu
        .memory
        .map(Space::Supervisor, host_page(3), PAGE_BYTES, alias)
        .unwrap();
    let held = machine.cpu.memory.held();
    machine.unmap(host_page(0), 2 * PAGE_BYTES).unwrap();
    machine.unmap(host_page(3), PAGE_BYTES).unwrap();
    assert_eq!(machine.cpu.memory.held(), held);
    let mut byte = [0];
    machine
        .peek_bytes(host_page(2), FunctionCode::SupervisorData, &mut byte)
        .unwrap();
    assert_eq!(byte, [6]);
    assert_eq!(spare_pages(&machine), [Some(0), Some(1), Some(2), Some(3)]);
    assert_eq!(
        machine.peek_bytes(host_page(0), FunctionCode::SupervisorData, &mut byte),
        Err(PeekError::Unserved(Unserved(host_page(0))))
    );
}

#[test]
fn a_snapshot_restores_after_a_partial_unmap_only_if_taken_after_it() {
    let mut machine = spare();
    let before = machine.save().unwrap();
    machine.unmap(Address(SPARE), PAGE_BYTES).unwrap();
    let after = machine.save().unwrap();
    assert!(machine.restore(&before).is_err());
    machine
        .poke_bytes(
            Address(SPARE + PAGE_BYTES),
            FunctionCode::SupervisorData,
            &[9],
        )
        .unwrap();
    machine.restore(&after).unwrap();
    assert_eq!(spare_pages(&machine), [None, Some(1), Some(2), Some(3)]);
}

/// Each cycle a [`Logger`] saw, with a write's value.
type Log = Rc<RefCell<Vec<(BusCycle, Option<u32>)>>>;

/// A peripheral that logs its cycles and answers reads with their address.
#[derive(Debug, Default)]
struct Logger {
    /// Each cycle, with a write's value.
    cycles: Log,
}

crate::unsavable!(Logger, "a logger");

impl MappedPeripheral for Logger {
    fn read(&mut self, cycle: BusCycle) -> Result<u32, BusError> {
        self.cycles.borrow_mut().push((cycle, None));
        Ok(cycle.address.0)
    }

    fn write(&mut self, cycle: BusCycle, value: u32) -> Result<(), BusError> {
        self.cycles.borrow_mut().push((cycle, Some(value)));
        Ok(())
    }
}

#[test]
fn a_peripheral_takes_a_long_in_one_access() {
    let mut machine = machine(&[MOVE_LONG_A0_TO_D0, MOVE_LONG_D0_TO_A0, RTS]);
    let logger = Logger::default();
    let cycles = Rc::clone(&logger.cycles);
    machine
        .add_peripheral(
            Address(PERIPHERAL)..=Address(PERIPHERAL + 0xff),
            Box::new(logger),
        )
        .unwrap();
    machine.cpu.registers.set_address(0, PERIPHERAL + 1);
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    assert_eq!(machine.cpu.registers.data[0], PERIPHERAL + 1);
    let cycles = cycles.borrow();
    assert_eq!(cycles.len(), 2);
    assert_eq!(cycles[0].0.size, Size::Long);
    assert_eq!(cycles[1], (cycles[1].0, Some(PERIPHERAL + 1)));
    assert_eq!(cycles[1].0.size, Size::Long);
    assert!(!cycles[1].0.stacking);
}

#[test]
fn a_write_hook_sees_a_long_s_previous_value_but_not_a_peripheral_s() {
    let mut machine = machine(&[MOVE_LONG_D0_TO_A0, MOVE_LONG_D0_TO_A1, RTS]);
    machine
        .add_peripheral(
            Address(PERIPHERAL)..=Address(PERIPHERAL + 0xff),
            Box::new(Logger::default()),
        )
        .unwrap();
    machine
        .poke_bytes(
            Address(DATA),
            FunctionCode::SupervisorData,
            &0x1122_3344_u32.to_be_bytes(),
        )
        .unwrap();
    machine.cpu.registers.data[0] = 0x1234_5678;
    machine.cpu.registers.set_address(0, DATA);
    machine.cpu.registers.set_address(1, PERIPHERAL + 1);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let into = Rc::clone(&seen);
    machine
        .cpu
        .add_write_hook(Address(0)..=Address(u32::MAX), move |access| {
            into.borrow_mut()
                .push((access.address.0, access.size, access.previous));
            ControlFlow::Continue(())
        });
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    assert_eq!(
        *seen.borrow(),
        [
            (DATA, Size::Long, Some(0x1122_3344)),
            (PERIPHERAL + 1, Size::Long, None),
        ]
    );
}

#[test]
fn a_watched_odd_long_write_at_a_region_s_last_byte_is_an_address_error() {
    let mut machine = machine(&[MOVE_LONG_D0_TO_A0, RTS]);
    let last = SPARE + PAGE_BYTES - 1;
    machine.map(Address(SPARE), PAGE_BYTES, &[]).unwrap();
    machine.cpu.add_write_hook(
        Address(SPARE)..=Address(last),
        |_| ControlFlow::Continue(()),
    );
    machine.cpu.set_stop_on_fault(true);
    machine.cpu.registers.set_address(0, last);
    let End::Fault(fault) = machine.run_limited(reaching_return()).end else {
        panic!("no fault");
    };
    assert_eq!(fault.cause, FaultCause::AddressError);
    assert_eq!(fault.address, Address(last));
}

#[test]
fn a_word_past_a_peripheral_s_end_is_a_bus_error() {
    let mut machine = machine(&[MOVE_WORD_A0_TO_D0]);
    machine
        .add_peripheral(
            Address(PERIPHERAL)..=Address(PERIPHERAL),
            Box::new(Logger::default()),
        )
        .unwrap();
    machine.cpu.set_stop_on_fault(true);
    machine.cpu.registers.set_address(0, PERIPHERAL + 1);
    let End::Fault(fault) = machine.run_limited(reaching_return()).end else {
        panic!("no fault");
    };
    assert_eq!(fault.address, Address(PERIPHERAL + 1));
    assert_eq!(
        machine.add_peripheral(
            Address(0)..=Address(PERIPHERAL),
            Box::new(Logger::default())
        ),
        Err(MapError::Overlap(Address(PERIPHERAL)))
    );
    assert_eq!(
        machine.add_peripheral(
            Address(PERIPHERAL + 0x10)..=Address(PERIPHERAL + 0xf),
            Box::new(Logger::default())
        ),
        Err(MapError::Empty(Address(PERIPHERAL + 0x10)))
    );
}

/// LPSTOP #$2700 (CPU32RM 4.4, LPSTOP).
const LPSTOP: [u16; 3] = [0xf800, 0x01c0, 0x2700];
/// BKPT #7 (CPU32RM 4.4, BKPT).
const BKPT_7: u16 = 0x484f;

#[test]
fn lpstop_stops_with_no_peripheral_seeing_its_broadcast() {
    let mut machine = machine(&LPSTOP);
    let logger = Logger::default();
    let cycles = Rc::clone(&logger.cycles);
    machine
        .add_peripheral(Address(RAM)..=Address(0x3_ffff), Box::new(logger))
        .unwrap();
    let run = machine.run_limited(Limits::default());
    assert_eq!(run.end, End::Stop(Stop::Stopped));
    assert_eq!(machine.cpu.exception_taken(), None);
    assert_eq!(machine.cpu.registers.sr(), 0x2700);
    assert_eq!(machine.cpu.registers.pc.0, CODE + 6);
    assert!(cycles.borrow().is_empty());
}

#[test]
fn bkpt_with_no_cpu_space_responder_is_illegal() {
    // RAM away from the breakpoint acknowledge's addresses, a peripheral
    // over them: CPU space reaches no peripheral (CPU32RM 6.2.5).
    let vbr = 0x8000;
    let mut machine = Machine::new(super::ADDRESS_BITS).unwrap();
    machine.map(Address(vbr), RAM, &[]).unwrap();
    machine
        .map(Address(CODE), PAGE_BYTES, &bytes(&[BKPT_7]))
        .unwrap();
    let logger = Logger::default();
    let cycles = Rc::clone(&logger.cycles);
    machine
        .add_peripheral(Address(0)..=Address(0xff), Box::new(logger))
        .unwrap();
    let handler = vbr + 0x1000;
    machine
        .poke_bytes(
            Address(vbr + Vector::ILLEGAL_INSTRUCTION.offset()),
            FunctionCode::SupervisorData,
            &handler.to_be_bytes(),
        )
        .unwrap();
    machine.cpu.registers.vbr = Address(vbr);
    machine.cpu.registers.set_sr(status::S);
    machine.cpu.registers.ssp = Address(vbr + RAM);
    machine.cpu.registers.pc = Address(CODE);
    machine.run_limited(Limits {
        instructions: Some(1),
        ..Limits::default()
    });
    assert_eq!(
        machine.cpu.exception_taken().map(|taken| taken.vector()),
        Some(Vector::ILLEGAL_INSTRUCTION)
    );
    assert_eq!(machine.cpu.registers.pc.0, handler);
    assert!(cycles.borrow().is_empty());
}

#[test]
fn a_requested_interrupt_is_taken_through_its_vector() {
    let mut machine = machine(&[NOP, RTS]);
    machine
        .poke_bytes(
            Address(u32::from(USER_VECTOR) * 4),
            FunctionCode::SupervisorData,
            &HANDLER.to_be_bytes(),
        )
        .unwrap();
    machine
        .poke_bytes(
            Address(HANDLER),
            FunctionCode::SupervisorData,
            &bytes(&STOP),
        )
        .unwrap();
    machine.request_interrupt(IrqPin::try_from(3).unwrap(), Some(Vector(USER_VECTOR)));
    let run = machine.run_limited(Limits {
        reach: Some(Address(HANDLER)),
        ..Limits::default()
    });
    assert_eq!(run.end, End::Reached);
    assert_eq!(machine.cpu.instructions(), 0, "taken before the first");
    assert_eq!(u8::from(machine.cpu.registers.interrupt_mask()), 3);
    machine.clear_interrupt(IrqPin::try_from(3).unwrap());
    // With no budget, a stop does not wait.
    let run = machine.run_limited(Limits::default());
    assert_eq!(run.end, End::Stop(crate::cpu::Stop::Stopped));
}

#[test]
fn the_format_matches_its_golden() {
    let mut machine = machine(&[MOVEQ_1_D0, RTS]);
    machine.run_limited(reaching_return());
    machine.request_interrupt(IrqPin::try_from(3).unwrap(), Some(Vector(0x40)));
    machine
        .add_peripheral(Address(PERIPHERAL)..=Address(PERIPHERAL), Box::new(Fixed))
        .unwrap();
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "machine",
        &[
            ("machine", super::VERSION),
            ("cpu", state::VERSION),
            ("stateless", state::STATELESS_VERSION),
        ],
        &machine.save().unwrap(),
    );
}

#[test]
fn a_snapshot_restores_memory_registers_and_requests() {
    let mut machine = machine(&[MOVEQ_1_D0, RTS]);
    let snapshot = machine.save().unwrap();
    machine.run_limited(reaching_return());
    machine
        .poke_bytes(Address(0x100), FunctionCode::SupervisorData, &[9])
        .unwrap();
    machine.restore(&snapshot).unwrap();
    assert_eq!(machine.cpu.registers.pc.0, CODE);
    assert_eq!(machine.cpu.registers.data[0], 0);
    let mut byte = [0];
    machine
        .peek_bytes(Address(0x100), FunctionCode::SupervisorData, &mut byte)
        .unwrap();
    assert_eq!(byte, [0]);
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
}

/// The format of a [`Latch`]'s snapshot.
const LATCH_VERSION: u32 = 1;

/// A peripheral that keeps the last value written, and reads it back.
#[derive(Debug, Default)]
struct Latch(u32);

impl Save for Latch {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(LATCH_VERSION, &self.0))
    }

    fn restore(&mut self, snapshot: &[u8]) -> Result<(), StateError> {
        self.0 = decode(LATCH_VERSION, snapshot)?;
        Ok(())
    }
}

impl MappedPeripheral for Latch {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(self.0)
    }

    fn write(&mut self, _: BusCycle, value: u32) -> Result<(), BusError> {
        self.0 = value;
        Ok(())
    }
}

/// A peripheral with no state: it reads zero and ignores writes.
#[derive(Debug)]
struct Fixed;

impl Stateless for Fixed {}

impl MappedPeripheral for Fixed {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

/// The second latch's first address.
const SECOND: u32 = PERIPHERAL + 0xff;

/// A machine with latches at [`PERIPHERAL`] and [`SECOND`], holding 1 and 2,
/// and a stateless peripheral between them.
fn latched() -> Machine {
    let mut machine = machine(&[NOP]);
    machine
        .add_peripheral(Address(SECOND)..=Address(SECOND + 3), Box::new(Latch(2)))
        .unwrap();
    machine
        .add_peripheral(
            Address(PERIPHERAL)..=Address(PERIPHERAL + 3),
            Box::new(Latch(1)),
        )
        .unwrap();
    machine
        .add_peripheral(
            Address(PERIPHERAL + 4)..=Address(PERIPHERAL + 7),
            Box::new(Fixed),
        )
        .unwrap();
    machine
}

/// `machine`'s peripheral at `address`, and a long cycle to it.
fn peripheral_at(machine: &mut Machine, address: u32) -> (&mut dyn MappedPeripheral, BusCycle) {
    let wired = machine
        .bus
        .peripherals
        .iter_mut()
        .find(|wired| *wired.range.start() == address)
        .unwrap();
    let cycle = BusCycle::new(
        Address(address),
        Size::Long,
        FunctionCode::SupervisorData,
        Cycle::ZERO,
    );
    (wired.peripheral.as_mut(), cycle)
}

/// What `machine`'s peripheral at `address` reads as a long.
fn latch(machine: &mut Machine, address: u32) -> u32 {
    let (peripheral, cycle) = peripheral_at(machine, address);
    peripheral.read(cycle).unwrap()
}

/// Sets `machine`'s peripheral at `address` to `value`.
fn set_latch(machine: &mut Machine, address: u32, value: u32) {
    let (peripheral, cycle) = peripheral_at(machine, address);
    peripheral.write(cycle, value).unwrap();
}

#[test]
fn a_snapshot_holds_each_peripheral_in_the_order_added() {
    let mut machine = latched();
    let snapshot = machine.save().unwrap();
    set_latch(&mut machine, PERIPHERAL, 10);
    set_latch(&mut machine, SECOND, 20);
    machine.restore(&snapshot).unwrap();
    assert_eq!(latch(&mut machine, PERIPHERAL), 1);
    assert_eq!(latch(&mut machine, SECOND), 2);
    let state: super::MachineState =
        decode_layer(Layer::Machine, super::VERSION, &snapshot).unwrap();
    let held: Vec<Option<u32>> = state
        .peripherals
        .iter()
        .map(|bytes| decode(LATCH_VERSION, bytes).ok())
        .collect();
    assert_eq!(held, [Some(2), Some(1), None], "the order added");
}

#[test]
fn a_peripheral_that_cannot_save_fails_the_save_naming_it() {
    let mut machine = latched();
    machine
        .add_peripheral(
            Address(0x00f1_0000)..=Address(0x00f1_00ff),
            Box::new(Logger::default()),
        )
        .unwrap();
    let error = machine.save().unwrap_err();
    assert_eq!(
        error.to_string(),
        StateError::unsupported("a logger")
            .of_peripheral("the peripheral at 0xf10000")
            .to_string()
    );
}

#[test]
fn a_restore_into_another_count_of_peripheral_is_refused_and_changes_nothing() {
    let mut machine = latched();
    let snapshot = machine.save().unwrap();
    machine
        .add_peripheral(
            Address(0x00f1_0000)..=Address(0x00f1_0003),
            Box::new(Latch(3)),
        )
        .unwrap();
    machine.run_limited(Limits {
        instructions: Some(1),
        ..Limits::default()
    });
    set_latch(&mut machine, PERIPHERAL, 10);
    let pc = machine.cpu.registers.pc;
    assert!(matches!(
        machine.restore(&snapshot),
        Err(StateError::Mismatch(reason)) if reason == "peripherals"
    ));
    assert_eq!(machine.cpu.registers.pc, pc);
    assert_eq!(latch(&mut machine, PERIPHERAL), 10);
}

#[test]
fn a_peripheral_or_core_that_refuses_its_snapshot_leaves_every_peripheral_as_it_was() {
    let mut machine = latched();
    let good = machine.save().unwrap();
    let mut state: super::MachineState =
        decode_layer(Layer::Machine, super::VERSION, &good).unwrap();
    state.peripherals[1] = encode(LATCH_VERSION + 1, &5_u32);
    let bad_peripheral = encode_layer(Layer::Machine, super::VERSION, &state);
    let mut state: super::MachineState =
        decode_layer(Layer::Machine, super::VERSION, &good).unwrap();
    state.cpu.truncate(2);
    let bad_cpu = encode_layer(Layer::Machine, super::VERSION, &state);
    set_latch(&mut machine, PERIPHERAL, 10);
    set_latch(&mut machine, SECOND, 20);
    let refused = machine.restore(&bad_peripheral).unwrap_err();
    assert!(
        matches!(&refused, StateError::Peripheral { name, .. } if name == "the peripheral at 0xf00001"),
        "{refused}"
    );
    assert!(matches!(
        machine.restore(&bad_cpu),
        Err(StateError::Truncated)
    ));
    assert_eq!(latch(&mut machine, PERIPHERAL), 10);
    assert_eq!(latch(&mut machine, SECOND), 20);
}

/// A peripheral that saves but refuses every snapshot, its own too, which
/// breaks [`Save::restore`]'s contract; it counts the restores it is
/// asked for.
#[derive(Debug, Default)]
struct Refuser(Rc<Cell<u32>>);

impl Save for Refuser {
    fn save(&self) -> Result<Vec<u8>, StateError> {
        Ok(encode(LATCH_VERSION, &()))
    }

    fn restore(&mut self, _: &[u8]) -> Result<(), StateError> {
        self.0.set(self.0.get() + 1);
        Err(StateError::invalid("a refuser"))
    }
}

impl MappedPeripheral for Refuser {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Ok(())
    }
}

#[test]
fn a_refusing_peripheral_is_not_rolled_back_and_the_peripherals_before_it_are() {
    let mut machine = latched();
    let restores = Rc::new(Cell::new(0));
    machine
        .add_peripheral(
            Address(0x00f1_0000)..=Address(0x00f1_0003),
            Box::new(Refuser(Rc::clone(&restores))),
        )
        .unwrap();
    let snapshot = machine.save().unwrap();
    set_latch(&mut machine, PERIPHERAL, 10);
    set_latch(&mut machine, SECOND, 20);
    assert_eq!(
        machine.restore(&snapshot).unwrap_err().to_string(),
        StateError::invalid("a refuser")
            .of_peripheral("the peripheral at 0xf10000")
            .to_string()
    );
    assert_eq!(restores.get(), 1, "asked once, not to take its backup");
    assert_eq!(latch(&mut machine, PERIPHERAL), 10);
    assert_eq!(latch(&mut machine, SECOND), 20);
}

#[test]
fn a_stateless_peripheral_refuses_another_peripheral_snapshot() {
    assert!(matches!(
        Fixed.restore(&Latch(1).save().unwrap()),
        Err(StateError::Invalid(reason)) if reason == "bytes past the end of the snapshot"
    ));
    let later = state::STATELESS_VERSION + 1;
    assert!(matches!(
        Fixed.restore(&encode(later, &())),
        Err(StateError::Version { found, expected }) if (found, expected) == (later, state::STATELESS_VERSION)
    ));
    let mut fixed = Fixed;
    let snapshot = fixed.save().unwrap();
    assert!(fixed.restore(&snapshot).is_ok());
}

#[test]
fn stacking_writes_are_flagged_to_hooks() {
    // TRAP #0 with its vector at the handler.
    let mut machine = machine(&[0x4e40]);
    let trap = u32::from(Vector::trap(0).0) * 4;
    machine
        .poke_bytes(
            Address(trap),
            FunctionCode::SupervisorData,
            &HANDLER.to_be_bytes(),
        )
        .unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let into = Rc::clone(&seen);
    machine.cpu.add_write_hook(
        Address(0)..=Address(RAM - 1),
        move |access: &MemoryAccess| {
            into.borrow_mut().push((access.stacking, access.pc));
            ControlFlow::Continue(())
        },
    );
    machine.run_limited(Limits {
        reach: Some(Address(HANDLER)),
        ..Limits::default()
    });
    let seen = seen.borrow();
    assert_eq!(seen.len(), 4, "a four-word frame");
    assert!(
        seen.iter()
            .all(|&(stacking, pc)| stacking && pc == Address(CODE))
    );
}

#[test]
fn a_reset_reads_the_vectors() {
    let mut machine = machine(&[NOP]);
    machine
        .poke_bytes(
            Address(0),
            FunctionCode::SupervisorData,
            &STACK.to_be_bytes(),
        )
        .unwrap();
    machine
        .poke_bytes(
            Address(4),
            FunctionCode::SupervisorData,
            &CODE.to_be_bytes(),
        )
        .unwrap();
    machine.reset();
    assert_eq!(machine.cpu.registers.pc.0, CODE);
    assert_eq!(machine.cpu.registers.ssp.0, STACK);
}

/// Where a long operand lands in the tests of what the memory hooks
/// see.
const DATA: u32 = 0x2000;
/// MOVE.L D0,(xxx).L (CPU32RM 4, MOVE), its address to follow.
const MOVE_LONG_D0_TO_ABSOLUTE: u16 = 0x23c0;
/// MOVE.L D0,-(SP).
const MOVE_LONG_D0_TO_PUSH: u16 = 0x2f00;
/// ADDQ.L #4,SP (CPU32RM 4, ADDQ).
const ADDQ_4_SP: u16 = 0x588f;
/// ADDQ.L #8,SP.
const ADDQ_8_SP: u16 = 0x508f;
/// LINK A6,#-8 (CPU32RM 4, LINK).
const LINK_A6: [u16; 2] = [0x4e56, 0xfff8];
/// UNLK A6 (CPU32RM 4, UNLK).
const UNLK_A6: u16 = 0x4e5e;
/// MOVEM.L D0-D1,-(SP) (CPU32RM 4, MOVEM): the mask reversed for
/// predecrement, D0 at bit 15.
const MOVEM_LONG_PUSH_D0_D1: [u16; 2] = [0x48e7, 0xc000];
/// MOVEM.L (A0),D0-D1.
const MOVEM_LONG_A0_TO_D0_D1: [u16; 2] = [0x4cd0, 0x0003];

/// A memory hook's view of an access: address, size, value, and
/// whether it was a write.
type Seen = (u32, Size, u32, bool);

/// Read and write hooks on all of RAM in [`machine`], logging into
/// `log` the accesses of instructions other than the one at `skip`.
fn watch_ram(machine: &mut Machine, log: &Rc<RefCell<Vec<Seen>>>, skip: Address) {
    for write in [false, true] {
        let into = Rc::clone(log);
        let hook = move |access: &MemoryAccess| {
            if access.pc != skip {
                into.borrow_mut()
                    .push((access.address.0, access.size, access.value, write));
            }
            ControlFlow::Continue(())
        };
        let range = Address(0)..=Address(RAM - 1);
        if write {
            machine.cpu.add_write_hook(range, hook);
        } else {
            machine.cpu.add_read_hook(range, hook);
        }
    }
}

/// What the memory hooks see of `code` before its last instruction, an
/// RTS, with D0 `0x1234_5678`, D1 `0x9abc_def0`, A0 [`DATA`] (eight
/// bytes `0x11` to `0x88` there) and A6 `0x0bad_cafe`.
fn hooked(code: &[u16]) -> Vec<Seen> {
    let mut machine = machine(code);
    machine.cpu.registers.data[0] = 0x1234_5678;
    machine.cpu.registers.data[1] = 0x9abc_def0;
    machine.cpu.registers.set_address(0, DATA);
    machine.cpu.registers.set_address(6, 0x0bad_cafe);
    machine
        .poke_bytes(
            Address(DATA),
            FunctionCode::SupervisorData,
            &0x1122_3344_5566_7788_u64.to_be_bytes(),
        )
        .unwrap();
    let log = Rc::default();
    let rts = Address(CODE).offset(2 * (code.len() as u32 - 1));
    watch_ram(&mut machine, &log, rts);
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    log.take()
}

#[test]
fn a_hook_sees_a_long_to_an_absolute_address_once() {
    let code = [MOVE_LONG_D0_TO_ABSOLUTE, 0, DATA as u16, RTS];
    assert_eq!(hooked(&code), [(DATA, Size::Long, 0x1234_5678, true)]);
}

#[test]
fn a_hook_sees_a_long_push_once() {
    assert_eq!(
        hooked(&[MOVE_LONG_D0_TO_PUSH, ADDQ_4_SP, RTS]),
        [(STACK - 8, Size::Long, 0x1234_5678, true)]
    );
}

#[test]
fn a_hook_sees_link_s_push_and_unlk_s_pop_once() {
    assert_eq!(
        hooked(&[LINK_A6[0], LINK_A6[1], UNLK_A6, RTS]),
        [
            (STACK - 8, Size::Long, 0x0bad_cafe, true),
            (STACK - 8, Size::Long, 0x0bad_cafe, false),
        ]
    );
}

#[test]
fn a_hook_sees_each_register_of_a_long_movem_once() {
    let push = [
        MOVEM_LONG_PUSH_D0_D1[0],
        MOVEM_LONG_PUSH_D0_D1[1],
        ADDQ_8_SP,
        RTS,
    ];
    assert_eq!(
        hooked(&push),
        [
            (STACK - 8, Size::Long, 0x9abc_def0, true),
            (STACK - 12, Size::Long, 0x1234_5678, true),
        ]
    );
    assert_eq!(
        hooked(&[MOVEM_LONG_A0_TO_D0_D1[0], MOVEM_LONG_A0_TO_D0_D1[1], RTS]),
        [
            (DATA, Size::Long, 0x1122_3344, false),
            (DATA + 4, Size::Long, 0x5566_7788, false),
            // MOVEM to registers reads one word more (data_movement's
            // `movem_to_registers_reads_one_word_more`).
            (DATA + 8, Size::Word, 0, false),
        ]
    );
}

#[test]
fn a_long_that_faults_on_its_second_word_is_not_seen() {
    for code in [MOVE_LONG_A0_TO_D0, MOVE_LONG_D0_TO_A0] {
        let mut machine = machine(&[code]);
        machine.cpu.set_stop_on_fault(true);
        machine.cpu.registers.set_address(0, RAM - 2);
        let log = Rc::default();
        watch_ram(&mut machine, &log, Address(RETURN));
        let invalid = Rc::new(RefCell::new(Vec::new()));
        let into = Rc::clone(&invalid);
        machine
            .cpu
            .add_invalid_hook(Address(0)..=Address(u32::MAX), move |access| {
                into.borrow_mut().push((access.address.0, access.size));
                ControlFlow::Continue(())
            });
        let End::Fault(fault) = machine.run_limited(reaching_return()).end else {
            panic!("no fault");
        };
        assert_eq!(fault.address, Address(RAM));
        assert_eq!(log.take(), [], "the first word is not shown alone");
        assert_eq!(*invalid.borrow(), [(RAM, Size::Word)]);
    }
}

#[test]
fn without_the_long_bus_a_hook_sees_a_long_as_two_words() {
    let mut machine = machine(&[MOVE_LONG_D0_TO_A0, RTS]);
    machine.cpu.set_long_bus(false);
    machine.cpu.registers.data[0] = 0x1234_5678;
    machine.cpu.registers.set_address(0, DATA);
    let log = Rc::default();
    watch_ram(&mut machine, &log, Address(CODE + 2));
    assert_eq!(machine.run_limited(reaching_return()).end, End::Reached);
    assert_eq!(
        log.take(),
        [
            (DATA, Size::Word, 0x1234, true),
            (DATA + 2, Size::Word, 0x5678, true)
        ]
    );
}

/// Where [`Bytes`] are: just past [`machine`]'s RAM.
const BYTES: u32 = RAM;

/// Bytes a debugger peeks and pokes with no cycle, from `base`, each
/// call's first address and length noted in `calls`, its function code
/// in `codes`.
#[derive(Debug, Default)]
struct Bytes {
    /// Its first address.
    base: u32,
    /// The bytes.
    bytes: [u8; 4],
    /// Each peek's and poke's first address and length, in order.
    calls: Rc<RefCell<Vec<(u32, usize)>>>,
    /// Each peek's and poke's function code, in order.
    codes: Rc<RefCell<Vec<FunctionCode>>>,
}

impl Bytes {
    /// Bytes at [`BYTES`], their calls noted in `calls`.
    fn at_bytes(calls: &Rc<RefCell<Vec<(u32, usize)>>>) -> Box<Self> {
        Box::new(Self {
            base: BYTES,
            calls: Rc::clone(calls),
            ..Self::default()
        })
    }

    /// The index in `bytes` of a call at `cycle` of `length`, noted.
    fn noted(&self, cycle: BusCycle, length: usize) -> usize {
        assert_eq!(cycle.size, Size::Byte);
        self.calls.borrow_mut().push((cycle.address.0, length));
        self.codes.borrow_mut().push(cycle.function_code);
        (cycle.address.0 - self.base) as usize
    }
}

impl Stateless for Bytes {}

impl MappedPeripheral for Bytes {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }

    fn peek(&self, cycle: BusCycle, data: &mut [u8]) -> bool {
        assert!(!cycle.write);
        let at = self.noted(cycle, data.len());
        data.copy_from_slice(&self.bytes[at..at + data.len()]);
        true
    }

    fn poke(&mut self, cycle: BusCycle, data: &[u8]) -> bool {
        assert!(cycle.write);
        let at = self.noted(cycle, data.len());
        self.bytes[at..at + data.len()].copy_from_slice(data);
        true
    }
}

#[test]
fn a_debugger_s_access_peeks_and_pokes_a_peripheral() {
    let mut machine = machine(&[NOP]);
    let range = Address(BYTES)..=Address(BYTES + 3);
    let calls = Rc::default();
    machine
        .add_peripheral(range, Bytes::at_bytes(&calls))
        .unwrap();
    machine
        .poke_bytes(
            Address(BYTES),
            FunctionCode::SupervisorData,
            &[0x12, 0x34, 0x56, 0x78],
        )
        .unwrap();
    let mut bytes = [0; 4];
    machine
        .peek_bytes(Address(BYTES), FunctionCode::SupervisorData, &mut bytes)
        .unwrap();
    assert_eq!(bytes, [0x12, 0x34, 0x56, 0x78]);
    // memory and the peripheral in one access, across the boundary
    machine
        .poke_bytes(Address(BYTES - 1), FunctionCode::SupervisorData, &[9, 8])
        .unwrap();
    let mut across = [0; 2];
    machine
        .peek_bytes(
            Address(BYTES - 1),
            FunctionCode::SupervisorData,
            &mut across,
        )
        .unwrap();
    assert_eq!(across, [9, 8]);
    // past the peripheral, nothing: no byte written
    assert_eq!(
        machine.poke_bytes(Address(BYTES + 3), FunctionCode::SupervisorData, &[1, 2]),
        Err(PeekError::Unserved(Unserved(Address(BYTES + 4))))
    );
    machine
        .peek_bytes(
            Address(BYTES + 3),
            FunctionCode::SupervisorData,
            &mut across[..1],
        )
        .unwrap();
    assert_eq!(across[0], 0x78);
    // one call for each access's run of the peripheral's bytes
    assert_eq!(
        *calls.borrow(),
        [
            (BYTES, 4),
            (BYTES, 4),
            (BYTES, 1),
            (BYTES, 1),
            (BYTES + 3, 1)
        ]
    );
}

#[test]
fn a_debugger_s_access_gives_a_peripheral_its_function_code() {
    let mut machine = machine(&[NOP]);
    let codes = Rc::default();
    let bytes = Box::new(Bytes {
        base: BYTES,
        codes: Rc::clone(&codes),
        ..Bytes::default()
    });
    machine
        .add_peripheral(Address(BYTES)..=Address(BYTES + 3), bytes)
        .unwrap();
    machine
        .poke_bytes(Address(BYTES), FunctionCode::UserData, &[1])
        .unwrap();
    machine
        .peek_bytes(Address(BYTES), FunctionCode::UserProgram, &mut [0])
        .unwrap();
    assert_eq!(
        *codes.borrow(),
        [FunctionCode::UserData, FunctionCode::UserProgram]
    );
}

#[test]
fn a_debugger_s_access_calls_each_peripheral_once_with_its_bytes() {
    let mut machine = machine(&[NOP]);
    let (first, second) = (Rc::default(), Rc::default());
    machine
        .add_peripheral(Address(BYTES)..=Address(BYTES + 3), Bytes::at_bytes(&first))
        .unwrap();
    let next = Box::new(Bytes {
        base: BYTES + 4,
        calls: Rc::clone(&second),
        ..Bytes::default()
    });
    machine
        .add_peripheral(Address(BYTES + 4)..=Address(BYTES + 7), next)
        .unwrap();
    let image = [1, 2, 3, 4, 5, 6, 7, 8, 9];
    machine
        .poke_bytes(Address(BYTES - 1), FunctionCode::SupervisorData, &image)
        .unwrap();
    let mut back = [0; 9];
    machine
        .peek_bytes(Address(BYTES - 1), FunctionCode::SupervisorData, &mut back)
        .unwrap();
    assert_eq!(back, image);
    assert_eq!(*first.borrow(), [(BYTES, 4), (BYTES, 4)]);
    assert_eq!(*second.borrow(), [(BYTES + 4, 4), (BYTES + 4, 4)]);
}

#[test]
fn memory_mapped_inside_a_peripheral_s_range_splits_its_run() {
    let mut machine = machine(&[NOP]);
    let calls = Rc::default();
    let window = PAGE_BYTES * 3;
    let base = 0x0010_0000;
    let peripheral = Box::new(Wide {
        base,
        bytes: vec![0; window as usize],
        calls: Rc::clone(&calls),
    });
    machine
        .add_peripheral(Address(base)..=Address(base + window - 1), peripheral)
        .unwrap();
    machine
        .map(Address(base + PAGE_BYTES), PAGE_BYTES, &[])
        .unwrap();
    let image: Vec<u8> = (0..window).map(|byte| byte as u8).collect();
    machine
        .poke_bytes(Address(base), FunctionCode::SupervisorData, &image)
        .unwrap();
    let mut back = vec![0; window as usize];
    machine
        .peek_bytes(Address(base), FunctionCode::SupervisorData, &mut back)
        .unwrap();
    assert_eq!(back, image);
    let page = PAGE_BYTES as usize;
    assert_eq!(
        *calls.borrow(),
        [
            (base, page),
            (base + 2 * PAGE_BYTES, page),
            (base, page),
            (base + 2 * PAGE_BYTES, page)
        ]
    );
}

/// [`Bytes`] of any length.
#[derive(Debug)]
struct Wide {
    /// Its first address.
    base: u32,
    /// The bytes.
    bytes: Vec<u8>,
    /// Each peek's and poke's first address and length, in order.
    calls: Rc<RefCell<Vec<(u32, usize)>>>,
}

impl Stateless for Wide {}

impl MappedPeripheral for Wide {
    fn read(&mut self, _: BusCycle) -> Result<u32, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _: BusCycle, _: u32) -> Result<(), BusError> {
        Err(BusError)
    }

    fn peek(&self, cycle: BusCycle, data: &mut [u8]) -> bool {
        self.calls.borrow_mut().push((cycle.address.0, data.len()));
        let at = (cycle.address.0 - self.base) as usize;
        data.copy_from_slice(&self.bytes[at..at + data.len()]);
        true
    }

    fn poke(&mut self, cycle: BusCycle, data: &[u8]) -> bool {
        self.calls.borrow_mut().push((cycle.address.0, data.len()));
        let at = (cycle.address.0 - self.base) as usize;
        self.bytes[at..at + data.len()].copy_from_slice(data);
        true
    }
}

#[test]
fn a_peripheral_that_declines_is_unpeekable() {
    let mut machine = machine(&[NOP]);
    machine
        .add_peripheral(Address(BYTES)..=Address(BYTES + 3), Box::new(Latch(1)))
        .unwrap();
    let mut bytes = [0; 2];
    let unpeekable = PeekError::Unpeekable(Address(BYTES));
    assert_eq!(
        machine.peek_bytes(Address(BYTES - 1), FunctionCode::SupervisorData, &mut bytes),
        Err(unpeekable)
    );
    assert_eq!(
        machine.poke_bytes(Address(BYTES), FunctionCode::SupervisorData, &[1]),
        Err(unpeekable)
    );
    assert_eq!(
        unpeekable.to_string(),
        format!("not peekable at {}", Address(BYTES))
    );
    assert_eq!(unpeekable.address(), Address(BYTES));
}
