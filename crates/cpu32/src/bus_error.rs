//! How a bus cycle ended in error, and the port widths it sizes to.

/// The cycle ended in a bus error (BERR, CPU32RM 6.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bus error")]
pub struct BusError;

/// The width of the port a cycle reaches, which DSACK reports and the
/// CPU32 sizes its cycles to (MC68331UM 4.4.2, dynamic bus sizing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PortSize {
    /// An 8-bit port: a word takes two byte cycles.
    Bits8,
    /// A 16-bit port.
    Bits16,
}
