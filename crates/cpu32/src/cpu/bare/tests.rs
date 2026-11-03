use std::cell::RefCell;
use std::ops::ControlFlow;
use std::rc::Rc;

use super::Deviations;
use super::End;
use super::FaultCause;
use super::Intercepted;
use super::Limits;
use crate::cpu::Stop;
use crate::cpu::hooks::MemoryAccess;
use crate::cycles::Cycles;
use crate::exception::CycleKind;
use crate::exception::Exception;
use crate::exception::MICROCODE_REVISION;
use crate::exception::Vector;
use crate::exception::format;
use crate::exception::ssw;
use crate::flat::Machine;
use crate::interrupt::InterruptLevel;
use crate::interrupt::IrqPin;
use crate::memory::PAGE_BYTES;
use crate::memory::Permissions;
use crate::registers::Registers;
use crate::registers::status;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::size::Size;

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
fn a_fault_stop_is_kept_until_the_next_run() {
    let mut machine = machine(&[MOVE_LONG_A0_TO_D0, NOP]);
    machine.cpu.set_stop_on_fault(true);
    machine.cpu.registers.set_address(0, NOWHERE);
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(machine.cpu.fault_stop(), Some(&fault));
    assert_eq!(
        fault.to_string(),
        "bus error: read of 4 bytes at 0x90000000, fc 5, pc 0x00001000"
    );
    machine.cpu.registers.set_address(0, STACK);
    assert_eq!(one(&mut machine), End::Instructions);
    assert_eq!(machine.cpu.fault_stop(), None);
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

/// One step, or whatever ends it sooner.
fn step(machine: &mut Machine) -> End {
    machine
        .run_limited(Limits {
            steps: Some(1),
            ..Limits::default()
        })
        .end
}

/// MOVE #$2000,SR: the mask down (CPU32RM 4, MOVE to SR).
const MOVE_TO_SR_UNMASKED: [u16; 2] = [0x46fc, 0x2000];

/// Points `vector` at `handler` and returns `handler`.
fn handle(machine: &mut Machine, vector: Vector, handler: u32) -> u32 {
    machine
        .write(Address(vector.offset()), &handler.to_be_bytes())
        .unwrap();
    handler
}

/// The word and long of the four-word frame at SP: its SR, its PC, and
/// its format and vector offset (CPU32RM 6.4).
fn frame(machine: &Machine) -> (u16, u32, u16) {
    let mut bytes = [0; 8];
    machine
        .read(Address(machine.cpu.registers.ssp), &mut bytes)
        .unwrap();
    let word = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
    (
        word(0),
        u32::from(word(2)) << 16 | u32::from(word(4)),
        word(6),
    )
}

#[test]
fn a_step_is_an_instruction_or_an_exception_up_to_its_handler() {
    let mut machine = machine(&[MOVEQ_1_D1, TRAP_0]);
    let trap = handle(&mut machine, Vector::trap(0), 0x4100);
    assert_eq!(step(&mut machine), End::Steps);
    assert_eq!(machine.cpu.registers.data[1], 1);
    assert_eq!(machine.cpu.registers.pc, CODE + 2);
    // the TRAP and its exception, not the handler's first instruction
    let run = machine.run_limited(Limits {
        steps: Some(1),
        ..Limits::default()
    });
    assert_eq!((run.end, run.steps), (End::Steps, 1));
    assert_eq!(machine.cpu.registers.pc, trap);
    assert_eq!(frame(&machine).1, CODE + 4, "the TRAP's frame");
    assert_eq!(frame(&machine).2, Vector::trap(0).offset() as u16);
    assert_eq!(machine.cpu.instructions(), 2);
}

#[test]
fn a_step_takes_an_interrupt_pending_at_its_start_or_its_end() {
    // pending at the start: taken, its handler not entered
    let mut first = machine(&[NOP]);
    let level_7 = handle(
        &mut first,
        Vector::autovector(InterruptLevel::new(7).unwrap()),
        0x4300,
    );
    first.request_interrupt(IrqPin::new(7).unwrap(), None);
    assert_eq!(step(&mut first), End::Steps);
    assert_eq!(first.cpu.registers.pc, level_7);
    assert_eq!(frame(&first).1, CODE, "the NOP not executed");
    assert_eq!(first.cpu.instructions(), 0);
    // pending at the end: the instruction unmasks it, it is taken, as
    // the trace before it (CPU32RM 6.2.10)
    let mut machine = machine(&MOVE_TO_SR_UNMASKED);
    machine
        .cpu
        .registers
        .set_sr(status::S | status::INTERRUPT_MASK);
    let level_3 = handle(
        &mut machine,
        Vector::autovector(InterruptLevel::new(3).unwrap()),
        0x4200,
    );
    machine.request_interrupt(IrqPin::new(3).unwrap(), None);
    let run = machine.run_limited(Limits {
        steps: Some(1),
        ..Limits::default()
    });
    assert_eq!((run.end, run.steps), (End::Steps, 2));
    assert_eq!(machine.cpu.registers.pc, level_3);
    assert_eq!(frame(&machine).1, CODE + 4, "after the MOVE");
    assert_eq!(machine.cpu.instructions(), 1);
}

#[test]
fn a_step_of_stop_ends_with_the_cpu_stopped_not_waiting() {
    // STOP #$2000
    let mut machine = machine(&[0x4e72, 0x2000]);
    assert_eq!(step(&mut machine), End::Steps);
    assert_eq!(machine.cpu.registers.pc, CODE + 4);
}

#[test]
fn a_step_cut_by_the_budget_is_finished_by_a_run_of_zero_steps() {
    // what the chip does when a quantum ends after the step's
    // instruction: the steps left, none, take only interrupts pending
    let mut machine = machine(&[MOVEQ_1_D1, NOP]);
    let run = machine.run_limited(Limits {
        steps: Some(1),
        cycles: Some(Cycles(1)),
        ..Limits::default()
    });
    assert_eq!((run.end, run.steps), (End::Stop(Stop::Budget), 1));
    let run = machine.run_limited(Limits {
        steps: Some(0),
        ..Limits::default()
    });
    assert_eq!((run.end, run.steps), (End::Steps, 0));
    assert_eq!(machine.cpu.registers.pc, CODE + 2, "the NOP not run");
}

#[test]
fn a_step_keeps_what_its_instruction_left_for_the_host() {
    // the look for an interrupt at the step's end, finding none,
    // leaves the instruction's registers as it began and its exception
    let mut machine = machine(&[MOVEQ_1_D1, TRAP_0]);
    machine.cpu.set_stop_on_fault(true);
    assert_eq!(step(&mut machine), End::Steps);
    assert_eq!(machine.cpu.instruction_registers().data[1], 0);
    assert_eq!(machine.cpu.instruction_registers().pc, CODE);
    assert_eq!(step(&mut machine), End::Steps);
    assert!(
        matches!(machine.cpu.exception_taken(), Some(Exception::Short { vector, .. }) if vector == Vector::trap(0))
    );
}

/// STOP #$2000 (CPU32RM 4, STOP).
const STOP_UNMASKED: [u16; 2] = [0x4e72, 0x2000];

/// A run of no instructions: what is pending at the boundary taken.
fn boundary_only(machine: &mut Machine) -> End {
    machine
        .run_limited(Limits {
            instructions: Some(0),
            ..Limits::default()
        })
        .end
}

#[test]
fn a_fault_stacking_a_pending_breakpoint_keeps_the_instruction_before_it() {
    let mut machine = machine(&[MOVEQ_1_D1, NOP]);
    machine.cpu.set_stop_on_fault(true);
    one(&mut machine);
    let mut bad_stack = machine.cpu.registers.clone();
    assert_eq!(bad_stack.data[1], 1);
    bad_stack.ssp = NOWHERE;
    machine.cpu.set_breakpoint_pending(true);
    machine.cpu.registers.ssp = NOWHERE;
    let End::Fault(fault) = boundary_only(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cycle, CycleKind::Write);
    assert_eq!(fault.address, Address(NOWHERE - 2));
    assert_eq!(fault.registers, bad_stack, "the MOVEQ kept");
    assert_eq!(machine.cpu.registers, bad_stack);
    assert!(machine.cpu.breakpoint_pending(), "left for the next run");
    // SSP fixed, the run goes on: vector 12 (CPU32RM 6.2.6), its frame
    // returning after the MOVEQ
    machine.cpu.registers.ssp = STACK;
    boundary_only(&mut machine);
    assert!(!machine.cpu.breakpoint_pending());
    assert_eq!(machine.cpu.registers.pc, HANDLER);
    assert_eq!(machine.cpu.registers.ssp, STACK - 12);
    assert_eq!(frame(&machine).1, CODE + 2);
}

#[test]
fn a_fault_stacking_a_breakpoint_after_stop_keeps_the_interrupt_before_it() {
    let mut machine = machine(&STOP_UNMASKED);
    machine.cpu.set_stop_on_fault(true);
    one(&mut machine);
    let level_3 = handle(
        &mut machine,
        Vector::autovector(InterruptLevel::new(3).unwrap()),
        0x4200,
    );
    machine.cpu.set_breakpoint_pending(true);
    // room for the interrupt's four words, not the breakpoint's six
    machine.cpu.registers.ssp = 8;
    machine.request_interrupt(IrqPin::new(3).unwrap(), None);
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cycle, CycleKind::Write);
    assert_eq!(fault.registers.pc, level_3, "the interrupt kept");
    assert_eq!(fault.registers.ssp, 0);
    assert_eq!(fault.registers.interrupt_mask(), 3);
    assert!(machine.cpu.breakpoint_pending(), "left for the next run");
}

#[test]
fn a_fault_stacking_the_interrupt_that_ends_stop_leaves_its_breakpoint_pending() {
    let mut machine = machine(&STOP_UNMASKED);
    machine.cpu.set_stop_on_fault(true);
    one(&mut machine);
    machine.cpu.set_breakpoint_pending(true);
    machine.cpu.registers.ssp = NOWHERE;
    machine.request_interrupt(IrqPin::new(3).unwrap(), None);
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.address, Address(NOWHERE - 2));
    assert_eq!(fault.registers.pc, CODE + 4, "the interrupt not taken");
    assert!(
        machine.cpu.breakpoint_pending(),
        "not acknowledged: no exception processing ended"
    );
}

/// LPSTOP #$2000 (CPU32RM 4.4, LPSTOP).
const LPSTOP_UNMASKED: [u16; 3] = [0xf800, 0x01c0, 0x2000];

#[test]
fn a_fault_stacking_the_interrupt_that_ends_stop_leaves_the_cpu_stopped() {
    for code in [&STOP_UNMASKED[..], &LPSTOP_UNMASKED] {
        let after = CODE + 2 * code.len() as u32;
        let mut machine = machine(code);
        machine.cpu.set_stop_on_fault(true);
        one(&mut machine);
        let stopped_sr = machine.cpu.registers.sr();
        machine.cpu.registers.ssp = NOWHERE;
        let pin = IrqPin::new(3).unwrap();
        machine.request_interrupt(pin, None);
        let End::Fault(fault) = one(&mut machine) else {
            panic!("no fault");
        };
        assert_eq!(fault.registers.pc, after, "the interrupt not taken");
        assert_eq!(fault.registers.sr(), stopped_sr, "SR as loaded");
        assert!(machine.cpu.stopped, "not ended (CPU32RM 4.4, STOP, LPSTOP)");
        // the request withdrawn: still stopped, nothing run past it
        machine.clear_interrupt(pin);
        assert_eq!(one(&mut machine), End::Stop(Stop::Stopped));
        assert_eq!(machine.cpu.registers.pc, after);
        // requested again with SSP fixed: the interrupt is taken
        machine.cpu.registers.ssp = STACK;
        let level_3 = handle(
            &mut machine,
            Vector::autovector(InterruptLevel::new(3).unwrap()),
            0x4200,
        );
        machine.request_interrupt(pin, None);
        let run = machine.run_limited(Limits {
            steps: Some(0),
            ..Limits::default()
        });
        assert_eq!((run.end, run.steps), (End::Steps, 1));
        assert!(!machine.cpu.stopped);
        assert_eq!(machine.cpu.registers.pc, level_3);
        assert_eq!(frame(&machine).1, after);
    }
}

#[test]
fn a_fault_stacking_an_interrupt_leaves_the_next_instruction_s_hooks_to_run() {
    let mut machine = machine(&[MOVEQ_1_D1, NOP]);
    machine.cpu.set_stop_on_fault(true);
    let calls = Rc::new(RefCell::new(0));
    let into = Rc::clone(&calls);
    machine
        .cpu
        .add_code_hook(Address(CODE + 2)..=Address(CODE + 2), move |_| {
            *into.borrow_mut() += 1;
            ControlFlow::Continue(())
        });
    one(&mut machine);
    machine.cpu.registers.ssp = NOWHERE;
    let pin = IrqPin::new(3).unwrap();
    machine.request_interrupt(pin, None);
    assert!(matches!(one(&mut machine), End::Fault(_)));
    assert_eq!(*calls.borrow(), 0, "the NOP not reached");
    machine.clear_interrupt(pin);
    one(&mut machine);
    assert_eq!(machine.cpu.registers.pc, CODE + 4);
    assert_eq!(*calls.borrow(), 1, "called before the NOP");
}

/// ADDQ.W #1,(A0) (CPU32RM 4, ADDQ).
const ADDQ_1_INDIRECT_A0: u16 = 0x5250;
/// Where [`ADDQ_1_INDIRECT_A0`] adds.
const COUNTER: u32 = 0x2000;

/// The word at `address`.
fn word_at(machine: &Machine, address: u32) -> u16 {
    let mut bytes = [0; 2];
    machine.read(Address(address), &mut bytes).unwrap();
    u16::from_be_bytes(bytes)
}

/// The six-word frame's instruction address at SP (CPU32RM 6.4.2).
fn frame_address(machine: &Machine) -> u32 {
    let mut bytes = [0; 4];
    machine
        .read(Address(machine.cpu.registers.ssp + 8), &mut bytes)
        .unwrap();
    u32::from_be_bytes(bytes)
}

/// [`ADDQ_1_INDIRECT_A0`] traced (T1), stopping on faults, with SSP
/// where nothing maps.
fn traced_addq() -> Machine {
    let mut machine = machine(&[ADDQ_1_INDIRECT_A0, NOP]);
    machine.cpu.set_stop_on_fault(true);
    machine.cpu.registers.set_sr(status::S | status::T1);
    machine.cpu.registers.set_address(0, COUNTER);
    machine.cpu.registers.ssp = NOWHERE;
    machine
}

#[test]
fn a_fault_stacking_a_trace_keeps_the_traced_instruction() {
    let mut machine = traced_addq();
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cycle, CycleKind::Write);
    assert_eq!(fault.address, Address(NOWHERE - 2));
    assert_eq!(fault.registers.pc, CODE + 2, "the ADDQ kept");
    assert_eq!(fault.registers.sr(), status::S | status::T1);
    assert_eq!(machine.cpu.registers, fault.registers);
    assert_eq!(word_at(&machine, COUNTER), 1);
    assert_eq!(machine.cpu.instructions(), 1);
    // SSP fixed, the run goes on: the trace taken (CPU32RM 6.2.10),
    // the ADDQ not run again
    machine.cpu.registers.ssp = STACK;
    assert_eq!(boundary_only(&mut machine), End::Instructions);
    assert_eq!(word_at(&machine, COUNTER), 1, "incremented once");
    assert_eq!(machine.cpu.registers.pc, HANDLER);
    assert_eq!(machine.cpu.registers.ssp, STACK - 12);
    assert_eq!(
        frame(&machine),
        (
            status::S | status::T1,
            CODE + 2,
            format::word(format::SIX_WORD, Vector::TRACE)
        )
    );
    assert_eq!(frame_address(&machine), CODE);
    assert_eq!(machine.cpu.instructions(), 1);
}

#[test]
fn a_fault_stacking_a_trace_after_a_trap_keeps_the_trap() {
    let mut machine = machine(&[TRAP_0]);
    machine.cpu.set_stop_on_fault(true);
    machine.cpu.registers.set_sr(status::S | status::T1);
    let trap = handle(&mut machine, Vector::trap(0), 0x4200);
    // room for the trap's four words, not the trace's six
    machine.cpu.registers.ssp = 8;
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.registers.pc, trap, "the trap kept");
    assert_eq!(fault.registers.ssp, 0);
    machine.cpu.registers.ssp = STACK;
    boundary_only(&mut machine);
    assert_eq!(machine.cpu.registers.pc, HANDLER);
    assert_eq!(frame(&machine).1, trap);
    assert_eq!(frame_address(&machine), CODE);
}

/// RTE (CPU32RM 4, RTE).
const RTE: u16 = 0x4e73;

#[test]
fn a_fault_stacking_rte_s_requeued_trace_keeps_the_rte() {
    let mut machine = machine(&[RTE, NOP]);
    machine.cpu.set_stop_on_fault(true);
    // a released word write to rerun, a trace pending in TR (CPU32RM
    // 6.3.1.1, Figure 6-6)
    let frame_at = STACK - 24;
    let words = [
        status::S,
        0,
        (CODE + 2) as u16,
        format::word(format::BUS_ERROR, Vector::BUS_ERROR),
        0,
        COUNTER as u16,
        0,
        0x1234,
        0,
        CODE as u16,
        u16::from(MICROCODE_REVISION) << 8,
        ssw::TR | ssw::RR | ssw::SIZ_WORD << ssw::SIZ_SHIFT | 5,
    ];
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_be_bytes()).collect();
    machine.write(Address(frame_at), &bytes).unwrap();
    machine.cpu.registers.ssp = frame_at;
    // the trace's frame goes where RTE's was, read-only
    let page = Address(STACK - PAGE_BYTES);
    machine
        .protect(page, PAGE_BYTES, Permissions::READ)
        .unwrap();
    let End::Fault(fault) = one(&mut machine) else {
        panic!("no fault");
    };
    assert_eq!(fault.cycle, CycleKind::Write);
    assert_eq!(fault.registers.pc, CODE + 2, "the RTE kept");
    assert_eq!(fault.registers.ssp, STACK);
    assert_eq!(word_at(&machine, COUNTER), 0x1234, "the write rerun");
    assert_eq!(machine.cpu.trace_pending(), Some(Address(CODE)));
    // the run goes on: the trace taken, the write not rerun again
    machine.write(Address(COUNTER), &[0, 0]).unwrap();
    machine.protect(page, PAGE_BYTES, Permissions::ALL).unwrap();
    boundary_only(&mut machine);
    assert_eq!(word_at(&machine, COUNTER), 0);
    assert_eq!(machine.cpu.registers.pc, HANDLER);
    assert_eq!(machine.cpu.registers.ssp, STACK - 12);
    assert_eq!(frame(&machine).1, CODE + 2);
    assert_eq!(frame_address(&machine), CODE);
}

/// An exception a fault stops the stacking of, and the runs around it.
struct Stacking {
    /// The exception.
    name: &'static str,
    /// The code.
    code: &'static [u16],
    /// SR as it runs.
    sr: u16,
    /// Whether a breakpoint is pending.
    breakpoint: bool,
    /// Whether an interrupt is requested.
    interrupt: bool,
    /// The run whose stacking faults.
    faults: Limits,
    /// The run that takes it after.
    resumes: Limits,
}

#[test]
fn a_fault_stopped_exception_is_charged_once_when_taken() {
    let instructions = |count| Limits {
        instructions: Some(count),
        ..Limits::default()
    };
    let steps = Limits {
        steps: Some(0),
        ..Limits::default()
    };
    let cases = [
        Stacking {
            name: "trace",
            code: &[ADDQ_1_INDIRECT_A0, NOP],
            sr: status::S | status::T1,
            breakpoint: false,
            interrupt: false,
            faults: instructions(1),
            resumes: instructions(0),
        },
        Stacking {
            name: "breakpoint",
            code: &[NOP],
            sr: status::S,
            breakpoint: true,
            interrupt: false,
            faults: instructions(0),
            resumes: instructions(0),
        },
        Stacking {
            name: "interrupt",
            code: &[NOP],
            sr: status::S,
            breakpoint: false,
            interrupt: true,
            faults: steps,
            resumes: steps,
        },
        Stacking {
            name: "trap",
            code: &[TRAP_0],
            sr: status::S,
            breakpoint: false,
            interrupt: false,
            faults: instructions(1),
            resumes: instructions(1),
        },
    ];
    for case in cases {
        let prepared = || {
            let mut machine = machine(case.code);
            machine.cpu.set_stop_on_fault(true);
            machine.cpu.registers.set_sr(case.sr);
            machine.cpu.registers.set_address(0, COUNTER);
            machine.cpu.set_breakpoint_pending(case.breakpoint);
            if case.interrupt {
                machine.request_interrupt(IrqPin::new(3).unwrap(), None);
            }
            machine
        };
        let name = case.name;
        let unfaulted = prepared().run_limited(case.faults).cycles;
        let mut machine = prepared();
        machine.cpu.registers.ssp = NOWHERE;
        let run = machine.run_limited(case.faults);
        assert!(matches!(run.end, End::Fault(_)), "{name}");
        machine.cpu.registers.ssp = STACK;
        let resumed = machine.run_limited(case.resumes);
        assert_eq!(machine.cpu.registers.pc, HANDLER, "{name}");
        assert_eq!(run.cycles + resumed.cycles, unfaulted, "{name}");
    }
}

#[test]
fn a_pending_trace_waits_out_background_mode_and_is_a_step() {
    let mut machine = traced_addq();
    machine.cpu.set_bdm_enabled(true);
    one(&mut machine);
    machine.cpu.registers.ssp = STACK;
    assert!(machine.cpu.breakpoint(), "halted, as BKPT halts it");
    assert_eq!(
        boundary_only(&mut machine),
        End::Stop(Stop::Background(Entry::Breakpoint))
    );
    assert_eq!(machine.cpu.trace_pending(), Some(Address(CODE)));
    // a step as a debugger takes one (CPU32RM 7.2.8.12): the trace up
    // to its handler, its first instruction not run
    machine.cpu.resume();
    let run = machine.run_limited(Limits {
        steps: Some(1),
        ..Limits::default()
    });
    assert_eq!((run.end, run.steps), (End::Steps, 1));
    assert_eq!(machine.cpu.registers.pc, HANDLER);
    assert_eq!(frame_address(&machine), CODE);
    assert_eq!(machine.cpu.trace_pending(), None);
}

#[test]
fn a_trace_left_pending_by_the_host_is_taken_or_withdrawn() {
    let mut taken = machine(&[MOVEQ_1_D1, NOP]);
    one(&mut taken);
    taken.cpu.set_trace_pending(Some(Address(CODE)));
    boundary_only(&mut taken);
    assert_eq!(taken.cpu.trace_pending(), None);
    assert_eq!(taken.cpu.registers.pc, HANDLER);
    assert_eq!(frame(&taken).1, CODE + 2);
    assert_eq!(frame_address(&taken), CODE);
    let mut withdrawn = machine(&[MOVEQ_1_D1, NOP]);
    one(&mut withdrawn);
    withdrawn.cpu.set_trace_pending(Some(Address(CODE)));
    withdrawn.cpu.set_trace_pending(None);
    boundary_only(&mut withdrawn);
    assert_eq!(withdrawn.cpu.registers.pc, CODE + 2, "none taken");
}

#[test]
fn reset_drops_a_pending_trace() {
    let mut machine = traced_addq();
    one(&mut machine);
    assert!(machine.cpu.trace_pending().is_some());
    machine
        .write(
            Address(0),
            &[STACK.to_be_bytes(), CODE.to_be_bytes()].concat(),
        )
        .unwrap();
    machine.reset().unwrap();
    assert_eq!(machine.cpu.trace_pending(), None);
}

#[test]
fn a_pending_trace_is_taken_before_a_pending_breakpoint() {
    let mut machine = traced_addq();
    one(&mut machine);
    machine.cpu.set_breakpoint_pending(true);
    machine.cpu.registers.ssp = STACK;
    boundary_only(&mut machine);
    // Table 6-2's order: the breakpoint's frame on the trace's
    assert_eq!(machine.cpu.registers.ssp, STACK - 24);
    let (_, pc, format_vector) = frame(&machine);
    assert_eq!(pc, HANDLER);
    assert_eq!(
        format_vector,
        format::word(format::SIX_WORD, Vector::HARDWARE_BREAKPOINT)
    );
    assert!(!machine.cpu.breakpoint_pending());
}

#[test]
fn a_fault_stacking_an_interrupt_is_not_a_step() {
    let mut machine = machine(&[MOVEQ_1_D1, NOP]);
    machine.cpu.set_stop_on_fault(true);
    let level_3 = handle(
        &mut machine,
        Vector::autovector(InterruptLevel::new(3).unwrap()),
        0x4200,
    );
    machine.cpu.registers.ssp = NOWHERE;
    machine.request_interrupt(IrqPin::new(3).unwrap(), None);
    let run = machine.run_limited(Limits {
        steps: Some(1),
        ..Limits::default()
    });
    assert!(matches!(run.end, End::Fault(_)));
    assert_eq!(run.steps, 0, "the interrupt not taken");
    // SSP fixed: taken on resume, and counted then
    machine.cpu.registers.ssp = STACK;
    let run = machine.run_limited(Limits {
        steps: Some(0),
        ..Limits::default()
    });
    assert_eq!((run.end, run.steps), (End::Steps, 1));
    assert_eq!(machine.cpu.registers.pc, level_3);
}
