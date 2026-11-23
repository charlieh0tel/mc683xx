# mc683xx

An emulator of the Motorola MC68331 (a CPU32 core with its SIM, QSM and
GPT) and the boards around it, for JavaScript, through WebAssembly: a
board loaded from its board file's text and its images' bytes, run a
frame at a time from the page's clock or for a stretch of board time,
its console as bytes, its state saved and restored.  The same emulator
is a set of Rust crates and the Python package `mc683xx`; see the
[repository](https://github.com/charlieh0tel/mc683xx) for them, the
board file format
([docs/board-files.md](https://github.com/charlieh0tel/mc683xx/blob/main/docs/board-files.md))
and the example boards.

Early: 0.x, and the API may change between minor versions.

## Install

```
npm install mc683xx
```

It has no dependencies.  Node 22.12 or later, or a browser with
WebAssembly and `BigInt`.

## Usage

The package is an ES module.  Its default export, `init`, loads the
WebAssembly module; call it once, before anything else, the same way
in Node and in a browser:

```js
import init, { Board } from "mc683xx";

await init();
```

In Node the module loads with the package, so `init` resolves at once;
`require("mc683xx")` gives the same module to CommonJS.  In a browser
`init` fetches `mc683xx_bg.wasm` from beside the package's
`web/mc683xx.js`, which bundlers that follow `new URL(...,
import.meta.url)` (webpack 5, Vite) copy; `init({ module_or_path })`
takes another URL, a `Response`, the bytes or a compiled
`WebAssembly.Module`.  A page with no bundler maps the name to the
file:

```html
<script type="importmap">
  { "imports": { "mc683xx": "./node_modules/mc683xx/web/mc683xx.js" } }
</script>
```

### Node

```js
import { readFileSync } from "node:fs";
import init, { Board } from "mc683xx";

await init();
const board = new Board(readFileSync("demo.toml", "utf8"), {
  "demo.bin": readFileSync("demo.bin"),
});
board.runFor(100_000_000_000n); // 0.1 s of board time
process.stdout.write(board.takeReceivedBytes("console"));
```

### Browser

```js
import init, { Board } from "mc683xx";

const [text, image] = await Promise.all([
  fetch("demo.toml").then((response) => response.text()),
  fetch("demo.bin").then((response) => response.arrayBuffer()),
  init(),
]);
const board = new Board(text, new Map([["demo.bin", new Uint8Array(image)]]));
board.runFor(100_000_000_000n); // boot, 0.1 s, unpaced
board.setPacing("realtime");
function frame(wall) {
  board.runFrame(wall, 100_000_000_000n); // at most 0.1 s a frame
  terminal.write(board.takeReceivedBytes("console"));
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
```

The repository's `examples/web` is such a page, the demo board's
console in xterm.js.

## API

Names are the Python package's in JavaScript's case, Rust's where
Python has none.  Times are board time in picoseconds, as `BigInt`s
(`1_000_000_000_000n` is a second).

Every failure throws an `Error`.  Its `name`, and the class it is an
instance of, say what kind of failure it is, and a page may branch on
them:

- `TypeError`: an argument of the wrong type (images not a `Map` or an
  object, bytes not a `Uint8Array`, picoseconds not a `bigint`);
- `RangeError`: an argument of the right type out of its range
  (picoseconds, a clock reading);
- an `Error` named `LoadError` (a board file or a board that does not
  load), `ParsePacingError` (a pacing that does not parse), `RunError` (a
  run that fails), `SendError` and `ReceiveError` (an endpoint no
  serial net takes or drives), `NoMemory` (no such memory) or
  `StateError` (a board that cannot save, or a snapshot it refuses).

Its `message` is for people: its text may change in any release, and
is not covered by semver.  The names may grow, in a minor release.

The package carries its TypeScript types: Node's entry its own, which
name no DOM or WebAssembly type (its `init` and `initSync` take any
argument and return at once, `InitInput` and `SyncInitInput` are
`unknown`, `InitOutput` opaque), so a Node project without the DOM
library checks them; a browser's are the bindings', with `InitOutput`
opaque but for its `memory`, a `WebAssembly.Memory`.  Both export the
same names.
Both reference the `esnext.disposable` library, which the classes'
`[Symbol.dispose]` needs.  A CommonJS TypeScript file imports the
package under `"module": "nodenext"`, which models Node's `require` of
an ES module (TypeScript 5.8 and later); under `node16` it is an error
(TS1479), and `await import("mc683xx")` is the way there.

- `new Board(text, images = null, enabled = [])`: the board file `text`
  describes, through its power-on reset, built around the MC68331; its
  peripheral types the built-in ones and the stock peripherals'.
  `images` gives each image's bytes, a `[[memory]]`'s or a
  peripheral's (an AT28C64B's `image`), a `Map` or an object
  from the path the file names (as it names it), a string, to a
  `Uint8Array`, `null`, `undefined` or none given for none; the `Map` and the
  arrays of any realm, an iframe's too.  A file a peripheral keeps (an
  AT28C64B's `file`) is one the module cannot open, so such a board
  throws; `enabled` the board file's options to turn on.
- `now`: board time.
- `pacing`, `setPacing(text)`: what `runFrame` keeps, as its text
  (`"unpaced"`, the default, `"realtime"`, or a factor, board seconds a
  wall second, such as `"2"`).  They are the board's here; Rust's and
  Python's are on the board's control handle.
- `runFrame(wall, picoseconds)`: runs to the board time due by the
  page's clock reading `wall`, in milliseconds (`performance.now()`;
  Python's `run_frame` takes seconds), under the pacing, but no more than `picoseconds`, so a page hidden a
  while does not run away: such a frame drops the time the page missed,
  and the next frames are paced from where it ended; unpaced, it runs
  `picoseconds`.  Paced, the first frame, and the first after a `setPacing`
  to another pacing (one to the pacing it has changes nothing, so a
  page may set its choice every frame), a `runFor` (even of `0n`) or a
  successful `restore`, runs nothing: it starts the pacing there.  A
  run never waits; the page calls it each frame
  (`requestAnimationFrame`).
- `runFor(picoseconds)`: runs that much board time from now, unpaced,
  whatever the pacing.
- Picoseconds, here and in `runFrame`, are a `bigint` from `0n` to
  `2n ** 64n - 1n`; one out of that range, or a `number`, throws.
- Both give a `BoardRun`: `stop`, why it ended (`"budget"` its time ran
  out, `"stopped"`, `"halted"`, `"hook"`, `"requested"`,
  `"background"`, `"fault"`, or `"unknown"` for a kind this release
  does not name), and `now`.  `stop`'s TypeScript type, `StopKind`,
  lists the names but is open (`string & {}`): a minor release may
  add a kind, so a `switch` on it keeps a `default`.
- `sendBytes(name, data)`: the run-time endpoint `name` sends each
  byte of `data`, a `Uint8Array` of any realm, an iframe's too
  (anything else throws, another typed array too), now, an untimed
  serial frame a byte; an endpoint that drives no serial net throws,
  even for no data.
- `takeReceivedBytes(name)`: the data of the serial frames that
  reached the endpoint since the last call, as a `Uint8Array`, a byte a
  frame (its low eight bits; breaks and times left out).
- `memory(name)`: the board memory's bytes.
- `save()`, `restore(snapshot)`: the board's state as a `Uint8Array`,
  and back from one, of any realm (anything else throws, an `Array` of
  numbers or another typed array too).
- `free()`: frees the board's memory in the module now, rather than
  when the garbage collector finds it.

Not offered yet: the chip's registers and bus, peripherals written in
JavaScript, observers, tracing, a peripheral's files.

## License

MIT or Apache-2.0, at your option (`LICENSE-MIT`, `LICENSE-APACHE`).
