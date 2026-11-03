// The npm package's types as a browser or a bundler sees them
// (web/mc683xx.d.ts): compiled, not run, by `make npm-types`.

import init, { Board, BoardRun, initSync } from "mc683xx";
import type { InitInput, InitOutput, SyncInitInput } from "mc683xx";

const input: InitInput = new URL("./mc683xx_bg.wasm", import.meta.url);
const output: InitOutput = await init({ module_or_path: input });
const memory: WebAssembly.Memory = output.memory;
const sameOutput: InitOutput = initSync({ module: new Uint8Array() satisfies SyncInitInput });

function run(text: string, image: Uint8Array): [string, bigint, Uint8Array] {
  using board = new Board(text, { "demo.bin": image }, null);
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

export { memory, output, run, sameOutput };
