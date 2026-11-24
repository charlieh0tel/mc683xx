"""docs/python.md's examples, run as the guide says they run: in order,
in one namespace."""

import ast
import inspect
from importlib import metadata
from pathlib import Path

import doc_examples

import mc683xx


def test_the_version_is_the_package_s():
    assert mc683xx.__version__ == metadata.version("mc683xx")
    assert "__version__" not in mc683xx.__all__


def test_the_guide_has_examples():
    assert len(doc_examples.blocks()) > 5


def test_the_guide_runs():
    # One compiled module, so a failure's traceback names the guide's
    # own line.
    code = compile(doc_examples.module(), str(doc_examples.GUIDE), "exec")
    exec(code, {"__name__": "python_guide"})


def test_the_protocols_docstrings_are_the_stubs():
    # The package's protocols are Python's alone, written twice: in the
    # package for help() and in the stubs for type checkers.
    stubs = ast.parse(Path(mc683xx.__file__).with_suffix(".pyi").read_text())
    documented = {
        node.name: ast.get_docstring(node)
        for node in stubs.body
        if isinstance(node, ast.ClassDef)
    }
    protocols = (mc683xx.MappedPeripheral, mc683xx.SpiPeripheral, mc683xx.Peripheral)
    for protocol in protocols:
        name = protocol.__name__
        assert inspect.getdoc(protocol) == documented[name], name
