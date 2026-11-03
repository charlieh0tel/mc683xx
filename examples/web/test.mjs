// The demo page in headless Chrome, served from the repository's top:
// it waits for the prompt, types `m` and Enter into the terminal as
// keys, and waits for the Mandelbrot set and the prompt after it.  `make
// web-test` runs it after `make wasm`; CHROME names the browser
// (crates/wasm/tests/chrome.mjs).

import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { POLL_MILLISECONDS, sleep, withPage } from "../../crates/wasm/tests/chrome.mjs";

const TOP = fileURLToPath(new URL("../../", import.meta.url));
const PAGE = "examples/web/index.html";
// How long the page may take to boot, and the Mandelbrot set to come.
const TIMEOUT_MILLISECONDS = 120_000;
// The prompt, its trailing space trimmed as the terminal's lines are.
const PROMPT = ">";
// A run of the set's inside, which only the Mandelbrot set prints.
const INSIDE = "@@@@@@";

await withPage(TOP, PAGE, async ({ send, evaluate }) => {
  // The terminal's lines, scrollback included, or null before it opens.
  const LINES = `(() => {
    if (!globalThis.demo) return null;
    const buffer = demo.terminal.buffer.active;
    const lines = [];
    for (let line = 0; line < buffer.length; line++) {
      lines.push(buffer.getLine(line).translateToString(true).trimEnd());
    }
    return lines;
  })()`;

  // The terminal's lines once `done` says of them, the page's status
  // in the error if it never does.
  async function waited(what, done) {
    const end = Date.now() + TIMEOUT_MILLISECONDS;
    for (;;) {
      const lines = await evaluate(LINES);
      if (lines && done(lines)) {
        return lines;
      }
      if (Date.now() > end) {
        const status = await evaluate(`document.getElementById("status").textContent`);
        throw new Error(`no ${what}: ${status}\n${(lines ?? []).join("\n")}`);
      }
      await sleep(POLL_MILLISECONDS);
    }
  }

  const last = (lines) => lines.filter((line) => line.length > 0).at(-1);
  const booted = await waited("prompt", (lines) => last(lines) === PROMPT);
  assert.ok(booted.some((line) => line.includes("MC68331 demo")), booted.join("\n"));

  await evaluate("demo.terminal.focus()");
  await send("Input.insertText", { text: "m" });
  for (const type of ["keyDown", "keyUp"]) {
    await send("Input.dispatchKeyEvent", {
      type,
      key: "Enter",
      code: "Enter",
      windowsVirtualKeyCode: 13,
      text: "\r",
    });
  }
  const drawn = await waited("Mandelbrot set", (lines) => {
    const inside = lines.findIndex((line) => line.includes(INSIDE));
    return inside >= 0 && last(lines.slice(inside)) === PROMPT;
  });
  assert.ok(drawn.includes("> m"), drawn.join("\n"));
  const status = await evaluate(`document.getElementById("status").textContent`);
  assert.match(status, /pacing realtime/);
  console.log(drawn.join("\n"));
  console.log(status);
  console.log("ok: the page boots the demo board and draws the Mandelbrot set");
});
