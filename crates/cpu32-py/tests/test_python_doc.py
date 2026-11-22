"""docs/python.md's examples, run as the guide says they run: in order,
in one namespace."""

from importlib import metadata

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
