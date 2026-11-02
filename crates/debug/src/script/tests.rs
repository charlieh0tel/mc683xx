use std::time::Duration;

use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;
use mc683xx_target::register::ParseRegisterError;
use mc683xx_target::register::ParseSystemRegisterError;
use mc683xx_target::register::Register;
use mc683xx_target::register::SystemRegister;
use mc683xx_target::size::Size;

use super::Command;
use super::Line;
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
            function_code: FunctionCode::SUPERVISOR_DATA,
        }
    );
    assert_eq!(
        command("write_memory.w 0xFFFA44 16383"),
        Command::WriteMemory {
            address: Address(0xfffa44),
            size: Size::Word,
            value: 0x3fff,
            function_code: FunctionCode::SUPERVISOR_DATA,
        }
    );
    assert_eq!(
        command("  write_memory.l\t0x200000 0xdeadbeef fc 1  # user data"),
        Command::WriteMemory {
            address: Address(0x20_0000),
            size: Size::Long,
            value: 0xdead_beef,
            function_code: FunctionCode::USER_DATA,
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
            function_code: FunctionCode::SUPERVISOR_DATA,
        }
    );
    assert_eq!(
        command("read_memory.b 0x10 fc 6 mask 0xf0 expect 0x80"),
        Command::ReadMemory {
            address: Address(0x10),
            size: Size::Byte,
            expect: 0x80,
            mask: 0xf0,
            function_code: FunctionCode::SUPERVISOR_PROGRAM,
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
            function_code: FunctionCode::SUPERVISOR_DATA,
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
            function_code: FunctionCode::CPU_SPACE,
        }
    );
}

#[test]
fn commands_print_as_they_parse() {
    for line in [
        "write_memory.b 0x00fffa21 0x40",
        "write_memory.l 0x00200000 0xdeadbeef fc 1",
        "write_register a7 0x20fffc",
        "write_system_register sr 0x2700",
        "read_memory.w 0x00fffa44 expect 0x3fff",
        "read_memory.b 0x00000010 expect 0x80 mask 0xf0 fc 6",
        "poll_memory.w 0x00300000 mask 0x8000 expect 0x8000 timeout_ms 100",
        "poll_memory.b 0x00300001 mask 0x1 expect 0x0 timeout_ms 0 fc 2",
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
            error: ParseRegisterError,
        }
    );
    assert_eq!(
        error("write_system_register pc 1"),
        ParseCommandError::SystemRegister {
            name: "pc".to_owned(),
            error: ParseSystemRegisterError,
        }
    );
}

#[test]
fn missing_operands_and_clauses() {
    for (line, what) in [
        ("write_memory.w", "address"),
        ("write_memory.w 0x100", "value"),
        ("write_register", "register"),
        ("write_register d0", "value"),
        ("write_system_register vbr", "value"),
        ("read_memory.w 0x100", "expect"),
        ("read_memory.w 0x100 mask 0xff", "expect"),
        ("read_memory.w 0x100 expect", "expect"),
        ("poll_memory.w 0x100 expect 0 timeout_ms 5", "mask"),
        ("poll_memory.w 0x100 mask 1 timeout_ms 5", "expect"),
        ("poll_memory.w 0x100 mask 1 expect 1", "timeout_ms"),
        ("write_memory.w 0x100 1 fc", "fc"),
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
        ParseCommandError::Repeated { clause: "expect" }
    );
}

#[test]
fn numbers_hex_with_0x_or_decimal() {
    for word in [
        "0x",
        "0X10",
        "x10",
        "+5",
        "-1",
        "0x+5",
        "1_000",
        "12a",
        "4294967296",
        "0x100000000",
    ] {
        assert_eq!(
            error(&format!("write_register d0 {word}")),
            ParseCommandError::Number {
                word: word.to_owned()
            },
            "{word}"
        );
    }
    assert_eq!(
        command("write_register d0 0xAbC"),
        Command::WriteRegister {
            register: Register::D0,
            value: 0xabc,
        }
    );
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
fn function_codes_are_three_bits() {
    assert_eq!(
        error("write_memory.b 0 0 fc 8"),
        ParseCommandError::FunctionCode { value: 8 }
    );
    assert!("write_memory.b 0 0 fc 0".parse::<Command>().is_ok());
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
                error: ParseRegisterError,
            },
        }
    );
    assert_eq!(
        error.to_string(),
        "line 3: \"d9\": not a data or address register"
    );
}
