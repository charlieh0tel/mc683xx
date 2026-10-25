//! Chip-select pins by the manual's names: `CSBOOT`, `CS0` to `CS10`
//! (SIMRM 7; MC68331UM 4.8).

use mc683xx_modules::chip_select::ChipSelect;

use crate::error::Error;

/// The pin named `name`, in any case.
pub(crate) fn parse(name: &str) -> Result<ChipSelect, Error> {
    name.parse().map_err(|_| Error::Select(name.to_owned()))
}

#[cfg(test)]
mod tests {
    use mc683xx_modules::chip_select::ChipSelect;
    use mc683xx_modules::chip_select::Number;

    use super::parse;
    use crate::error::Error;

    #[test]
    fn names_go_both_ways() {
        for index in 0..ChipSelect::COUNT {
            let select = ChipSelect::from_index(index).unwrap();
            assert_eq!(parse(&select.to_string()).unwrap(), select);
        }
    }

    #[test]
    fn any_case() {
        assert_eq!(parse("csboot").unwrap(), ChipSelect::Boot);
        assert_eq!(
            parse("cs10").unwrap(),
            ChipSelect::Numbered(Number::new(10).unwrap())
        );
    }

    #[test]
    fn eleven_numbered() {
        assert!(matches!(parse("CS11"), Err(Error::Select(text)) if text == "CS11"));
    }
}
