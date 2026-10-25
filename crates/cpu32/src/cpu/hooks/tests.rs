use std::cell::RefCell;
use std::ops::ControlFlow;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

use super::ExceptionEntry;
use super::Invalid;
use super::InvalidAccess;
use super::MemoryAccess;
use crate::address::Address;
use crate::cpu::Cpu;
use crate::cpu::Stop;
use crate::cycles::Cycles;
use crate::exception::CycleKind;
use crate::exception::Vector;
use crate::function_code::FunctionCode;
use crate::instructions::tests::CODE;
use crate::instructions::tests::NoBus;
use crate::instructions::tests::NoInterrupts;
use crate::instructions::tests::machine;
use crate::instructions::tests::poke_long;
use crate::instructions::tests::step;
use crate::memory::PAGE_BYTES;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::size::Size;

/// NOP.
const NOP: u16 = 0x4e71;
/// TRAP #0.
const TRAP: u16 = 0x4e40;
/// RTE.
const RTE: u16 = 0x4e73;
/// MOVE.L D0, (A0).
const MOVE_D0_TO_INDIRECT: u16 = 0x2080;
/// MOVE.L (A0), D1.
const MOVE_INDIRECT_TO_D1: u16 = 0x2210;
/// The supervisor stack's top.
const STACK: u32 = 0x4000;
/// Where every vector's handler is.
const HANDLER: u32 = 0x2000;
/// A data page apart from the code's.
const DATA: u32 = 0x5000;
/// An address nothing answers.
const NOWHERE: u32 = 0x0010_0000;

/// A machine running `code` at [`CODE`], its stack at [`STACK`], every
/// vector to [`HANDLER`], which runs `handler`.
fn prepared(code: &[u16], handler: &[u16]) -> Cpu {
    let mut cpu = machine(code);
    cpu.registers.ssp = STACK;
    for vector in 0..=u8::MAX {
        poke_long(&mut cpu, Vector(vector).offset(), HANDLER);
    }
    for (index, pair) in handler.chunks(2).enumerate() {
        let low = pair.get(1).copied().unwrap_or(NOP);
        poke_long(
            &mut cpu,
            HANDLER + 4 * index as u32,
            u32::from(pair[0]) << 16 | u32::from(low),
        );
    }
    cpu
}

/// Every address.
fn everywhere() -> RangeInclusive<Address> {
    Address(0)..=Address(u32::MAX)
}

/// A log the hooks append to.
fn log<T>() -> Rc<RefCell<Vec<T>>> {
    Rc::new(RefCell::new(Vec::new()))
}

#[test]
fn a_code_hook_sees_registers_and_the_stack_and_stops_before() {
    let mut cpu = prepared(&[NOP, NOP], &[]);
    poke_long(&mut cpu, STACK, 0xdead_beef);
    let seen = log();
    let into = Rc::clone(&seen);
    let id = cpu.add_code_hook(Address(CODE + 2)..=Address(CODE + 2), move |instruction| {
        into.borrow_mut().push((
            instruction.pc,
            instruction.stack_top,
            instruction.registers.ssp,
        ));
        ControlFlow::Break(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(id));
    // Not executed.
    assert_eq!(cpu.registers.pc, CODE + 2);
    assert_eq!(cpu.instructions(), 1);
    // The next run executes it without the hook.
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(cpu.registers.pc, CODE + 4);
    assert_eq!(
        *seen.borrow(),
        [(Address(CODE + 2), Some(0xdead_beef), STACK)]
    );
}

#[test]
fn a_block_hook_sees_each_block_start() {
    // Two NOPs, a TRAP into a handler that returns, a NOP.
    let mut cpu = prepared(&[NOP, NOP, TRAP, NOP], &[NOP, RTE]);
    let starts = log();
    let into = Rc::clone(&starts);
    cpu.add_block_hook(everywhere(), move |instruction| {
        into.borrow_mut().push(instruction.pc.0);
        ControlFlow::Continue(())
    });
    step(&mut cpu, 6);
    assert_eq!(*starts.borrow(), [CODE, HANDLER, CODE + 6]);
}

#[test]
fn a_block_hook_stop_resumes_into_the_code_hooks() {
    let mut cpu = prepared(&[NOP], &[]);
    let code = log();
    let into = Rc::clone(&code);
    let block = cpu.add_block_hook(everywhere(), |_| ControlFlow::Break(()));
    cpu.add_code_hook(everywhere(), move |instruction| {
        into.borrow_mut().push(instruction.pc.0);
        ControlFlow::Continue(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(block));
    assert!(code.borrow().is_empty());
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(*code.borrow(), [CODE]);
}

/// A machine of NOPs with a block hook everywhere that logs each
/// block's start, and a code hook on `CODE + 2` that stops the run.
fn stopping_at_the_second() -> (Cpu, Rc<RefCell<Vec<u32>>>) {
    let mut cpu = prepared(&[NOP, NOP, NOP, NOP], &[]);
    let starts = log();
    let into = Rc::clone(&starts);
    cpu.add_block_hook(everywhere(), move |instruction| {
        into.borrow_mut().push(instruction.pc.0);
        ControlFlow::Continue(())
    });
    cpu.add_code_hook(Address(CODE + 2)..=Address(CODE + 2), |_| {
        ControlFlow::Break(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert!(matches!(step(&mut cpu, 1).stop, Stop::Hook(_)));
    (cpu, starts)
}

#[test]
fn a_host_jump_after_a_stop_starts_a_block() {
    let (mut cpu, starts) = stopping_at_the_second();
    cpu.registers.pc = CODE + 4;
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(*starts.borrow(), [CODE, CODE + 4]);
}

#[test]
fn a_resume_without_a_jump_starts_no_block() {
    let (mut cpu, starts) = stopping_at_the_second();
    cpu.registers.pc = CODE + 2;
    assert_eq!(step(&mut cpu, 2).stop, Stop::Budget);
    assert_eq!(cpu.registers.pc, CODE + 6);
    assert_eq!(*starts.borrow(), [CODE]);
}

#[test]
fn a_host_jump_back_to_a_stopped_hook_runs_it_again() {
    let (mut cpu, starts) = stopping_at_the_second();
    cpu.registers.pc = CODE;
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert!(matches!(step(&mut cpu, 1).stop, Stop::Hook(_)));
    assert_eq!(cpu.registers.pc, CODE + 2);
    assert_eq!(*starts.borrow(), [CODE, CODE]);
}

#[test]
fn a_restore_starts_a_block() {
    let (mut cpu, starts) = stopping_at_the_second();
    let stopped = cpu.save();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    let running = cpu.save();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    cpu.restore(&running).unwrap();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(*starts.borrow(), [CODE, CODE + 4]);
    // A snapshot of a stop keeps its skip: the hooks that ran there
    // stay run.
    cpu.restore(&stopped).unwrap();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(cpu.registers.pc, CODE + 4);
    assert_eq!(*starts.borrow(), [CODE, CODE + 4]);
}

#[test]
fn a_reset_starts_a_block() {
    let (mut cpu, starts) = stopping_at_the_second();
    poke_long(&mut cpu, Vector::RESET_STACK_POINTER.offset(), STACK);
    poke_long(&mut cpu, Vector::RESET_PROGRAM_COUNTER.offset(), CODE + 2);
    cpu.reset(&mut NoBus, Cycles::ZERO).unwrap();
    // The stop's skip is gone: the code hook stops again.
    assert!(matches!(step(&mut cpu, 1).stop, Stop::Hook(_)));
    assert_eq!(*starts.borrow(), [CODE, CODE + 2]);
}

#[test]
fn memory_hooks_see_each_word_cycle_and_stop_after_the_instruction() {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT, MOVE_INDIRECT_TO_D1, NOP], &[]);
    cpu.registers.set_address(0, DATA);
    cpu.registers.data[0] = 0x1234_5678;
    let writes = log();
    let into = Rc::clone(&writes);
    let write = cpu.add_write_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut().push((access.address.0, access.value));
        ControlFlow::Break(())
    });
    let reads = log();
    let into = Rc::clone(&reads);
    cpu.add_read_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut().push((access.address.0, access.value));
        ControlFlow::Continue(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(write));
    assert_eq!(cpu.registers.pc, CODE + 2);
    assert_eq!(*writes.borrow(), [(DATA, 0x1234), (DATA + 2, 0x5678)]);
    step(&mut cpu, 1);
    // Fetches are not reads.
    assert_eq!(*reads.borrow(), [(DATA, 0x1234), (DATA + 2, 0x5678)]);
    assert_eq!(cpu.registers.data[1], 0x1234_5678);
}

#[test]
fn memory_hooks_slow_only_their_pages() {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT], &[]);
    cpu.registers.set_address(0, DATA);
    let calls = Rc::new(RefCell::new(0));
    let into = Rc::clone(&calls);
    let page = DATA + PAGE_BYTES;
    let id = cpu.add_write_hook(Address(page)..=Address(page + 1), move |_| {
        *into.borrow_mut() += 1;
        ControlFlow::Continue(())
    });
    assert_eq!(cpu.memory.watched(Address(DATA)), 0);
    assert_ne!(cpu.memory.watched(Address(page)), 0);
    // The hooked page leaves the fast path; the others stay on it.
    let word = |cpu: &Cpu, at| cpu.memory.read(Space::Supervisor, Address(at), Size::Word);
    assert!(word(&cpu, page).is_none());
    assert!(word(&cpu, DATA).is_some());
    assert!(
        cpu.memory
            .read_backed(Space::Supervisor, Address(page), Size::Word)
            .is_some()
    );
    step(&mut cpu, 1);
    assert_eq!(*calls.borrow(), 0);
    assert!(cpu.remove_hook(id));
    assert!(!cpu.remove_hook(id));
    // No hooks left: no cycle looks one up.
    assert!(!cpu.memory.is_special());
}

#[test]
fn an_invalid_access_hook_sees_a_bus_error_then_the_exception_is_taken() {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT], &[]);
    cpu.registers.set_address(0, NOWHERE);
    cpu.registers.data[0] = 0xabcd_0123;
    let seen = log();
    let into = Rc::clone(&seen);
    let id = cpu.add_invalid_hook(everywhere(), move |access: &InvalidAccess| {
        into.borrow_mut().push(*access);
        ControlFlow::Break(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(id));
    assert_eq!(cpu.registers.pc, HANDLER);
    assert_eq!(
        *seen.borrow(),
        [InvalidAccess {
            address: Address(NOWHERE),
            size: Size::Word,
            function_code: FunctionCode::SUPERVISOR_DATA,
            cycle: CycleKind::Write,
            value: Some(0xabcd),
            reason: Invalid::BusError,
            pc: Address(CODE),
            stacking: false,
        }]
    );
}

#[test]
fn an_exception_hook_sees_the_entry_and_stops_before_the_handler() {
    let mut cpu = prepared(&[TRAP], &[NOP]);
    let seen = log();
    let into = Rc::clone(&seen);
    let id = cpu.add_exception_hook(move |entry: &ExceptionEntry| {
        into.borrow_mut().push(*entry);
        ControlFlow::Break(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(id));
    assert_eq!(cpu.registers.pc, HANDLER);
    assert_eq!(
        *seen.borrow(),
        [ExceptionEntry {
            vector: Vector::TRAP,
            pc: Address(CODE + 2),
            handler: Address(HANDLER),
            stack: STACK - 8,
            level: None,
        }]
    );
}

#[test]
fn a_write_to_a_read_only_page_is_a_bus_error() {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT], &[]);
    cpu.registers.set_address(0, DATA);
    cpu.registers.data[0] = 0xffff_ffff;
    cpu.memory
        .protect(
            Space::Supervisor,
            Address(DATA),
            PAGE_BYTES,
            Permissions::READ,
        )
        .unwrap();
    let reasons = log();
    let into = Rc::clone(&reasons);
    cpu.add_invalid_hook(everywhere(), move |access: &InvalidAccess| {
        into.borrow_mut().push(access.reason);
        ControlFlow::Continue(())
    });
    step(&mut cpu, 1);
    assert_eq!(cpu.registers.pc, HANDLER);
    assert_eq!(*reasons.borrow(), [Invalid::Protected]);
    assert_eq!(
        cpu.memory
            .peek(Space::Supervisor, Address(DATA), Size::Long),
        Some(0)
    );
    // User space keeps its own permissions.
    assert_eq!(
        cpu.memory.permissions(Space::User, Address(DATA)),
        Permissions::ALL
    );
}

#[test]
fn a_fetch_from_a_page_without_execute_is_a_bus_error() {
    let mut cpu = prepared(&[NOP], &[]);
    let page = CODE / PAGE_BYTES * PAGE_BYTES;
    cpu.memory
        .protect(
            Space::Supervisor,
            Address(page),
            PAGE_BYTES,
            Permissions::READ.union(Permissions::WRITE),
        )
        .unwrap();
    let cycles = log();
    let into = Rc::clone(&cycles);
    cpu.add_invalid_hook(everywhere(), move |access: &InvalidAccess| {
        into.borrow_mut().push((access.cycle, access.reason));
        ControlFlow::Continue(())
    });
    step(&mut cpu, 1);
    assert_eq!(*cycles.borrow(), [(CycleKind::Fetch, Invalid::Protected)]);
    assert_eq!(cpu.registers.pc, HANDLER);
}

/// Runs of NOPs from [`CODE`], `instructions` of them less a few: the
/// least wall-clock time of several tries, against a loaded host.
fn time_nops(cpu: &mut Cpu, instructions: usize) -> Duration {
    /// Cycles in each run: the cost of fewer than 1000 NOPs.
    const BUDGET: Cycles = Cycles(1500);
    /// Tries.
    const TRIES: usize = 20;
    (0..TRIES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..instructions / 1000 {
                cpu.registers.pc = CODE;
                cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, BUDGET);
            }
            start.elapsed()
        })
        .min()
        .expect("tries")
}

/// Not a check, a measurement ([PLAN.md, Hooks]): a NOP stream with no
/// hooks, with a write hook on a page it never touches, and with a
/// code hook on every instruction.  `cargo test --release -p cpu32
/// hook_cost -- --ignored --nocapture`.
///
/// [PLAN.md, Hooks]: https://github.com/charlieh0tel/mc683xx/blob/main/PLAN.md#hooks
#[test]
#[ignore = "a measurement"]
fn hook_cost() {
    const INSTRUCTIONS: usize = 1_000_000;
    let code = [NOP; 1000];
    let mut cpu = prepared(&code, &[]);
    let none = time_nops(&mut cpu, INSTRUCTIONS);
    let id = cpu.add_write_hook(Address(DATA)..=Address(DATA), |_| ControlFlow::Continue(()));
    let elsewhere = time_nops(&mut cpu, INSTRUCTIONS);
    cpu.remove_hook(id);
    cpu.add_code_hook(everywhere(), |_| ControlFlow::Continue(()));
    let every = time_nops(&mut cpu, INSTRUCTIONS);
    let per = |duration: Duration| duration.as_nanos() as f64 / INSTRUCTIONS as f64;
    println!(
        "ns per NOP: no hooks {:.2}, write hook elsewhere {:.2}, code hook on each {:.2}",
        per(none),
        per(elsewhere),
        per(every)
    );
}
