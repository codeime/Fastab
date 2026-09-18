#!/usr/bin/env node
/**
 * Build/test-only reference output probes for original bundled Fig functions
 * and the generated closure-preserving hook modules. Neither path calls a
 * real CLI. Every call is confined to a child VM, supplied only synthetic
 * input and mocked exec rows, and stopped by a hard wall-clock deadline.
 * Probe outputs are diagnostics, not real-CLI functional parity baselines.
 */
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, readdir, realpath } from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  hookFileName,
} from "./spec-hook-contract.mjs";
import {
  closurePreservingHookModule,
  sourceModuleFileName,
} from "./compile-spec-ir.mjs";
import { withReferenceAudit } from "./reference-audit-worker.mjs";
import { comparePath } from "./spec-pair.mjs";
import { sameVersionedIrFamily } from "./spec-versions.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const worker = join(repoDir, "scripts", "reference-hook-worker.mjs");
const defaultSourceRoot = join(repoDir, "bundle", "specs");
const defaultIrRoot = join(repoDir, "bundle", "specs-ir");
const sourceIndexCache = new WeakMap();
// macOS resolves /var and /tmp through /private; a path under either is the
// same file as its /private twin. Linux has no such link, so treating these as
// aliases there would make every temp-dir fixture "escape its approved root".
const KNOWN_SYSTEM_ALIASES = new Map(
  process.platform === "darwin"
    ? [
        ["/var", "/private/var"],
        ["/tmp", "/private/tmp"],
      ]
    : [],
);

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

function inside(root, target) {
  const path = relative(root, target);
  return (
    path === "" ||
    (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`))
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

async function readRegularFile(path, label, { root } = {}) {
  const absolute = resolve(path);
  if (root && !inside(root, absolute)) {
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
    (approvedRoot && !inside(approvedRoot, canonical))
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
    return await handle.readFile("utf8");
  } finally {
    await handle?.close().catch(() => {});
  }
}

function sourceInstance(audit, hookId) {
  let cached = sourceIndexCache.get(audit);
  if (!cached) {
    const index = new Map();
    const sourceSha256ByModule = {};
    for (const record of audit.sourceToIr ?? []) {
      if (
        typeof record.source !== "string" ||
        !/^[a-f0-9]{64}$/.test(record.sourceSha256) ||
        Object.hasOwn(sourceSha256ByModule, record.source)
      ) {
        throw new Error(
          "restricted audit has invalid or duplicate source SHA",
        );
      }
      sourceSha256ByModule[record.source] = record.sourceSha256;
      for (const [field, instances] of Object.entries(
        record.hookInstances ?? {},
      )) {
        for (const instance of instances) {
          if (index.has(instance.id)) {
            throw new Error(`duplicate audited hook id ${instance.id}`);
          }
        index.set(instance.id, {
          source: record.source,
          path: instance.path,
          sha256: instance.sha256,
          sourceFileSha256: record.sourceSha256,
          ir: record.ir,
          irSha256: record.irSha256,
          field,
        });
        }
      }
    }
    cached = { index, sourceSha256ByModule };
    sourceIndexCache.set(audit, cached);
  }
  const found = cached.index.get(hookId);
  if (!found) throw new Error(`hook ${hookId} has no audited source instance`);
  return { ...found, sourceSha256ByModule: cached.sourceSha256ByModule };
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

function isSafeModuleFile(value) {
  return (
    typeof value === "string" &&
    /^[^/\\\0]+\.js$/.test(value) &&
    value !== ".js" &&
    value !== "..js"
  );
}

function isSafeSourceFile(value) {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    !value.includes("\0") &&
    !value.includes("\\") &&
    !value.startsWith("/") &&
    value.split("/").every((part) => part && part !== "." && part !== "..")
  );
}

function sortedStrings(values) {
  return [...values].sort(comparePath);
}

function sameStringSet(left, right) {
  const a = sortedStrings(left);
  const b = sortedStrings(right);
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

function rejectUnknownFields(value, allowed, label) {
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) {
      throw new Error(`${label} contains unknown field ${key}`);
    }
  }
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

function auditedHookInstances(audit) {
  const byId = new Map();
  const hookManifest = audit.hookManifest;
  if (!Array.isArray(hookManifest)) {
    throw new Error("restricted audit has no hook manifest");
  }
  for (const entry of hookManifest) {
    if (!isRecord(entry) || typeof entry.id !== "string" || !entry.id) {
      throw new Error("restricted audit has an invalid hook manifest entry");
    }
    if (byId.has(entry.id)) {
      throw new Error(`restricted audit duplicates hook ${entry.id}`);
    }
    byId.set(entry.id, entry);
  }
  return byId;
}

function auditedIdentity(audit, hookId) {
  const source = sourceInstance(audit, hookId);
  const auditedHook = auditedHookInstances(audit).get(hookId);
  if (!auditedHook) {
    throw new Error(`hook ${hookId} has no audited hook manifest entry`);
  }
  if (
    auditedHook.id !== hookId ||
    auditedHook.file !== hookFileName(hookId) ||
    auditedHook.field !==
      Object.entries(IR_TO_SOURCE_FIELD).find(
        ([, sourceField]) => sourceField === source.field,
      )?.[0]
  ) {
    throw new Error(`hook ${hookId} manifest identity differs from the audit`);
  }
  if (
    !sameVersionedIrFamily(auditedHook.ir, source.ir) ||
    !isSha256(auditedHook.sha256)
  ) {
    throw new Error(`hook ${hookId} manifest provenance is invalid`);
  }
  return { source, auditedHook };
}

function resultIdentity(audit, hookId, { module, moduleSha256, manifestSha256 } = {}) {
  const { source, auditedHook } = auditedIdentity(audit, hookId);
  const identity = {
    hookId,
    source: source.source,
    path: source.path,
    field: source.field,
    sourceField: source.field,
    ir: source.ir,
    irSha256: source.irSha256,
    sourceSha256: source.sourceFileSha256,
    functionBodySha256: source.sha256,
    hookFile: auditedHook.file,
    hookFileSha256: auditedHook.sha256,
  };
  if (module !== undefined) identity.module = module;
  if (moduleSha256 !== undefined) identity.moduleSha256 = moduleSha256;
  if (manifestSha256 !== undefined) identity.manifestSha256 = manifestSha256;
  return identity;
}

async function verifyAuditedHookArtifact(hookId, _hooksRoot, identity) {
  if (
    !identity ||
    !(isSha256(identity.functionBodySha256) || isSha256(identity.hookFileSha256))
  ) {
    throw new Error(`hook ${hookId} has no audited body digest`);
  }
}

async function moduleInstance(audit, hookId, irRoot, hooksRoot, sourceRoot) {
  if (audit?.ok !== true) {
    throw new Error("source/IR audit must pass before module probing");
  }
  const { source, auditedHook } = auditedIdentity(audit, hookId);
  const instances = [];
  for (const record of audit.sourceToIr ?? []) {
    if (record.source !== source.source) continue;
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
  if (instances.length === 0) {
    throw new Error(`hook ${hookId} has no audited source instances`);
  }
  const sourceText = await readRegularFile(
    join(sourceRoot, source.source),
    `source ${source.source}`,
    { root: sourceRoot },
  );
  const moduleSource = closurePreservingHookModule(
    sourceText,
    source.source,
    instances,
  );
  const module = sourceModuleFileName(source.source);
  return {
    ...source,
    path: source.path,
    module,
    moduleSha256: sha256(moduleSource),
    moduleSource,
    sourceField: source.sourceField,
    functionBodySha256: auditedHook.functionBodySha256 ?? source.sha256,
    manifestSha256: isSha256(audit.typedHooks?.sidecarSha256)
      ? audit.typedHooks.sidecarSha256
      : sha256(moduleSource),
  };
}

function runChild(payload, timeoutMs) {
  // Serialize before spawning: a malformed fixture must not leave a child
  // blocked on stdin while the caller receives a JSON error.
  const input = JSON.stringify(payload);
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      ["--experimental-vm-modules", "--no-warnings", worker],
      {
        stdio: ["pipe", "pipe", "pipe"],
        shell: false,
        // The VM has no process global; clear host credentials as a second
        // guard if a future source construct escapes that realm.
        env: {
          NODE_NO_WARNINGS: "1",
          TZ: "UTC",
          LANG: "en_US.UTF-8",
          LC_ALL: "en_US.UTF-8",
        },
      },
    );
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    let exceededOutput = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    // A child killed by the watchdog may close stdin before a large fixture
    // finishes writing. The close event below remains the single outcome.
    child.stdin.on("error", () => {});
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      if (stdout.length > 1_000_000) {
        exceededOutput = true;
        child.kill("SIGKILL");
      }
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
      if (stderr.length > 100_000) {
        exceededOutput = true;
        child.kill("SIGKILL");
      }
    });
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      if (timedOut) return resolve({ status: "timeout" });
      if (exceededOutput) return resolve({ status: "output-limit" });
      if (code !== 0) {
        return resolve({
          status: "worker-failed",
          stderrLength: stderr.length,
        });
      }
      try {
        const result = JSON.parse(stdout.trim());
        resolve(result);
      } catch {
        resolve({ status: "worker-result-invalid" });
      }
    });
    child.stdin.end(input);
  });
}

const MAX_BATCH_INVOCATIONS = 256;
const MAX_BATCH_INPUT_BYTES = 1_500_000;

function validateInvocationBatch(invocations) {
  if (!Array.isArray(invocations) || invocations.length === 0) {
    throw new Error("reference invocation batch must be a non-empty array");
  }
  if (invocations.length > MAX_BATCH_INVOCATIONS) {
    throw new Error(
      `reference invocation batch exceeds ${MAX_BATCH_INVOCATIONS} entries`,
    );
  }
  for (const invocation of invocations) {
    if (
      !isRecord(invocation) ||
      !Array.isArray(invocation.args) ||
      !Array.isArray(invocation.mockExecRules) ||
      Object.keys(invocation).some(
        (key) => key !== "args" && key !== "mockExecRules",
      )
    ) {
      throw new Error(
        "reference invocation must contain only args and mockExecRules arrays",
      );
    }
  }
  const bytes = Buffer.byteLength(JSON.stringify(invocations), "utf8");
  if (bytes > MAX_BATCH_INPUT_BYTES) {
    throw new Error(
      `reference invocation batch exceeds ${MAX_BATCH_INPUT_BYTES}-byte input limit`,
    );
  }
}

async function captureReferenceBatchResult({
  hookId,
  invocations,
  timeoutMs,
  payload,
  metadata,
}) {
  validateInvocationBatch(invocations);
  const result = await runChild({ ...payload, hookId, invocations }, timeoutMs);
  const runs =
    result.status === "batch-success" && Array.isArray(result.results)
      ? result.results.map((run) => ({ ...run, ...metadata }))
      : [];
  return {
    ...metadata,
    status: result.status,
    ...(result.stage ? { stage: result.stage } : {}),
    ...(result.errorClass ? { errorClass: result.errorClass } : {}),
    ...(result.stderrLength != null
      ? { stderrLength: result.stderrLength }
      : {}),
    runs,
  };
}

export async function captureHookReference(options = {}) {
  rejectPublicAuditOption(options, "captureHookReference", [
    "hookId",
    "args",
    "mockExecRules",
    "timeoutMs",
    "sourceRoot",
    "irRoot",
    "hooksRoot",
  ]);
  const {
    hookId,
    args = [],
    mockExecRules = [],
    timeoutMs = 2000,
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    hooksRoot,
  } = options;
  if (typeof hookId !== "string" || !hookId) {
    throw new Error("hookId is required");
  }
  if (!Number.isInteger(timeoutMs) || timeoutMs < 50 || timeoutMs > 7000) {
    throw new Error("reference timeout must be 50..7000 ms");
  }
  return withReferenceAudit(
    { sourceRoot, irRoot, hooksRoot },
    async (report) => {
      if (report.ok !== true)
        throw new Error("source/IR audit must pass before reference probing");
      const instance = sourceInstance(report, hookId);
      const identity = resultIdentity(report, hookId);
      await verifyAuditedHookArtifact(
        hookId,
        hooksRoot,
        identity,
        irRoot,
      );
      const result = await runChild(
        { sourceRoot, ...instance, args, mockExecRules },
        timeoutMs,
      );
      return {
        ...result,
        ...identity,
        referenceKind: "synthetic-probe-not-real-cli-baseline",
      };
    },
  );
}

/**
 * Probe the closure-preserving generated module for one audited hook.
 *
 * The parent only reads the manifest and generated module bytes to verify
 * their audited digests. It never imports either source or generated JS; all
 * executable code stays inside the isolated child VM worker.
 */
export async function captureHookModuleReference(options = {}) {
  rejectPublicAuditOption(options, "captureHookModuleReference", [
    "hookId",
    "args",
    "mockExecRules",
    "timeoutMs",
    "sourceRoot",
    "irRoot",
    "hooksRoot",
  ]);
  const {
    hookId,
    args = [],
    mockExecRules = [],
    timeoutMs = 2000,
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    hooksRoot,
  } = options;
  if (typeof hookId !== "string" || !hookId) {
    throw new Error("hookId is required");
  }
  if (!Number.isInteger(timeoutMs) || timeoutMs < 50 || timeoutMs > 7000) {
    throw new Error("reference timeout must be 50..7000 ms");
  }
  return withReferenceAudit(
    { sourceRoot, irRoot, hooksRoot },
    async (audit) => {
      const instance = await moduleInstance(
        audit,
        hookId,
        irRoot,
        hooksRoot,
        sourceRoot,
      );
      const identity = resultIdentity(audit, hookId, {
        module: instance.module,
        moduleSha256: instance.moduleSha256,
        manifestSha256: instance.manifestSha256,
      });
      const result = await runChild(
        {
          mode: "module",
          irRoot,
          hookId,
          field: instance.field,
          module: instance.module,
          moduleSha256: instance.moduleSha256,
          moduleSource: instance.moduleSource,
          args,
          mockExecRules,
        },
        timeoutMs,
      );
      return {
        ...result,
        ...identity,
        referenceKind: "synthetic-module-probe-not-real-cli-baseline",
      };
    },
  );
}

/**
 * Probe one audited source hook for several JSON-only argument sets in one
 * isolated worker. The source module is loaded once and every result keeps
 * its own status and executor trace. This is test/build tooling only.
 */
export async function captureHookReferenceBatch(options = {}) {
  rejectPublicAuditOption(options, "captureHookReferenceBatch", [
    "hookId",
    "invocations",
    "timeoutMs",
    "sourceRoot",
    "irRoot",
    "hooksRoot",
  ]);
  const {
    hookId,
    invocations = [],
    timeoutMs = 7000,
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    hooksRoot,
  } = options;
  if (typeof hookId !== "string" || !hookId) {
    throw new Error("hookId is required");
  }
  validateInvocationBatch(invocations);
  if (!Number.isInteger(timeoutMs) || timeoutMs < 50 || timeoutMs > 7000) {
    throw new Error("reference timeout must be 50..7000 ms");
  }
  return withReferenceAudit(
    { sourceRoot, irRoot, hooksRoot },
    async (audit) => {
      if (audit.ok !== true) {
        throw new Error("source/IR audit must pass before reference probing");
      }
      const instance = sourceInstance(audit, hookId);
      const identity = resultIdentity(audit, hookId);
      await verifyAuditedHookArtifact(hookId, hooksRoot, identity, irRoot);
      return captureReferenceBatchResult({
        hookId,
        invocations,
        timeoutMs,
        payload: {
          sourceRoot,
          ...instance,
        },
        metadata: {
          ...identity,
          referenceKind: "synthetic-probe-not-real-cli-baseline",
        },
      });
    },
  );
}

/**
 * Probe one generated closure-preserving hook module for several JSON-only
 * argument sets in one isolated worker. The module is loaded once per child;
 * no source or command is imported/executed by the parent.
 */
export async function captureHookModuleReferenceBatch(options = {}) {
  rejectPublicAuditOption(options, "captureHookModuleReferenceBatch", [
    "hookId",
    "invocations",
    "timeoutMs",
    "sourceRoot",
    "irRoot",
    "hooksRoot",
  ]);
  const {
    hookId,
    invocations = [],
    timeoutMs = 7000,
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    hooksRoot,
  } = options;
  if (typeof hookId !== "string" || !hookId) {
    throw new Error("hookId is required");
  }
  validateInvocationBatch(invocations);
  if (!Number.isInteger(timeoutMs) || timeoutMs < 50 || timeoutMs > 7000) {
    throw new Error("reference timeout must be 50..7000 ms");
  }
  return withReferenceAudit(
    { sourceRoot, irRoot, hooksRoot },
    async (audit) => {
      if (audit.ok !== true) {
        throw new Error("source/IR audit must pass before module probing");
      }
      const instance = await moduleInstance(
        audit,
        hookId,
        irRoot,
        hooksRoot,
        sourceRoot,
      );
      const identity = resultIdentity(audit, hookId, {
        module: instance.module,
        moduleSha256: instance.moduleSha256,
        manifestSha256: instance.manifestSha256,
      });
      return captureReferenceBatchResult({
        hookId,
        invocations,
        timeoutMs,
        payload: {
          mode: "module",
          irRoot,
          field: instance.field,
          module: instance.module,
          moduleSha256: instance.moduleSha256,
          moduleSource: instance.moduleSource,
        },
        metadata: {
          ...identity,
          referenceKind: "synthetic-module-probe-not-real-cli-baseline",
        },
      });
    },
  );
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (isMain) {
  process.stderr.write(
    "error: use captureHookReference or captureHookModuleReference from a caller; this entrypoint does not import source specs\n",
  );
  process.exitCode = 1;
}
