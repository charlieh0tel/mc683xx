# Examples

Example boards and their firmware, and the documentation's examples
run as tests (`mc683xx-examples`, not published).

| Path | What |
| --- | --- |
| `boards/` | Board files and their committed images |
| `firmware/demo/` | The demo board's C firmware |
| `src/` | `echo.bin`'s generator, in Rust |
| `tests/` | The host guide's and README's examples, the committed images' checks, the demo's console golden |
| `web/` | The demo board in a web page, through the JavaScript bindings |

## Boards

Run from the repository's root.  The run goes on until Ctrl-C
(`--seconds` limits its board time), `--pace realtime` keeps board
time to wall time, and `--help` gives the rest.

### echo

`boards/echo.toml`: a boot ROM, RAM, a recording stub on CS1, a 10
MHz clock divided to 1 PPS on IC1, and the SCI on a console.  Its
firmware echoes each frame the SCI receives.

```text
cargo run --release -p mc683xx-run -- examples/boards/echo.toml \
    --bind console=stdio --pace realtime
```

`echo.bin` is hand-assembled by `src/echo.rs`; `make echo-image`
rewrites it, and a test checks the committed file is current.

### demo

`boards/demo.toml`: echo's board without the stub, with OC2 counted.
Its firmware, in C, is a console over the SCI:

| Command | Does |
| --- | --- |
| `m` | Mandelbrot set, fixed point |
| `p` | Primes below 1000 |
| `t` | Uptime from the PIT's 1024 Hz ticks |
| `c` | The 1 PPS period on IC1, in TCNT counts |
| `x ADDR` | Hex dump of 64 bytes |
| `f` | An address error, reported from its frame and recovered |
| `h`, `?` | Help |

```text
cargo run --release -p mc683xx-run -- examples/boards/demo.toml \
    --bind console=stdio --pace realtime
```

`demo.bin` and `demo.elf` (with symbols and line info) are committed;
`make demo-image` rebuilds them, reproducibly, with
`m68k-linux-gnu-gcc` (`-mcpu=cpu32`, no libgcc).
[docs/gdb-firmware.md](../docs/gdb-firmware.md) debugs it under gdb,
source level; `tests/demo_console.rs` pins its console
(`UPDATE_GOLDEN=1` rewrites the golden), `tests/demo_browser.rs`
runs it as a browser host would (its image as bytes, paced from the
host's clock, no file, clock or sleep), and the `demo/mandelbrot`
benchmark runs its `m`.

## Web page

`web/` runs the demo board in a browser: `index.html` and `demo.js`,
an ES module that loads the JavaScript bindings
([crates/wasm](../crates/wasm/README.md)), fetches
`boards/demo.toml` and `boards/demo.bin`, and shows the console in
[xterm.js](https://xtermjs.org/) 6.0.0 from jsDelivr.  Keys go to the
console; each animation frame runs the board to the time due by the
page's clock, at most 0.1 s of board time a frame.  The `pace` query
sets the pacing: `realtime` by default, `unpaced`, or a factor
(`?pace=4`).  Build the bindings and serve the repository's top, so
the page reaches `../boards/` and `crates/wasm/pkg/`:

```text
make wasm
python3 -m http.server 8000
```

and open <http://localhost:8000/examples/web/>.  `make web-test`
builds them and checks the page in headless Chrome (`web/test.mjs`, on
the bindings' driver, `crates/wasm/tests/chrome.mjs`: the DevTools
protocol over Node's WebSocket, Node 22 or later; `CHROME=path` names
the browser): it types `m` and Enter and waits
for the Mandelbrot set and the prompt.  It needs the network, for
xterm.js.

The page is on GitHub Pages, at
<https://charlieh0tel.github.io/mc683xx/>, deployed from each release
(`docs/releasing.md`).  `make pages` assembles that site in Cargo's
target directory, `pages/mc683xx/`: the page, the bindings' web module
and `.wasm`, and the board's files at their repository paths, so the
page's relative paths hold, and at its top `web/pages.html`, which
sends a visitor on to `examples/web/` with the query kept.  `make
pages-test` serves `pages/` and checks the site as `web-test` checks
the page, opened at `/mc683xx/`, the subpath GitHub Pages serves it
under.

## Screencast

`screencast/record.sh` records the top README's screencast, the demo
board's firmware under gdb: it drives `mc683xx-run` and
`gdb-multiarch` in a tmux session, records it with asciinema
(`screencast/demo.cast`) and renders `docs/demo.gif` with agg.  From
the repository's top, with those on PATH or agg's path given:
`examples/screencast/record.sh [AGG]`.

## See also

- [docs/board-files.md](../docs/board-files.md): the board file format.
- [docs/peripherals.md](../docs/peripherals.md): the stock peripherals.
- [docs/host-guide.md](../docs/host-guide.md): the Rust host API.
