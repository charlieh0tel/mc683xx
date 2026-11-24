"""The Motorola MC68331 (CPU32) emulator: type stubs for the compiled
module (docs/python.md, PLAN.md Python).  `make python` checks them
against the module with mypy's stubtest."""

from collections.abc import Callable, Iterable, Mapping, Sequence
from os import PathLike
from typing import ClassVar, Literal, Protocol, final

from typing_extensions import disjoint_base

__all__ = [
    "Machine",
    "BusCycle",
    "FaultStop",
    "FaultCause",
    "PeripheralId",
    "MemoryId",
    "Intercepted",
    "StopRequest",
    "SyncRequest",
    "Diversion",
    "Dsack",
    "DsackFault",
    "Termination",
    "Board",
    "BoardChip",
    "PeripheralAccess",
    "Role",
    "Lane",
    "Transfer",
    "Drive",
    "Moment",
    "TimeBase",
    "Serial",
    "External",
    "ObserverId",
    "BoardControl",
    "BoardStopRequest",
    "Pacing",
    "Builder",
    "Mc68331",
    "Run",
    "LimitedRun",
    "BoardRun",
    "End",
    "Stop",
    "Halt",
    "Entry",
    "ResetCause",
    "FunctionCode",
    "Event",
    "Ending",
    "Frame",
    "Output",
    "Channel",
    "Port",
    "Input",
    "PortState",
    "QspiClock",
    "QspiTransfer",
    "ClockPolarity",
    "ClockPhase",
    "Space",
    "HookId",
    "Instruction",
    "MemoryAccess",
    "PendingRead",
    "InvalidAccess",
    "BusCycleKind",
    "Invalid",
    "ExceptionEntry",
    "ExceptionReturn",
    "BusCycleError",
    "BusError",
    "UnpeekableError",
    "StateError",
    "LoadError",
    "RunError",
    "TargetError",
    "MappedPeripheral",
    "SpiPeripheral",
    "Peripheral",
]

__version__: str
"""The package's version, one for the crates, the package and npm's."""

# What the host passes in, the methods the module calls on it.  Each
# protocol declares the methods it requires; its docstring the optional
# ones, a missing one taking the Rust trait's default.

class MappedPeripheral(Protocol):
    """A bus peripheral, as `Builder.peripheral`, `Builder.bus_peripheral`
    and `Machine.add_peripheral` take it (Rust's `MappedPeripheral` and
    `Timed`): `read` and `write` answer its cycles.  The module also
    calls these, each optional, a missing one taking the trait's
    default: `wait_states(cycle) -> int | None`, `dsack_port(cycle) ->
    int | None` (8, 16 or None), `interrupt_request() -> bool`,
    `reset(now: int)`, `next_event() -> int | None`, `advance(now:
    int)`, `peek(cycle) -> int | None` and `poke(cycle, value) -> bool`
    (a debugger's byte with no cycle's side effects; `None` and `False`
    decline, as missing ones do), and `save() -> bytes` with
    `restore(snapshot: bytes)`, both or neither."""

    def read(self, cycle: BusCycle, /) -> int: ...
    def write(self, cycle: BusCycle, value: int, /) -> object: ...

class SpiPeripheral(Protocol):
    """The QSPI's peripheral, as `Builder.set_qspi` takes it (Rust's
    `SpiPeripheral`): `transfer` shifts a queue entry's bits.  The
    module also calls these, each optional: `set_chip_selects(levels:
    int, now: int)`, `set_clock(clock: QspiClock, now: int)`,
    `reset(now: int)`, `deferrable() -> bool`, and `save() -> bytes`
    with `restore(snapshot: bytes)`, both or neither."""

    def transfer(self, transfer: QspiTransfer, /) -> int: ...

class Peripheral(Protocol):
    """A board peripheral, as the class `Board`'s `types` names makes it
    (Rust's `Peripheral`), called with its parameters as keyword
    arguments.  Its class declares its ports in `PORTS`, `(name, kind,
    direction)` each, and is told serial frames as `Serial`s; every
    method is optional, a missing one taking the
    trait's default: `read(access, drive) -> int`, `write(access,
    value, drive)`, `wait_states(access, moment) -> int | None`,
    `dsack_port(access, moment) -> int | None`, `wants_horizon() -> bool`,
    `interrupt_request() -> bool`, `power_on(drive)`,
    `reset(drive)`, `transfer(transfer, drive) -> int`,
    `set_qspi_clock(clock, moment)`, `peek(access, moment) -> int |
    None`, `poke(access, value, moment) -> bool`, `set_selected(attachment, selected,
    drive)`, `input(port, value, drive)`, `analog_output(port, moment) ->
    float | None`, `next_event(moment) -> int | None`, `advance(drive)`,
    `qspi_deferrable() -> bool`, `memory_read(memory, access, drive) ->
    int`, `memory_write(memory, access, value, drive)`, `inspect() ->
    Mapping[str, object]`, and `save() -> bytes` with
    `restore(snapshot: bytes)`, both or neither (docs/python.md, Board
    files)."""

class BusCycleError(Exception):
    """Raised by a host's bus cycle or peek that gave no value (Rust's
    BusCycleError): BusError or UnpeekableError."""

class BusError(BusCycleError):
    """Raised by a peripheral's read or write: BERR ends
    the cycle."""

class UnpeekableError(BusCycleError):
    """Raised by peek where only a real bus cycle can tell: a peripheral,
    or a module register a read would change."""

class StateError(ValueError):
    """Raised by restore for a snapshot of another format or other
    hardware, another count of peripherals, bytes that do not decode, or
    values no run could save; by save and restore for a peripheral that
    cannot save, named."""

class LoadError(ValueError):
    """Raised by Board for a board file it refuses, naming what in the
    file is wrong; a file it cannot read, the board file or a memory's
    image, raises OSError instead."""

class RunError(RuntimeError):
    """Raised by a run that could not go on: a board peripheral that
    stalls board time, or a run's end the bindings do not know; and by
    take_exception for an exception the CPU could not take."""

class TargetError(RuntimeError):
    """Raised by the chip's background debug controls: halt with
    background mode not enabled at the last reset, or go or step with
    the CPU not in background mode."""

@disjoint_base
class Stop:
    """Why a run ended."""

    def __hash__(self) -> int: ...

    @final
    class Budget(Stop):
        """It spent its budget."""

        __match_args__ = ()
        def __new__(cls) -> Stop.Budget: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Stopped(Stop):
        """`stop` or `lpstop`: the CPU waits for an interrupt."""

        __match_args__ = ()
        def __new__(cls) -> Stop.Stopped: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Halted(Stop):
        """The CPU halted; only reset restarts it."""

        __match_args__ = ("halt",)
        def __new__(cls, halt: Halt) -> Stop.Halted: ...
        @property
        def halt(self) -> Halt: ...

    @final
    class Hook(Stop):
        """A hook stopped it: the first to, if several did at once."""

        __match_args__ = ("hook",)
        def __new__(cls, hook: HookId) -> Stop.Hook: ...
        @property
        def hook(self) -> HookId: ...

    @final
    class Requested(Stop):
        """The host or a peripheral asked it to stop."""

        __match_args__ = ()
        def __new__(cls) -> Stop.Requested: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Background(Stop):
        """The CPU is in background mode, for its entry source."""

        __match_args__ = ("entry",)
        def __new__(cls, entry: Entry) -> Stop.Background: ...
        @property
        def entry(self) -> Entry: ...

    @final
    class Fault(Stop):
        """A fault stopped it rather than being taken
        (`set_stop_on_fault`): `fault_stop` says which."""

        __match_args__ = ()
        def __new__(cls) -> Stop.Fault: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

@disjoint_base
class Entry:
    """Why the CPU entered background mode (CPU32RM Table 7-2)."""

    def __hash__(self) -> int: ...

    @final
    class Breakpoint(Entry):
        """BKPT asserted."""

        __match_args__ = ()
        def __new__(cls) -> Entry.Breakpoint: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Background(Entry):
        """A BGND instruction."""

        __match_args__ = ()
        def __new__(cls) -> Entry.Background: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class DoubleBusFault(Entry):
        """A double bus fault, with the second fault's SSW."""

        __match_args__ = ("ssw",)
        def __new__(cls, ssw: int) -> Entry.DoubleBusFault: ...
        @property
        def ssw(self) -> int: ...

    @final
    class Unknown(Entry):
        """An ATEMP Table 7-2 does not list."""

        __match_args__ = ("atemp",)
        def __new__(cls, atemp: int) -> Entry.Unknown: ...
        @property
        def atemp(self) -> int: ...

@final
class Halt:
    """Why the CPU halted."""

    DOUBLE_BUS_FAULT: ClassVar[Halt]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class Ending:
    """How a gdb session ended, as `serve_gdb` returns it."""

    DETACHED: ClassVar[Ending]
    KILLED: ClassVar[Ending]
    HUNG_UP: ClassVar[Ending]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class ResetCause:
    """A reset's cause, as RSR records it (MC68331UM D.3.4)."""

    EXTERNAL: ClassVar[ResetCause]
    POWER_ON: ClassVar[ResetCause]
    WATCHDOG: ClassVar[ResetCause]
    HALT_MONITOR: ClassVar[ResetCause]
    LOSS_OF_CLOCK: ClassVar[ResetCause]
    SYSTEM: ClassVar[ResetCause]
    TEST: ClassVar[ResetCause]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class FunctionCode:
    """The function code FC[2:0] a bus cycle carries, naming its address
    space (CPU32RM 5.3, Table 5-1).  `str()` is its Rust name,
    `supervisor-data`."""

    RESERVED0: ClassVar[FunctionCode]
    USER_DATA: ClassVar[FunctionCode]
    USER_PROGRAM: ClassVar[FunctionCode]
    RESERVED3: ClassVar[FunctionCode]
    RESERVED4: ClassVar[FunctionCode]
    SUPERVISOR_DATA: ClassVar[FunctionCode]
    SUPERVISOR_PROGRAM: ClassVar[FunctionCode]
    CPU_SPACE: ClassVar[FunctionCode]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class Space:
    """An address space whose pages `Mc68331.protect` sets."""

    USER: ClassVar[Space]
    SUPERVISOR: ClassVar[Space]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class ClockPolarity:
    """SCK's level between transfers (QSMRM 4.3.1, CPOL)."""

    IDLE_LOW: ClassVar[ClockPolarity]
    IDLE_HIGH: ClassVar[ClockPolarity]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class ClockPhase:
    """Which SCK edges change and capture the data (QSMRM 4.3.1, CPHA)."""

    CAPTURE_LEADING: ClassVar[ClockPhase]
    CHANGE_LEADING: ClassVar[ClockPhase]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class BusCycleKind:
    """A bus cycle's kind (CPU32RM 6.3)."""

    FETCH: ClassVar[BusCycleKind]
    READ: ClassVar[BusCycleKind]
    WRITE: ClassVar[BusCycleKind]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class Invalid:
    """Why a bus cycle was invalid."""

    BUS_ERROR: ClassVar[Invalid]
    PROTECTED: ClassVar[Invalid]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class Channel:
    """One of the GPT's two PWM outputs."""

    A: ClassVar[Channel]
    B: ClassVar[Channel]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class Input:
    """A GPT input pin: a port GP pin, PAI or PCLK."""

    PGP0: ClassVar[Input]
    PGP1: ClassVar[Input]
    PGP2: ClassVar[Input]
    PGP3: ClassVar[Input]
    PGP4: ClassVar[Input]
    PGP5: ClassVar[Input]
    PGP6: ClassVar[Input]
    PGP7: ClassVar[Input]
    PAI: ClassVar[Input]
    PCLK: ClassVar[Input]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class Port:
    """One of the chip's general-purpose ports."""

    C: ClassVar[Port]
    E: ClassVar[Port]
    F: ClassVar[Port]
    GP: ClassVar[Port]
    QS: ClassVar[Port]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class PortState:
    """A port's registers and the levels the board drives on its pins."""

    def __new__(cls, latch: int, direction: int, assignment: int, pins: int) -> PortState: ...
    def __hash__(self) -> int: ...
    @property
    def latch(self) -> int: ...
    @property
    def direction(self) -> int: ...
    @property
    def assignment(self) -> int: ...
    @property
    def pins(self) -> int: ...

@final
class Role:
    """What a board peripheral does with an attachment's cycles."""

    READ: ClassVar[Role]
    WRITE: ClassVar[Role]
    READ_WRITE: ClassVar[Role]
    IACK: ClassVar[Role]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class Lane:
    """The data lanes an attachment wires to a board peripheral."""

    BOTH: ClassVar[Lane]
    UPPER: ClassVar[Lane]
    LOWER: ClassVar[Lane]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class PeripheralId:
    """A peripheral on a bare `Machine` or a chip, as `add_peripheral`
    or the chip `Builder`'s peripheral methods return it; `str()` is
    `peripheral 3`.  The chip's methods raise `KeyError` for one its
    builder never returned."""

    def __hash__(self) -> int: ...

@final
class MemoryId:
    """Host memory wired into the chip, as the `Builder`'s `memory` and
    `fixed_memory` return it; `str()` is `memory 2`.  The chip's
    methods raise `KeyError` for one its builder never returned."""

    def __hash__(self) -> int: ...

@final
class HookId:
    """A hook, as `add_*_hook` returns it and `Stop.Hook` carries it;
    `str()` is `hook 3`."""

    def __hash__(self) -> int: ...

@final
class Run:
    """A chip's run's outcome."""

    @property
    def cycles(self) -> int: ...
    @property
    def stop(self) -> Stop: ...
    def __hash__(self) -> int: ...

@disjoint_base
class End:
    """How a bare `Machine`'s limited run ended."""

    def __hash__(self) -> int: ...

    @final
    class Stop(End):
        """As a chip's run ends."""

        __match_args__ = ("stop",)
        def __new__(cls, stop: Stop) -> End.Stop: ...
        @property
        def stop(self) -> Stop: ...

    @final
    class Reached(End):
        """PC reached `reach`."""

        __match_args__ = ()
        def __new__(cls) -> End.Reached: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Instructions(End):
        """It ran `instructions`."""

        __match_args__ = ()
        def __new__(cls) -> End.Instructions: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Fault(End):
        """A fault, with `set_stop_on_fault`."""

        __match_args__ = ("fault",)
        def __new__(cls, fault: FaultStop) -> End.Fault: ...
        @property
        def fault(self) -> FaultStop: ...

    @final
    class Exception(End):
        """An exception the CPU was about to take, with
        `set_stop_on_exception`: nothing stacked."""

        __match_args__ = ("vector", "pc")
        def __new__(cls, vector: int, pc: int) -> End.Exception: ...
        @property
        def vector(self) -> int: ...
        @property
        def pc(self) -> int: ...

@final
class LimitedRun:
    """A bare `Machine`'s limited run's outcome."""

    def __hash__(self) -> int: ...
    @property
    def cycles(self) -> int: ...
    @property
    def end(self) -> End: ...

@final
class BoardRun:
    """Why a board's run ended, and when, in picoseconds."""

    @property
    def stop(self) -> Stop: ...
    @property
    def now(self) -> int: ...
    def __hash__(self) -> int: ...

@final
class BusCycle:
    """A bus cycle as a mapped peripheral sees it, a chip's or a bare
    `Machine`'s."""

    @property
    def select(self) -> str | None:
        """The chip select asserted, on a chip: `CSBOOT`, `CS0` to `CS10`;
        `None` on a bare `Machine`."""
    @property
    def stacking(self) -> bool:
        """Whether exception processing is stacking a frame, on a bare `Machine`;
        always `False` on a chip."""
    @property
    def address(self) -> int:
        """The address: on a chip ADDR, on a bare `Machine` all 32 bits."""
    @property
    def size(self) -> int:
        """Bytes in the cycle: 1 or 2, or 4 on a bare `Machine` for a long the
        peripheral takes whole."""
    @property
    def function_code(self) -> FunctionCode:
        """Its function code."""
    @property
    def write(self) -> bool:
        """Whether it is a write, R/W low; set for `wait_states` and
        `dsack_port` too."""
    @property
    def now(self) -> int:
        """When it starts, in system clocks."""

@final
class QspiClock:
    """The QSPI's serial clock, as a peripheral's `clock` is told it."""

    @property
    def polarity(self) -> ClockPolarity: ...
    @property
    def phase(self) -> ClockPhase: ...
    @property
    def period(self) -> int:
        """System clocks in an SCK period, 2 x SPBR."""
    @property
    def system_clock_nanohertz(self) -> int:
        """The system clock's frequency, in nanohertz."""
    @property
    def baud_nanohertz(self) -> int:
        """SCK's frequency, the system clock / period (QSMRM equation
        4-1), in nanohertz, rounded down."""

@final
class QspiTransfer:
    """A QSPI transfer, one queue entry's, as a peripheral's `transfer`
    gets it."""

    @property
    def entry(self) -> int:
        """The queue entry, 0 to 15."""
    @property
    def chip_selects(self) -> int:
        """The levels on PCS[3:0]."""
    @property
    def out(self) -> int:
        """The bits on MOSI, right-aligned: those shifted out, or the
        pin's level where MOSI is not the QSPI's output."""
    @property
    def bits(self) -> int: ...
    @property
    def clock(self) -> QspiClock:
        """The serial clock it is shifted with: its mode and SCK period."""
    @property
    def now(self) -> int:
        """When it ends, in system clocks."""

@final
class Instruction:
    """An instruction about to execute, as a code or block hook sees it."""

    @property
    def pc(self) -> int: ...
    @property
    def registers(self) -> dict[str, int]: ...
    @property
    def stack_top(self) -> int | None: ...

@final
class MemoryAccess:
    """An operand cycle, as a read or write hook sees it."""

    @property
    def address(self) -> int: ...
    @property
    def size(self) -> int: ...
    @property
    def function_code(self) -> FunctionCode: ...
    @property
    def value(self) -> int: ...
    @property
    def previous(self) -> int | None: ...
    @property
    def pc(self) -> int: ...
    @property
    def stacking(self) -> bool:
        """Whether exception processing is stacking a frame: a hook
        after the program's own writes alone returns early on it."""
    @property
    def registers(self) -> dict[str, int] | None:
        """The registers as the instruction began, by name, on a bare
        `Machine`; always `None` on an `Mc68331` and a `Board`."""

@final
class PendingRead:
    """An operand read cycle about to be served from memory, as a
    pre-read hook sees it, beside a bytearray of the bytes it is to
    read."""

    @property
    def address(self) -> int: ...
    @property
    def size(self) -> int: ...
    @property
    def function_code(self) -> FunctionCode: ...
    @property
    def pc(self) -> int: ...
    @property
    def stacking(self) -> bool: ...

@final
class InvalidAccess:
    """A bus cycle that ended in a bus error, as an invalid access hook
    sees it."""

    @property
    def address(self) -> int: ...
    @property
    def size(self) -> int: ...
    @property
    def function_code(self) -> FunctionCode: ...
    @property
    def kind(self) -> BusCycleKind: ...
    @property
    def value(self) -> int | None: ...
    @property
    def reason(self) -> Invalid: ...
    @property
    def pc(self) -> int: ...
    @property
    def stacking(self) -> bool:
        """Whether exception processing is stacking a frame."""
    @property
    def registers(self) -> dict[str, int] | None: ...

@final
class ExceptionEntry:
    """An exception entered, as an exception hook sees it."""

    @property
    def vector(self) -> int: ...
    @property
    def pc(self) -> int: ...
    @property
    def handler(self) -> int: ...
    @property
    def stack(self) -> int: ...
    @property
    def level(self) -> int | None: ...

@final
class ExceptionReturn:
    """An exception returned from, as an exception return hook sees it."""

    @property
    def vector(self) -> int: ...
    @property
    def pc(self) -> int: ...
    @property
    def sr(self) -> int: ...
    @property
    def stack(self) -> int: ...

@disjoint_base
class Frame:
    """A frame on TXD or RXD."""

    def __hash__(self) -> int: ...

    @final
    class Data(Frame):
        """The data bits as on the line, LSB first, right-aligned."""

        __match_args__ = ("data",)
        def __new__(cls, data: int) -> Frame.Data: ...
        @property
        def data(self) -> int: ...

    @final
    class Break(Frame):
        """A frame of zeros, stop bit included (QSMRM 5.2.2, SBK)."""

        __match_args__ = ()
        def __new__(cls) -> Frame.Break: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

@disjoint_base
class Output:
    """What a PWM pin or OC1's carries."""

    def __hash__(self) -> int: ...

    @final
    class Level(Output):
        """A steady level."""

        __match_args__ = ("level",)
        def __new__(cls, level: bool) -> Output.Level: ...
        @property
        def level(self) -> bool: ...

    @final
    class Wave(Output):
        """High for `high_counts` of every `period_counts` PWMCNT counts;
        `clocks_per_count` system clocks a count, or None when PCLK clocks
        PWMCNT."""

        __match_args__ = ("period_counts", "high_counts", "clocks_per_count")
        def __new__(
            cls, period_counts: int, high_counts: int, clocks_per_count: int | None
        ) -> Output.Wave: ...
        @property
        def period_counts(self) -> int: ...
        @property
        def high_counts(self) -> int: ...
        @property
        def clocks_per_count(self) -> int | None: ...

    @final
    class Clock(Output):
        """A counter's clock: PWMCNT's on PWMA (PPROUT), TCNT's on OC1
        (CPROUT); `clocks_per_count` system clocks a count, or None for
        PCLK."""

        __match_args__ = ("clocks_per_count",)
        def __new__(cls, clocks_per_count: int | None) -> Output.Clock: ...
        @property
        def clocks_per_count(self) -> int | None: ...

@disjoint_base
class Event:
    """Something the chip did that the host may want to know of."""

    def __hash__(self) -> int: ...

    @final
    class Reset(Event):
        """The chip reset, at `now`, for `cause`."""

        __match_args__ = ("now", "cause")
        def __new__(cls, now: int, cause: ResetCause) -> Event.Reset: ...
        @property
        def now(self) -> int: ...
        @property
        def cause(self) -> ResetCause: ...

    @final
    class ArbitrationTie(Event):
        """Two interrupt sources of one level had the same nonzero IARB."""

        __match_args__ = ("now", "level", "arbitration")
        def __new__(cls, now: int, level: int, arbitration: int) -> Event.ArbitrationTie: ...
        @property
        def now(self) -> int: ...
        @property
        def level(self) -> int: ...
        @property
        def arbitration(self) -> int: ...

    @final
    class BusCycle(Event):
        """A bus cycle that reached the chip, while bus cycles are traced."""

        __match_args__ = (
            "now",
            "address",
            "size",
            "function_code",
            "write",
            "value",
            "bus_error",
        )
        def __new__(
            cls,
            now: int,
            address: int,
            size: int,
            function_code: FunctionCode,
            write: bool,
            value: int,
            bus_error: bool,
        ) -> Event.BusCycle: ...
        @property
        def now(self) -> int: ...
        @property
        def address(self) -> int: ...
        @property
        def size(self) -> int: ...
        @property
        def function_code(self) -> FunctionCode: ...
        @property
        def write(self) -> bool: ...
        @property
        def value(self) -> int: ...
        @property
        def bus_error(self) -> bool: ...

    @final
    class Exception(Event):
        """An exception entered, while exceptions are traced."""

        __match_args__ = ("now", "vector", "pc", "handler", "stack", "level")
        def __new__(
            cls, now: int, vector: int, pc: int, handler: int, stack: int, level: int | None
        ) -> Event.Exception: ...
        @property
        def now(self) -> int: ...
        @property
        def vector(self) -> int: ...
        @property
        def pc(self) -> int: ...
        @property
        def handler(self) -> int: ...
        @property
        def stack(self) -> int: ...
        @property
        def level(self) -> int | None: ...

    @final
    class Dsack(Event):
        """A peripheral's DSACK pins disagree with the chip at `now`."""

        __match_args__ = ("now", "fault")
        def __new__(cls, now: int, fault: DsackFault) -> Event.Dsack: ...
        @property
        def now(self) -> int: ...
        @property
        def fault(self) -> DsackFault: ...

    @final
    class Unknown(Event):
        """An event the bindings cannot convert, at `now`: `text` is what
        happened, as Rust prints it."""

        __match_args__ = ("now", "text")
        def __new__(cls, now: int, text: str) -> Event.Unknown: ...
        @property
        def now(self) -> int: ...
        @property
        def text(self) -> str: ...

@final
class Builder:
    """The board as the host wires it, before the power-on reset."""

    def __new__(
        cls,
        *,
        data: int = ...,
        reference_hz: int | None = None,
        external_hz: int | None = None,
    ) -> Builder: ...
    def memory(
        self, contents: bytes | bytearray, pins: Sequence[str], *, writable: bool = True
    ) -> MemoryId: ...
    def fixed_memory(
        self, contents: bytes | bytearray, base: int, *, writable: bool = True
    ) -> MemoryId: ...
    def set_fixed_timing(
        self, memory: MemoryId, *, port: int = 16, termination: Termination | None = None
    ) -> None: ...
    def set_reload_scope(self, memory: MemoryId, scope: Sequence[ResetCause]) -> None: ...
    def peripheral(self, peripheral: MappedPeripheral, pins: Sequence[str]) -> PeripheralId: ...
    def bus_peripheral(
        self,
        peripheral: MappedPeripheral,
        *,
        windows: Sequence[tuple[int, int]] = ...,
        acknowledges: Sequence[int] = ...,
        dsack: Dsack,
    ) -> PeripheralId: ...
    def unwired_peripheral(self, peripheral: object) -> PeripheralId: ...
    def set_qspi(self, peripheral: SpiPeripheral) -> None: ...
    def set_dsack(self, peripheral: PeripheralId, dsack: Dsack) -> None: ...
    def set_qspi_reset_scope(self, scope: Sequence[ResetCause]) -> None: ...
    def stop_request(self) -> StopRequest: ...
    def sync_request(self) -> SyncRequest: ...
    def diversion(self) -> Diversion: ...
    def set_interrupt(self, peripheral: PeripheralId, pin: int) -> None: ...
    def set_reset_scope(self, peripheral: PeripheralId, scope: Sequence[ResetCause]) -> None: ...
    def set_autovector(self, pin: int) -> None: ...
    def build(self) -> Mc68331: ...

@final
class Mc68331:
    """The chip."""

    def run_for(self, budget: int) -> Run: ...
    def run_until(self, deadline: int) -> Run: ...
    def serve_gdb(
        self, address: str = ..., ready: Callable[[int], object] | None = None
    ) -> Ending: ...
    def bus_read(self, address: int, size: int, function_code: FunctionCode = ...) -> int: ...
    def bus_write(self, address: int, size: int, value: int, function_code: FunctionCode = ...) -> None: ...
    def peek(self, address: int, size: int, function_code: FunctionCode = ...) -> int: ...
    def poke(
        self, address: int, size: int, value: int, function_code: FunctionCode = ...
    ) -> None: ...
    def reset(self) -> None: ...
    def reset_for(self, cause: ResetCause) -> None: ...
    def reset_with_breakpoint(self) -> None: ...
    def assert_breakpoint(self) -> bool: ...
    def halt(self) -> Entry: ...
    def go(self) -> None: ...
    def step(self) -> Entry: ...
    @property
    def now(self) -> int: ...
    @property
    def halted(self) -> Halt | None: ...
    @property
    def background(self) -> Entry | None: ...
    @property
    def exception_taken(self) -> int | None: ...
    def read_register(self, name: str) -> int: ...
    def write_register(self, name: str, value: int) -> None:
        """Writes register `name`; `OverflowError` for a value outside 0
        to 2**32 - 1, `ValueError` for no register `name`."""
    def registers(self) -> dict[str, int]: ...
    def memory(self, memory: MemoryId, offset: int = 0, length: int | None = None) -> bytes: ...
    def set_memory(self, memory: MemoryId, offset: int, data: bytes | bytearray) -> None: ...
    def peripheral(self, peripheral: PeripheralId) -> object: ...
    def set_interrupt_pin(self, pin: int, asserted: bool) -> None: ...
    def set_pins(self, port: Port, driven: int, levels: int) -> None: ...
    def outputs(self, port: Port) -> tuple[int, int]: ...
    def port(self, port: Port) -> PortState: ...
    def take_port_changes(self) -> list[tuple[int, Port, int, int]]: ...
    @property
    def system_hz(self) -> int: ...
    def qspi_peripheral(self) -> object | None: ...
    def sci_receive(self, frame: Frame) -> None: ...
    def take_sci_transmitted(self) -> list[tuple[int, Frame]]: ...
    @property
    def qspi_chip_selects(self) -> int: ...
    def set_qspi_slave_select(self, asserted: bool) -> None: ...
    def qspi_slave_shift(self, mosi: int, bits: int, period: int) -> tuple[int, int]: ...
    def set_gpt_input(self, input: Input, level: bool) -> None: ...
    def schedule_gpt_input(self, at: int, input: Input, level: bool) -> None: ...
    def pwm(self, channel: Channel) -> Output: ...
    def oc1(self) -> Output: ...
    def pwm_level(self, channel: Channel) -> bool: ...
    def take_pwm_changes(self) -> list[tuple[int, Channel, bool]]: ...
    def take_output_changes(
        self,
    ) -> list[
        tuple[int, Port, int, int] | tuple[int, Channel, bool] | tuple[int, None, str]
    ]: ...
    def set_report_pwm_edges(self, channel: Channel, report: bool) -> None: ...
    def set_report_oc1_clock_edges(self, report: bool) -> None: ...
    def take_events(self) -> list[Event]: ...
    def set_tracing(self, *, bus_cycles: bool = False, exceptions: bool = False) -> None: ...
    @property
    def instructions(self) -> int: ...
    def add_code_hook(
        self, callback: Callable[[Instruction], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_block_hook(
        self, callback: Callable[[Instruction], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_pre_read_hook(
        self,
        callback: Callable[[PendingRead, bytearray], object],
        begin: int = 0,
        end: int = ...,
    ) -> HookId: ...
    def add_read_hook(
        self, callback: Callable[[MemoryAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_write_hook(
        self, callback: Callable[[MemoryAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_invalid_hook(
        self, callback: Callable[[InvalidAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_exception_hook(self, callback: Callable[[ExceptionEntry], object]) -> HookId: ...
    def add_exception_return_hook(
        self, callback: Callable[[ExceptionReturn], object]
    ) -> HookId: ...
    def remove_hook(self, hook: HookId) -> bool: ...
    def set_stop_on_fault(self, stop: bool) -> None: ...
    @property
    def stops_on_fault(self) -> bool: ...
    @property
    def fault_stop(self) -> FaultStop | None: ...
    def protect(self, space: Space, start: int, length: int, permissions: str) -> None: ...
    def save(self) -> bytes: ...
    def restore(self, snapshot: bytes | bytearray) -> None: ...

@final
class BoardChip:
    """A board's chip, as `Board.chip` gives it: the `Mc68331` methods
    that read or hook the chip the board runs."""

    def bus_read(self, address: int, size: int, function_code: FunctionCode = ...) -> int: ...
    def bus_write(self, address: int, size: int, value: int, function_code: FunctionCode = ...) -> None: ...
    def peek(self, address: int, size: int, function_code: FunctionCode = ...) -> int: ...
    def poke(
        self, address: int, size: int, value: int, function_code: FunctionCode = ...
    ) -> None: ...
    @property
    def now(self) -> int: ...
    @property
    def halted(self) -> Halt | None: ...
    @property
    def background(self) -> Entry | None: ...
    @property
    def exception_taken(self) -> int | None: ...
    @property
    def instructions(self) -> int: ...
    def read_register(self, name: str) -> int: ...
    def write_register(self, name: str, value: int) -> None:
        """Writes register `name`; `OverflowError` for a value outside 0
        to 2**32 - 1, `ValueError` for no register `name`."""
    def registers(self) -> dict[str, int]: ...
    def outputs(self, port: Port) -> tuple[int, int]: ...
    def port(self, port: Port) -> PortState: ...
    @property
    def system_hz(self) -> int: ...
    @property
    def qspi_chip_selects(self) -> int: ...
    def pwm(self, channel: Channel) -> Output: ...
    def oc1(self) -> Output: ...
    def pwm_level(self, channel: Channel) -> bool: ...
    def add_code_hook(
        self, callback: Callable[[Instruction], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_block_hook(
        self, callback: Callable[[Instruction], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_pre_read_hook(
        self,
        callback: Callable[[PendingRead, bytearray], object],
        begin: int = 0,
        end: int = ...,
    ) -> HookId: ...
    def add_read_hook(
        self, callback: Callable[[MemoryAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_write_hook(
        self, callback: Callable[[MemoryAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_invalid_hook(
        self, callback: Callable[[InvalidAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_exception_hook(self, callback: Callable[[ExceptionEntry], object]) -> HookId: ...
    def add_exception_return_hook(
        self, callback: Callable[[ExceptionReturn], object]
    ) -> HookId: ...
    def remove_hook(self, hook: HookId) -> bool: ...
    def set_stop_on_fault(self, stop: bool) -> None: ...
    @property
    def stops_on_fault(self) -> bool: ...
    @property
    def fault_stop(self) -> FaultStop | None: ...
    def protect(self, space: Space, start: int, length: int, permissions: str) -> None: ...

@final
class FaultCause:
    """Why a fault was one."""

    BUS_ERROR: ClassVar[FaultCause]
    PROTECTED: ClassVar[FaultCause]
    ADDRESS_ERROR: ClassVar[FaultCause]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@final
class FaultStop:
    """A fault that stopped a run (`set_stop_on_fault`), as `End.Fault`
    and a chip's `fault_stop` carry it; `str()` is Rust's `Display`,
    `bus error: write of 2 bytes at 0x90000000 in supervisor-data, pc
    0x00001000`."""

    def __hash__(self) -> int: ...
    @property
    def kind(self) -> BusCycleKind: ...
    @property
    def cause(self) -> FaultCause: ...
    @property
    def address(self) -> int: ...
    @property
    def size(self) -> int: ...
    @property
    def function_code(self) -> FunctionCode: ...
    @property
    def pc(self) -> int: ...
    @property
    def registers(self) -> dict[str, int]: ...

@final
class Intercepted:
    """An exception an instruction raised that the host handles, before
    anything is stacked."""

    @property
    def vector(self) -> int: ...
    @property
    def pc(self) -> int: ...

@final
class StopRequest:
    """The handle by which the host asks a run to stop."""

    def request(self) -> None: ...
    def withdraw(self) -> None: ...

@final
class SyncRequest:
    """The handle by which the host's peripherals end a chip's run
    early."""

    def request(self) -> None: ...

@final
class Diversion:
    """The handle by which the host's peripherals divert a memory's
    cycles to a peripheral and back, by their ids."""

    def divert(self, memory: MemoryId, peripheral: PeripheralId) -> None: ...
    def release(self, memory: MemoryId) -> None: ...

@final
class Dsack:
    """The DSACK pins a peripheral drives (SIMRM Table 5-2)."""

    DSACK0: ClassVar[Dsack]
    DSACK1: ClassVar[Dsack]
    BOTH: ClassVar[Dsack]
    def __hash__(self) -> int: ...
    @property
    def name(self) -> str: ...

@disjoint_base
class Termination:
    """How a select would end a cycle it matches (SIMRM 7.5, 7.7), a
    class per variant."""

    def __hash__(self) -> int: ...

    @final
    class Fast(Termination):
        """Internal DSACK after two clocks."""

        __match_args__ = ()
        def __new__(cls) -> Termination.Fast: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class WaitStates(Termination):
        """Internal DSACK after `wait_states` wait states, 0 to 13."""

        __match_args__ = ("wait_states",)
        def __new__(cls, wait_states: int) -> Termination.WaitStates: ...
        @property
        def wait_states(self) -> int: ...

    @final
    class External(Termination):
        """The peripheral asserts DSACK."""

        __match_args__ = ()
        def __new__(cls) -> Termination.External: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Eclk(Termination):
        """MODE set: the cycle runs on ECLK."""

        __match_args__ = ()
        def __new__(cls) -> Termination.Eclk: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

@disjoint_base
class DsackFault:
    """How a peripheral's DSACK pins disagree with the chip."""

    def __hash__(self) -> int: ...

    @final
    class Unassigned(DsackFault):
        """PEPAR assigns none of the pins it drives as DSACK (SIMRM 9.1)."""

        __match_args__ = ("select",)
        def __new__(cls, select: str) -> DsackFault.Unassigned: ...
        @property
        def select(self) -> str: ...

    @final
    class BusUnassigned(DsackFault):
        """The same for a peripheral on no select (`bus_peripheral`)."""

        __match_args__ = ("peripheral",)
        def __new__(cls, peripheral: PeripheralId) -> DsackFault.BusUnassigned: ...
        @property
        def peripheral(self) -> PeripheralId: ...

@final
class Machine:
    """A bare CPU32 on flat memory."""

    def __new__(cls, *, address_bits: int = ...) -> Machine: ...
    def map(
        self,
        start: int,
        length: int,
        *,
        permissions: str = "rwx",
        contents: bytes | bytearray | None = None,
    ) -> None: ...
    def unmap(self, start: int, length: int) -> None: ...
    def protect(self, start: int, length: int, permissions: str) -> None: ...
    def read(self, address: int, length: int) -> bytes: ...
    def write(self, address: int, data: bytes | bytearray) -> None: ...
    def add_peripheral(
        self, begin: int, end: int, peripheral: MappedPeripheral
    ) -> PeripheralId: ...
    def remove_peripheral(self, peripheral: PeripheralId) -> bool: ...
    def read_register(self, name: str) -> int: ...
    def write_register(self, name: str, value: int) -> None:
        """Writes register `name`; `OverflowError` for a value outside 0
        to 2**32 - 1, `ValueError` for no register `name`."""
    def registers(self) -> dict[str, int]: ...
    def reset(self) -> None: ...
    def request_interrupt(self, pin: int, vector: int | None = None) -> None: ...
    def clear_interrupt(self, pin: int) -> None: ...
    def take_exception(self, vector: int, level: int | None = None) -> None: ...
    def run_limited(
        self,
        *,
        begin: int | None = None,
        reach: int | None = None,
        instructions: int | None = None,
        cycles: int | None = None,
    ) -> LimitedRun: ...
    def stop_request(self) -> StopRequest: ...
    def serve_gdb(
        self, address: str = ..., ready: Callable[[int], object] | None = None
    ) -> Ending: ...
    def add_intercept_hook(
        self, callback: Callable[[Intercepted], object], vectors: Iterable[int] | None
    ) -> HookId: ...
    def set_stop_on_fault(self, stop: bool) -> None: ...
    @property
    def stops_on_fault(self) -> bool: ...
    def set_stop_on_exception(self, vectors: Iterable[int] | None) -> None: ...
    @property
    def stops_on_exception(self) -> frozenset[int]: ...
    @property
    def instructions(self) -> int: ...
    @property
    def now(self) -> int: ...
    @property
    def halted(self) -> Halt | None: ...
    @property
    def exception_taken(self) -> int | None: ...
    def add_code_hook(
        self, callback: Callable[[Instruction], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_block_hook(
        self, callback: Callable[[Instruction], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_pre_read_hook(
        self,
        callback: Callable[[PendingRead, bytearray], object],
        begin: int = 0,
        end: int = ...,
    ) -> HookId: ...
    def add_read_hook(
        self, callback: Callable[[MemoryAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_write_hook(
        self, callback: Callable[[MemoryAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_invalid_hook(
        self, callback: Callable[[InvalidAccess], object], begin: int = 0, end: int = ...
    ) -> HookId: ...
    def add_exception_hook(self, callback: Callable[[ExceptionEntry], object]) -> HookId: ...
    def add_exception_return_hook(
        self, callback: Callable[[ExceptionReturn], object]
    ) -> HookId: ...
    def remove_hook(self, hook: HookId) -> bool: ...
    def save(self) -> bytes: ...
    def restore(self, snapshot: bytes | bytearray) -> None: ...

@final
class PeripheralAccess:
    """A bus cycle as a board peripheral sees it, through one of its
    attachments (docs/board-files.md)."""

    @property
    def attachment(self) -> int:
        """The attachment it came through: its index in `attach`."""
    @property
    def select(self) -> str | None:
        """The chip select that carried it: `CSBOOT`, `CS0` to `CS10`;
        `None` on no select, the peripheral's own decode taking it."""
    @property
    def role(self) -> Role: ...
    @property
    def lane(self) -> Lane: ...
    @property
    def address(self) -> int:
        """The address on the peripheral's own address pins, plus the offset."""
    @property
    def size(self) -> int:
        """Bytes: 1 or 2."""
    @property
    def function_code(self) -> FunctionCode:
        """Its function code."""
    @property
    def write(self) -> bool:
        """Whether it is a write, R/W low; set for `wait_states` and
        `dsack_port` too."""
    @property
    def horizon(self) -> int | None:
        """On a read or write of a peripheral whose `wants_horizon()` is
        true, the board time in picoseconds before which everything that
        could reach it has; `None` otherwise."""

@final
class Transfer:
    """A QSPI transfer as a board peripheral sees it, through its attachment
    (docs/board-files.md)."""

    @property
    def attachment(self) -> int:
        """The attachment it came through: its index in `attach`."""
    @property
    def entry(self) -> int:
        """The queue entry, 0 to 15."""
    @property
    def chip_selects(self) -> int:
        """The levels on PCS[3:0]."""
    @property
    def out(self) -> int:
        """The bits shifted out, right-aligned."""
    @property
    def bits(self) -> int: ...
    @property
    def clock(self) -> QspiClock:
        """The serial clock it is shifted with: its mode and SCK period."""
    @property
    def starts_frame(self) -> bool:
        """Whether it is the first since the attachment's select asserted."""

@final
class TimeBase:
    """The chip's system clock on board time: for converting between
    picoseconds and the chip's clocks."""

    def __new__(cls, frequency_hz: int, *, cycle: int = 0, time: int = 0) -> TimeBase:
        """A base: clock `cycle` falls at board time `time` picoseconds,
        and the clock runs at `frequency_hz` from there."""

    @property
    def frequency_hz(self) -> int:
        """The clock's frequency, whole hertz."""
    def time(self, at: int) -> int:
        """The board time clock `at` falls at, in picoseconds, rounded
        down."""
    def cycle(self, at: int) -> int:
        """The first clock at or after board time `at` picoseconds."""

@final
class Moment:
    """A call's time and the chip's system clock then, as a board
    peripheral's calls with no `Drive` are given it."""

    def __new__(cls, now: int, system_clock: TimeBase) -> Moment:
        """The moment `now` picoseconds of board time, the chip's
        system clock on board time `system_clock`."""

    @property
    def now(self) -> int:
        """Board time, in picoseconds."""
    @property
    def system_clock(self) -> TimeBase:
        """The chip's system clock on board time, as of then."""
    def cycle(self) -> int:
        """The chip's clock at it, rounded up."""

@final
class Drive:
    """Where a board peripheral drives its outputs during a call."""

    @property
    def now(self) -> int:
        """The call's time, in picoseconds."""
    @property
    def moment(self) -> Moment:
        """The call's time and the chip's system clock then."""
    @property
    def system_clock(self) -> TimeBase:
        """The chip's system clock on board time at the call."""
    def drive(self, port: int, value: object) -> None: ...
    def drive_at(self, at: int, port: int, value: object) -> None: ...
    def write_memory(self, memory: str, offset: int, data: bytes | bytearray) -> None: ...
    def sample(self, port: int) -> float | None: ...
    def take_memory(self, memory: str) -> None: ...
    def release_memory(self, memory: str) -> None: ...
    def stop(self) -> None: ...

@final
class Serial:
    """A serial frame with its timing: the frame, the sender's bit rate
    in nanohertz and picoseconds from start bit to the end of the stop
    bits, both zero for an untimed frame."""

    def __new__(cls, frame: Frame, baud_nanohertz: int, bits: int) -> Serial: ...
    @staticmethod
    def timed(frame: Frame, baud_nanohertz: int, length: int) -> Serial: ...
    @staticmethod
    def untimed(frame: Frame) -> Serial: ...
    @property
    def frame(self) -> Frame: ...
    @property
    def baud_nanohertz(self) -> int: ...
    @property
    def length(self) -> int: ...
    def end(self, start: int) -> int: ...
    def __hash__(self) -> int: ...

@final
class External:
    """A run-time endpoint: its name, its nets' type (`serial`, `logic`
    and so on) and its direction, `out` if it drives a net, `in` if it
    is driven, `in-out` if both."""

    @property
    def name(self) -> str: ...
    @property
    def kind(self) -> str: ...
    @property
    def direction(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    def __hash__(self) -> int: ...

@final
class ObserverId:
    """An observer attached to a net, as `Board.observe` returns it;
    `str()` is `observer 2`."""

    def __hash__(self) -> int: ...

@disjoint_base
class Pacing:
    """How fast a paced run's board time may go against wall time, a
    class per variant."""

    @staticmethod
    def parse(text: str) -> Pacing:
        """`unpaced`, `realtime` or a factor, as the runner's `--pace`
        takes it."""
    @property
    def factor(self) -> float | None:
        """Board seconds a wall second, `None` unpaced."""
    def __eq__(self, other: object, /) -> bool: ...
    def __hash__(self) -> int: ...

    @final
    class Unpaced(Pacing):
        """As fast as the host runs it, the default."""

        __match_args__ = ()
        def __new__(cls) -> Pacing.Unpaced: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class RealTime(Pacing):
        """A board second a wall second."""

        __match_args__ = ()
        def __new__(cls) -> Pacing.RealTime: ...
        def __len__(self) -> int: ...
        def __getitem__(self, key: int, /) -> object: ...

    @final
    class Scaled(Pacing):
        """`factor` board seconds a wall second: finite and above 0, or
        building it raises `ValueError`."""

        __match_args__ = ("factor",)
        def __new__(cls, factor: float) -> Pacing.Scaled: ...
        @property
        def factor(self) -> float: ...

@final
class BoardControl:
    """A board's control handle, for another thread: a stop, inputs and
    the pacing, taken by the run at its sync points."""

    def stop(self) -> None: ...
    @property
    def stopping(self) -> bool: ...
    def send(self, name: str, value: object, *, kind: str = "serial") -> None: ...
    def set_pacing(self, pacing: Pacing) -> None: ...
    @property
    def pacing(self) -> Pacing: ...

@final
class BoardStopRequest:
    """The handle by which the host asks a board's run to stop, on the
    board's own thread: for an observer's callback."""

    def request(self) -> None: ...

@final
class Board:
    """A board file loaded and run (docs/board-files.md)."""

    def __new__(
        cls,
        path: str | PathLike[str],
        *,
        types: Mapping[str, Callable[..., Peripheral]] | None = None,
        enabled: Sequence[str] = ...,
        firmware: str | PathLike[str] | None = None,
        images: Mapping[str, str | PathLike[str]] | None = None,
        seed: int = 0,
        work_dir: str | PathLike[str] | None = None,
        parameters: Mapping[str, Mapping[str, object]] | None = None,
    ) -> Board: ...
    def run_for(
        self,
        *,
        seconds: float | None = None,
        picoseconds: int | None = None,
        pacing: Pacing | None = None,
        quantum_picoseconds: int = ...,
    ) -> BoardRun: ...
    def run_until(
        self,
        *,
        seconds: float | None = None,
        picoseconds: int | None = None,
        pacing: Pacing | None = None,
        quantum_picoseconds: int = ...,
    ) -> BoardRun: ...
    def run_frame(
        self,
        wall: float,
        *,
        seconds: float | None = None,
        picoseconds: int | None = None,
    ) -> BoardRun: ...
    def control(self) -> BoardControl: ...
    def stop_request(self) -> BoardStopRequest: ...
    def serve_gdb(
        self, address: str = ..., ready: Callable[[int], object] | None = None
    ) -> Ending: ...
    @property
    def now(self) -> int: ...
    @property
    def instructions(self) -> int: ...
    def peripheral(self, name: str) -> object | None: ...
    @property
    def chip(self) -> BoardChip: ...
    def reset(self) -> None: ...
    def reset_for(self, cause: ResetCause) -> None: ...
    def reset_with_breakpoint(self) -> None: ...
    def assert_breakpoint(self) -> bool: ...
    def halt(self) -> Entry: ...
    def go(self) -> None: ...
    def take_events(self) -> list[Event]: ...
    def marking(self, name: str) -> str | None: ...
    def inspect(self, name: str) -> dict[str, object]: ...
    def memory(self, name: str) -> bytes: ...
    def externals(self) -> list[External]: ...
    def send(self, name: str, value: object, *, kind: str = "serial") -> None: ...
    def take_received(self, name: str) -> list[tuple[int, object]]: ...
    def nets(self) -> list[tuple[str, str]]: ...
    def net_value(self, name: str) -> object | None: ...
    def observe(
        self,
        name: str,
        callback: Callable[[str, int, object], object],
        *,
        on_restore: Callable[[str, int, object | None], object] | None = None,
    ) -> ObserverId: ...
    def unobserve(self, observer: ObserverId) -> bool: ...
    def set_tracing(
        self,
        *,
        bus_cycles: bool = False,
        exceptions: bool = False,
        pins: bool = False,
        qspi: bool = False,
        nets: bool = False,
    ) -> None: ...
    def watch_code(self, pc: int) -> HookId: ...
    def take_trace(self) -> list[str]: ...
    def save(self) -> bytes: ...
    def restore(self, snapshot: bytes | bytearray) -> None: ...
