# mc683xx-wasm

JavaScript bindings for MC68331 boards, through WebAssembly
(`wasm32-unknown-unknown`, `wasm-bindgen`): a board loaded from its
board file's text and its images' bytes, run a frame at a time from the
page's clock, its console as bytes, its state saved and restored.
`examples/web` runs the demo board in a page with them
(`examples/README.md`, Web page).  Built as the npm package `mc683xx`,
not published yet (PLAN.md, WebAssembly).

## Building

```
make wasm
```

builds the crate for `wasm32-unknown-unknown`, lints it there, builds
the npm package `mc683xx` (`make npm-package`) into the ignored
`crates/wasm/pkg`, from the module in Cargo's target directory
(`CARGO_TARGET_DIR` or the workspace's `target`, as `cargo metadata`
gives it), and runs the Node tests (`tests/demo.test.mjs`,
`node:test`, Node 22.12 or later).  It needs the
`wasm32-unknown-unknown` target (`rustup target add`) and the
`wasm-bindgen` CLI at the crate's exact `wasm-bindgen` version, 0.2.130
(`WASM_BINDGEN=path` names another binary).  The crate builds and
tests natively too, as `make ci` does.

`tests/chrome.mjs` drives a page in headless Chrome for the browser
tests, `make web-test`'s (`examples/README.md`, Web page): the
DevTools protocol over Node's WebSocket, `python3 -m http.server`;
`CHROME=path` names the browser.  Each step has a deadline: a server
or browser that never says where it listens, a connection that never
opens or a command never answered fails the test in 30 s, the
processes stopped.  Chrome's profile is removed once Chrome and the
processes it started have exited.  With `CI` set, as GitHub Actions
sets it, Chrome runs with `--no-sandbox`: Ubuntu 24.04 restricts the
unprivileged user namespaces its sandbox needs.

## The npm package

`npm/` holds what the package adds to the bindings; `make
npm-package` assembles it in `crates/wasm/pkg`:

- `web/`: the bindings, `wasm-bindgen --target web --out-name mc683xx
  --remove-name-section`: an ES module whose default export, `init`,
  fetches the `.wasm` beside it.  `examples/web` loads it from here.
- `node.js` (`npm/node.js`): Node's entry, the same module
  instantiated from the file, so `init` resolves at once and one
  usage, `import init, { Board } from "mc683xx"; await init();`, runs
  in both.  The package's `exports` give Node (the `node` condition)
  this entry and everything else, browsers and bundlers, `web/`; a
  CommonJS caller's `require` loads it too.  One
  binding rather than wasm-bindgen's `nodejs` build as well keeps one
  `.wasm` in the package, not two, and one instance for ES and
  CommonJS callers alike.
- `node.d.ts`, the Node entry's types, and `web/mc683xx.d.ts` again,
  written by `npm/types.mjs` from wasm-bindgen's: the classes copied,
  `init`, `initSync` and `InitOutput` declared as the Node entry gives
  them, naming no DOM or WebAssembly type, which a Node project's
  types lack; both reference the `esnext.disposable` library for the
  classes' `[Symbol.dispose]`.  Copied, not written by hand, so the
  two keep in step; anything new after the classes in wasm-bindgen's
  output, or a browser's type in them, stops the build.
- `package.json`, written by `npm/package.mjs` from `cargo metadata`:
  the version is the workspace's.
- `README.md` (`npm/README.md`, the JavaScript guide and API) and the
  license texts.

The crate itself is not published to crates.io (PLAN.md,
WebAssembly).

## API

[npm/README.md](npm/README.md), the package's README, is the guide and
reference.  Names are the Python module's (`docs/python.md`) in
JavaScript's case, Rust's where Python has none; where they differ
from Python's:

- `new Board(text, images, enabled)` takes the board file's text and
  its `[[memory]]` images' bytes; Python's `Board(path, ...)` reads the
  file and its images itself.  A peripheral's file parameter (an
  AT28C64B's `image`) is not among them, and throws (PLAN.md,
  WebAssembly).
- `setPacing(text)` takes the text Python's `Pacing(text)` takes.
- `runFrame(wall, picoseconds)` is Rust's `Board::run_frame`, `wall`
  in milliseconds (`performance.now()`); Python's `run_frame` takes
  `wall` in seconds and `seconds` or `picoseconds` by keyword.
- `runFor` is Rust's `Board::run_for`, unpaced (Python's `run_for`
  paces, which a page cannot: it cannot sleep).
- `BoardRun.stop` names the kind of Rust's `Stop` (`"budget"`,
  `"halted"`, ...), by a mapping of the bindings' own, not `Stop`'s
  `Display`.
- `send(name, data)` sends each byte of `data` (Python's `send` sends
  one value); `takeReceived(name)` gives the bytes alone (Python's
  `take_received` gives `(time, value)` each).
- `save()` and `restore(snapshot)` are Rust's `Board::save` and
  `Board::restore`.
