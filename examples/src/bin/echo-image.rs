//! Writes `examples/boards/echo.bin`, the example board's firmware
//! (`mc683xx_examples::echo`), or the file its one argument names.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use mc683xx_examples::echo::image;

fn main() -> io::Result<()> {
    let path = env::args_os().nth(1).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("boards/echo.bin"),
        PathBuf::from,
    );
    fs::write(path, image())
}
