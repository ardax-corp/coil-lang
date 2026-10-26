#!/usr/bin/env node
// Run a wasm32-wasip1 build of `coil` under Node's WASI, like the native
// binary: `node scripts/wasi_coil.mjs path/to/coil.wasm [coil args…]`.
//
// The current directory is preopened as `.` (relative paths work as with a
// cwd) and the system temp directory as `/tmp`. Paths outside those two are
// not reachable, so pass roots and files relative to the current directory.
//
// Run Node with `--no-turbo-fast-api-calls`: Node 22 can collect garbage
// inside a WASI fast API call (path_open) and segfault walking the frames.
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { WASI } from "node:wasi";

const [wasm, ...args] = process.argv.slice(2);
if (!wasm) {
  console.error("usage: wasi_coil.mjs COIL_WASM [args…]");
  process.exit(2);
}
const wasi = new WASI({
  version: "preview1",
  args: ["coil", ...args],
  // No HOME: the host's home is not preopened, and caches under it would
  // land in the current directory instead.
  env: Object.fromEntries(Object.entries(process.env).filter(([k]) => k !== "HOME")),
  preopens: { ".": process.cwd(), "/tmp": tmpdir() },
  returnOnExit: true,
});
const module = await WebAssembly.compile(readFileSync(wasm));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
process.exitCode = wasi.start(instance);
