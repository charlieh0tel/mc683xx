"""The Python blocks of docs/python.md, which run in order in one
namespace.  `test_python_doc.py` runs them; run as a script, this
prints them as one module, for mypy to check against the stubs."""

import re
import sys
from pathlib import Path

GUIDE = Path(__file__).resolve().parents[3] / "docs" / "python.md"
BLOCK = re.compile(r"^```python\n(.*?)^```$", re.MULTILINE | re.DOTALL)


def blocks() -> list[tuple[int, str]]:
    """Each block's first line number in the guide, and its source."""
    text = GUIDE.read_text()
    return [(text.count("\n", 0, match.start(1)) + 1, match.group(1)) for match in BLOCK.finditer(text)]


def module() -> str:
    """The blocks as one module, padded so its line numbers are the
    guide's."""
    lines: list[str] = []
    for line, source in blocks():
        lines.extend([""] * (line - 1 - len(lines)))
        lines.extend(source.splitlines())
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    sys.stdout.write(module())
