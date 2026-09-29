// The runner half of the wasm test harness.
//
// It serves the built test page and, per request, opens it in a real browser to
// run exactly one GPU test. The verdict is read out of `sessionStorage`, which
// the Rust side writes: a browser has no exit code to hand back, and a
// panicking test traps its wasm instance rather than unwinding, so the panic
// hook is the only place a result can be decided.
//
// `cargo xtask test-wasm` starts and stops this process; `cargo nextest` is
// what drives the requests, one per test.

import { mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import express from "express";
import { chromium } from "playwright";

const HERE = path.dirname(fileURLToPath(import.meta.url));
/** The page and the wasm modules, assembled by `cargo xtask test-wasm`. */
const DIST = path.join(HERE, "..", "dist");

const PORT = Number(process.env.PORT ?? 3000);
/** How long one test may take. Software WebGL is slow, so this is generous. */
const TIMEOUT_MS = Number(process.env.TIMEOUT ?? 30_000);
const BASE_URL = `http://127.0.0.1:${PORT}`;
/** Run with a visible window, to watch a failure happen. */
const SHOW = process.argv.includes("--show");

// The browser's own home, rather than the invoking user's. Chrome wants to
// write a profile and a crash database under `HOME`, and a container — a CI
// runner included — may mount that read-only, which would fail the launch
// before any test could run. A directory of its own is writable wherever the
// system's temporary directory is, and keeps the browser's files out of the
// user's home.
const BROWSER_HOME = mkdtempSync(path.join(os.tmpdir(), "unlit3d-wasm-test-"));

const browser = await chromium.launch({
  headless: !SHOW,
  // An explicit binary lets a developer reuse a browser they already have
  // instead of downloading one.
  executablePath: process.env.CHROME_PATH || undefined,
  env: {
    ...process.env,
    HOME: BROWSER_HOME,
    XDG_CACHE_HOME: path.join(BROWSER_HOME, "cache"),
    XDG_CONFIG_HOME: path.join(BROWSER_HOME, "config"),
  },
  args: [
    // A container often has no usable sandbox.
    "--no-sandbox",
    // `/dev/shm` is small in a container, and Chrome needs more than it offers.
    "--disable-dev-shm-usage",
    // Software WebGL, so a machine with no GPU still runs these tests.
    "--enable-unsafe-swiftshader",
  ],
});

const app = express();

app.get("/run_test", async (req, res) => {
  const name = String(req.query.name ?? "");
  const module = String(req.query.wasm ?? "");
  console.error(`run ${module}::${name}`);

  // A fresh context per test. `sessionStorage` carries the verdict, so one
  // test's result must not be able to answer for the next.
  const context = await browser.newContext();
  context.setDefaultTimeout(TIMEOUT_MS);
  const page = await context.newPage();

  // Collected so a failure carries the browser's own account of it, which is
  // otherwise lost when the page goes away.
  const logs = [];
  page.on("console", (message) => logs.push(`[${message.type()}] ${message.text()}`));
  page.on("pageerror", (error) => logs.push(`[pageerror] ${error.message}`));

  try {
    const url = new URL(BASE_URL);
    url.search = new URLSearchParams({ wasm: module, name }).toString();
    await page.goto(url.toString());

    // Whichever the harness writes first decides the test: a pass writes
    // `test_success`, and anything else — a failed assertion, a panic, an
    // expected panic that never happened — writes `test_failure`.
    const failure = await Promise.race([
      page.waitForFunction(() => window.sessionStorage.test_success).then(() => null),
      page
        .waitForFunction(() => window.sessionStorage.test_failure)
        .then(() => page.evaluate(() => window.sessionStorage.test_failure)),
    ]);

    if (failure === null) {
      res.sendStatus(200);
    } else {
      res.status(500).send(`${failure}\n\n${logs.join("\n")}`);
    }
  } catch (error) {
    // A timeout, a page that died before reporting: the test failed, and the
    // browser's log is the most useful thing to report with it.
    res.status(500).send(`${error?.stack ?? error}\n\n${logs.join("\n")}`);
  } finally {
    await context.close();
  }
});

// The example's own page, built and bindgened by `cargo xtask test-wasm`.
// Serving it here is what lets one route load it in a browser.
const EXAMPLE = path.join(HERE, "..", "example");

// Load the example and check that it really draws something.
//
// The GPU tests above exercise the rendering paths directly, but they never
// start the example — the program a browser actually runs — so a change that
// compiles and still leaves the page blank passes them. This is the check that
// the example boots, finds a backend and puts pixels on its canvas.
//
// A rendered scene is neither empty nor flat, so both are tested: an unstarted
// canvas is transparent, and a canvas that never drew reads as one color.
app.get("/run_example", async (_req, res) => {
  console.error("run the example");

  const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
  context.setDefaultTimeout(TIMEOUT_MS);
  const page = await context.newPage();

  const logs = [];
  page.on("console", (message) => logs.push(`[${message.type()}] ${message.text()}`));
  page.on("pageerror", (error) => logs.push(`[pageerror] ${error.message}`));

  try {
    await page.goto(`${BASE_URL}/example/`);

    // Polled rather than awaited on a fixed sleep: software WebGL2 takes a
    // while to reach its first frame, and how long varies by machine. This
    // rejects — and so fails the run — if the canvas never gets there.
    await page.waitForFunction(
      () => {
        const canvas = document.querySelector("canvas");
        if (!canvas || canvas.width === 0) return false;
        // `preserveDrawingBuffer` is off, so the canvas is read through a copy
        // rather than `getImageData` on the live one.
        const snapshot = document.createElement("canvas");
        snapshot.width = canvas.width;
        snapshot.height = canvas.height;
        const context = snapshot.getContext("2d");
        context.drawImage(canvas, 0, 0);
        const { data } = context.getImageData(0, 0, canvas.width, canvas.height);
        let opaque = 0;
        const colors = new Set();
        for (let i = 0; i < data.length; i += 4) {
          if (data[i + 3] > 0) opaque++;
          colors.add((data[i] << 16) | (data[i + 1] << 8) | data[i + 2]);
        }
        // A scene covers the canvas and shades it; a blank one is neither.
        return opaque > data.length / 8 && colors.size > 16;
      },
      undefined,
      { timeout: TIMEOUT_MS },
    );

    res.sendStatus(200);
  } catch (error) {
    res.status(500).send(`${error?.stack ?? error}\n\n${logs.join("\n")}`);
  } finally {
    await context.close();
  }
});

app.use("/example", express.static(EXAMPLE));
app.use("/", express.static(DIST));

app.listen(PORT, () => {
  console.error(`wasm test runner listening on ${BASE_URL}`);
});

// `cargo xtask test-wasm` asks the runner to stop when the run is over, so the
// browser is closed rather than left behind. A signal does the same, for a run
// that is interrupted.
async function shutdown() {
  await browser.close();
  rmSync(BROWSER_HOME, { recursive: true, force: true });
  process.exit(0);
}

app.get("/shutdown", async (_req, res) => {
  res.sendStatus(200);
  await shutdown();
});

for (const signal of ["SIGTERM", "SIGINT"]) {
  process.on(signal, shutdown);
}
