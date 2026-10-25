use std::cell::RefCell;
use std::ops::ControlFlow;
use std::rc::Rc;

use super::Deviations;
use super::End;
use super::FaultCause;
use super::Intercepted;
use super::Limits;
use crate::address::Address;
use crate::cpu::hooks::MemoryAccess;
use crate::exception::CycleKind;
use crate::exception::Vector;
use crate::flat::Machine;
use crate::memory::PAGE_BYTES;
use crate::memory::Permissions;
use crate::registers::Registers;
use crate::registers::status;
use crate::size::Size;

/// RAM from zero.
const RAM: u32 = 0x1_0000;
/// The code.
const CODE: u32 = 0x1000;
/// The supervisor stack's top.
const STACK: u32 = 0x8000;
/// A handler.
const HANDLER: u32 = 0x4000;
/// An address nothing maps.
const NOWHERE: u32 = 0x9000_0000;
/// TRAP #0 (CPU32RM 4, TRAP).
const TRAP_0: u16 = 0x4e40;
/// DIVU.W (A0)+,D0.
const DIVU_POSTINCREMENT: u16 = 0x80d8;
/// MOVE.W (A0),D0.
const MOVE_WORD_A0_TO_D0: u16 = 0x3010;
/// MOVE.L (A0),D0.
const MOVE_LONG_A0_TO_D0: u16 = 0x2010;
/// MOVEQ #1,D1.
const MOVEQ_1_D1: u16 = 0x7201;
/// NOP.
const NOP: u16 = 0x4e71;

/// A machine with RAM from zero, `code` at [`CODE`], every vector at
/// [`HANDLER`], supervisor state, SP at [`STACK`].
fn machine(code: &[u16]) -> Machine {
    let mut machine = Machine::new(crate::flat::ADDRESS_BITS).unwrap();
    machine.map(Address(0), RAM, &[]).unwrap();
    for vector in 2..=255u32 {
        machine
            .write(Address(4 * vector), &HANDLER.to_be_bytes())
            .unwrap();
    }
    let code: Vec<u8> = code.iter().flat_map(|word| word.to_be_bytes()).collect();
    machine.write(Address(CODE), &code).unwrap();
    machine.cpu.registers.set_sr(status::S);
    machine.cpu.registers.ssp = STACK;
    machine.cpu.registers.pc = CODE;
    machine
}

/// One instruction, or whatever ends it sooner.
fn one(machine: &mut Machine) -> End {
    machine
        .run_limited(Limits {
            instructions: Some(1),
            ..Limits::default()
        })
        .end
}

#[test]
fn a_fault_stops_the_run_with_the_registers_as_the_instruction_began() {
    let mut machine = machine(&[MOVE_LONG_A0_TO_D0]);
    machine.cpu.set_stop_on_fault(true);
    machine.cpu.registers.set_address(0, NOWHERE);
    let before: Registers = machine.cpu.registers.clone();
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cycle, CycleKind::Read);
    assert_eq!(fault.cause, FaultCause::BusError);
    assert_eq!(fault.address, Address(NOWHERE));
    assert_eq!(fault.size, Size::Long);
    assert_eq!(fault.registers, before);
    assert_eq!(machine.cpu.registers, before, "nothing stacked");
    assert_eq!(machine.cpu.instructions(), 0);
}

#[test]
fn a_protected_page_and_an_odd_address_say_so() {
    let mut machine = machine(&[MOVE_WORD_A0_TO_D0]);
    machine.cpu.set_stop_on_fault(true);
    machine
        .protect(Address(0x2000), PAGE_BYTES, Permissions::NONE)
        .unwrap();
    machine.cpu.registers.set_address(0, 0x2000);
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cause, FaultCause::Protected);
    machine.cpu.registers.set_address(0, 0x3001);
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cause, FaultCause::AddressError);
}

#[test]
fn a_bad_stack_pointer_stops_without_a_halt_and_the_run_goes_on() {
    let mut machine = machine(&[TRAP_0]);
    machine.cpu.set_stop_on_fault(true);
    machine.cpu.registers.ssp = NOWHERE;
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cycle, CycleKind::Write);
    assert_eq!(fault.address, Address(NOWHERE - 2));
    assert_eq!(machine.cpu.registers.pc, CODE);
    assert_eq!(machine.cpu.registers.ssp, NOWHERE);
    machine.cpu.registers.ssp = STACK;
    one(&mut machine);
    assert_eq!(machine.cpu.registers.pc, HANDLER);
    assert_eq!(machine.cpu.registers.ssp, STACK - 8);
}

#[test]
fn an_intercepted_trap_stacks_nothing_and_can_be_taken_natively() {
    let mut machine = machine(&[TRAP_0]);
    machine.cpu.set_intercepted(Vector::trap(0), true);
    assert_eq!(
        one(&mut machine),
        End::Intercepted(Intercepted {
            vector: Vector::trap(0),
            pc: Address(CODE),
        })
    );
    assert_eq!(machine.cpu.registers.pc, CODE);
    assert_eq!(machine.cpu.registers.ssp, STACK);
    assert_eq!(machine.cpu.instructions(), 0);
    machine.cpu.take_natively_once();
    one(&mut machine);
    assert_eq!(machine.cpu.registers.pc, HANDLER);
    assert_eq!(machine.cpu.registers.ssp, STACK - 8);
}

#[test]
fn an_intercept_undoes_the_instruction_s_postincrement() {
    let mut machine = machine(&[DIVU_POSTINCREMENT]);
    machine.cpu.set_intercepted(Vector::ZERO_DIVIDE, true);
    machine.cpu.registers.set_address(0, 0x2000);
    let end = one(&mut machine);
    assert!(
        matches!(end, End::Intercepted(Intercepted { vector, .. }) if vector == Vector::ZERO_DIVIDE)
    );
    assert_eq!(machine.cpu.registers.address(0), 0x2000);
    assert!(machine.cpu.intercepts());
}

#[test]
fn the_misaligned_deviation_reads_bytes() {
    let mut machine = machine(&[MOVE_LONG_A0_TO_D0]);
    machine
        .write(Address(0x2001), &[0x12, 0x34, 0x56, 0x78])
        .unwrap();
    machine.cpu.registers.set_address(0, 0x2001);
    machine.cpu.set_deviations(Deviations {
        misaligned: true,
        untraced: false,
    });
    one(&mut machine);
    assert_eq!(machine.cpu.registers.data[0], 0x1234_5678);
}

#[test]
fn the_untraced_deviation_ignores_t1() {
    let mut machine = machine(&[NOP, NOP]);
    machine.cpu.registers.set_sr(status::S | status::T1);
    machine.cpu.set_deviations(Deviations {
        misaligned: false,
        untraced: true,
    });
    one(&mut machine);
    assert_eq!(machine.cpu.registers.pc, CODE + 2);
    machine.cpu.set_deviations(Deviations::default());
    one(&mut machine);
    assert_eq!(machine.cpu.registers.pc, HANDLER, "traced");
}

#[test]
fn a_memory_hook_has_the_registers_as_the_instruction_began() {
    let mut machine = machine(&[MOVEQ_1_D1, MOVE_WORD_A0_TO_D0]);
    machine.cpu.registers.set_address(0, 0x2000);
    let seen: Rc<RefCell<Vec<u32>>> = Rc::default();
    let into = Rc::clone(&seen);
    machine.cpu.add_read_hook(
        Address(0x2000)..=Address(0x2001),
        move |access: &MemoryAccess| {
            into.borrow_mut().push(access.pc.0);
            ControlFlow::Break(())
        },
    );
    one(&mut machine);
    machine.run_limited(Limits::default());
    assert_eq!(*seen.borrow(), [CODE + 2]);
    assert_eq!(machine.cpu.instruction_registers().data[1], 1);
    assert_eq!(machine.cpu.instruction_registers().pc, CODE + 2);
}
