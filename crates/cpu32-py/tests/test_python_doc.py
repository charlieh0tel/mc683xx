"""docs/python.md's examples, run as the guide says they run: in order,
in one namespace."""

import doc_examples


def test_the_guide_has_examples():
    assert len(doc_examples.blocks()) > 5


def test_the_guide_runs():
    # One compiled module, so a failure's traceback names the guide's
    # own line.
    code = compile(doc_examples.module(), str(doc_examples.GUIDE), "exec")
    exec(code, {"__name__": "python_guide"})
