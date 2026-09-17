/**
 * Shared helpers for hook output baseline fixtures. Build/test only: every
 * hook run goes through reference-hook-worker.mjs and never starts a CLI.
 */
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { classifyNativeHooks } from "./classify-native-hooks.mjs";
import { captureHookReference, captureHookReferenceBatch } from "./capture-hook-reference.mjs";
import {
  BASELINE_KIND,
  BASELINE_VERSION,
  FIELD_CONTRACTS,
  baselineRelativePath,
  fieldContract,
  validateBaseline,
  validateInputFixture,
} from "./hook-baseline-contract.mjs";
import { comparePath } from "./spec-pair.mjs";

export const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
export const nativeHooksRoot = join(
  repoDir,
  "crates",
  "ec_engine",
  "testdata",
  "native-hooks",
);
export const inputRoot = join(nativeHooksRoot, "inputs");
export const baselineRoot = join(nativeHooksRoot, "baseline");
export const cliOutputRoot = join(nativeHooksRoot, "cli-output");

export const DEFAULT_TIMEOUT_MS = 5000;
export const TIMEOUT_CASE_MS = 50;
export const TIMEOUT_DELAY_MS = 10_000;
export const DEFAULT_CONTEXT = Object.freeze({
  currentWorkingDirectory: "/repo",
  currentProcess: "zsh",
  sshPrefix: "",
  environmentVariables: Object.freeze({
    HOME: "/home/user",
    PATH: "/usr/bin:/bin",
  }),
  searchTerm: "",
  isDangerous: false,
});

export const MIXED_TEMPLATE_SUGGESTIONS = Object.freeze([
  { name: "README.md", type: "file" },
  { name: "src/", type: "folder" },
  { name: "package.json", type: "file" },
  { name: "dist/", type: "folder" },
  { name: ".env", type: "file" },
  { name: "bin/", type: "folder" },
  { name: "LICENSE", type: "file" },
  { name: "node_modules/", type: "folder" },
]);

export function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

export function argvDigest(argv) {
  return sha256(JSON.stringify(argv));
}

export function stableStringify(value) {
  return `${JSON.stringify(sortKeys(value), null, 2)}\n`;
}

export function sortKeys(value) {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort(comparePath)
        .map((key) => [key, sortKeys(value[key])]),
    );
  }
  return value;
}

export function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

export function inputRelativePath(field, bodySha256) {
  return ["crates", "ec_engine", "testdata", "native-hooks", "inputs", field, `${bodySha256}.json`].join(
    "/",
  );
}

export function defaultContext(searchTerm = "") {
  return {
    ...DEFAULT_CONTEXT,
    environmentVariables: { ...DEFAULT_CONTEXT.environmentVariables },
    searchTerm,
  };
}

export function workerArgsForCase(field, args, context) {
  const contract = fieldContract(field);
  const out = cloneJson(args);
  if (contract.execIndex != null) {
    out.splice(contract.execIndex, 0, { $referenceExec: true });
  }
  if (field === "custom") {
    out.push(cloneJson(context));
  }
  return out;
}

export function mockExecRulesFromCase(exec) {
  return (exec ?? []).map((rule) => {
    const out = {};
    if (rule.command != null) out.command = rule.command;
    if (Array.isArray(rule.args)) out.args = rule.args;
    if (rule.stdout != null) out.stdout = rule.stdout;
    if (rule.stderr != null) out.stderr = rule.stderr;
    if (rule.status != null) out.status = rule.status;
    if (rule.delayMs != null) out.delayMs = rule.delayMs;
    return out;
  });
}

export function firstName(node) {
  if (!node || typeof node !== "object") return null;
  if (Array.isArray(node.names) && typeof node.names[0] === "string" && node.names[0]) {
    return node.names[0];
  }
  if (typeof node.name === "string" && node.name) return node.name;
  if (Array.isArray(node.name) && typeof node.name[0] === "string" && node.name[0]) {
    return node.name[0];
  }
  return null;
}

export function parseIrPath(path) {
  if (typeof path !== "string" || !path.startsWith("root")) return [];
  const parts = [];
  const re = /\.([A-Za-z_$][\w$]*)|\[(\d+)\]/g;
  let match;
  while ((match = re.exec(path))) {
    parts.push(match[1] ?? Number(match[2]));
  }
  return parts;
}

export function tokensFromIr(spec, path) {
  const tokens = [];
  const rootName = firstName(spec);
  if (rootName) tokens.push(rootName);
  let node = spec;
  const parts = parseIrPath(path ?? "");
  for (let index = 0; index < parts.length; index += 1) {
    const key = parts[index];
    if (!node || typeof node !== "object") break;
    if (key === "subcommands" && typeof parts[index + 1] === "number") {
      node = Array.isArray(node.subcommands) ? node.subcommands[parts[index + 1]] : null;
      const name = firstName(node);
      if (name) tokens.push(name);
      index += 1;
      continue;
    }
    if (
      key === "args" ||
      key === "options" ||
      key === "persistentOptions" ||
      key === "generators" ||
      key === "additionalSuggestions"
    ) {
      break;
    }
    node = node[key];
  }
  tokens.push("");
  return tokens.length > 1 ? tokens : [rootName ?? "cmd", ""];
}

export async function loadCliOutput(command, args) {
  const argv = [command, ...(args ?? [])];
  const file = join(cliOutputRoot, `${argvDigest(argv)}.json`);
  try {
    const sample = JSON.parse(await readFile(file, "utf8"));
    if (!sample || typeof sample !== "object") return null;
    return {
      command,
      args: [...(args ?? [])],
      stdout: typeof sample.stdout === "string" ? sample.stdout : "",
      stderr: typeof sample.stderr === "string" ? sample.stderr : "",
      status: Number.isInteger(sample.status) ? sample.status : 127,
    };
  } catch {
    return null;
  }
}

export function missingCommandRule(command, args) {
  return {
    command,
    args: [...(args ?? [])],
    stdout: "",
    stderr: "command not found",
    status: 127,
  };
}

export function normalizeSuggestion(item) {
  if (typeof item === "string") {
    if (!item) throw new Error("empty string suggestion");
    return item;
  }
  if (!item || typeof item !== "object" || Array.isArray(item)) {
    throw new Error("suggestion must be a string or object");
  }
  const out = {};
  if (typeof item.name === "string" && item.name) {
    out.name = item.name;
  } else if (Array.isArray(item.name) && item.name.every((part) => typeof part === "string" && part)) {
    out.name = item.name;
  } else {
    throw new Error("suggestion is missing name");
  }
  for (const key of ["displayName", "insertValue", "description", "icon", "type", "replaceValue"]) {
    if (typeof item[key] === "string" && item[key]) out[key] = item[key];
  }
  if (Number.isInteger(item.priority)) out.priority = item.priority;
  for (const key of ["hidden", "isDangerous", "deprecated", "shouldAddSpace"]) {
    if (typeof item[key] === "boolean") out[key] = item[key];
  }
  if (Object.hasOwn(item, "args") && item.args != null) out.args = item.args;
  return out;
}

export function expectedFromResult(field, result) {
  if (result?.status === "timeout") return { kind: "timeout" };
  if (result?.status !== "success") {
    return {
      kind: "error",
      value: String(result?.errorClass ?? result?.status ?? "error"),
    };
  }
  const resultKind = FIELD_CONTRACTS[field].resultKind;
  try {
    return { kind: resultKind, value: coerceResult(resultKind, result.value) };
  } catch (error) {
    return {
      kind: "error",
      value: error instanceof Error ? error.message : String(error),
    };
  }
}

function coerceResult(kind, value) {
  switch (kind) {
    case "suggestions":
      if (!Array.isArray(value)) throw new Error("suggestions result is not an array");
      return value.map((item) => normalizeSuggestion(item));
    case "string":
      if (typeof value !== "string") throw new Error("string result is not a string");
      return value;
    case "bool":
      if (typeof value !== "boolean") throw new Error("bool result is not a boolean");
      return value;
    case "argv":
      return coerceArgv(value);
    case "spec":
      return coerceSpec(value);
    default:
      throw new Error(`unhandled result kind ${kind}`);
  }
}

function coerceArgv(value) {
  if (typeof value === "string") return value;
  if (Array.isArray(value) && value.every((item) => typeof item === "string")) {
    return value;
  }
  if (value && typeof value === "object" && typeof value.command === "string") {
    const args = Array.isArray(value.args) ? value.args : [];
    if (args.some((item) => typeof item !== "string")) {
      throw new Error("argv args must be strings");
    }
    return { command: value.command, args };
  }
  throw new Error("argv result is not a command");
}

function coerceSpec(value) {
  if (typeof value === "string" && value) return { name: value };
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const names = firstName(value);
    if (!names) throw new Error("spec result is missing name");
    return value;
  }
  throw new Error("spec result is not Fig JSON");
}

export function timeoutCase(field, args, context) {
  return {
    id: "timeout",
    args: cloneJson(args),
    exec: [{ delayMs: TIMEOUT_DELAY_MS }],
    context: context ?? defaultContext(),
    timeoutMs: TIMEOUT_CASE_MS,
  };
}

export async function loadBodyGroups(options = {}) {
  const report = await classifyNativeHooks(options);
  const groups = report.bodyGroups.filter((group) => group.bodySha256);
  if (groups.length !== report.coverage.uniqueBodies) {
    throw new Error(
      `body group count ${groups.length} != uniqueBodies ${report.coverage.uniqueBodies}`,
    );
  }
  return { report, groups };
}

export function locationForHook(report, hookId) {
  const hook = report.hooks.find((item) => item.id === hookId);
  return hook?.locations?.[0] ?? null;
}

export async function loadIrSpec(irRoot, sourceFile) {
  if (typeof sourceFile !== "string") return null;
  const irFile = sourceFile.replace(/\.js$/, ".json");
  try {
    return JSON.parse(await readFile(join(irRoot, irFile), "utf8"));
  } catch {
    return null;
  }
}

export async function tokensForGroup(report, group, irRoot) {
  const hookId = group.sampleHookIds?.[0] ?? group.hookIds?.[0];
  const location = locationForHook(report, hookId);
  const spec = location ? await loadIrSpec(irRoot, location.source) : null;
  if (spec && location?.path) return tokensFromIr(spec, location.path);
  const root = typeof hookId === "string" ? hookId.split("#")[0] : "cmd";
  return [root, ""];
}

export async function recordExecRules({
  hookId,
  field,
  args,
  context,
  sourceRoot,
  irRoot,
  timeoutMs = DEFAULT_TIMEOUT_MS,
}) {
  const workerArgs = workerArgsForCase(field, args, context);
  const rules = [];
  for (let attempt = 0; attempt < 16; attempt += 1) {
    const result = await captureHookReference({
      hookId,
      args: workerArgs,
      mockExecRules: rules,
      timeoutMs,
      sourceRoot,
      irRoot,
    });
    if (result.status === "success") return rules;
    const trace = Array.isArray(result.execTrace) ? result.execTrace : [];
    let added = false;
    for (const call of trace) {
      const command = call?.command == null ? "" : String(call.command);
      const callArgs = Array.isArray(call?.args)
        ? call.args.map((item) => (item == null ? "" : String(item)))
        : [];
      if (!command) continue;
      if (
        rules.some(
          (rule) =>
            rule.command === command && JSON.stringify(rule.args) === JSON.stringify(callArgs),
        )
      ) {
        continue;
      }
      const sample = await loadCliOutput(command, callArgs);
      rules.push(sample ?? missingCommandRule(command, callArgs));
      added = true;
    }
    if (!added) return rules;
  }
  return rules;
}

export async function captureCase({
  hookId,
  field,
  caseValue,
  sourceRoot,
  irRoot,
}) {
  const result = await captureHookReference({
    hookId,
    args: workerArgsForCase(field, caseValue.args, caseValue.context),
    mockExecRules: mockExecRulesFromCase(caseValue.exec),
    timeoutMs: caseValue.timeoutMs,
    sourceRoot,
    irRoot,
  });
  return expectedFromResult(field, result);
}

export async function captureCases({
  hookId,
  field,
  cases,
  sourceRoot,
  irRoot,
}) {
  const expected = new Array(cases.length);
  const batchable = [];
  cases.forEach((item, index) => {
    if (item.timeoutMs < 100 || item.id === "timeout") {
      return;
    }
    batchable.push({ index, item });
  });
  if (batchable.length > 0) {
    const batch = await captureHookReferenceBatch({
      hookId,
      invocations: batchable.map(({ item }) => ({
        args: workerArgsForCase(field, item.args, item.context),
        mockExecRules: mockExecRulesFromCase(item.exec),
      })),
      timeoutMs: Math.max(...batchable.map(({ item }) => item.timeoutMs), DEFAULT_TIMEOUT_MS),
      sourceRoot,
      irRoot,
    });
    const runs = Array.isArray(batch.runs) ? batch.runs : [];
    batchable.forEach(({ index }, runIndex) => {
      expected[index] = expectedFromResult(field, runs[runIndex] ?? batch);
    });
  }
  for (const [index, item] of cases.entries()) {
    if (expected[index]) continue;
    expected[index] = await captureCase({
      hookId,
      field,
      caseValue: item,
      sourceRoot,
      irRoot,
    });
  }
  return expected;
}

export function fixtureIdentity(group) {
  return {
    version: BASELINE_VERSION,
    kind: BASELINE_KIND,
    field: group.sourceField,
    bodySha256: group.bodySha256,
    representativeHookId: group.sampleHookIds?.[0] ?? group.hookIds?.[0],
    hookCount: group.hookCount,
  };
}

export function finalizeInputFixture(value) {
  const fixture = sortKeys(value);
  validateInputFixture(fixture);
  return fixture;
}

export function finalizeBaseline(value) {
  const baseline = sortKeys(value);
  validateBaseline(baseline);
  return baseline;
}

export { baselineRelativePath, validateBaseline, validateInputFixture };
