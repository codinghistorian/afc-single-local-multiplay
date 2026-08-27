"use strict";

const path = require("node:path");

if (process.argv.length !== 3) {
  console.error("usage: node run_wasm_determinism_probe.cjs <wasm-bindgen-module>");
  process.exit(2);
}

const bindings = require(path.resolve(process.argv[2]));
const report = bindings.afc_determinism_probe_json();
if (typeof report !== "string" || report.length === 0) {
  throw new Error("the WASM determinism probe returned an invalid report");
}
process.stdout.write(`${report}\n`);
