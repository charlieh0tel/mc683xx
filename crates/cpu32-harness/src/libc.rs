//! The Z3805A 3543B image's C library routines on known inputs, each
//! result checked against what the C routine of that name gives: a port
//! of smartclock's (github.com/charlieh0tel/smartclock)
//! `tools/firmware/libc_check.py` (by this project's author), with
//! the answers computed here as it computes them in Python.  Only
//! routines that touch nothing but their arguments and RAM.

use crate::firmware::Call;
use crate::firmware::Firmware;
use crate::firmware::RAM;
use crate::firmware::RAM_BYTES;
use crate::firmware::Stopped;

/// The image whose addresses these are.
pub const IMAGE: &str = "z3805a-3543b";
/// `libc_check.py`'s stack top: in RAM.
const STACK_TOP: u32 = RAM + RAM_BYTES - 0x10;
/// A byte comparator for qsort, assembled by hand in `libc_check.py`:
/// `movea.l (4,sp),a0; movea.l (8,sp),a1; moveq #0,d0; move.b (a0),d0;
/// moveq #0,d1; move.b (a1),d1; sub.l d1,d0; rts`.
const COMPARE_BYTES: [u8; 20] = [
    0x20, 0x6f, 0x00, 0x04, 0x22, 0x6f, 0x00, 0x08, 0x70, 0x00, 0x10, 0x10, 0x72, 0x00, 0x12, 0x11,
    0x90, 0x81, 0x4e, 0x75,
];
/// Where the comparator is.
const COMPARE_AT: u32 = RAM + 0x7000;
/// rand's seed.
const RAND_SEED: u32 = 0x10_3936;
/// rand's linear congruential generator (ANSI C's example).
const RAND_MULTIPLIER: u32 = 1_103_515_245;
const RAND_INCREMENT: u32 = 12345;
/// rand's result: 15 bits of the seed's high half.
const RAND_SHIFT: u32 = 16;
const RAND_MASK: u32 = 0x7fff;
/// `math.isclose`'s tolerance in `libc_check.py`.
const RELATIVE_TOLERANCE: f64 = 1e-15;

/// The routines, by name, at their Z3805A 3543B addresses.
const ADDRESSES: &[(&str, u32)] = &[
    ("abs", 0x5EC9E),
    ("labs", 0x5EF02),
    ("atoi", 0x5ECFA),
    ("div", 0x5EEA2),
    ("ldiv", 0x5EED2),
    ("memcmp", 0x5EF12),
    ("memcpy", 0x5EF48),
    ("memset", 0x5EF66),
    ("qsort", 0x5F0CC),
    ("rand", 0x5F102),
    ("strcat", 0x5F128),
    ("strchr", 0x5F140),
    ("strncmp", 0x5F15A),
    ("strncpy", 0x5F190),
    ("toupper", 0x5F1BE),
    ("sprintf", 0x60896),
    ("fabs", 0x6092A),
    ("frexp", 0x6098A),
    ("modf", 0x60D94),
    ("log", 0x60A10),
    ("pow", 0x60F5E),
    ("sqrt", 0x6191C),
    ("integer_to_float", 0x5E168),
    ("unsigned_to_float", 0x5E20E),
    ("float_to_integer", 0x5DFBC),
    ("float_to_unsigned", 0x5E04E),
    ("integer_to_double", 0x5E08E),
    ("unsigned_to_double", 0x5E108),
    ("double_to_integer", 0x5DDDE),
    ("double_to_unsigned", 0x5DE9E),
    ("double_subtract_reversed", 0x5D30E),
    ("float_add_in_place", 0x5D08A),
    ("float_subtract_in_place", 0x5D080),
    ("float_multiply_in_place", 0x5E316),
    ("float_divide_in_place", 0x5D7DA),
    ("double_add_in_place", 0x5D338),
    ("double_add_pointers", 0x5D34A),
    ("double_add_pointer", 0x5D34E),
    ("double_subtract_in_place", 0x5D2E2),
    ("double_subtract_pointer", 0x5D2F8),
    ("double_subtract_reversed_pointers", 0x5D306),
    ("double_subtract_reversed_pointer", 0x5D30A),
    ("double_multiply_in_place", 0x5E5E6),
    ("double_multiply_pointers", 0x5E5F8),
    ("double_multiply_pointer", 0x5E5FC),
    ("double_divide_in_place", 0x5DA3E),
    ("double_divide_reversed_pointers", 0x5DA62),
    ("double_divide_reversed_pointer", 0x5DA66),
    ("double_is_integer", 0x60E68),
    ("double_truncate_sixteenths", 0x60EFA),
    ("next_decimal_digit", 0x5FAD6),
    ("double_to_digits", 0x5F3BA),
];

/// A call's registers: D0, D1, A0, A1, as `libc_check.py` sets them.
#[derive(Debug, Clone, Copy, Default)]
struct Given {
    d0: u32,
    d1: u32,
    a0: u32,
    a1: u32,
}

/// What one check came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The routine gave the C routine's answer.
    Passed,
    /// It returned something else.
    Failed,
    /// A call stopped before it returned.
    Stopped(Stopped),
}

/// One check: the routine, what it was given, and how it came out.
#[derive(Debug, Clone)]
pub struct Check {
    /// The routine.
    pub routine: &'static str,
    /// Its inputs, for the report.
    pub description: String,
    /// The outcome.
    pub outcome: Outcome,
}

/// The image's routines on the core, calls sharing RAM as in
/// `libc_check.py`.
struct Libc {
    /// The image.
    firmware: Firmware,
}

impl Libc {
    /// Calls `name` with `longs` on the stack, first argument lowest,
    /// and `given` in the registers: D0 and D1 after, or how it stopped.
    fn call(&mut self, name: &str, longs: &[u32], given: Given) -> Result<(u32, u32), Stopped> {
        let entry = ADDRESSES
            .iter()
            .find(|(routine, _)| *routine == name)
            .map(|&(_, address)| address)
            .expect("a routine of ADDRESSES");
        let arguments: Vec<u8> = longs.iter().flat_map(|long| long.to_be_bytes()).collect();
        let sp = self.firmware.push_call(STACK_TOP, &arguments);
        let call: Call = self.firmware.call(entry, sp, |registers| {
            registers.data[0] = given.d0;
            registers.data[1] = given.d1;
            registers.set_address(0, given.a0);
            registers.set_address(1, given.a1);
        });
        match call.stopped {
            Stopped::Returned => Ok((call.d0(), call.d1())),
            stopped => Err(stopped),
        }
    }

    /// `call` with arguments on the stack alone.
    fn stack(&mut self, name: &str, longs: &[u32]) -> Result<(u32, u32), Stopped> {
        self.call(name, longs, Given::default())
    }

    fn buffer(&mut self, contents: &[u8]) -> u32 {
        self.firmware.buffer(contents)
    }

    fn double_buffer(&mut self, value: f64) -> u32 {
        self.buffer(&value.to_be_bytes())
    }

    fn read_double(&self, at: u32) -> f64 {
        f64::from_be_bytes(self.firmware.read(at, 8).try_into().expect("8 bytes"))
    }

    fn read_long(&self, at: u32) -> u32 {
        self.firmware.long(at)
    }
}

/// A check's body: whether the routine answered right, or how a call
/// stopped.
type Body = Box<dyn Fn(&mut Libc) -> Result<bool, Stopped>>;

/// A double's two longs, high first.
fn double_words(value: f64) -> [u32; 2] {
    let bits = value.to_bits();
    [(bits >> 32) as u32, bits as u32]
}

fn from_double_words(high: u32, low: u32) -> f64 {
    f64::from_bits(u64::from(high) << 32 | u64::from(low))
}

fn float_bits(value: f32) -> u32 {
    value.to_bits()
}

fn from_float_bits(bits: u32) -> f32 {
    f32::from_bits(bits)
}

/// The sign of `value`: -1, 0 or 1.
fn sign(value: i32) -> i32 {
    value.signum()
}

/// `math.isclose` with `RELATIVE_TOLERANCE`.
fn is_close(a: f64, b: f64) -> bool {
    (a - b).abs() <= RELATIVE_TOLERANCE * a.abs().max(b.abs())
}

/// `math.frexp` of a normal number: a mantissa in [0.5, 1) and the
/// power of two.
fn frexp(value: f64) -> (f64, i32) {
    /// A double's exponent field and bias for a mantissa in [0.5, 1).
    const EXPONENT_SHIFT: u32 = 52;
    const EXPONENT_MASK: u64 = 0x7ff;
    const HALF_BIAS: i32 = 1022;
    if value == 0.0 {
        return (value, 0);
    }
    let exponent = (value.to_bits() >> EXPONENT_SHIFT & EXPONENT_MASK) as i32 - HALF_BIAS;
    (value / 2f64.powi(exponent), exponent)
}

/// The checks of `libc_check.py`'s `checks` and `entry_checks`.
#[expect(
    clippy::too_many_lines,
    reason = "one table of checks, as in libc_check.py"
)]
#[expect(
    clippy::approx_constant,
    reason = "libc_check.py's inputs, not pi and e"
)]
fn checks() -> Vec<(&'static str, String, Body)> {
    let mut checks: Vec<(&'static str, String, Body)> = Vec::new();
    let mut add = |routine: &'static str, description: String, body: Body| {
        checks.push((routine, description, body));
    };

    for name in ["abs", "labs"] {
        for value in [0, 5, -5, -0x7fff_ffff_i32] {
            add(
                name,
                format!("{value}"),
                Box::new(move |m| Ok(m.stack(name, &[value as u32])?.0 as i32 == value.abs())),
            );
        }
    }

    for (text, value) in [
        (&b"0"[..], 0),
        (b"42", 42),
        (b"  -17x", -17),
        (b"+8", 8),
        (b"x9", 0),
    ] {
        add(
            "atoi",
            format!("{:?}", String::from_utf8_lossy(text)),
            Box::new(move |m| {
                let at = m.buffer(&[text, b"\0"].concat());
                Ok(m.stack("atoi", &[at])?.0 as i32 == value)
            }),
        );
    }

    for name in ["div", "ldiv"] {
        for (a, b) in [(17_i32, 5_i32), (-17, 5), (17, -5)] {
            add(
                name,
                format!("{a}/{b}"),
                Box::new(move |m| {
                    let (quotient, remainder) = m.stack(name, &[a as u32, b as u32])?;
                    // C truncates toward zero, as Rust does.
                    Ok((quotient as i32, remainder as i32) == (a / b, a % b))
                }),
            );
        }
    }

    for (a, b, n) in [
        (&b"abc"[..], &b"abd"[..], 3_usize),
        (b"abc", b"abd", 2),
        (b"abd", b"abc", 3),
    ] {
        add(
            "memcmp",
            format!("{a:?}/{b:?}/{n}"),
            Box::new(move |m| {
                let wanted = a[..n].cmp(&b[..n]) as i32;
                let (first, second) = (m.buffer(a), m.buffer(b));
                Ok(sign(m.stack("memcmp", &[first, second, n as u32])?.0 as i32) == wanted)
            }),
        );
    }

    add(
        "memcpy",
        "5 bytes".to_owned(),
        Box::new(|m| {
            let destination = m.buffer(b"");
            let source = m.buffer(b"hello world");
            let got = m.stack("memcpy", &[destination, source, 5])?.0;
            Ok(got == destination && m.firmware.read(destination, 6) == b"hello\xa5")
        }),
    );

    add(
        "memset",
        "4 bytes".to_owned(),
        Box::new(|m| {
            let destination = m.buffer(b"");
            let got = m.stack("memset", &[destination, 0x41, 4])?.0;
            Ok(got == destination && m.firmware.read(destination, 5) == b"AAAA\xa5")
        }),
    );

    add(
        "qsort",
        "bytes".to_owned(),
        Box::new(|m| {
            let values = b"qsortbyte";
            let base = m.buffer(values);
            m.firmware.write(COMPARE_AT, &COMPARE_BYTES);
            m.stack("qsort", &[base, values.len() as u32, 1, COMPARE_AT])?;
            let mut sorted = values.to_vec();
            sorted.sort_unstable();
            Ok(m.firmware.read(base, values.len()) == sorted)
        }),
    );

    // Three calls in turn, from a seed of 1: each the generator's next.
    for call in 1..=3 {
        add(
            "rand",
            "LCG 1103515245, 12345".to_owned(),
            Box::new(move |m| {
                if call == 1 {
                    m.firmware.write(RAND_SEED, &1_u32.to_be_bytes());
                }
                let seed = (0..call).fold(1_u32, |seed, _| {
                    seed.wrapping_mul(RAND_MULTIPLIER)
                        .wrapping_add(RAND_INCREMENT)
                });
                Ok(m.stack("rand", &[])?.0 == seed >> RAND_SHIFT & RAND_MASK)
            }),
        );
    }

    add(
        "strcat",
        "ab+cd".to_owned(),
        Box::new(|m| {
            let destination = m.buffer(b"ab\0");
            let source = m.buffer(b"cd\0");
            let got = m.stack("strcat", &[destination, source])?.0;
            Ok(got == destination && m.firmware.string(destination) == b"abcd")
        }),
    );

    for (character, offset) in [(b'l', Some(2)), (b'z', None), (0, Some(5))] {
        add(
            "strchr",
            if character == 0 {
                "NUL".to_owned()
            } else {
                char::from(character).to_string()
            },
            Box::new(move |m| {
                let haystack = m.buffer(b"hello\0");
                let got = m.stack("strchr", &[haystack, u32::from(character)])?.0;
                Ok(got == offset.map_or(0, |offset| haystack + offset))
            }),
        );
    }

    for (a, b, n, wanted) in [
        (&b"abc"[..], &b"abd"[..], 3, -1),
        (b"abc", b"abd", 2, 0),
        (b"ab", b"ab", 9, 0),
        (b"b", b"a", 1, 1),
    ] {
        add(
            "strncmp",
            format!("{a:?}/{b:?}/{n}"),
            Box::new(move |m| {
                let first = m.buffer(&[a, b"\0"].concat());
                let second = m.buffer(&[b, b"\0"].concat());
                Ok(sign(m.stack("strncmp", &[first, second, n])?.0 as i32) == wanted)
            }),
        );
    }

    add(
        "strncpy",
        "pads".to_owned(),
        Box::new(|m| {
            let destination = m.buffer(b"");
            let source = m.buffer(b"ab\0");
            let got = m.stack("strncpy", &[destination, source, 4])?.0;
            Ok(got == destination && m.firmware.read(destination, 5) == b"ab\0\0\xa5")
        }),
    );
    add(
        "strncpy",
        "truncates".to_owned(),
        Box::new(|m| {
            let destination = m.buffer(b"");
            let source = m.buffer(b"abcdef\0");
            m.stack("strncpy", &[destination, source, 3])?;
            Ok(m.firmware.read(destination, 4) == b"abc\xa5")
        }),
    );

    for character in *b"aZz0{`" {
        add(
            "toupper",
            char::from(character).to_string(),
            Box::new(move |m| {
                Ok(m.stack("toupper", &[u32::from(character)])?.0
                    == u32::from(character.to_ascii_uppercase()))
            }),
        );
    }

    add(
        "sprintf",
        "%d %5s %x %c".to_owned(),
        Box::new(|m| {
            let destination = m.buffer(b"");
            let format = m.buffer(b"%d|%5s|%x|%c\0");
            let text = m.buffer(b"ab\0");
            m.stack(
                "sprintf",
                &[destination, format, 42, text, 255, u32::from(b'Q')],
            )?;
            Ok(m.firmware.string(destination) == b"42|   ab|ff|Q")
        }),
    );

    for value in [2.5_f64, -2.5, 0.0] {
        add(
            "fabs",
            format!("{value}"),
            Box::new(move |m| {
                let (high, low) = m.stack("fabs", &double_words(value))?;
                Ok(from_double_words(high, low) == value.abs())
            }),
        );
    }

    for value in [8.0_f64, 0.75, -3.0] {
        add(
            "frexp",
            format!("{value}"),
            Box::new(move |m| {
                let exponent = m.buffer(b"");
                let [high, low] = double_words(value);
                let (high, low) = m.stack("frexp", &[high, low, exponent])?;
                let got = (from_double_words(high, low), m.read_long(exponent) as i32);
                Ok(got == frexp(value))
            }),
        );
    }

    for value in [3.25_f64, -2.5] {
        add(
            "modf",
            format!("{value}"),
            Box::new(move |m| {
                let whole = m.buffer(b"");
                let [high, low] = double_words(value);
                let (high, low) = m.stack("modf", &[high, low, whole])?;
                Ok(from_double_words(high, low) == value.fract()
                    && m.read_double(whole) == value.trunc())
            }),
        );
    }

    for value in [4.0_f64, 2.0] {
        add(
            "sqrt",
            format!("{value}"),
            Box::new(move |m| {
                let (high, low) = m.stack("sqrt", &double_words(value))?;
                Ok(is_close(from_double_words(high, low), value.sqrt()))
            }),
        );
    }
    for value in [1.0_f64, std::f64::consts::E, 10.0] {
        add(
            "log",
            format!("{value}"),
            Box::new(move |m| {
                let (high, low) = m.stack("log", &double_words(value))?;
                Ok(is_close(from_double_words(high, low), value.ln()))
            }),
        );
    }
    for (x, y) in [(2.0_f64, 10.0_f64), (9.0, 0.5)] {
        add(
            "pow",
            format!("{x}^{y}"),
            Box::new(move |m| {
                let arguments = [double_words(x), double_words(y)].concat();
                let (high, low) = m.stack("pow", &arguments)?;
                Ok(is_close(from_double_words(high, low), x.powf(y)))
            }),
        );
    }

    let in_d0 = |d0: u32| Given {
        d0,
        ..Given::default()
    };
    for value in [0_i32, 7, -7] {
        add(
            "integer_to_float",
            format!("{value}"),
            Box::new(move |m| {
                let (result, _) = m.call("integer_to_float", &[], in_d0(value as u32))?;
                Ok(from_float_bits(result) == value as f32)
            }),
        );
    }
    for value in [7_u32, 0xf000_0000] {
        add(
            "unsigned_to_float",
            format!("{value:#x}"),
            Box::new(move |m| {
                let (result, _) = m.call("unsigned_to_float", &[], in_d0(value))?;
                Ok(from_float_bits(result) == value as f32)
            }),
        );
    }
    for value in [7.9_f32, -7.9] {
        add(
            "float_to_integer",
            format!("{value}"),
            Box::new(move |m| {
                let (result, _) = m.call("float_to_integer", &[], in_d0(float_bits(value)))?;
                Ok(result as i32 == value as i32)
            }),
        );
    }
    add(
        "float_to_unsigned",
        "3e9".to_owned(),
        Box::new(move |m| {
            let value = 3e9_f32;
            let (result, _) = m.call("float_to_unsigned", &[], in_d0(float_bits(value)))?;
            Ok(result == value as u32)
        }),
    );
    for value in [0_i32, 7, -7] {
        add(
            "integer_to_double",
            format!("{value}"),
            Box::new(move |m| {
                let (high, low) = m.call("integer_to_double", &[], in_d0(value as u32))?;
                Ok(from_double_words(high, low) == f64::from(value))
            }),
        );
    }
    add(
        "unsigned_to_double",
        "0xf0000000".to_owned(),
        Box::new(move |m| {
            let value = 0xf000_0000_u32;
            let (high, low) = m.call("unsigned_to_double", &[], in_d0(value))?;
            Ok(from_double_words(high, low) == f64::from(value))
        }),
    );
    let in_d0_d1 = |value: f64| {
        let [d0, d1] = double_words(value);
        Given {
            d0,
            d1,
            ..Given::default()
        }
    };
    for value in [7.9_f64, -7.9] {
        add(
            "double_to_integer",
            format!("{value}"),
            Box::new(move |m| {
                let (result, _) = m.call("double_to_integer", &[], in_d0_d1(value))?;
                Ok(result as i32 == value as i32)
            }),
        );
    }
    add(
        "double_to_unsigned",
        "3e9".to_owned(),
        Box::new(move |m| {
            let (result, _) = m.call("double_to_unsigned", &[], in_d0_d1(3e9))?;
            Ok(result == 3_000_000_000)
        }),
    );

    for (value, integer, odd) in [
        (3.0_f64, 1, 1),
        (4.0, 1, 0),
        (2.5, 0, 0),
        (-5.0, 1, 1),
        (1e30, 1, 0),
    ] {
        add(
            "double_is_integer",
            format!("{value}"),
            Box::new(move |m| {
                let flag = m.buffer(&[0; 4]);
                let [high, low] = double_words(value);
                let (got, _) = m.stack("double_is_integer", &[high, low, flag])?;
                Ok((got, m.read_long(flag)) == (integer, odd))
            }),
        );
    }
    for value in [3.14159_f64, -2.71828, 0.1, 100.03, 0.03] {
        add(
            "double_truncate_sixteenths",
            format!("{value}"),
            Box::new(move |m| {
                let (high, low) = m.stack("double_truncate_sixteenths", &double_words(value))?;
                Ok(from_double_words(high, low) == (value * 16.0).trunc() / 16.0)
            }),
        );
    }

    for value in [1234_u32, 9, 0, 1_000_000_007] {
        add(
            "next_decimal_digit",
            format!("{value}"),
            Box::new(move |m| {
                let at = m.buffer(&value.to_be_bytes());
                let (digit, _) = m.stack("next_decimal_digit", &[at])?;
                Ok((digit, m.read_long(at)) == (u32::from(b'0') + value % 10, value / 10))
            }),
        );
    }
    for (value, digits, wanted, wanted_exponent) in
        [(123.456_f64, 3, &b"123"[..], 3), (0.00125, 4, b"13", -2)]
    {
        add(
            "double_to_digits",
            format!("{value}"),
            Box::new(move |m| {
                let exponent = m.buffer(&[0; 4]);
                let sign = m.buffer(&[0; 4]);
                let out = m.buffer(b"");
                let [high, low] = double_words(value);
                let significant = u32::from(value > 1.0);
                m.stack(
                    "double_to_digits",
                    &[high, low, digits, exponent, sign, out, significant],
                )?;
                Ok(m.firmware.string(out) == wanted
                    && m.read_long(exponent) as i32 == wanted_exponent)
            }),
        );
    }

    for (a, d) in [(3.0_f64, 1.0_f64), (1.0, 3.0), (-2.5, 0.5)] {
        add(
            "double_subtract_reversed",
            format!("{a}-{d}"),
            Box::new(move |m| {
                let [d0, d1] = double_words(d);
                let [a0, a1] = double_words(a);
                let given = Given { d0, d1, a0, a1 };
                let (high, low) = m.call("double_subtract_reversed", &[], given)?;
                Ok(from_double_words(high, low) == a - d)
            }),
        );
    }

    entry_checks(&mut add);
    checks
}

/// An operation on two doubles.
type Operation = fn(f64, f64) -> f64;

/// The in-place and pointer entries of the float and double routines
/// (`libc_check.py`'s `entry_checks`).
fn entry_checks(add: &mut impl FnMut(&'static str, String, Body)) {
    let (x, y) = (6.0_f64, 1.5_f64);
    let float_operations: [(&str, Operation); 4] = [
        ("float_add_in_place", |a, b| a + b),
        ("float_subtract_in_place", |a, b| a - b),
        ("float_multiply_in_place", |a, b| a * b),
        ("float_divide_in_place", |a, b| a / b),
    ];
    for (name, operation) in float_operations {
        add(
            name,
            format!("*x={x}, y={y}"),
            Box::new(move |m| {
                let at = m.buffer(&(x as f32).to_be_bytes());
                let given = Given {
                    d1: float_bits(y as f32),
                    a0: at,
                    ..Given::default()
                };
                m.call(name, &[], given)?;
                let got = f32::from_be_bytes(m.firmware.read(at, 4).try_into().expect("4 bytes"));
                Ok(f64::from(got) == operation(x, y))
            }),
        );
    }

    let [y_high, y_low] = double_words(y);
    let in_place: [(&str, Operation); 4] = [
        ("double_add_in_place", |a, b| a + b),
        ("double_subtract_in_place", |a, b| a - b),
        ("double_multiply_in_place", |a, b| a * b),
        ("double_divide_in_place", |a, b| a / b),
    ];
    for (name, operation) in in_place {
        add(
            name,
            format!("*x={x}, y={y}"),
            Box::new(move |m| {
                let at = m.double_buffer(x);
                let given = Given {
                    d0: y_high,
                    d1: y_low,
                    a0: at,
                    ..Given::default()
                };
                m.call(name, &[], given)?;
                Ok(m.read_double(at) == operation(x, y))
            }),
        );
    }

    // Both operands by pointer, a in A0 and b in A1.
    let pointers: [(&str, Operation); 4] = [
        ("double_add_pointers", |a, b| a + b),
        ("double_multiply_pointers", |a, b| a * b),
        ("double_subtract_reversed_pointers", |a, b| b - a),
        ("double_divide_reversed_pointers", |a, b| b / a),
    ];
    for (name, operation) in pointers {
        add(
            name,
            format!("*a={x}, *b={y}"),
            Box::new(move |m| {
                let given = Given {
                    a0: m.double_buffer(x),
                    a1: m.double_buffer(y),
                    ..Given::default()
                };
                let (high, low) = m.call(name, &[], given)?;
                Ok(from_double_words(high, low) == operation(x, y))
            }),
        );
    }

    // a in D0:D1, b by pointer in A1.
    let [x_high, x_low] = double_words(x);
    let pointer: [(&str, Operation); 5] = [
        ("double_add_pointer", |a, b| a + b),
        ("double_multiply_pointer", |a, b| a * b),
        ("double_subtract_pointer", |a, b| a - b),
        ("double_subtract_reversed_pointer", |a, b| b - a),
        ("double_divide_reversed_pointer", |a, b| b / a),
    ];
    for (name, operation) in pointer {
        add(
            name,
            format!("a={x}, *b={y}"),
            Box::new(move |m| {
                let given = Given {
                    d0: x_high,
                    d1: x_low,
                    a1: m.double_buffer(y),
                    ..Given::default()
                };
                let (high, low) = m.call(name, &[], given)?;
                Ok(from_double_words(high, low) == operation(x, y))
            }),
        );
    }
}

/// Runs every check on `firmware`, the Z3805A 3543B image, in order,
/// sharing its RAM as `libc_check.py` does.
#[must_use]
pub fn run(firmware: Firmware) -> Vec<Check> {
    let mut libc = Libc { firmware };
    checks()
        .into_iter()
        .map(|(routine, description, body)| Check {
            routine,
            description,
            outcome: match body(&mut libc) {
                Ok(true) => Outcome::Passed,
                Ok(false) => Outcome::Failed,
                Err(stopped) => Outcome::Stopped(stopped),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::checks;
    use super::frexp;

    #[test]
    fn frexp_is_pythons() {
        assert_eq!(frexp(8.0), (0.5, 4));
        assert_eq!(frexp(0.75), (0.75, 0));
        assert_eq!(frexp(-3.0), (-0.75, 2));
    }

    #[test]
    fn every_check_of_libc_check_is_here() {
        // libc_check.py's 94 checks and 17 entry checks.
        assert_eq!(checks().len(), 94 + 17);
    }
}
