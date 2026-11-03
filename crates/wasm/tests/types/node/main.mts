// The npm package's types as a Node ES module sees them (node.d.ts):
// compiled, not run, by `make npm-types`.

import init, { Board, BoardRun, initSync } from "mc683xx";
import type { InitInput, InitOutput, SyncInitInput } from "mc683xx";

const input: InitInput = new URL("./mc683xx_bg.wasm", import.meta.url);
const output: InitOutput = await init(input);
const sameOutput: InitOutput = initSync(new Uint8Array() satisfies SyncInitInput);

function run(text: string, image: Uint8Array): [string, bigint, Uint8Array] {
  using board = new Board(text, new Map([["demo.bin", image]]), ["option"]);
  const ran: BoardRun = board.runFor(1n);
  const framed: BoardRun = board.runFrame(performance.now(), 1_000_000_000n);
  board.send("console", new Uint8Array([0x0d]));
  board.setPacing("realtime");
  board.restore(board.save());
  // @ts-expect-error picoseconds are a bigint
  board.runFor(1);
  // @ts-expect-error board time is read-only
  board.now = 0n;
  // @ts-expect-error board time is a bigint
  const seconds: number = board.now;
  // @ts-expect-error a stop is a string
  const code: number = ran.stop;
  const stop: string = ran.stop;
  const now: bigint = board.now + framed.now;
  const pacing: string = board.pacing;
  return [stop + pacing, now, board.takeReceived("console")];
}

export { output, run, sameOutput };
