#!/usr/bin/env node
/**
 * Generate the source/IR audit in a short-lived, read-only Node process.
 *
 * The parent side of this module never imports audit-spec-hooks (which would
 * import every source spec). It starts this file as a child with Node's
 * permission model enabled, allowing reads only for the repository scripts
 * and the selected input trees. The child calls the restricted audit entry
 * with a proof of the parent-held pair lock and returns one JSON report over
 * stdin/stdout. The child is
 * relied on for denied fs writes, child_process, and worker creation; network
 * isolation is not provided by this harness.
 *
 * This is test/build tooling only.  It must never be bundled into the app.
 */
import { AsyncLocalStorage } from "node:async_hooks";
import { createHash } from "node:crypto";
import { lstat, readFile, realpath } from "node:fs/promises";
import { dirname, join, parse, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  PAIR_LOCK_NAME,
  verifyPair,
  withPairLock,
} from "./spec-pair.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const auditScript = join(repoDir, "scripts", "audit-spec-hooks.mjs");
const repoScriptsDir = join(repoDir, "scripts");
const repoNodeModulesDir = join(repoDir, "node_modules");
const defaultSourceRoot = join(repoDir, "bundle", "specs");
const defaultIrRoot = join(repoDir, "bundle", "specs-ir");
const pairLockPath = join(repoDir, "bundle", PAIR_LOCK_NAME);
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

export const REFERENCE_AUDIT_TIMEOUT_MS = 30_000;
export const MAX_AUDIT_INPUT_BYTES = 64 * 1024;
export const MAX_AUDIT_REPORT_BYTES = 16 * 1024 * 1024;
export const MAX_AUDIT_STDERR_BYTES = 256 * 1024;

const auditContext = new AsyncLocalStorage();

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function isKnownSystemAlias(path, canonical) {
  const absolute = resolve(path);
  const resolved = resolve(canonical);
  for (const [alias, target] of KNOWN_SYSTEM_ALIASES) {
    if (
      (absolute === alias || absolute.startsWith(`${alias}${sep}`)) &&
      resolved === resolve(target + absolute.slice(alias.length))
    ) {
      return true;
    }
  }
  return false;
}

function isSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

function safeRootValue(value, label) {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.length > 4096 ||
    value.includes("\0") ||
    value.includes(",")
  ) {
    throw new Error(`${label} must be a non-empty path without NUL or comma`);
  }
  return value;
}

async function readableDirectory(value, label) {
  const input = safeRootValue(value, label);
  const lexical = resolve(input);
  const pathRoot = parse(lexical).root;
  const components = lexical.slice(pathRoot.length).split(sep).filter(Boolean);
  let current = pathRoot;
  // Check the raw path and every existing ancestor before realpath. Resolving
  // first would turn a caller-selected symlink root into an apparently safe
  // directory and would also permit a custom hooks root to escape its tree.
  for (let index = 0; index < components.length; index += 1) {
    current = current ? join(current, components[index]) : components[index];
    const info = await lstat(current);
    if (info.isSymbolicLink()) {
      const canonical = await realpath(current).catch(() => null);
      if (canonical && isKnownSystemAlias(current, canonical)) continue;
      throw new Error(`${label} or one of its ancestors is a symbolic link`);
    }
    if (index < components.length - 1 && !info.isDirectory()) {
      throw new Error(`${label} ancestor is not a directory`);
    }
  }
  const resolved = await realpath(lexical);
  if (resolved !== lexical && !isKnownSystemAlias(lexical, resolved)) {
    throw new Error(`${label} or one of its ancestors is a symbolic link`);
  }
  const info = await lstat(resolved);
  if (!info.isDirectory() || info.isSymbolicLink()) {
    throw new Error(`${label} must resolve to a regular directory`);
  }
  // A second raw-path pass narrows the lstat -> realpath race. The audit
  // walker performs the same checks before importing any source module.
  current = pathRoot;
  for (let index = 0; index < components.length; index += 1) {
    current = current ? join(current, components[index]) : components[index];
    const currentInfo = await lstat(current);
    if (currentInfo.isSymbolicLink()) {
      const canonical = await realpath(current).catch(() => null);
      if (canonical && isKnownSystemAlias(current, canonical)) continue;
      throw new Error(`${label} or one of its ancestors is a symbolic link`);
    }
  }
  return resolved;
}

async function normalizeRoots({
  sourceRoot = defaultSourceRoot,
  irRoot = defaultIrRoot,
  hooksRoot,
} = {}) {
  const [source, ir] = await Promise.all([
    readableDirectory(sourceRoot, "sourceRoot"),
    readableDirectory(irRoot, "irRoot"),
  ]);
  const hooks = await readableDirectory(
    hooksRoot ?? join(ir, "hooks"),
    "hooksRoot",
  );
  return { sourceRoot: source, irRoot: ir, hooksRoot: hooks };
}

// Node's permission matcher treats a path prefix such as `specs` as a
// textual prefix, so granting both `bundle/specs` and the sibling
// `bundle/specs-ir` makes the first directory itself unreadable. macOS's
// firmlink exposes the same tree at this non-overlapping canonical path. Use
// it only after realpath has proved that it is the identical directory; this
// changes neither the caller's root nor the audit's containment checks.
//
// Linux has no alternate spelling, so `permissionGrants` below handles the
// same collision generically for whatever roots remain.
async function permissionSafeSourceRoot(sourceRoot) {
  if (process.platform !== "darwin" || !sourceRoot.startsWith("/Users/")) {
    return sourceRoot;
  }
  const candidate = `/System/Volumes/Data${sourceRoot}`;
  try {
    const [sourceInfo, candidateInfo, sourceCanonical, candidateCanonical] =
      await Promise.all([
        lstat(sourceRoot),
        lstat(candidate),
        realpath(sourceRoot),
        realpath(candidate),
      ]);
    if (
      sourceInfo.isDirectory() &&
      candidateInfo.isDirectory() &&
      sourceInfo.dev === candidateInfo.dev &&
      sourceInfo.ino === candidateInfo.ino &&
      sourceCanonical !== candidateCanonical
    ) {
      return candidate;
    }
  } catch {
    // The alternate firmlink is not present on every macOS volume.
  }
  return sourceRoot;
}

/**
 * Turn the read roots into `--allow-fs-read` grants.
 *
 * Reproduced on Node 22.23: when one granted directory is a textual prefix
 * of another granted sibling (`…/bundle/specs` and `…/bundle/specs-ir`), the
 * shorter directory itself is denied for readdir/lstat/realpath while files
 * inside it stay readable, and a trailing separator does not help. Granting
 * the shadowed root as `<root>*` restores it. The wildcard only adds siblings
 * that share the same name prefix, which the canonical bundle does not have,
 * and roots without a colliding sibling keep their exact grant.
 */
export function permissionGrants(roots) {
  const list = [...new Set(roots)];
  return list.map((root) => {
    const shadowed = list.some(
      (other) =>
        other !== root &&
        other.startsWith(root) &&
        !other.startsWith(`${root}${sep}`),
    );
    return shadowed ? `${root}*` : root;
  });
}

function rejectUnknownFields(value, allowed, label) {
  if (!isRecord(value)) throw new Error(`${label} must be an object`);
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) {
      throw new Error(`${label} contains unknown field ${key}`);
    }
  }
}

function rejectPublicAuditOption(
  options,
  label,
  allowed = ["sourceRoot", "irRoot", "hooksRoot"],
) {
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
    if (!allowed.includes(key)) {
      throw new Error(`${label} contains unknown field ${key}`);
    }
  }
}

async function validateAuditReport(report) {
  if (!isRecord(report) || report.ok !== true) {
    const errors = isRecord(report?.errors)
      ? Object.fromEntries(
          Object.entries(report.errors).filter(([, entries]) =>
            Array.isArray(entries) && entries.length,
          ),
        )
      : {};
    throw new Error(
      `strict source/IR audit failed: ${JSON.stringify(errors).slice(0, 2000)}`,
    );
  }
  if (!Array.isArray(report.hookManifest) || !Array.isArray(report.sourceToIr)) {
    throw new Error("reference audit report is missing full provenance");
  }
  const reproducibility = report.reproducibility;
  if (!isRecord(reproducibility) || reproducibility.algorithm !== "sha256") {
    throw new Error("reference audit report has invalid reproducibility metadata");
  }
  if (
    !isSha256(reproducibility.generatorSha256) ||
    !isSha256(reproducibility.manifestSha256)
  ) {
    throw new Error("reference audit report has invalid reproducibility digests");
  }
  const { reproducibility: omitted, ...auditedFields } = report;
  if (reproducibility.manifestSha256 !== sha256(JSON.stringify(auditedFields))) {
    throw new Error("reference audit report manifest digest is invalid");
  }
  const auditSource = await readFile(auditScript, "utf8");
  if (reproducibility.generatorSha256 !== sha256(auditSource)) {
    throw new Error("reference audit report was generated by a stale audit implementation");
  }
  return report;
}

function outputError(error) {
  return {
    status: "worker-failed",
    errorClass: error?.name ?? "Error",
    message: typeof error?.message === "string" ? error.message : String(error),
  };
}

async function readStdinBounded() {
  let input = "";
  for await (const chunk of process.stdin) {
    input += chunk;
    if (Buffer.byteLength(input, "utf8") > MAX_AUDIT_INPUT_BYTES) {
      throw new Error(`audit worker input exceeds ${MAX_AUDIT_INPUT_BYTES} bytes`);
    }
  }
  return input;
}

async function runAuditChild() {
  const input = await readStdinBounded();
  let request;
  try {
    request = JSON.parse(input);
  } catch (error) {
    throw new Error("audit worker request is invalid JSON", { cause: error });
  }
  rejectUnknownFields(
    request,
    ["sourceRoot", "irRoot", "hooksRoot", "lockProof"],
    "audit worker request",
  );
  const sourceRoot = safeRootValue(request.sourceRoot, "sourceRoot");
  const irRoot = safeRootValue(request.irRoot, "irRoot");
  const hooksRoot = safeRootValue(request.hooksRoot, "hooksRoot");

  // This dynamic import is intentionally reachable only in the child.  The
  // parent side therefore has no source-spec import edge at all.
  const { auditSpecsHooksForReferenceWorker } = await import(
    pathToFileURL(auditScript).href,
  );
  const report = await auditSpecsHooksForReferenceWorker(
    { sourceRoot, irRoot, hooksRoot },
    request.lockProof,
  );
  process.stdout.write(`${JSON.stringify(report)}\n`);
}

function sameRoot(left, right) {
  return (
    left.sourceRoot === right.sourceRoot &&
    left.irRoot === right.irRoot &&
    left.hooksRoot === right.hooksRoot
  );
}

function deepFreeze(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) {
    return value;
  }
  for (const child of Object.values(value)) deepFreeze(child);
  return Object.freeze(value);
}

/** Start a fresh permission-restricted audit child while the pair lock is held. */
async function spawnReferenceAudit(roots, timeoutMs, lockProof) {
  const childRoots = {
    ...roots,
    sourceRoot: await permissionSafeSourceRoot(roots.sourceRoot),
  };
  const input = JSON.stringify({ ...childRoots, lockProof });
  if (Buffer.byteLength(input, "utf8") > MAX_AUDIT_INPUT_BYTES) {
    throw new Error(`audit worker input exceeds ${MAX_AUDIT_INPUT_BYTES} bytes`);
  }

  // Importing child_process is a parent-only operation. Keeping it dynamic is
  // important: the audit child executes under a policy that denies
  // child_process, fs-write, and worker access. Node's permission model does
  // not provide network isolation, so this worker makes no such claim.
  const { spawn } = await import("node:child_process");
  // Permit only the selected trees and the scripts/dependency roots needed to
  // load the audit implementation. Do not grant a custom fixture's parent:
  // that would make sibling files readable by the restricted child and would
  // undermine the audit root boundary.
  const fsReadRoots = new Set([
    repoScriptsDir,
    repoNodeModulesDir,
    childRoots.sourceRoot,
    childRoots.irRoot,
    childRoots.hooksRoot,
    pairLockPath,
  ]);
  return new Promise((resolveReport, rejectReport) => {
    const child = spawn(
      process.execPath,
      [
        "--experimental-permission",
        "--experimental-vm-modules",
        ...permissionGrants(fsReadRoots).map(
          (grant) => `--allow-fs-read=${grant}`,
        ),
        "--no-warnings",
        fileURLToPath(import.meta.url),
      ],
      {
        cwd: repoDir,
        shell: false,
        stdio: ["pipe", "pipe", "pipe"],
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
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let timedOut = false;
    let exceededOutput = false;
    let settled = false;
    const finishReject = (error) => {
      if (settled) return;
      settled = true;
      rejectReport(error);
    };
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdin.on("error", () => {});
    child.stdout.on("data", (chunk) => {
      stdoutBytes += Buffer.byteLength(chunk, "utf8");
      if (stdoutBytes > MAX_AUDIT_REPORT_BYTES) {
        exceededOutput = true;
        child.kill("SIGKILL");
        return;
      }
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderrBytes += Buffer.byteLength(chunk, "utf8");
      if (stderrBytes > MAX_AUDIT_STDERR_BYTES) {
        exceededOutput = true;
        child.kill("SIGKILL");
        return;
      }
      stderr += chunk;
    });
    child.on("error", finishReject);
    child.on("close", (code, signal) => {
      clearTimeout(timer);
      if (settled) return;
      if (timedOut) {
        finishReject(new Error("reference audit worker timed out"));
        return;
      }
      if (exceededOutput) {
        finishReject(new Error("reference audit worker exceeded its output limit"));
        return;
      }
      if (code !== 0) {
        const suffix = stderr ? `: ${stderr.slice(0, 500)}` : "";
        finishReject(
          new Error(
            `reference audit worker failed (${signal ?? `exit ${code}`})${suffix}`,
          ),
        );
        return;
      }
      if (!stdout.trim() || stdout.trim().includes("\n")) {
        finishReject(new Error("reference audit worker returned malformed JSON"));
        return;
      }
      let report;
      try {
        report = JSON.parse(stdout.trim());
      } catch (error) {
        finishReject(new Error("reference audit worker returned invalid JSON", { cause: error }));
        return;
      }
      validateAuditReport(report)
        .then((valid) => {
          if (!settled) {
            settled = true;
            resolveReport(valid);
          }
        })
        .catch(finishReject);
    });
    child.stdin.end(input);
  });
}

async function assertPairStable(roots, before, label) {
  const after = await verifyPair({
    sourceRoot: roots.sourceRoot,
    irRoot: roots.irRoot,
  });
  if (after.pairSha256 !== before.pairSha256) {
    throw new Error(`${label}: source/IR pair changed while reading or probing`);
  }
  return after;
}

async function generateReferenceAuditLocked(roots, timeoutMs, lockProof) {
  const before = await verifyPair(roots);
  const report = await spawnReferenceAudit(roots, timeoutMs, lockProof);
  await assertPairStable(roots, before, "reference audit");
  return report;
}

/**
 * Start a fresh permission-restricted audit child and return its full report.
 * The pair lock covers both the child and the final generation check, so a
 * publisher cannot slip a different source/IR generation between them.
 */
export async function generateReferenceAudit(options = {}) {
  rejectPublicAuditOption(options, "generateReferenceAudit", [
    "sourceRoot",
    "irRoot",
    "hooksRoot",
    "timeoutMs",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    hooksRoot,
    timeoutMs = REFERENCE_AUDIT_TIMEOUT_MS,
  } = options;
  if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 60_000) {
    throw new Error("reference audit timeout must be 100..60000 ms");
  }
  const active = auditContext.getStore();
  const run = async (lockProof) => {
    const roots = await normalizeRoots({ sourceRoot, irRoot, hooksRoot });
    return generateReferenceAuditLocked(roots, timeoutMs, lockProof);
  };
  // A caller may request an audit from inside an existing operation (for
  // example a test callback). Reusing that held lock avoids self-deadlock;
  // the nested operation still performs its own final pair check.
  if (active) return run(active.lockProof);
  return withPairLock(pairLockPath, (lock) => run(lock.proof));
}

/** Run a group of probes under one locked, immutable audit context. */
async function withReferenceAuditLocked(options, callback, lockProof) {
  const roots = await normalizeRoots(options);
  const before = await verifyPair(roots);
  const audit = await spawnReferenceAudit(
    roots,
    REFERENCE_AUDIT_TIMEOUT_MS,
    lockProof,
  );
  const frozenAudit = deepFreeze(audit);
  const result = await auditContext.run(
    {
      ...roots,
      audit: frozenAudit,
      pairSha256: before.pairSha256,
      lockProof,
    },
    () => callback(frozenAudit),
  );
  await assertPairStable(roots, before, "reference audit and probes");
  return result;
}

/**
 * Run a group of probes under one newly generated audit. The pair lock stays
 * held until the callback and its final provenance recheck both complete.
 */
export async function withReferenceAudit(options = {}, callback) {
  rejectPublicAuditOption(options, "withReferenceAudit", [
    "sourceRoot",
    "irRoot",
    "hooksRoot",
  ]);
  if (typeof callback !== "function") {
    throw new Error("withReferenceAudit callback is required");
  }
  const active = auditContext.getStore();
  if (active) {
    const roots = await normalizeRoots({
      sourceRoot: options.sourceRoot ?? active.sourceRoot,
      irRoot: options.irRoot ?? active.irRoot,
      hooksRoot: options.hooksRoot ?? active.hooksRoot,
    });
    if (sameRoot(active, roots)) return callback(active.audit);
    // The outer context already owns the repository pair lock. Do not try to
    // acquire it again for a nested, explicitly different root.
    return withReferenceAuditLocked(roots, callback, active.lockProof);
  }
  return withPairLock(pairLockPath, (lock) =>
    withReferenceAuditLocked(options, callback, lock.proof),
  );
}

/** @internal Used by capture-hook-reference; it never accepts caller data. */
export async function referenceAuditFor(options = {}) {
  rejectPublicAuditOption(options, "referenceAuditFor", [
    "sourceRoot",
    "irRoot",
    "hooksRoot",
  ]);
  const active = auditContext.getStore();
  const roots = await normalizeRoots({
    sourceRoot: options?.sourceRoot ?? active?.sourceRoot,
    irRoot: options?.irRoot ?? active?.irRoot,
    // A probe API need not repeat an operation's custom hooks root. Reuse it
    // when present; an explicitly supplied root still gets its own audit.
    hooksRoot: options?.hooksRoot ?? active?.hooksRoot,
  });
  if (active && sameRoot(active, roots)) return active.audit;
  return generateReferenceAudit(roots);
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  try {
    await runAuditChild();
  } catch (error) {
    process.stdout.write(`${JSON.stringify(outputError(error))}\n`);
    process.exitCode = 1;
  }
}
