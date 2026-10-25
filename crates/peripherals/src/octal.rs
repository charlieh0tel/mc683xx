//! What the octal logic peripherals share, the latch and the buffer: their
//! level parameters.

use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;

/// A peripheral's eight data lines, one a bit of its byte.
pub(crate) const LINES: usize = 8;

/// A level parameter's words for high and low.
pub(crate) type Words = [(&'static str, bool); 2];
/// A pin's level: `"high"` or `"low"`.
pub(crate) const LEVELS: Words = [("high", true), ("low", false)];
/// A pull resistor's: `"up"` (high) or `"down"`.
pub(crate) const PULLS: Words = [("up", true), ("down", false)];

/// The level parameter `key`, one of `words`, or `default` without it.
pub(crate) fn level(
    context: &Context<'_>,
    key: &str,
    words: Words,
    default: bool,
) -> Result<bool, ParameterError> {
    let Some(word) = context.parameters.string(key)? else {
        return Ok(default);
    };
    words
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, level)| *level)
        .ok_or_else(|| {
            ParameterError::invalid(
                key,
                format!("{word:?} is not {:?} or {:?}", words[0].0, words[1].0),
            )
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use mc683xx_board::peripheral::Drive;
    use mc683xx_board::peripheral::PeripheralAccess;
    use mc683xx_board::signal::Signal;
    use mc683xx_board::time::Time;
    use mc683xx_cpu32::function_code::FunctionCode;
    use mc683xx_cpu32::size::Size;
    use mc683xx_modules::chip_select::ChipSelect;

    /// A data cycle of `size` at `now`.
    pub(crate) const fn access(size: Size, now: Time) -> PeripheralAccess {
        PeripheralAccess::new(
            ChipSelect::Boot,
            0,
            size,
            FunctionCode::SUPERVISOR_DATA,
            now,
        )
    }

    /// The levels `drive` drove, by port.
    pub(crate) fn levels(drive: Drive<'_>) -> Vec<(usize, bool)> {
        drive
            .finish()
            .driven
            .into_iter()
            .map(|driven| match driven.signal {
                Signal::Level(level) => (driven.port, level),
                signal => panic!("{signal:?} is not a level"),
            })
            .collect()
    }
}
