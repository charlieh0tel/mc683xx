//! README.md's Rust example is `readme/board.rs`: it must appear there
//! verbatim, and it must run.

const README: &str = include_str!("../../README.md");
const BOARD: &str = include_str!("readme/board.rs");

mod board {
    include!("readme/board.rs");

    #[test]
    fn the_readme_board_example_runs() {
        // The example names its board file from the workspace root,
        // where a reader runs it.  The only test here that depends on
        // the working directory.
        std::env::set_current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap();
        main().unwrap();
    }
}

#[test]
fn the_readme_shows_the_board_example() {
    assert!(
        README.contains(&format!("```rust\n{BOARD}```\n")),
        "README.md's Rust example differs from examples/tests/readme/board.rs"
    );
}
