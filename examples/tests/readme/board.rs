use std::error::Error;
use std::path::Path;

use mc683xx_board::board::Board;
use mc683xx_board::description::Options;
use mc683xx_board::registry::Environment;
use mc683xx_board::registry::Registry;
use mc683xx_board::time::Time;
use mc683xx_mc68331::chip::Mc68331;

fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = Registry::with_builtins();
    mc683xx_peripherals::register(&mut registry)?;
    let mut board = Board::<Mc68331>::read(
        Path::new("examples/boards/echo.toml"),
        &Options::default(),
        &registry,
        Environment::default(),
    )?;
    let run = board.run_until(Time::from_seconds(1))?;
    println!("{:?} at {:?}", run.stop, run.now);
    Ok(())
}
