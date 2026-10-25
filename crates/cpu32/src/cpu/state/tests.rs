use std::cell::RefCell;
use std::ops::ControlFlow;
use std::rc::Rc;

use super::CpuState;
use super::StateError;
use super::VERSION;
use super::decode;
use super::encode;
use super::restore_each;
use crate::address::Address;
use crate::cpu::Cpu;
use crate::cycles::Cycles;
use crate::effective_address::Altered;
use crate::instructions::tests::CODE;
use crate::instructions::tests::NoBus;
use crate::instructions::tests::NoInterrupts;
use crate::instructions::tests::machine;
use crate::memory::Memory;
use crate::memory::PAGE_BYTES;
use crate::memory::Permissions;
use crate::memory::Space;
use crate::registers::status;
use crate::size::Size;

/// Where the loop stores.
const DATA: u32 = 0x3000;

/// A loop: MOVE.W D0, (A0)+; DBRA D0, back; BRA to the start.  It
/// fills [`DATA`] on with a falling count, changing flow every
/// instruction or two.
const LOOP: [u16; 4] = [0x30c0, 0x51c8, 0xfffc, 0x60f8];

/// A CPU on the loop, the counter and pointer set.
fn looping() -> Cpu {
    let mut cpu = machine(&LOOP);
    cpu.registers.set_address(0, DATA);
    cpu.registers.data[0] = 40;
    cpu
}

/// The PCs of the instructions `budget` cycles run, in order.
fn trace(cpu: &mut Cpu, budget: u64) -> Vec<u32> {
    let pcs = Rc::new(RefCell::new(Vec::new()));
    let into = Rc::clone(&pcs);
    let id = cpu.add_code_hook(Address(0)..=Address(u32::MAX), move |instruction| {
        into.borrow_mut().push(instruction.pc.0);
        ControlFlow::Continue(())
    });
    cpu.run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(budget));
    cpu.remove_hook(id);
    pcs.take()
}

/// What a run leaves that the next could see.
fn observed(cpu: &Cpu) -> (Vec<u8>, u64) {
    (cpu.save(), cpu.instructions())
}

#[test]
fn a_snapshot_mid_run_reproduces_the_run() {
    let mut cpu = looping();
    trace(&mut cpu, 333);
    let snapshot = cpu.save();
    let first = trace(&mut cpu, 1000);
    let after = observed(&cpu);
    assert!(
        first.len() > 50 && first.contains(&(CODE + 6)),
        "{first:x?}"
    );
    // The same CPU, back.
    cpu.restore(&snapshot).unwrap();
    assert_eq!(trace(&mut cpu, 1000), first);
    assert_eq!(observed(&cpu), after);
    // A fresh one, on other code.
    let mut other = machine(&[0x4e71; 4]);
    other.restore(&snapshot).unwrap();
    assert_eq!(trace(&mut other, 1000), first);
    assert_eq!(observed(&other), after);
    assert_eq!(
        other
            .memory
            .peek(Space::Supervisor, Address(DATA), Size::Word),
        Some(40)
    );
}

#[test]
fn the_format_matches_its_golden() {
    let mut cpu = looping();
    trace(&mut cpu, 333);
    snapshot_golden::check(
        env!("CARGO_MANIFEST_DIR"),
        "cpu",
        &[("cpu", VERSION)],
        &cpu.save(),
    );
}

#[test]
fn hooks_stay_and_permissions_come_back() {
    let mut cpu = looping();
    cpu.memory
        .protect(Space::User, Address(0), PAGE_BYTES, Permissions::READ)
        .unwrap();
    let snapshot = cpu.save();
    let mut other = machine(&LOOP);
    let id = other.add_write_hook(Address(DATA)..=Address(DATA), |_| ControlFlow::Break(()));
    other.restore(&snapshot).unwrap();
    assert_eq!(
        other.memory.permissions(Space::User, Address(0)),
        Permissions::READ
    );
    assert_eq!(
        other
            .run_for(&mut NoBus, &mut NoInterrupts, Cycles::ZERO, Cycles(1000))
            .stop,
        crate::cpu::Stop::Hook(id)
    );
}

#[test]
fn another_format_is_refused() {
    let mut cpu = looping();
    let mut snapshot = cpu.save();
    snapshot[0] ^= 0xff;
    assert!(matches!(
        cpu.restore(&snapshot),
        Err(StateError::Version { found, expected }) if (found, expected) == (VERSION ^ 0xff, VERSION)
    ));
    assert!(matches!(
        cpu.restore(&snapshot[..2]),
        Err(StateError::Truncated)
    ));
    let good = looping().save();
    assert!(matches!(
        cpu.restore(&good[..good.len() / 2]),
        Err(StateError::Decode(_))
    ));
    let mut longer = good.clone();
    longer.push(0);
    assert!(matches!(
        cpu.restore(&longer),
        Err(StateError::Invalid(reason)) if reason == "bytes past the end of the snapshot"
    ));
    // Unchanged by the refusals.
    assert_eq!(cpu.registers.data[0], 40);
}

#[test]
fn other_hardware_is_refused() {
    let snapshot = looping().save();
    let mut small = Cpu::new(Memory::new(24).unwrap());
    small.registers.set_sr(status::S);
    assert!(matches!(
        small.restore(&snapshot),
        Err(StateError::Mismatch(reason)) if reason == "memory regions"
    ));
    let mut wide = Cpu::new(Memory::new(32).unwrap());
    assert!(matches!(
        wide.restore(&snapshot),
        Err(StateError::Mismatch(reason)) if reason == "address lines"
    ));
    assert_eq!(small.registers.sr(), status::S);
}

/// `snapshot` with `change` made to what it holds.
fn edited(snapshot: &[u8], change: impl FnOnce(&mut CpuState)) -> Vec<u8> {
    let mut state: CpuState = decode(VERSION, snapshot).unwrap();
    change(&mut state);
    encode(VERSION, &state)
}

#[test]
fn a_state_no_run_could_save_is_refused() {
    let snapshot = looping().save();
    let invalid: [(Vec<u8>, &str); 3] = [
        (
            edited(&snapshot, |state| {
                state.registers.set_sr_unmasked(status::S | 1 << 5);
            }),
            "status register",
        ),
        (
            edited(&snapshot, |state| state.altered = Altered::only(8, 0)),
            "altered address registers",
        ),
        (
            edited(&snapshot, |state| state.sampled_mask = 8),
            "sampled interrupt mask",
        ),
    ];
    let mut cpu = looping();
    for (bytes, what) in invalid {
        assert!(matches!(cpu.restore(&bytes), Err(StateError::Invalid(reason)) if reason == what));
    }
    assert_eq!(cpu.save(), snapshot, "unchanged by the refusals");
}

#[test]
fn a_refused_restore_puts_back_only_the_parts_restored_before() {
    // Part 2 of four refuses: parts 0 and 1 get their backups, part 2
    // is asked once, and part 3 never.
    let snapshots = [[10_u8], [11], [12], [13]];
    let backups = [[0_u8], [1], [2], [3]];
    let mut calls = Vec::new();
    let refused = restore_each(&snapshots, &backups, |part, bytes| {
        calls.push((part, bytes[0]));
        if part == 2 {
            Err(StateError::invalid("part 2"))
        } else {
            Ok(())
        }
    });
    assert!(matches!(refused, Err(StateError::Invalid(reason)) if reason == "part 2"));
    assert_eq!(calls, [(0, 10), (1, 11), (2, 12), (0, 0), (1, 1)]);
}

#[test]
fn a_part_refusing_its_backup_leaves_the_rest_put_back_and_the_first_error() {
    let snapshots = [[10_u8], [11], [12]];
    let backups = [[0_u8], [1], [2]];
    let mut calls = Vec::new();
    let refused = restore_each(&snapshots, &backups, |part, bytes| {
        calls.push((part, bytes[0]));
        match (part, bytes[0]) {
            (0, 0) => Err(StateError::invalid("part 0's backup")),
            (2, _) => Err(StateError::invalid("part 2")),
            _ => Ok(()),
        }
    });
    assert!(matches!(refused, Err(StateError::Invalid(reason)) if reason == "part 2"));
    assert_eq!(calls, [(0, 10), (1, 11), (2, 12), (0, 0), (1, 1)]);
}

#[test]
fn a_reason_may_be_built_at_run_time() {
    let register = 3;
    let error = StateError::invalid(format!("register {register}"));
    assert_eq!(error.to_string(), "snapshot inconsistent: register 3");
    assert!(matches!(error, StateError::Invalid(reason) if reason == "register 3"));
    assert_eq!(
        StateError::mismatch("a fixed reason").to_string(),
        "snapshot of other hardware: a fixed reason"
    );
}
