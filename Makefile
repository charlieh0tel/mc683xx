.PHONY: hooks ci fmt-check licenses hook-test clippy features test docs deny bench singlestep-data singlestep musashi oracle python corpus \
	firmware-path corpus-musashi unicorn corpus-unicorn corpus-oracles boot-smoke \
	boot-diff reference-path smartclock-boards gdb echo-image demo-image wasm npm-package npm-test npm-types web-test \
	pages pages-test

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
LICENSED_CRATES := cpu32 cpu32-py debug gdb mc683xx mc68331 mc68331-board mc68331-run peripherals target

licenses:
	@for crate in $(LICENSED_CRATES); do \
		for file in $(LICENSE_FILES); do \
			cmp $$file crates/$$crate/$$file || exit 1; \
		done; \
	done

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# The published crates' rustdoc, as docs.rs builds it, with no warnings.
PUBLISHED_LIBRARIES := mc683xx-target mc683xx-cpu32 mc683xx-modules mc683xx-mc68331 mc683xx-board mc683xx-gdb mc683xx-debug mc683xx-peripherals
docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps $(addprefix -p ,$(PUBLISHED_LIBRARIES))

test:
	cargo test --workspace

# examples/boards/echo.bin, the example board's firmware, from
# examples/src/echo.rs.  `make test` checks the committed file matches.
echo-image:
	cargo run -p mc683xx-examples --bin echo-image

# examples/boards/demo.elf and demo.bin, the demo board's firmware, from
# the C in examples/firmware/demo.  Not part of `ci`: it needs
# m68k-linux-gnu-gcc.
demo-image:
	$(MAKE) -B -C examples/firmware/demo

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
# GDB=path to an m68k gdb.  The smoke on a raw image, then a source-level
# session on the demo board (docs/gdb-firmware.md).  Not part of `ci`,
# which needs no gdb.
gdb:
	sh scripts/gdb-smoke.sh
	sh scripts/gdb-demo.sh

# The JavaScript bindings (crates/wasm, PLAN.md WebAssembly): the crate
# for wasm32-unknown-unknown, linted there with the workspace crates it
# depends on, and built as the npm package (npm-package), which npm-test
# tests.  Not part of `ci`: it needs the wasm32-unknown-unknown target,
# the wasm-bindgen CLI at the crate's exact wasm-bindgen version, and
# Node 22.12 or later.  CI's `wasm` job runs it, npm-test and
# npm-types, with the CLI scripts/install-wasm-bindgen.sh installs.
WASM_BINDGEN ?= wasm-bindgen
# Cargo's target directory, as cargo metadata gives it (absolute):
# CARGO_TARGET_DIR or a configured one, else the workspace's target/.
# Read once, when a recipe first needs it, not for every make: the first
# use sets it.  The eval parses only the assignment, never the path,
# which takes its value as it is, `#` and `$` and all; recipes quote it
# (quote), so a space in it is one argument.
TARGET_DIR_READ = $(or $(shell cargo metadata --format-version 1 --no-deps \
	| node -p 'JSON.parse(require("node:fs").readFileSync(0, "utf8")).target_directory'), \
	$(error no target directory from cargo metadata))
TARGET_DIR = $(eval TARGET_DIR := $$(TARGET_DIR_READ))$(TARGET_DIR)
# $(1) as one shell word: single-quoted, each ' in it closed, escaped
# and reopened.
quote = '$(subst ','\'',$(1))'
WASM_MODULE = $(TARGET_DIR)/wasm32-unknown-unknown/release/mc683xx_wasm.wasm
# The npm package `mc683xx`, ignored: the bindings bound by wasm-bindgen
# for the web (web/, which examples/web loads), crates/wasm/npm's Node
# entry, README and package.json (from cargo metadata, the workspace's
# version), the Node entry's types (node.d.ts, from the web bindings',
# by crates/wasm/npm/types.mjs), and the license texts.  `npm publish` from here publishes it
# (docs/releasing.md).
NPM_PACKAGE := crates/wasm/pkg
# Where npm-test packs the package and installs it in a project of its
# own, as a user would.
NPM_TEST = $(TARGET_DIR)/npm-test

wasm: npm-package
	cargo clippy --locked --target wasm32-unknown-unknown -p mc683xx-wasm -- -D warnings

# Built --locked: a release's package is built from its tag's Cargo.lock.
npm-package:
	cargo build --locked --release --target wasm32-unknown-unknown -p mc683xx-wasm
	rm -rf $(NPM_PACKAGE)
	$(WASM_BINDGEN) --target web --remove-name-section --out-name mc683xx \
		--out-dir $(NPM_PACKAGE)/web $(call quote,$(WASM_MODULE))
	node crates/wasm/npm/types.mjs $(NPM_PACKAGE)
	cp crates/wasm/npm/node.js crates/wasm/npm/README.md $(LICENSE_FILES) $(NPM_PACKAGE)/
	cargo metadata --format-version 1 --no-deps \
		| node crates/wasm/npm/package.mjs > $(NPM_PACKAGE)/package.json

# A fresh project for the package in $(1), $(1)/project, its own
# package.json, and the package packed beside it, $(1)/mc683xx-*.tgz,
# for the recipe to install.  A directory with a `#` in its path is
# refused: npm reads one in a tarball's path as a URL's fragment, and
# installs nothing.
define npm_project
	@case $(call quote,$(1)) in *\#*) \
		echo "$@: npm cannot install from a path with a '#':" $(call quote,$(1)) >&2; \
		exit 2;; esac
	rm -rf $(call quote,$(1))
	mkdir -p $(call quote,$(1)/project)
	cd $(NPM_PACKAGE) && npm pack --pack-destination $(call quote,$(1))
	echo '{ "private": true }' > $(call quote,$(1)/project/package.json)
endef

# The packed package installed from its tarball in a fresh project
# (offline: it has no dependencies), with the Node tests
# (crates/wasm/tests/*.test.*), the demo module they share and the demo
# board's files copied in: the tests import `mc683xx` by name, as an ES
# module and from CommonJS; then a bare page in headless Chrome
# (crates/wasm/tests/browser.mjs, CHROME=path naming another browser)
# imports the file the package gives a browser and runs the board.
# Needs Chrome and python3 (http.server), but no network.
npm-test: npm-package
	$(call npm_project,$(NPM_TEST))
	mkdir $(call quote,$(NPM_TEST)/project/boards)
	cp crates/wasm/tests/*.test.* crates/wasm/tests/demo.mjs $(call quote,$(NPM_TEST)/project/)
	cp examples/boards/demo.toml examples/boards/demo.bin $(call quote,$(NPM_TEST)/project/boards/)
	cd $(call quote,$(NPM_TEST)/project) && npm install --offline --no-audit --no-fund ../mc683xx-*.tgz
	cd $(call quote,$(NPM_TEST)/project) && node --test
	node crates/wasm/tests/browser.mjs $(call quote,$(NPM_TEST)/project)

# The package's TypeScript declarations, compiled as its users' would
# be (tsc, `strict`, `skipLibCheck` off): the packed package installed
# in a fresh project with TypeScript and Node's types at exact versions,
# and three consumers in crates/wasm/tests/types, each its own
# tsconfig.json, compiled against it: a Node ES module (node.d.ts,
# `nodenext`, Node's types), a browser's or a bundler's (the web
# bindings' web/mc683xx.d.ts, `bundler`, the DOM's types, not Node's)
# and a CommonJS module that requires it (node.d.ts).  Not part of
# `ci` or npm-test: it needs the network, for TypeScript.  CI's `wasm`
# job runs it after npm-test.
TYPESCRIPT_TOOLS := typescript@5.9.3 @types/node@22.20.5
NPM_TYPES = $(TARGET_DIR)/npm-types
TYPES_CONSUMERS := node browser commonjs

npm-types: npm-package
	$(call npm_project,$(NPM_TYPES))
	cp -R $(addprefix crates/wasm/tests/types/,$(TYPES_CONSUMERS)) $(call quote,$(NPM_TYPES)/project/)
	cd $(call quote,$(NPM_TYPES)/project) && npm install --no-audit --no-fund --save-exact \
		../mc683xx-*.tgz $(TYPESCRIPT_TOOLS)
	cd $(call quote,$(NPM_TYPES)/project) && for consumer in $(TYPES_CONSUMERS); do \
		echo "tsc: $$consumer"; node_modules/.bin/tsc -p $$consumer || exit 1; \
	done

# The demo page (examples/web) in headless Chrome, CHROME=path naming
# another browser: it types at the demo board's console and waits for
# the Mandelbrot set.  Not part of `ci`: it needs what `wasm` does,
# Chrome, and the network for xterm.js.
web-test: wasm
	node examples/web/test.mjs

# The demo page as a static site, for GitHub Pages
# (.github/workflows/pages.yml): the page, the npm package's web module
# and .wasm as npm-package builds them, and the demo board's files, each
# at its repository path so the page's relative paths hold, and at the
# top examples/web/pages.html, which sends a visitor on to the page.
# Built in PAGES_SITE, a directory named for the project site's
# subpath, so serving PAGES serves it as GitHub Pages does, at
# /mc683xx/.
PAGES = $(TARGET_DIR)/pages
PAGES_SITE = $(PAGES)/mc683xx
PAGES_WEB := examples/web
PAGES_BOARDS := examples/boards
PAGES_MODULE := $(NPM_PACKAGE)/web

pages: npm-package
	rm -rf $(call quote,$(PAGES))
	mkdir -p $(foreach dir,$(PAGES_WEB) $(PAGES_BOARDS) $(PAGES_MODULE),$(call quote,$(PAGES_SITE)/$(dir)))
	cp $(PAGES_WEB)/pages.html $(call quote,$(PAGES_SITE)/index.html)
	cp $(PAGES_WEB)/index.html $(PAGES_WEB)/demo.js $(call quote,$(PAGES_SITE)/$(PAGES_WEB)/)
	cp $(PAGES_BOARDS)/demo.toml $(PAGES_BOARDS)/demo.bin $(call quote,$(PAGES_SITE)/$(PAGES_BOARDS)/)
	cp $(PAGES_MODULE)/mc683xx.js $(PAGES_MODULE)/mc683xx_bg.wasm $(call quote,$(PAGES_SITE)/$(PAGES_MODULE)/)

# The site in headless Chrome as web-test checks the page: PAGES
# served, the site's top opened at /mc683xx/, so the page is reached
# through pages.html, under the subpath.  Not part of `ci`, as
# web-test is not; the `pages` workflow runs it before it deploys.
pages-test: pages
	node examples/web/test.mjs $(call quote,$(PAGES)) mc683xx/
