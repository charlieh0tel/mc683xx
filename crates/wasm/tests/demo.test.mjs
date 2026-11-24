// The demo board (examples/boards/demo.toml) through the npm package, as
// an ES module imports it by name: booted, typed at, paced in frames,
// saved and restored, and its mistakes thrown.

import assert from "node:assert/strict";
import { test } from "node:test";
import { runInNewContext } from "node:vm";
import {
  BOOT,
  CONSOLE,
  HELP_END,
  image,
  PICOSECONDS_PER_SECOND,
  SLICE,
  text,
} from "./demo.mjs";
import init, { Board } from "mc683xx";

await init();

const FRAME_MILLISECONDS = 16;

const decoder = new TextDecoder();

function booted() {
  const board = new Board(text, new Map([["demo.bin", image]]));
  const run = board.runFor(BOOT);
  assert.equal(run.stop, "budget");
  assert.equal(run.now, BOOT);
  return { board, text: decoder.decode(board.takeReceivedBytes(CONSOLE)) };
}

test("the demo boots and answers help", () => {
  const { board, text: banner } = booted();
  assert.match(banner, /MC68331 demo/);
  assert.ok(banner.endsWith("> "), banner);
  board.sendBytes(CONSOLE, new TextEncoder().encode("h\n"));
  board.runFor(BOOT);
  const help = decoder.decode(board.takeReceivedBytes(CONSOLE));
  assert.ok(help.startsWith("h\r\n"), help);
  assert.ok(help.endsWith(HELP_END), help);
});

test("images may be an object", () => {
  const board = new Board(text, { "demo.bin": image });
  board.runFor(BOOT);
  assert.match(decoder.decode(board.takeReceivedBytes(CONSOLE)), /MC68331 demo/);
});

test("bytes and images from another realm (an iframe's, a vm context's) are taken", () => {
  // instanceof fails for these: another realm's Uint8Array and Map
  const foreign = runInNewContext("({ bytes: (data) => new Uint8Array(data), map: () => new Map() })");
  const images = foreign.map();
  images.set("demo.bin", foreign.bytes(image));
  assert.ok(!(images instanceof Map) && !(images.get("demo.bin") instanceof Uint8Array));
  const board = new Board(text, images);
  board.runFor(BOOT);
  assert.match(decoder.decode(board.takeReceivedBytes(CONSOLE)), /MC68331 demo/);
  board.sendBytes(CONSOLE, foreign.bytes([...new TextEncoder().encode("h\n")]));
  board.runFor(BOOT);
  assert.ok(decoder.decode(board.takeReceivedBytes(CONSOLE)).endsWith(HELP_END));
  // what only claims to be one, or is another typed array, is not
  for (const data of [{ [Symbol.toStringTag]: "Uint8Array" }, new Int8Array([1]), runInNewContext("new Int8Array([1])")]) {
    assert.throws(() => board.sendBytes(CONSOLE, data), /not a Uint8Array/);
  }
});

test("frames keep to the clock, each at most a slice", () => {
  const { board } = booted();
  board.setPacing("realtime");
  assert.equal(board.pacing, "realtime");
  const start = board.now;
  let wall = 1000;
  assert.equal(board.runFrame(wall, SLICE).now, start, "the first starts the pacing");
  const frames = 30;
  for (let frame = 0; frame < frames; frame++) {
    wall += FRAME_MILLISECONDS;
    assert.equal(board.runFrame(wall, SLICE).stop, "budget");
  }
  const expected = (BigInt(frames * FRAME_MILLISECONDS) * PICOSECONDS_PER_SECOND) / 1000n;
  const nanosecond = 1000n;
  const paced = board.now - start;
  assert.ok(paced - expected < nanosecond && expected - paced < nanosecond, `${paced}`);
  const behind = board.now;
  assert.equal(board.runFrame(wall + 3_600_000, SLICE).now, behind + SLICE);
});

test("picoseconds are a bigint in a u64's range", () => {
  const { board } = booted();
  const start = board.now;
  for (const picoseconds of [2n ** 64n + 5n, 2n ** 64n, -1n]) {
    assert.throws(() => board.runFor(picoseconds), /out of range/);
    assert.throws(() => board.runFrame(0, picoseconds), /out of range/);
  }
  assert.throws(() => board.runFor(5), /picoseconds are not a bigint but a number$/);
  assert.throws(() => board.runFrame(0, 5), /not a bigint but a number$/);
  assert.throws(() => board.runFor(null), /not a bigint but null$/);
  assert.equal(board.now, start);
  // the whole range, past a number's 2 ** 53: paced, a frame runs what
  // is due, not its bound
  board.setPacing("realtime");
  const most = 2n ** 64n - 1n;
  assert.equal(board.runFrame(1000, most).now, start, "starts");
  const run = board.runFrame(1000 + FRAME_MILLISECONDS, most);
  const expected = (BigInt(FRAME_MILLISECONDS) * PICOSECONDS_PER_SECOND) / 1000n;
  const nanosecond = 1000n;
  const paced = run.now - start;
  assert.ok(paced - expected < nanosecond && expected - paced < nanosecond, `${paced}`);
  assert.equal(board.runFor(0n).now, run.now);
});

/** The demo board with an AT28C64B of the given parameters. */
const withEeprom = (parameters) => `${text}
[[peripheral]]
name = "eeprom"
type = "at28c64b"
attach = [{ select = "CS1", lines = 13 }]
reset = []
parameters = { ${parameters} }
`;

test("images reach a peripheral's image, and it must be there", () => {
  const eeprom = withEeprom('image = "eeprom.bin"');
  const images = new Map([
    ["demo.bin", image],
    ["eeprom.bin", new Uint8Array(16)],
  ]);
  new Board(eeprom, images).free();
  assert.throws(
    () => new Board(eeprom, new Map([["demo.bin", image]])),
    /eeprom: parameter "image": reading eeprom\.bin: /,
  );
});

test("a file a peripheral keeps needs a file system", () => {
  const images = new Map([["demo.bin", image]]);
  assert.throws(
    () => new Board(withEeprom('file = "eeprom.bin"'), images),
    /eeprom: parameter "file": /,
  );
});

test("a restore goes back", () => {
  const { board } = booted();
  const snapshot = board.save();
  assert.ok(snapshot instanceof Uint8Array);
  const saved = board.now;
  board.runFor(BOOT);
  board.restore(snapshot);
  assert.equal(board.now, saved);
  assert.deepEqual(board.memory("rom").subarray(0, image.length), image);
});

test("a snapshot is a Uint8Array", () => {
  const { board } = booted();
  const saved = board.now;
  const snapshot = board.save();
  board.runFor(BOOT);
  for (const [data, described] of [
    [[...snapshot], "an Array"],
    [snapshot.buffer, "an ArrayBuffer"],
    [new Uint8ClampedArray(snapshot), "an Uint8ClampedArray"],
    [null, "null"],
  ]) {
    assert.throws(() => board.restore(data), { message: `a snapshot is not a Uint8Array but ${described}` });
  }
  board.restore(runInNewContext("(data) => new Uint8Array(data)")([...snapshot]));
  assert.equal(board.now, saved);
});

test("mistakes throw the Rust error's message", () => {
  assert.throws(() => new Board(text, new Map()), /demo\.bin/);
  assert.throws(() => new Board(text), /demo\.bin/);
  assert.throws(() => new Board(text, { "demo.bin": [1, 2] }), /not a Uint8Array/);
  assert.throws(
    () => new Board(text, new Map([[1, image]])),
    /image's name is not a string but a number$/,
  );
  assert.throws(() => new Board(text, 5), /neither a Map nor an object/);
  const { board } = booted();
  assert.throws(() => board.setPacing("fast"), /no pacing "fast"/);
  assert.throws(() => board.runFrame(Number.NaN, SLICE), /no clock reading/);
  assert.throws(() => board.sendBytes("nowhere", new Uint8Array([1])), Error);
  assert.throws(() => board.sendBytes("nowhere", new Uint8Array()), /external\.nowhere/);
  const notBytes = [
    ["h\n", "a string"],
    [[104, 10], "an Array"],
    [new ArrayBuffer(2), "an ArrayBuffer"],
    [new Int8Array(2), "an Int8Array"],
    [{}, "an Object"],
    [null, "null"],
    [undefined, "undefined"],
  ];
  for (const [data, described] of notBytes) {
    assert.throws(() => board.sendBytes(CONSOLE, data), { message: `data to send is not a Uint8Array but ${described}` });
  }
  assert.throws(() => board.memory("nowhere"), /no memory "nowhere"/);
});

test("mistakes throw an error named for their kind", () => {
  const named = (name) => (error) => error instanceof Error && error.name === name;
  assert.throws(() => new Board(text), named("LoadError"));
  assert.throws(() => new Board(text, 5), TypeError);
  assert.throws(() => new Board(text, { "demo.bin": [1, 2] }), TypeError);
  const { board } = booted();
  assert.throws(() => board.setPacing("fast"), named("ParsePacingError"));
  assert.throws(() => board.runFrame(Number.NaN, SLICE), RangeError);
  assert.throws(() => board.runFor(-1n), RangeError);
  assert.throws(() => board.runFor(5), TypeError);
  assert.throws(() => board.sendBytes(CONSOLE, [1]), TypeError);
  assert.throws(() => board.sendBytes("nowhere", new Uint8Array()), named("SendError"));
  assert.throws(() => board.takeReceivedBytes("nowhere"), named("NoReceiver"));
  assert.throws(() => board.memory("nowhere"), named("NoMemory"));
  assert.throws(() => board.restore(new Uint8Array([1])), named("StateError"));
  assert.throws(() => board.restore([1]), TypeError);
});
