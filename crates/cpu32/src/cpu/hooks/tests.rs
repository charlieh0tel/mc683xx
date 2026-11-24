use std::cell::RefCell;
use std::ops::ControlFlow;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

use crate::BusError;
use crate::bus::Bus;
use crate::bus::BusAccess;
use crate::bus::Read;
use crate::cpu::Cpu;
use crate::cpu::Stop;
use crate::cycles::Cycle;
use crate::cycles::Cycles;
use crate::exception::BusCycleKind;
use crate::exception::Exception;
use crate::exception::Vector;
use crate::hooks::ExceptionEntry;
use crate::hooks::ExceptionReturn;
use crate::hooks::Invalid;
use crate::hooks::InvalidAccess;
use crate::hooks::MemoryAccess;
use crate::hooks::PendingRead;
use crate::hooks::PendingWrite;
use crate::instructions::tests::CODE;
use crate::instructions::tests::NoBus;
use crate::instructions::tests::NoInterrupts;
use crate::instructions::tests::RAM_BYTES;
use crate::instructions::tests::machine;
use crate::instructions::tests::machine_with_clocks;
use crate::instructions::tests::peek_long;
use crate::instructions::tests::poke_long;
use crate::instructions::tests::step;
use crate::memory::Backing;
use crate::memory::Memory;
use crate::memory::PAGE_BYTES;
use crate::memory::Page;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::registers::status;
use mc683xx_target::Address;
use mc683xx_target::FunctionCode;
use mc683xx_target::Size;

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
    cpu.registers.ssp = Address(STACK);
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
    assert_eq!(id.to_string(), "hook 1");
    // Not executed.
    assert_eq!(cpu.registers.pc.0, CODE + 2);
    assert_eq!(cpu.instructions(), 1);
    // The next run executes it without the hook.
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(cpu.registers.pc.0, CODE + 4);
    assert_eq!(
        *seen.borrow(),
        [(Address(CODE + 2), Some(0xdead_beef), Address(STACK))]
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
fn a_page_without_instruction_hooks_stays_quiet_until_one_is_added() {
    // A TRAP into a handler that returns, twice, the handler's page
    // alone with a block hook, over its NOP and RTE.
    let mut cpu = prepared(&[NOP, TRAP, NOP, TRAP], &[NOP, RTE]);
    let starts = log();
    let into = Rc::clone(&starts);
    cpu.add_block_hook(
        Address(HANDLER)..=Address(HANDLER + 3),
        move |instruction| {
            into.borrow_mut().push(instruction.pc.0);
            ControlFlow::Continue(())
        },
    );
    // NOP, TRAP, the handler's NOP and RTE, back on the quiet page
    step(&mut cpu, 4);
    assert_eq!(cpu.registers.pc.0, CODE + 4);
    let code = log();
    let into = Rc::clone(&code);
    cpu.add_code_hook(Address(CODE + 4)..=Address(CODE + 4), move |instruction| {
        into.borrow_mut().push(instruction.pc.0);
        ControlFlow::Continue(())
    });
    step(&mut cpu, 4);
    // The return's block start, on the code's page, was forgotten there.
    assert_eq!(*starts.borrow(), [HANDLER, HANDLER]);
    assert_eq!(*code.borrow(), [CODE + 4]);
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
    cpu.registers.pc = Address(CODE + 4);
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(*starts.borrow(), [CODE, CODE + 4]);
}

#[test]
fn a_resume_without_a_jump_starts_no_block() {
    let (mut cpu, starts) = stopping_at_the_second();
    cpu.registers.pc = Address(CODE + 2);
    assert_eq!(step(&mut cpu, 2).stop, Stop::Budget);
    assert_eq!(cpu.registers.pc.0, CODE + 6);
    assert_eq!(*starts.borrow(), [CODE]);
}

#[test]
fn a_host_jump_back_to_a_stopped_hook_runs_it_again() {
    let (mut cpu, starts) = stopping_at_the_second();
    cpu.registers.pc = Address(CODE);
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert!(matches!(step(&mut cpu, 1).stop, Stop::Hook(_)));
    assert_eq!(cpu.registers.pc.0, CODE + 2);
    assert_eq!(*starts.borrow(), [CODE, CODE]);
}

#[test]
fn a_restore_starts_a_block() {
    let (mut cpu, starts) = stopping_at_the_second();
    let stopped = cpu.save().unwrap();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    let running = cpu.save().unwrap();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    cpu.restore(&running).unwrap();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(*starts.borrow(), [CODE, CODE + 4]);
    // A snapshot of a stop keeps its skip: the hooks that ran there
    // stay run.
    cpu.restore(&stopped).unwrap();
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(cpu.registers.pc.0, CODE + 4);
    assert_eq!(*starts.borrow(), [CODE, CODE + 4]);
}

#[test]
fn a_reset_starts_a_block() {
    let (mut cpu, starts) = stopping_at_the_second();
    poke_long(&mut cpu, Vector::RESET_STACK_POINTER.offset(), STACK);
    poke_long(&mut cpu, Vector::RESET_PROGRAM_COUNTER.offset(), CODE + 2);
    cpu.reset(&mut NoBus, Cycle::ZERO);
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
    assert_eq!(cpu.registers.pc.0, CODE + 2);
    assert_eq!(*writes.borrow(), [(DATA, 0x1234), (DATA + 2, 0x5678)]);
    step(&mut cpu, 1);
    // Fetches are not reads.
    assert_eq!(*reads.borrow(), [(DATA, 0x1234), (DATA + 2, 0x5678)]);
    assert_eq!(cpu.registers.data[1], 0x1234_5678);
}

/// The pre-read hook's `(address, size, copy)`s.
type PreReads = Vec<(Address, Size, Vec<u8>)>;

/// The pre-read calls MOVE.L (A0),D1 makes with A0 at `at`, and D1
/// after it, a hook covering `covered` setting the copy's last byte to
/// 0x99, on `bus`, the long seen once if `long_bus`; with the read hooks'
/// `(size, value)`s.
fn pre_read_long(
    at: u32,
    covered: RangeInclusive<Address>,
    bus: &mut impl Bus,
    long_bus: bool,
) -> (PreReads, u32, Vec<(Size, u32)>) {
    let mut cpu = prepared(&[MOVE_INDIRECT_TO_D1, NOP], &[]);
    cpu.set_long_bus(long_bus);
    cpu.registers.set_address(0, at);
    let pending = log();
    let into = Rc::clone(&pending);
    cpu.add_pre_read_hook(covered, move |read: &PendingRead, _, bytes: &mut [u8]| {
        into.borrow_mut()
            .push((read.address, read.size, bytes.to_vec()));
        bytes[bytes.len() - 1] = 0x99;
        ControlFlow::Continue(())
    });
    let reads = log();
    let into = Rc::clone(&reads);
    cpu.add_read_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut().push((access.size, access.value));
        ControlFlow::Continue(())
    });
    cpu.run_for(bus, &mut NoInterrupts, Cycle::ZERO, Cycles::from_clocks(1));
    (pending.take(), cpu.registers.data[1], reads.take())
}

#[test]
fn a_pre_read_hook_answers_a_long_once_from_its_copy_before_the_read_hooks() {
    let mut cpu = prepared(&[MOVE_INDIRECT_TO_D1, NOP], &[]);
    cpu.registers.set_address(0, DATA);
    poke_long(&mut cpu, DATA, 0x1111_2222);
    let pending = log();
    let into = Rc::clone(&pending);
    // the low word's second byte alone: one call for the whole long
    let id = cpu.add_pre_read_hook(
        Address(DATA + 3)..=Address(DATA + 3),
        move |read: &PendingRead, _, bytes: &mut [u8]| {
            into.borrow_mut()
                .push((read.address.0, read.size, bytes.to_vec()));
            bytes[3] = 0x99;
            ControlFlow::Break(())
        },
    );
    let reads = log();
    let into = Rc::clone(&reads);
    cpu.add_read_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut().push(access.value);
        ControlFlow::Continue(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(id));
    assert_eq!(cpu.registers.pc.0, CODE + 2, "the instruction completed");
    assert_eq!(cpu.registers.data[1], 0x1111_2299);
    assert_eq!(
        *pending.borrow(),
        [(DATA, Size::Long, vec![0x11, 0x11, 0x22, 0x22])]
    );
    assert_eq!(*reads.borrow(), [0x1111, 0x2299]);
    // the copy answered the read; memory is as it was
    assert_eq!(
        cpu.memory
            .peek(Space::Supervisor, Address(DATA), Size::Long),
        Some(0x1111_2222)
    );
}

#[test]
fn a_pre_read_hook_reads_other_memory_through_its_view() {
    let mut cpu = prepared(&[MOVE_INDIRECT_TO_D1, NOP], &[]);
    cpu.registers.set_address(0, DATA);
    poke_long(&mut cpu, DATA + 0x10, 0x5555_6666);
    cpu.add_pre_read_hook(
        Address(DATA)..=Address(DATA),
        |read: &PendingRead, memory: &Memory, bytes: &mut [u8]| {
            let image = memory.peek(
                Space::of(read.function_code),
                Address(DATA + 0x10),
                Size::Long,
            );
            bytes.copy_from_slice(&image.unwrap().to_be_bytes());
            ControlFlow::Continue(())
        },
    );
    step(&mut cpu, 1);
    assert_eq!(cpu.registers.data[1], 0x5555_6666);
}

#[test]
fn a_pre_read_hook_answers_a_long_bus_s_read_hooks_once() {
    let (pending, d1, reads) = pre_read_long(DATA, everywhere(), &mut NoBus, true);
    assert_eq!(pending, [(Address(DATA), Size::Long, vec![0; 4])]);
    assert_eq!(d1, 0x99);
    assert_eq!(reads, [(Size::Long, 0x99)]);
}

#[test]
fn a_long_across_two_pages_of_memory_is_one_pre_read() {
    let at = DATA + PAGE_BYTES - 2;
    let next = Address(DATA + PAGE_BYTES);
    let (pending, d1, reads) = pre_read_long(at, next..=next, &mut NoBus, false);
    assert_eq!(pending, [(Address(at), Size::Long, vec![0; 4])]);
    assert_eq!(d1, 0x99);
    assert_eq!(reads, [(Size::Word, 0), (Size::Word, 0x99)]);
}

/// A bus that answers every read with [`ANSWER`].
#[derive(Debug)]
struct Answering;

/// What [`Answering`] reads.
const ANSWER: u32 = 0xabcd;

impl Bus for Answering {
    fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
        Ok(Read {
            value: ANSWER,
            stall: Cycles::ZERO,
        })
    }

    fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
        Err(BusError)
    }
}

#[test]
fn a_long_into_the_bus_is_no_pre_read() {
    let at = RAM_BYTES - 2;
    let (pending, d1, reads) = pre_read_long(at, everywhere(), &mut Answering, false);
    assert!(pending.is_empty());
    assert_eq!(d1, ANSWER);
    assert_eq!(reads, [(Size::Word, 0), (Size::Word, ANSWER)]);
}

#[test]
fn a_pre_read_hook_sees_no_fetch_write_or_misaligned_read() {
    // MOVE.L D0,(A0); MOVE.W (A1),D1 with A1 odd: an address error,
    // whose vector, at 12, is a read the hook sees
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT, 0x3211, NOP], &[]);
    cpu.registers.set_address(0, DATA);
    cpu.registers.set_address(1, DATA + 1);
    let calls = Rc::new(RefCell::new(0));
    let into = Rc::clone(&calls);
    cpu.add_pre_read_hook(Address(0)..=Address(DATA + 1), move |_, _, _| {
        *into.borrow_mut() += 1;
        ControlFlow::Continue(())
    });
    step(&mut cpu, 2);
    assert_eq!(
        cpu.exception_taken().map(Exception::vector),
        Some(Vector::ADDRESS_ERROR)
    );
    assert_eq!(*calls.borrow(), 1, "the vector's read alone");
}

#[test]
fn a_pre_read_hook_skips_a_read_its_page_s_permissions_refuse() {
    // a long whose second word's page refuses reads: a fault, no call
    let mut cpu = prepared(&[MOVE_INDIRECT_TO_D1, NOP], &[]);
    cpu.registers.set_address(0, DATA + PAGE_BYTES - 2);
    cpu.memory
        .protect(
            Space::Supervisor,
            Address(DATA + PAGE_BYTES),
            PAGE_BYTES,
            Permissions::WRITE,
        )
        .unwrap();
    let calls = Rc::new(RefCell::new(0));
    let into = Rc::clone(&calls);
    cpu.add_pre_read_hook(
        Address(DATA)..=Address(DATA + 2 * PAGE_BYTES),
        move |_, _, _| {
            *into.borrow_mut() += 1;
            ControlFlow::Continue(())
        },
    );
    step(&mut cpu, 1);
    assert_eq!(
        cpu.exception_taken().map(Exception::vector),
        Some(Vector::BUS_ERROR)
    );
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn a_pre_read_hook_slows_its_page_s_reads_alone_and_is_removed() {
    let mut cpu = prepared(&[NOP], &[]);
    let page = DATA + PAGE_BYTES;
    let id = cpu.add_pre_read_hook(Address(page)..=Address(page), |_, _, _| {
        ControlFlow::Continue(())
    });
    let word = |cpu: &Cpu, at| cpu.memory.read(Space::Supervisor, Address(at), Size::Word);
    assert!(word(&cpu, page).is_none());
    assert!(word(&cpu, DATA).is_some());
    assert!(
        cpu.memory
            .write(Space::Supervisor, Address(page), Size::Word, 0)
            .is_some()
    );
    assert!(cpu.has_hook(id));
    assert!(cpu.remove_hook(id));
    assert!(!cpu.has_hook(id));
    assert!(!cpu.memory.is_special());
}

/// A write hook's `(address, size, value, previous)`s.
type Writes = Rc<RefCell<Vec<(u32, Size, u32, Option<u32>)>>>;

/// A write hook everywhere on `cpu` that logs each write.
fn log_writes(cpu: &mut Cpu) -> Writes {
    let writes = log();
    let into = Rc::clone(&writes);
    cpu.add_write_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut()
            .push((access.address.0, access.size, access.value, access.previous));
        ControlFlow::Continue(())
    });
    writes
}

/// The long word at `at` in supervisor space, hooked page or not.
fn stored(cpu: &Cpu, at: u32) -> u32 {
    cpu.memory
        .peek(Space::Supervisor, Address(at), Size::Long)
        .expect("RAM")
}

/// A machine running MOVE.L D0,(A0) with A0 at `at`, D0 `0x1234_5678`
/// and `0x1111_2222` in memory there, the long seen once if `long_bus`.
fn writing_long(at: u32, long_bus: bool) -> Cpu {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT, NOP], &[]);
    cpu.set_long_bus(long_bus);
    cpu.registers.set_address(0, at);
    cpu.registers.data[0] = 0x1234_5678;
    if at + 4 <= RAM_BYTES {
        poke_long(&mut cpu, at, 0x1111_2222);
    }
    cpu
}

#[test]
fn a_pre_write_hook_replaces_a_long_once_before_the_write_hooks() {
    let mut cpu = writing_long(DATA, false);
    let pending = log();
    let into = Rc::clone(&pending);
    // the low word's second byte alone: one call for the whole long
    let id = cpu.add_pre_write_hook(
        Address(DATA + 3)..=Address(DATA + 3),
        move |write: &mut PendingWrite, memory: &Memory, bytes: &mut [u8]| {
            let before = memory.peek(Space::Supervisor, write.address, Size::Long);
            into.borrow_mut()
                .push((write.address.0, write.size, bytes.to_vec(), before));
            bytes[3] = 0x99;
            ControlFlow::Break(())
        },
    );
    let writes = log_writes(&mut cpu);
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(id));
    assert_eq!(cpu.registers.pc.0, CODE + 2, "the instruction completed");
    assert_eq!(
        *pending.borrow(),
        [(
            DATA,
            Size::Long,
            vec![0x12, 0x34, 0x56, 0x78],
            Some(0x1111_2222)
        )]
    );
    // the write hooks see what was stored, and what it replaced
    assert_eq!(
        *writes.borrow(),
        [
            (DATA, Size::Word, 0x1234, Some(0x1111)),
            (DATA + 2, Size::Word, 0x5699, Some(0x2222))
        ]
    );
    assert_eq!(stored(&cpu, DATA), 0x1234_5699);
}

#[test]
fn a_pre_write_hook_replaces_a_long_bus_s_write_once() {
    let mut cpu = writing_long(DATA, true);
    cpu.add_pre_write_hook(everywhere(), |_, _, bytes: &mut [u8]| {
        bytes.copy_from_slice(&[0xca, 0xfe, 0xf0, 0x0d]);
        ControlFlow::Continue(())
    });
    let writes = log_writes(&mut cpu);
    step(&mut cpu, 1);
    assert_eq!(
        *writes.borrow(),
        [(DATA, Size::Long, 0xcafe_f00d, Some(0x1111_2222))]
    );
}

#[test]
fn a_dropped_store_writes_nothing_and_takes_its_time() {
    let unhooked = step(&mut writing_long(DATA, false), 1).cycles;
    let mut cpu = writing_long(DATA, false);
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_pre_write_hook(everywhere(), |write: &mut PendingWrite, _, _| {
        write.set_dropped(true);
        ControlFlow::Continue(())
    });
    // a later hook sees the drop, and its bytes go nowhere
    cpu.add_pre_write_hook(everywhere(), move |write: &mut PendingWrite, _, bytes| {
        into.borrow_mut().push(write.dropped());
        bytes.fill(0xff);
        ControlFlow::Continue(())
    });
    let writes = log_writes(&mut cpu);
    let run = step(&mut cpu, 1);
    assert_eq!(*seen.borrow(), [true]);
    assert!(writes.borrow().is_empty());
    assert_eq!(stored(&cpu, DATA), 0x1111_2222);
    assert_eq!(cpu.registers.pc.0, CODE + 2);
    assert_eq!(run.cycles, unhooked);
}

#[test]
fn a_later_pre_write_hook_may_keep_a_dropped_store() {
    let mut cpu = writing_long(DATA, false);
    cpu.add_pre_write_hook(everywhere(), |write: &mut PendingWrite, _, _| {
        write.set_dropped(true);
        ControlFlow::Continue(())
    });
    cpu.add_pre_write_hook(everywhere(), |write: &mut PendingWrite, _, _| {
        write.set_dropped(false);
        ControlFlow::Continue(())
    });
    step(&mut cpu, 1);
    assert_eq!(stored(&cpu, DATA), 0x1234_5678);
}

#[test]
fn a_pre_write_hook_sees_exception_stacking_word_by_word() {
    let mut cpu = prepared(&[TRAP], &[NOP]);
    let seen = log();
    let into = Rc::clone(&seen);
    let frame = Address(STACK - 8)..=Address(STACK - 1);
    cpu.add_pre_write_hook(frame, move |write: &mut PendingWrite, _, bytes| {
        into.borrow_mut()
            .push((write.address.0, write.size, write.stacking));
        // the frame's highest word
        if write.address.0 == STACK - 2 {
            bytes.copy_from_slice(&[0xab, 0xcd]);
        }
        ControlFlow::Continue(())
    });
    let writes = log_writes(&mut cpu);
    step(&mut cpu, 1);
    assert_eq!(
        *seen.borrow(),
        [
            (STACK - 2, Size::Word, true),
            (STACK - 4, Size::Word, true),
            (STACK - 6, Size::Word, true),
            (STACK - 8, Size::Word, true)
        ]
    );
    assert_eq!(writes.borrow()[0].2, 0xabcd);
    assert_eq!(stored(&cpu, STACK - 4) & 0xffff, 0xabcd);
}

/// A bus that takes every write.
#[derive(Debug)]
struct Taking;

impl Bus for Taking {
    fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
        Ok(Cycles::ZERO)
    }
}

#[test]
fn a_long_into_the_bus_is_no_pre_write() {
    let mut cpu = writing_long(RAM_BYTES - 2, false);
    let calls = Rc::new(RefCell::new(0));
    let into = Rc::clone(&calls);
    cpu.add_pre_write_hook(everywhere(), move |_, _, _| {
        *into.borrow_mut() += 1;
        ControlFlow::Continue(())
    });
    cpu.run_for(
        &mut Taking,
        &mut NoInterrupts,
        Cycle::ZERO,
        Cycles::from_clocks(1),
    );
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(cpu.registers.pc.0, CODE + 2);
}

#[test]
fn a_pre_write_hook_sees_no_read_or_misaligned_write() {
    // MOVE.L (A0),D1; MOVE.W D0,(A1) with A1 odd: an address error,
    // whose frame's words are writes the hook sees
    let mut cpu = prepared(&[MOVE_INDIRECT_TO_D1, 0x3280 | 1 << 9, NOP], &[]);
    cpu.registers.set_address(0, DATA);
    cpu.registers.set_address(1, DATA + 1);
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_pre_write_hook(everywhere(), move |write: &mut PendingWrite, _, _| {
        into.borrow_mut().push(write.stacking);
        ControlFlow::Continue(())
    });
    step(&mut cpu, 2);
    assert_eq!(
        cpu.exception_taken().map(Exception::vector),
        Some(Vector::ADDRESS_ERROR)
    );
    assert!(!seen.borrow().is_empty());
    assert!(seen.borrow().iter().all(|&stacking| stacking));
}

#[test]
fn a_pre_write_hook_skips_a_write_its_page_s_permissions_refuse() {
    // a long whose second word's page refuses writes: a fault, no call
    let mut cpu = writing_long(DATA + PAGE_BYTES - 2, false);
    cpu.memory
        .protect(
            Space::Supervisor,
            Address(DATA + PAGE_BYTES),
            PAGE_BYTES,
            Permissions::READ,
        )
        .unwrap();
    let calls = Rc::new(RefCell::new(0));
    let into = Rc::clone(&calls);
    cpu.add_pre_write_hook(
        Address(DATA)..=Address(DATA + 2 * PAGE_BYTES),
        move |_, _, _| {
            *into.borrow_mut() += 1;
            ControlFlow::Continue(())
        },
    );
    step(&mut cpu, 1);
    assert_eq!(
        cpu.exception_taken().map(Exception::vector),
        Some(Vector::BUS_ERROR)
    );
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn a_long_across_two_pages_of_memory_is_one_pre_write() {
    let at = DATA + PAGE_BYTES - 2;
    let mut cpu = writing_long(at, false);
    let next = Address(DATA + PAGE_BYTES);
    let calls = log();
    let into = Rc::clone(&calls);
    cpu.add_pre_write_hook(next..=next, move |write: &mut PendingWrite, _, bytes| {
        into.borrow_mut().push((write.address.0, write.size));
        bytes[0] = 0x99;
        ControlFlow::Continue(())
    });
    step(&mut cpu, 1);
    assert_eq!(*calls.borrow(), [(at, Size::Long)]);
    assert_eq!(stored(&cpu, at), 0x9934_5678);
}

#[test]
fn a_pre_write_hook_slows_its_page_s_writes_alone_and_is_removed() {
    let mut cpu = prepared(&[NOP], &[]);
    let page = DATA + PAGE_BYTES;
    let id = cpu.add_pre_write_hook(Address(page)..=Address(page), |_, _, _| {
        ControlFlow::Continue(())
    });
    let write = |cpu: &mut Cpu, at| {
        cpu.memory
            .write(Space::Supervisor, Address(at), Size::Word, 0)
            .is_some()
    };
    assert!(!write(&mut cpu, page));
    assert!(write(&mut cpu, DATA));
    assert!(
        cpu.memory
            .read(Space::Supervisor, Address(page), Size::Word)
            .is_some()
    );
    assert!(cpu.has_hook(id));
    assert!(cpu.remove_hook(id));
    assert!(!cpu.has_hook(id));
    assert!(!cpu.memory.is_special());
}

#[test]
fn a_write_hook_sees_what_each_write_replaced_and_a_read_hook_none() {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT, MOVE_INDIRECT_TO_D1], &[]);
    cpu.registers.set_address(0, DATA);
    cpu.registers.data[0] = 0x1234_5678;
    poke_long(&mut cpu, DATA, 0xdead_beef);
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_write_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut().push((access.value, access.previous));
        ControlFlow::Continue(())
    });
    let into = Rc::clone(&seen);
    cpu.add_read_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut().push((access.value, access.previous));
        ControlFlow::Continue(())
    });
    step(&mut cpu, 2);
    assert_eq!(
        *seen.borrow(),
        [
            (0x1234, Some(0xdead)),
            (0x5678, Some(0xbeef)),
            (0x1234, None),
            (0x5678, None),
        ]
    );
}

/// The write hook's `(value, previous)` pairs for MOVE.L D0,(A0) at
/// [`DATA`], its page mapped to read from bytes of `0xaa` (or from the
/// bus if `read` is false) and to write to bytes of `0xbb`; the long
/// seen once if `long_bus`.
fn previous_through_split_page(read: bool, long_bus: bool) -> Vec<(u32, Option<u32>)> {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT], &[]);
    cpu.set_long_bus(long_bus);
    cpu.registers.set_address(0, DATA);
    cpu.registers.data[0] = 0x1234_5678;
    let mut backing = |byte| {
        Some(Backing {
            region: cpu.memory.add_region(vec![byte; PAGE_BYTES as usize]),
            offset: 0,
        })
    };
    let rom = backing(0xaa);
    let ram = backing(0xbb);
    let page = Page::BUS.reading(rom.filter(|_| read)).writing(ram);
    cpu.memory
        .map(Space::Supervisor, Address(DATA), PAGE_BYTES, page)
        .unwrap();
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_write_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut().push((access.value, access.previous));
        ControlFlow::Continue(())
    });
    step(&mut cpu, 1);
    assert_eq!(
        cpu.memory.region(ram.unwrap().region)[..4],
        [0x12, 0x34, 0x56, 0x78]
    );
    seen.take()
}

#[test]
fn a_write_hook_sees_what_the_write_backing_held_not_the_read_one() {
    assert_eq!(
        previous_through_split_page(true, false),
        [(0x1234, Some(0xbbbb)), (0x5678, Some(0xbbbb))]
    );
    assert_eq!(
        previous_through_split_page(true, true),
        [(0x1234_5678, Some(0xbbbb_bbbb))]
    );
}

#[test]
fn a_write_hook_sees_what_write_only_ram_held() {
    assert_eq!(
        previous_through_split_page(false, false),
        [(0x1234, Some(0xbbbb)), (0x5678, Some(0xbbbb))]
    );
}

/// A bus that takes every write.
#[derive(Debug)]
struct Writable;

impl Bus for Writable {
    fn read(&mut self, _access: BusAccess) -> Result<Read, BusError> {
        Err(BusError)
    }

    fn write(&mut self, _access: BusAccess, _value: u32) -> Result<Cycles, BusError> {
        Ok(Cycles::ZERO)
    }
}

#[test]
fn a_write_hook_sees_no_previous_value_for_a_long_in_cpu_space() {
    // MOVES.L D0,(A0) (CPU32RM 4.4, MOVES), with DFC 7.
    let mut cpu = prepared(&[0x0e90, 0x0800], &[]);
    cpu.set_long_bus(true);
    cpu.registers.dfc = FunctionCode::CpuSpace;
    cpu.registers.set_address(0, DATA);
    poke_long(&mut cpu, DATA, 0xdead_beef);
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_write_hook(everywhere(), move |access: &MemoryAccess| {
        into.borrow_mut()
            .push((access.function_code, access.previous));
        ControlFlow::Continue(())
    });
    cpu.run_for(
        &mut Writable,
        &mut NoInterrupts,
        Cycle::ZERO,
        Cycles::from_clocks(1),
    );
    assert_eq!(*seen.borrow(), [(FunctionCode::CpuSpace, None)]);
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
fn a_long_across_a_page_costs_and_reads_as_one_inside_a_page() {
    let code = [MOVE_D0_TO_INDIRECT, MOVE_INDIRECT_TO_D1];
    let run = |at: u32| {
        let mut cpu = machine_with_clocks(&code, 3);
        cpu.registers.set_address(0, at);
        cpu.registers.data[0] = 0x1234_5678;
        let cycles = [step(&mut cpu, 1).cycles, step(&mut cpu, 1).cycles];
        assert_eq!(cpu.registers.data[1], 0x1234_5678);
        assert_eq!(peek_long(&cpu, at), 0x1234_5678);
        cycles
    };
    assert_eq!(run(DATA + PAGE_BYTES - 2), run(DATA + PAGE_BYTES - 4));
}

#[test]
fn a_long_onto_a_hooked_page_shows_the_hook_its_low_word() {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT, MOVE_INDIRECT_TO_D1], &[]);
    let page = DATA + PAGE_BYTES;
    cpu.registers.set_address(0, page - 2);
    cpu.registers.data[0] = 0x1234_5678;
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_write_hook(Address(page)..=Address(page + 1), move |access| {
        into.borrow_mut().push((access.address.0, access.value));
        ControlFlow::Continue(())
    });
    let into = Rc::clone(&seen);
    cpu.add_read_hook(Address(page)..=Address(page + 1), move |access| {
        into.borrow_mut().push((access.address.0, access.value));
        ControlFlow::Continue(())
    });
    step(&mut cpu, 2);
    assert_eq!(*seen.borrow(), [(page, 0x5678), (page, 0x5678)]);
    assert_eq!(cpu.registers.data[1], 0x1234_5678);
}

#[test]
fn a_long_onto_the_bus_writes_its_high_word_then_faults_on_its_low() {
    let mut cpu = prepared(&[MOVE_D0_TO_INDIRECT], &[]);
    cpu.registers.set_address(0, RAM_BYTES - 2);
    cpu.registers.data[0] = 0x1234_5678;
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_invalid_hook(everywhere(), move |access: &InvalidAccess| {
        into.borrow_mut()
            .push((access.address, access.size, access.value));
        ControlFlow::Continue(())
    });
    step(&mut cpu, 1);
    assert_eq!(cpu.registers.pc.0, HANDLER);
    assert_eq!(
        *seen.borrow(),
        [(Address(RAM_BYTES), Size::Word, Some(0x5678))]
    );
    assert_eq!(
        cpu.memory
            .peek(Space::Supervisor, Address(RAM_BYTES - 2), Size::Word),
        Some(0x1234)
    );
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
    assert_eq!(cpu.registers.pc.0, HANDLER);
    assert_eq!(
        *seen.borrow(),
        [InvalidAccess {
            address: Address(NOWHERE),
            size: Size::Word,
            function_code: FunctionCode::SupervisorData,
            kind: BusCycleKind::Write,
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
    assert_eq!(cpu.registers.pc.0, HANDLER);
    assert_eq!(
        *seen.borrow(),
        [ExceptionEntry {
            vector: Vector::TRAP,
            pc: Address(CODE + 2),
            handler: Address(HANDLER),
            stack: Address(STACK - 8),
            level: None,
        }]
    );
}

#[test]
fn an_exception_return_hook_sees_the_frame_it_pops_and_stops_after_rte() {
    let mut cpu = prepared(&[TRAP, NOP], &[RTE]);
    let sr = status::S | status::Z;
    cpu.registers.set_sr(sr);
    let seen = log();
    let into = Rc::clone(&seen);
    let id = cpu.add_exception_return_hook(move |exception_return: &ExceptionReturn| {
        into.borrow_mut().push(*exception_return);
        ControlFlow::Break(())
    });
    assert_eq!(step(&mut cpu, 1).stop, Stop::Budget);
    assert_eq!(step(&mut cpu, 1).stop, Stop::Hook(id));
    // CPU32RM 6.2.12: SR and PC restored, the four words deallocated
    assert_eq!(cpu.registers.pc.0, CODE + 2);
    assert_eq!(cpu.registers.ssp.0, STACK);
    assert_eq!(
        *seen.borrow(),
        [ExceptionReturn {
            vector: Vector::TRAP,
            pc: Address(CODE + 2),
            sr,
            stack: Address(STACK - 8),
        }]
    );
    assert!(cpu.remove_hook(id));
    assert!(!cpu.has_hook(id));
}

#[test]
fn an_exception_return_hook_sees_a_bus_error_frame_returned_from() {
    let mut cpu = prepared(&[MOVE_INDIRECT_TO_D1], &[RTE]);
    cpu.registers.set_sr(status::S);
    cpu.registers.set_address(0, NOWHERE);
    let seen = log();
    let into = Rc::clone(&seen);
    cpu.add_exception_return_hook(move |exception_return: &ExceptionReturn| {
        into.borrow_mut().push(*exception_return);
        ControlFlow::Continue(())
    });
    step(&mut cpu, 2);
    // CPU32RM 6.3.2: the twelve-word frame's RTE restarts the MOVE
    let bus_error_frame = 24;
    assert_eq!(
        *seen.borrow(),
        [ExceptionReturn {
            vector: Vector::BUS_ERROR,
            pc: Address(CODE),
            sr: status::S,
            stack: Address(STACK - bus_error_frame),
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
    assert_eq!(cpu.registers.pc.0, HANDLER);
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
        into.borrow_mut().push((access.kind, access.reason));
        ControlFlow::Continue(())
    });
    step(&mut cpu, 1);
    assert_eq!(
        *cycles.borrow(),
        [(BusCycleKind::Fetch, Invalid::Protected)]
    );
    assert_eq!(cpu.registers.pc.0, HANDLER);
}

/// Runs of NOPs from [`CODE`], `instructions` of them less a few: the
/// least wall-clock time of several tries, against a loaded host.
fn time_nops(cpu: &mut Cpu, instructions: usize) -> Duration {
    /// Cycles in each run: the cost of fewer than 1000 NOPs.
    const BUDGET: Cycles = Cycles::from_clocks(1500);
    /// Tries.
    const TRIES: usize = 20;
    (0..TRIES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..instructions / 1000 {
                cpu.registers.pc = Address(CODE);
                cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycle::ZERO, BUDGET);
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
