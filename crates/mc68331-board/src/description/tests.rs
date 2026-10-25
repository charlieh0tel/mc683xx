use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::bus::PortSize;
use mc683xx_cpu32::interrupt::IrqPin;
use mc683xx_modules::peripheral::BusDecode;
use mc683xx_modules::sim::chip_selects::Termination;
use std::env;
use std::fs;
use std::path::Path;
use std::process;

use mc683xx_mc68331::chip::Mc68331;
use mc683xx_modules::chip_select::ChipSelect;
use mc683xx_modules::chip_select::Number;
use mc683xx_modules::sim::ResetCause;
use mc683xx_modules::sim::reset::ResetScope;

use super::Description;
use super::Options;
use super::Target;
use crate::board::Board;
use crate::endpoint::Endpoint;
use crate::error::LoadError;
use crate::peripheral::Lane;
use crate::peripheral::Role;
use crate::registry::Environment;
use crate::registry::ParameterValue;
use crate::registry::Registry;

/// A board with RAM and a peripheral, for the errors to break.
const BASE: &str = r#"
version = 1

[chip]
type = "mc68331"

[[memory]]
name = "ram"
size = 0x800
writable = true
attach = [{ select = "CS0" }]
"#;

/// `text` parsed with no options, its paths in a directory that holds
/// nothing.
fn parse(text: &str) -> Result<Description, LoadError> {
    Description::parse(text, Path::new("/nonexistent"), &Options::default())
}

/// `BASE` plus `extra`, an `extra` that starts `[chip]` adding its keys
/// to `BASE`'s chip table.
fn with(extra: &str) -> String {
    match extra.strip_prefix("[chip]\n") {
        Some(keys) => BASE.replace(CHIP_TYPE, &format!("{CHIP_TYPE}{keys}")),
        None => format!("{BASE}{extra}"),
    }
}

/// `BASE`'s chip, as its `[chip]` table names it.
const CHIP_TYPE: &str = "type = \"mc68331\"\n";

/// The error `BASE` plus `extra` gives, as text: the file's, or the
/// load's around the MC68331, which checks the chip's table and pins.
fn error(extra: &str) -> String {
    match parse(&with(extra)) {
        Err(error) => error.to_string(),
        Ok(description) => Board::<Mc68331>::load(
            &description,
            &Registry::with_builtins(),
            Environment::default(),
        )
        .map(|_| ())
        .unwrap_err()
        .to_string(),
    }
}

/// CS`number`.
fn cs(number: u8) -> ChipSelect {
    ChipSelect::Numbered(Number::new(number).unwrap())
}

#[test]
fn a_board_resolves() {
    let description = parse(&format!(
        "{BASE}{}",
        r#"
[board]
name = "test"
options = ["panel"]

[[decoder]]
name = "u1"
select = "CS5"
address_bits = [13, 14, "low"]
alias = { "3" = 1 }

[[peripheral]]
name = "latch"
type = "registers"
attach = [{ decoder = "u1", output = 0, role = "write", lines = [1, 2] }]
reset = []

[[peripheral]]
name = "panel"
type = "fixed"
option = "panel"
attach = [{ qspi = 0, mask = 0x1 }]

[[peripheral]]
name = "pps"
type = "divider"

[[net]]
name = "one-pps"
type = "edge"
from = "pps.out"
to = ["chip.PGP0", "external.scope"]
"#
    ))
    .unwrap();
    assert_eq!(description.name(), Some("test"));
    assert_eq!(description.memories[0].pins, vec![cs(0)]);
    assert_eq!(
        description.decoders[0].outputs,
        vec![0, 1, 2, 1, 4, 5, 6, 7]
    );
    let latch = &description.peripherals[0];
    assert_eq!(latch.attachments[0].role, Role::Write);
    assert_eq!(latch.attachments[0].lines, vec![1, 2]);
    assert_eq!(
        latch.attachments[0].target,
        Target::Decoder {
            decoder: 0,
            output: 0
        }
    );
    assert!(!latch.reset.contains(ResetCause::PowerOn));
    assert_eq!(
        description.peripherals.len(),
        2,
        "the panel's option is off"
    );
    assert_eq!(
        description.nets[0].to,
        vec![
            Endpoint::Chip("PGP0".to_owned()),
            Endpoint::External("scope".to_owned())
        ]
    );
}

#[test]
fn parameters_and_aliases_take_a_note() {
    let description = parse(&format!(
        "{BASE}{}",
        r#"
[[decoder]]
name = "u1"
select = "CS5"
address_bits = [13]
alias = { note = "U1 pin 3 lifted", "1" = 0 }

[[peripheral]]
name = "latch"
type = "fixed"
parameters = { note = "from the schematic", value = 1 }
"#
    ))
    .unwrap();
    assert_eq!(description.decoders[0].outputs, vec![0, 0]);
    let parameters = &description.peripherals[0].parameters;
    assert_eq!(parameters.keys().collect::<Vec<_>>(), ["value"]);
}

#[test]
fn an_option_puts_its_entries_on_the_board() {
    let text = "version = 1\n[chip]\ntype = \"mc68331\"\n[board]\noptions = [\"x\"]\n[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\noption = \"x\"\n";
    let options = Options {
        enabled: ["x".to_owned()].into(),
        ..Options::default()
    };
    let description = Description::parse(text, Path::new("."), &options).unwrap();
    assert_eq!(description.peripherals.len(), 1);
    let options = Options {
        enabled: ["y".to_owned()].into(),
        ..Options::default()
    };
    assert!(matches!(
        Description::parse(text, Path::new("."), &options),
        Err(LoadError::Option(option)) if option == "y"
    ));
}

#[test]
fn a_run_sets_parameters_over_the_files() {
    let text = "version = 1\n[chip]\ntype = \"mc68331\"\n[board]\noptions = [\"x\"]\n[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\nparameters = { value = 1, keep = \"k\" }\n[[peripheral]]\nname = \"e\"\ntype = \"fixed\"\noption = \"x\"\n";
    let mut options = Options::default();
    options.set("d.value=0x10").unwrap();
    options.set("d.name=eeprom.bin").unwrap();
    options.set("e.value=2").unwrap();
    options.set_parameter("d", "on", true);
    let mut description = Description::parse(text, Path::new("."), &options).unwrap();
    let parameters = &description.peripherals[0].parameters;
    assert_eq!(parameters["value"], ParameterValue::Integer(0x10));
    assert_eq!(parameters["keep"], ParameterValue::from("k"));
    assert_eq!(
        parameters["name"],
        ParameterValue::from("eeprom.bin"),
        "a bare word is a string"
    );
    assert_eq!(parameters["on"], ParameterValue::Boolean(true));
    description.set_parameter("d", "value", 3).unwrap();
    assert_eq!(
        description.peripherals[0].parameters["value"],
        ParameterValue::Integer(3)
    );
    assert!(matches!(
        description.set_parameter("z", "value", 3),
        Err(LoadError::Peripheral(peripheral)) if peripheral == "z"
    ));
    options.set("z.value=1").unwrap();
    assert!(matches!(
        Description::parse(text, Path::new("."), &options),
        Err(LoadError::Peripheral(peripheral)) if peripheral == "z"
    ));
    for malformed in ["d.value", "value=1", ".value=1", "d.=1"] {
        assert!(matches!(
            Options::default().set(malformed),
            Err(LoadError::Setting(setting)) if setting == malformed
        ));
    }
}

#[test]
fn a_lane_role_is_a_lane() {
    let description = parse(&format!(
        "{BASE}{}",
        r#"
[[peripheral]]
name = "upper"
type = "fixed"
attach = [{ select = "CS2", role = "upper" }]

[[peripheral]]
name = "lower"
type = "fixed"
attach = [{ select = "CS2", lane = "lower" }]
"#
    ))
    .unwrap();
    let upper = &description.peripherals[0].attachments[0];
    assert_eq!((upper.role, upper.lane), (Role::ReadWrite, Lane::Upper));
}

#[test]
fn versions_other_than_one_are_refused() {
    assert!(matches!(parse("version = 2"), Err(LoadError::Version(_))));
    assert!(matches!(parse("[chip]"), Err(LoadError::Version(_))));
    assert!(matches!(
        parse("version = 2\nfuture = 1"),
        Err(LoadError::Version(_))
    ));
}

#[test]
fn unknown_keys_are_refused_with_their_line() {
    let message = error("[chip]\ncrystal = 1\n");
    assert!(message.contains("crystal"), "{message}");
    assert!(message.contains("line"), "{message}");
}

#[test]
fn the_file_names_its_chip_and_another_chip_refuses_it() {
    assert!(matches!(
        parse(&BASE.replace(CHIP_TYPE, "")),
        Err(LoadError::NoChip)
    ));
    assert!(matches!(
        parse(
            "version = 1\n[[memory]]\nname = \"rom\"\nsize = 0x800\nattach = [{ select = \"CSBOOT\" }]\n"
        ),
        Err(LoadError::NoChip)
    ));
    let description = parse(&BASE.replace("mc68331", "mc68332")).unwrap();
    assert_eq!(description.chip_type(), "mc68332");
    let error = Board::<Mc68331>::load(
        &description,
        &Registry::with_builtins(),
        Environment::default(),
    )
    .map(|_| ())
    .unwrap_err();
    assert!(
        matches!(&error, LoadError::ChipType { found, expected: "mc68331" } if found == "mc68332"),
        "{error}"
    );
    let description = parse(&with("[chip]\nreference_hz = 32768\n")).unwrap();
    assert_eq!(description.chip_type(), "mc68331");
    assert!(
        Board::<Mc68331>::load(
            &description,
            &Registry::with_builtins(),
            Environment::default()
        )
        .is_ok()
    );
}

#[test]
fn a_peripheral_keeps_its_marking_and_refuses_unknown_keys() {
    let description = parse(&format!(
        "{BASE}{}",
        r#"
[[peripheral]]
name = "u12"
type = "fixed"
marking = "U12, MC144110P"

[[peripheral]]
name = "u13"
type = "fixed"
"#
    ))
    .unwrap();
    assert_eq!(description.marking("u12"), Some("U12, MC144110P"));
    assert_eq!(description.marking("u13"), None);
    assert_eq!(description.marking("u14"), None);
    let message = error("[[peripheral]]\nname = \"u12\"\ntype = \"fixed\"\npart = \"MC144110P\"\n");
    assert!(message.contains("part"), "{message}");
    assert!(message.contains("line"), "{message}");
}

#[test]
fn every_mistake_names_what_is_wrong() {
    let cases = [
        (
            "[[peripheral]]\nname = \"ram\"\ntype = \"fixed\"\n",
            "\"ram\" is named twice",
        ),
        (
            "[[peripheral]]\nname = \"a.b\"\ntype = \"fixed\"\n",
            "cannot be a name",
        ),
        (
            "[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\nattach = [{ select = \"CS11\" }]\n",
            "d: no chip select \"CS11\"",
        ),
        (
            "[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\nattach = [{ select = \"CS0\" }]\n",
            "CS0: ram and d both take its cycles",
        ),
        (
            "[[memory]]\nname = \"rom\"\nimage = \"missing.bin\"\nattach = [{ select = \"CS1\" }]\n",
            "rom: image /nonexistent/missing.bin",
        ),
        (
            "[[memory]]\nname = \"odd\"\nsize = 0x801\nattach = [{ select = \"CS1\" }]\n",
            "not a whole number",
        ),
        (
            "[[memory]]\nname = \"huge\"\nsize = 0x100000000000\nattach = [{ select = \"CS1\" }]\n",
            "huge: 0x100000000000 bytes is more than ADDR[23:0] reach",
        ),
        (
            "[[memory]]\nname = \"over\"\nsize = 0x1000800\nattach = [{ select = \"CS1\" }]\n",
            "more than ADDR[23:0] reach",
        ),
        (
            "[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\nattach = [{ select = \"CS1\", lines = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 0, 1, 2, 3, 4, 5, 6, 7, 8] }]\n",
            "a peripheral has at most 24 address lines",
        ),
        (
            "[[memory]]\nname = \"rom\"\nsize = 0x800\nattach = [{ select = \"CS1\", role = \"iack\" }]\n",
            "answers no acknowledge",
        ),
        (
            "[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\nirq = 0\n",
            "d: no IRQ pin at level 0",
        ),
        (
            "[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\nattach = [{ decoder = \"u9\", output = 0 }]\n",
            "no decoder \"u9\"",
        ),
        (
            "[[peripheral]]\nname = \"d\"\ntype = \"fixed\"\nattach = [{ select = \"CS1\", qspi = 1 }]\n",
            "give one of",
        ),
        (
            "[chip]\nreference_hz = 32768\nexternal_hz = 16000000\n",
            "not both",
        ),
        (
            // past u64's nanohertz: a fraction of a hertz
            "[chip]\nexternal_hz = 9000000000000000000\n",
            "give whole hertz",
        ),
        (
            "[[net]]\nname = \"n\"\ntype = \"logic\"\nfrom = \"nobody.out\"\nto = [\"chip.PAI\"]\n",
            "no endpoint \"nobody.out\"",
        ),
        (
            "[[net]]\nname = \"n\"\ntype = \"serial\"\nfrom = \"external.x\"\nto = [\"chip.PAI\"]\n",
            "chip.PAI is logic",
        ),
        (
            "[[net]]\nname = \"n\"\ntype = \"logic\"\nfrom = \"chip.PAI\"\nto = [\"external.x\"]\n",
            "chip.PAI cannot drive",
        ),
        (
            "[[net]]\nname = \"n\"\ntype = \"logic\"\nfrom = [\"external.a\", \"external.b\"]\nto = [\"chip.PAI\"]\n",
            "wired-and",
        ),
        (
            "[[net]]\nname = \"a\"\ntype = \"logic\"\nfrom = \"external.a\"\nto = [\"chip.PAI\"]\n[[net]]\nname = \"b\"\ntype = \"logic\"\nfrom = \"external.b\"\nto = [\"chip.PAI\"]\n",
            "already on net a",
        ),
        (
            "[[unconnected]]\nselect = \"CS0\"\n",
            "CS0: unconnected and ram",
        ),
    ];
    for (extra, expected) in cases {
        let message = error(extra);
        assert!(message.contains(expected), "{extra}: {message}");
    }
}

#[test]
fn split_selects_may_be_shared() {
    let shared = r#"
[[peripheral]]
name = "reads"
type = "fixed"
attach = [{ select = "CS4", role = "read" }]

[[peripheral]]
name = "writes"
type = "fixed"
attach = [{ select = "CS4", role = "write", lane = "upper" }]

[[peripheral]]
name = "low-writes"
type = "fixed"
attach = [{ select = "CS4", role = "write", lane = "lower" }]
"#;
    assert!(parse(&format!("{BASE}{shared}")).is_ok());
    let clash = "[[peripheral]]\nname = \"more\"\ntype = \"fixed\"\nattach = [{ select = \"CS4\", lane = \"upper\" }]\n";
    let message = parse(&format!("{BASE}{shared}{clash}"))
        .unwrap_err()
        .to_string();
    assert!(message.contains("CS4: reads and more"), "{message}");
}

#[test]
fn an_image_is_read_at_its_offset_and_padded() {
    let directory = env::temp_dir().join(format!("mc683xx-board-{}", process::id()));
    fs::create_dir_all(&directory).unwrap();
    let image: Vec<u8> = (0..0x1000_u32).map(|byte| byte as u8).collect();
    fs::write(directory.join("image.bin"), &image).unwrap();
    let text = r#"
version = 1

[chip]
type = "mc68331"
[board]
images = ["image.bin"]
[[memory]]
name = "high"
image = "image.bin"
image_offset = 0x800
size = 0x1000
fill = 0xff
attach = [{ select = "CSBOOT" }]
"#;
    let description = Description::parse(text, &directory, &Options::default()).unwrap();
    let bytes = &description.memories[0].bytes;
    assert_eq!(bytes.len(), 0x1000);
    assert_eq!(bytes[0], 0x00);
    assert_eq!(bytes[0x7ff], 0xff, "past the image, filled");
    assert_eq!(&bytes[..0x800], &image[0x800..]);
    let other: Vec<u8> = vec![0x5a; 0x1000];
    fs::write(directory.join("other.bin"), &other).unwrap();
    let options = Options {
        firmware: Some(directory.join("other.bin")),
        ..Options::default()
    };
    let description = Description::parse(text, &directory, &options).unwrap();
    assert_eq!(
        description.memories[0].bytes[0], 0x5a,
        "the chosen firmware"
    );
    fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_reset_scope_names_every_rsr_cause() {
    let fixed = "[[memory]]\nname = \"code\"\nsize = 0x1000\nbase = 0x600000\nreload = [\"loss-of-clock\", \"test\"]\n";
    let description = parse(&format!("{BASE}{fixed}")).unwrap();
    assert_eq!(
        description.memories[1].reload,
        ResetScope::of(&[ResetCause::LossOfClock, ResetCause::Test])
    );
}

#[test]
fn a_fixed_memory_takes_a_base_in_place_of_a_select() {
    let fixed = "[[memory]]\nname = \"code\"\nsize = 0x1000\nbase = 0x600000\nreload = [\"external\", \"system\"]\n";
    let description = parse(&format!("{BASE}{fixed}")).unwrap();
    let code = &description.memories[1];
    assert_eq!(
        (code.base, code.pins.as_slice()),
        (Some(0x60_0000), &[][..])
    );
    assert!(code.reload.contains(ResetCause::System));
    assert_eq!(
        code.timing,
        (PortSize::Bits16, Termination::Fast),
        "fast on 16 bits by default"
    );
    let timed =
        "[[memory]]\nname = \"flash\"\nsize = 0x1000\nbase = 0x600000\nport = 8\nwait_states = 1\n";
    let description = parse(&format!("{BASE}{timed}")).unwrap();
    assert_eq!(
        description.memories[1].timing,
        (PortSize::Bits8, Termination::WaitStates(1))
    );
    assert!(!code.reload.contains(ResetCause::Watchdog));
    assert_eq!(
        description.memories[0].reload,
        ResetScope::NONE,
        "contents survive resets by default"
    );
    let cases = [
        (
            "[[memory]]\nname = \"both\"\nsize = 0x800\nbase = 0x600000\nattach = [{ select = \"CS1\" }]\n",
            "both: attachment 0: a memory takes a select or a base, not both",
        ),
        (
            "[[memory]]\nname = \"neither\"\nsize = 0x800\n",
            "neither: attachment 0: a memory needs a select or a base",
        ),
        (
            "[[memory]]\nname = \"odd\"\nsize = 0x800\nbase = 0x600400\n",
            "odd: base 0x600400 is not on a 2-Kbyte page boundary",
        ),
        (
            "[[memory]]\nname = \"high\"\nsize = 0x1000\nbase = 0xfff800\n",
            "high: base 0xfff800 puts it past ADDR[23:0]",
        ),
        (
            "[[memory]]\nname = \"a\"\nsize = 0x1000\nbase = 0x600000\n[[memory]]\nname = \"b\"\nsize = 0x800\nbase = 0x600800\n",
            "b: overlaps a",
        ),
        (
            "[[memory]]\nname = \"wide\"\nsize = 0x800\nbase = 0x600000\nport = 32\n",
            "wide: port 32 is not 8 or 16",
        ),
        (
            "[[memory]]\nname = \"slow\"\nsize = 0x800\nbase = 0x600000\nwait_states = 14\n",
            "slow: wait_states 14 is more than 13",
        ),
        (
            "[[memory]]\nname = \"selected\"\nsize = 0x800\nattach = [{ select = \"CS1\" }]\nwait_states = 1\n",
            "selected: attachment 0: port and wait_states are a fixed memory's: a select's come from its option register",
        ),
    ];
    for (extra, expected) in cases {
        assert_eq!(error(extra), expected, "{extra}");
    }
}

#[test]
fn a_peripheral_on_no_select_takes_a_window_or_an_acknowledge() {
    let on_bus = "[[peripheral]]\nname = \"io\"\ntype = \"registers\"\ndsack = \"DSACK1\"\nattach = [{ base = 0x800000, size = 0x100, role = \"read\" }, { iack = 4 }]\n";
    let description = parse(&with(on_bus)).unwrap();
    let io = &description.peripherals[0];
    assert_eq!(
        io.attachments[0].target,
        Target::Bus(BusDecode::Window {
            base: Address(0x80_0000),
            length: 0x100
        })
    );
    assert_eq!(io.attachments[0].role, Role::Read);
    assert_eq!(
        io.attachments[1].target,
        Target::Bus(BusDecode::Acknowledge(IrqPin::new(4).unwrap()))
    );
    assert_eq!(io.attachments[1].role, Role::Iack, "an acknowledge's role");
    let peripheral = |dsack: &str, attach: &str| {
        format!("[[peripheral]]\nname = \"io\"\ntype = \"registers\"\n{dsack}attach = [{attach}]\n")
    };
    let dsack = "dsack = \"DSACK1\"\n";
    let cases = [
        (
            peripheral("", "{ base = 0x800000, size = 0x100 }"),
            "io: attachment 0: a peripheral on no select drives DSACK itself: give dsack",
        ),
        (
            peripheral(dsack, "{ base = 0x800000, size = 0x100, select = \"CS1\" }"),
            "io: attachment 0: base and size, or iack, put it on no select: give no select, decoder or qspi",
        ),
        (
            peripheral(dsack, "{ base = 0x800000 }"),
            "io: attachment 0: give base and size, or iack, not both",
        ),
        (
            peripheral(dsack, "{ base = 0xffff00, size = 0x200 }"),
            "io: attachment 0: its window must be inside ADDR[23:0] and not empty",
        ),
        (
            peripheral(dsack, "{ iack = 0 }"),
            "io: attachment 0: iack is a level, 1 to 7",
        ),
        (
            peripheral(dsack, "{ iack = 3, role = \"read\" }"),
            "io: attachment 0: an iack attachment's role is iack",
        ),
        (
            peripheral(dsack, "{ base = 0x800000, size = 0x100, role = \"iack\" }"),
            "io: attachment 0: a window takes no iack role: give iack = <level>",
        ),
    ];
    for (extra, expected) in cases {
        assert_eq!(error(&extra), expected, "{extra}");
    }
}
