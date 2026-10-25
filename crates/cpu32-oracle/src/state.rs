//! The registers an instruction test sets and compares.

/// The registers a test case gives and checks: those the 68000 tests
/// and the oracles share.  A7 is `usp` or `ssp`, as SR's S bit says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    /// D0-D7.
    pub data: [u32; 8],
    /// A0-A6.
    pub address: [u32; 7],
    /// The user stack pointer.
    pub usp: u32,
    /// The supervisor stack pointer.
    pub ssp: u32,
    /// The status register.
    pub sr: u16,
    /// The program counter.
    pub pc: u32,
}

impl State {
    /// Each register that differs between `self`, expected, and
    /// `actual`, as a line for a report.
    #[must_use]
    pub fn differences(&self, actual: &Self) -> Vec<String> {
        let named = |state: &Self| {
            let data = (0..8).map(|n| (format!("D{n}"), state.data[n]));
            let address = (0..7).map(|n| (format!("A{n}"), state.address[n]));
            let rest = [
                ("USP".to_owned(), state.usp),
                ("SSP".to_owned(), state.ssp),
                ("SR".to_owned(), u32::from(state.sr)),
                ("PC".to_owned(), state.pc),
            ];
            data.chain(address).chain(rest).collect::<Vec<_>>()
        };
        named(self)
            .into_iter()
            .zip(named(actual))
            .filter(|((_, expected), (_, actual))| expected != actual)
            .map(|((name, expected), (_, actual))| {
                format!("{name}: expected {expected:#010x}, got {actual:#010x}")
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::State;

    #[test]
    fn differences_name_the_register() {
        let expected = State::default();
        let actual = State {
            address: [0, 0, 0, 5, 0, 0, 0],
            ..expected
        };
        assert_eq!(
            expected.differences(&actual),
            ["A3: expected 0x00000000, got 0x00000005"]
        );
        assert!(expected.differences(&expected).is_empty());
    }
}
