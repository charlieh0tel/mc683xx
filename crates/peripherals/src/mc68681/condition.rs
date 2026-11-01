//! The DUART's stop conditions, for a debugger
//! ([`mc683xx_board::condition`]): a character a channel's transmitter
//! starts sending (MC68681 3.1), or one its receiver assembles (3.2),
//! any or one byte.

use std::mem;

use mc683xx_board::condition::ArmError;
use mc683xx_board::condition::ConditionInfo;
use mc683xx_board::condition::ConditionToken;

/// `tx CHANNEL [BYTE]`.
const TX: ConditionInfo = ConditionInfo {
    name: "tx",
    arguments: "CHANNEL [BYTE]",
    help: "channel a or b starts sending a character, BYTE or any",
};
/// `rx CHANNEL [BYTE]`.
const RX: ConditionInfo = ConditionInfo {
    name: "rx",
    arguments: "CHANNEL [BYTE]",
    help: "channel a or b receives a character, BYTE or any",
};
/// Both.
pub(super) const CONDITIONS: &[ConditionInfo] = &[TX, RX];

/// Which way a character goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    /// Out, through the transmitter.
    Transmit,
    /// In, through the receiver.
    Receive,
}

/// An armed condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Watch {
    /// Its token.
    token: ConditionToken,
    /// Which way.
    direction: Direction,
    /// The channel, 0 for A.
    channel: usize,
    /// The byte, or any.
    byte: Option<u8>,
}

/// The armed conditions and those that held since the DUART last
/// drove: outside its state and its snapshot.
#[derive(Debug, Default)]
pub(super) struct Watches {
    /// The armed conditions.
    armed: Vec<Watch>,
    /// The last token given.
    given: u32,
    /// The tokens of conditions that held, to fire.
    held: Vec<ConditionToken>,
}

impl Watches {
    /// Arms `condition` with `arguments`: `tx` or `rx`, the channel `a`
    /// or `b`, and a byte, hexadecimal with `0x` or decimal, if only
    /// one byte is wanted.
    pub(super) fn arm(
        &mut self,
        condition: &str,
        arguments: &[String],
    ) -> Result<ConditionToken, ArmError> {
        let direction = match condition {
            "tx" => Direction::Transmit,
            "rx" => Direction::Receive,
            _ => return Err(ArmError::Unknown(condition.to_owned())),
        };
        let (channel, byte) = match arguments {
            [channel] => (channel, None),
            [channel, byte] => (channel, Some(byte)),
            _ => {
                return Err(ArmError::Arguments(format!(
                    "`{condition}` takes CHANNEL [BYTE]"
                )));
            }
        };
        let channel = match channel.as_str() {
            "a" => 0,
            "b" => 1,
            other => {
                return Err(ArmError::Arguments(format!("no channel {other:?}: a or b")));
            }
        };
        let byte = byte.map(|byte| parse_byte(byte)).transpose()?;
        self.given += 1;
        let token = ConditionToken(self.given);
        self.armed.push(Watch {
            token,
            direction,
            channel,
            byte,
        });
        Ok(token)
    }

    /// Disarms `token`'s condition.
    pub(super) fn disarm(&mut self, token: ConditionToken) {
        self.armed.retain(|watch| watch.token != token);
    }

    /// Channel `channel`'s characters `bytes` went `direction`: the
    /// conditions they hold are to fire.
    pub(super) fn saw(&mut self, direction: Direction, channel: usize, bytes: &[u8]) {
        for &byte in bytes {
            self.held.extend(
                self.armed
                    .iter()
                    .filter(|watch| {
                        (watch.direction, watch.channel) == (direction, channel)
                            && watch.byte.is_none_or(|wanted| wanted == byte)
                    })
                    .map(|watch| watch.token),
            );
        }
    }

    /// The conditions that held since the last call.
    pub(super) fn take(&mut self) -> Vec<ConditionToken> {
        mem::take(&mut self.held)
    }

    /// Forgets the conditions that held, as a restore does.
    pub(super) fn forget(&mut self) {
        self.held.clear();
    }
}

/// A byte as `monitor stop` gives it: hexadecimal with `0x`, or
/// decimal.
fn parse_byte(text: &str) -> Result<u8, ArmError> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u8::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| ArmError::Arguments(format!("not a byte: {text:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `words` as arguments.
    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    #[test]
    fn bad_arguments_are_refused() {
        let mut watches = Watches::default();
        assert_eq!(
            watches.arm("echo", &arguments(&["a"])),
            Err(ArmError::Unknown("echo".to_owned()))
        );
        for words in [
            &[][..],
            &["c"],
            &["a", "0x100"],
            &["a", "1", "2"],
            &["a", "x"],
        ] {
            assert!(
                matches!(
                    watches.arm("tx", &arguments(words)),
                    Err(ArmError::Arguments(_))
                ),
                "{words:?}"
            );
        }
    }

    #[test]
    fn a_character_holds_the_conditions_it_matches() {
        let mut watches = Watches::default();
        let any = watches.arm("rx", &arguments(&["b"])).unwrap();
        let return_ = watches.arm("rx", &arguments(&["b", "0x0d"])).unwrap();
        let sent = watches.arm("tx", &arguments(&["b", "13"])).unwrap();
        watches.saw(Direction::Receive, 0, b"\r");
        assert!(watches.take().is_empty(), "the other channel");
        watches.saw(Direction::Receive, 1, b"x\r");
        assert_eq!(watches.take(), vec![any, any, return_]);
        watches.disarm(any);
        watches.saw(Direction::Transmit, 1, b"\r");
        assert_eq!(watches.take(), vec![sent]);
    }
}
