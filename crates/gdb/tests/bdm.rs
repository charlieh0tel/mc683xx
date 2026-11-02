//! The BDM stub driven over an in-memory connection with raw remote
//! serial protocol packets, over the emulated chip as a BDM debugger
//! drives it.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use mc683xx_cpu32::cycles::Cycles;
use mc683xx_gdb::Ending;
use mc683xx_gdb::SessionError;
use mc683xx_gdb::bdm::Bdm;
use mc683xx_gdb::serve;
use mc683xx_mc68331::chip::Builder;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_mc68331::debug::TargetError;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;
use wire::Send;
use wire::Wire;
use wire::bytes;
use wire::hex;
use wire::packet;
use wire::registers;
use wire::replies;

mod wire;

/// RAM, on no select.
const RAM: u32 = 0x20_0000;
/// Bytes of RAM.
const RAM_BYTES: usize = 2048;
/// Where the program starts, in RAM.
const START: u32 = RAM + 0x100;
/// Where it stores D0.
const STORE: u32 = RAM + 0x200;
/// The initial SSP.
const STACK: u32 = RAM + 0x800;
/// The program (CPU32RM 4): `moveq #5,d0`, `addq.l #1,d0`, `move.l
/// d0,(a0)`, `bra.s` back to its start.
const PROGRAM: [u16; 4] = [0x7005, 0x5280, 0x2080, 0x60f8];
/// The store's address in the program.
const STORE_PC: u32 = START + 4;
/// The branch's address.
const BRANCH_PC: u32 = START + 6;
/// An address in the boot ROM.
const ROM_ADDRESS: u32 = 0x100;
/// gdb's Ctrl-C.
const INTERRUPT: u8 = 0x03;
/// PC's index in a `g` packet.
const PC: usize = 17;
/// System clocks the chip runs each time the stub asks whether it has
/// stopped.
const LOOK: Cycles = Cycles(1000);

/// The chip as silicon behind a pod: it runs on its own, here a little
/// each time the stub looks.  The second field counts register writes.
struct Running(Mc68331, usize);

impl Cpu32Target for Running {
    type Error = TargetError;

    fn reset_halted(&mut self) -> Result<Entry, TargetError> {
        self.0.reset_halted()
    }

    fn halt(&mut self) -> Result<Entry, TargetError> {
        self.0.halt()
    }

    fn halted(&mut self) -> Result<Option<Entry>, TargetError> {
        if Cpu32Target::halted(&mut self.0)?.is_none() {
            self.0.run_for(LOOK);
        }
        Cpu32Target::halted(&mut self.0)
    }

    fn go(&mut self) -> Result<(), TargetError> {
        self.0.go()
    }

    fn step(&mut self) -> Result<Entry, TargetError> {
        self.0.step()
    }

    fn read_register(&mut self, register: Register) -> Result<u32, TargetError> {
        self.0.read_register(register)
    }

    fn write_register(&mut self, register: Register, value: u32) -> Result<(), TargetError> {
        self.1 += 1;
        self.0.write_register(register, value)
    }

    fn read_system_register(&mut self, register: SystemRegister) -> Result<u32, TargetError> {
        self.0.read_system_register(register)
    }

    fn write_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), TargetError> {
        self.1 += 1;
        self.0.write_system_register(register, value)
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &mut [u8],
    ) -> Result<(), TargetError> {
        self.0.read_memory(address, function_code, size, data)
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &[u8],
    ) -> Result<(), TargetError> {
        self.0.write_memory(address, function_code, size, data)
    }
}

/// The chip booting the program in RAM, reset halted, A0 at the store.
fn chip() -> Running {
    let mut rom = vec![0; RAM_BYTES];
    rom[..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&START.to_be_bytes());
    let mut ram = vec![0; RAM_BYTES];
    let program = bytes(&PROGRAM);
    let at = (START - RAM) as usize;
    ram[at..at + program.len()].copy_from_slice(&program);
    let mut builder = Builder::default();
    builder.memory(rom, false, &[ChipSelect::Boot]).unwrap();
    builder.fixed_memory(Address(RAM), ram, true).unwrap();
    let mut chip = builder.build();
    chip.reset_halted().unwrap();
    chip.write_register(Register::A0, STORE).unwrap();
    Running(chip, 0)
}

/// Serves `script` to `target`: how it ended and the stub's replies.
fn session(
    target: &mut Running,
    script: Vec<Send>,
) -> (
    Result<Ending, SessionError<TargetError, io::Error>>,
    Vec<String>,
) {
    let output = Rc::new(RefCell::new(Vec::new()));
    let ending = serve(&mut Bdm(target), Wire::new(script, &output));
    let replies = replies(&output.borrow());
    (ending, replies)
}

/// The word at `address`, as the target reads it.
fn word(target: &mut Running, address: u32) -> [u8; 2] {
    let mut word = [0; 2];
    target
        .read_memory(
            Address(address),
            FunctionCode::SUPERVISOR_DATA,
            Size::Word,
            &mut word,
        )
        .unwrap();
    word
}

#[test]
fn a_breakpoint_stops_on_its_instruction_and_a_step_goes_on() {
    let mut target = chip();
    let (ending, replies) = session(
        &mut target,
        vec![
            packet("g"),
            packet(&format!("Z0,{STORE_PC:x},2")),
            packet("c"),
            packet("g"),
            packet(&format!("z0,{STORE_PC:x},2")),
            packet("s"),
            packet("g"),
            packet(&format!("m{STORE:x},4")),
            packet("D"),
        ],
    );
    assert_eq!(ending.unwrap(), Ending::Detached);
    assert_eq!(registers(&replies[0])[PC], START);
    assert_eq!(replies[1], "OK");
    assert!(replies[2].starts_with("T05") && replies[2].contains("swbreak"));
    assert_eq!(registers(&replies[3])[PC], STORE_PC, "on the breakpoint");
    assert_eq!(replies[4], "OK");
    assert_eq!(replies[5], "S05");
    assert_eq!(registers(&replies[6])[PC], BRANCH_PC, "the store ran");
    assert_eq!(replies[7], "00000006");
    assert_eq!(
        Cpu32Target::halted(&mut target.0),
        Ok(None),
        "detached, it runs"
    );
    target.halt().unwrap();
    assert_eq!(word(&mut target, STORE_PC), [0x20, 0x80], "put back");
}

#[test]
fn a_breakpoint_left_planted_is_removed_with_the_session() {
    let mut target = chip();
    let (ending, _) = session(
        &mut target,
        vec![packet(&format!("Z0,{STORE_PC:x},2")), packet("k")],
    );
    assert_eq!(ending.unwrap(), Ending::Killed);
    assert_eq!(word(&mut target, STORE_PC), [0x20, 0x80]);
    assert!(
        Cpu32Target::halted(&mut target.0).unwrap().is_some(),
        "killed, halted"
    );
}

#[test]
fn a_breakpoint_in_rom_is_refused_and_registers_write_through() {
    let mut target = chip();
    let (ending, replies) = session(
        &mut target,
        vec![
            packet(&format!("Z0,{ROM_ADDRESS:x},2")),
            // a short G: D0 alone
            packet("G00000042"),
            packet("k"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[0], "E16");
    assert_eq!(replies[1], "OK");
    assert_eq!(target.read_register(Register::D0), Ok(0x42));
    assert_eq!(target.1, 1, "D0 alone written");
    assert_eq!(target.read_system_register(SystemRegister::Rpc), Ok(START));
}

#[test]
fn ctrl_c_halts_a_running_target() {
    let mut target = chip();
    let (ending, replies) = session(
        &mut target,
        vec![packet("c"), Send::Now(vec![INTERRUPT]), packet("k")],
    );
    ending.unwrap();
    assert_eq!(replies[0], "S02");
    assert_eq!(
        Cpu32Target::halted(&mut target.0),
        Ok(Some(Entry::Breakpoint))
    );
}

#[test]
fn monitor_sets_the_function_code_and_resets() {
    let mut target = chip();
    let (ending, replies) = session(
        &mut target,
        vec![
            packet("s"),
            packet(&format!("qRcmd,{}", hex("fc 6"))),
            packet(&format!("qRcmd,{}", hex("fc 9"))),
            packet(&format!("qRcmd,{}", hex("reset"))),
            packet("g"),
            packet("k"),
        ],
    );
    ending.unwrap();
    let said = |text: &str| {
        replies
            .iter()
            .any(|reply| *reply == format!("O{}", hex(text)))
    };
    assert!(said("fc 6\n"));
    assert!(said("fc takes a function code, 0 to 7\n"));
    assert_eq!(registers(replies.last().unwrap())[PC], START, "reset");
}

#[test]
fn a_session_lost_while_running_halts_and_cleans_up() {
    // a breakpoint the program never reaches, then a continue, then
    // nothing: the connection ends with the target running
    let mut target = chip();
    let never = START + 0x40;
    let (ending, _) = session(
        &mut target,
        vec![packet(&format!("Z0,{never:x},2")), packet("c")],
    );
    assert!(matches!(ending, Err(SessionError::Connection(_))));
    assert!(
        Cpu32Target::halted(&mut target.0).unwrap().is_some(),
        "halted"
    );
    assert_eq!(word(&mut target, never), [0, 0], "put back");
}

#[test]
fn a_breakpoint_comes_out_of_the_space_it_went_into_and_removal_repeats() {
    let mut target = chip();
    let (ending, replies) = session(
        &mut target,
        vec![
            packet(&format!("Z0,{STORE_PC:x},2")),
            packet(&format!("qRcmd,{}", hex("fc 2"))),
            packet(&format!("z0,{STORE_PC:x},2")),
            packet(&format!("z0,{STORE_PC:x},2")),
            packet("k"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[0], "OK");
    assert_eq!(replies.iter().filter(|reply| *reply == "OK").count(), 4);
    assert_eq!(word(&mut target, STORE_PC), [0x20, 0x80]);
}

#[test]
fn memory_over_a_breakpoint_is_the_program_s() {
    // gdb sees the word BGND replaced, and a write to it is what the
    // removal puts back
    let mut target = chip();
    let byte = STORE_PC + 1;
    let (ending, replies) = session(
        &mut target,
        vec![
            packet(&format!("Z0,{STORE_PC:x},2")),
            packet(&format!("m{STORE_PC:x},2")),
            packet(&format!("M{byte:x},1:81")),
            packet(&format!("m{STORE_PC:x},2")),
            packet("k"),
        ],
    );
    ending.unwrap();
    assert_eq!(replies[1], "2080");
    assert_eq!(replies[2], "OK");
    assert_eq!(replies[3], "2081");
    assert_eq!(word(&mut target, STORE_PC), [0x20, 0x81]);
}
