//! Page permissions as a Python host writes them (`Memory::protect`):
//! a string of `r` (operand reads), `w` (operand writes) and `x`
//! (instruction fetches), each at most once, in any order and case;
//! `""` permits no cycle.

use mc683xx_cpu32::memory::Permissions;

use crate::error::Error;

/// Each permission's letter.
const LETTERS: [(char, Permissions); 3] = [
    ('r', Permissions::READ),
    ('w', Permissions::WRITE),
    ('x', Permissions::EXECUTE),
];

/// The permissions `text` names.
pub(crate) fn parse(text: &str) -> Result<Permissions, Error> {
    text.chars()
        .try_fold(Permissions::NONE, |permissions, letter| {
            LETTERS
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&letter))
                .map(|&(_, permission)| permission)
                .filter(|&permission| !permissions.contains(permission))
                .map(|permission| permissions.union(permission))
                .ok_or_else(|| Error::Permissions(text.to_owned()))
        })
}

#[cfg(test)]
mod tests {
    use mc683xx_cpu32::memory::Permissions;

    use super::parse;
    use crate::error::Error;

    #[test]
    fn letters_in_any_order_and_case() {
        assert_eq!(parse("").unwrap(), Permissions::NONE);
        assert_eq!(parse("r").unwrap(), Permissions::READ);
        assert_eq!(parse("XwR").unwrap(), Permissions::ALL);
        assert_eq!(
            parse("rx").unwrap(),
            Permissions::READ.union(Permissions::EXECUTE)
        );
    }

    #[test]
    fn others_and_repeats_are_refused() {
        assert!(matches!(parse("rr"), Err(Error::Permissions(text)) if text == "rr"));
        assert!(matches!(parse("rwz"), Err(Error::Permissions(text)) if text == "rwz"));
    }
}
