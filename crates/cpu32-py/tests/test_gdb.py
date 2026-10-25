"""`serve_gdb` on the bare `Machine`, the chip and a board, driven by
`rsp_client.py` in a subprocess with raw packets: no gdb needed.  The
client starts once the stub listens (`ready`), on the port it picked."""

import struct
import subprocess
import sys
from pathlib import Path

import pytest

import mc683xx

CLIENT = Path(__file__).with_name("rsp_client.py")
RAM_BYTES = 64 << 10
STACK = 0x8000
START = 0x1000
STORE = 0x2000
PERIPHERAL = 0xF00000
# moveq #5,d0; addq.l #1,d0; move.l d0,(a0); bra.s back to the moveq
# (CPU32RM 4).
PROGRAM = [0x7005, 0x5280, 0x2080, 0x60F8]
STORE_PC = START + 4
BRANCH_PC = START + 6
# move.l (a0),d0 (CPU32RM 4).
MOVE_LONG_A0_TO_D0 = 0x2010
SUPERVISOR = 0x2700


def words(values: list[int]) -> bytes:
    return struct.pack(f">{len(values)}H", *values)


def serve(target: mc683xx.Machine | mc683xx.Mc68331 | mc683xx.Board, *items: str) -> tuple[mc683xx.Ending, list[str]]:
    """Serves the client's `items` to `target`: how the session ended and
    the client's replies."""
    clients: list[subprocess.Popen[str]] = []

    def ready(port: int) -> None:
        # The target is not borrowed yet: ready may look at it.
        assert target.instructions >= 0
        command = [sys.executable, "-I", str(CLIENT), str(port), *items]
        clients.append(subprocess.Popen(command, stdout=subprocess.PIPE, text=True))

    try:
        ending = target.serve_gdb("127.0.0.1:0", ready=ready)
    finally:
        output, _ = clients[0].communicate(timeout=30)
    return ending, output.splitlines()


def cpu(program: list[int] = PROGRAM) -> mc683xx.Machine:
    """RAM from zero with the reset vectors and `program` at START."""
    bare = mc683xx.Machine()
    bare.map(0, RAM_BYTES)
    bare.write(0, struct.pack(">II", STACK, START))
    bare.write(START, words(program))
    bare.reset()
    bare.write_register("A0", STORE)
    return bare


def test_gdb_runs_the_bare_cpu_with_its_python_hooks() -> None:
    bare = cpu()
    calls: list[int] = []
    bare.add_code_hook(lambda instruction: calls.append(instruction.pc), STORE_PC, STORE_PC)
    ending, replies = serve(
        bare,
        f"Z0,{BRANCH_PC:x},2",
        "c",
        f"m{START:x},4",
        "s",
        f"M{STORE:x},2:abcd",
        "D",
    )
    assert ending == mc683xx.Ending.DETACHED
    assert "swbreak" in replies[1]
    assert replies[2] == "70055280"
    assert replies[3] == "S05"
    assert replies[4] == "OK"
    assert calls == [STORE_PC]
    assert bare.read_register("PC") == START
    assert bare.read(STORE, 2) == b"\xab\xcd"
    # The CPU runs again once gdb has let go, the breakpoint gone.
    assert bare.run_limited(instructions=8).end == mc683xx.End.Instructions()


def test_ctrl_c_interrupts_and_kill_ends() -> None:
    bare = cpu()
    ending, replies = serve(bare, "c!", "^C", "k")
    assert ending == mc683xx.Ending.KILLED
    assert replies == ["S02"]


def test_a_peripheral_s_exception_ends_the_session() -> None:
    class Failing:
        def read(self, cycle: mc683xx.BusCycle) -> int:
            raise ValueError("the peripheral failed")

        def write(self, cycle: mc683xx.BusCycle, value: int) -> None:
            pass

    bare = cpu([MOVE_LONG_A0_TO_D0, 0x60FE])
    bare.add_peripheral(PERIPHERAL, PERIPHERAL + 3, Failing())
    bare.write_register("A0", PERIPHERAL)
    with pytest.raises(ValueError, match="the peripheral failed"):
        serve(bare, "c")
    assert bare.run_limited(instructions=1).end == mc683xx.End.Instructions()


def test_gdb_peeks_the_chip_and_resets_it() -> None:
    image = bytearray(4 * 2048)
    struct.pack_into(">II", image, 0, STACK, START)
    image[START : START + 8] = words(PROGRAM)
    builder = mc683xx.Builder()
    builder.memory(bytes(image), ["CSBOOT"], writable=False)
    chip = builder.build()
    ending, replies = serve(
        chip,
        f"m{START:x},2",
        "s",
        "s",
        "qRcmd," + b"reset".hex(),
        "D",
    )
    assert ending == mc683xx.Ending.DETACHED
    assert replies[0] == "7005"
    assert replies[1:3] == ["S05", "S05"]
    assert chip.read_register("PC") == START


BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
"""


def test_gdb_runs_a_board(tmp_path: Path) -> None:
    image = bytearray(64 << 10)
    struct.pack_into(">II", image, 0, STACK, START)
    image[START : START + 8] = words(PROGRAM)
    (tmp_path / "rom.bin").write_bytes(bytes(image))
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"))
    ending, replies = serve(board, f"Z0,{BRANCH_PC:x},2", "c", "D")
    assert ending == mc683xx.Ending.DETACHED
    assert "swbreak" in replies[1]
    assert board.now > 0
