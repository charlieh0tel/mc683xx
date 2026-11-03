// The npm package in a bare page in headless Chrome: no bundler, no
// terminal, nothing from the network.  `make npm-test` runs it on the
// project it installed the package in, which holds the Node tests'
// demo module and the board's files.  The page maps `mc683xx` to the
// file the package's `exports` give a browser, boots the demo board
// unpaced, sends `h` and a carriage return to its console and the test
// checks the help comes back.

import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { POLL_MILLISECONDS, sleep, withPage } from "./chrome.mjs";

const PROJECT = resolve(process.argv[2] ?? assert.fail("usage: browser.mjs PROJECT"));
const PACKAGE = "node_modules/mc683xx";
const PAGE = "index.html";
// The conditions a browser's resolution holds, a bundler's for the web:
// not `node`.
const CONDITIONS = ["browser", "import", "default"];
// How long the page may take to load and boot.
const TIMEOUT_MILLISECONDS = 60_000;

const { BOOT, CONSOLE, HELP_END } = await import(pathToFileURL(join(PROJECT, "demo.mjs")));

// The path `target`, a package.json `exports` value, resolves to under
// `CONDITIONS`, as Node's resolution walks it: the first key in order
// whose condition holds and whose value resolves.
function resolved(target) {
  if (typeof target === "string") {
    return target;
  }
  for (const [condition, value] of Object.entries(target ?? {})) {
    const path = CONDITIONS.includes(condition) ? resolved(value) : undefined;
    if (path !== undefined) {
      return path;
    }
  }
  return undefined;
}

const manifest = JSON.parse(readFileSync(join(PROJECT, PACKAGE, "package.json"), "utf8"));
const entry = resolved(manifest.exports?.["."]);
assert.ok(entry, "the package's exports give a browser nothing");

writeFileSync(
  join(PROJECT, PAGE),
  `<!doctype html>
<meta charset="utf-8">
<title>mc683xx npm test</title>
<script type="importmap">
  ${JSON.stringify({ imports: { mc683xx: `./${PACKAGE}/${entry.replace(/^\.\//, "")}` } })}
</script>
<script>
  // A module that fails to load or resolve, which the module's own catch
  // never sees.
  addEventListener(
    "error",
    (event) => {
      globalThis.result ??= { error: event.message || \`\${event.target.src || "the module"} failed to load\` };
    },
    true,
  );
</script>
<script type="module">
  import init, { Board } from "mc683xx";

  async function fetched(url) {
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(\`\${url}: \${response.status}\`);
    }
    return response;
  }

  try {
    const [text, image] = await Promise.all([
      fetched("boards/demo.toml").then((response) => response.text()),
      fetched("boards/demo.bin").then((response) => response.arrayBuffer()),
      init(),
    ]);
    const board = new Board(text, new Map([["demo.bin", new Uint8Array(image)]]));
    const decoder = new TextDecoder();
    board.runFor(${BOOT}n);
    const banner = decoder.decode(board.takeReceived(${JSON.stringify(CONSOLE)}));
    board.send(${JSON.stringify(CONSOLE)}, new TextEncoder().encode("h\\r"));
    board.runFor(${BOOT}n);
    const help = decoder.decode(board.takeReceived(${JSON.stringify(CONSOLE)}));
    globalThis.result = { banner, help };
  } catch (error) {
    globalThis.result = { error: String(error) };
  }
</script>
`,
);

const result = await withPage(PROJECT, PAGE, async ({ evaluate }) => {
  const end = Date.now() + TIMEOUT_MILLISECONDS;
  for (;;) {
    const value = await evaluate("globalThis.result");
    if (value) {
      return value;
    }
    assert.ok(Date.now() < end, "the page never finished");
    await sleep(POLL_MILLISECONDS);
  }
});
assert.equal(result.error, undefined);
assert.match(result.banner, /MC68331 demo/);
assert.ok(result.help.endsWith(HELP_END), result.help);
console.log(`${result.banner}${result.help}`);
console.log(`ok: ${entry} boots the demo board in a bare page`);
