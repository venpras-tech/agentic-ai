#!/usr/bin/env node
/**
 * Dev launcher for the Tauri app.
 *
 * Wraps `tauri dev` with port-conflict handling. The Vite dev server and the
 * Tauri `devUrl` have to agree on a port, and `tauri.conf.json` hardcodes both
 * to 5173, so a stale process on that port makes `tauri dev` fail with a
 * confusing "dev server did not respond" error. This script detects the
 * conflict up front and offers to move the whole dev stack to another port.
 *
 * Usage:
 *   npm run tauri:dev            # use 5173, or prompt if it is taken
 *   npm run tauri:dev -- --port 5180
 *   TAURI_DEV_PORT=5180 npm run tauri:dev
 *
 * Non-interactive shells (CI, piped input) never block: they fall back to the
 * first free port and say so on stdout.
 */
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import net from "node:net";
import path from "node:path";
import readline from "node:readline";
import process from "node:process";

const DEFAULT_PORT = 5173;
const MIN_PORT = 1024;
const MAX_PORT = 65535;

/** Matches the port baked into tauri.conf.json / vite.config.ts. */
function configuredPort(argv) {
  const flag = argv.find((a) => a === "--port" || a === "-p");
  if (flag) {
    const next = argv[argv.indexOf(flag) + 1];
    if (next && /^\d+$/.test(next)) return Number(next);
  }
  const inline = argv.find((a) => a.startsWith("--port="));
  if (inline) {
    const n = Number(inline.slice("--port=".length));
    if (Number.isInteger(n)) return n;
  }
  const fromEnv = Number(process.env.TAURI_DEV_PORT);
  if (Number.isInteger(fromEnv) && fromEnv > 0) return fromEnv;
  return DEFAULT_PORT;
}

/**
 * Whether a single bind address is available.
 *
 * `EADDRINUSE` means something already holds it. `EADDRNOTAVAIL` /
 * `EAFNOSUPPORT` mean this machine has no such address family, which must not
 * be mistaken for a conflict.
 */
function canBind(port, host) {
  return new Promise((resolve) => {
    const server = net
      .createServer()
      .once("error", (err) => resolve(err.code !== "EADDRINUSE"))
      .once("listening", () => server.close(() => resolve(true)));
    server.listen(port, host);
  });
}

/**
 * True when Vite will be able to bind `port`.
 *
 * Every loopback family is probed, not just one. Vite binds whatever
 * `localhost` resolves to, which is `::1` on Windows, and there a wildcard or
 * `127.0.0.1` bind *succeeds* even while `[::1]` is taken - so probing a single
 * address reports a stale server on the real port as free. The port is only
 * usable when all probeable families are free.
 */
async function isFree(port) {
  const results = await Promise.all([canBind(port, "127.0.0.1"), canBind(port, "::1")]);
  return results.every(Boolean);
}

/**
 * First free port at or after `start`, scanning upward. Bounded so a fully
 * occupied range fails fast instead of spinning.
 */
async function firstFreePort(start, attempts = 200) {
  for (let i = 0; i < attempts; i += 1) {
    const candidate = start + i;
    if (candidate > MAX_PORT) break;
    if (await isFree(candidate)) return candidate;
  }
  return null;
}

/** Human-readable summary of who holds the port, for the conflict message. */
async function describeHolder(port) {
  if (process.platform !== "win32") return null;
  try {
    const { execFile } = await import("node:child_process");
    const out = await new Promise((resolve, reject) => {
      execFile(
        "powershell.exe",
        [
          "-NoProfile",
          "-Command",
          `(Get-NetTCPConnection -LocalPort ${port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1).OwningProcess`,
        ],
        { windowsHide: true },
        (err, stdout) => (err ? reject(err) : resolve(String(stdout).trim())),
      );
    });
    if (!out) return null;
    const { execFile: exec2 } = await import("node:child_process");
    const name = await new Promise((resolve) => {
      exec2(
        "powershell.exe",
        [
          "-NoProfile",
          "-Command",
          `(Get-Process -Id ${out} -ErrorAction SilentlyContinue).ProcessName`,
        ],
        { windowsHide: true },
        (err, stdout) => resolve(err ? "" : String(stdout).trim()),
      );
    });
    return name ? `${name} (pid ${out})` : `pid ${out}`;
  } catch {
    return null;
  }
}

function ask(question) {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => {
    rl.question(question, (answer) => {
      rl.close();
      resolve(answer.trim());
    });
  });
}

/**
 * Resolve the port to start on, prompting the user when the preferred one is
 * taken. Returns `null` when the user cancels.
 */
async function resolvePort(preferred) {
  if (await isFree(preferred)) return preferred;

  const holder = await describeHolder(preferred);
  console.error(`\nPort ${preferred} is already in use${holder ? ` by ${holder}` : ""}.`);
  console.error("The Tauri dev server and the app must use the same port.\n");

  const interactive = process.stdin.isTTY === true;
  if (!interactive) {
    const fallback = await firstFreePort(preferred + 1);
    if (fallback === null) {
      console.error(`No free port found near ${preferred}.`);
      return null;
    }
    console.error(`Non-interactive shell: starting on port ${fallback} instead.`);
    return fallback;
  }

  while (true) {
    const answer = await ask(
      `Enter a different port number (Enter = next free port, q = cancel): `,
    );

    if (/^q$/i.test(answer)) {
      console.error("Cancelled.");
      return null;
    }

    if (answer === "") {
      const fallback = await firstFreePort(preferred + 1);
      if (fallback === null) {
        console.error(`No free port found near ${preferred}. Try again.`);
        continue;
      }
      return fallback;
    }

    const port = Number(answer);
    if (!Number.isInteger(port) || port < MIN_PORT || port > MAX_PORT) {
      console.error(`Enter a number between ${MIN_PORT} and ${MAX_PORT}.`);
      continue;
    }
    if (!(await isFree(port))) {
      console.error(`Port ${port} is also in use. Pick another.`);
      continue;
    }
    return port;
  }
}

/**
 * Merge a port into the Tauri config for this run only, leaving
 * tauri.conf.json on disk untouched.
 *
 * Both keys must move together: `devUrl` is what the webview loads, and
 * `beforeDevCommand` is what starts Vite. Overriding only one leaves the app
 * pointed at the wrong (or occupied) port.
 */
function tauriArgs(port, passthrough) {
  const override = JSON.stringify({
    build: {
      devUrl: `http://localhost:${port}`,
      beforeDevCommand: `npm --prefix ui run dev -- --port ${port} --strictPort`,
    },
  });
  return ["dev", "--config", override, ...passthrough];
}

/**
 * Absolute path to the locally installed Tauri CLI entry.
 *
 * Invoked through the current node binary rather than `npx`, because going
 * through a shell on Windows strips the double quotes out of the `--config`
 * JSON argument and Tauri then fails to parse it.
 */
function tauriCliEntry() {
  const require = createRequire(import.meta.url);
  const pkg = require.resolve("@tauri-apps/cli/package.json");
  return path.join(path.dirname(pkg), "tauri.js");
}

async function main() {
  const argv = process.argv.slice(2);
  // Anything after a bare `--` is forwarded to the app binary untouched.
  const split = argv.indexOf("--");
  const tauriArgv = split === -1 ? argv : argv.slice(0, split);
  const appArgs = split === -1 ? [] : argv.slice(split);

  const preferred = configuredPort(tauriArgv);
  // A port passed explicitly is a deliberate choice: respect it and let the
  // underlying strictPort error surface rather than silently moving it.
  const explicit = tauriArgv.includes("--port") || process.env.TAURI_DEV_PORT;
  const port = explicit ? preferred : await resolvePort(preferred);

  if (port === null) process.exit(1);

  if (port !== preferred) {
    console.error(`Starting dev stack on port ${port}.\n`);
  }

  const child = spawn(
    process.execPath,
    [tauriCliEntry(), ...tauriArgs(port, appArgs)],
    { stdio: "inherit" },
  );

  // Forward interrupts so Ctrl+C stops tauri rather than orphaning it.
  for (const sig of ["SIGINT", "SIGTERM"]) {
    process.on(sig, () => child.kill(sig));
  }

  child.on("exit", (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code ?? 0);
  });
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
});
