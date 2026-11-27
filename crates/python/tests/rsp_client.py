"""A stand-in for gdb in `test_gdb.py`: sends raw remote serial protocol
packets (the gdb manual, Remote Protocol) to the stub and prints each
reply's body, run-length decoded, a line each.

usage: rsp_client.py PORT ITEM...

An ITEM is a packet's body, sent once the last is answered; one ending
in `!` is sent without waiting for its answer (a continue gdb will
interrupt); `^C` sends gdb's Ctrl-C."""

import socket
import sys

INTERRUPT = b"\x03"
# Run-length counts are a character's code less this.
RUN_LENGTH_BIAS = 29


def packet(body: str) -> bytes:
    checksum = sum(body.encode()) & 0xFF
    return f"${body}#{checksum:02x}".encode()


def decoded(body: str) -> str:
    out: list[str] = []
    index = 0
    while index < len(body):
        if body[index] == "*":
            out.append(out[-1] * (ord(body[index + 1]) - RUN_LENGTH_BIAS))
            index += 2
        else:
            out.append(body[index])
            index += 1
    return "".join(out)


def reply(gdb: socket.socket, pending: bytearray) -> str | None:
    """The next reply's body, past console output (`O` and hex, as
    `qRcmd` sends before its `OK`), or None if the stub closed."""
    while True:
        start = pending.find(b"$")
        end = pending.find(b"#", start)
        if start >= 0 and end >= 0 and len(pending) >= end + 3:
            body = pending[start + 1 : end].decode()
            del pending[: end + 3]
            if body.startswith("O") and body != "OK":
                continue
            return decoded(body)
        data = gdb.recv(4096)
        if not data:
            return None
        pending.extend(data)


def main() -> None:
    port, items = int(sys.argv[1]), sys.argv[2:]
    with socket.create_connection(("127.0.0.1", port)) as gdb:
        pending = bytearray()
        for item in items:
            if item == "^C":
                gdb.sendall(INTERRUPT)
            elif item.endswith("!"):
                gdb.sendall(packet(item[:-1]))
                continue
            else:
                gdb.sendall(packet(item))
            answer = reply(gdb, pending)
            if answer is None:
                return
            print(answer, flush=True)


if __name__ == "__main__":
    try:
        main()
    except (ConnectionError, BrokenPipeError):
        pass
