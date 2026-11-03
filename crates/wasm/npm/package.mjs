// Writes the npm package's package.json to stdout from `cargo metadata
// --format-version 1 --no-deps` on stdin: the version, description,
// license and repository mc683xx-wasm's Cargo.toml gives
// (docs/releasing.md), so the package's version is the workspace's.

import { readFileSync } from "node:fs";

const CRATE = "mc683xx-wasm";
// The web bindings (`wasm-bindgen --target web --out-name mc683xx`) and
// Node's entry, which re-exports them, each with its types (types.mjs).
const WEB = "./web/mc683xx.js";
const TYPES = "./web/mc683xx.d.ts";
const NODE = "./node.js";
const NODE_TYPES = "./node.d.ts";

const metadata = JSON.parse(readFileSync(0, "utf8"));
const crate = metadata.packages.find((member) => member.name === CRATE);
if (!crate) {
  throw new Error(`no ${CRATE} in cargo metadata`);
}

const manifest = {
  name: "mc683xx",
  version: crate.version,
  description: crate.description,
  license: crate.license,
  repository: {
    type: "git",
    url: `git+${crate.repository}.git`,
    directory: "crates/wasm",
  },
  homepage: `${crate.repository}#readme`,
  bugs: `${crate.repository}/issues`,
  keywords: ["emulator", "m68k", "cpu32", "mc68331", "motorola", "webassembly"],
  type: "module",
  // Node's require of an ES module, which a CommonJS caller of the Node
  // entry needs, is on by default from 22.12.
  engines: { node: ">=22.12.0" },
  // npm adds package.json and README.md itself, but a license only as
  // LICENSE or LICENCE.
  files: [NODE, NODE_TYPES, WEB, TYPES, "./web/mc683xx_bg.wasm", "LICENSE-MIT", "LICENSE-APACHE"],
  types: TYPES,
  exports: {
    ".": {
      node: { types: NODE_TYPES, default: NODE },
      default: { types: TYPES, default: WEB },
    },
    "./package.json": "./package.json",
  },
  sideEffects: [NODE],
};

process.stdout.write(`${JSON.stringify(manifest, null, 2)}\n`);
