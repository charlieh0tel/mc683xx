use std::time::Duration;

use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::integer::ParseIntegerError;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use super::Clause;
use super::Command;
use super::Line;
use super::Operand;
use super::ParseCommandError;
use super::ParseScriptError;
use super::Script;

/// `line`'s command.
fn command(line: &str) -> Command {
    line.parse().unwrap()
}

/// Why `line` is not a command.
fn error(line: &str) -> ParseCommandError {
    line.parse::<Command>().unwrap_err()
}

#[test]
fn write_memory_in_each_size() {
    assert_eq!(
        command("write_memory.b 0xfffa21 0x40"),
        Command::WriteMemory {
            address: Address(0xfffa21),
            size: Size::Byte,
            value: 0x40,
            function_code: FunctionCode::SupervisorData,
        }
    );
    assert_eq!(
        command("write_memory.w 0xFFFA44 16383"),
        Command::WriteMemory {
            address: Address(0xfffa44),
            size: Size::Word,
            value: 0x3fff,
            function_code: FunctionCode::SupervisorData,
        }
    );
    assert_eq!(
        command("  write_memory.l\t0x200000 0xdeadbeef fc 1  # user data"),
        Command::WriteMemory {
            address: Address(0x20_0000),
            size: Size::Long,
            value: 0xdead_beef,
            function_code: FunctionCode::UserData,
        }
    );
}

#[test]
fn register_writes() {
    assert_eq!(
        command("write_register A7 0x20fffc"),
        Command::WriteRegister {
            register: Register::A7,
            value: 0x20_fffc,
        }
    );
    assert_eq!(
        command("write_register d0 4294967295"),
        Command::WriteRegister {
            register: Register::D0,
            value: u32::MAX,
        }
    );
    assert_eq!(
        command("write_system_register VBR 0x200000"),
        Command::WriteSystemRegister {
            register: SystemRegister::Vbr,
            value: 0x20_0000,
        }
    );
}

#[test]
fn reads_with_and_without_mask_and_fc() {
    assert_eq!(
        command("read_memory.w 0xfffa44 expect 0x3fff"),
        Command::ReadMemory {
            address: Address(0xfffa44),
            size: Size::Word,
            expect: 0x3fff,
            mask: 0xffff,
            function_code: FunctionCode::SupervisorData,
        }
    );
    assert_eq!(
        command("read_memory.b 0x10 fc 6 mask 0xf0 expect 0x80"),
        Command::ReadMemory {
            address: Address(0x10),
            size: Size::Byte,
            expect: 0x80,
            mask: 0xf0,
            function_code: FunctionCode::SupervisorProgram,
        }
    );
}

#[test]
fn polls_take_mask_expect_and_timeout() {
    assert_eq!(
        command("poll_memory.l 0x300000 mask 0x80000000 expect 0 timeout_ms 250"),
        Command::PollMemory {
            address: Address(0x30_0000),
            size: Size::Long,
            expect: 0,
            mask: 0x8000_0000,
            timeout: Duration::from_millis(250),
            function_code: FunctionCode::SupervisorData,
        }
    );
    assert_eq!(
        command("poll_memory.b 0x300001 timeout_ms 0x10 expect 1 mask 1 fc 7"),
        Command::PollMemory {
            address: Address(0x30_0001),
            size: Size::Byte,
            expect: 1,
            mask: 1,
            timeout: Duration::from_millis(16),
            function_code: FunctionCode::CpuSpace,
        }
    );
}

#[test]
fn commands_print_as_they_parse() {
    for line in [
        "write_memory.b 0x00fffa21 0x40",
        "write_memory.l 0x00200000 0xdeadbeef fc user-data",
        "write_register a7 0x20fffc",
        "write_system_register sr 0x2700",
        "read_memory.w 0x00fffa44 expect 0x3fff",
        "read_memory.b 0x00000010 expect 0x80 mask 0xf0 fc supervisor-program",
        "poll_memory.w 0x00300000 mask 0x8000 expect 0x8000 timeout_ms 100",
        "poll_memory.b 0x00300001 mask 0x1 expect 0x0 timeout_ms 0 fc user-program",
    ] {
        assert_eq!(command(line).to_string(), line);
    }
}

#[test]
fn words_that_name_nothing() {
    assert_eq!(error(""), ParseCommandError::Empty);
    assert_eq!(error("  # only a comment"), ParseCommandError::Empty);
    assert_eq!(
        error("load 0x100"),
        ParseCommandError::Unknown {
            name: "load".to_owned()
        }
    );
    assert_eq!(
        error("write_register.l d0 1"),
        ParseCommandError::Unknown {
            name: "write_register.l".to_owned()
        }
    );
    assert_eq!(
        error("WRITE_MEMORY.b 0 0"),
        ParseCommandError::Unknown {
            name: "WRITE_MEMORY.b".to_owned()
        }
    );
    for line in ["write_memory 0 0", "read_memory.q 0 expect 0"] {
        assert!(
            matches!(error(line), ParseCommandError::Size { .. }),
            "{line}"
        );
    }
    assert_eq!(
        error("write_register d8 1"),
        ParseCommandError::Register {
            name: "d8".to_owned(),
            error: "d8".parse::<Register>().unwrap_err(),
        }
    );
    assert_eq!(
        error("write_system_register pc 1"),
        ParseCommandError::SystemRegister {
            name: "pc".to_owned(),
            error: "pc".parse::<SystemRegister>().unwrap_err(),
        }
    );
}

#[test]
fn missing_operands_and_clauses() {
    for (line, what) in [
        ("write_memory.w", Operand::Address),
        ("write_memory.w 0x100", Operand::Value),
        ("write_register", Operand::Register),
        ("write_register d0", Operand::Value),
        ("write_system_register vbr", Operand::Value),
        ("read_memory.w 0x100", Operand::Clause(Clause::Expect)),
        (
            "read_memory.w 0x100 mask 0xff",
            Operand::Clause(Clause::Expect),
        ),
        (
            "read_memory.w 0x100 expect",
            Operand::Clause(Clause::Expect),
        ),
        (
            "poll_memory.w 0x100 expect 0 timeout_ms 5",
            Operand::Clause(Clause::Mask),
        ),
        (
            "poll_memory.w 0x100 mask 1 timeout_ms 5",
            Operand::Clause(Clause::Expect),
        ),
        (
            "poll_memory.w 0x100 mask 1 expect 1",
            Operand::Clause(Clause::TimeoutMs),
        ),
        ("write_memory.w 0x100 1 fc", Operand::Clause(Clause::Fc)),
    ] {
        assert_eq!(error(line), ParseCommandError::Missing { what }, "{line}");
    }
}

#[test]
fn unexpected_and_repeated_words() {
    assert_eq!(
        error("write_memory.w 0x100 1 2"),
        ParseCommandError::Unexpected {
            word: "2".to_owned()
        }
    );
    assert_eq!(
        error("write_register d0 1 fc 5"),
        ParseCommandError::Unexpected {
            word: "fc".to_owned()
        }
    );
    assert_eq!(
        error("write_memory.w 0x100 1 mask 1"),
        ParseCommandError::Unexpected {
            word: "mask".to_owned()
        }
    );
    assert_eq!(
        error("read_memory.w 0x100 expect 1 expect 1"),
        ParseCommandError::Repeated {
            clause: Clause::Expect
        }
    );
}

#[test]
fn numbers_as_parse_integer_takes_them() {
    for (word, why) in [
        ("0x", ParseIntegerError::Invalid),
        ("0X10", ParseIntegerError::Invalid),
        ("x10", ParseIntegerError::Invalid),
        ("+5", ParseIntegerError::Invalid),
        ("-1", ParseIntegerError::Invalid),
        ("0x+5", ParseIntegerError::Invalid),
        ("$10", ParseIntegerError::Invalid),
        ("010", ParseIntegerError::Invalid),
        ("12a", ParseIntegerError::Invalid),
        ("4294967296", ParseIntegerError::TooLarge),
        ("0x100000000", ParseIntegerError::TooLarge),
    ] {
        assert_eq!(
            error(&format!("write_register d0 {word}")),
            ParseCommandError::Number {
                word: word.to_owned(),
                error: why,
            },
            "{word}"
        );
    }
    for (word, value) in [("0xAbC", 0xabc), ("1_000", 1000), ("0b101", 5)] {
        assert_eq!(
            command(&format!("write_register d0 {word}")),
            Command::WriteRegister {
                register: Register::D0,
                value,
            }
        );
    }
}

#[test]
fn values_fit_their_size() {
    assert_eq!(
        error("write_memory.b 0x100 0x100"),
        ParseCommandError::TooWide {
            value: 0x100,
            size: Size::Byte,
        }
    );
    assert_eq!(
        error("read_memory.w 0x100 expect 0x10000"),
        ParseCommandError::TooWide {
            value: 0x10000,
            size: Size::Word,
        }
    );
    assert_eq!(
        error("poll_memory.b 0x100 mask 0x1ff expect 0 timeout_ms 1"),
        ParseCommandError::TooWide {
            value: 0x1ff,
            size: Size::Byte,
        }
    );
    assert_eq!(
        error("read_memory.w 0x100 expect 0x3 mask 0x1"),
        ParseCommandError::OutsideMask { expect: 3, mask: 1 }
    );
}

#[test]
fn word_and_long_addresses_are_even() {
    assert_eq!(
        error("write_memory.w 0x101 0"),
        ParseCommandError::Odd {
            address: Address(0x101),
            size: Size::Word,
        }
    );
    assert_eq!(
        error("poll_memory.l 0x103 mask 1 expect 1 timeout_ms 1"),
        ParseCommandError::Odd {
            address: Address(0x103),
            size: Size::Long,
        }
    );
    assert!("write_memory.b 0x101 0".parse::<Command>().is_ok());
}

#[test]
fn function_codes_by_name_or_number() {
    for word in ["8", "0x8", "supervisor", "5x", "-1"] {
        assert_eq!(
            error(&format!("write_memory.b 0 0 fc {word}")),
            ParseCommandError::FunctionCode {
                word: word.to_owned()
            },
            "{word}"
        );
    }
    for (word, function_code) in [
        ("0", FunctionCode::Reserved0),
        ("0b111", FunctionCode::CpuSpace),
        ("CPU-Space", FunctionCode::CpuSpace),
        ("reserved3", FunctionCode::Reserved3),
    ] {
        assert_eq!(
            command(&format!("write_memory.b 0 0 fc {word}")),
            Command::WriteMemory {
                address: Address(0),
                size: Size::Byte,
                value: 0,
                function_code,
            }
        );
    }
}

#[test]
fn errors_say_what_is_wrong() {
    assert_eq!(
        error("write_memory.w 0x101 0").to_string(),
        "a 2-byte access at odd address 0x00000101"
    );
    assert_eq!(
        error("write_register x9 0").to_string(),
        "\"x9\": not a data or address register"
    );
    assert_eq!(
        error("write_memory.b 0 0x1ff").to_string(),
        "0x1ff does not fit in 8 bits"
    );
    assert_eq!(
        error("poll_memory.w 0x100 mask 1 expect 1").to_string(),
        "missing timeout_ms"
    );
    assert_eq!(
        error("read_memory.w 0x100 expect 1 fc 1 fc 2").to_string(),
        "`fc` given twice"
    );
    assert_eq!(error("write_memory.w").to_string(), "missing address");
}

#[test]
fn a_script_skips_comments_and_blank_lines_and_numbers_its_lines() {
    let script: Script = "# Chip selects\n\
         \n\
         write_memory.w 0xfffa44 0x3fff   # CSPAR0\n\
         \t\n\
         write_register a7 0x20fffc\n"
        .parse()
        .unwrap();
    assert_eq!(
        script.lines(),
        [
            Line {
                number: 3,
                command: command("write_memory.w 0xfffa44 0x3fff"),
            },
            Line {
                number: 5,
                command: command("write_register a7 0x20fffc"),
            },
        ]
    );
    assert_eq!("".parse::<Script>().unwrap().lines(), []);
}

#[test]
fn a_script_prints_its_commands_a_line_each() {
    let script: Script = "# Chip selects\n\
         write_memory.w 0xfffa44 0x3fff   # CSPAR0\n\
         \n\
         read_memory.w 0xfffa44 expect 0x3fff fc 6\n"
        .parse()
        .unwrap();
    let printed = script.to_string();
    assert_eq!(
        printed,
        "write_memory.w 0x00fffa44 0x3fff\n\
         read_memory.w 0x00fffa44 expect 0x3fff fc supervisor-program\n"
    );
    let reparsed: Script = printed.parse().unwrap();
    let commands = |script: &Script| {
        script
            .lines()
            .iter()
            .map(|line| line.command.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(commands(&reparsed), commands(&script));
    assert_eq!("".parse::<Script>().unwrap().to_string(), "");
}

#[test]
fn a_bad_line_names_its_number() {
    let error = "write_register d0 1\n\nwrite_register d9 1\n"
        .parse::<Script>()
        .unwrap_err();
    assert_eq!(
        error,
        ParseScriptError {
            line: 3,
            error: ParseCommandError::Register {
                name: "d9".to_owned(),
                error: "d9".parse::<Register>().unwrap_err(),
            },
        }
    );
    assert_eq!(
        error.to_string(),
        "line 3: \"d9\": not a data or address register"
    );
}

#[test]
fn clauses_and_operands_print_as_named_and_parse_back() {
    let operands = [Operand::Address, Operand::Value, Operand::Register]
        .into_iter()
        .chain(Clause::ALL.map(Operand::Clause));
    for operand in operands {
        assert_eq!(operand.to_string().parse(), Ok(operand));
    }
    for clause in Clause::ALL {
        assert_eq!(clause.to_string().parse(), Ok(clause));
    }
    assert_eq!("timeout_ms".parse(), Ok(Clause::TimeoutMs));
    assert_eq!(
        "Mask".parse::<Clause>().unwrap_err().to_string(),
        "no clause \"Mask\""
    );
    assert_eq!(
        "size".parse::<Operand>().unwrap_err().to_string(),
        "no operand \"size\""
    );
}
