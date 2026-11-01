//! Why the CPU entered background mode.

use core::fmt;

/// ATEMP\[15:0\] after a double bus fault (CPU32RM Table 7-2).
const DOUBLE_BUS_FAULT: u16 = 0xFFFF;
/// ATEMP after a BGND instruction.
const BACKGROUND: u32 = 0x0000_0001;
/// ATEMP after a hardware breakpoint.
const BREAKPOINT: u32 = 0x0000_0000;
/// ATEMP\[31:16\]: the SSW after a double bus fault.
const SSW_SHIFT: u32 = 16;

/// The source of a transition into background mode, as ATEMP reports
/// it on entry (CPU32RM 7.2.3, Table 7-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entry {
    /// BKPT asserted: an external or peripheral breakpoint, or BKPT
    /// held low past reset (CPU32RM 7.2.1, 7.2.2.1).
    Breakpoint,
    /// A BGND instruction (CPU32RM 7.2.2.2).
    Background,
    /// A double bus fault (CPU32RM 7.2.2.3).
    DoubleBusFault {
        /// The special status word of the second fault (CPU32RM 6.3).
        ssw: u16,
    },
    /// An ATEMP Table 7-2 does not list: the target is halted, but its
    /// entry source is unknown (a misread, or ATEMP read too late).
    Unknown {
        /// ATEMP as read.
        atemp: u32,
    },
}

impl Entry {
    /// The entry ATEMP reports, [`Entry::Unknown`] for a value Table
    /// 7-2 does not list.
    #[must_use]
    pub const fn from_atemp(atemp: u32) -> Self {
        match atemp {
            BREAKPOINT => Self::Breakpoint,
            BACKGROUND => Self::Background,
            _ if atemp as u16 == DOUBLE_BUS_FAULT => Self::DoubleBusFault {
                ssw: (atemp >> SSW_SHIFT) as u16,
            },
            _ => Self::Unknown { atemp },
        }
    }

    /// ATEMP as the CPU writes it on this entry, or as read for
    /// [`Entry::Unknown`].
    #[must_use]
    pub const fn atemp(self) -> u32 {
        match self {
            Self::Breakpoint => BREAKPOINT,
            Self::Background => BACKGROUND,
            Self::DoubleBusFault { ssw } => (ssw as u32) << SSW_SHIFT | DOUBLE_BUS_FAULT as u32,
            Self::Unknown { atemp } => atemp,
        }
    }
}

/// `breakpoint`, `background`, `double bus fault` and the SSW, or
/// `unknown entry` and ATEMP.
impl fmt::Display for Entry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Breakpoint => formatter.write_str("breakpoint"),
            Self::Background => formatter.write_str("background"),
            Self::DoubleBusFault { ssw } => write!(formatter, "double bus fault, SSW {ssw:#06x}"),
            Self::Unknown { atemp } => write!(formatter, "unknown entry, ATEMP {atemp:#010x}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::Entry;

    #[test]
    fn atemp_values_are_table_7_2s() {
        assert_eq!(Entry::Breakpoint.atemp(), 0x0000_0000);
        assert_eq!(Entry::Background.atemp(), 0x0000_0001);
        assert_eq!(Entry::DoubleBusFault { ssw: 0x1234 }.atemp(), 0x1234_FFFF);
    }

    #[test]
    fn atemp_round_trips() {
        for entry in [
            Entry::Breakpoint,
            Entry::Background,
            Entry::DoubleBusFault { ssw: 0 },
            Entry::DoubleBusFault { ssw: 0xFFFF },
        ] {
            assert_eq!(Entry::from_atemp(entry.atemp()), entry);
        }
        for atemp in [2, 0x0001_0000, 0x2580_0000] {
            assert_eq!(Entry::from_atemp(atemp), Entry::Unknown { atemp });
            assert_eq!(Entry::from_atemp(atemp).atemp(), atemp);
        }
    }

    #[test]
    fn entries_show_their_source() {
        assert_eq!(
            Entry::DoubleBusFault { ssw: 0x45 }.to_string(),
            "double bus fault, SSW 0x0045"
        );
        assert_eq!(
            Entry::Unknown { atemp: 2 }.to_string(),
            "unknown entry, ATEMP 0x00000002"
        );
    }
}
