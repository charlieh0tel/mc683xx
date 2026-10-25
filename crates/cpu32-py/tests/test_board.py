"""A board file from Python: Python classes registered as peripheral types
by name, a peripheral on a select driving a net into another, and the run's
trace."""

import struct
from pathlib import Path
from types import MappingProxyType

import pytest

import mc683xx

# The SIM's registers with MM set (MC68331UM D.3); CSBAR1 and CSOR1
# (MC68331UM Table D-3).
SIM = 0xFFFA00
CSBAR1, CSOR1 = SIM + 0x50, SIM + 0x52
# CS1 at $200000, 64 Kbytes; both bytes, read and write, fast
# termination (MC68331UM D.3.25, D.3.26).
CS1_BAR = 0x2003
CSOR_FAST = 0x7BB0
LATCH = 0x200000
ROM_BYTES = 64 << 10
STACK = 0x10FFFE
START = 0x400
NOP = 0x4E71
MOVE_WORD_IMMEDIATE_TO_ABSOLUTE = 0x33FC


def store_word(address: int, value: int) -> list[int]:
    return [MOVE_WORD_IMMEDIATE_TO_ABSOLUTE, value, address >> 16, address & 0xFFFF]


def rom(program: list[int]) -> bytes:
    image = bytearray(struct.pack(">H", NOP) * (ROM_BYTES // 2))
    image[0:8] = struct.pack(">II", STACK, START)
    for index, word in enumerate(program):
        image[START + 2 * index : START + 2 * index + 2] = struct.pack(">H", word)
    return bytes(image)


BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "latch"
type = "latch"
attach = [{ select = "CS1", role = "write" }]
parameters = { width = 1 }
[[peripheral]]
name = "lamp"
type = "lamp"
marking = "DS1, LED"
[[net]]
name = "lamp-on"
type = "logic"
from = "latch.q0"
to = ["lamp.on"]
"""


class Latch:
    """A write-only latch whose bit 0 drives `q0`."""

    PORTS = [("q0", "logic", "out")]

    def __init__(self, width: int) -> None:
        self.width = width
        self.written: list[int] = []
        self.accesses: list[tuple[str, mc683xx.Role, mc683xx.Lane]] = []

    def write(self, access: mc683xx.PeripheralAccess, value: int, drive: mc683xx.Drive) -> None:
        self.written.append(value)
        self.accesses.append((access.select, access.role, access.lane))
        drive.drive(0, bool(value & 1))


class Lamp:
    """A lamp on a logic input."""

    PORTS = [("on", "logic", "in")]

    def __init__(self) -> None:
        self.changes: list[tuple[int, bool]] = []

    def input(self, port: int, value: object, now: int, drive: mc683xx.Drive) -> None:
        assert isinstance(value, bool)
        self.changes.append((now, value))


def test_python_peripherals_on_a_board_file(tmp_path: Path) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST) + store_word(LATCH, 1)
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    board.set_tracing(bus_cycles=True)
    run = board.run_for(seconds=0.0001)
    assert run.stop == mc683xx.Stop.Budget()
    assert 0 < board.instructions < board.cycles
    latch = board.peripheral("latch")
    assert isinstance(latch, Latch)
    assert latch.width == 1
    assert latch.written == [1]
    assert latch.accesses == [("CS1", mc683xx.Role.WRITE, mc683xx.Lane.BOTH)]
    lamp = board.peripheral("lamp")
    assert isinstance(lamp, Lamp)
    assert [level for _, level in lamp.changes] == [True]
    assert board.marking("lamp") == "DS1, LED"
    assert board.marking("latch") is None
    trace = board.take_trace()
    assert any(
        line.startswith("mc68331: bus-cycle ")
        and line.endswith("access=write address=0x00200000 size=2 value=0x0001")
        for line in trace
    )
    assert board.memory("rom")[:8] == struct.pack(">II", STACK, START)


def test_ports_are_name_kind_direction(tmp_path: Path) -> None:
    class Reversed(Lamp):
        PORTS = [("on", "in", "logic")]

    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    with pytest.raises(TypeError, match="^on: "):
        mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Reversed})


def test_a_watched_pc_is_a_hook_the_chip_removes(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)

    def traced(watch: bool, remove: bool) -> list[str]:
        board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
        if watch:
            hook = board.watch_code(START)
            assert isinstance(hook, mc683xx.HookId)
            if remove:
                assert board.chip.remove_hook(hook)
        board.run_for(seconds=0.0001)
        return board.take_trace()

    assert len(traced(True, False)) > len(traced(False, False))
    assert traced(True, True) == traced(False, False)


def test_a_board_s_chip_reads_and_hooks_as_a_chip_does(tmp_path: Path) -> None:
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST) + store_word(LATCH, 1)
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    chip = board.chip
    assert isinstance(chip, mc683xx.BoardChip)
    seen: list[int] = []
    hook = chip.add_code_hook(lambda instruction: seen.append(instruction.pc), START, START)
    board.run_for(seconds=0.0001)
    assert seen == [START]
    assert chip.remove_hook(hook)
    assert chip.read_register("SSP") <= STACK
    assert chip.registers()["PC"] == chip.read_register("PC")
    assert chip.peek(CSBAR1, 2) == CS1_BAR
    assert chip.halted is None
    assert chip.instructions == board.instructions
    assert chip.now == board.cycles
    assert chip.system_hz == 8_388_608
    chip.write_register("D0", 7)
    assert board.chip.read_register("D0") == 7
    assert isinstance(board.take_events(), list)


ANALOG_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "oven"
type = "oven"
[[peripheral]]
name = "probe"
type = "probe"
[[net]]
name = "temperature"
type = "analog"
from = "oven.out"
to = ["probe.in"]
"""


class Oven:
    """A source whose value moves with time, sampled, never driven."""

    PORTS = [("out", "analog", "out")]

    def sample(self, port: int, now: int) -> float | None:
        return 20.0 + now / 1e9

    def save(self) -> bytes:
        return b""

    def restore(self, snapshot: bytes) -> None:
        assert snapshot == b""


class Probe:
    """A sink that samples its input every 10 us and can save."""

    PORTS = [("in", "analog", "in")]
    PERIOD = 10_000_000

    def __init__(self) -> None:
        self.next = self.PERIOD
        self.readings: list[float | None] = []

    def next_event(self) -> int | None:
        return self.next

    def advance(self, now: int, drive: mc683xx.Drive) -> None:
        self.readings.append(drive.sample(0))
        self.next = now + self.PERIOD

    def save(self) -> bytes:
        return struct.pack(">QI", self.next, len(self.readings))

    def restore(self, snapshot: bytes) -> None:
        self.next, count = struct.unpack(">QI", snapshot)
        del self.readings[count:]


class Unsaved:
    """A sink with no snapshot."""

    PORTS = [("in", "analog", "in")]


def test_python_peripherals_sample_and_save(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(ANALOG_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"oven": Oven, "probe": Probe})
    board.run_for(picoseconds=25_000_000)
    probe = board.peripheral("probe")
    assert isinstance(probe, Probe)
    assert probe.readings == [20.01, 20.02]
    without = mc683xx.Board(str(tmp_path / "board.toml"), types={"oven": Oven, "probe": Unsaved})
    try:
        without.save()
        raise AssertionError("a peripheral without save cannot save")
    except mc683xx.StateError as error:
        assert str(error) == "probe: a Python peripheral cannot save its state", error
    snapshot = board.save()
    board.run_for(picoseconds=20_000_000)
    assert len(probe.readings) == 4
    board.restore(snapshot)
    assert len(probe.readings) == 2 and probe.next == 30_000_000
    board.run_for(picoseconds=10_000_000)
    assert probe.readings == [20.01, 20.02, 20.03]


class SaveOnlyProbe:
    """A sink with `save` but no `restore`."""

    PORTS = [("in", "analog", "in")]

    def save(self) -> bytes:
        return b""


class InterruptedProbe(Probe):
    """A probe whose restore is interrupted."""

    def restore(self, snapshot: bytes) -> None:
        raise KeyboardInterrupt


def test_a_python_peripheral_with_save_alone_cannot_save(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(ANALOG_BOARD)
    path = str(tmp_path / "board.toml")
    saving = mc683xx.Board(path, types={"oven": Oven, "probe": Probe})
    board = mc683xx.Board(path, types={"oven": Oven, "probe": SaveOnlyProbe})
    with pytest.raises(mc683xx.StateError, match="^probe: a Python peripheral cannot save"):
        board.save()
    with pytest.raises(mc683xx.StateError, match="^probe: a Python peripheral cannot save"):
        board.restore(saving.save())


def test_a_keyboard_interrupt_in_a_restore_is_raised_and_changes_nothing(
    tmp_path: Path,
) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(ANALOG_BOARD)
    board = mc683xx.Board(
        str(tmp_path / "board.toml"), types={"oven": Oven, "probe": InterruptedProbe}
    )
    board.run_for(picoseconds=25_000_000)
    snapshot = board.save()
    board.run_for(picoseconds=20_000_000)
    before = board.save()
    with pytest.raises(KeyboardInterrupt):
        board.restore(snapshot)
    assert board.save() == before


# The QSM's registers with MM set (MC68331UM Table D-1): PORTQS, PQSPAR,
# SPCR0 to SPCR2, transmit and command RAM (QSMRM 4.2).
QSM = 0xFFFC00
PORTQS, PQSPAR = QSM + 0x15, QSM + 0x16
SPCR0, SPCR1, SPCR2 = QSM + 0x18, QSM + 0x1A, QSM + 0x1C
TRANSMIT_RAM, COMMAND_RAM = QSM + 0x120, QSM + 0x140
MOVE_BYTE_IMMEDIATE_TO_ABSOLUTE = 0x13FC
# The queue's one entry: NEWQP and ENDQP 3 (QSMRM 4.2.4).
ENTRY = 3


def store_byte(address: int, value: int) -> list[int]:
    return [MOVE_BYTE_IMMEDIATE_TO_ABSOLUTE, value, address >> 16, address & 0xFFFF]


# PCS[3:0] outputs, high; entry 3 to PCS 0101, 8 bits, at SPBR 2; then
# SPE (QSMRM 4.3).
QSPI_PROGRAM = (
    store_byte(PORTQS, 0x78)
    + store_word(PQSPAR, 0x7878)
    + store_word(TRANSMIT_RAM + 2 * ENTRY, 0x1234)
    + store_byte(COMMAND_RAM + ENTRY, 0b0101)
    + store_word(SPCR2, ENTRY << 8 | ENTRY)
    + store_word(SPCR0, 0x8002)
    + store_word(SPCR1, 0x8000)
)

QSPI_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "flash"
type = "flash"
attach = [{ qspi = 0b0101 }]
"""


class EntryFlash:
    """A QSPI peripheral that keeps each transfer's queue entry."""

    def __init__(self) -> None:
        self.transfers: list[tuple[int, int, int, int, bool]] = []

    def transfer(self, transfer: mc683xx.Transfer, drive: mc683xx.Drive) -> int:
        self.transfers.append(
            (transfer.attachment, transfer.entry, transfer.out, transfer.bits, transfer.starts_frame)
        )
        return 0xA5


def test_a_qspi_peripheral_is_told_the_entry(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom(QSPI_PROGRAM))
    (tmp_path / "board.toml").write_text(QSPI_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"flash": EntryFlash})
    board.run_for(seconds=0.0001)
    flash = board.peripheral("flash")
    assert isinstance(flash, EntryFlash)
    assert flash.transfers == [(0, ENTRY, 0x34, 8, True)]


# SYNCR (MC68331UM Table D-1): $CF00, 16.777216 MHz.
SYNCR, SYNCR_FAST = 0xFFFA04, 0xCF00
# A wait: MOVEQ #100,D3; DBRA D3,* (the queue's entry ends in it).
WAIT = [0x7000 | 3 << 9 | 100, 0x51CB, 0xFFFE]
# The queue again, after the wait, with CPOL and CPHA set and the
# system clock at 16.777216 MHz (QSMRM 4.3.1).
QSPI_PROGRAM_AGAIN = (
    QSPI_PROGRAM
    + WAIT
    + store_word(SYNCR, SYNCR_FAST)
    + store_word(SPCR0, 0x8302)
    + store_word(SPCR1, 0x8000)
)


class ClockedFlash:
    """A QSPI peripheral that keeps the clocks it is told and each transfer's."""

    def __init__(self) -> None:
        self.told: list[tuple[mc683xx.ClockPolarity, mc683xx.ClockPhase, int]] = []
        self.transfers: list[tuple[mc683xx.ClockPolarity, mc683xx.ClockPhase, int]] = []

    def qspi_clock(self, clock: mc683xx.QspiClock, now: int) -> None:
        self.told.append((clock.polarity, clock.phase, clock.system_clock_hz))

    def transfer(self, transfer: mc683xx.Transfer, drive: mc683xx.Drive) -> int:
        clock = transfer.clock
        self.transfers.append((clock.polarity, clock.phase, clock.system_clock_hz))
        return 0


def test_a_qspi_peripheral_is_told_the_clock_and_each_transfer_carries_it(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom(QSPI_PROGRAM_AGAIN))
    (tmp_path / "board.toml").write_text(QSPI_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"flash": ClockedFlash})
    board.run_for(seconds=0.001)
    flash = board.peripheral("flash")
    assert isinstance(flash, ClockedFlash)
    expected = [
        (mc683xx.ClockPolarity.IDLE_LOW, mc683xx.ClockPhase.CAPTURE_LEADING, 8_388_608),
        (mc683xx.ClockPolarity.IDLE_HIGH, mc683xx.ClockPhase.CHANGE_LEADING, 16_777_216),
    ]
    assert flash.told == expected
    assert flash.transfers == expected


class TimedFlash:
    """A QSPI peripheral that keeps each transfer's time and says whether its
    calls may come late."""

    DEFERRABLE = False

    def __init__(self) -> None:
        self.transfers: list[tuple[int, int]] = []
        self.asked = 0

    def qspi_deferrable(self) -> bool:
        self.asked += 1
        return self.DEFERRABLE

    def transfer(self, transfer: mc683xx.Transfer, drive: mc683xx.Drive) -> int:
        self.transfers.append((transfer.out, transfer.now))
        return 0xA5


class DeferrableFlash(TimedFlash):
    DEFERRABLE = True


def test_a_deferrable_qspi_peripheral_sees_the_same_transfers(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom(QSPI_PROGRAM))
    (tmp_path / "board.toml").write_text(QSPI_BOARD)
    seen = []
    for kind in (TimedFlash, DeferrableFlash):
        board = mc683xx.Board(str(tmp_path / "board.toml"), types={"flash": kind})
        board.run_for(seconds=0.0001)
        flash = board.peripheral("flash")
        assert isinstance(flash, kind)
        assert flash.asked == 1, "asked once, as the board is built"
        seen.append(flash.transfers)
    assert seen[0] == seen[1]
    assert [out for out, _ in seen[0]] == [0x34]


SERIAL_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "modem"
type = "modem"
[[net]]
name = "to-modem"
type = "serial"
from = "external.host"
to = ["modem.rx"]
[[net]]
name = "from-modem"
type = "serial"
from = "modem.tx"
to = ["external.echo"]
"""


class TimedModem:
    """Echoes each frame, its timing kept, and records what it got."""

    PORTS = [("rx", "serial", "in"), ("tx", "serial", "out")]
    TIMED_SERIAL = True

    def __init__(self) -> None:
        self.received: list[object] = []

    def input(self, port: int, value: object, now: int, drive: mc683xx.Drive) -> None:
        self.received.append(value)
        drive.drive(1, value)


class Modem(TimedModem):
    """As `TimedModem`, but told frames' data alone."""

    TIMED_SERIAL = False


def test_serial_values_carry_their_timing(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(SERIAL_BOARD)
    baud = 9_600 * 10**9
    frame = mc683xx.Serial(mc683xx.Frame.Data(0x41), baud, 10)
    assert (frame.frame, frame.baud_nanohertz, frame.length) == (mc683xx.Frame.Data(0x41), baud, 1_041_666_666)
    assert frame == mc683xx.Serial.timed(mc683xx.Frame.Data(0x41), baud, 1_041_666_666)
    assert frame.end(5) == 5 + frame.length
    assert mc683xx.Serial.untimed(mc683xx.Frame.Break()).length == 0
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"modem": TimedModem})
    echo, host = board.externals()
    assert (echo.name, echo.kind, echo.direction) == ("echo", "serial", "in")
    assert (host.name, host.kind, host.direction) == ("host", "serial", "out")
    assert repr(host) == 'External(name="host", kind="serial", direction="out")'
    # as the names alone, which it once gave
    assert board.externals() == ["echo", "host"]
    assert str(host) == "host" and "host" in {host}
    board.send("host", frame)
    board.send("host", 0x42)
    with pytest.raises(ValueError, match="external.echo drives no net"):
        board.send("echo", 0x42)
    with pytest.raises(ValueError, match="drives serial nets only"):
        board.send("host", True, kind="logic")
    board.run_for(picoseconds=1_000_000)
    modem = board.peripheral("modem")
    assert isinstance(modem, TimedModem)
    assert modem.received == [frame, mc683xx.Serial.untimed(mc683xx.Frame.Data(0x42))]
    received = board.take_received("echo", timed=True)
    assert [value for _, value in received] == modem.received
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"modem": Modem})
    board.send("host", frame)
    board.send("host", None)
    board.run_for(picoseconds=1_000_000)
    untimed = board.peripheral("modem")
    assert isinstance(untimed, Modem)
    assert untimed.received == [0x41, None]
    assert [value for _, value in board.take_received("echo")] == [0x41, None]
    with pytest.raises(ValueError, match="no net drives external.host"):
        board.take_received("host")


def test_a_run_sets_parameters_over_the_files(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    types = {"latch": Latch, "lamp": Lamp}
    path = str(tmp_path / "board.toml")
    board = mc683xx.Board(path, types=types, parameters={"latch": {"width": 3}})
    latch = board.peripheral("latch")
    assert isinstance(latch, Latch)
    assert latch.width == 3
    try:
        mc683xx.Board(path, types=types, parameters={"lump": {"width": 3}})
        raise AssertionError("no peripheral lump")
    except ValueError as error:
        assert "lump" in str(error)


class Failing:
    """A peripheral whose questions, asked with no `Drive`, raise."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self, **parameters: object) -> None:
        self.calls = 0

    def interrupt_request(self) -> bool:
        self.calls += 1
        raise KeyError("no interrupt")

    def next_event(self) -> int | None:
        self.calls += 1
        raise KeyError("no next event")


FAILING_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "failing"
type = "failing"
"""


def test_a_peripheral_s_failed_question_stops_the_run_at_once(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(FAILING_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"failing": Failing})
    peripheral = board.peripheral("failing")
    assert isinstance(peripheral, Failing)
    start, calls = board.now, peripheral.calls
    try:
        board.run_for(seconds=1.0)
        raise AssertionError("run should raise")
    except KeyError:
        pass
    # Stopped within a microsecond of board time, a few questions in.
    assert board.now - start < 1_000_000
    assert peripheral.calls - calls < 100


@pytest.mark.parametrize("seconds", [float("nan"), float("inf"), float("-inf"), -1e-6])
def test_a_run_refuses_seconds_not_finite_or_negative(tmp_path: Path, seconds: float) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    with pytest.raises(ValueError, match="finite number of seconds"):
        board.run_for(seconds=seconds)
    assert board.now == 0
    assert board.run_for(seconds=0.0).stop == mc683xx.Stop.Budget()


def test_run_until_runs_to_a_time_and_run_for_for_a_span(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})
    run = board.run_until(picoseconds=3_000_000)
    assert run.stop == mc683xx.Stop.Budget()
    assert run.now == 3_000_000
    assert board.now == 3_000_000
    board.run_for(picoseconds=2_000_000)
    assert board.now == 5_000_000
    # a time already passed: at once
    cycles = board.cycles
    assert board.run_until(picoseconds=1_000_000).now == 5_000_000
    assert board.cycles == cycles
    assert board.now == 5_000_000


def test_the_options_are_any_mappings(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(
        str(tmp_path / "board.toml"),
        types=MappingProxyType({"latch": Latch, "lamp": Lamp}),
        images=MappingProxyType({}),
        parameters=MappingProxyType({"latch": MappingProxyType({"width": 2})}),
    )
    latch = board.peripheral("latch")
    assert isinstance(latch, Latch)
    assert latch.width == 2


def test_a_board_of_another_chip_is_refused(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(rom([]))
    (tmp_path / "board.toml").write_text(BOARD.replace('type = "mc68331"', 'type = "mc68332"'))
    with pytest.raises(ValueError, match="chip type \"mc68332\""):
        mc683xx.Board(str(tmp_path / "board.toml"), types={"latch": Latch, "lamp": Lamp})


INTERRUPTED_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "register"
type = "interrupted"
attach = [{ select = "CS1" }]
"""
MOVE_WORD_ABSOLUTE_TO_D0 = 0x3039
BRA_BACK_8 = 0x60F8


class Interrupted:
    """Reads 0x1234, but its first read is interrupted by Ctrl-C."""

    PORTS: list[tuple[str, str, str]] = []

    def __init__(self) -> None:
        self.reads = 0

    def read(self, access: mc683xx.PeripheralAccess, drive: mc683xx.Drive) -> int:
        self.reads += 1
        if self.reads == 1:
            raise KeyboardInterrupt
        return 0x1234


def test_a_keyboard_interrupt_in_a_read_stops_the_run_without_a_bus_error(tmp_path: Path) -> None:
    # reads the register in a loop; a bus error would find no handler
    # (the vector a NOP pair, odd) and halt the CPU
    program = store_word(CSBAR1, CS1_BAR) + store_word(CSOR1, CSOR_FAST)
    program += [MOVE_WORD_ABSOLUTE_TO_D0, LATCH >> 16, LATCH & 0xFFFF, BRA_BACK_8]
    (tmp_path / "rom.bin").write_bytes(rom(program))
    (tmp_path / "board.toml").write_text(INTERRUPTED_BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"), types={"interrupted": Interrupted})
    with pytest.raises(KeyboardInterrupt):
        board.run_for(seconds=1.0)
    # the read answered undriven, 0
    assert board.chip.read_register("D0") & 0xFFFF == 0
    assert board.chip.halted is None
    assert board.run_for(seconds=0.001).stop == mc683xx.Stop.Budget()
    assert board.chip.read_register("D0") & 0xFFFF == 0x1234


SWITCHED_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "ram"
size = 0x800
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "failing"
type = "failing"
[[peripheral]]
name = "interrupted"
type = "interrupted"
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["failing.in", "interrupted.in"]
"""


class FailingInput:
    """Raises `RuntimeError` at its input."""

    PORTS = [("in", "logic", "in")]

    def input(self, port: int, value: object, now: int, drive: mc683xx.Drive) -> None:
        raise RuntimeError("failing")


class InterruptedInput:
    """Interrupted by Ctrl-C at its input."""

    PORTS = [("in", "logic", "in")]

    def input(self, port: int, value: object, now: int, drive: mc683xx.Drive) -> None:
        raise KeyboardInterrupt


def test_a_keyboard_interrupt_is_raised_before_an_earlier_peripheral_error(tmp_path: Path) -> None:
    (tmp_path / "board.toml").write_text(SWITCHED_BOARD)
    board = mc683xx.Board(
        str(tmp_path / "board.toml"),
        types={"failing": FailingInput, "interrupted": InterruptedInput},
    )
    board.control().send("switch", True, kind="logic")
    with pytest.raises(KeyboardInterrupt) as raised:
        board.run_for(seconds=0.001)
    assert isinstance(raised.value.__context__, RuntimeError)
    assert board.run_for(seconds=0.001).stop == mc683xx.Stop.Budget()
