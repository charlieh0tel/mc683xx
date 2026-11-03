//! An emulated target's breakpoints, watchpoints and steps: the core's
//! hooks, so no instruction is written to memory.  They work in ROM,
//! and firmware that sums its code never sees them.  Watchpoints are
//! read and write hooks; a single step is a code hook on every address,
//! for one instruction.

use std::cell::Cell;
use std::ops::ControlFlow;
use std::ops::RangeInclusive;
use std::rc::Rc;

use gdbstub::common::Signal;
use gdbstub::stub::SingleThreadStopReason;
use gdbstub::target::ext::breakpoints::WatchKind;
use mc683xx_cpu32::cpu::hooks::HookId;
use mc683xx_cpu32::cpu::hooks::MemoryAccess;
use mc683xx_target::address::Address;

use super::QUANTUM;
use super::Session;
use super::signaled;
use crate::target::Target;
use crate::target::TargetRun;

/// Core runs a step makes at most: one that stops before the
/// instruction for the step's own hook, and one that runs it.
const STEP_RUNS: usize = 2;
/// Bytes before a watched address an access may start and still reach
/// it: a long's.
const ACCESS_REACH: u32 = 3;

/// What stopped the target for gdb: the first of the stub's hooks to
/// ask since the run began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hit {
    /// A breakpoint, as gdb set it.
    Breakpoint(Kind),
    /// A watchpoint, of its kind, at the address gdb gave.
    Watch(WatchKind, u32),
    /// The step's hook.
    Step,
}

/// A breakpoint's kind, as gdb set it.  Both are code hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `Z0`: gdb's `break`.
    Software,
    /// `Z1`: gdb's `hbreak`.
    Hardware,
}

/// The stub's record of a hit, shared with its hooks.
pub(super) type Hits = Rc<Cell<Option<Hit>>>;

/// Records `hit` unless one is recorded already.
fn record(hits: &Hits, hit: Hit) {
    if hits.get().is_none() {
        hits.set(Some(hit));
    }
}

/// A watchpoint gdb set.
#[derive(Debug)]
pub(super) struct Watchpoint {
    /// Its address, as gdb gave it.
    address: u32,
    /// Its bytes.
    length: u32,
    /// What it watches.
    kind: WatchKind,
    /// Its read and write hooks.
    hooks: Vec<HookId>,
}

impl<T: Target> Session<'_, T> {
    /// One instruction: a code hook on every address stops the run
    /// before the next.  If the hook stopped it before the first (no
    /// hook had stopped there yet), the next run goes past it, as a
    /// run does past a hook's stop.  The CPU waiting in `stop` steps no
    /// further than a quantum's wait.  A target without the CPU's
    /// controls, offered no step, is `SIGILL`.
    pub(super) fn hook_step(&mut self) -> Result<SingleThreadStopReason<u32>, T::Error> {
        let hits = Rc::clone(&self.hit);
        let Some((id, pc, count)) = self.target.with_cpu(|cpu| {
            let id = cpu.add_code_hook(everywhere(), move |_| {
                record(&hits, Hit::Step);
                ControlFlow::Break(())
            });
            (id, cpu.registers.pc, cpu.instructions())
        }) else {
            return Ok(SingleThreadStopReason::Signal(Signal::SIGILL));
        };
        let stepped = self.step_runs(pc, count);
        self.target.with_cpu(|cpu| cpu.remove_hook(id));
        stepped
    }

    /// `hook_step`'s runs, from `pc` with `count` instructions run.
    fn step_runs(&mut self, pc: u32, count: u64) -> Result<SingleThreadStopReason<u32>, T::Error> {
        let mut ran = TargetRun::Quantum;
        let mut watched = None;
        for _ in 0..STEP_RUNS {
            self.hit.set(None);
            let hits = Rc::clone(&self.hit);
            ran = self
                .target
                .run_for(QUANTUM, &move || hits.get().is_some())?;
            if let Some(hit @ Hit::Watch(..)) = self.hit.get() {
                watched = Some(hit);
            }
            let moved = self
                .target
                .with_cpu(|cpu| cpu.instructions() != count || cpu.registers.pc != pc)
                .unwrap_or(true);
            if moved || ran != TargetRun::Hook {
                break;
            }
        }
        let watched = watched.filter(|_| self.survives(ran));
        Ok(match (watched, ran) {
            (Some(hit), _) => stop_reason(hit),
            (None, TargetRun::Quantum | TargetRun::Hook) => SingleThreadStopReason::DoneStep,
            (None, other) => signaled(other),
        })
    }

    /// Adds a breakpoint of `kind` at `address`: a code hook there.
    pub(super) fn add_hook_breakpoint(&mut self, kind: Kind, address: u32) -> bool {
        if self
            .hooked
            .iter()
            .any(|&(other, at, _)| (other, at) == (kind, address))
        {
            return true;
        }
        let hits = Rc::clone(&self.hit);
        let Some(id) = self.target.with_cpu(|cpu| {
            let at = cpu.memory.mask(Address(address));
            cpu.add_code_hook(at..=at, move |_| {
                record(&hits, Hit::Breakpoint(kind));
                ControlFlow::Break(())
            })
        }) else {
            return false;
        };
        self.hooked.push((kind, address, id));
        true
    }

    /// Removes the breakpoint of `kind` at `address`; whether there was
    /// one.
    pub(super) fn remove_hook_breakpoint(&mut self, kind: Kind, address: u32) -> bool {
        let Some(index) = self
            .hooked
            .iter()
            .position(|&(other, at, _)| (other, at) == (kind, address))
        else {
            return false;
        };
        let (_, _, id) = self.hooked.remove(index);
        self.target
            .with_cpu(|cpu| cpu.remove_hook(id))
            .unwrap_or(false)
    }

    /// Adds a watchpoint: read hooks, write hooks or both on the bytes,
    /// each asking to stop for an access that overlaps them.  The stop
    /// comes once the instruction ends.
    pub(super) fn add_watchpoint(&mut self, address: u32, length: u32, kind: WatchKind) -> bool {
        let Some(last_offset) = length.checked_sub(1) else {
            return false;
        };
        let hits = &self.hit;
        let Some(hooks) = self.target.with_cpu(|cpu| {
            let first = cpu.memory.mask(Address(address)).0;
            let last = first.saturating_add(last_offset);
            let reach = Address(first.saturating_sub(ACCESS_REACH))..=Address(last);
            let hook = |hits: Hits| {
                move |access: &MemoryAccess| {
                    let start = access.address.0;
                    let end = start.saturating_add(access.size.bytes() - 1);
                    if start <= last && end >= first {
                        record(&hits, Hit::Watch(kind, address));
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                }
            };
            let mut hooks = Vec::new();
            if matches!(kind, WatchKind::Read | WatchKind::ReadWrite) {
                hooks.push(cpu.add_read_hook(reach.clone(), hook(Rc::clone(hits))));
            }
            if matches!(kind, WatchKind::Write | WatchKind::ReadWrite) {
                hooks.push(cpu.add_write_hook(reach, hook(Rc::clone(hits))));
            }
            hooks
        }) else {
            return false;
        };
        self.watchpoints.push(Watchpoint {
            address,
            length,
            kind,
            hooks,
        });
        true
    }

    /// Removes a watchpoint; whether there was one.
    pub(super) fn remove_watchpoint(&mut self, address: u32, length: u32, kind: WatchKind) -> bool {
        let Some(index) = self
            .watchpoints
            .iter()
            .position(|watch| (watch.address, watch.length, watch.kind) == (address, length, kind))
        else {
            return false;
        };
        let watch = self.watchpoints.remove(index);
        self.target.with_cpu(|cpu| {
            for id in watch.hooks {
                cpu.remove_hook(id);
            }
        });
        true
    }

    /// Removes every hook the session installed.
    pub(super) fn remove_hooks(&mut self) {
        let breakpoints = self.hooked.drain(..).map(|(_, _, id)| id);
        let watchpoints = self.watchpoints.drain(..).flat_map(|watch| watch.hooks);
        let hooks: Vec<HookId> = breakpoints.chain(watchpoints).collect();
        if hooks.is_empty() {
            return;
        }
        self.target.with_cpu(|cpu| {
            for id in hooks {
                cpu.remove_hook(id);
            }
        });
    }
}

/// Every address, for the step's hook.
const fn everywhere() -> RangeInclusive<Address> {
    Address(0)..=Address(u32::MAX)
}

/// What gdb is told of `hit`.
pub(super) const fn stop_reason(hit: Hit) -> SingleThreadStopReason<u32> {
    match hit {
        Hit::Breakpoint(Kind::Software) => SingleThreadStopReason::SwBreak(()),
        Hit::Breakpoint(Kind::Hardware) => SingleThreadStopReason::HwBreak(()),
        Hit::Watch(kind, addr) => SingleThreadStopReason::Watch {
            tid: (),
            kind,
            addr,
        },
        Hit::Step => SingleThreadStopReason::DoneStep,
    }
}
