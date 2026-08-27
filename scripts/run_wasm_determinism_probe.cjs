"use strict";

const path = require("node:path");

if (
  process.argv.length < 3 ||
  process.argv.length > 4 ||
  (process.argv.length === 4 && process.argv[3] !== "--release-identity")
) {
  console.error(
    "usage: node run_wasm_determinism_probe.cjs <wasm-bindgen-module> [--release-identity]",
  );
  process.exit(2);
}

const bindings = require(path.resolve(process.argv[2]));
const report =
  process.argv[3] === "--release-identity"
    ? bindings.afc_release_identity_json()
    : bindings.afc_determinism_probe_json();
if (typeof report !== "string" || report.length === 0) {
  throw new Error("the WASM probe returned an invalid report");
}
process.stdout.write(`${report}\n`);
