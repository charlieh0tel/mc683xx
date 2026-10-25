# Contributing

Thanks for helping.  Bug reports and feature requests go through the
issue forms; changes through pull requests against `main`.

## License

mc683xx is `MIT OR Apache-2.0`.  By contributing you agree that your
contribution is licensed the same way, with no additional terms.

## Where the behavior comes from

- The vendor manuals in `third_party/motorola/` and the datasheets in
  `third_party/datasheets/` are the specification.  A change to how an
  instruction, a register, a pin or a peripheral behaves cites the
  document and section it follows (for example CPU32RM 6.2.1, QSMRM
  4.3.1) in the code and in the pull request.
- Where a manual is silent or contradicts itself, say so, record the
  choice and why in `PLAN.md`, and do not present a guess as the
  part's behavior.
- Never copy, port or paraphrase code from QEMU, Unicorn or any other
  GPL source, and do not read their implementation to write ours.
  Using them as black-box oracles in feature-gated tests is fine.
  Code from a permissively licensed project needs its source and
  license recorded in `third_party/NOTICE`; ask in an issue first.
- No firmware or ROM images you may not redistribute.

## Before you open a pull request

- `make hooks` once per clone, then `make ci` must pass: formatting,
  clippy, every test.  If you touched the Python bindings, `make
  python` too.  Changes to the core or the modules should also pass
  `make singlestep` and `make oracle`.
- Every fix comes with a test that fails without it; every new
  instruction, register behavior or peripheral path gets one.
- Keep `README.md`, `PLAN.md` and `docs/` current in the same commit
  as the change.
- A snapshot format change bumps its `VERSION` and the `VERSION` of
  every format that holds it, then `UPDATE_GOLDEN=1 cargo test`
  regenerates the goldens.
- The Python API follows the Rust API's naming; a Rust rename renames
  the Python name in the same commit.
- Determinism: the same inputs give the same run.  No host time, no
  host-dependent iteration order, no threads in the core.
- Performance changes are measured before and after (`make bench`,
  `docs/performance.md`).

## Style

- `AGENTS.md` holds the project's full coding rules (imports,
  visibility, error types, naming, comments); they apply to every
  contributor.
- US English and ASCII only, in code, docs and commit messages.
- One functional change per commit and per pull request; a refactor
  goes in its own commit first.  Commit messages describe the change
  and nothing else: no tool or AI-assistant attribution trailers (the
  commit-msg hook and CI refuse them).
