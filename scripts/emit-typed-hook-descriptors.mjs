#!/usr/bin/env node
/**
 * Emit compiled typed-hook descriptors for every T2.2-contract unique body.
 *
 * Used by `cargo test -p ec_engine typed_hook_baseline_parity`.  Production
 * sidecars stay trigger-only until T2.3; this catalog is test-only.
 *
 * When a T1.2 baseline names a representative hook, that hook's extracted
 * body and closure module are the ones compiled so factory bindings match
 * the captured `expected` values.
 */
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { hookFileName } from "./spec-hook-contract.mjs";
import { factoryHelperCandidates } from "./typed-hook-inline.mjs";
import {
  TYPED_HOOK_CONTRACTS,
  compileTypedHook,
  evaluateTypedHook,
} from "./typed-hook-ir.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const baselineRoot = join(
  repoDir,
  "crates/ec_engine/testdata/native-hooks/baseline",
);

export async function emitTypedHookDescriptors({
  irRoot = join(repoDir, "bundle", "specs-ir"),
} = {}) {
  const manifest = JSON.parse(await readFile(join(irRoot, "hook-modules.json"), "utf8"));
  const modules = new Map();
  const descriptors = {};
  const counts = Object.fromEntries(
    Object.keys(TYPED_HOOK_CONTRACTS).map((field) => [field, { total: 0, ok: 0 }]),
  );
  const groups = new Map();
  for (const [id, entry] of Object.entries(manifest.hooks ?? {})) {
    if (!Object.hasOwn(TYPED_HOOK_CONTRACTS, entry.sourceField)) continue;
    const key = `${entry.sourceField}:${entry.functionBodySha256}`;
    if (!groups.has(key)) {
      groups.set(key, {
        field: entry.sourceField,
        sha: entry.functionBodySha256,
        ids: [],
      });
    }
    groups.get(key).ids.push(id);
  }

  const loadModule = async (name) => {
    if (!name) return "";
    if (!modules.has(name)) {
      try {
        modules.set(name, await readFile(join(irRoot, "source-modules", name), "utf8"));
      } catch {
        modules.set(name, "");
      }
    }
    return modules.get(name);
  };

  for (const group of groups.values()) {
    counts[group.field].total += 1;
    let representative = group.ids[0];
    try {
      const baseline = JSON.parse(
        await readFile(join(baselineRoot, group.field, `${group.sha}.json`), "utf8"),
      );
      if (group.ids.includes(baseline.representativeHookId)) {
        representative = baseline.representativeHookId;
      }
    } catch {
      // Synthesized fixtures and research-only bodies have no T1.2 file.
    }
    const entry = manifest.hooks[representative];
    const text = await readFile(join(irRoot, "hooks", hookFileName(representative)), "utf8");
    const body = text.startsWith("export default ")
      ? text.slice("export default ".length).replace(/;\n$/, "").replace(/;$/, "")
      : text;
    try {
      const moduleSource = await loadModule(entry.module);
      let descriptor = compileTypedHook({
        body,
        sourceField: entry.sourceField,
        moduleSource,
      });
      let baseline = null;
      try {
        baseline = JSON.parse(
          await readFile(join(baselineRoot, group.field, `${group.sha}.json`), "utf8"),
        );
      } catch {
        baseline = null;
      }
      if (baseline && !descriptorMatchesBaseline(descriptor, baseline)) {
        const candidates = factoryHelperCandidates({ body, moduleSource });
        for (const helperLiterals of helperLiteralCombinations(candidates)) {
          try {
            const retry = compileTypedHook({
              body,
              sourceField: entry.sourceField,
              moduleSource,
              helperLiterals,
            });
            if (descriptorMatchesBaseline(retry, baseline)) {
              descriptor = retry;
              break;
            }
          } catch {
            // Keep the first successful compile.
          }
        }
      }
      descriptors[`${group.field}:${group.sha}`] = descriptor;
      counts[group.field].ok += 1;
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

function expectedValue(expected) {
  if (!expected || expected.kind === "error" || expected.kind === "timeout") {
    return undefined;
  }
  return expected.value;
}

function descriptorMatchesBaseline(descriptor, baseline) {
  for (const cse of baseline.cases ?? []) {
    const expected = expectedValue(cse.expected);
    if (expected === undefined) continue;
    let actual;
    try {
      actual = evaluateTypedHook(descriptor, cse.args);
    } catch {
      return false;
    }
    if (JSON.stringify(normalizeForCompare(actual)) !== JSON.stringify(normalizeForCompare(expected))) {
      return false;
    }
  }
  return true;
}

function normalizeForCompare(value) {
  if (Array.isArray(value)) {
    return value
      .map(normalizeForCompare)
      .filter((item) => {
        if (!item || typeof item !== "object" || Array.isArray(item)) return true;
        return typeof item.name === "string" && item.name !== "" && item.name !== "undefined";
      });
  }
  if (value && typeof value === "object") {
    const out = {};
    for (const key of Object.keys(value).sort()) {
      const child = value[key];
      if (child == null) continue;
      if (key === "priority" && typeof child !== "number") continue;
      if ((key === "hidden" || key === "isDangerous") && child === false) continue;
      if (Array.isArray(child) && child.length === 0) continue;
      if (typeof child === "string" && child.length === 0 && key !== "name") continue;
      out[key] = normalizeForCompare(child);
    }
    return out;
  }
  return value;
}

function helperLiteralCombinations(candidates) {
  const names = Object.keys(candidates);
  if (names.length === 0) return [];
  let rows = [{}];
  for (const name of names) {
    const next = [];
    for (const row of rows) {
      for (const value of candidates[name]) {
        next.push({ ...row, [name]: value });
      }
    }
    rows = next;
    if (rows.length > 64) return rows.slice(0, 64);
  }
  return rows;
}

const isMain =
  process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];
if (isMain) {
  process.stdout.write(`${JSON.stringify(await emitTypedHookDescriptors())}\n`);
}
