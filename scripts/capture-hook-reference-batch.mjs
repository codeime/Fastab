#!/usr/bin/env node
/**
 * Batch diagnostics for original Fig hooks and generated closure modules.
 *
 * This is deliberately a synthetic probe, not a real CLI run and not a
 * functional-equivalence baseline. Each selected audited hook instance is
 * invoked twice through each path, in four fresh reference-hook workers, with
 * one fixed, JSON-only fixture. No command is ever executed: the only
 * executor supplied to the VM is the reference worker's exact-match mock.
 */
import { readdir, realpath } from "node:fs/promises";
import { constants } from "node:fs";
import { lstat, open } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  captureHookModuleReference,
  captureHookReference,
} from "./capture-hook-reference.mjs";
import { withReferenceAudit } from "./reference-audit-worker.mjs";
import { comparePath } from "./spec-pair.mjs";
import {
  aggregateStats,
  assessEvidenceStability,
  compareRuns,
  resultCategory,
  runWithConcurrency,
  stableJson,
} from "./capture-hook-reference-batch-logic.mjs";
import {
  HOOK_MODULE_MANIFEST,
  HOOK_MODULES_DIR,
} from "./spec-hook-contract.mjs";
import { writeReferenceFile } from "./reference-safe-io.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const defaultSourceRoot = join(repoDir, "bundle", "specs");
const defaultIrRoot = join(repoDir, "bundle", "specs-ir");
const KNOWN_SYSTEM_ALIASES = new Map([
  ["/var", "/private/var"],
  ["/tmp", "/private/tmp"],
]);
const HARNESS_FILES = Object.freeze([
  "scripts/audit-spec-hooks.mjs",
  "scripts/filepaths-helper.mjs",
  "scripts/capture-hook-reference.mjs",
  "scripts/reference-audit-worker.mjs",
  "scripts/reference-hook-worker.mjs",
  "scripts/spec-hook-contract.mjs",
  "scripts/capture-hook-reference-batch.mjs",
  "scripts/capture-hook-reference-batch-logic.mjs",
]);

export const BATCH_REPORT_VERSION = 2;
export const PROBES_PER_PATH = 2;
export const PATH_COUNT = 2;
export const PROBES_PER_INSTANCE = PROBES_PER_PATH * PATH_COUNT;
export const DEFAULT_LIMIT = 18;
export const MAX_LIMIT = 256;
export const MAX_CONCURRENCY = 4;
export const DEFAULT_CONCURRENCY = 4;
export const DEFAULT_TIMEOUT_MS = 2000;

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function rejectPublicAuditOption(options, label, allowed) {
  if (!isRecord(options)) throw new Error(`${label} options must be an object`);
  for (const key of Object.keys(options)) {
    if (key === "audit") {
      throw new Error(
        `${label} no longer accepts an audit report; it is generated in a restricted child process`,
      );
    }
    if (key === "auditPath") {
      throw new Error(
        `${label} no longer accepts auditPath; it is generated in a restricted child process`,
      );
    }
    if (["probe", "sourceProbe", "moduleProbe"].includes(key)) {
      throw new Error(`${label} contains unknown field ${key}`);
    }
    if (allowed && !allowed.includes(key)) {
      throw new Error(`${label} contains unknown field ${key}`);
    }
  }
}

const IR_TO_SOURCE_FIELD = Object.freeze({
  jsLoadSpec: "loadSpec",
  jsTrigger: "trigger",
  jsAlias: "alias",
  jsGetQueryTerm: "getQueryTerm",
  jsGenerateSpec: "generateSpec",
  jsScript: "script",
  jsPostProcess: "postProcess",
  jsCustom: "custom",
  jsFilterTemplateSuggestions: "filterTemplateSuggestions",
});

const SOURCE_FIELDS = Object.freeze(Object.values(IR_TO_SOURCE_FIELD));

// Every value in this matrix is JSON-safe and intentionally small. The
// `$referenceExec` marker is interpreted only by reference-hook-worker.mjs;
// it never becomes a host function and never launches a process.
export const FIXTURE_MATRIX = Object.freeze({
  loadSpec: {
    id: "loadSpec-token-exec-v1",
    args: ["synthetic-token", { $referenceExec: true }],
    mockExecRules: [
      {
        command: "synthetic-command",
        args: ["synthetic-token"],
        cwd: null,
        env: null,
        timeout: null,
        status: 0,
        stdout: "synthetic-output",
        stderr: "",
      },
    ],
    inputShape: ["token", "mock-exec"],
  },
  trigger: {
    id: "trigger-search-previous-v1",
    args: ["synthetic-search", "synthetic-previous"],
    mockExecRules: [],
    inputShape: ["search-term", "previous-search-term"],
  },
  alias: {
    id: "alias-token-exec-v1",
    args: ["synthetic-alias", { $referenceExec: true }],
    mockExecRules: [],
    inputShape: ["token", "mock-exec"],
  },
  getQueryTerm: {
    id: "getQueryTerm-search-term-v1",
    args: ["prefix:synthetic"],
    mockExecRules: [],
    inputShape: ["search-term"],
  },
  generateSpec: {
    id: "generateSpec-tokens-exec-v1",
    args: [["synthetic-token"], { $referenceExec: true }],
    mockExecRules: [],
    inputShape: ["tokens", "mock-exec"],
  },
  script: {
    id: "script-tokens-v1",
    args: [["synthetic", "token"]],
    mockExecRules: [],
    inputShape: ["tokens"],
  },
  postProcess: {
    id: "postProcess-stdout-tokens-v1",
    args: [
      '[{"name":"synthetic","description":"reference fixture"}]',
      ["synthetic"],
    ],
    mockExecRules: [],
    inputShape: ["stdout", "tokens"],
  },
  custom: {
    id: "custom-tokens-exec-context-v1",
    args: [
      ["synthetic-token"],
      { $referenceExec: true },
      {
        currentWorkingDirectory: "/__easy_complete_reference__",
        currentProcess: "synthetic-shell",
        searchTerm: "synthetic-search-term",
        sshPrefix: "",
        environmentVariables: {
          EC_REFERENCE_FIXTURE: "1",
          HOME: "/__easy_complete_home__",
          PATH: "/usr/bin:/bin",
          LANG: "C.UTF-8",
        },
        isDangerous: false,
      },
    ],
    mockExecRules: [],
    inputShape: ["tokens", "mock-exec", "shell-context"],
  },
  filterTemplateSuggestions: {
    id: "filterTemplateSuggestions-suggestions-v1",
    args: [
      [
        { name: "synthetic.txt", description: "text" },
        { name: "synthetic/", description: "directory" },
        { name: "other.bin", description: "other" },
      ],
    ],
    mockExecRules: [],
    inputShape: ["suggestions"],
  },
});

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function isSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

function validArtifactPath(name, extension) {
  return (
    typeof name === "string" &&
    name.endsWith(extension) &&
    !isAbsolute(name) &&
    !name.includes("\0") &&
    !name.includes("\\") &&
    name.split("/").every((part) => part && part !== "." && part !== "..")
  );
}

function knownAliasTarget(path) {
  const absolute = resolve(path);
  for (const [alias, target] of KNOWN_SYSTEM_ALIASES) {
    if (absolute === alias || absolute.startsWith(`${alias}${sep}`)) {
      return resolve(target + absolute.slice(alias.length));
    }
  }
  return null;
}

function pathInside(root, target) {
  const path = relative(resolve(root), resolve(target));
  return (
    path === "" ||
    (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`))
  );
}

async function assertNoSymlinkAncestors(path, label) {
  const absolute = resolve(path);
  const components = absolute.split(sep).filter(Boolean);
  let current = absolute.startsWith(sep) ? sep : "";
  for (let index = 0; index < components.length; index += 1) {
    current = current ? join(current, components[index]) : components[index];
    const info = await lstat(current);
    if (info.isSymbolicLink()) {
      const canonical = await realpath(current).catch(() => null);
      if (canonical && knownAliasTarget(current) === canonical) continue;
      throw new Error(`${label} contains a symbolic-link ancestor`);
    }
    if (index < components.length - 1 && !info.isDirectory()) {
      throw new Error(`${label} ancestor is not a directory`);
    }
  }
  return absolute;
}

async function readRegularFile(path, label, { root, encoding = "utf8" } = {}) {
  const absolute = resolve(path);
  if (root && !pathInside(root, absolute)) {
    throw new Error(`${label} escapes its approved root`);
  }
  await assertNoSymlinkAncestors(absolute, label);
  const before = await lstat(absolute);
  if (before.isSymbolicLink()) throw new Error(`${label} is a symbolic link`);
  if (!before.isFile()) throw new Error(`${label} is a special entry`);
  const canonical = await realpath(absolute);
  const approvedRoot = root ? knownAliasTarget(root) ?? resolve(root) : null;
  if (
    (canonical !== absolute && knownAliasTarget(absolute) !== canonical) ||
    (approvedRoot && !pathInside(approvedRoot, canonical))
  ) {
    throw new Error(`${label} escapes its approved root`);
  }
  let handle;
  try {
    handle = await open(
      absolute,
      constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
    );
    const after = await handle.stat();
    if (
      !after.isFile() ||
      after.dev !== before.dev ||
      after.ino !== before.ino ||
      after.mode !== before.mode
    ) {
      throw new Error(`${label} changed identity while opening`);
    }
    return await handle.readFile({ encoding });
  } finally {
    await handle?.close().catch(() => {});
  }
}

function sameStringSet(left, right) {
  const a = [...left].sort(comparePath);
  const b = [...right].sort(comparePath);
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

async function artifactDigest(root, name, extension) {
  if (!validArtifactPath(name, extension))
    throw new Error(`invalid audited ${extension} artifact path`);
  const approved = await realpath(root);
  const target = resolve(approved, name);
  const canonical = await realpath(target);
  const path = relative(approved, canonical);
  if (path === ".." || path.startsWith(`..${sep}`) || isAbsolute(path)) {
    throw new Error(`audited ${extension} artifact escapes its root`);
  }
  if (canonical !== target) {
    throw new Error(`audited ${extension} artifact is a symbolic link`);
  }
  return sha256(
    await readRegularFile(target, `audited ${extension} artifact`, {
      root: approved,
    }),
  );
}

function requireSha(value, label) {
  if (typeof value !== "string" || !/^[a-f0-9]{64}$/.test(value)) {
    throw new Error(`${label} must be a lowercase SHA-256 digest`);
  }
  return value;
}

function requireRelativeArtifact(value, extension, label) {
  if (!validArtifactPath(value, extension)) {
    throw new Error(`${label} has an invalid audited artifact path`);
  }
  return value;
}

async function readAuditedHookModuleEvidence(audit, irRoot) {
  const moduleSummary = audit.hookModules;
  if (
    !moduleSummary ||
    moduleSummary.validated !== true ||
    moduleSummary.manifest !== HOOK_MODULE_MANIFEST ||
    moduleSummary.directory !== HOOK_MODULES_DIR
  ) {
    throw new Error(
      "a passing audit with validated closure-preserving hook modules is required",
    );
  }
  const expectedManifestSha = requireSha(
    moduleSummary.manifestSha256,
    "audit hookModules.manifestSha256",
  );
  const approvedIrRoot = await realpath(irRoot);
  const manifestPath = join(approvedIrRoot, HOOK_MODULE_MANIFEST);
  const manifestText = await readRegularFile(
    manifestPath,
    "closure-preserving hook module manifest",
    { root: approvedIrRoot },
  );
  const actualManifestSha = sha256(manifestText);
  if (actualManifestSha !== expectedManifestSha) {
    throw new Error(
      "closure-preserving hook module manifest SHA differs from audit",
    );
  }
  let manifest;
  try {
    manifest = JSON.parse(manifestText);
  } catch (error) {
    throw new Error("closure-preserving hook module manifest is invalid JSON", {
      cause: error,
    });
  }
  if (
    !manifest ||
    typeof manifest !== "object" ||
    Array.isArray(manifest) ||
    manifest.version !== 1 ||
    manifest.kind !== "closure-preserving-hook-modules" ||
    !manifest.hooks ||
    typeof manifest.hooks !== "object" ||
    Array.isArray(manifest.hooks) ||
    !manifest.modules ||
    typeof manifest.modules !== "object" ||
    Array.isArray(manifest.modules)
  ) {
    throw new Error(
      "closure-preserving hook module manifest schema is invalid",
    );
  }
  if (
    moduleSummary.manifestHooks !== Object.keys(manifest.hooks).length ||
    moduleSummary.manifestModules !== Object.keys(manifest.modules).length
  ) {
    throw new Error(
      "closure-preserving hook module manifest counts differ from audit",
    );
  }
  const hookDescriptors = {};
  for (const [id, descriptor] of Object.entries(manifest.hooks)) {
    if (!descriptor || typeof descriptor !== "object" || Array.isArray(descriptor)) {
      throw new Error(`hook ${id} has invalid manifest descriptor`);
    }
    const allowed = [
      "module",
      "moduleSha256",
      "path",
      "sourceField",
      "functionBodySha256",
    ];
    for (const key of Object.keys(descriptor)) {
      if (!allowed.includes(key)) {
        throw new Error(`hooks.${id} contains unknown field ${key}`);
      }
    }
    if (
      !validArtifactPath(descriptor.module, ".js") ||
      !isSha256(descriptor.moduleSha256) ||
      typeof descriptor.path !== "string" ||
      typeof descriptor.sourceField !== "string" ||
      !isSha256(descriptor.functionBodySha256)
    ) {
      throw new Error(`hook ${id} has invalid manifest descriptor`);
    }
    hookDescriptors[id] = {
      module: descriptor.module,
      moduleSha256: descriptor.moduleSha256,
      path: descriptor.path,
      sourceField: descriptor.sourceField,
      functionBodySha256: descriptor.functionBodySha256,
    };
  }
  const modules = new Map();
  for (const [file, metadata] of Object.entries(manifest.modules)) {
    requireRelativeArtifact(file, ".js", `module ${file}`);
    if (!metadata || typeof metadata !== "object" || Array.isArray(metadata)) {
      throw new Error(`module ${file} has invalid manifest metadata`);
    }
    const moduleSha256 = requireSha(
      metadata.moduleSha256,
      `module ${file}.moduleSha256`,
    );
    modules.set(file, moduleSha256);
  }
  const modulesRoot = join(approvedIrRoot, HOOK_MODULES_DIR);
  const resolvedModulesRoot = await realpath(modulesRoot);
  if (resolvedModulesRoot !== modulesRoot || !pathInside(approvedIrRoot, resolvedModulesRoot)) {
    throw new Error(
      "closure-preserving hook module directory escapes its IR root",
    );
  }
  const moduleEntries = await readdir(resolvedModulesRoot, { withFileTypes: true });
  const moduleFilesOnDisk = [];
  for (const entry of moduleEntries) {
    const entryPath = join(resolvedModulesRoot, entry.name);
    const info = await lstat(entryPath);
    if (info.isSymbolicLink()) {
      throw new Error(
        "closure-preserving hook module directory contains a symbolic link",
      );
    }
    if (info.isDirectory()) {
      throw new Error(
        "closure-preserving hook module directory contains nested entries",
      );
    }
    if (!info.isFile()) {
      throw new Error(
        "closure-preserving hook module directory contains a special entry",
      );
    }
    if (entry.name.endsWith(".js")) moduleFilesOnDisk.push(entry.name);
  }
  if (
    !sameStringSet(moduleFilesOnDisk, [...modules.keys()]) ||
    moduleSummary.filesOnDisk !== moduleFilesOnDisk.length
  ) {
    throw new Error(
      "closure-preserving hook module files differ from the audited manifest",
    );
  }
  const moduleFiles = [];
  for (const [file, expected] of modules) {
    const actual = await artifactDigest(modulesRoot, file, ".js");
    if (actual !== expected) {
      throw new Error(
        `closure-preserving hook module ${file} SHA differs from audit`,
      );
    }
    moduleFiles.push({ file, sha256: expected });
  }
  moduleFiles.sort((left, right) => comparePath(left.file, right.file));
  return {
    manifestSha256: expectedManifestSha,
    manifestHooks: Object.keys(manifest.hooks).length,
    manifestModules: modules.size,
    moduleFiles,
    hookDescriptors,
  };
}

async function auditedArtifactSnapshot(audit, sourceRoot, irRoot, hooksRoot) {
  const sourceFiles = new Map();
  const irFiles = new Map();
  const hookFiles = new Map();
  for (const record of audit.sourceToIr ?? []) {
    const source = requireRelativeArtifact(
      record.source,
      ".js",
      "audited source",
    );
    const sourceSha256 = requireSha(
      record.sourceSha256,
      `source ${source}.sourceSha256`,
    );
    const previousSource = sourceFiles.get(source);
    if (previousSource && previousSource !== sourceSha256) {
      throw new Error(`source ${source} has inconsistent audited SHA`);
    }
    sourceFiles.set(source, sourceSha256);
    if (record.ir != null) {
      const ir = requireRelativeArtifact(record.ir, ".json", "audited IR");
      const irSha256 = requireSha(record.irSha256, `IR ${ir}.irSha256`);
      const previousIr = irFiles.get(ir);
      if (previousIr && previousIr !== irSha256) {
        throw new Error(`IR ${ir} has inconsistent audited SHA`);
      }
      irFiles.set(ir, irSha256);
    }
  }
  for (const entry of audit.hookManifest ?? []) {
    requireRelativeArtifact(entry.file, ".js", `hook ${entry.id}`);
    const hookSha256 = requireSha(entry.sha256, `hook ${entry.id}.sha256`);
    const previousHook = hookFiles.get(entry.file);
    if (previousHook && previousHook !== hookSha256) {
      throw new Error(`hook file ${entry.file} has inconsistent audited SHA`);
    }
    hookFiles.set(entry.file, hookSha256);
  }
  for (const [name, expected] of sourceFiles) {
    if ((await artifactDigest(sourceRoot, name, ".js")) !== expected) {
      throw new Error(`source ${name} SHA differs from restricted audit`);
    }
  }
  for (const [name, expected] of irFiles) {
    if ((await artifactDigest(irRoot, name, ".json")) !== expected) {
      throw new Error(`IR ${name} SHA differs from restricted audit`);
    }
  }
  for (const [name, expected] of hookFiles) {
    if ((await artifactDigest(hooksRoot, name, ".js")) !== expected) {
      throw new Error(`hook ${name} SHA differs from restricted audit`);
    }
  }
  const moduleEvidence = await readAuditedHookModuleEvidence(audit, irRoot);
  const evidence = {
    sourceFiles: [...sourceFiles.entries()]
      .map(([file, sha256]) => ({ file, sha256 }))
      .sort((left, right) => comparePath(left.file, right.file)),
    irFiles: [...irFiles.entries()]
      .map(([file, sha256]) => ({ file, sha256 }))
      .sort((left, right) => comparePath(left.file, right.file)),
    hookFiles: [...hookFiles.entries()]
      .map(([file, sha256]) => ({ file, sha256 }))
      .sort((left, right) => comparePath(left.file, right.file)),
    moduleFiles: moduleEvidence.moduleFiles,
    manifestSha256: moduleEvidence.manifestSha256,
  };
  return {
    sourceFiles: sourceFiles.size,
    irFiles: irFiles.size,
    hookFiles: hookFiles.size,
    ...moduleEvidence,
    snapshotSha256: sha256(stableJson(evidence)),
  };
}

function validateSelectedMappings(selected, sourceIndex) {
  const irFiles = new Map();
  const hookFiles = new Map();
  for (const entry of selected) {
    const instance = sourceIndex.get(entry.id);
    if (!instance || entry.ir !== instance.ir) {
      throw new Error(`hook ${entry.id} has a stale source/IR mapping`);
    }
    const hookSha256 = requireSha(entry.sha256, `hook ${entry.id}.sha256`);
    const irSha256 = requireSha(instance.irSha256, `IR ${entry.ir}.irSha256`);
    const previousIr = irFiles.get(entry.ir);
    if (previousIr && previousIr !== irSha256) {
      throw new Error(`IR ${entry.ir} has inconsistent audited SHA`);
    }
    irFiles.set(entry.ir, irSha256);
    const previousHook = hookFiles.get(entry.file);
    if (previousHook && previousHook !== hookSha256) {
      throw new Error(`hook file ${entry.file} has inconsistent audited SHA`);
    }
    hookFiles.set(entry.file, hookSha256);
  }
}

async function harnessHashes() {
  return Object.fromEntries(
    await Promise.all(
      HARNESS_FILES.map(async (file) => [
        file,
        sha256(
          await readRegularFile(join(repoDir, file), `harness file ${file}`, {
            root: repoDir,
          }),
        ),
      ]),
    ),
  );
}

function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

function sourceIndexFromAudit(audit) {
  const index = new Map();
  for (const record of audit.sourceToIr ?? []) {
    for (const [field, instances] of Object.entries(
      record.hookInstances ?? {},
    )) {
      for (const instance of instances) {
        if (!instance.id) continue;
        if (index.has(instance.id)) {
          throw new Error(`duplicate audited hook id ${instance.id}`);
        }
        index.set(instance.id, {
          source: record.source,
          path: instance.path,
          field,
          ir: record.ir,
          irSha256: record.irSha256,
          // The audit keeps the whole source-file digest on the record and
          // the extracted function-body digest on each instance. The former
          // proves the source/import tree was the audited tree; the latter is
          // what the VM worker checks before invocation.
          sourceSha256: record.sourceSha256,
          functionBodySha256: instance.sha256,
        });
      }
    }
  }
  return index;
}

function normalizeFixtureMatrix(matrix = FIXTURE_MATRIX) {
  if (!matrix || typeof matrix !== "object" || Array.isArray(matrix)) {
    throw new Error(
      "fixtureMatrix must be an object keyed by source hook field",
    );
  }
  const normalized = {};
  for (const field of SOURCE_FIELDS) {
    const entry = matrix[field] ?? FIXTURE_MATRIX[field];
    if (!entry) continue;
    if (!Array.isArray(entry.args)) {
      throw new Error(`fixture ${field} args must be an array`);
    }
    if (!Array.isArray(entry.mockExecRules)) {
      throw new Error(`fixture ${field} mockExecRules must be an array`);
    }
    const id =
      typeof entry.id === "string" && entry.id ? entry.id : `${field}-fixture`;
    normalized[field] = {
      id,
      args: cloneJson(entry.args),
      mockExecRules: cloneJson(entry.mockExecRules),
      inputShape: Array.isArray(entry.inputShape)
        ? entry.inputShape.map(String)
        : [],
    };
  }
  return normalized;
}

function fieldBuckets(manifest) {
  const buckets = new Map(SOURCE_FIELDS.map((field) => [field, []]));
  for (const entry of manifest) {
    const field = IR_TO_SOURCE_FIELD[entry.field] ?? entry.field;
    const bucket = buckets.get(field);
    if (bucket) bucket.push(entry);
  }
  for (const values of buckets.values()) {
    values.sort((left, right) => comparePath(left.id, right.id));
  }
  return buckets;
}

/**
 * Select a deterministic, field-balanced prefix. `--all` is the only way to
 * request every manifest entry; a normal invocation can never silently turn
 * into a 3692-instance run.
 */
export function selectHookInstances(
  manifest,
  { limit = DEFAULT_LIMIT, all = false } = {},
) {
  if (!Array.isArray(manifest))
    throw new Error("audit hook manifest is required");
  if (!all && (!Number.isInteger(limit) || limit < 0 || limit > MAX_LIMIT)) {
    throw new Error(`limit must be an integer in 0..${MAX_LIMIT}`);
  }
  if (all)
    return [...manifest].sort((left, right) => comparePath(left.id, right.id));
  const target = Math.min(limit, manifest.length);
  const buckets = fieldBuckets(manifest);
  const selected = [];
  while (selected.length < target) {
    let progressed = false;
    for (const values of buckets.values()) {
      if (selected.length >= target) break;
      const entry = values.shift();
      if (!entry) continue;
      selected.push(entry);
      progressed = true;
    }
    if (!progressed) break;
  }
  return selected;
}

function resultMetadataMatches(
  result,
  instance,
  manifestEntry,
  pathName,
  auditedArtifacts,
) {
  const expectedCommon = {
    hookId: manifestEntry.id,
    source: instance.source,
    path: instance.path,
    field: instance.field,
    sourceField: instance.field,
    ir: instance.ir,
    irSha256: instance.irSha256,
    sourceSha256: instance.sourceSha256,
    functionBodySha256: instance.functionBodySha256,
    hookFile: manifestEntry.file,
    hookFileSha256: manifestEntry.sha256,
  };
  const sourceMetadataMatches = Object.entries(expectedCommon).every(
    ([field, expected]) => result?.[field] === expected,
  );
  if (!sourceMetadataMatches) return false;
  if (pathName === "source") {
    return (
      result?.sourceShaVerified === true &&
      result?.module === undefined &&
      result?.moduleSha256 === undefined &&
      result?.manifestSha256 === undefined
    );
  }
  const descriptor = auditedArtifacts.hookDescriptors?.[manifestEntry.id];
  if (!descriptor) return false;
  return (
    result?.module === descriptor.module &&
    result?.moduleSha256 === descriptor.moduleSha256 &&
    result?.sourceField === descriptor.sourceField &&
    result?.path === descriptor.path &&
    result?.functionBodySha256 === descriptor.functionBodySha256 &&
    result?.moduleShaVerified === true &&
    result?.manifestSha256 === auditedArtifacts.manifestSha256
  );
}

function normalizedBatchError(error) {
  return {
    status: "batch-error",
    stage: "batch",
    errorClass: error?.name || "Error",
    message: error?.message || String(error),
    sourceShaVerified: false,
    execTrace: [],
  };
}

function buildManifestProvenance(audit, selected) {
  const manifest = [...(audit.hookManifest ?? [])].sort((left, right) =>
    comparePath(left.id, right.id),
  );
  const selectedManifest = [...selected].sort((left, right) =>
    comparePath(left.id, right.id),
  );
  return {
    algorithm: "sha256",
    totalInstances: manifest.length,
    hookManifestSha256: sha256(stableJson(manifest)),
    // This is the digest consumed by both the generated-module probe and the
    // Rust loader. Keep it explicit so a report cannot be mistaken for a
    // source-only baseline whose hook list happened to have the same digest.
    manifestSha256: audit.hookModules?.manifestSha256 ?? null,
    hookModulesManifestSha256: audit.hookModules?.manifestSha256 ?? null,
    hookModulesManifest: audit.hookModules?.manifest ?? HOOK_MODULE_MANIFEST,
    hookModulesDirectory: audit.hookModules?.directory ?? HOOK_MODULES_DIR,
    selectedInstances: selectedManifest.length,
    selectedManifestSha256: sha256(stableJson(selectedManifest)),
    auditReportSha256: audit.reproducibility?.manifestSha256 ?? null,
  };
}

function buildInstanceManifestEntry(
  entry,
  sourceInstance,
  fixture,
  moduleManifestSha256,
) {
  return {
    id: entry.id,
    file: entry.file,
    field: entry.field,
    sourceField:
      sourceInstance?.field ?? IR_TO_SOURCE_FIELD[entry.field] ?? null,
    ir: entry.ir,
    hookFileSha256: entry.sha256 ?? null,
    source: sourceInstance?.source ?? null,
    sourcePath: sourceInstance?.path ?? null,
    sourceSha256: sourceInstance?.sourceSha256 ?? null,
    functionBodySha256: sourceInstance?.functionBodySha256 ?? null,
    fixtureId: fixture?.id ?? null,
    moduleManifestSha256: moduleManifestSha256 ?? null,
  };
}

function thinSummary(report, { reportWritten = false } = {}) {
  const selection = {
    all: report.selection.all,
    requestedLimit: report.selection.requestedLimit,
    strategy: report.selection.strategy,
    totalManifestInstances: report.selection.totalManifestInstances,
    selectedInstances: report.selection.selectedInstances,
  };
  return {
    version: report.version,
    kind: report.kind,
    ok: report.ok,
    selection,
    stats: report.stats,
    baseline: report.baseline,
    audit: {
      ok: report.provenance.audit.ok,
      manifestSha256: report.provenance.manifest.hookModulesManifestSha256,
      hookManifestSha256: report.provenance.manifest.hookManifestSha256,
      totalInstances: report.provenance.manifest.totalInstances,
    },
    reportWritten,
  };
}

/**
 * Probe selected audited source-hook instances through both the original
 * source module and the generated closure-preserving module. Each path gets
 * two fresh VM workers. `probe` remains a source-path alias for focused drift
 * tests; production uses the two explicit probes below.
 */
async function captureHookReferenceBatchWithAudit(options = {}, reportAudit) {
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    hooksRoot = join(irRoot, "hooks"),
    limit = DEFAULT_LIMIT,
    all = false,
    concurrency = DEFAULT_CONCURRENCY,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    fixtureMatrix = FIXTURE_MATRIX,
    probe = captureHookReference,
    sourceProbe = probe,
    moduleProbe = captureHookModuleReference,
  } = options;
  if (
    !Number.isInteger(concurrency) ||
    concurrency < 1 ||
    concurrency > MAX_CONCURRENCY
  ) {
    throw new Error(`concurrency must be an integer in 1..${MAX_CONCURRENCY}`);
  }
  if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 7000) {
    throw new Error("reference timeout must be 100..7000 ms");
  }
  if (all && limit !== DEFAULT_LIMIT && limit !== undefined) {
    throw new Error("--all cannot be combined with an explicit limit");
  }
  if (!reportAudit || reportAudit.ok !== true) {
    throw new Error("a passing restricted source/IR audit is required");
  }
  const harnessBefore = await harnessHashes();
  const auditEvidenceBefore = sha256(JSON.stringify(reportAudit));
  const auditFileShaBefore = null;
  const normalizedFixtures = normalizeFixtureMatrix(fixtureMatrix);
  const manifest = Array.isArray(reportAudit.hookManifest)
    ? reportAudit.hookManifest
    : [];
  const selected = selectHookInstances(manifest, { limit, all });
  const sourceIndex = sourceIndexFromAudit(reportAudit);
  validateSelectedMappings(selected, sourceIndex);
  const auditedArtifactsBefore = await auditedArtifactSnapshot(
    reportAudit,
    sourceRoot,
    irRoot,
    hooksRoot,
  );
  const provenanceManifest = buildManifestProvenance(reportAudit, selected);
  const instanceEntries = selected.map((entry) => {
    const sourceInstance = sourceIndex.get(entry.id) ?? null;
    const sourceField =
      sourceInstance?.field ?? IR_TO_SOURCE_FIELD[entry.field] ?? null;
    const fixture = normalizedFixtures[sourceField] ?? {
      id: `${sourceField ?? "unknown"}-missing-fixture`,
      args: [],
      mockExecRules: [],
      inputShape: [],
    };
    return {
      entry,
      sourceInstance,
      sourceField,
      fixture,
    };
  });

  const runJobs = instanceEntries.flatMap((instance) =>
    ["source", "module"].flatMap((pathName) =>
      Array.from({ length: PROBES_PER_PATH }, (_, probeIndex) => ({
        instance,
        pathName,
        probeIndex,
      })),
    ),
  );
  const runResults = await runWithConcurrency(
    runJobs,
    concurrency,
    async ({ instance, pathName, probeIndex }) => {
      let raw;
      try {
        if (!instance.sourceInstance) {
          throw new Error(
            `hook ${instance.entry.id} has no audited source instance`,
          );
        }
        const probeOptions = {
          hookId: instance.entry.id,
          args: cloneJson(instance.fixture.args),
          mockExecRules: cloneJson(instance.fixture.mockExecRules),
          timeoutMs,
          sourceRoot,
          irRoot,
          hooksRoot,
        };
        raw = await (pathName === "source" ? sourceProbe : moduleProbe)({
          ...probeOptions,
        });
      } catch (error) {
        raw = normalizedBatchError(error);
      }
      const metadataVerified = resultMetadataMatches(
        raw,
        instance.sourceInstance ?? {},
        instance.entry,
        pathName,
        auditedArtifactsBefore,
      );
      const sourceShaVerified =
        pathName === "source" &&
        raw?.sourceShaVerified === true &&
        metadataVerified;
      const moduleShaVerified =
        pathName === "module" &&
        raw?.moduleShaVerified === true &&
        metadataVerified;
      const manifestShaVerified =
        pathName === "module" &&
        raw?.manifestSha256 === auditedArtifactsBefore.manifestSha256;
      const category = resultCategory(raw);
      return {
        path: pathName,
        probeIndex,
        category,
        status: raw?.status ?? "unknown",
        sourceShaVerified,
        moduleShaVerified,
        manifestShaVerified,
        metadataVerified,
        ...(raw?.stage ? { stage: raw.stage } : {}),
        ...(raw?.errorClass ? { errorClass: raw.errorClass } : {}),
        ...(raw?.message ? { message: raw.message } : {}),
        ...(Object.hasOwn(raw ?? {}, "value") ? { value: raw.value } : {}),
        ...(Object.hasOwn(raw ?? {}, "execTrace")
          ? { execTrace: raw.execTrace }
          : {}),
        ...(raw?.module ? { module: raw.module } : {}),
        ...(raw?.moduleSha256 ? { moduleSha256: raw.moduleSha256 } : {}),
      };
    },
    normalizedBatchError,
  );
  const harnessAfter = await harnessHashes();
  const auditEvidenceAfter = sha256(JSON.stringify(reportAudit));
  const auditFileShaAfter = null;
  let artifactsError = null;
  let auditedArtifactsAfter = null;
  try {
    auditedArtifactsAfter = await auditedArtifactSnapshot(
      reportAudit,
      sourceRoot,
      irRoot,
      hooksRoot,
    );
  } catch (error) {
    artifactsError = error.message;
  }
  const evidenceAssessment = assessEvidenceStability({
    harnessBefore,
    harnessAfter,
    auditBefore: auditEvidenceBefore,
    auditAfter: auditEvidenceAfter,
    auditFileBefore: auditFileShaBefore,
    auditFileAfter: auditFileShaAfter,
    artifactsBefore: auditedArtifactsBefore,
    artifactsAfter: auditedArtifactsAfter,
    artifactsError,
  });
  const {
    harnessStable,
    auditStable,
    artifactsStable,
    artifactsDrift,
  } = evidenceAssessment;

  const reports = instanceEntries.map((instance, index) => {
    const runs = runResults.slice(
      index * PROBES_PER_INSTANCE,
      (index + 1) * PROBES_PER_INSTANCE,
    );
    const sourceRuns = runs.filter((run) => run.path === "source");
    const moduleRuns = runs.filter((run) => run.path === "module");
    const comparison = compareRuns(sourceRuns, moduleRuns);
    const firstModuleRun = moduleRuns[0];
    return {
      ...buildInstanceManifestEntry(
        instance.entry,
        instance.sourceInstance
          ? { ...instance.sourceInstance, field: instance.sourceField }
          : null,
        instance.fixture,
        auditedArtifactsBefore.manifestSha256,
      ),
      fixture: {
        id: instance.fixture.id,
        inputShape: instance.fixture.inputShape,
        args: instance.fixture.args,
        mockExecRules: instance.fixture.mockExecRules,
      },
      module: {
        manifestSha256: auditedArtifactsBefore.manifestSha256,
        file: firstModuleRun?.module ?? null,
        sha256: firstModuleRun?.moduleSha256 ?? null,
      },
      runs: {
        source: sourceRuns,
        module: moduleRuns,
      },
      comparison,
    };
  });

  const evidenceStable = harnessStable && artifactsStable && auditStable;
  if (!evidenceStable) {
    for (const report of reports) {
      if (report.comparison.baselineConfirmed) {
        report.comparison.baselineConfirmed = false;
        if (report.comparison.status === "synthetic-source-module-confirmed") {
          report.comparison.status = "inconclusive";
        }
      }
    }
  }
  const stats = aggregateStats(reports);
  const auditOk = reportAudit.ok === true;
  const baselineStatus =
    !harnessStable || !artifactsStable || !auditStable
      ? "not-established"
      : stats.selectedInstances === 0
        ? "not-run"
        : stats.confirmedInstances === stats.selectedInstances
          ? "synthetic-source-module-confirmed"
          : "not-established";
  const report = {
    version: BATCH_REPORT_VERSION,
    kind: "hook-source-module-parity-batch-diagnostic",
    contract: {
      referenceKind: "synthetic-source-module-probe-not-real-cli-baseline",
      paths: ["source", "module"],
      pathCount: PATH_COUNT,
      sourceKind: "original-bundled-fig-source",
      moduleKind: "generated-closure-preserving-hook-module",
      probesPerPath: PROBES_PER_PATH,
      probesPerInstance: PROBES_PER_INSTANCE,
      freshWorkerPerProbe: true,
      executesRealShell: false,
      fixedSingleInputPerField: true,
      baselineConfirmationRequires: [
        "both source probes are stable successes",
        "both generated-module probes are stable successes",
        "source and generated-module value and execTrace are equal",
        "all source, module, and manifest SHA checks pass",
      ],
      stableErrorPolicy:
        "same error stage and class on both repeats remains inconclusive",
      inconclusiveStatuses: ["error", "pending", "timeout", "unknown"],
      divergencePolicy:
        "any repeat instability, cross-path status/value/trace difference, or metadata failure is divergent",
      fixturePolicy:
        "deterministic JSON fixtures with exact-match mocked exec only; no real shell or CLI",
      networkIsolation: false,
      childPermissionBoundary: [
        "filesystem writes denied",
        "child_process creation denied",
        "worker creation denied",
        "network isolation is not provided",
      ],
      realCliFunctionalEquivalence: false,
    },
    selection: {
      all,
      requestedLimit: all ? null : limit,
      strategy: all ? "manifest-id-order" : "field-round-robin",
      totalManifestInstances: manifest.length,
      selectedInstances: selected.length,
      selectedHookIds: selected.map((entry) => entry.id),
    },
    provenance: {
      node: {
        version: process.version,
        execPath: process.execPath,
        v8: process.versions.v8,
        platform: process.platform,
        arch: process.arch,
      },
      workerEnvironment: {
        TZ: "UTC",
        LANG: "en_US.UTF-8",
        LC_ALL: "en_US.UTF-8",
        inheritedHostEnvironment: false,
      },
      roots: {
        sourceRoot: resolve(sourceRoot),
        irRoot: resolve(irRoot),
        hooksRoot: resolve(hooksRoot),
      },
      auditedArtifacts: {
        before: auditedArtifactsBefore,
        after: auditedArtifactsAfter,
        stableDuringRun: artifactsStable,
        drift: artifactsDrift,
      },
      audit: {
        version: reportAudit.version ?? null,
        ok: auditOk,
        stableDuringRun: auditStable,
        beforeSha256: auditEvidenceBefore,
        afterSha256: auditEvidenceAfter,
        fileBeforeSha256: auditFileShaBefore,
        fileAfterSha256: auditFileShaAfter,
        path: null,
        errors: reportAudit.errors ?? {},
      },
      manifest: provenanceManifest,
      fixture: {
        algorithm: "sha256",
        version: 1,
        matrixSha256: sha256(stableJson(normalizedFixtures)),
        fields: normalizedFixtures,
        executorPolicy:
          "exact-match mock rules only; unmatched calls fail in the VM",
      },
      harness: {
        files: harnessBefore,
        stableDuringRun: harnessStable,
        afterRunSha256: harnessAfter,
      },
    },
    stats,
    baseline: {
      status: baselineStatus,
      confirmedInstances: stats.confirmedInstances,
      totalSelectedInstances: stats.selectedInstances,
      isRealCliBaseline: false,
      functionalEquivalenceClaim: false,
    },
    instances: reports,
  };
  report.ok =
    auditOk &&
    harnessStable &&
    auditStable &&
    artifactsStable &&
    stats.selectedInstances > 0 &&
    stats.attemptedProbes === stats.expectedProbes &&
    stats.errors === 0 &&
    stats.pending === 0 &&
    stats.timeout === 0 &&
    stats.unknown === 0 &&
    stats.differingTraceOutput === 0 &&
    stats.crossPathDifferences === 0 &&
    stats.divergentInstances === 0 &&
    stats.confirmedInstances === stats.selectedInstances &&
    stats.sourceShaVerified === stats.expectedProbesPerPath &&
    stats.moduleShaVerified === stats.expectedProbesPerPath &&
    stats.manifestShaVerified === stats.expectedProbesPerPath &&
    stats.metadataVerified === stats.expectedProbes;
  report.reproducibility = {
    algorithm: "sha256",
    reportSha256: sha256(stableJson(report)),
  };
  return report;
}

/**
 * Generate one fresh, permission-restricted source/IR audit for this batch,
 * then run every probe inside its operation-scoped context. The audit is
 * intentionally not an option: callers cannot inject a stale or compact
 * report into the reference path.
 */
export async function captureHookReferenceBatch(options = {}) {
  rejectPublicAuditOption(options, "captureHookReferenceBatch", [
    "sourceRoot",
    "irRoot",
    "hooksRoot",
    "limit",
    "all",
    "concurrency",
    "timeoutMs",
    "fixtureMatrix",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    hooksRoot = join(irRoot, "hooks"),
    ...rest
  } = options;
  // Probe overrides are intentionally accepted only by the non-exported
  // `captureHookReferenceBatchWithAudit` helper below this public boundary.
  // Callers can therefore not substitute a result or stale audit.
  return withReferenceAudit(
    { sourceRoot, irRoot, hooksRoot },
    (audit) =>
      captureHookReferenceBatchWithAudit(
        { ...rest, sourceRoot, irRoot, hooksRoot },
        audit,
      ),
  );
}

export function compactBatchReport(report, options = {}) {
  return thinSummary(report, options);
}

export async function writeBatchReport(outputPath, report) {
  return writeReferenceFile(
    outputPath,
    `${JSON.stringify(report, null, 2)}\n`,
    { overwrite: false, label: "batch report" },
  );
}

function optionValue(argv, index, option) {
  const value = argv[index + 1];
  if (!value || value.startsWith("--")) {
    throw new Error(`${option} requires a value`);
  }
  return value;
}

function parseInteger(value, option) {
  if (!/^\d+$/.test(value))
    throw new Error(`${option} must be a non-negative integer`);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed)) throw new Error(`${option} is too large`);
  return parsed;
}

export function parseBatchArgs(argv = process.argv.slice(2)) {
  const options = {
    all: false,
    limit: DEFAULT_LIMIT,
    concurrency: DEFAULT_CONCURRENCY,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    out: null,
    sourceRoot: defaultSourceRoot,
    irRoot: defaultIrRoot,
    hooksRoot: join(defaultIrRoot, "hooks"),
    help: false,
  };
  let explicitLimit = false;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      options.help = true;
    } else if (arg === "--all") {
      options.all = true;
    } else if (arg === "--limit") {
      explicitLimit = true;
      options.limit = parseInteger(
        optionValue(argv, index++, "--limit"),
        "--limit",
      );
    } else if (arg === "--concurrency") {
      options.concurrency = parseInteger(
        optionValue(argv, index++, "--concurrency"),
        "--concurrency",
      );
    } else if (arg === "--timeout-ms") {
      options.timeoutMs = parseInteger(
        optionValue(argv, index++, "--timeout-ms"),
        "--timeout-ms",
      );
    } else if (arg === "--out") {
      options.out = optionValue(argv, index++, "--out");
    } else if (arg === "--source-root") {
      options.sourceRoot = optionValue(argv, index++, "--source-root");
    } else if (arg === "--ir-root") {
      options.irRoot = optionValue(argv, index++, "--ir-root");
    } else if (arg === "--hooks-root") {
      options.hooksRoot = optionValue(argv, index++, "--hooks-root");
    } else if (arg.startsWith("--limit=")) {
      explicitLimit = true;
      options.limit = parseInteger(arg.slice("--limit=".length), "--limit");
    } else if (arg.startsWith("--concurrency=")) {
      options.concurrency = parseInteger(
        arg.slice("--concurrency=".length),
        "--concurrency",
      );
    } else if (arg.startsWith("--timeout-ms=")) {
      options.timeoutMs = parseInteger(
        arg.slice("--timeout-ms=".length),
        "--timeout-ms",
      );
    } else if (arg.startsWith("--out=")) {
      options.out = arg.slice("--out=".length);
    } else if (arg.startsWith("--source-root=")) {
      options.sourceRoot = arg.slice("--source-root=".length);
    } else if (arg.startsWith("--ir-root=")) {
      options.irRoot = arg.slice("--ir-root=".length);
    } else if (arg.startsWith("--hooks-root=")) {
      options.hooksRoot = arg.slice("--hooks-root=".length);
    } else {
      throw new Error(`unknown option: ${arg}`);
    }
  }
  if (!options.all && options.limit > MAX_LIMIT) {
    throw new Error(`--limit must be an integer in 0..${MAX_LIMIT}`);
  }
  if (options.concurrency < 1 || options.concurrency > MAX_CONCURRENCY) {
    throw new Error(
      `--concurrency must be an integer in 1..${MAX_CONCURRENCY}`,
    );
  }
  if (options.timeoutMs < 100 || options.timeoutMs > 7000) {
    throw new Error("--timeout-ms must be in 100..7000 ms");
  }
  if (options.all && explicitLimit) {
    throw new Error("--all cannot be combined with an explicit --limit");
  }
  return options;
}

function helpText() {
  return [
    "Usage: node scripts/capture-hook-reference-batch.mjs [options]",
    "",
    `Default: probe ${DEFAULT_LIMIT} field-balanced instances twice per source/module path, max concurrency ${MAX_CONCURRENCY}.`,
    "--all                 explicitly probe the complete audited manifest",
    `--limit N             probe at most N instances (0..${MAX_LIMIT})`,
    `--concurrency N       worker concurrency (1..${MAX_CONCURRENCY})`,
    "--timeout-ms N        per-probe wall-clock timeout (100..7000)",
    "--out PATH            create a detailed JSON report (never overwrite)",
    "                     source/IR audit is generated in a restricted child",
    "--source-root PATH    source spec root",
    "--ir-root PATH        compiled IR root",
    "--hooks-root PATH     extracted hook root",
  ].join("\n");
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  try {
    const options = parseBatchArgs();
    if (options.help) {
      process.stdout.write(`${helpText()}\n`);
      process.exitCode = 0;
    } else {
      // Refuse an existing destination before reading/probing the bundle. The
      // final `wx` in writeBatchReport protects against a concurrent creator.
      if (options.out) {
        const preflightParent = dirname(resolve(options.out));
        await assertNoSymlinkAncestors(preflightParent, "batch report parent");
        const preflightInfo = await lstat(preflightParent);
        if (!preflightInfo.isDirectory() || preflightInfo.isSymbolicLink()) {
          throw new Error("batch report parent must be a regular directory");
        }
        try {
          const preflightTarget = await lstat(resolve(options.out));
          if (preflightTarget.isSymbolicLink()) {
            throw new Error("batch report target is a symbolic link");
          }
          throw new Error(
            `refusing to overwrite existing report: ${options.out}`,
          );
        } catch (error) {
          if (error?.code !== "ENOENT") throw error;
        }
      }
      const report = await captureHookReferenceBatch(options);
      let reportWritten = false;
      if (options.out) {
        await writeBatchReport(options.out, report);
        reportWritten = true;
      }
      process.stdout.write(
        `${JSON.stringify(compactBatchReport(report, { reportWritten }), null, 2)}\n`,
      );
      if (!report.ok) process.exitCode = 1;
    }
  } catch (error) {
    process.stderr.write(`error: ${error?.message || String(error)}\n`);
    process.exitCode = 1;
  }
}
