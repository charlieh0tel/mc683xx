// The demo board (examples/boards/demo.toml) the bindings' tests run:
// its file's text, its image's bytes and what the tests know of it.

import { readFileSync } from "node:fs";

const boards = new URL("../../../examples/boards/", import.meta.url);
export const text = readFileSync(new URL("demo.toml", boards), "utf8");
export const image = new Uint8Array(readFileSync(new URL("demo.bin", boards)));

export const CONSOLE = "console";
export const PICOSECONDS_PER_SECOND = 1_000_000_000_000n;
// Board time the boot runs for, past the banner and the prompt.
export const BOOT = PICOSECONDS_PER_SECOND / 10n;
// The most board time a frame runs.
export const SLICE = PICOSECONDS_PER_SECOND / 10n;
export const HELP_END = "h, ?    this help\r\n> ";
