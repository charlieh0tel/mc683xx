//! The chip under a debugger's control: `Cpu32Target`.

use std::ops::ControlFlow;

use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::bus::StopRequest;
use mc683xx_cpu32::cpu::Stop;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_cpu32::exception::Vector;
use mc683xx_cpu32::peripheral::PeripheralId;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::sim::chip_selects::Termination;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use super::BRANCH_TO_SELF;
use super::ROM_BYTES;
use super::Recorder;
use super::START;
use super::cs;
use super::image;
use super::on;
use super::write_register;
use crate::chip::Builder;
use crate::chip::Mc68331;
use crate::debug::TargetError;

/// BGND (CPU32RM 4.4).
const BGND: u16 = 0x4afa;
/// MOVEQ #1,D0.
const MOVEQ_1_D0: u16 = 0x7001;
/// TRAP #0 (CPU32RM 4, TRAP).
const TRAP_0: u16 = 0x4e40;
/// STOP, its immediate SR next (CPU32RM 4, STOP).
const STOP: u16 = 0x4e72;
/// RAM at a fixed address, on no select.
const RAM: u32 = 0x20_0000;
/// Bytes of RAM: one page.
const RAM_BYTES: usize = 2048;

/// A chip booting `program` from CSBOOT, with RAM at `RAM`, and the
/// handle by which its runs are asked to stop.
fn chip_and_stop(program: &[u16]) -> (Mc68331, StopRequest) {
    let mut builder = Builder::default();
    builder
        .memory(image(program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder
        .fixed_memory(Address(RAM), vec![0; RAM_BYTES], true)
        .unwrap();
    let request = builder.stop_request();
    (builder.build(), request)
}

/// A chip booting `program` from CSBOOT, with RAM at `RAM`.
fn chip(program: &[u16]) -> Mc68331 {
    chip_and_stop(program).0
}

#[test]
fn a_reset_with_bkpt_low_halts_before_the_first_instruction() {
    let mut chip = chip(&[MOVEQ_1_D0, BRANCH_TO_SELF]);
    assert_eq!(chip.reset_halted(), Ok(Entry::Breakpoint));
    assert_eq!(chip.read_system_register(SystemRegister::Rpc), Ok(START));
    assert_eq!(chip.read_system_register(SystemRegister::Pcc), Ok(START));
    assert_eq!(chip.read_system_register(SystemRegister::Atemp), Ok(0));
    chip.run_for(Cycles(1000));
    assert_eq!(chip.read_register(Register::D0), Ok(0), "nothing ran");
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Breakpoint)));
}

#[test]
fn registers_and_memory_through_the_target() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    chip.write_register(Register::D3, 0x1234_5678).unwrap();
    chip.write_register(Register::A2, 0x0000_abcd).unwrap();
    assert_eq!(chip.read_register(Register::D3), Ok(0x1234_5678));
    assert_eq!(chip.read_register(Register::A2), Ok(0x0000_abcd));
    chip.write_system_register(SystemRegister::Vbr, RAM)
        .unwrap();
    assert_eq!(chip.read_system_register(SystemRegister::Vbr), Ok(RAM));
    chip.write_system_register(SystemRegister::Far, 7).unwrap();
    assert_eq!(
        chip.read_system_register(SystemRegister::Far),
        Ok(0),
        "read only"
    );
    let data = [1, 2, 3, 4, 5, 6, 7, 8];
    for size in [Size::Byte, Size::Word, Size::Long] {
        chip.write_memory(Address(RAM), FunctionCode::SUPERVISOR_DATA, size, &data)
            .unwrap();
        let mut back = [0; 8];
        chip.read_memory(Address(RAM), FunctionCode::SUPERVISOR_DATA, size, &mut back)
            .unwrap();
        assert_eq!(back, data, "{size:?}");
    }
    let mut word = [0; 2];
    chip.read_memory(
        Address(START),
        FunctionCode::SUPERVISOR_PROGRAM,
        Size::Word,
        &mut word,
    )
    .unwrap();
    assert_eq!(word, BRANCH_TO_SELF.to_be_bytes());
    assert!(matches!(
        chip.read_memory(
            Address(RAM),
            FunctionCode::SUPERVISOR_DATA,
            Size::Long,
            &mut [0; 3]
        ),
        Err(TargetError::Length { .. })
    ));
    assert!(matches!(
        chip.read_memory(
            Address(RAM + 1),
            FunctionCode::SUPERVISOR_DATA,
            Size::Word,
            &mut word
        ),
        Err(TargetError::Access { .. })
    ));
}

#[test]
fn go_runs_to_a_bgnd_and_halt_stops_a_running_cpu() {
    let mut chip = chip(&[MOVEQ_1_D0, BGND, BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    assert_eq!(chip.go(), Ok(()));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(None));
    chip.run_for(Cycles(1000));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Background)));
    assert_eq!(chip.read_register(Register::D0), Ok(1));
    assert_eq!(
        chip.read_system_register(SystemRegister::Pcc),
        Ok(START + 2)
    );
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(START + 4)
    );
    chip.go().unwrap();
    chip.run_for(Cycles(1000));
    assert_eq!(
        Cpu32Target::halted(&mut chip),
        Ok(None),
        "spinning on the branch"
    );
    assert_eq!(chip.halt(), Ok(Entry::Breakpoint));
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(START + 4)
    );
    assert_eq!(chip.halt(), Ok(Entry::Breakpoint), "a halted CPU stays so");
}

#[test]
fn a_fault_on_go_s_first_fetch_is_its_own_exception() {
    // the reset's vector fetches are behind it: an odd RPC is an
    // address error, not a double bus fault (CPU32RM 7.2.8.12, 6.2.3)
    let handler = RAM + 0x100;
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    let space = FunctionCode::SUPERVISOR_DATA;
    chip.write_system_register(SystemRegister::Vbr, RAM)
        .unwrap();
    chip.write_memory(
        Address(RAM + Vector::ADDRESS_ERROR.offset()),
        space,
        Size::Long,
        &handler.to_be_bytes(),
    )
    .unwrap();
    chip.write_memory(Address(handler), space, Size::Word, &BGND.to_be_bytes())
        .unwrap();
    chip.write_system_register(SystemRegister::Ssp, RAM + 0x400)
        .unwrap();
    chip.write_system_register(SystemRegister::Rpc, START + 1)
        .unwrap();
    chip.go().unwrap();
    chip.run_for(Cycles(1000));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Background)));
    assert_eq!(chip.read_system_register(SystemRegister::Pcc), Ok(handler));
}

#[test]
fn a_step_stops_at_the_next_boundary_before_any_handler() {
    // MOVEQ #1,D0; TRAP #0; STOP #$2700; BGND
    let mut chip = chip(&[MOVEQ_1_D0, TRAP_0, STOP, 0x2700, BGND]);
    assert_eq!(chip.step(), Err(TargetError::NotHalted));
    chip.reset_halted().unwrap();
    let handler = RAM + 0x100;
    let space = FunctionCode::SUPERVISOR_DATA;
    chip.write_system_register(SystemRegister::Vbr, RAM)
        .unwrap();
    chip.write_memory(
        Address(RAM + Vector::trap(0).offset()),
        space,
        Size::Long,
        &handler.to_be_bytes(),
    )
    .unwrap();
    chip.write_system_register(SystemRegister::Ssp, RAM + 0x400)
        .unwrap();
    let rpc = |chip: &mut Mc68331| chip.read_system_register(SystemRegister::Rpc).unwrap();
    assert_eq!(chip.step(), Ok(Entry::Breakpoint));
    assert_eq!(rpc(&mut chip), START + 2);
    assert_eq!(chip.read_register(Register::D0), Ok(1));
    assert_eq!(chip.step(), Ok(Entry::Breakpoint), "the TRAP");
    assert_eq!(rpc(&mut chip), handler, "its handler not entered");
    chip.write_system_register(SystemRegister::Rpc, START + 4)
        .unwrap();
    assert_eq!(chip.step(), Ok(Entry::Breakpoint), "the STOP");
    assert_eq!(rpc(&mut chip), START + 8, "not stopped");
    assert_eq!(chip.step(), Ok(Entry::Background), "the BGND");
    assert_eq!(rpc(&mut chip), START + 10);
}

/// LPSTOP #$2000 (CPU32RM 4, LPSTOP).
const LPSTOP: [u16; 3] = [0xf800, 0x01c0, 0x2000];

#[test]
fn a_stepped_lpstop_leaves_the_chip_awake() {
    // under T1 the CPU does not enter the stopped condition (CPU32RM
    // 6.2.10); nor do the modules stay stopped
    let mut chip = chip(&[LPSTOP[0], LPSTOP[1], LPSTOP[2], BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    assert_eq!(chip.step(), Ok(Entry::Breakpoint));
    assert_eq!(chip.sim().low_power(), None);
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(START + 6)
    );
}

/// The QSM's base (MC68331UM Table D-1).
const QSM: u32 = 0xff_fc00;

/// Writes `value` to the word at `address` in supervisor data space.
fn store(chip: &mut Mc68331, address: u32, value: u16) {
    chip.write_memory(
        Address(address),
        FunctionCode::SUPERVISOR_DATA,
        Size::Word,
        &value.to_be_bytes(),
    )
    .unwrap();
}

#[test]
fn a_stepped_lpstop_takes_a_module_interrupt_it_unmasks() {
    // the SCI at level 3, vector $40, TIE and TE: TDRE requests at once
    // (QSMRM 4.6); masked until LPSTOP's SR
    let mut chip = chip(&[LPSTOP[0], LPSTOP[1], LPSTOP[2], BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    store(&mut chip, QSM, 0x0085);
    store(&mut chip, QSM + 0x04, 0x0340);
    store(&mut chip, QSM + 0x08, 1);
    store(&mut chip, QSM + 0x0a, 0x0088);
    let handler = RAM + 0x200;
    chip.write_system_register(SystemRegister::Vbr, RAM)
        .unwrap();
    chip.write_memory(
        Address(RAM + Vector(0x40).offset()),
        FunctionCode::SUPERVISOR_DATA,
        Size::Long,
        &handler.to_be_bytes(),
    )
    .unwrap();
    chip.write_system_register(SystemRegister::Ssp, RAM + 0x400)
        .unwrap();
    assert_eq!(chip.step(), Ok(Entry::Breakpoint));
    assert_eq!(
        chip.read_system_register(SystemRegister::Rpc),
        Ok(handler),
        "taken, its handler not entered"
    );
}

#[test]
fn a_stop_the_host_asks_for_in_a_step_fails_it() {
    let (mut chip, request) = chip_and_stop(&[MOVEQ_1_D0, BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    let at = Address(START);
    chip.cpu_control().add_code_hook(at..=at, move |_| {
        request.request();
        ControlFlow::Continue(())
    });
    assert_eq!(chip.step(), Err(TargetError::Stopped));
    assert_eq!(chip.read_register(Register::D0), Ok(1), "the MOVEQ ran");
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Breakpoint)));
    chip.go().unwrap();
    assert_ne!(
        chip.run_for(Cycles(100)).stop,
        Stop::Requested,
        "the request taken"
    );
}

/// The SIM's base (MC68331UM Table D-1).
const SIM: u32 = 0xff_fa00;
/// The GPT's base (MC68331UM Table D-1).
const GPT: u32 = 0xff_f900;
/// GPT TCNT (MC68331UM D.6.9).
const TCNT: u32 = GPT + 0x0a;

/// The word at `address` in supervisor data space.
fn load(chip: &mut Mc68331, address: u32) -> u16 {
    let mut word = [0; 2];
    chip.read_memory(
        Address(address),
        FunctionCode::SUPERVISOR_DATA,
        Size::Word,
        &mut word,
    )
    .unwrap();
    u16::from_be_bytes(word)
}

#[test]
fn freeze_holds_the_watchdog_with_frzsw_and_the_gpt_with_frz0() {
    // MC68331UM 4.2.13, 7.3.2
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    // FRZSW clear: the watchdog, on out of reset, resets the frozen
    // CPU, which comes out of it running
    chip.run_for(Cycles(1 << 20));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(None));
    chip.reset_halted().unwrap();
    // FRZSW, MM as it was; GPTMCR FRZ0 and SUPV
    store(&mut chip, SIM, 0x40cf);
    store(&mut chip, GPT, 0x2080);
    let counted = load(&mut chip, TCNT);
    chip.run_for(Cycles(1 << 20));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Breakpoint)));
    assert_eq!(load(&mut chip, TCNT), counted, "TCNT held");
    chip.go().unwrap();
    chip.run_for(Cycles(10_000));
    chip.halt().unwrap();
    assert_ne!(load(&mut chip, TCNT), counted, "counting once FREEZE ends");
}

#[test]
fn background_mode_writes_tcnt() {
    // TCNT is writable while FREEZE is asserted (GPTRM 3.1, 9.3);
    // GPTMCR FRZ0 and SUPV hold it there
    let mut chip = chip(&[BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    store(&mut chip, GPT, 0x2080);
    store(&mut chip, TCNT, 0x8000);
    assert_eq!(load(&mut chip, TCNT), 0x8000);
    chip.go().unwrap();
    chip.run_for(Cycles(10_000));
    chip.halt().unwrap();
    let counted = load(&mut chip, TCNT);
    assert!(counted > 0x8000, "{counted:#x}: counting on from the write");
}

#[test]
fn a_halt_in_lpstop_wakes_the_chip() {
    let mut chip = chip(&[LPSTOP[0], LPSTOP[1], LPSTOP[2], BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    chip.go().unwrap();
    chip.run_for(Cycles(1000));
    assert!(chip.sim().low_power().is_some(), "in LPSTOP");
    assert_eq!(chip.halt(), Ok(Entry::Breakpoint));
    assert_eq!(chip.sim().low_power(), None);
}

#[test]
fn a_hook_that_stops_a_step_is_not_a_step() {
    let mut chip = chip(&[MOVEQ_1_D0, BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    let at = Address(START);
    chip.cpu_control()
        .add_code_hook(at..=at, |_| ControlFlow::Break(()));
    assert_eq!(chip.step(), Err(TargetError::Stopped));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Breakpoint)));
    assert_eq!(chip.read_register(Register::D0), Ok(0), "nothing ran");
}

#[test]
fn without_a_bkpt_reset_the_cpu_cannot_be_halted() {
    let mut chip = chip(&[BRANCH_TO_SELF]);
    assert_eq!(chip.halt(), Err(TargetError::NotEnabled));
    assert_eq!(chip.go(), Err(TargetError::NotHalted));
    assert_eq!(
        chip.read_register(Register::D0),
        Err(TargetError::NotHalted)
    );
    chip.reset_halted().unwrap();
    chip.reset();
    assert_eq!(
        chip.halt(),
        Err(TargetError::NotEnabled),
        "BKPT high at reset"
    );
}

/// Slow memory at a fixed address, on no select: one page.
const SLOW: u32 = 0x30_0000;
/// What the slow memory holds, in every byte.
const SLOW_BYTE: u8 = 0x5a;
/// SYPCR (MC68331UM D.3.12), the low byte of its word.
const SYPCR: u32 = SIM + 0x20;
/// SYPCR BME, BMT 8 clocks, the watchdog off (MC68331UM D.3.12).
const SYPCR_BUS_MONITOR_8: u16 = 0x0007;
/// SIMCR out of reset (MC68331UM D.3.1), FRZBM clear.
const SIMCR_RESET: u16 = 0x00cf;
/// SIMCR out of reset but FRZBM, bit 13: FREEZE stops the bus monitor
/// (MC68331UM D.3.1, 4.2.13).
const SIMCR_FRZBM: u16 = 0x20cf;
/// MOVE.W (xxx).L,D0 (CPU32RM 4, MOVE), its address next.
const MOVE_W_ABSOLUTE_D0: u16 = 0x3039;

/// Whether a BDM read of the word at `SLOW` ends well, and whether the
/// core's page tables serve it (a debugger's write that needs no
/// cycle).
fn slow_access(chip: &mut Mc68331) -> (bool, bool) {
    let mut word = [0; 2];
    let read = chip.read_memory(
        Address(SLOW),
        FunctionCode::SUPERVISOR_DATA,
        Size::Word,
        &mut word,
    );
    assert!(
        matches!(read, Ok(()) | Err(TargetError::Access { .. })),
        "{read:?}"
    );
    if read.is_ok() {
        assert_eq!(word, [SLOW_BYTE; 2]);
    }
    let served = chip.poke(Address(SLOW), &[SLOW_BYTE]).is_ok();
    (read.is_ok(), served)
}

#[test]
fn freeze_stops_the_bus_monitor_with_frzbm() {
    // MC68331UM 4.2.13, D.3.1: 16 clocks of slow memory against the
    // monitor's 8 (SIMRM 3.2)
    let mut builder = Builder::default();
    builder
        .memory(
            image(&[BRANCH_TO_SELF], ROM_BYTES),
            false,
            &[ChipSelect::Boot],
        )
        .unwrap();
    builder
        .fixed_memory(Address(RAM), vec![0; RAM_BYTES], true)
        .unwrap();
    let slow = builder
        .fixed_memory(Address(SLOW), vec![SLOW_BYTE; RAM_BYTES], true)
        .unwrap();
    builder
        .fixed_timing(
            slow,
            PortSize::Bits16,
            Termination::WaitStates(Termination::MOST_WAIT_STATES),
        )
        .unwrap();
    let mut chip = builder.build();
    chip.reset_halted().unwrap();
    assert_eq!(slow_access(&mut chip), (true, true), "no monitor");
    store(&mut chip, SYPCR, SYPCR_BUS_MONITOR_8);
    assert_eq!(
        slow_access(&mut chip),
        (false, false),
        "frozen, FRZBM clear"
    );
    store(&mut chip, SIM, SIMCR_FRZBM);
    assert_eq!(
        slow_access(&mut chip),
        (true, true),
        "FRZBM written while frozen"
    );
    // the program, from RAM, reads the slow word; a bus error goes to
    // a BGND of its own
    let program = RAM + 0x200;
    let handler = RAM + 0x300;
    let space = FunctionCode::SUPERVISOR_DATA;
    let code: Vec<u8> = [MOVE_W_ABSOLUTE_D0, (SLOW >> 16) as u16, SLOW as u16, BGND]
        .iter()
        .flat_map(|word| word.to_be_bytes())
        .collect();
    chip.write_memory(Address(program), space, Size::Word, &code)
        .unwrap();
    chip.write_memory(Address(handler), space, Size::Word, &BGND.to_be_bytes())
        .unwrap();
    chip.write_memory(
        Address(RAM + Vector::BUS_ERROR.offset()),
        space,
        Size::Long,
        &handler.to_be_bytes(),
    )
    .unwrap();
    chip.write_system_register(SystemRegister::Vbr, RAM)
        .unwrap();
    chip.write_system_register(SystemRegister::Ssp, RAM + 0x400)
        .unwrap();
    chip.write_system_register(SystemRegister::Rpc, program)
        .unwrap();
    chip.go().unwrap();
    chip.run_for(Cycles(1000));
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Background)));
    assert_eq!(
        chip.read_system_register(SystemRegister::Pcc),
        Ok(handler),
        "the monitor watches the CPU's cycle once FREEZE ends"
    );
    assert_eq!(
        slow_access(&mut chip),
        (true, true),
        "frozen again by BGND, FRZBM set"
    );
    store(&mut chip, SIM, SIMCR_RESET);
    assert_eq!(
        slow_access(&mut chip),
        (false, false),
        "FRZBM cleared while frozen"
    );
}

/// CSBAR2 (MC68331UM D.3.25).
const CSBAR2: u32 = SIM + 0x54;
/// CSOR2 (MC68331UM D.3.26).
const CSOR2: u32 = SIM + 0x56;
/// CS2's option for breakpoint acknowledges: both bytes, read, DSACK
/// after no wait states, CPU space, any level (MC68331UM Table 4-22).
/// With CSBAR2 zero, its 2-Kbyte block covers $00001E.
const CSOR2_BREAKPOINT_ACKNOWLEDGE: u16 = 0x6800;
/// Where [`breakpoint_chip`]'s vector 12 handler is, from `RAM`.
const BREAKPOINT_HANDLER: u32 = 0x100;
/// Its supervisor stack's top, from `RAM`.
const BREAKPOINT_STACK: u32 = 0x400;

/// A chip running `program` with BDM disabled, its vectors and
/// supervisor stack in RAM, vector 12's handler at
/// [`BREAKPOINT_HANDLER`], and, if `answered`, a peripheral on CS2 that
/// ends breakpoint acknowledge cycles with DSACK.
fn breakpoint_chip(program: &[u16], answered: bool) -> (Mc68331, Option<PeripheralId>) {
    let mut builder = Builder::default();
    builder
        .memory(image(program, ROM_BYTES), false, &[ChipSelect::Boot])
        .unwrap();
    builder
        .fixed_memory(Address(RAM), vec![0; RAM_BYTES], true)
        .unwrap();
    let responder = answered.then(|| {
        builder
            .peripheral(Recorder::answering(BGND.into()), &[cs(2)])
            .unwrap()
    });
    let mut chip = builder.build();
    if answered {
        write_register(&mut chip, CSBAR2, 0x0000);
        write_register(&mut chip, CSOR2, CSOR2_BREAKPOINT_ACKNOWLEDGE);
    }
    let handler = RAM + BREAKPOINT_HANDLER;
    let entry = RAM + Vector::HARDWARE_BREAKPOINT.offset();
    chip.poke(Address(entry), &handler.to_be_bytes()).unwrap();
    let registers = chip.registers_mut();
    registers.vbr = RAM;
    registers.ssp = RAM + BREAKPOINT_STACK;
    (chip, responder)
}

#[test]
fn with_bdm_disabled_bkpt_runs_an_acknowledge_that_dsack_ignores() {
    let (mut chip, responder) = breakpoint_chip(&[MOVEQ_1_D0, MOVEQ_1_D0, BRANCH_TO_SELF], true);
    chip.run_for(Cycles(1));
    assert_eq!(chip.cpu().registers.pc, START + 2);
    assert!(!chip.assert_breakpoint(), "no background mode");
    chip.run_for(Cycles(1));
    let reads = &chip
        .peripheral::<Recorder>(responder.unwrap())
        .unwrap()
        .reads;
    // CPU space $00001E: type 0000, BKPT #7, T set; a word (CPU32RM
    // 5.3.1.1, 6.2.6; MC68331UM 4.5.4.1.2, Figure 4-12)
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].address, 0x1e);
    assert_eq!(reads[0].function_code, FunctionCode::CPU_SPACE);
    assert_eq!(reads[0].size, Size::Word);
    assert_eq!(reads[0].select, on(2));
    // the word returned is not executed, and nothing is stacked
    assert_eq!(chip.cpu().registers.ssp, RAM + BREAKPOINT_STACK);
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(None));
    chip.run_for(Cycles(1));
    assert_eq!(chip.cpu().registers.pc, START + 4, "the next instruction");
}

#[test]
fn with_bdm_disabled_bkpt_takes_vector_12_on_a_bus_error() {
    let (mut chip, _) = breakpoint_chip(&[MOVEQ_1_D0, MOVEQ_1_D0, BRANCH_TO_SELF], false);
    chip.run_for(Cycles(1));
    let sr = chip.cpu().registers.sr();
    assert!(!chip.assert_breakpoint());
    assert!(chip.cpu().breakpoint_pending(), "left for the next run");
    chip.run_for(Cycles(1));
    assert!(!chip.cpu().breakpoint_pending());
    assert_eq!(chip.cpu().registers.pc, RAM + BREAKPOINT_HANDLER);
    // format $2 (CPU32RM 6.2.6, 6.4.2): SR, the next PC, $2030, and the
    // instruction executing when the breakpoint was sensed
    let frame = RAM + BREAKPOINT_STACK - 12;
    assert_eq!(chip.cpu().registers.ssp, frame);
    let words: Vec<u32> = (0..6)
        .map(|word| {
            chip.peek(
                Address(frame + 2 * word),
                Size::Word,
                FunctionCode::SUPERVISOR_DATA,
            )
            .unwrap()
        })
        .collect();
    assert_eq!(
        words,
        [
            sr.into(),
            START >> 16,
            (START + 2) & 0xffff,
            0x2030,
            START >> 16,
            START & 0xffff
        ]
    );
}

#[test]
fn a_reset_drops_a_pending_breakpoint() {
    let (mut chip, _) = breakpoint_chip(&[MOVEQ_1_D0, BRANCH_TO_SELF], false);
    chip.assert_breakpoint();
    chip.reset();
    assert!(!chip.cpu().breakpoint_pending());
}

#[test]
fn with_bdm_enabled_bkpt_enters_background_mode_at_once() {
    let mut chip = chip(&[MOVEQ_1_D0, BRANCH_TO_SELF]);
    chip.reset_halted().unwrap();
    chip.go().unwrap();
    chip.run_for(Cycles(100));
    assert!(chip.assert_breakpoint());
    assert_eq!(Cpu32Target::halted(&mut chip), Ok(Some(Entry::Breakpoint)));
    assert!(!chip.cpu().breakpoint_pending());
}
