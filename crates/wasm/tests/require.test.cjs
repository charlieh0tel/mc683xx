// The npm package from CommonJS: `require` gives the module an ES import
// does, initialized, and it runs the demo board.

const assert = require("node:assert/strict");
const { test } = require("node:test");
const { Board } = require("mc683xx");
const { BOOT, CONSOLE, image, text } = require("./demo.mjs");

test("require gives the ES module's Board", async () => {
  assert.equal(Board, (await import("mc683xx")).Board);
});

test("the demo boots from CommonJS", () => {
  const board = new Board(text, { "demo.bin": image });
  board.runFor(BOOT);
  assert.match(new TextDecoder().decode(board.takeReceived(CONSOLE)), /MC68331 demo/);
});
