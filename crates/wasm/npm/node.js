// The package's entry for Node: the web bindings with their module
// instantiated here, from the file beside them, as Node's fetch cannot
// read a file.  The default export, `init`, then resolves at once, so
// `import init, { Board } from "mc683xx"; await init();` runs in Node
// and in a browser alike.  `require("mc683xx")` loads this module too
// (Node's require of an ES module), the same instance.

import { readFileSync } from "node:fs";
import { initSync } from "./web/mc683xx.js";

initSync({ module: readFileSync(new URL("./web/mc683xx_bg.wasm", import.meta.url)) });

export * from "./web/mc683xx.js";
export { default } from "./web/mc683xx.js";
