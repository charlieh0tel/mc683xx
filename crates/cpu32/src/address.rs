//! Addresses on the CPU's bus.

use std::fmt;

/// A byte address on the CPU32's 32-bit address bus (CPU32RM 5.3).  A
/// chip with fewer address lines masks it (see
/// [`Memory`](crate::memory::Memory)).
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Address(pub u32);

impl Address {
    /// The address `by` bytes on, wrapping as the bus does.
    #[must_use]
    pub const fn offset(self, by: u32) -> Self {
        Self(self.0.wrapping_add(by))
    }

    /// Whether the address is odd: a word or long access there is an
    /// address error (CPU32RM 6.2.3).
    #[must_use]
    pub const fn is_odd(self) -> bool {
        self.0 & 1 != 0
    }
}

/// `0x` and eight hex digits, as traces write an address.
impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#010x}", self.0)
    }
}

/// The address's hex digits, as [`u32`]'s, its flags and width honored.
impl fmt::LowerHex for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl From<u32> for Address {
    fn from(address: u32) -> Self {
        Self(address)
    }
}

impl From<Address> for u32 {
    fn from(address: Address) -> Self {
        address.0
    }
}

#[cfg(test)]
mod tests {
    use super::Address;

    #[test]
    fn an_address_shows_as_hex() {
        assert_eq!(Address(0x1234).to_string(), "0x00001234");
        assert_eq!(format!("{:#x}", Address(0xab)), "0xab");
        assert_eq!(u32::from(Address::from(7)), 7);
    }
}
