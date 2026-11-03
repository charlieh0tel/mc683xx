// Writes the npm package's type declarations, in the package directory
// its argument names, from the web bindings' (web/mc683xx.d.ts, which
// wasm-bindgen writes): node.d.ts, for the Node entry, and the web
// bindings' again, each referencing the `esnext.disposable` library,
// which their classes' `[Symbol.dispose]` needs.
//
// node.d.ts is the web bindings' classes, and `init`, `initSync`,
// `InitOutput`, `InitInput` and `SyncInitInput` as the Node entry gives
// them: the module instantiated when it loads, so both return at once
// and take any argument, the two input types `unknown`.  The web
// bindings' `init` names DOM and WebAssembly types (`RequestInfo`,
// `BufferSource`, `WebAssembly.Module`), which a Node project's types
// lack.  The classes are copied, never written by hand, so the two
// declarations keep in step; anything new after them in wasm-bindgen's
// output stops the build, as does a DOM or WebAssembly name in them, or
// a name the web declaration exports that node.d.ts does not, so code
// importing a type from `mc683xx` checks in Node and a browser alike.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [packageDirectory] = process.argv.slice(2);
if (!packageDirectory) {
  throw new Error("give the package's directory");
}
const WEB_TYPES = join(packageDirectory, "web", "mc683xx.d.ts");
const NODE_TYPES = join(packageDirectory, "node.d.ts");

const REFERENCE = '/// <reference lib="esnext.disposable" />\n';
// Where wasm-bindgen's initialization starts, after the classes.
const INITIALIZATION = "\nexport type InitInput =";
// What wasm-bindgen declares from there: all of it replaced for Node.
const INITIALIZED = ["InitInput", "InitOutput", "SyncInitInput", "initSync", "__wbg_init"];
// A name only a browser's or WebAssembly's types declare.
const BROWSER_ONLY = /\b(WebAssembly|RequestInfo|Response|BufferSource|URL)\b/;

// An export's name, `default` for the default export.
const EXPORT = /^export\s+(default\s+)?(?:declare\s+)?(?:class|interface|type|function|enum|const|let|var)\s+(\w+)/;

const NODE_INITIALIZATION = `/**
 * What \`init\` takes in a browser, where it loads the module; ignored
 * here.
 */
export type InitInput = unknown;

/**
 * What \`initSync\` takes in a browser; ignored here.
 */
export type SyncInitInput = unknown;

/**
 * The module's exports, for the bindings alone: opaque here, as Node's
 * types have no WebAssembly.
 */
export interface InitOutput {
    readonly memory: unknown;
}

/**
 * Resolves at once: Node's entry instantiated the module when it
 * loaded, and the argument is ignored.  For code shared with a browser,
 * where it loads the module.
 */
export default function init(module_or_path?: InitInput): Promise<InitOutput>;

/**
 * Returns at once, as \`init\` resolves: the module is instantiated
 * already, and the argument is ignored.
 */
export function initSync(module?: SyncInitInput): InitOutput;
`;

const web = readFileSync(WEB_TYPES, "utf8");
const start = web.indexOf(INITIALIZATION);
if (start < 0 || web.indexOf(INITIALIZATION, start + 1) >= 0) {
  throw new Error(`${WEB_TYPES}: not one ${JSON.stringify(INITIALIZATION.trim())}`);
}
const classes = web.slice(0, start + 1);
const declared = [...web.slice(start).matchAll(/^export (?:default )?(?:type|interface|function) (\w+)/gm)]
  .map((match) => match[1]);
if (JSON.stringify(declared) !== JSON.stringify(INITIALIZED)) {
  throw new Error(`${WEB_TYPES}: after the classes ${declared.join(", ")}, not ${INITIALIZED.join(", ")}`);
}
const browserOnly = classes.match(BROWSER_ONLY);
if (browserOnly) {
  throw new Error(`${WEB_TYPES}: the classes name ${browserOnly[0]}`);
}

// The names `declarations`, TypeScript, exports, `default` for its
// default export; `file` names it in an error.
function exported(file, declarations) {
  return new Set(
    declarations
      .split("\n")
      .filter((line) => /^export\b/.test(line))
      .map((line) => {
        const match = line.match(EXPORT);
        if (!match) {
          throw new Error(`${file}: an export not understood: ${line}`);
        }
        return match[1] ? "default" : match[2];
      }),
  );
}

const node = classes + NODE_INITIALIZATION;
const nodeExports = exported(NODE_TYPES, node);
const missing = [...exported(WEB_TYPES, web)].filter((name) => !nodeExports.has(name));
if (missing.length > 0) {
  throw new Error(`${NODE_TYPES}: does not export ${missing.join(", ")}, as ${WEB_TYPES} does`);
}

writeFileSync(WEB_TYPES, REFERENCE + web);
writeFileSync(NODE_TYPES, REFERENCE + node);
