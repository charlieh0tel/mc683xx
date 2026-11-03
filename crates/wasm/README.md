# mc683xx-wasm

JavaScript bindings for MC68331 boards, through WebAssembly
(`wasm32-unknown-unknown`, `wasm-bindgen`): a board loaded from its
board file's text and its images' bytes, run a frame at a time from the
page's clock, its console as bytes, its state saved and restored.
Not published yet, to npm or crates.io (PLAN.md, WebAssembly).

## Building

```
make wasm
```

builds the crate for `wasm32-unknown-unknown`, binds the module in
Cargo's target directory (`CARGO_TARGET_DIR` or the workspace's
`target`, as `cargo metadata` gives it) with the `wasm-bindgen` CLI
(the crate's exact `wasm-bindgen` version, 0.2.130;
`WASM_BINDGEN=path` names another binary) into the ignored
`crates/wasm/pkg`: `nodejs/` for Node, `web/` for a page (an ES module
whose default export, `init`, loads the `.wasm`), and runs the Node
tests (`tests/demo.test.mjs`, `node:test`, Node 22 or later).  It
needs the `wasm32-unknown-unknown` target (`rustup target add`).  The
crate builds and tests natively too, as `make ci` does.

## API

Names are the Python module's (`docs/python.md`) in JavaScript's case,
Rust's where Python has none.  Times are board time in picoseconds, as
`BigInt`s.  An error is thrown as an `Error` with the Rust error's
message.

```js
import init, { Board } from "./pkg/web/mc683xx_wasm.js";

await init();
const board = new Board(text, new Map([["demo.bin", image]]));
board.runFor(100_000_000_000n);          // boot, 0.1 s, unpaced
board.setPacing("realtime");
function frame(wall) {
  board.runFrame(wall, 100_000_000_000n); // at most 0.1 s a frame
  terminal.write(board.takeReceived("console"));
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
```

- `new Board(text, images, enabled = [])`: the board file `text`
  describes, through its power-on reset, built around the MC68331; its
  peripheral types the built-in ones and the stock peripherals'
  (`mc683xx-peripherals`).  `images` gives each `[[memory]]` image's
  bytes, a `Map` or an object from the path the file names (as it names
  it), a string, to a `Uint8Array`, `null` or `undefined` for none; the
  `Map` and the arrays of any realm, an iframe's too.  They reach
  memories alone: a peripheral's file parameter (an AT28C64B's `image`
  or `file`) still names a file, which the module cannot read, so such a
  board throws (an AT28C64B with neither runs, erased, in memory;
  PLAN.md, WebAssembly).  `enabled` names the board file's options to
  turn on.  Python's `Board(path, ...)` reads the file and its images
  itself.
- `now`: board time.
- `pacing`, `setPacing(text)`: what `runFrame` keeps, as its text
  (`"unpaced"`, the default, `"realtime"`, or a factor, board seconds a
  wall second, such as `"2"`), as Python's `Pacing(text)` takes it.
- `runFrame(wall, picoseconds)`: runs to the board time due by the
  page's clock reading `wall`, in milliseconds (`performance.now()`),
  under the pacing, but no more than `picoseconds`, so a page hidden a
  while does not run away: such a frame drops the time the page missed,
  and the next frames are paced from where it ended; unpaced, it runs
  `picoseconds` (Rust's `Board::run_frame`; Python's `run_frame` takes
  `wall` in seconds and `seconds` or `picoseconds` by keyword).  The
  first frame, and the first after a `setPacing` to another pacing
  (one to the pacing it has changes nothing, so a page may set its
  choice every frame), a `runFor` (even of `0n`) or a successful
  `restore`, runs nothing: it starts the pacing there.  A run never waits; the page
  calls it each frame (`requestAnimationFrame`).
- `runFor(picoseconds)`: runs that much board time from now, unpaced,
  whatever the pacing (Rust's `Board::run_for`; Python's `run_for`
  paces, which a page cannot: it cannot sleep).
- Picoseconds, here and in `runFrame`, are a `bigint` from `0n` to
  `2n ** 64n - 1n`; one out of that range, or a `number`, throws.
- Both give a `BoardRun`: `stop`, why it ended, as Rust's `Stop`
  prints (`"budget"` its time ran out, `"stopped"`, `"halted"`,
  `"hook"`, `"requested"`, `"background"`, `"fault"`), and `now`.
- `send(name, data)`: the run-time endpoint `name` sends each byte of
  `data`, a `Uint8Array` of any realm, an iframe's too (anything else
  throws, another typed array too), now, an untimed serial frame a byte
  (Python's `send` sends one value); an endpoint that drives no serial
  net throws, even for no data.
- `takeReceived(name)`: the data of the serial frames that reached the
  endpoint since the last call, as a `Uint8Array`, a byte a frame (its
  low eight bits; breaks left out, and times: Python's `take_received`
  gives `(time, value)` each).
- `memory(name)`: the board memory's bytes.
- `save()`, `restore(snapshot)`: the board's state as a `Uint8Array`,
  and back, as Rust's `Board::save` and `Board::restore`.
- `free()`: frees the board's memory in the module now, rather than
  when the garbage collector finds it.

Not offered yet: the chip's registers and bus, Python peripherals'
counterpart (JavaScript peripherals), observers, tracing, a
peripheral's files and the other `Board` methods.
