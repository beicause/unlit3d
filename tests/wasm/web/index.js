// The page half of the wasm test harness.
//
// The runner opens this page once per test as `?wasm=<module>&name=<test>`. It
// loads that module's wasm-bindgen output, hands the test's name to the export
// the harness generated, and leaves the verdict to the Rust side, which
// publishes it into `sessionStorage` — where the runner is watching for it.

export async function start() {
  const params = new URL(window.location.href).searchParams;
  const name = params.get("name");
  const module = params.get("wasm");
  if (!name || !module) {
    throw new Error("the test page needs both a `wasm` module and a `name`");
  }

  // The map is written beside the built modules, so the page never has to
  // guess a path from a name the query string supplied.
  const paths = await (await fetch("./wasm_paths.json")).json();
  const script = paths[module];
  if (!script) {
    throw new Error(`no wasm module named ${module} in wasm_paths.json`);
  }

  const { default: init, run_test } = await import(script);
  await init();

  // Deliberately not awaited: the export starts the test on the page's own
  // task queue and returns immediately, because the test's device request is
  // a promise the page has to yield to. Its result arrives through
  // `sessionStorage`, not as a return value.
  run_test(name);
}
