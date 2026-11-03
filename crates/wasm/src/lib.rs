//! JavaScript bindings for MC68331 boards, through WebAssembly
//! (`wasm32-unknown-unknown`, `wasm-bindgen`): a board loaded from its
//! board file's text and its images' bytes, run a frame at a time from
//! the page's clock, its console as bytes, its state saved and
//! restored.  `README.md` is the JavaScript guide; it builds and tests
//! natively too, where the Rust API ([`board::Board::with_images`]) is
//! the one to call.

pub mod board;
pub mod error;

#[cfg(test)]
mod tests;
