"""Python's cycle collector reaches the objects a `Machine`, a `Builder`,
an `Mc68331` and a `Board` hold in Rust: a harness that holds its
target and hooks it with its own bound methods is collected."""

import gc
import struct
import weakref
from pathlib import Path
from typing import Any, Callable

import mc683xx

HARNESSES = 50
RAM_BYTES = 64 << 10
ROM_BYTES = 64 << 10
STACK = 0x8000
START = 0x400
PERIPHERAL = 0xF00000
# TRAP #0's vector (CPU32RM Table 6-1).
TRAP_0 = 32

BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[peripheral]]
name = "peripheral"
type = "peripheral"
"""

# A board with a net, for an observer.
NET_BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
[[net]]
name = "switch"
type = "logic"
from = "external.switch"
to = ["external.lamp"]
"""


def collected(make: Callable[[], object]) -> int:
    """How many of `HARNESSES` harnesses `make` builds are left once
    the collector has run."""
    gc.collect()
    alive = [weakref.ref(make()) for _ in range(HARNESSES)]
    gc.collect()
    return sum(reference() is not None for reference in alive)


class CpuHarness:
    """Unicorn's harness shape: the CPU, a peripheral, an intercept
    callback and hooks, all its own bound methods."""

    def __init__(self) -> None:
        self.cpu = mc683xx.Machine()
        self.cpu.map(0, RAM_BYTES)
        self.cpu.write(0, struct.pack(">II", STACK, START))
        self.cpu.reset()
        self.cpu.add_code_hook(self.on_code)
        self.cpu.add_peripheral(PERIPHERAL, PERIPHERAL + 1, self)
        self.cpu.set_intercepted(TRAP_0, True)
        self.cpu.set_intercept_callback(self.on_trap)
        # A run, so that the hook's queue and a peripheral's exception
        # have held the harness too.
        self.cpu.run_limited(instructions=2)

    def on_code(self, instruction: mc683xx.Instruction) -> None:
        pass

    def on_trap(self, intercepted: mc683xx.Intercepted) -> bool:
        return False

    def read(self, cycle: mc683xx.BusCycle) -> int:
        return 0

    def write(self, cycle: mc683xx.BusCycle, value: int) -> None:
        pass


class ChipHarness:
    """The chip's: a peripheral and a hook of its own."""

    def __init__(self) -> None:
        image = bytearray(4 * 2048)
        struct.pack_into(">II", image, 0, STACK, START)
        self.builder = mc683xx.Builder()
        self.builder.memory(bytes(image), ["CSBOOT"], writable=False)
        self.builder.unwired_peripheral(self)
        self.chip = self.builder.build()
        self.chip.add_code_hook(self.on_code)

    def on_code(self, instruction: mc683xx.Instruction) -> None:
        pass

    def next_event(self) -> int | None:
        return None


class UnbuiltHarness:
    """A builder never built, holding the harness's peripheral."""

    def __init__(self) -> None:
        self.builder = mc683xx.Builder()
        self.builder.unwired_peripheral(self)


class BoardHarness:
    """A board whose peripheral's class holds the harness."""

    def __init__(self, directory: Path) -> None:
        harness = self

        class Peripheral:
            PORTS: list[tuple[str, str, str]] = []

            def __init__(self, **parameters: Any) -> None:
                self.harness = harness

        self.board = mc683xx.Board(str(directory / "board.toml"), types={"peripheral": Peripheral})


class ObservingHarness:
    """A board observed by the harness's own bound method."""

    def __init__(self, directory: Path) -> None:
        self.board = mc683xx.Board(str(directory / "board.toml"))
        self.board.observe("switch", self.on_switch)
        self.board.send("switch", True, kind="logic")
        self.board.run_for(picoseconds=1000)

    def on_switch(self, net: str, at: int, value: object) -> None:
        pass


def test_a_cpu_harness_is_collected() -> None:
    assert collected(CpuHarness) == 0


def test_a_chip_harness_is_collected() -> None:
    assert collected(ChipHarness) == 0
    assert collected(UnbuiltHarness) == 0


def test_a_board_harness_is_collected(tmp_path: Path) -> None:
    image = bytearray(ROM_BYTES)
    struct.pack_into(">II", image, 0, STACK, START)
    (tmp_path / "rom.bin").write_bytes(bytes(image))
    (tmp_path / "board.toml").write_text(BOARD)
    assert collected(lambda: BoardHarness(tmp_path)) == 0


def test_an_observing_harness_is_collected(tmp_path: Path) -> None:
    image = bytearray(ROM_BYTES)
    struct.pack_into(">II", image, 0, STACK, START)
    (tmp_path / "rom.bin").write_bytes(bytes(image))
    (tmp_path / "board.toml").write_text(NET_BOARD)
    assert collected(lambda: ObservingHarness(tmp_path)) == 0


def test_a_removed_hook_lets_its_callable_go() -> None:
    class Callable:
        def __call__(self, instruction: mc683xx.Instruction) -> None:
            pass

    bare = mc683xx.Machine()
    callback = Callable()
    reference = weakref.ref(callback)
    hook = bare.add_code_hook(callback)
    del callback
    assert reference() is not None
    assert bare.remove_hook(hook)
    assert reference() is None
