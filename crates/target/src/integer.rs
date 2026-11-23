//! The one number syntax a person types to the workspace's commands:
//! init scripts, gdb `monitor` commands, the runner's options, board
//! file strings.  Not for gdb packets or S-records, which have their
//! own.

/// A prefix and the radix it selects.
const PREFIXES: [(&str, u32); 2] = [("0x", 16), ("0b", 2)];

/// The separator allowed between two digits.
const SEPARATOR: u8 = b'_';

/// `text` as an integer of type `T`: decimal, hexadecimal after `0x`
/// or binary after `0b`, the prefix in lower case and the hexadecimal
/// digits in either.  An `_` may stand between two digits.  There is
/// no sign, and a decimal number has no leading zero but `0` itself.
///
/// ```
/// use mc683xx_target::integer::ParseIntegerError;
/// use mc683xx_target::integer::parse_integer;
///
/// assert_eq!(parse_integer::<u32>("0xfff_a44"), Ok(0xfffa44));
/// assert_eq!(parse_integer::<u8>("0b101"), Ok(5));
/// assert_eq!(parse_integer::<u8>("256"), Err(ParseIntegerError::TooLarge));
/// assert_eq!(parse_integer::<u32>("$ff"), Err(ParseIntegerError::Invalid));
/// ```
///
/// # Errors
/// [`ParseIntegerError::Invalid`] for text that is not a number in
/// this syntax, [`ParseIntegerError::TooLarge`] for one past `T`.
pub fn parse_integer<T: TryFrom<u32>>(text: &str) -> Result<T, ParseIntegerError> {
    let (digits, radix) = PREFIXES
        .into_iter()
        .find_map(|(prefix, radix)| text.strip_prefix(prefix).map(|digits| (digits, radix)))
        .unwrap_or((text, 10));
    let digits = digits.as_bytes();
    // An empty group: no digits, or a separator not between two.
    let misplaced = digits
        .split(|&byte| byte == SEPARATOR)
        .any(<[u8]>::is_empty);
    let leading_zero = radix == 10 && digits.len() > 1 && digits[0] == b'0';
    if misplaced || leading_zero {
        return Err(ParseIntegerError::Invalid);
    }
    let value =
        digits
            .iter()
            .filter(|&&byte| byte != SEPARATOR)
            .try_fold(0_u32, |value, &byte| {
                let digit = char::from(byte)
                    .to_digit(radix)
                    .ok_or(ParseIntegerError::Invalid)?;
                value
                    .checked_mul(radix)
                    .and_then(|value| value.checked_add(digit))
                    .ok_or(ParseIntegerError::TooLarge)
            })?;
    T::try_from(value).map_err(|_| ParseIntegerError::TooLarge)
}

/// Why text is not an integer [`parse_integer`] takes; without the
/// text, as [`ParseRegisterError`](crate::register::ParseRegisterError).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum ParseIntegerError {
    /// Not a number in the syntax.
    #[error("not a number: decimal, 0x hexadecimal or 0b binary")]
    Invalid,
    /// A number too large for its type.
    #[error("number too large")]
    TooLarge,
}

#[cfg(test)]
mod tests {
    use super::ParseIntegerError;
    use super::parse_integer;

    #[test]
    fn three_radixes() {
        assert_eq!(parse_integer::<u32>("0"), Ok(0));
        assert_eq!(parse_integer::<u32>("1234"), Ok(1234));
        assert_eq!(parse_integer::<u32>("0x00fF"), Ok(0xff));
        assert_eq!(parse_integer::<u32>("0b0110"), Ok(6));
        assert_eq!(parse_integer::<u32>("0x0"), Ok(0));
        assert_eq!(parse_integer::<u32>("4294967295"), Ok(u32::MAX));
        assert_eq!(parse_integer::<u32>("0xffffffff"), Ok(u32::MAX));
    }

    #[test]
    fn separators_stand_between_digits() {
        assert_eq!(parse_integer::<u32>("1_000_000"), Ok(1_000_000));
        assert_eq!(parse_integer::<u32>("0xff_fa44"), Ok(0xfffa44));
        assert_eq!(parse_integer::<u32>("0b1_0"), Ok(2));
        for text in ["_1", "1_", "1__0", "0x_1", "0b_1", "_"] {
            assert_eq!(
                parse_integer::<u32>(text),
                Err(ParseIntegerError::Invalid),
                "{text}"
            );
        }
    }

    #[test]
    fn other_syntax_is_refused() {
        for text in [
            "", "0x", "0b", "05", "00", "0_1", "+5", "-1", "$ff", "0X5", "0B1", "0o7", "5 ", " 5",
            "0x5g", "0b2", "1e3", "ff",
        ] {
            assert_eq!(
                parse_integer::<u32>(text),
                Err(ParseIntegerError::Invalid),
                "{text}"
            );
        }
    }

    #[test]
    fn too_large_for_the_type() {
        assert_eq!(parse_integer::<u8>("255"), Ok(255));
        assert_eq!(parse_integer::<u8>("256"), Err(ParseIntegerError::TooLarge));
        assert_eq!(
            parse_integer::<u32>("4294967296"),
            Err(ParseIntegerError::TooLarge)
        );
        assert_eq!(
            parse_integer::<u32>("0x1_0000_0000"),
            Err(ParseIntegerError::TooLarge)
        );
        assert_eq!(
            parse_integer::<u16>("0x10000"),
            Err(ParseIntegerError::TooLarge)
        );
    }
}
