//! What the DACs share: the span their codes divide.

use mc683xx_board::registry::Context;
use mc683xx_board::registry::ParameterError;
use serde::Deserialize;
use serde::Serialize;

/// A voltage, in volts.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub(crate) struct Volts(pub(crate) f64);

/// What a code of a DAC's resolution gives: from `low` at code 0 up,
/// a step of (`high` - `low`) / 2^resolution a code.  The full-scale
/// code gives `high` less a step: a resistor string's top tap.  Saved
/// with a DAC's snapshot, so that it restores only into a DAC of the
/// same span.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Span {
    /// The output at code 0.
    low: Volts,
    /// The output a code past the last would give.
    high: Volts,
}

impl Span {
    /// From the parameters `low_key` and `high_key`, in volts, each
    /// with its default.
    pub(crate) fn from_parameters(
        context: &Context<'_>,
        (low_key, low): (&str, Volts),
        (high_key, high): (&str, Volts),
    ) -> Result<Self, ParameterError> {
        let volts = |key: &str, default: Volts| -> Result<Volts, ParameterError> {
            let value = context.parameters.float(key)?.unwrap_or(default.0);
            if value.is_finite() {
                Ok(Volts(value))
            } else {
                Err(ParameterError::invalid(
                    key,
                    format!("{value} is not a voltage"),
                ))
            }
        };
        Ok(Self {
            low: volts(low_key, low)?,
            high: volts(high_key, high)?,
        })
    }

    /// The output for `code` of `resolution` bits.
    pub(crate) fn output(self, code: u16, resolution: u32) -> Volts {
        let steps = f64::from(1_u32 << resolution);
        Volts(self.low.0 + (self.high.0 - self.low.0) * f64::from(code) / steps)
    }
}

#[cfg(test)]
mod tests {
    use super::Span;
    use super::Volts;

    #[test]
    fn a_span_steps_from_its_low_end() {
        let span = Span {
            low: Volts(-5.0),
            high: Volts(5.0),
        };
        assert_eq!(span.output(0, 16), Volts(-5.0));
        assert_eq!(span.output(0x8000, 16), Volts(0.0));
        assert_eq!(span.output(0xffff, 16), Volts(5.0 - 10.0 / 65536.0));
    }
}
