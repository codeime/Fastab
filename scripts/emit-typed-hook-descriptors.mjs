#!/usr/bin/env node
/**
 * Emit compiled typed-hook descriptors for every T2.2-contract unique body.
 *
 * Used by `cargo test -p ec_engine typed_hook_baseline_parity`.  Production
 * sidecars stay trigger-only until T2.3; this catalog is test-only.
 */
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { hookFileName } from "./spec-hook-contract.mjs";
import { TYPED_HOOK_CONTRACTS, compileTypedHook } from "./typed-hook-ir.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");

export async function emitTypedHookDescriptors({
  irRoot = join(repoDir, "bundle", "specs-ir"),
} = {}) {
  const manifest = JSON.parse(await readFile(join(irRoot, "hook-modules.json"), "utf8"));
  const modules = new Map();
  const descriptors = {};
  const counts = Object.fromEntries(
    Object.keys(TYPED_HOOK_CONTRACTS).map((field) => [field, { total: 0, ok: 0 }]),
  );
  const seen = new Set();
  for (const [id, entry] of Object.entries(manifest.hooks ?? {})) {
    if (!Object.hasOwn(TYPED_HOOK_CONTRACTS, entry.sourceField)) continue;
    const key = `${entry.sourceField}:${entry.functionBodySha256}`;
    if (seen.has(key)) continue;
    seen.add(key);
    counts[entry.sourceField].total += 1;
    const text = await readFile(join(irRoot, "hooks", hookFileName(id)), "utf8");
    const body = text.startsWith("export default ")
      ? text.slice("export default ".length).replace(/;\n$/, "").replace(/;$/, "")
      : text;
    let moduleSource = "";
    if (entry.module) {
      if (!modules.has(entry.module)) {
        try {
          modules.set(
            entry.module,
            await readFile(join(irRoot, "source-modules", entry.module), "utf8"),
          );
        } catch {
          modules.set(entry.module, "");
        }
      }
      moduleSource = modules.get(entry.module);
    }
    try {
      descriptors[key] = compileTypedHook({
        body,
        sourceField: entry.sourceField,
        moduleSource,
      });
      counts[entry.sourceField].ok += 1;
    } catch {
      // T2.4 / later.
    }
  }
  return {
    version: 1,
    kind: "typed-hook-baseline-descriptors",
    counts,
    descriptors,
  };
}

const isMain =
  process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];
if (isMain) {
  process.stdout.write(`${JSON.stringify(await emitTypedHookDescriptors())}\n`);
}
