//! `--gdb`: the board under gdb ([docs/gdb.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/gdb.md)), the runner's turns kept
//! between the stub's quanta, so the bindings and the trace go on
//! while gdb runs it, and its pacing kept while gdb has it running.

use std::io;
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use mc683xx_board::board::Board;
use mc683xx_board::condition::Conditions;
use mc683xx_board::control::BoardControl;
use mc683xx_board::pacing::Pacer;
use mc683xx_cpu32::cpu::control::CpuControl;
use mc683xx_cpu32::cycles::Cycles;
use mc683xx_gdb::Ending;
use mc683xx_gdb::SessionError;
use mc683xx_gdb::connection;
use mc683xx_gdb::connection::PollError;
use mc683xx_gdb::connection::Waiter;
use mc683xx_gdb::target::AccessError;
use mc683xx_gdb::target::Target;
use mc683xx_gdb::target::TargetRun;
use mc683xx_mc68331::chip::Mc68331;
use mc683xx_target::address::Address;
use mc683xx_target::function_code::FunctionCode;

use crate::Turns;
use crate::error::RunError;

/// The board as gdb runs it.
struct Debugged<'a> {
    /// The board.
    board: &'a mut Board<Mc68331>,
    /// What goes in and out between quanta.
    turns: &'a mut Turns,
    /// The board's control, whose pacing paces it.
    control: BoardControl,
    /// Holds board time to wall time under the pacing.
    pacer: Pacer,
    /// Whether gdb has the board running, continued and not yet
    /// stopped: only then is it paced, so a step never waits.
    running: bool,
}

impl Debugged<'_> {
    /// Whether the board is still ahead of wall time under its pacing
    /// after one wait of at most the pacer's slice, cut short by a
    /// stop: the stub then looks for gdb's bytes before it runs on, so
    /// a slow pacing never keeps gdb waiting.
    fn ahead(&mut self) -> bool {
        let control = &self.control;
        self.pacer
            .wait_with(self.board.now(), control, |delay| {
                control.sleep(delay);
                ControlFlow::Break(())
            })
            .is_break()
    }
}

impl Target for Debugged<'_> {
    type Error = RunError;

    fn with_cpu<R>(&mut self, call: impl FnOnce(&mut CpuControl<'_>) -> R) -> Option<R> {
        Target::with_cpu(self.board, call)
    }

    fn read_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &mut [u8],
    ) -> Result<usize, AccessError<RunError>> {
        Target::read_memory(self.board, address, function_code, data)
            .map_err(|error| error.map(RunError::from))
    }

    fn write_memory(
        &mut self,
        address: Address,
        function_code: FunctionCode,
        data: &[u8],
    ) -> Result<(), AccessError<RunError>> {
        Target::write_memory(self.board, address, function_code, data)
            .map_err(|error| error.map(RunError::from))
    }

    fn resume(&mut self) -> Result<(), RunError> {
        // the pacing starts again where gdb stopped the board, which
        // does not run to catch up the wall time it stood still
        self.pacer = Pacer::new();
        self.running = true;
        Ok(())
    }

    /// A quantum the pacing holds back is none: the board runs no
    /// time, and the stub looks for gdb's bytes.
    fn run_for(&mut self, cycles: Cycles, hit: &dyn Fn() -> bool) -> Result<TargetRun, RunError> {
        if self.running && self.ahead() {
            return Ok(TargetRun::Quantum);
        }
        self.turns.before(self.board)?;
        let ran = Target::run_for(self.board, cycles, hit)?;
        self.turns.after(self.board)?;
        if ran != TargetRun::Quantum || hit() {
            self.running = false;
        }
        Ok(ran)
    }

    fn halt(&mut self) -> Result<TargetRun, RunError> {
        self.running = false;
        Ok(TargetRun::Requested)
    }

    fn reset(&mut self) -> Result<bool, RunError> {
        Ok(Target::reset(self.board)?)
    }

    fn conditions(&mut self) -> Option<&mut dyn Conditions> {
        Some(self.board)
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
    let accepted = connection::accept_polled(
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
    let mut debugged = Debugged {
        control: board.control(),
        board,
        turns,
        pacer: Pacer::new(),
        running: false,
    };
    match mc683xx_gdb::serve(&mut debugged, connection) {
        Ok(ending) => Ok(ending),
        Err(SessionError::Target(error)) => Err(error),
        Err(SessionError::Connection(PollError::Host(Interrupted))) => Ok(Ending::Detached),
        Err(error) => {
            eprintln!("mc683xx-run: gdb: {error}; running on as if it detached");
            Ok(Ending::Detached)
        }
    }
}
