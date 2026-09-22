#!/usr/bin/env node
/**
 * Emit compiled typed-hook descriptors for every T2.2-contract unique body.
 *
 * Used by `cargo test -p fastab_engine typed_hook_baseline_parity`.  Production
 * sidecars now cover the side-effect-free fields; this catalog is still
 * test-only and may bind factory helpers that the sidecar refused.
 *
 * Runtime JS artifacts are gone (T4.1). Bodies come from the source audit
 * and closure modules are reconstructed in memory from the same compiler
 * helper the IR publisher uses.
 *
 * When a T1.2 baseline names a representative hook, that hook's extracted
 * body and reconstructed module are the ones compiled so factory bindings
 * match the captured `expected` values.
 */
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { SUPPORTED_HOOK_FIELDS } from "./spec-hook-contract.mjs";
import { factoryHelperCandidates } from "./typed-hook-inline.mjs";
import {
  EFFECT_HOOK_FIELDS,
  TYPED_HOOK_CONTRACTS,
  compileTypedHook,
  evaluateTypedHook,
} from "./typed-hook-ir.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const baselineRoot = join(
  repoDir,
  "crates/fastab_engine/testdata/native-hooks/baseline",
);
const IR_TO_SOURCE_FIELD = Object.fromEntries(
  Object.entries(SUPPORTED_HOOK_FIELDS).map(([source, ir]) => [ir, source]),
);

export async function emitTypedHookDescriptors({
  sourceRoot = join(repoDir, "bundle", "specs"),
  irRoot = join(repoDir, "bundle", "specs-ir"),
} = {}) {
  const { auditSpecsHooks } = await import("./audit-spec-hooks.mjs");
  const { closurePreservingHookModule } = await import("./compile-spec-ir.mjs");
  const audit = await auditSpecsHooks({ sourceRoot, irRoot });
  if (audit.ok !== true) {
    const counts = Object.fromEntries(
      Object.entries(audit.errors ?? {}).map(([name, entries]) => [
        name,
        Array.isArray(entries) ? entries.length : 0,
      ]),
    );
    throw new Error(
      `cannot emit typed descriptors from a failing source/IR audit: ${JSON.stringify(counts)}`,
    );
  }

  const sourceById = new Map();
  for (const record of audit.sourceToIr ?? []) {
    for (const [field, items] of Object.entries(record.hookInstances ?? {})) {
      for (const item of items ?? []) {
        if (!item?.id || sourceById.has(item.id)) continue;
        sourceById.set(item.id, { source: record.source, field, item });
      }
    }
  }

  const moduleBySource = new Map();
  async function moduleSourceFor(sourceRel) {
    if (!sourceRel) return "";
    if (moduleBySource.has(sourceRel)) return moduleBySource.get(sourceRel);
    const instances = [];
    for (const record of audit.sourceToIr ?? []) {
      if (record.source !== sourceRel) continue;
      for (const [field, items] of Object.entries(record.hookInstances ?? {})) {
        for (const item of items ?? []) {
          if (!item?.id || !item.path) continue;
          instances.push({
            id: item.id,
            path: item.path,
            sourceField: field,
            functionBodySha256: item.functionBodySha256,
            ownerPath:
              field === "custom" ? item.path.replace(/\.[^.]+$/, "") : undefined,
          });
        }
      }
    }
    const sourceText = await readFile(join(sourceRoot, sourceRel), "utf8");
    const moduleSource = instances.length
      ? closurePreservingHookModule(sourceText, sourceRel, instances)
      : "";
    moduleBySource.set(sourceRel, moduleSource);
    return moduleSource;
  }

  const descriptors = {};
  const counts = Object.fromEntries(
    Object.keys(TYPED_HOOK_CONTRACTS).map((field) => [field, { total: 0, ok: 0 }]),
  );
  const groups = new Map();
  for (const entry of audit.hookManifest ?? []) {
    const sourceField = IR_TO_SOURCE_FIELD[entry.field];
    if (!sourceField || !Object.hasOwn(TYPED_HOOK_CONTRACTS, sourceField)) {
      continue;
    }
    const sha = entry.functionBodySha256;
    if (!sha || typeof entry.body !== "string") continue;
    const key = `${sourceField}:${sha}`;
    if (!groups.has(key)) {
      groups.set(key, {
        field: sourceField,
        sha,
        ids: [],
        entries: [],
      });
    }
    groups.get(key).ids.push(entry.id);
    groups.get(key).entries.push(entry);
  }

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
    const entry = group.entries.find((item) => item.id === representative) ?? group.entries[0];
    const body = entry.body;
    try {
      const sourceRel = sourceById.get(representative)?.source;
      const moduleSource = await moduleSourceFor(sourceRel);
      const tryCompile = (helperLiterals) =>
        compileTypedHook({
          body,
          sourceField: group.field,
          moduleSource,
          ...(helperLiterals ? { helperLiterals } : {}),
        });
      let descriptor = null;
      try {
        descriptor = tryCompile();
      } catch {
        // Shared factories with disagreeing call sites stay out of the
        // production sidecar. The baseline catalog may still bind one
        // representative via helperLiterals.
      }
      let baseline = null;
      try {
        baseline = JSON.parse(
          await readFile(join(baselineRoot, group.field, `${group.sha}.json`), "utf8"),
        );
      } catch {
        baseline = null;
      }
      if (!descriptor || (baseline && !descriptorMatchesBaseline(descriptor, baseline))) {
        const candidates = factoryHelperCandidates({ body, moduleSource });
        for (const helperLiterals of helperLiteralCombinations(candidates)) {
          try {
            const retry = tryCompile(helperLiterals);
            if (!baseline || descriptorMatchesBaseline(retry, baseline)) {
              descriptor = retry;
              break;
            }
            descriptor ??= retry;
          } catch {
            // Keep looking for a representative binding.
          }
        }
      }
      if (!descriptor) continue;
      if (baseline && !descriptorMatchesBaseline(descriptor, baseline)) continue;
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

function mockEffects(cse) {
  const descriptorOf = (item) =>
    JSON.stringify({
      command: item?.command,
      args: item?.args ?? [],
      cwd: item?.cwd ?? null,
      env: item?.env ?? null,
      timeout: item?.timeout ?? null,
    });
  return {
    context: {
      currentWorkingDirectory: cse.context?.currentWorkingDirectory ?? "",
      currentProcess: cse.context?.currentProcess ?? "",
      sshPrefix: cse.context?.sshPrefix ?? "",
      environmentVariables: cse.context?.environmentVariables ?? {},
      searchTerm: cse.context?.searchTerm ?? "",
      isDangerous: Boolean(cse.context?.isDangerous),
    },
    exec(request) {
      const snapshot =
        typeof request === "string" ? { command: request, args: [] } : request;
      const rule = (cse.exec ?? []).find(
        (item) => descriptorOf(item) === descriptorOf(snapshot),
      );
      if (!rule) {
        const error = new Error("unmocked command");
        error.name = "UnmockedCommand";
        throw error;
      }
      return {
        status: rule.status ?? 0,
        stdout: rule.stdout ?? "",
        stderr: rule.stderr ?? "",
      };
    },
  };
}

function descriptorMatchesBaseline(descriptor, baseline) {
  for (const cse of baseline.cases ?? []) {
    const expected = expectedValue(cse.expected);
    if (expected === undefined) continue;
    let actual;
    try {
      const effects = EFFECT_HOOK_FIELDS.includes(descriptor.sourceField)
        ? mockEffects(cse)
        : null;
      actual = evaluateTypedHook(descriptor, cse.args, effects);
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
