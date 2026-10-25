//! The committed `boards/echo.bin` is what `echo::image` builds; after
//! a change to either, `make echo-image` rewrites the file.

use std::fs;
use std::path::Path;

use mc683xx_examples::echo::image;

#[test]
fn the_committed_echo_image_is_the_generators() {
    let committed =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("boards/echo.bin")).unwrap();
    assert!(
        committed == image(),
        "boards/echo.bin is stale: run `make echo-image`"
    );
}
