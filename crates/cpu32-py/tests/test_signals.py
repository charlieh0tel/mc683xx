"""A signal's Python handler runs during a long run, between its
quanta: Ctrl-C (or any handler that raises) ends a run of the bare
`Machine`, the chip or a board that would otherwise go on for hours."""

import re
import signal
import socket
import struct
import threading
import time
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from pathlib import Path

import pytest

import mc683xx

STACK = 0x8000
START = 0x400
# bra.s to itself (CPU32RM 4).
BRANCH_TO_SELF = 0x60FE
ROM_BYTES = 64 << 10
# When the alarm goes off, and how long the run may take to see it.
ALARM_SECONDS = 0.05
PROMPTLY_SECONDS = 5.0

BOARD = """
version = 1

[chip]
type = "mc68331"
[[memory]]
name = "rom"
image = "rom.bin"
attach = [{ select = "CSBOOT" }]
"""

pytestmark = pytest.mark.skipif(not hasattr(signal, "setitimer"), reason="needs setitimer")


class Alarm(Exception):
    """What the alarm's handler raises, as Ctrl-C raises KeyboardInterrupt."""


@contextmanager
def alarm() -> Iterator[None]:
    """SIGALRM after `ALARM_SECONDS`, its handler raising `Alarm`."""

    def handler(number: int, frame: object) -> None:
        raise Alarm

    previous = signal.signal(signal.SIGALRM, handler)
    signal.setitimer(signal.ITIMER_REAL, ALARM_SECONDS)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def interrupted(run: Callable[[], object]) -> float:
    """How long `run_for` took to raise the alarm's exception."""
    start = time.monotonic()
    with alarm(), pytest.raises(Alarm):
        run()
    return time.monotonic() - start


def image() -> bytes:
    rom = bytearray(ROM_BYTES)
    struct.pack_into(">IIH", rom, 0, STACK, START, 0)
    struct.pack_into(">H", rom, START, BRANCH_TO_SELF)
    return bytes(rom)


def test_a_bare_run_with_no_limit_is_interrupted() -> None:
    bare = mc683xx.Machine()
    bare.map(0, ROM_BYTES, contents=image())
    bare.reset()
    assert interrupted(bare.run_limited) < PROMPTLY_SECONDS
    assert bare.read_register("PC") == START
    assert bare.run_limited(instructions=3).end == mc683xx.End.Instructions()


def test_quanta_change_nothing_the_cpu_does() -> None:
    def counter() -> mc683xx.Machine:
        bare = mc683xx.Machine()
        bare.map(0, ROM_BYTES)
        # addq.l #1,d0; bra.s back to it (CPU32RM 4).
        bare.write(0, struct.pack(">IIHH", STACK, START, 0, 0) + bytes(START - 12))
        bare.write(START, struct.pack(">HH", 0x5280, 0x60FC))
        bare.reset()
        return bare

    long, short = counter(), counter()
    assert long.run_limited(instructions=250_001).end == mc683xx.End.Instructions()
    for _ in range(250):
        short.run_limited(instructions=1000)
    short.run_limited(instructions=1)
    assert long.registers() == short.registers()
    assert (long.instructions, long.now) == (short.instructions, short.now)
    assert long.run_limited(cycles=1_000_000).end == mc683xx.End.Stop(mc683xx.Stop.Budget())
    assert long.now >= short.now + 1_000_000


def test_a_chip_run_is_interrupted() -> None:
    builder = mc683xx.Builder()
    builder.memory(image(), ["CSBOOT"], writable=False)
    chip = builder.build()
    assert interrupted(lambda: chip.run_for(10**13)) < PROMPTLY_SECONDS
    assert interrupted(lambda: chip.run_until(10**13)) < PROMPTLY_SECONDS
    assert chip.read_register("PC") == START


def packet(body: str) -> bytes:
    """`body` as a remote serial protocol packet (the gdb manual,
    Remote Protocol)."""
    data = body.encode()
    return b"$" + data + b"#" + f"{sum(data) & 0xFF:02x}".encode()


def converse(port: int, bodies: list[str], done: threading.Event) -> list[bytes]:
    """Sends each of `bodies` to the stub at `port` once the last is
    answered, then holds the connection, silent, until `done`."""
    replies: list[bytes] = []
    with socket.create_connection(("127.0.0.1", port)) as gdb:
        for body in bodies:
            gdb.sendall(packet(body))
            reply = b""
            while not re.search(rb"\$[^#]*#..", reply):
                reply += gdb.recv(4096)
            replies.append(reply)
        done.wait(timeout=PROMPTLY_SECONDS)
    return replies


def bare_cpu() -> mc683xx.Machine:
    bare = mc683xx.Machine()
    bare.map(0, ROM_BYTES, contents=image())
    bare.reset()
    return bare


def test_an_in_process_client_is_served() -> None:
    bare = bare_cpu()
    replies: list[bytes] = []
    clients: list[threading.Thread] = []
    done = threading.Event()
    done.set()

    def ready(port: int) -> None:
        client = threading.Thread(target=lambda: replies.extend(converse(port, ["g", "D"], done)))
        clients.append(client)
        client.start()

    assert bare.serve_gdb("127.0.0.1:0", ready=ready) == mc683xx.Ending.DETACHED
    clients[0].join(timeout=PROMPTLY_SECONDS)
    assert len(replies) == 2 and b"OK" in replies[1]


def test_a_signal_ends_a_session_waiting_for_gdb() -> None:
    bare = bare_cpu()
    assert interrupted(lambda: bare.serve_gdb("127.0.0.1:0")) < PROMPTLY_SECONDS
    # The CPU is free again.
    assert bare.run_limited(instructions=3).end == mc683xx.End.Instructions()


def test_a_signal_ends_a_session_and_removes_its_breakpoints() -> None:
    for target in (bare_cpu(), chip()):
        done = threading.Event()

        def ready(port: int) -> None:
            threading.Thread(target=converse, args=(port, [f"Z0,{START:x},2"], done)).start()

        try:
            assert interrupted(lambda: target.serve_gdb("127.0.0.1:0", ready=ready)) < PROMPTLY_SECONDS
        finally:
            done.set()
        # gdb's breakpoint went with the session: nothing stops the run.
        if isinstance(target, mc683xx.Machine):
            assert target.run_limited(instructions=3).end == mc683xx.End.Instructions()
        else:
            assert target.run_for(1000).stop == mc683xx.Stop.Budget()


def chip() -> mc683xx.Mc68331:
    builder = mc683xx.Builder()
    builder.memory(image(), ["CSBOOT"], writable=False)
    return builder.build()


def test_a_board_run_is_interrupted(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(image())
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"))
    assert interrupted(lambda: board.run_for(seconds=1e6)) < PROMPTLY_SECONDS
    assert board.now > 0


def test_a_paced_board_run_is_interrupted_while_it_sleeps(tmp_path: Path) -> None:
    (tmp_path / "rom.bin").write_bytes(image())
    (tmp_path / "board.toml").write_text(BOARD)
    board = mc683xx.Board(str(tmp_path / "board.toml"))
    # a quantum of board time an hour of wall time: the alarm comes in a sleep
    crawl = mc683xx.Pacing.scaled(5e-3 / 3600)
    assert interrupted(lambda: board.run_for(seconds=1.0, pacing=crawl)) < PROMPTLY_SECONDS
    assert board.now == 5_000_000_000
