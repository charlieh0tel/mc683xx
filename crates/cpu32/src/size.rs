//! Operand sizes.

/// The size of an operand or a bus cycle (CPU32RM 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Size {
    /// 8 bits.
    Byte,
    /// 16 bits.
    Word,
    /// 32 bits.
    Long,
}

impl Size {
    /// The size from the common two-bit size field: 00 byte, 01 word,
    /// 10 long; 11 is no size (CPU32RM section 4, as in ADD's Size
    /// field).
    #[must_use]
    pub const fn from_field(bits: u16) -> Option<Self> {
        match bits & 0b11 {
            0b00 => Some(Self::Byte),
            0b01 => Some(Self::Word),
            0b10 => Some(Self::Long),
            _ => None,
        }
    }

    /// The size as a number, for a handler specialized by size with a
    /// const generic: 0 byte, 1 word, 2 long.
    #[must_use]
    pub(crate) const fn index(self) -> u8 {
        match self {
            Self::Byte => 0,
            Self::Word => 1,
            Self::Long => 2,
        }
    }

    /// The size of `index`, as `index` gives it; anything else is long.
    #[must_use]
    pub(crate) const fn of_index(index: u8) -> Self {
        match index {
            0 => Self::Byte,
            1 => Self::Word,
            _ => Self::Long,
        }
    }

    /// Bytes in an operand of this size.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Long => 4,
        }
    }

    /// The bits of a value this size holds.
    #[must_use]
    pub const fn mask(self) -> u32 {
        match self {
            Self::Byte => 0xff,
            Self::Word => 0xffff,
            Self::Long => 0xffff_ffff,
        }
    }

    /// The sign bit of a value this size.
    #[must_use]
    pub const fn sign_bit(self) -> u32 {
        match self {
            Self::Byte => 0x80,
            Self::Word => 0x8000,
            Self::Long => 0x8000_0000,
        }
    }

    /// `value`, taken as this size, sign extended to 32 bits.
    #[must_use]
    pub const fn sign_extend(self, value: u32) -> u32 {
        match self {
            Self::Byte => ((value as u8).cast_signed() as i32).cast_unsigned(),
            Self::Word => ((value as u16).cast_signed() as i32).cast_unsigned(),
            Self::Long => value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Size;

    #[test]
    fn sign_extension() {
        assert_eq!(Size::Byte.sign_extend(0x1ff), 0xffff_ffff);
        assert_eq!(Size::Word.sign_extend(0x7fff), 0x7fff);
        assert_eq!(Size::Word.sign_extend(0x8000), 0xffff_8000);
    }

    #[test]
    fn the_size_field() {
        assert_eq!(Size::from_field(0b10), Some(Size::Long));
        assert_eq!(Size::from_field(0b11), None);
    }
}
