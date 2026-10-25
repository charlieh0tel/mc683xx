//! `--gdb`: the board under gdb ([docs/gdb.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md)), the runner's turns kept
//! between the stub's quanta, so the bindings and the trace go on
//! while gdb runs it.

use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use mc683xx_board::board::Board;

use mc683xx_cpu32::address::Address;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_gdb::stub;
use mc683xx_gdb::stub::Ending;
use mc683xx_gdb::stub::PollError;
use mc683xx_gdb::stub::Waiter;
use mc683xx_gdb::target::Target;
use mc683xx_gdb::target::TargetRun;
use mc683xx_gdb::target::Unwritable;
use mc683xx_mc68331::chip::Mc68331;

use crate::Turns;
use crate::error::RunError;

/// The board as gdb runs it.
struct Debugged<'a> {
    /// The board.
    board: &'a mut Board<Mc68331>,
    /// What goes in and out between quanta.
    turns: &'a mut Turns,
}

impl Target for Debugged<'_> {
    type Error = RunError;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> R {
        self.board.with_cpu(call)
    }

    fn peek(&mut self, address: Address, data: &mut [u8]) -> usize {
        Target::peek(self.board, address, data)
    }

    fn poke(&mut self, address: Address, data: &[u8]) -> Result<(), Unwritable> {
        Target::poke(self.board, address, data)
    }

    fn run_for(&mut self, cycles: Cycles, hit: &dyn Fn() -> bool) -> Result<TargetRun, RunError> {
        self.turns.before(self.board)?;
        let ran = Target::run_for(self.board, cycles, hit)?;
        self.turns.after(self.board)?;
        Ok(ran)
    }

    fn reset(&mut self) -> bool {
        Target::reset(self.board)
    }
}

/// Ctrl-C, which ends the session.
#[derive(Debug, thiserror::Error)]
#[error("interrupted")]
struct Interrupted;

/// The runner waiting for gdb: between its waits it looks for Ctrl-C.
struct Interruptible<'a>(&'a AtomicBool);

impl Waiter for Interruptible<'_> {
    type Error = Interrupted;

    fn wait<R: Send>(&mut self, wait: impl FnOnce() -> R + Send) -> R {
        wait()
    }

    fn idle(&mut self) -> Result<(), Interrupted> {
        if self.0.load(Ordering::SeqCst) {
            Err(Interrupted)
        } else {
            Ok(())
        }
    }
}

/// Waits for gdb at `address`, saying where on standard error, and
/// serves it until it detaches or kills the board, or `interrupted`
/// (Ctrl-C) is set, which ends the session as a detach does, at its
/// next wait for gdb.  A session that fails, gdb gone or a packet the
/// stub cannot take, is said on standard error and ends as a detach
/// does, so the run goes on to its end; only the board's own errors end
/// the run.
pub(crate) fn serve(
    board: &mut Board<Mc68331>,
    turns: &mut Turns,
    address: &str,
    interrupted: &AtomicBool,
) -> Result<Ending, RunError> {
    let accepted = stub::accept_polled(
        address,
        |at| {
            eprintln!("mc683xx-run: waiting for gdb on {at}");
            Ok(())
        },
        Interruptible(interrupted),
    );
    let connection = match accepted {
        Ok(connection) => connection,
        Err(PollError::Host(Interrupted)) => return Ok(Ending::Detached),
        Err(PollError::Connection(error)) => return Err(RunError::Gdb(error)),
        Err(error) => return Err(RunError::Gdb(io::Error::other(error.to_string()))),
    };
    match stub::serve(&mut Debugged { board, turns }, connection) {
        Ok(ending) => Ok(ending),
        Err(stub::SessionError::Target(error)) => Err(error),
        Err(stub::SessionError::Connection(PollError::Host(Interrupted))) => Ok(Ending::Detached),
        Err(error) => {
            eprintln!("mc683xx-run: gdb: {error}; running on as if it detached");
            Ok(Ending::Detached)
        }
    }
}
