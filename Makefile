.PHONY: hooks ci fmt-check licenses hook-test clippy features test docs deny bench singlestep-data singlestep musashi oracle python corpus \
	firmware-path corpus-musashi unicorn corpus-unicorn corpus-oracles boot-smoke \
	boot-diff reference-path smartclock-boards gdb echo-image

# Once per clone: the commit-msg hook refuses AI attribution (AGENTS.md)
# and pre-push uploads the Git LFS objects (.gitattributes).
hooks:
	git config core.hooksPath .githooks

# What CI runs.
ci: fmt-check licenses hook-test clippy features test docs

# The commit-msg hook's checks on sample identities and messages.
hook-test:
	sh scripts/test-commit-msg.sh

fmt-check:
	cargo fmt --all -- --check

# The license texts in each published crate and the Python package:
# copies of the root's, not links, so a Windows checkout has them too.
LICENSE_FILES := LICENSE-MIT LICENSE-APACHE
LICENSED_CRATES := cpu32 cpu32-py gdb mc683xx mc68331 mc68331-board mc68331-run peripherals

licenses:
	@for crate in $(LICENSED_CRATES); do \
		for file in $(LICENSE_FILES); do \
			cmp $$file crates/$$crate/$$file || exit 1; \
		done; \
	done

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# The published crates' rustdoc, as docs.rs builds it, with no warnings.
PUBLISHED_LIBRARIES := mc683xx-cpu32 mc683xx-modules mc683xx-mc68331 mc683xx-board mc683xx-gdb mc683xx-peripherals
docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps $(addprefix -p ,$(PUBLISHED_LIBRARIES))

test:
	cargo test --workspace

# examples/boards/echo.bin, the example board's firmware, from
# examples/src/echo.rs.  `make test` checks the committed file matches.
echo-image:
	cargo run -p mc683xx-examples --bin echo-image

# The gdb stub's features: it builds for the bare CPU alone and for the
# chip without the board, and the bare build pulls in neither.  The board
# crate builds without the MC68331, its generic code naming no part.
features:
	cargo clippy -p mc683xx-gdb --no-default-features --all-targets -- -D warnings
	cargo clippy -p mc683xx-gdb --no-default-features --features mc68331 --all-targets -- -D warnings
	! cargo tree -p mc683xx-gdb --no-default-features -e normal | grep -E 'mc683xx-(board|mc68331) '
	cargo clippy -p mc683xx-board --no-default-features --lib -- -D warnings
	! cargo tree -p mc683xx-board --no-default-features -e normal | grep -E 'mc683xx-mc68331 '

# The dependencies' licenses and the RustSec advisories against them
# (deny.toml).  Not part of `ci`: it needs cargo-deny, which CI's deny
# job takes from cargo-deny-action, and the network for the advisories.
deny:
	cargo deny check licenses advisories

# The benchmarks (docs/performance.md).  Not part of `ci`, whose clippy
# only compiles them.  Pass criterion's options in BENCH_ARGS, such as
# `--save-baseline before` and then `--baseline before`.
bench:
	cargo bench -p mc683xx-bench -- $(BENCH_ARGS)

# Instruction-level oracles, fetched once at pinned commits into the
# ignored external/ (PLAN.md, Verification).  Not part of `ci`: they
# need the network the first time.
EXTERNAL := $(CURDIR)/external
SINGLESTEP_URL := https://github.com/SingleStepTests/m68000
SINGLESTEP_COMMIT := 64b253116a3de04aaac4346c43680960dc9b67e5
MUSASHI_URL := https://github.com/kstenerud/Musashi
MUSASHI_COMMIT := 313ebf1bd9f4d0d93341eb5ce21fd8a119e9dbdd
ORACLE_FEATURES := --features cpu32-oracle/musashi,cpu32-harness/musashi

# SingleStepTests m68000 (MIT): about 140 MB.
singlestep-data:
	sh scripts/fetch-pinned.sh $(SINGLESTEP_URL) $(SINGLESTEP_COMMIT) $(EXTERNAL)/m68000

singlestep: singlestep-data
	SINGLESTEP_M68000=$(EXTERNAL)/m68000/v1 \
		cargo test --release -p cpu32-harness --test single_step -- --nocapture

# Musashi (MIT), the differential oracle; test-only (third_party/NOTICE).
musashi:
	sh scripts/fetch-pinned.sh $(MUSASHI_URL) $(MUSASHI_COMMIT) $(EXTERNAL)/musashi

oracle: musashi
	MUSASHI_DIR=$(EXTERNAL)/musashi \
		cargo clippy -p cpu32-oracle -p cpu32-harness --all-targets $(ORACLE_FEATURES) -- -D warnings
	MUSASHI_DIR=$(EXTERNAL)/musashi \
		cargo test --release -p cpu32-oracle -p cpu32-harness $(ORACLE_FEATURES) -- --nocapture

# The Python bindings (cpu32-py, PLAN.md Python), built with maturin
# and tested with pytest in the ignored .venv, which the first run
# creates (it needs the network then, and again when the tools'
# pins change).  The type stubs are checked against the module with
# mypy's stubtest, and docs/python.md's examples with mypy.  Not part
# of `ci`, which needs no Python.
VENV := $(CURDIR)/.venv
PYTHON_CRATE := crates/cpu32-py
PYTHON_TOOLS := maturin==1.15.0 pytest==9.1.1 mypy==2.4.0
# A stamp naming the pins, so a change to them installs them.
PYTHON_TOOLS_STAMP := $(VENV)/tools-$(subst ==,-,$(subst $() ,_,$(PYTHON_TOOLS)))

$(PYTHON_TOOLS_STAMP):
	test -x $(VENV)/bin/python || python3 -m venv $(VENV)
	$(VENV)/bin/pip install --quiet $(PYTHON_TOOLS)
	rm -f $(VENV)/tools-*
	touch $@

python: $(PYTHON_TOOLS_STAMP)
	PYO3_PYTHON=$(VENV)/bin/python \
		cargo clippy -p cpu32-py --all-targets --features python -- -D warnings
	PYO3_PYTHON=$(VENV)/bin/python cargo test -p cpu32-py --features python
	VIRTUAL_ENV=$(VENV) $(VENV)/bin/maturin develop --quiet -m $(PYTHON_CRATE)/Cargo.toml
	$(VENV)/bin/pytest -q $(PYTHON_CRATE)
	$(VENV)/bin/python -m mypy.stubtest mc683xx \
		--allowlist $(PYTHON_CRATE)/stubtest-allowlist.txt
	$(VENV)/bin/python $(PYTHON_CRATE)/tests/doc_examples.py > $(VENV)/python_guide.py
	$(VENV)/bin/mypy --strict $(VENV)/python_guide.py
	$(VENV)/bin/mypy $(PYTHON_CRATE)/tests

# The smartclock firmware corpus (PLAN.md, Verification): HP's images,
# never committed, read from SMARTCLOCK_FIRMWARE, smartclock's
# third_party/firmware.  Not part of `ci`.
corpus: firmware-path
	SMARTCLOCK_FIRMWARE=$(SMARTCLOCK_FIRMWARE) \
		cargo test --release -p cpu32-harness --test corpus -- --nocapture --test-threads 1

firmware-path:
	@test -n "$(SMARTCLOCK_FIRMWARE)" || \
		{ echo "set SMARTCLOCK_FIRMWARE to smartclock's third_party/firmware"; exit 2; }

# The corpus's functions against oracles, each case's end compared
# (PLAN.md, Verification).  Reported, not asserted; not in `ci`.
# Unicorn (GPL, test-only, third_party/NOTICE) builds QEMU's m68k from
# C the first time: it needs cmake and a C compiler, and some minutes.
UNICORN_FEATURES := --features cpu32-oracle/unicorn,cpu32-harness/unicorn
CORPUS_ORACLES = SMARTCLOCK_FIRMWARE=$(SMARTCLOCK_FIRMWARE) MUSASHI_DIR=$(EXTERNAL)/musashi \
	cargo test --release -p cpu32-harness $(1) --test corpus oracles_compared \
	-- --nocapture --test-threads 1

corpus-musashi: firmware-path musashi
	$(call CORPUS_ORACLES,$(ORACLE_FEATURES))

# Unicorn's own tests: its known faults as they show, and the oracle.
unicorn:
	cargo clippy -p cpu32-oracle -p cpu32-harness --all-targets $(UNICORN_FEATURES) -- -D warnings
	cargo test --release -p cpu32-oracle -p cpu32-harness $(UNICORN_FEATURES)

corpus-unicorn: firmware-path
	$(call CORPUS_ORACLES,$(UNICORN_FEATURES))

# Both, so that a mismatch both oracles agree on is listed first.
corpus-oracles: firmware-path musashi
	$(call CORPUS_ORACLES,$(ORACLE_FEATURES) $(UNICORN_FEATURES))

# Whole images booted from power-on reset on the chip, stubs on the
# board's peripherals, with a trace (PLAN.md, Verification).  Not part of
# `ci`.  BOOT_SMOKE_SECONDS sets the emulated time, 3 s by default.
boot-smoke: firmware-path
	SMARTCLOCK_FIRMWARE=$(SMARTCLOCK_FIRMWARE) \
		cargo test --release -p cpu32-harness --test boot_smoke -- --nocapture

# A whole image's boot compared with smartclock-emu's reference trace
# of it (PLAN.md, Verification), with stubs and then replaying the
# reference's peripheral reads.  Reported, not asserted; not in `ci`.
# BOOT_DIFF_REFERENCE names the trace, whose header names the image
# and the EEPROM dump; BOOT_DIFF_SECONDS the emulated time (the
# reference's and a second); BOOT_DIFF_OUTPUT a directory for the
# chip's traces.
boot-diff: firmware-path reference-path
	SMARTCLOCK_FIRMWARE=$(SMARTCLOCK_FIRMWARE) BOOT_DIFF_REFERENCE=$(BOOT_DIFF_REFERENCE) \
		cargo test --release -p cpu32-harness --test boot_diff -- --nocapture

reference-path:
	@test -n "$(BOOT_DIFF_REFERENCE)" || \
		{ echo "set BOOT_DIFF_REFERENCE to smartclock-emu's reference trace"; exit 2; }

# smartclock's board files (smartclock's boards), each loaded with the
# stock peripherals (mc683xx-peripherals), so their parameters are
# checked, and stubs for the other peripheral types, and run (PLAN.md,
# Boards).  Each board's failure is listed.  Not part of `ci`.
smartclock-boards:
	@test -n "$(SMARTCLOCK_BOARDS)" || \
		{ echo "set SMARTCLOCK_BOARDS to smartclock's boards"; exit 2; }
	SMARTCLOCK_BOARDS=$(SMARTCLOCK_BOARDS) \
		cargo test -p mc683xx-peripherals --test smartclock_boards -- --ignored --nocapture

# A real gdb against the runner's --gdb (docs/gdb.md): gdb-multiarch, or
# GDB=path to an m68k gdb.  Not part of `ci`, which needs no gdb.
gdb:
	sh scripts/gdb-smoke.sh
