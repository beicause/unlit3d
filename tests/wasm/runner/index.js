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

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import express from "express";
import { chromium } from "playwright";

const HERE = path.dirname(fileURLToPath(import.meta.url));
/** The page and the wasm modules, assembled by `cargo xtask test-wasm`. */
const DIST = path.join(HERE, "..", "dist");

/** Where a frame that failed its comparison is written.
 *
 * The page has no filesystem, so the wasm side hands the encoded frame here
 * through a window property and this process — which has one — writes it. CI
 * uploads the directory, so a mismatch arrives as an image to look at rather
 * than only as a score. The harness's own `UNLIT3D_SNAPSHOT_MISMATCH_DIR`
 * renames it, and `cargo xtask test-wasm` points it at the wasm pass's own
 * directory so a host run's frames are not mixed in with a browser run's.
 */
const MISMATCH_DIR =
  process.env.UNLIT3D_SNAPSHOT_MISMATCH_DIR ??
  path.resolve(HERE, "..", "..", "..", "target", "snapshot-mismatches-wasm");

const PORT = Number(process.env.PORT ?? 3000);
/**
 * Which of the browser's GPU backends this process serves.
 *
 * The wasm binary asks for WebGPU and falls back to WebGL2 on its own, so the
 * browser's launch flags are the whole of the choice: a pass run against a
 * browser without WebGPU is a WebGL2 pass. It has to be named rather than
 * defaulted, because a pass that silently ran the other backend would report
 * green while exercising nothing it was started for — the same trap
 * `UNLIT3D_DEVICE_TIER` exists to avoid on the host.
 */
const BACKEND_ENV = "UNLIT3D_WASM_BACKEND";
const BACKEND_ARGS = {
  webgpu: [
    // Chrome gates WebGPU behind this even when the adapter is its own
    // software Vulkan rather than a real device.
    "--enable-unsafe-webgpu",
    // Vulkan has to be an enabled feature, not only a chosen backend: without
    // it the browser reaches WebGPU through SwiftShader all the same, but the
    // canvas it draws into is never composited and a windowed run reads back
    // transparency. Chrome's own WebGPU documentation adds the same flag.
    "--enable-features=Vulkan",
    // The rest pins both ANGLE and WebGPU to that SwiftShader Vulkan device, so
    // the pass does not depend on a GPU being present and does not quietly
    // pick a different one when it is.
    "--use-angle=vulkan",
    "--use-vulkan=swiftshader",
    "--use-webgpu-adapter=swiftshader",
    "--disable-vulkan-surface",
    // A headed browser runs Chrome's GPU watchdog, and a software Vulkan
    // device under the load of several tests at once can leave the GPU main
    // thread without progress long enough for it to fire: it exits the process
    // with RESULT_CODE_HUNG, which the browser logs as
    // "GPU process exited unexpectedly: exit_code=512" and every device in the
    // browser dies with it. A kill turns slowness into a cascade of failed
    // tests, and the harness already bounds each one. (A headless browser has
    // no watchdog, so the WebGL2 pass never sees this. See WORKAROUND.md,
    // "A headed browser's GPU watchdog kills the GPU process under load".
    "--disable-gpu-watchdog",
  ],
  webgl2: [],
};
/**
 * The backends whose canvas only shows anything in a window.
 *
 * A WebGPU canvas reaches the compositor through a shared image the software
 * Vulkan devices cannot back, so a headless browser renders into it and then
 * hands back transparency: the tests still pass — they read their frames from
 * offscreen textures — but the example check, which reads the canvas, would
 * fail on a frame that was drawn. See WORKAROUND.md, "A WebGPU canvas reads
 * back transparent in a headless browser".
 */
const BACKENDS_NEEDING_A_WINDOW = new Set(["webgpu"]);
const BACKEND = process.env[BACKEND_ENV] ?? "";
if (!Object.hasOwn(BACKEND_ARGS, BACKEND)) {
  console.error(
    `${BACKEND_ENV}=${JSON.stringify(BACKEND)} names no browser backend; ` +
      `set it to one of ${Object.keys(BACKEND_ARGS).join(", ")}`,
  );
  process.exit(1);
}
/**
 * How long one test may take.
 *
 * Software WebGL is slow and a shared CI runner is slower still: the
 * interactive example steps over 600 frames before it reports, and that has
 * been measured past the minute-and-a-half mark on a loaded runner. This
 * stays under the wasm nextest profile's slow-timeout, so a wedged page
 * fails with the browser's own log rather than a bare kill from the harness.
 */
const TIMEOUT_MS = Number(process.env.TIMEOUT ?? 300_000);
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

/**
 * A private X server, for a backend whose canvas only shows inside a window.
 *
 * A CI runner has no display, and a browser started there has no window to
 * composite into — which for the WebGPU backend means the canvas stays
 * transparent however the flags are set. Xvfb gives the browser a display to
 * draw in without one being attached to the machine. The server picks its own
 * display number and reports it once it is ready, so two runs on one machine
 * cannot collide on a number.
 */
async function startXvfb() {
  const server = spawn(
    "Xvfb",
    // A 24-bit screen, large enough for the example pages' 800x600 viewport.
    ["-displayfd", "1", "-screen", "0", "1280x1024x24", "-nolisten", "tcp"],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  const display = await new Promise((resolve, reject) => {
    let written = "";
    server.stdout.on("data", (chunk) => {
      written += chunk;
      const ready = /^(\d+)\s*$/m.exec(written);
      if (ready) resolve(":" + ready[1]);
    });
    server.stderr.on("data", (chunk) => {
      // Tagged, so the server's own complaints are not read as the browser's.
      process.stderr.write("xvfb: " + chunk);
    });
    server.on("error", reject);
    server.on("exit", (code) =>
      reject(new Error("Xvfb exited with " + code + " before reporting a display")),
    );
  });
  return { server, display };
}

// Only the backends in the set above get a window; the rest stay headless,
// which is both faster and leaves less to go wrong on a machine with no display.
const NEEDS_WINDOW = BACKENDS_NEEDING_A_WINDOW.has(BACKEND);
// Whether this run draws into a window at all — either because the backend
// cannot draw anywhere else, or because `--show` was asked for.
const HEADED = SHOW || NEEDS_WINDOW;
// An X server is a Linux matter. Where one is needed it is started privately
// rather than borrowed: a display the run did not make may be a Wayland
// session's Xwayland, and is gone when the session ends. `--show` is the
// exception, its point being a window the developer watches, which only the
// host's own display puts in front of them.
const PRIVATE_DISPLAY =
  HEADED && process.platform === "linux" && !(SHOW && process.env.DISPLAY);
const xvfb = PRIVATE_DISPLAY
  ? await startXvfb().catch((error) => {
      console.error(
        `${BACKEND_ENV}=${BACKEND} needs a window, so an X server is required: ` +
          `could not start Xvfb (${error}). Install it, or run under a display yourself.`,
      );
      process.exit(1);
    })
  : null;

// Chrome picks a display backend from the environment, and picks Wayland over
// X when both are offered. Wayland's route to the compositor needs a DRM
// device, which the software Vulkan device this pass draws with does not have,
// so the browser is kept on X: the private server where there is one, the
// host's own X session otherwise.
const browserEnv = {
  ...process.env,
  HOME: BROWSER_HOME,
  XDG_CACHE_HOME: path.join(BROWSER_HOME, "cache"),
  XDG_CONFIG_HOME: path.join(BROWSER_HOME, "config"),
};
if (HEADED) delete browserEnv.WAYLAND_DISPLAY;
if (xvfb) browserEnv.DISPLAY = xvfb.display;
// X11 ozone is the Linux window system's; elsewhere the browser has its own,
// and naming a platform it does not have would be ignored at best.
const OZONE_X11 = HEADED && process.platform === "linux";

/**
 * Launch the browser this run drives.
 *
 * A launch that throws — a missing binary, a `/dev/shm` too small to map —
 * would otherwise leave the private X server and the browser home behind,
 * since a top-level await that rejects ends the process before the shutdown
 * route exists to clean either up. Both are removed here instead.
 */
async function launchBrowser() {
  try {
    return await chromium.launch({
      // A window is the whole point for the backends that need one: what they
      // draw only reaches a screen there, so `--show` cannot make those
      // headless.
      headless: !HEADED,
      // An explicit binary lets a developer reuse a browser they already have
      // instead of downloading one.
      executablePath: process.env.CHROME_PATH || undefined,
      env: browserEnv,
      args: [
        // A container often has no usable sandbox.
        "--no-sandbox",
        // `/dev/shm` is small in a container, and Chrome needs more than it
        // offers.
        "--disable-dev-shm-usage",
        // Software rendering for both backends, so a machine with no GPU still
        // runs these tests.
        "--enable-unsafe-swiftshader",
        ...BACKEND_ARGS[BACKEND],
        // The X server above, rather than whichever display the host happens
        // to have offered the browser.
        ...(OZONE_X11 ? ["--ozone-platform=x11"] : []),
      ],
    });
  } catch (error) {
    release();
    console.error(`could not launch the browser: ${error}`);
    process.exit(1);
  }
}

/** Remove what this process made that the system would not clean up itself. */
function release() {
  xvfb?.server.kill();
  rmSync(BROWSER_HOME, { recursive: true, force: true });
}

const browser = await launchBrowser();

const app = express();

/**
 * Write the frame a failed comparison left in the page, if it left one.
 *
 * The name is the snapshot's, a path relative to the snapshot directory, so a
 * frame keeps the scene or test it belongs to instead of colliding on a file
 * name. Nothing is written when the test failed for another reason — a panic
 * with no comparison behind it — and a failure to write is reported rather than
 * raised, since the test's own failure is what the caller must still see.
 */
async function saveMismatch(page) {
  let record;
  try {
    record = await page.evaluate(() => {
      const found = window.__unlit3d_mismatch;
      return found ? { name: found.name, bytes: Array.from(found.bytes) } : null;
    });
  } catch {
    // The page may already be gone; the test's failure is reported either way.
    return;
  }
  if (!record) return;

  try {
    const target = path.join(MISMATCH_DIR, record.name);
    mkdirSync(path.dirname(target), { recursive: true });
    writeFileSync(target, Buffer.from(record.bytes));
    console.error(`wrote the mismatched frame to ${target}`);
  } catch (error) {
    console.error(`could not write the mismatched frame: ${error}`);
  }
}

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
      // A frame that failed a comparison is written here, because the page had
      // nowhere to put it. Collected under the mismatch directory so a CI job
      // can upload what failed, the same way the host pass does.
      await saveMismatch(page);
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

// The device the browser draws with, in the same JSON `cargo xtask print-gpu`
// prints. The page publishes it into `sessionStorage` for the same reason a
// test's verdict goes there: a wasm export returns to a page that has moved
// on, so the value has to outlive the call.
app.get("/gpu_info", async (req, res) => {
  const module = String(req.query.wasm ?? "");
  console.error(`describe the device ${module} draws with`);

  const context = await browser.newContext();
  context.setDefaultTimeout(TIMEOUT_MS);
  const page = await context.newPage();

  const logs = [];
  page.on("console", (message) => logs.push(`[${message.type()}] ${message.text()}`));
  page.on("pageerror", (error) => logs.push(`[pageerror] ${error.message}`));

  try {
    const url = new URL(BASE_URL);
    url.search = new URLSearchParams({ wasm: module, info: "1" }).toString();
    await page.goto(url.toString());
    const info = await page.waitForFunction(() => window.sessionStorage.gpu_info);
    res.send(await info.jsonValue());
  } catch (error) {
    res.status(500).send(`${error?.stack ?? error}\n\n${logs.join("\n")}`);
  } finally {
    await context.close();
  }
});

// The examples' own pages, built and bindgened by `cargo xtask test-wasm`.
// Serving them here is what lets one route load either in a browser.
const EXAMPLES = path.join(HERE, "..", "examples");

// Load one example and check that it really draws something.
//
// The GPU tests above exercise the rendering paths directly, but they never
// start the example — the program a browser actually runs — so a change that
// compiles and still leaves the page blank passes them. This is the check that
// the example boots, finds a backend and puts pixels on its canvas.
//
// The `app` query names the example's directory under `examples/`; the harness
// passes the same names `run-wasm` publishes them under.
//
// A rendered scene is neither empty nor flat, so both are tested: an unstarted
// canvas is transparent, and a canvas that never drew reads as one color.
app.get("/run_example", async (req, res) => {
  const app = String(req.query.app ?? "");
  console.error(`run the ${app} example`);

  const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
  context.setDefaultTimeout(TIMEOUT_MS);
  const page = await context.newPage();

  const logs = [];
  page.on("console", (message) => logs.push(`[${message.type()}] ${message.text()}`));
  page.on("pageerror", (error) => logs.push(`[pageerror] ${error.message}`));

  try {
    await page.goto(`${BASE_URL}/examples/${app}/`);

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

app.use("/examples", express.static(EXAMPLES));
app.use("/", express.static(DIST));

app.listen(PORT, () => {
  console.error(`wasm test runner listening on ${BASE_URL} for ${BACKEND}`);
});

/** How long the browser is given to close before the process goes anyway. */
const CLOSE_TIMEOUT = 10_000;

// `cargo xtask test-wasm` asks the runner to stop when the run is over, so the
// browser is closed rather than left behind. A signal does the same, for a run
// that is interrupted.
//
// The close is bounded: `--disable-gpu-watchdog` leaves nothing to kill a
// wedged GPU process, so a close waiting on one may never return, and the
// harness kills this process a moment later — leaving that browser orphaned.
// Exiting first is the lesser evil.
async function shutdown() {
  await Promise.race([
    browser.close().catch(() => {}),
    new Promise((resolve) => setTimeout(resolve, CLOSE_TIMEOUT)),
  ]);
  release();
  process.exit(0);
}

app.get("/shutdown", async (_req, res) => {
  res.sendStatus(200);
  await shutdown();
});

for (const signal of ["SIGTERM", "SIGINT"]) {
  process.on(signal, shutdown);
}
