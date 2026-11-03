// A page in headless Chrome for the bindings' browser tests: a directory
// served by `python3 -m http.server`, the page driven over the DevTools
// protocol with Node's own WebSocket (Node 22 or later).  CHROME names
// the browser (google-chrome by default).

import { spawn } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const CHROME = process.env.CHROME ?? "google-chrome";
// Under CI (CI set, as GitHub Actions sets it), Chrome runs without its
// sandbox: Ubuntu 24.04, GitHub's ubuntu-latest, restricts unprivileged
// user namespaces through AppArmor, which the sandbox needs, so a
// Chrome no profile lets use them may not start.  The runner is a
// throwaway machine.  Elsewhere the sandbox stays: a
// developer's machine keeps it, and web-test loads xterm.js from a CDN.
const SANDBOX = process.env.CI ? ["--no-sandbox"] : [];
// How often a test polls the page.
export const POLL_MILLISECONDS = 250;
// How long the server and Chrome get to exit once asked to (SIGTERM)
// before they are killed (SIGKILL).
const EXIT_GRACE_MILLISECONDS = 5_000;
// How often the processes Chrome leaves are looked at.
const EXITED_POLL_MILLISECONDS = 50;
// How many times, and how far apart, the profile's removal is tried
// again, should a write still land as it goes.
const REMOVE_RETRIES = 10;
const REMOVE_RETRY_MILLISECONDS = 100;
// How long a DevTools command may take to answer.
const COMMAND_TIMEOUT_MILLISECONDS = 30_000;
// How long the server and Chrome may take to say where they listen,
// and the DevTools connection to open.
const START_TIMEOUT_MILLISECONDS = 30_000;

export async function sleep(milliseconds) {
  await new Promise((resolve) => setTimeout(resolve, milliseconds));
}

// `promise`, or a rejection naming `what` if it has not settled within
// `milliseconds`.
function within(promise, milliseconds, what) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what}: nothing within ${milliseconds} ms`)), milliseconds);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

// Resolves with the first match of `pattern` in `child`'s `stream`;
// rejects if the child exits first, fails to start or prints none in
// time.
function matched(child, stream, pattern) {
  const found = new Promise((resolve, reject) => {
    child.once("error", reject);
    let text = "";
    child[stream].on("data", (data) => {
      text += data;
      const match = text.match(pattern);
      if (match) {
        resolve(match);
      }
    });
    child.on("exit", (code) => reject(new Error(`${child.spawnfile} exited ${code}: ${text}`)));
  });
  return within(found, START_TIMEOUT_MILLISECONDS, `${child.spawnfile}: ${pattern}`);
}

// Ends `child`, if it started and has not exited: asks it to exit,
// kills it if it has not within the grace period, and resolves once it
// has exited.  Its pipes are closed, so a process it started that
// holds them keeps nothing waiting.
async function stopped(child) {
  if (child.pid !== undefined && child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill("SIGTERM");
    try {
      await within(exited, EXIT_GRACE_MILLISECONDS, `${child.spawnfile}: exit`);
    } catch {
      child.kill("SIGKILL");
      await exited;
    }
  }
  for (const stream of [child.stdin, child.stdout, child.stderr]) {
    stream?.destroy();
  }
}

// The state letter of process `pid` from /proc/PID/stat (`Z` a zombie),
// or undefined for none (exited and reaped, or no /proc).
function processState(pid) {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    // The command, in parentheses, may hold spaces and parentheses.
    return stat.slice(stat.lastIndexOf(")") + 2).split(" ")[0];
  } catch {
    return undefined;
  }
}

// The processes descended from `pid`, from /proc (none without it, as
// on macOS): Chrome's zygote, renderers and services, which go on
// writing its profile a moment after it exits.
function descendants(pid) {
  const children = new Map();
  let entries = [];
  try {
    entries = readdirSync("/proc").filter((name) => /^\d+$/.test(name));
  } catch {
    return [];
  }
  for (const entry of entries) {
    try {
      const stat = readFileSync(`/proc/${entry}/stat`, "utf8");
      const parent = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]);
      children.set(parent, [...(children.get(parent) ?? []), Number(entry)]);
    } catch {
      // exited meanwhile
    }
  }
  const found = [];
  const waiting = [pid];
  while (waiting.length > 0) {
    for (const child of children.get(waiting.pop()) ?? []) {
      found.push(child);
      waiting.push(child);
    }
  }
  return found;
}

// Resolves once each of `pids` has exited (a zombie counts), killing
// any left after the grace period; gives up on one that outlives that
// too, which the profile's removal retries cover.
async function exited(pids) {
  const deadline = Date.now() + EXIT_GRACE_MILLISECONDS;
  let killed = false;
  for (;;) {
    const left = pids.filter((pid) => ![undefined, "Z"].includes(processState(pid)));
    if (left.length === 0 || (killed && Date.now() > deadline + EXIT_GRACE_MILLISECONDS)) {
      return;
    }
    if (!killed && Date.now() > deadline) {
      for (const pid of left) {
        try {
          process.kill(pid, "SIGKILL");
        } catch {
          // exited meanwhile
        }
      }
      killed = true;
    }
    await sleep(EXITED_POLL_MILLISECONDS);
  }
}

// A DevTools protocol connection, if it opens in time: `send` gives a
// command's result, or rejects if none comes in time or the connection
// closes first.
async function devtools(url) {
  const socket = new WebSocket(url);
  const opened = new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = () => reject(new Error(`${url}: the DevTools connection failed`));
  });
  try {
    await within(opened, START_TIMEOUT_MILLISECONDS, `${url}: the DevTools connection`);
  } catch (error) {
    socket.close();
    throw error;
  }
  let next = 0;
  const pending = new Map();
  const settled = (id) => {
    const waiting = pending.get(id);
    if (waiting) {
      pending.delete(id);
      clearTimeout(waiting.timer);
    }
    return waiting;
  };
  const closed = () => {
    for (const id of [...pending.keys()]) {
      settled(id).reject(new Error("the DevTools connection closed"));
    }
  };
  socket.onclose = closed;
  socket.onerror = closed;
  socket.onmessage = (event) => {
    const message = JSON.parse(event.data);
    const waiting = settled(message.id);
    if (waiting) {
      if (message.error) {
        waiting.reject(new Error(message.error.message));
      } else {
        waiting.resolve(message.result);
      }
    }
  };
  return {
    send(method, params = {}, sessionId = undefined) {
      const id = ++next;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(
          () => settled(id)?.reject(new Error(`${method}: no answer`)),
          COMMAND_TIMEOUT_MILLISECONDS,
        );
        pending.set(id, { resolve, reject, timer });
        if (socket.readyState === WebSocket.OPEN) {
          socket.send(JSON.stringify({ id, method, params, sessionId }));
        } else {
          closed();
        }
      });
    },
    close() {
      socket.close();
    },
  };
}

// Serves the directory `root`, opens its `page` (a path under it) in
// headless Chrome and gives `use` the page: `send(method, params)`, a
// DevTools command to it, and `evaluate(expression)`, its value there.
// Stops the browser and the server when `use` settles, or a step
// before it fails, and removes Chrome's profile once Chrome's processes
// have all exited, trying again a while if a write still lands.
export async function withPage(root, page, use) {
  let server;
  let profile;
  let chrome;
  let connection;
  try {
    server = spawn("python3", ["-u", "-m", "http.server", "0", "--bind", "127.0.0.1"], {
      cwd: root,
      stdio: ["ignore", "pipe", "pipe"],
    });
    profile = mkdtempSync(join(tmpdir(), "mc683xx-web-test-"));
    const [, port] = await matched(server, "stdout", /port (\d+)/);
    chrome = spawn(
      CHROME,
      [
        "--headless=new",
        ...SANDBOX,
        "--remote-debugging-port=0",
        `--user-data-dir=${profile}`,
        "--no-first-run",
        "--no-default-browser-check",
        "about:blank",
      ],
      { stdio: ["ignore", "ignore", "pipe"] },
    );
    const [, browser] = await matched(chrome, "stderr", /DevTools listening on (ws:\/\/\S+)/);
    connection = await devtools(browser);
    const { targetId } = await connection.send("Target.createTarget", {
      url: `http://127.0.0.1:${port}/${page}`,
    });
    const { sessionId } = await connection.send("Target.attachToTarget", {
      targetId,
      flatten: true,
    });
    const send = (method, params = {}) => connection.send(method, params, sessionId);
    return await use({
      send,
      async evaluate(expression) {
        const { result, exceptionDetails } = await send("Runtime.evaluate", {
          expression,
          returnByValue: true,
        });
        if (exceptionDetails) {
          throw new Error(`${expression}: ${exceptionDetails.text}`);
        }
        return result.value;
      },
    });
  } finally {
    connection?.close();
    const helpers = chrome?.pid === undefined ? [] : descendants(chrome.pid);
    await Promise.all([chrome, server].filter(Boolean).map(stopped));
    await exited(helpers);
    if (profile) {
      rmSync(profile, {
        recursive: true,
        force: true,
        maxRetries: REMOVE_RETRIES,
        retryDelay: REMOVE_RETRY_MILLISECONDS,
      });
    }
  }
}
