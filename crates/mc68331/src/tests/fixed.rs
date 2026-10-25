//! Memory at a fixed address, on no chip select: emulator-only.

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::BusError;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::cpu::state::StateError;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::function_code::FunctionCode;
use mc683xx_cpu32::size::Size;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::chip_selects::Termination;

use super::BRANCH_TO_SELF;
use super::NOP;
use super::ROM_BYTES;
use super::Recorder;
use super::SIM;
use super::cs;
use super::cycle;
use super::cycle_in;
use super::image;
use super::load_word;
use super::store_word;
use super::write_register;
use crate::chip::BuildError;
use crate::chip::Builder;
use crate::chip::Mc68331;
use crate::chip::MemoryId;
use crate::event::Event;
use crate::state;
use mc683xx_modules::sim::reset::ResetScope;

/// JMP (xxx).L (CPU32RM 4, JMP).
const JUMP_ABSOLUTE: u16 = 0x4ef9;
/// RESET (CPU32RM 4, RESET).
const RESET: u16 = 0x4e70;
/// Where the fixed code goes: a consumer's rewritten routines.
const CODE: u32 = 0x60_0000;
/// Where the fixed data goes.
const DATA: u32 = 0x6f_0000;
/// The fixed memories' size.
const FIXED_BYTES: usize = 4 << 10;
/// CSBARBT on a 1-Mbyte block from $600000 (SIMRM 7.10.2).
const CSBARBT_AT_CODE: u16 = 0x6007;
/// CSBAR1 and CSOR1: CS1 on 2 Kbytes at $600000, fast termination,
/// both bytes, read and write, any space (SIMRM 7.10).
const CS1_AT_CODE: [(u32, u16); 2] = [(SIM + 0x50, 0x6000), (SIM + 0x52, 0x7bb0)];

/// A boot ROM that jumps to `CODE`.
fn jumping() -> Vec<u8> {
    image(
        &[JUMP_ABSOLUTE, (CODE >> 16) as u16, CODE as u16],
        ROM_BYTES,
    )
}

/// `program` at the start of a fixed memory's image.
fn code(program: &[u16]) -> Vec<u8> {
    let mut bytes = vec![0; FIXED_BYTES];
    for (index, word) in program.iter().enumerate() {
        bytes[2 * index..2 * index + 2].copy_from_slice(&word.to_be_bytes());
    }
    bytes
}

/// A chip booting into `program` at `CODE`, read only, with writable
/// data at `DATA` filled with `0x5a`, reloaded at `reload`.
fn fixed(program: &[u16], reload: ResetScope) -> (Mc68331, MemoryId, MemoryId) {
    let mut builder = Builder::default();
    builder
        .memory(jumping(), false, &[ChipSelect::Boot])
        .unwrap();
    let code = builder
        .fixed_memory(Address(CODE), code(program), false)
        .unwrap();
    let data = builder
        .fixed_memory(Address(DATA), vec![0x5a; FIXED_BYTES], true)
        .unwrap();
    builder.reload_scope(data, reload);
    (builder.build(), code, data)
}

/// `program`, then a branch to itself.
fn ending(program: &[&[u16]]) -> Vec<u16> {
    program
        .iter()
        .flat_map(|words| words.iter().copied())
        .chain([BRANCH_TO_SELF])
        .collect()
}

#[test]
fn reset_code_runs_in_a_fixed_memory_with_no_select_programmed() {
    let program = ending(&[&store_word(DATA + 2, 0x1234), &load_word(DATA, 1)]);
    let (mut chip, _, data) = fixed(&program, ResetScope::NONE);
    chip.run_for(Cycles(500));
    assert_eq!(&chip.memory(data)[0..4], [0x5a, 0x5a, 0x12, 0x34]);
    assert_eq!(chip.cpu().registers.data[1] & 0xffff, 0x5a5a);
    let pc = chip.cpu().registers.pc;
    assert!((CODE..CODE + FIXED_BYTES as u32).contains(&pc), "{pc:#x}");
    // a fixed memory's cycle costs only the tables' two clocks, in any
    // space but CPU space
    assert_eq!(
        cycle(&mut chip, DATA + 2, Size::Word, None),
        Ok((0x1234, 2))
    );
    assert_eq!(
        cycle_in(
            &mut chip,
            FunctionCode::USER_PROGRAM,
            CODE,
            Size::Word,
            None
        ),
        Ok((u32::from(program[0]), 2))
    );
    assert_eq!(
        cycle_in(&mut chip, FunctionCode::CPU_SPACE, CODE, Size::Word, None),
        Err(BusError)
    );
    // past it, nothing answers
    let past = DATA + FIXED_BYTES as u32;
    assert_eq!(cycle(&mut chip, past, Size::Word, None), Err(BusError));
    assert_eq!(
        chip.peek(Address(DATA + 2), Size::Word, FunctionCode::SUPERVISOR_DATA),
        Ok(0x1234)
    );
}

#[test]
fn a_chip_select_matching_a_cycle_wins_over_a_fixed_memory() {
    let mut builder = Builder::default();
    builder
        .memory(image(&[NOP], ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    let selected = builder.memory(vec![0x11; 2048], true, &[cs(1)]).unwrap();
    let fixed = builder
        .fixed_memory(Address(CODE), vec![0x22; FIXED_BYTES], true)
        .unwrap();
    let mut chip = builder.build();
    assert_eq!(cycle(&mut chip, CODE, Size::Word, None), Ok((0x2222, 2)));
    for (register, value) in CS1_AT_CODE {
        write_register(&mut chip, register, value);
    }
    assert_eq!(cycle(&mut chip, CODE, Size::Word, None), Ok((0x1111, 2)));
    cycle(&mut chip, CODE, Size::Word, Some(0x3333)).unwrap();
    assert_eq!(chip.memory(selected)[0..2], [0x33, 0x33]);
    assert_eq!(chip.memory(fixed)[0..2], [0x22, 0x22]);
    assert_eq!(
        chip.peek(Address(CODE), Size::Word, FunctionCode::SUPERVISOR_DATA),
        Ok(0x3333)
    );
    // past CS1's 2 Kbytes, the fixed memory answers again
    assert_eq!(
        cycle(&mut chip, CODE + 0x800, Size::Word, None),
        Ok((0x2222, 2))
    );
    // CSBOOT moved over it: its ROM's NOPs answer
    write_register(&mut chip, SIM + 0x48, CSBARBT_AT_CODE);
    assert_eq!(
        cycle(&mut chip, CODE + 0x800, Size::Word, None),
        Ok((u32::from(NOP), 16))
    );
}

#[test]
fn a_read_only_fixed_memory_ignores_writes() {
    let program = ending(&[&store_word(CODE + 0x100, 0x1234)]);
    let (mut chip, code, _) = fixed(&program, ResetScope::NONE);
    chip.run_for(Cycles(500));
    assert_eq!(chip.halted(), None);
    assert_eq!(chip.memory(code)[0x100..0x102], [0, 0]);
    assert_eq!(
        cycle(&mut chip, CODE + 0x100, Size::Word, Some(0x55)),
        Ok((0, 2))
    );
    assert_eq!(chip.memory(code)[0x100..0x102], [0, 0]);
}

#[test]
fn a_fixed_memory_survives_resets_unless_reloaded() {
    let (mut kept, _, kept_data) = fixed(&[BRANCH_TO_SELF], ResetScope::NONE);
    let only_external = ResetScope::NONE.with(ResetCause::External);
    let (mut reloaded, _, reloaded_data) = fixed(&[BRANCH_TO_SELF], only_external);
    for chip in [&mut kept, &mut reloaded] {
        cycle(chip, DATA, Size::Word, Some(0x1234)).unwrap();
        chip.reset();
    }
    assert_eq!(kept.memory(kept_data)[0..2], [0x12, 0x34]);
    assert_eq!(reloaded.memory(reloaded_data)[0..2], [0x5a, 0x5a]);
    // the watchdog's reset is not in its scope
    cycle(&mut reloaded, DATA, Size::Word, Some(0x1234)).unwrap();
    reloaded.take_events();
    reloaded.run_for(Cycles(2 * 512 * 256));
    assert!(watchdog_reset(&mut reloaded));
    assert_eq!(reloaded.memory(reloaded_data)[0..2], [0x12, 0x34]);
}

/// Whether the watchdog reset the chip since the last look.
fn watchdog_reset(chip: &mut Mc68331) -> bool {
    chip.take_events()
        .iter()
        .any(|event| matches!(event, Event::Reset(_, ResetCause::Watchdog)))
}

#[test]
fn the_watchdogs_reset_reloads_a_memory_in_its_scope() {
    let (mut chip, _, data) = fixed(&[BRANCH_TO_SELF], ResetScope::ALL);
    cycle(&mut chip, DATA, Size::Word, Some(0x1234)).unwrap();
    chip.take_events();
    chip.run_for(Cycles(2 * 512 * 256));
    assert!(watchdog_reset(&mut chip));
    assert_eq!(chip.memory(data)[0..2], [0x5a, 0x5a]);
}

#[test]
fn the_reset_instruction_reloads_before_the_next_instruction() {
    let program = ending(&[&store_word(DATA, 0x1234), &[RESET], &load_word(DATA, 0)]);
    let system = ResetScope::NONE.with(ResetCause::System);
    let (mut reloaded, _, _) = fixed(&program, system);
    let (mut kept, _, _) = fixed(&program, ResetScope::NONE);
    for chip in [&mut reloaded, &mut kept] {
        chip.run_for(Cycles(2000));
        assert_eq!(chip.halted(), None);
    }
    assert_eq!(reloaded.cpu().registers.data[0] & 0xffff, 0x5a5a);
    assert_eq!(kept.cpu().registers.data[0] & 0xffff, 0x1234);
}

#[test]
fn a_fixed_memory_is_in_the_snapshot_and_never_diverted() {
    let mut builder = Builder::default();
    builder
        .memory(
            image(&[BRANCH_TO_SELF], ROM_BYTES),
            false,
            &[ChipSelect::Boot],
        )
        .unwrap();
    let selected = builder.memory(vec![0x11; 2048], false, &[cs(1)]).unwrap();
    let fixed = builder
        .fixed_memory(Address(DATA), vec![0x5a; FIXED_BYTES], true)
        .unwrap();
    let peripheral = builder.unwired_peripheral(Recorder::answering(0xbeef));
    let mut chip = builder.build();
    cycle(&mut chip, DATA, Size::Word, Some(0x1234)).unwrap();
    let snapshot = chip.save().unwrap();
    cycle(&mut chip, DATA, Size::Word, Some(0x5678)).unwrap();
    chip.restore(&snapshot).unwrap();
    assert_eq!(chip.memory(fixed)[0..2], [0x12, 0x34]);
    assert_eq!(cycle(&mut chip, DATA, Size::Word, None), Ok((0x1234, 2)));
    // asking to divert it does nothing
    chip.divert_memory(fixed, Some(peripheral));
    assert_eq!(cycle(&mut chip, DATA, Size::Word, None), Ok((0x1234, 2)));
    assert_eq!(chip.save().unwrap(), snapshot);
    // nor does a snapshot divert it
    chip.divert_memory(selected, Some(peripheral));
    let diverted = chip.save().unwrap();
    let to_fixed = state::with_diverted(&diverted, |diverted| diverted[0].0 = fixed.0);
    assert!(matches!(
        chip.restore(&to_fixed),
        Err(StateError::Invalid(reason)) if reason == "a fixed memory diverted"
    ));
}

#[test]
fn a_fixed_memory_must_fit_on_pages_and_not_overlap() {
    let mut builder = Builder::default();
    let page = vec![0; 2048];
    assert_eq!(
        builder.fixed_memory(Address(CODE + 2), page.clone(), true),
        Err(BuildError::Placement {
            base: Address(CODE + 2),
            length: 2048
        })
    );
    assert_eq!(
        builder.fixed_memory(Address(0xff_f800), vec![0; 4096], true),
        Err(BuildError::Placement {
            base: Address(0xff_f800),
            length: 4096
        })
    );
    assert_eq!(
        builder.fixed_memory(Address(CODE), vec![0; 100], true),
        Err(BuildError::Length(100))
    );
    builder
        .fixed_memory(Address(CODE), vec![0; 4096], true)
        .unwrap();
    assert_eq!(
        builder.fixed_memory(Address(CODE + 2048), page.clone(), true),
        Err(BuildError::Overlap {
            base: Address(CODE + 2048),
            other: Address(CODE)
        })
    );
    builder
        .fixed_memory(Address(CODE + 4096), page, true)
        .unwrap();
}

/// How far a chip running NOPs from `CODE` gets in a few thousand
/// clocks, its fixed code timed `timing` if given.
fn nops_reached(timing: Option<(PortSize, Termination)>) -> u32 {
    let mut builder = Builder::default();
    builder
        .memory(jumping(), false, &[ChipSelect::Boot])
        .unwrap();
    let code = builder
        .fixed_memory(Address(CODE), code(&[NOP; FIXED_BYTES / 2]), false)
        .unwrap();
    if let Some((port, termination)) = timing {
        builder.fixed_timing(code, port, termination).unwrap();
    }
    let mut chip = builder.build();
    chip.run_for(Cycles(4_000));
    chip.cpu().registers.pc - CODE
}

#[test]
fn a_fixed_memory_runs_at_the_port_and_wait_states_it_is_given() {
    let fast = nops_reached(None);
    assert_eq!(
        fast,
        nops_reached(Some((PortSize::Bits16, Termination::Fast))),
        "fast termination on 16 bits by default"
    );
    let waiting = nops_reached(Some((PortSize::Bits16, Termination::WaitStates(1))));
    let narrow = nops_reached(Some((PortSize::Bits8, Termination::WaitStates(1))));
    assert!(
        fast > waiting && waiting > narrow,
        "{fast} {waiting} {narrow}"
    );
}

#[test]
fn fixed_timing_takes_only_fixed_memories_and_internal_terminations() {
    let mut builder = Builder::default();
    let rom = builder
        .memory(jumping(), false, &[ChipSelect::Boot])
        .unwrap();
    let code = builder
        .fixed_memory(Address(CODE), code(&[NOP]), false)
        .unwrap();
    assert_eq!(
        builder.fixed_timing(rom, PortSize::Bits16, Termination::Fast),
        Err(BuildError::NotFixed)
    );
    for termination in [
        Termination::External,
        Termination::Eclk,
        Termination::WaitStates(Termination::MOST_WAIT_STATES + 1),
    ] {
        assert_eq!(
            builder.fixed_timing(code, PortSize::Bits8, termination),
            Err(BuildError::Termination)
        );
    }
    assert_eq!(
        builder.fixed_timing(
            code,
            PortSize::Bits8,
            Termination::WaitStates(Termination::MOST_WAIT_STATES)
        ),
        Ok(())
    );
}
