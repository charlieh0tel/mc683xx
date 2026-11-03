// The demo board in the page: its console in a terminal, run each
// animation frame at the pacing the page's `pace` query gives
// (`realtime` by default, `unpaced` or a factor).

import { Terminal } from "https://cdn.jsdelivr.net/npm/@xterm/xterm@6.0.0/lib/xterm.mjs";
import init, { Board } from "../../crates/wasm/pkg/web/mc683xx_wasm.js";

const BOARD_FILE = "../boards/demo.toml";
// The image as the board file names it, and where the page fetches it.
const IMAGE_NAME = "demo.bin";
const IMAGE_URL = "../boards/demo.bin";
const CONSOLE = "console";
const PICOSECONDS_PER_SECOND = 1e12;
// The most board time a frame runs, so a tab hidden a while does not run
// away when it comes back: 0.1 s.
const SLICE = 100_000_000_000n;

const status = document.getElementById("status");

async function fetched(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url}: ${response.status} ${response.statusText}`);
  }
  return response;
}

async function start() {
  const [text, image] = await Promise.all([
    fetched(BOARD_FILE).then((response) => response.text()),
    fetched(IMAGE_URL).then((response) => response.arrayBuffer()),
    init(),
  ]);
  const board = new Board(text, new Map([[IMAGE_NAME, new Uint8Array(image)]]));
  const pacing = new URLSearchParams(location.search).get("pace") ?? "realtime";
  board.setPacing(pacing);

  const terminal = new Terminal({ rows: 30, scrollback: 1000 });
  terminal.open(document.getElementById("terminal"));
  const encoder = new TextEncoder();
  terminal.onData((data) => board.send(CONSOLE, encoder.encode(data)));
  terminal.focus();
  // For tests, and for a look from the browser's console.
  globalThis.demo = { board, terminal };

  function frame(wall) {
    try {
      board.runFrame(wall, SLICE);
      const received = board.takeReceived(CONSOLE);
      if (received.length > 0) {
        terminal.write(received);
      }
    } catch (error) {
      status.textContent = `Stopped: ${error.message}`;
      return;
    }
    const seconds = Number(board.now) / PICOSECONDS_PER_SECOND;
    status.textContent = `Board time ${seconds.toFixed(3)} s, pacing ${board.pacing}.`;
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

start().catch((error) => {
  status.textContent = `Failed: ${error.message}`;
});
