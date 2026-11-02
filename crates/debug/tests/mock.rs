//! Loads and scripts on a mock target that records its calls and fails
//! where a test asks: a byte that drops writes, a byte that bus-errors,
//! a status bit that sets after some reads.

use mc683xx_debug::image::Image;
use mc683xx_debug::image::Segment;
use mc683xx_debug::load::LoadError;
use mc683xx_debug::load::LoadOptions;
use mc683xx_debug::load::OddEdges;
use mc683xx_debug::load::Verification;
use mc683xx_debug::script::Command;
use mc683xx_debug::script::CommandError;
use mc683xx_debug::script::Script;
use mc683xx_debug::script::ScriptError;
use mc683xx_target::Cpu32Target;
use mc683xx_target::address::Address;
use mc683xx_target::entry::Entry;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

/// Bytes of the mock's memory, from address 0.
const MEMORY_BYTES: usize = 0x1000;
/// The status byte that reads 0 until enough reads, then [`READY`].
const STATUS: u32 = 0x800;
/// The status byte's ready bit.
const READY: u8 = 0x80;

/// The mock's error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum MockError {
    /// An access touched the failing byte, or memory the mock lacks.
    #[error("bus error at {0}")]
    Bus(Address),
}

/// One memory call the mock took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Call {
    /// A write, or a read.
    write: bool,
    /// Its address.
    address: Address,
    /// Its space.
    function_code: FunctionCode,
    /// Its access size.
    size: Size,
    /// Its bytes.
    bytes: usize,
}

/// A halted target: memory, registers and its faults.
#[derive(Debug)]
struct Mock {
    /// Memory from 0.
    memory: Vec<u8>,
    /// D0-D7, A0-A7.
    registers: [u32; 16],
    /// RPC.
    rpc: u32,
    /// VBR.
    vbr: u32,
    /// A byte whose writes are dropped.
    stuck: Option<u32>,
    /// A byte an access touching fails, on writes or on reads.
    failing: Option<(u32, bool)>,
    /// Reads of [`STATUS`] before it reads ready.
    ready_after: usize,
    /// Reads of [`STATUS`] so far.
    status_reads: usize,
    /// The memory calls, in order.
    calls: Vec<Call>,
}

impl Mock {
    /// A mock with zeroed memory and no faults, never ready.
    fn new() -> Self {
        Self {
            memory: vec![0; MEMORY_BYTES],
            registers: [0; 16],
            rpc: 0,
            vbr: 0,
            stuck: None,
            failing: None,
            ready_after: usize::MAX,
            status_reads: 0,
            calls: Vec::new(),
        }
    }

    /// The call's byte addresses, or the error for one that fails.
    fn check(&mut self, call: Call) -> Result<std::ops::Range<usize>, MockError> {
        self.calls.push(call);
        let start = call.address.0 as usize;
        let range = start..start + call.bytes;
        if let Some((byte, on_write)) = self.failing
            && on_write == call.write
            && range.contains(&(byte as usize))
        {
            return Err(MockError::Bus(Address(byte)));
        }
        if range.end > self.memory.len() {
            return Err(MockError::Bus(call.address));
        }
        Ok(range)
    }
}

impl Cpu32Target for Mock {
    type Error = MockError;

    fn reset_halted(&mut self) -> Result<Entry, MockError> {
        Ok(Entry::Breakpoint)
    }

    fn halt(&mut self) -> Result<Entry, MockError> {
        Ok(Entry::Breakpoint)
    }

    fn halted(&mut self) -> Result<Option<Entry>, MockError> {
        Ok(Some(Entry::Breakpoint))
    }

    fn go(&mut self) -> Result<(), MockError> {
        Ok(())
    }

    fn step(&mut self) -> Result<Entry, MockError> {
        Ok(Entry::Breakpoint)
    }

    fn read_register(&mut self, register: Register) -> Result<u32, MockError> {
        Ok(self.registers[usize::from(register.code())])
    }

    fn write_register(&mut self, register: Register, value: u32) -> Result<(), MockError> {
        self.registers[usize::from(register.code())] = value;
        Ok(())
    }

    fn read_system_register(&mut self, register: SystemRegister) -> Result<u32, MockError> {
        Ok(match register {
            SystemRegister::Rpc => self.rpc,
            SystemRegister::Vbr => self.vbr,
            _ => 0,
        })
    }

    fn write_system_register(
        &mut self,
        register: SystemRegister,
        value: u32,
    ) -> Result<(), MockError> {
        match register {
            SystemRegister::Rpc => self.rpc = value,
            SystemRegister::Vbr => self.vbr = value,
            _ => {}
        }
        Ok(())
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &mut [u8],
    ) -> Result<(), MockError> {
        let range = self.check(Call {
            write: false,
            address,
            function_code,
            size,
            bytes: data.len(),
        })?;
        if range.contains(&(STATUS as usize)) {
            self.status_reads += 1;
            if self.status_reads > self.ready_after {
                self.memory[STATUS as usize] = READY;
            }
        }
        data.copy_from_slice(&self.memory[range]);
        Ok(())
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        size: Size,
        data: &[u8],
    ) -> Result<(), MockError> {
        let range = self.check(Call {
            write: true,
            address,
            function_code,
            size,
            bytes: data.len(),
        })?;
        for (at, byte) in range.zip(data) {
            if self.stuck != Some(at as u32) {
                self.memory[at] = *byte;
            }
        }
        Ok(())
    }
}

/// `length` bytes counting up from `first`.
fn counting(first: u8, length: usize) -> Vec<u8> {
    (0..length).map(|n| first.wrapping_add(n as u8)).collect()
}

/// The memory calls of one direction.
fn calls(mock: &Mock, write: bool) -> Vec<(u32, Size, usize)> {
    mock.calls
        .iter()
        .filter(|call| call.write == write)
        .map(|call| (call.address.0, call.size, call.bytes))
        .collect()
}

#[test]
fn a_load_writes_words_in_supervisor_data_verifies_and_sets_rpc() {
    let mut mock = Mock::new();
    let image = Image::new(
        [
            Segment::new(Address(0x100), counting(1, 8)),
            Segment::new(Address(0x400), counting(0x40, 600)),
        ],
        Some(Address(0x100)),
    )
    .unwrap();
    let report = image.load(&mut mock, &LoadOptions::default()).unwrap();
    assert_eq!(
        report
            .segments
            .iter()
            .map(|segment| (segment.address, segment.bytes))
            .collect::<Vec<_>>(),
        [(Address(0x100), 8), (Address(0x400), 600)]
    );
    assert_eq!(report.bytes, 608);
    assert_eq!(report.entry, Some(Address(0x100)));
    assert_eq!(report.verification, Verification::Passed);
    assert_eq!(mock.rpc, 0x100);
    assert_eq!(mock.memory[0x100..0x108], counting(1, 8));
    assert_eq!(mock.memory[0x400..0x658], counting(0x40, 600));
    let chunks = vec![
        (0x100, Size::Word, 8),
        (0x400, Size::Word, 256),
        (0x500, Size::Word, 256),
        (0x600, Size::Word, 88),
    ];
    assert_eq!(calls(&mock, true), chunks);
    assert_eq!(calls(&mock, false), chunks, "read back as written");
    assert!(
        mock.calls
            .iter()
            .all(|call| call.function_code == FunctionCode::SUPERVISOR_DATA)
    );
    assert_eq!(
        report.to_string(),
        "0x00000100: 8 bytes\n0x00000400: 600 bytes\n\
         608 bytes in 2 segments, verified, RPC 0x00000100"
    );
}

#[test]
fn odd_edges_are_refused_before_any_write_or_written_in_bytes() {
    let image = Image::new(
        [
            Segment::new(Address(0x100), counting(1, 4)),
            Segment::new(Address(0x201), counting(1, 4)),
        ],
        None,
    )
    .unwrap();
    let mut mock = Mock::new();
    let error = image.load(&mut mock, &LoadOptions::default()).unwrap_err();
    assert!(
        matches!(
            error,
            LoadError::OddEdge {
                address: Address(0x201)
            }
        ),
        "{error}"
    );
    assert!(mock.calls.is_empty(), "nothing written");

    let mut options = LoadOptions::default();
    options.odd_edges = OddEdges::Bytes;
    options.function_code = FunctionCode::SUPERVISOR_PROGRAM;
    let report = image.load(&mut mock, &options).unwrap();
    assert_eq!(report.entry, None);
    assert_eq!(mock.rpc, 0, "no entry, RPC alone");
    assert_eq!(
        calls(&mock, true),
        [
            (0x100, Size::Word, 4),
            (0x201, Size::Byte, 1),
            (0x202, Size::Word, 2),
            (0x204, Size::Byte, 1),
        ]
    );
    assert_eq!(mock.memory[0x201..0x205], counting(1, 4));
    assert!(
        mock.calls
            .iter()
            .all(|call| call.function_code == FunctionCode::SUPERVISOR_PROGRAM)
    );

    let mut mock = Mock::new();
    let error = Image::from_binary(Address(0x300), counting(1, 3))
        .unwrap()
        .load(&mut mock, &LoadOptions::default())
        .unwrap_err();
    assert!(
        matches!(
            error,
            LoadError::OddEdge {
                address: Address(0x302)
            }
        ),
        "the odd end: {error}"
    );
}

#[test]
fn a_byte_that_does_not_take_its_write_fails_verification() {
    let mut mock = Mock::new();
    mock.stuck = Some(0x305);
    let mut image = Image::from_binary(Address(0x300), counting(1, 8)).unwrap();
    image.set_entry(Some(Address(0x300)));
    let error = image.load(&mut mock, &LoadOptions::default()).unwrap_err();
    assert!(
        matches!(
            error,
            LoadError::Verify {
                address: Address(0x305),
                wrote: 6,
                read: 0,
            }
        ),
        "{error}"
    );
    assert_eq!(error.to_string(), "0x00000305: wrote 0x06, read back 0x00");
    assert_eq!(mock.rpc, 0, "RPC left after a failed load");

    let mut options = LoadOptions::default();
    options.verify = false;
    let report = image.load(&mut mock, &options).unwrap();
    assert_eq!(report.verification, Verification::Skipped);
    assert!(calls(&mock, false).len() == 1, "no read back this time");
    assert_eq!(mock.rpc, 0x300);
}

#[test]
fn a_failed_write_or_read_back_names_its_call() {
    let image = Image::from_binary(Address(0x100), counting(0, 0x300)).unwrap();
    let mut mock = Mock::new();
    mock.failing = Some((0x250, true));
    let error = image.load(&mut mock, &LoadOptions::default()).unwrap_err();
    assert!(
        matches!(
            error,
            LoadError::Write {
                address: Address(0x200),
                bytes: 256,
                error: MockError::Bus(Address(0x250)),
            }
        ),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        "writing 256 bytes at 0x00000200: bus error at 0x00000250"
    );

    let mut mock = Mock::new();
    mock.failing = Some((0x350, false));
    let error = image.load(&mut mock, &LoadOptions::default()).unwrap_err();
    assert!(
        matches!(
            error,
            LoadError::Read {
                address: Address(0x300),
                bytes: 256,
                error: MockError::Bus(Address(0x350)),
            }
        ),
        "{error}"
    );
}

#[test]
fn a_script_writes_and_checks_in_order() {
    let mut mock = Mock::new();
    let script: Script = "write_memory.l 0x100 0x12345678\n\
         write_memory.b 0x105 0xab fc 1\n\
         write_register a7 0x20fffc\n\
         write_system_register vbr 0x200000\n\
         read_memory.w 0x102 expect 0x5678\n\
         read_memory.l 0x100 mask 0xff00ff00 expect 0x12005600\n"
        .parse()
        .unwrap();
    script.run(&mut mock).unwrap();
    assert_eq!(mock.memory[0x100..0x106], [0x12, 0x34, 0x56, 0x78, 0, 0xab]);
    assert_eq!(mock.registers[usize::from(Register::A7.code())], 0x20_fffc);
    assert_eq!(mock.vbr, 0x20_0000);
    assert_eq!(
        mock.calls,
        [
            Call {
                write: true,
                address: Address(0x100),
                function_code: FunctionCode::SUPERVISOR_DATA,
                size: Size::Long,
                bytes: 4,
            },
            Call {
                write: true,
                address: Address(0x105),
                function_code: FunctionCode::USER_DATA,
                size: Size::Byte,
                bytes: 1,
            },
            Call {
                write: false,
                address: Address(0x102),
                function_code: FunctionCode::SUPERVISOR_DATA,
                size: Size::Word,
                bytes: 2,
            },
            Call {
                write: false,
                address: Address(0x100),
                function_code: FunctionCode::SUPERVISOR_DATA,
                size: Size::Long,
                bytes: 4,
            },
        ]
    );
}

#[test]
fn a_mismatch_stops_the_script_at_its_line() {
    let mut mock = Mock::new();
    let script: Script = "write_memory.w 0x100 0x1234\n\
         # the check\n\
         read_memory.w 0x100 expect 0x4321\n\
         write_register d0 1\n"
        .parse()
        .unwrap();
    let error = script.run(&mut mock).unwrap_err();
    assert!(
        matches!(
            error,
            ScriptError {
                line: 3,
                command: Command::ReadMemory { .. },
                error: CommandError::Mismatch { read: 0x1234 },
            }
        ),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        "line 3: `read_memory.w 0x00000100 expect 0x4321`: read 0x1234"
    );
    assert_eq!(mock.registers[0], 0, "stopped there");
}

#[test]
fn a_target_error_names_the_access() {
    let mut mock = Mock::new();
    let error = "write_memory.w 0x2000 1"
        .parse::<Script>()
        .unwrap()
        .run(&mut mock)
        .unwrap_err();
    assert!(
        matches!(
            error.error,
            CommandError::Target(MockError::Bus(Address(0x2000)))
        ),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        "line 1: `write_memory.w 0x00002000 0x1`: bus error at 0x00002000"
    );
}

#[test]
fn a_poll_reads_until_ready_or_times_out() {
    let mut mock = Mock::new();
    mock.ready_after = 3;
    let poll: Command = "poll_memory.b 0x800 mask 0x80 expect 0x80 timeout_ms 10000"
        .parse()
        .unwrap();
    poll.run(&mut mock).unwrap();
    assert_eq!(mock.status_reads, 4);

    let mut mock = Mock::new();
    let error = "poll_memory.b 0x800 mask 0x80 expect 0x80 timeout_ms 0"
        .parse::<Script>()
        .unwrap()
        .run(&mut mock)
        .unwrap_err();
    assert!(
        matches!(
            error,
            ScriptError {
                line: 1,
                error: CommandError::Timeout { read: 0 },
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(mock.status_reads, 1, "one read, always");
    assert!(error.to_string().ends_with(": timed out, last read 0x0"));

    let mut mock = Mock::new();
    let error = "poll_memory.w 0x2000 mask 1 expect 1 timeout_ms 5"
        .parse::<Command>()
        .unwrap()
        .run(&mut mock)
        .unwrap_err();
    assert!(matches!(error, CommandError::Target(MockError::Bus(_))));
}
