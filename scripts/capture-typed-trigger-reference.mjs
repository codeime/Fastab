#!/usr/bin/env node
/**
 * Capture a deterministic, synthetic reference for every typed trigger.
 *
 * This is a build/test-time differential check. Each trigger is evaluated in
 * one isolated VM worker for the source path and one worker for the generated
 * closure module. The workers receive only JSON strings and an empty executor
 * rule set: no command marker is sent and no CLI/shell is ever started.
 */
import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, realpath } from "node:fs/promises";
import {
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  captureHookModuleReferenceBatch,
  captureHookReferenceBatch,
} from "./capture-hook-reference.mjs";
import { withReferenceAudit } from "./reference-audit-worker.mjs";
import { comparePath, verifyPair } from "./spec-pair.mjs";
import { writeReferenceFile } from "./reference-safe-io.mjs";
import {
  HOOK_MODULE_MANIFEST,
  TYPED_HOOK_DESCRIPTOR_MAX_BYTES,
  TYPED_HOOK_ID_MAX_BYTES,
  TYPED_HOOK_MODULE_MAX_BYTES,
  TYPED_HOOK_PATH_MAX_BYTES,
  TYPED_HOOK_SIDECAR,
  TYPED_HOOK_SIDECAR_KIND,
  TYPED_HOOK_SIDECAR_VERSION,
  hookFileName,
  utf8ByteLength,
} from "./spec-hook-contract.mjs";
import { compileTypedHook, validateTypedHookIr } from "./typed-hook-ir.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const defaultSourceRoot = join(repoDir, "bundle", "specs");
const defaultIrRoot = join(repoDir, "bundle", "specs-ir");
const defaultBaseline = join(
  repoDir,
  "crates",
  "ec_engine",
  "testdata",
  "typed-hooks",
  "reference.json",
);
const defaultGetQueryTermBaseline = join(
  repoDir,
  "crates",
  "ec_engine",
  "testdata",
  "typed-hooks",
  "asdf-get-query-term-reference.json",
);
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

export const REFERENCE_BASELINE_VERSION = 1;
export const REFERENCE_BASELINE_KIND = "typed-trigger-reference";
export const REFERENCE_TIMEOUT_MS = 7000;
export const REFERENCE_CONCURRENCY = 4;
export const MAX_CORPUS_CASES = 256;
export const MAX_BASELINE_BYTES = 64 * 1024 * 1024;

// The compiler emits two references to this one closure in asdf.  This
// allowlist is deliberately closed: a new getQueryTerm hook must not silently
// become part of the native research path without a new differential review.
export const ASDF_GET_QUERY_TERM_CANDIDATE_IDS = Object.freeze([
  "asdf#getQueryTerm#7",
  "asdf#getQueryTerm#8",
]);
export const ASDF_GET_QUERY_TERM_BODY =
  'n=>n.includes("latest")?n.slice(n.indexOf(":")+1):n';
export const GET_QUERY_TERM_INPUT_CORPUS = Object.freeze([
  Object.freeze({ id: "empty", args: [""] }),
  Object.freeze({ id: "plain", args: ["channel"] }),
  Object.freeze({ id: "latest-no-colon", args: ["latest"] }),
  Object.freeze({ id: "latest-after-colon", args: ["channel:latest"] }),
  Object.freeze({ id: "latest-before-colon", args: ["latest:channel"] }),
  Object.freeze({ id: "first-colon", args: ["a:latest:b:latest"] }),
  Object.freeze({ id: "utf16-prefix", args: ["😀:latest"] }),
  Object.freeze({ id: "utf16-without-match", args: ["😀/plain"] }),
]);
export const GET_QUERY_TERM_REFERENCE_BASELINE_VERSION = 1;
export const GET_QUERY_TERM_REFERENCE_BASELINE_KIND =
  "typed-get-query-term-reference";

// Keep these cases deliberately independent of today's command/spec list. The
// trigger contract is [search, previous], and every value is a plain string.
// In particular, no $referenceExec marker is present in this corpus.
export const INPUT_CORPUS = Object.freeze([
  Object.freeze({ id: "empty", args: ["", ""] }),
  Object.freeze({ id: "empty-after-value", args: ["", "x"] }),
  Object.freeze({ id: "plain-change", args: ["git", "g"] }),
  Object.freeze({ id: "plain-previous", args: ["value", "other"] }),
  Object.freeze({ id: "comma", args: ["one,two", "one,"] }),
  Object.freeze({ id: "comma-trailing", args: ["one,", "one"] }),
  Object.freeze({ id: "colon", args: ["scope:item", "scope:"] }),
  Object.freeze({ id: "colon-trailing", args: ["scope:", "scope:item"] }),
  Object.freeze({ id: "colon-added", args: ["scope:item", "scope"] }),
  Object.freeze({ id: "colon-removed", args: ["scope", "scope:item"] }),
  Object.freeze({ id: "separators-trailing", args: ["a,b:", "a,b:"] }),
  Object.freeze({ id: "short-g", args: ["-g", ""] }),
  Object.freeze({ id: "long-global", args: ["--global", "-g"] }),
  Object.freeze({ id: "short-G", args: ["-G", "--global"] }),
  Object.freeze({ id: "accent-slash", args: ["é/", "x/"] }),
  Object.freeze({ id: "emoji-slash", args: ["😀/", "é/"] }),
  Object.freeze({ id: "emoji-prefix", args: ["a😀/", "ab/"] }),
  Object.freeze({ id: "composed-unicode", args: ["café/", "café/"] }),
  Object.freeze({ id: "decomposed-unicode", args: ["café/", "café/"] }),
]);

const HARNESS_FILES = Object.freeze([
  "scripts/audit-spec-hooks.mjs",
  "scripts/capture-hook-reference.mjs",
  "scripts/capture-typed-trigger-reference.mjs",
  "scripts/filepaths-helper.mjs",
  "scripts/reference-audit-worker.mjs",
  "scripts/reference-hook-worker.mjs",
  "scripts/reference-safe-io.mjs",
  "scripts/spec-hook-contract.mjs",
  "scripts/spec-pair.mjs",
  "scripts/typed-hook-inline.mjs",
  "scripts/typed-hook-ir.mjs",
  "scripts/typed-regex.mjs",
]);

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

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
    if (!allowed.includes(key)) {
      throw new Error(`${label} contains unknown field ${key}`);
    }
  }
}

function assertSha(value, label) {
  if (typeof value !== "string" || !/^[a-f0-9]{64}$/.test(value)) {
    throw new Error(`${label} must be a lowercase SHA-256 digest`);
  }
  return value;
}

function assertKnownFields(value, fields, label) {
  if (!isRecord(value)) throw new Error(`${label} must be an object`);
  const allowed = new Set(fields);
  for (const key of Object.keys(value)) {
    if (!allowed.has(key))
      throw new Error(`${label} contains unknown field ${key}`);
  }
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value)
      .sort(comparePath)
      .map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function sortedIds(values) {
  return [...values].sort(comparePath);
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

function assertCorpus(cases) {
  if (!Array.isArray(cases) || cases.length === 0) {
    throw new Error("typed trigger reference corpus must be non-empty");
  }
  if (cases.length > MAX_CORPUS_CASES) {
    throw new Error(
      `typed trigger reference corpus exceeds ${MAX_CORPUS_CASES}`,
    );
  }
  const ids = new Set();
  let bytes = 0;
  for (const item of cases) {
    assertKnownFields(item, ["id", "args"], "reference corpus case");
    if (
      typeof item.id !== "string" ||
      !item.id ||
      ids.has(item.id) ||
      utf8ByteLength(item.id) > TYPED_HOOK_ID_MAX_BYTES ||
      !isSafeCorpusText(item.id, { caseId: true })
    ) {
      throw new Error("reference corpus case ids must be unique safe strings");
    }
    if (
      !Array.isArray(item.args) ||
      item.args.length !== 2 ||
      item.args.some(
        (arg) => typeof arg !== "string" || !isSafeCorpusText(arg),
      )
    ) {
      throw new Error(
        `reference corpus case ${item.id} must have two string arguments`,
      );
    }
    if (JSON.stringify(item).includes("$referenceExec")) {
      throw new Error(
        "typed trigger corpus must not contain an executor marker",
      );
    }
    ids.add(item.id);
    bytes += utf8ByteLength(JSON.stringify(item));
  }
  if (bytes > 1_000_000) {
    throw new Error("typed trigger reference corpus exceeds the input limit");
  }
}

function assertGetQueryTermCorpus(cases) {
  if (!Array.isArray(cases) || cases.length === 0) {
    throw new Error("getQueryTerm reference corpus must be non-empty");
  }
  if (cases.length > MAX_CORPUS_CASES) {
    throw new Error(
      `getQueryTerm reference corpus exceeds ${MAX_CORPUS_CASES}`,
    );
  }
  const ids = new Set();
  let bytes = 0;
  for (const item of cases) {
    assertKnownFields(item, ["id", "args"], "getQueryTerm reference corpus case");
    if (
      typeof item.id !== "string" ||
      !item.id ||
      ids.has(item.id) ||
      utf8ByteLength(item.id) > TYPED_HOOK_ID_MAX_BYTES ||
      !isSafeCorpusText(item.id, { caseId: true })
    ) {
      throw new Error(
        "getQueryTerm reference corpus case ids must be unique safe strings",
      );
    }
    if (
      !Array.isArray(item.args) ||
      item.args.length !== 1 ||
      typeof item.args[0] !== "string" ||
      !isSafeCorpusText(item.args[0])
    ) {
      throw new Error(
        `getQueryTerm reference case ${item.id} must have one string argument`,
      );
    }
    if (JSON.stringify(item).includes("$referenceExec")) {
      throw new Error(
        "getQueryTerm reference corpus must not contain an executor marker",
      );
    }
    ids.add(item.id);
    bytes += utf8ByteLength(JSON.stringify(item));
  }
  if (bytes > 1_000_000) {
    throw new Error("getQueryTerm reference corpus exceeds the input limit");
  }
}

function isSafeCorpusText(value, { caseId = false } = {}) {
  // Rust's serde_json rejects unpaired UTF-16 surrogates while JavaScript's
  // JSON parser/stringifier accepts them. Reject them here so the baseline
  // corpus has one cross-language contract without excluding valid Unicode,
  // including emoji and composed/decomposed forms.
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!Number.isInteger(next) || next < 0xdc00 || next > 0xdfff) {
        return false;
      }
      index += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      return false;
    }
    if (
      caseId &&
      ((unit <= 0x1f) ||
        (unit >= 0x7f && unit <= 0x9f) ||
        unit === 0x2f ||
        unit === 0x5c)
    ) {
      return false;
    }
  }
  return true;
}

function validRelativeFile(file) {
  return (
    typeof file === "string" &&
    file.length > 0 &&
    !isAbsolute(file) &&
    !file.includes("\0") &&
    !file.includes("\\") &&
    file.split("/").every((part) => part && part !== "." && part !== "..")
  );
}

function validSourceHookPath(path) {
  return (
    typeof path === "string" &&
    /^root(?:\.[A-Za-z_$][\w$]*|\[\d+\])*$/.test(path)
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

function pathsMatchKnownAlias(path, canonical) {
  const target = knownAliasTarget(path);
  return target !== null && resolve(canonical) === target;
}

function isInside(root, target) {
  const path = relative(resolve(root), resolve(target));
  return (
    path === "" ||
    (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`))
  );
}

async function assertNoSymlinkAncestors(
  path,
  label,
  { allowMissingLeaf = false } = {},
) {
  const absolute = resolve(path);
  const components = absolute.split(sep).filter(Boolean);
  let current = absolute.startsWith(sep) ? sep : "";
  for (let index = 0; index < components.length; index += 1) {
    current = current ? join(current, components[index]) : components[index];
    try {
      const info = await lstat(current);
      if (info.isSymbolicLink()) {
        const canonical = await realpath(current).catch(() => null);
        if (!canonical || !pathsMatchKnownAlias(current, canonical)) {
          throw new Error(`${label} contains a symbolic-link ancestor`);
        }
        continue;
      }
      if (index < components.length - 1 && !info.isDirectory()) {
        throw new Error(`${label} ancestor is not a directory`);
      }
    } catch (error) {
      if (
        error?.code === "ENOENT" &&
        allowMissingLeaf &&
        index === components.length - 1
      ) {
        return absolute;
      }
      throw error;
    }
  }
  return absolute;
}

async function readSafeAbsolute(path, label, { root = null } = {}) {
  const absolute = resolve(path);
  if (root && !isInside(root, absolute)) {
    throw new Error(`${label} escapes its approved root`);
  }
  await assertNoSymlinkAncestors(absolute, label);
  const before = await lstat(absolute);
  if (before.isSymbolicLink()) throw new Error(`${label} is a symbolic link`);
  if (!before.isFile()) throw new Error(`${label} must be a regular file`);
  const canonical = await realpath(absolute);
  const approvedRoot = root ? await realpath(resolve(root)) : null;
  if (
    (canonical !== absolute && !pathsMatchKnownAlias(absolute, canonical)) ||
    (approvedRoot && !isInside(approvedRoot, canonical))
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

async function readRegular(root, file, label) {
  if (!validRelativeFile(file)) throw new Error(`${label} has an unsafe path`);
  return readSafeAbsolute(join(root, file), label, { root });
}

async function writeSafeAbsolute(path, text, label) {
  return writeReferenceFile(path, text, {
    overwrite: true,
    label,
  });
}

async function hashFiles(files) {
  const records = await Promise.all(
    files.map(async (file) => ({
      file,
      sha256: sha256(await readRegular(repoDir, file, `harness file ${file}`)),
    })),
  );
  records.sort((left, right) => comparePath(left.file, right.file));
  const canonical = records
    .map(({ file, sha256: digest }) => `${file}\0${digest}\n`)
    .join("");
  return sha256(Buffer.from(canonical));
}

async function loadSidecar(irRoot, audit) {
  const sidecarText = await readRegular(
    irRoot,
    TYPED_HOOK_SIDECAR,
    "typed hook sidecar",
  );
  const sidecarSha256 = sha256(sidecarText);
  if (audit.typedHooks?.sidecarSha256 !== sidecarSha256) {
    throw new Error("typed hook sidecar SHA differs from strict audit");
  }
  if (utf8ByteLength(sidecarText) > MAX_BASELINE_BYTES) {
    throw new Error("typed hook sidecar exceeds the reference input limit");
  }
  let sidecar;
  try {
    sidecar = JSON.parse(sidecarText);
  } catch (error) {
    throw new Error("typed hook sidecar is invalid JSON", { cause: error });
  }
  assertKnownFields(
    sidecar,
    ["version", "kind", "contracts", "hooks"],
    "typed hook sidecar",
  );
  if (
    sidecar.version !== TYPED_HOOK_SIDECAR_VERSION ||
    sidecar.kind !== TYPED_HOOK_SIDECAR_KIND ||
    !isRecord(sidecar.contracts) ||
    !isRecord(sidecar.hooks)
  ) {
    throw new Error("typed hook sidecar schema is invalid");
  }
  if (audit.typedHooks.hooks !== Object.keys(sidecar.hooks).length) {
    throw new Error("typed hook sidecar count differs from strict audit");
  }
  return { sidecar, sidecarSha256 };
}

function parseHookManifest(text) {
  let manifest;
  try {
    manifest = JSON.parse(text);
  } catch (error) {
    throw new Error("closure-preserving hook manifest is invalid JSON", {
      cause: error,
    });
  }
  assertKnownFields(
    manifest,
    ["version", "kind", "hooks", "modules"],
    "closure-preserving hook manifest",
  );
  if (
    manifest.version !== 1 ||
    manifest.kind !== "closure-preserving-hook-modules" ||
    !isRecord(manifest.hooks) ||
    !isRecord(manifest.modules)
  ) {
    throw new Error("closure-preserving hook manifest schema is invalid");
  }
  for (const [id, descriptor] of Object.entries(manifest.hooks)) {
    assertKnownFields(
      descriptor,
      ["module", "moduleSha256", "path", "sourceField", "functionBodySha256"],
      `closure-preserving hook manifest hook ${id}`,
    );
    if (
      typeof descriptor.module !== "string" ||
      !/^[^/\\\0]+\.js$/.test(descriptor.module) ||
      descriptor.module === ".js" ||
      descriptor.module === "..js" ||
      !/^[a-f0-9]{64}$/.test(descriptor.moduleSha256) ||
      !/^[a-f0-9]{64}$/.test(descriptor.functionBodySha256) ||
      !validSourceHookPath(descriptor.path) ||
      typeof descriptor.sourceField !== "string"
    ) {
      throw new Error(
        `closure-preserving hook manifest hook ${id} has invalid identity`,
      );
    }
  }
  for (const [file, metadata] of Object.entries(manifest.modules)) {
    assertKnownFields(
      metadata,
      ["source", "sourceSha256", "moduleSha256", "hookIds"],
      `closure-preserving hook manifest module ${file}`,
    );
    if (
      !/^[^/\\\0]+\.js$/.test(file) ||
      !isRecord(metadata) ||
      !validRelativeFile(metadata.source) ||
      !/^[a-f0-9]{64}$/.test(metadata.sourceSha256) ||
      !/^[a-f0-9]{64}$/.test(metadata.moduleSha256) ||
      !Array.isArray(metadata.hookIds)
    ) {
      throw new Error(
        `closure-preserving hook manifest module ${file} has invalid metadata`,
      );
    }
  }
  return manifest;
}

async function loadEvidence({ sourceRoot, irRoot, audit }) {
  if (!audit || audit.ok !== true) {
    const errors = Object.fromEntries(
      Object.entries(audit?.errors ?? {}).filter(
        ([, entries]) => entries.length,
      ),
    );
    throw new Error(
      `strict source/IR audit failed: ${JSON.stringify(errors).slice(0, 2000)}`,
    );
  }
  const pair = await verifyPair({ sourceRoot, irRoot });
  const { sidecar, sidecarSha256 } = await loadSidecar(irRoot, audit);
  const hookManifestText = await readRegular(
    irRoot,
    HOOK_MODULE_MANIFEST,
    "closure-preserving hook manifest",
  );
  const hookManifestSha256 = sha256(hookManifestText);
  if (audit.hookModules?.manifestSha256 !== hookManifestSha256) {
    throw new Error("closure-preserving hook manifest SHA differs from audit");
  }
  const hookManifest = parseHookManifest(hookManifestText);
  if (
    audit.hookModules?.manifestHooks !== Object.keys(hookManifest.hooks).length ||
    audit.hookModules?.manifestModules !== Object.keys(hookManifest.modules).length
  ) {
    throw new Error("closure-preserving hook manifest counts differ from audit");
  }
  return {
    audit,
    pair,
    sidecar,
    sidecarSha256,
    hookManifestSha256,
    hookManifest,
  };
}

function auditedSourceInstance(audit, hookId) {
  for (const record of audit.sourceToIr ?? []) {
    for (const [sourceField, instances] of Object.entries(
      record.hookInstances ?? {},
    )) {
      for (const instance of instances ?? []) {
        if (instance?.id === hookId) return { record, sourceField, instance };
      }
    }
  }
  return null;
}

function auditedTypedIds(audit, sidecar, hookManifest) {
  const auditManifest = new Map(
    (audit.hookManifest ?? []).map((entry) => [entry.id, entry]),
  );
  const closureManifest = new Map(Object.entries(hookManifest.hooks));
  const ids = sortedIds(Object.keys(sidecar.hooks));
  if (ids.length === 0) throw new Error("typed hook sidecar has no hooks");
  for (const id of ids) {
    const entry = sidecar.hooks[id];
    if (!isRecord(entry) || entry.sourceField !== "trigger") {
      throw new Error(`typed hook ${id} has an invalid trigger entry`);
    }
    const audited = auditManifest.get(id);
    const descriptor = closureManifest.get(id);
    const source = auditedSourceInstance(audit, id);
    if (!audited || audited.field !== "jsTrigger" || !source) {
      throw new Error(`typed hook ${id} is not a strict audited trigger`);
    }
    if (!descriptor) {
      throw new Error(`typed hook ${id} is missing from the closure manifest`);
    }
    const moduleMetadata = hookManifest.modules[descriptor.module];
    if (
      audited.file !== hookFileName(id) ||
      audited.ir !== source.record.ir ||
      !/^[a-f0-9]{64}$/.test(audited.sha256) ||
      source.sourceField !== "trigger" ||
      descriptor.path !== source.instance.path ||
      descriptor.sourceField !== source.sourceField ||
      descriptor.functionBodySha256 !== source.instance.sha256 ||
      !isRecord(moduleMetadata) ||
      moduleMetadata.moduleSha256 !== descriptor.moduleSha256 ||
      moduleMetadata.source !== source.record.source ||
      moduleMetadata.sourceSha256 !== source.record.sourceSha256 ||
      !moduleMetadata.hookIds.includes(id)
    ) {
      throw new Error(`typed hook ${id} closure identity differs from audit`);
    }
    for (const field of [
      "module",
      "moduleSha256",
      "functionBodySha256",
      "path",
      "sourceField",
    ]) {
      if (entry[field] !== descriptor[field]) {
        throw new Error(`typed hook ${id} ${field} differs from audit`);
      }
    }
  }
  return ids;
}

function typedIdentity(audit, hookManifest, id, manifestSha256) {
  const source = auditedSourceInstance(audit, id);
  const audited = (audit.hookManifest ?? []).find((entry) => entry.id === id);
  const descriptor = hookManifest.hooks[id];
  if (!source || !audited || !descriptor) {
    throw new Error(`typed hook ${id} has incomplete audited identity`);
  }
  return {
    hookId: id,
    source: source.record.source,
    path: descriptor.path,
    field: source.sourceField,
    sourceField: descriptor.sourceField,
    ir: source.record.ir,
    irSha256: source.record.irSha256,
    sourceSha256: source.record.sourceSha256,
    functionBodySha256: descriptor.functionBodySha256,
    hookFile: audited.file,
    hookFileSha256: audited.sha256,
    module: descriptor.module,
    moduleSha256: descriptor.moduleSha256,
    manifestSha256,
  };
}

function auditedAsdfGetQueryTermIds(audit, hookManifest) {
  const manifestIds = Object.keys(hookManifest.hooks).filter((id) =>
    id.startsWith("asdf#getQueryTerm#"),
  );
  if (stableJson(sortedIds(manifestIds)) !== stableJson(ASDF_GET_QUERY_TERM_CANDIDATE_IDS)) {
    throw new Error(
      "asdf getQueryTerm candidate set is not exactly the two reviewed hook ids",
    );
  }
  const expectedBodySha256 = sha256(ASDF_GET_QUERY_TERM_BODY);
  for (const id of ASDF_GET_QUERY_TERM_CANDIDATE_IDS) {
    const source = auditedSourceInstance(audit, id);
    const descriptor = hookManifest.hooks[id];
    if (
      !source ||
      source.sourceField !== "getQueryTerm" ||
      !descriptor ||
      descriptor.sourceField !== "getQueryTerm" ||
      descriptor.functionBodySha256 !== expectedBodySha256 ||
      source.instance.sha256 !== expectedBodySha256 ||
      descriptor.path !== source.instance.path
    ) {
      throw new Error(
        `asdf getQueryTerm candidate ${id} does not match the reviewed source closure`,
      );
    }
  }
  return [...ASDF_GET_QUERY_TERM_CANDIDATE_IDS];
}

async function readAuditedAsdfGetQueryTermBody(audit, irRoot, id) {
  const audited = (audit.hookManifest ?? []).find((entry) => entry.id === id);
  if (!audited || typeof audited.file !== "string") {
    throw new Error(`asdf getQueryTerm ${id} has no audited hook artifact`);
  }
  const text = await readRegular(
    irRoot,
    `hooks/${audited.file}`,
    `asdf getQueryTerm hook artifact ${id}`,
  );
  const match = text.trim().match(/^export\s+default\s+([\s\S]*?);?$/);
  if (!match) {
    throw new Error(`asdf getQueryTerm ${id} hook artifact has no standalone body`);
  }
  const body = match[1].trim();
  if (sha256(text) !== audited.sha256) {
    throw new Error(`asdf getQueryTerm ${id} hook artifact SHA differs from audit`);
  }
  const bodySha256 = sha256(body);
  const source = auditedSourceInstance(audit, id);
  if (!source || bodySha256 !== source.instance.sha256) {
    throw new Error(`asdf getQueryTerm ${id} hook artifact body SHA differs from source audit`);
  }
  if (bodySha256 !== sha256(ASDF_GET_QUERY_TERM_BODY)) {
    throw new Error(`asdf getQueryTerm ${id} hook artifact is not the reviewed source closure`);
  }
  return body;
}

function getQueryTermIdentity(audit, hookManifest, id, manifestSha256) {
  const identity = typedIdentity(audit, hookManifest, id, manifestSha256);
  if (identity.field !== "getQueryTerm" || identity.sourceField !== "getQueryTerm") {
    throw new Error(`asdf getQueryTerm ${id} has an unexpected source field`);
  }
  return identity;
}

function assertRunIdentity(run, identity, pathName) {
  for (const field of [
    "hookId",
    "source",
    "path",
    "field",
    "sourceField",
    "ir",
    "irSha256",
    "sourceSha256",
    "functionBodySha256",
    "hookFile",
    "hookFileSha256",
  ]) {
    if (run[field] !== identity[field]) {
      throw new Error(
        `typed trigger ${identity.hookId} ${pathName} identity field ${field} differs`,
      );
    }
  }
  if (pathName === "module") {
    for (const field of ["module", "moduleSha256"]) {
      if (run[field] !== identity[field]) {
        throw new Error(
          `typed trigger ${identity.hookId} module identity field ${field} differs`,
        );
      }
    }
    if (run.manifestSha256 !== identity.manifestSha256) {
      throw new Error(
        `typed trigger ${identity.hookId} module manifest identity differs`,
      );
    }
  } else if (Object.hasOwn(run, "module") || Object.hasOwn(run, "moduleSha256")) {
    throw new Error(
      `typed trigger ${identity.hookId} source result unexpectedly contains module identity`,
    );
  }
}

function verifyRun(run, { id, pathName, index, identity }) {
  if (!run || run.status !== "success") {
    throw new Error(
      `typed trigger ${id} ${pathName} case ${index} did not succeed: ${JSON.stringify(run)}`,
    );
  }
  if (typeof run.value !== "boolean") {
    throw new Error(
      `typed trigger ${id} ${pathName} case ${index} was not boolean`,
    );
  }
  if (!Array.isArray(run.execTrace) || run.execTrace.length !== 0) {
    throw new Error(
      `typed trigger ${id} ${pathName} case ${index} executed a command`,
    );
  }
  if (pathName === "source" && run.sourceShaVerified !== true) {
    throw new Error(`typed trigger ${id} source identity was not verified`);
  }
  if (pathName === "module" && run.moduleShaVerified !== true) {
    throw new Error(`typed trigger ${id} module identity was not verified`);
  }
  assertRunIdentity(run, identity, pathName);
}

function verifyGetQueryTermRun(run, { id, pathName, index, identity }) {
  if (!run || run.status !== "success") {
    throw new Error(
      `asdf getQueryTerm ${id} ${pathName} case ${index} did not succeed: ${JSON.stringify(run)}`,
    );
  }
  if (typeof run.value !== "string") {
    throw new Error(
      `asdf getQueryTerm ${id} ${pathName} case ${index} was not a string`,
    );
  }
  if (!Array.isArray(run.execTrace) || run.execTrace.length !== 0) {
    throw new Error(`asdf getQueryTerm ${id} ${pathName} case ${index} executed a command`);
  }
  if (pathName === "source" && run.sourceShaVerified !== true) {
    throw new Error(`asdf getQueryTerm ${id} source identity was not verified`);
  }
  if (pathName === "module" && run.moduleShaVerified !== true) {
    throw new Error(`asdf getQueryTerm ${id} module identity was not verified`);
  }
  assertRunIdentity(run, identity, pathName);
}

async function probeHook({ id, sourceRoot, irRoot, cases, identity }) {
  const invocations = cases.map((item) => ({
    args: [...item.args],
    mockExecRules: [],
  }));
  const [source, module] = await Promise.all([
    captureHookReferenceBatch({
      hookId: id,
      invocations,
      sourceRoot,
      irRoot,
      timeoutMs: REFERENCE_TIMEOUT_MS,
    }),
    captureHookModuleReferenceBatch({
      hookId: id,
      invocations,
      sourceRoot,
      irRoot,
      timeoutMs: REFERENCE_TIMEOUT_MS,
    }),
  ]);
  if (
    source.runs.length !== cases.length ||
    module.runs.length !== cases.length
  ) {
    throw new Error(`typed trigger ${id} did not return every corpus case`);
  }
  const expected = [];
  for (let index = 0; index < cases.length; index += 1) {
    const sourceRun = source.runs[index];
    const moduleRun = module.runs[index];
    verifyRun(sourceRun, { id, pathName: "source", index, identity });
    verifyRun(moduleRun, { id, pathName: "module", index, identity });
    if (sourceRun.value !== moduleRun.value) {
      throw new Error(`typed trigger ${id} differs between source and module`);
    }
    if (stableJson(sourceRun.execTrace) !== stableJson(moduleRun.execTrace)) {
      throw new Error(`typed trigger ${id} executor traces differ`);
    }
    expected.push(sourceRun.value);
  }
  return expected;
}

async function runConcurrent(items, concurrency, fn) {
  const output = new Array(items.length);
  let next = 0;
  async function worker() {
    while (true) {
      const index = next;
      next += 1;
      if (index >= items.length) return;
      output[index] = await fn(items[index], index);
    }
  }
  await Promise.all(
    Array.from({ length: Math.min(concurrency, items.length) }, () => worker()),
  );
  return output;
}

export async function buildTypedTriggerReference(options = {}) {
  rejectPublicAuditOption(options, "buildTypedTriggerReference", [
    "sourceRoot",
    "irRoot",
    "cases",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    cases = INPUT_CORPUS,
  } = options;
  assertCorpus(cases);
  return withReferenceAudit({ sourceRoot, irRoot }, async (audit) => {
    const evidence = await loadEvidence({ sourceRoot, irRoot, audit });
    const ids = auditedTypedIds(
      evidence.audit,
      evidence.sidecar,
      evidence.hookManifest,
    );
    const expectedValues = await runConcurrent(
      ids,
      REFERENCE_CONCURRENCY,
      (id) =>
        probeHook({
          id,
          sourceRoot,
          irRoot,
          cases,
          identity: typedIdentity(
            evidence.audit,
            evidence.hookManifest,
            id,
            evidence.hookManifestSha256,
          ),
        }),
    );
    const afterPair = await verifyPair({ sourceRoot, irRoot });
    if (afterPair.pairSha256 !== evidence.pair.pairSha256) {
      throw new Error("source/IR pair changed while probing typed triggers");
    }
    const generatorSha256 = sha256(
      await readRegular(
        repoDir,
        "scripts/compile-spec-ir.mjs",
        "compiler generator",
      ),
    );
    const harnessSha256 = await hashFiles(HARNESS_FILES);
    const expected = Object.fromEntries(
      ids.map((id, index) => [id, expectedValues[index]]),
    );
    const baseline = {
      version: REFERENCE_BASELINE_VERSION,
      kind: REFERENCE_BASELINE_KIND,
      generatorSha256,
      harnessSha256,
      pairSha256: evidence.pair.pairSha256,
      hookManifestSha256: evidence.hookManifestSha256,
      sidecarSha256: evidence.sidecarSha256,
      cases: clone(cases),
      catalog: clone(evidence.sidecar),
      expected,
    };
    validateReferenceBaseline(baseline);
    return baseline;
  });
}

async function probeAsdfGetQueryTerm({ id, sourceRoot, irRoot, cases, identity }) {
  const invocations = cases.map((item) => ({
    args: [...item.args],
    mockExecRules: [],
  }));
  const [source, module] = await Promise.all([
    captureHookReferenceBatch({
      hookId: id,
      invocations,
      sourceRoot,
      irRoot,
      timeoutMs: REFERENCE_TIMEOUT_MS,
    }),
    captureHookModuleReferenceBatch({
      hookId: id,
      invocations,
      sourceRoot,
      irRoot,
      timeoutMs: REFERENCE_TIMEOUT_MS,
    }),
  ]);
  if (
    source.runs.length !== cases.length ||
    module.runs.length !== cases.length
  ) {
    throw new Error(`asdf getQueryTerm ${id} did not return every corpus case`);
  }
  const expected = [];
  for (let index = 0; index < cases.length; index += 1) {
    const sourceRun = source.runs[index];
    const moduleRun = module.runs[index];
    verifyGetQueryTermRun(sourceRun, {
      id,
      pathName: "source",
      index,
      identity,
    });
    verifyGetQueryTermRun(moduleRun, {
      id,
      pathName: "module",
      index,
      identity,
    });
    if (sourceRun.value !== moduleRun.value) {
      throw new Error(`asdf getQueryTerm ${id} differs between source and module`);
    }
    if (stableJson(sourceRun.execTrace) !== stableJson(moduleRun.execTrace)) {
      throw new Error(`asdf getQueryTerm ${id} executor traces differ`);
    }
    expected.push(sourceRun.value);
  }
  return expected;
}

export async function buildTypedGetQueryTermReference(options = {}) {
  rejectPublicAuditOption(options, "buildTypedGetQueryTermReference", [
    "sourceRoot",
    "irRoot",
    "cases",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    cases = GET_QUERY_TERM_INPUT_CORPUS,
  } = options;
  assertGetQueryTermCorpus(cases);
  return withReferenceAudit({ sourceRoot, irRoot }, async (audit) => {
    const evidence = await loadEvidence({ sourceRoot, irRoot, audit });
    const ids = auditedAsdfGetQueryTermIds(
      evidence.audit,
      evidence.hookManifest,
    );
    const descriptors = await Promise.all(
      ids.map(async (id) =>
        compileTypedHook({
          body: await readAuditedAsdfGetQueryTermBody(evidence.audit, irRoot, id),
          sourceField: "getQueryTerm",
        }),
      ),
    );
    const descriptor = descriptors[0];
    if (descriptors.some((candidate) => stableJson(candidate) !== stableJson(descriptor))) {
      throw new Error("asdf getQueryTerm candidate descriptors differ");
    }
    const identities = Object.fromEntries(
      ids.map((id) => [
        id,
        getQueryTermIdentity(
          evidence.audit,
          evidence.hookManifest,
          id,
          evidence.hookManifestSha256,
        ),
      ]),
    );
    const expectedValues = await runConcurrent(
      ids,
      REFERENCE_CONCURRENCY,
      (id) =>
        probeAsdfGetQueryTerm({
          id,
          sourceRoot,
          irRoot,
          cases,
          identity: identities[id],
        }),
    );
    const afterPair = await verifyPair({ sourceRoot, irRoot });
    if (afterPair.pairSha256 !== evidence.pair.pairSha256) {
      throw new Error("source/IR pair changed while probing asdf getQueryTerm");
    }
    const generatorSha256 = sha256(
      await readRegular(
        repoDir,
        "scripts/compile-spec-ir.mjs",
        "compiler generator",
      ),
    );
    const harnessSha256 = await hashFiles(HARNESS_FILES);
    const candidates = Object.fromEntries(
      ids.map((id) => {
        const identity = identities[id];
        return [
          id,
          {
            source: identity.source,
            sourceSha256: identity.sourceSha256,
            ir: identity.ir,
            irSha256: identity.irSha256,
            hookFile: identity.hookFile,
            hookFileSha256: identity.hookFileSha256,
            module: identity.module,
            moduleSha256: identity.moduleSha256,
            path: identity.path,
            sourceField: identity.sourceField,
            functionBodySha256: identity.functionBodySha256,
            descriptor,
          },
        ];
      }),
    );
    const baseline = {
      version: GET_QUERY_TERM_REFERENCE_BASELINE_VERSION,
      kind: GET_QUERY_TERM_REFERENCE_BASELINE_KIND,
      generatorSha256,
      harnessSha256,
      pairSha256: evidence.pair.pairSha256,
      hookManifestSha256: evidence.hookManifestSha256,
      cases: clone(cases),
      candidates,
      expected: Object.fromEntries(
        ids.map((id, index) => [id, expectedValues[index]]),
      ),
    };
    validateGetQueryTermReferenceBaseline(baseline);
    return baseline;
  });
}

// Keep an asdf-named alias for callers that describe this research slice by
// its reviewed source rather than by the generic hook field.
export const buildAsdfGetQueryTermReference = buildTypedGetQueryTermReference;

function validateGetQueryTermReferenceBaseline(value) {
  assertKnownFields(
    value,
    [
      "version",
      "kind",
      "generatorSha256",
      "harnessSha256",
      "pairSha256",
      "hookManifestSha256",
      "cases",
      "candidates",
      "expected",
    ],
    "typed getQueryTerm reference baseline",
  );
  if (
    value.version !== GET_QUERY_TERM_REFERENCE_BASELINE_VERSION ||
    value.kind !== GET_QUERY_TERM_REFERENCE_BASELINE_KIND
  ) {
    throw new Error("typed getQueryTerm reference baseline version or kind is invalid");
  }
  for (const field of [
    "generatorSha256",
    "harnessSha256",
    "pairSha256",
    "hookManifestSha256",
  ]) {
    assertSha(value[field], `getQueryTerm baseline ${field}`);
  }
  assertGetQueryTermCorpus(value.cases);
  if (!isRecord(value.candidates) || !isRecord(value.expected)) {
    throw new Error("typed getQueryTerm reference candidates and expected must be objects");
  }
  const candidateIds = sortedIds(Object.keys(value.candidates));
  if (stableJson(candidateIds) !== stableJson(ASDF_GET_QUERY_TERM_CANDIDATE_IDS)) {
    throw new Error("typed getQueryTerm baseline must contain exactly the two asdf candidates");
  }
  if (stableJson(candidateIds) !== stableJson(sortedIds(Object.keys(value.expected)))) {
    throw new Error("typed getQueryTerm expected ids do not match candidates");
  }
  const expectedBodySha256 = sha256(ASDF_GET_QUERY_TERM_BODY);
  for (const id of candidateIds) {
    const entry = value.candidates[id];
    assertKnownFields(
      entry,
      [
        "source",
        "sourceSha256",
        "ir",
        "irSha256",
        "hookFile",
        "hookFileSha256",
        "module",
        "moduleSha256",
        "path",
        "sourceField",
        "functionBodySha256",
        "descriptor",
      ],
      `typed getQueryTerm candidate ${id}`,
    );
    if (
      !validRelativeFile(entry.source) ||
      !validRelativeFile(entry.ir) ||
      !validRelativeFile(entry.hookFile) ||
      !/^[^/\\\0]+\.js$/.test(entry.hookFile) ||
      utf8ByteLength(entry.source) > TYPED_HOOK_PATH_MAX_BYTES ||
      utf8ByteLength(entry.ir) > TYPED_HOOK_PATH_MAX_BYTES ||
      utf8ByteLength(entry.hookFile) > TYPED_HOOK_MODULE_MAX_BYTES
    ) {
      throw new Error(`typed getQueryTerm candidate ${id} has unsafe file provenance`);
    }
    for (const field of ["sourceSha256", "irSha256", "hookFileSha256"]) {
      assertSha(entry[field], `typed getQueryTerm candidate ${id}.${field}`);
    }
    if (
      typeof entry.module !== "string" ||
      !/^[^/\\\0]+\.js$/.test(entry.module) ||
      entry.module === ".js" ||
      entry.module === "..js" ||
      utf8ByteLength(entry.module) > TYPED_HOOK_MODULE_MAX_BYTES
    ) {
      throw new Error(`typed getQueryTerm candidate ${id} has an invalid module`);
    }
    assertSha(entry.moduleSha256, `typed getQueryTerm candidate ${id}.moduleSha256`);
    assertSha(
      entry.functionBodySha256,
      `typed getQueryTerm candidate ${id}.functionBodySha256`,
    );
    if (entry.functionBodySha256 !== expectedBodySha256) {
      throw new Error(`typed getQueryTerm candidate ${id} source body is not the reviewed closure`);
    }
    if (
      typeof entry.path !== "string" ||
      !validSourceHookPath(entry.path) ||
      utf8ByteLength(entry.path) > TYPED_HOOK_PATH_MAX_BYTES ||
      entry.sourceField !== "getQueryTerm"
    ) {
      throw new Error(`typed getQueryTerm candidate ${id} provenance is invalid`);
    }
    if (!isRecord(entry.descriptor)) {
      throw new Error(`typed getQueryTerm candidate ${id} descriptor is invalid`);
    }
    try {
      validateTypedHookIr(entry.descriptor);
    } catch (error) {
      throw new Error(`typed getQueryTerm candidate ${id} descriptor is invalid`, {
        cause: error,
      });
    }
    const expected = value.expected[id];
    if (
      !Array.isArray(expected) ||
      expected.length !== value.cases.length ||
      expected.some((item) => typeof item !== "string")
    ) {
      throw new Error(`typed getQueryTerm expected values for ${id} are invalid`);
    }
  }
  if (utf8ByteLength(JSON.stringify(value)) > MAX_BASELINE_BYTES) {
    throw new Error("typed getQueryTerm reference baseline is too large");
  }
  return value;
}

export async function checkTypedGetQueryTermReference(options = {}) {
  rejectPublicAuditOption(options, "checkTypedGetQueryTermReference", [
    "sourceRoot",
    "irRoot",
    "baselinePath",
    "cases",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    baselinePath = defaultGetQueryTermBaseline,
    cases = GET_QUERY_TERM_INPUT_CORPUS,
  } = options;
  const baselineText = await readSafeAbsolute(
    baselinePath,
    "typed getQueryTerm reference baseline",
  );
  if (utf8ByteLength(baselineText) > MAX_BASELINE_BYTES) {
    throw new Error("typed getQueryTerm reference baseline is too large");
  }
  let baseline;
  try {
    baseline = JSON.parse(baselineText);
  } catch (error) {
    throw new Error("typed getQueryTerm reference baseline is invalid JSON", {
      cause: error,
    });
  }
  validateGetQueryTermReferenceBaseline(baseline);
  const actual = await buildTypedGetQueryTermReference({
    sourceRoot,
    irRoot,
    cases,
  });
  if (baselineText !== `${JSON.stringify(actual)}\n`) {
    throw new Error(
      "typed getQueryTerm reference baseline is stale or differs from the current source/module probes",
    );
  }
  return {
    baseline,
    hooks: Object.keys(actual.expected).length,
    cases: actual.cases.length,
  };
}

export async function updateTypedGetQueryTermReference(options = {}) {
  rejectPublicAuditOption(options, "updateTypedGetQueryTermReference", [
    "sourceRoot",
    "irRoot",
    "baselinePath",
    "cases",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    baselinePath = defaultGetQueryTermBaseline,
    cases = GET_QUERY_TERM_INPUT_CORPUS,
  } = options;
  const baseline = await buildTypedGetQueryTermReference({
    sourceRoot,
    irRoot,
    cases,
  });
  const text = `${JSON.stringify(baseline)}\n`;
  await writeSafeAbsolute(
    baselinePath,
    text,
    "typed getQueryTerm reference baseline",
  );
  return {
    baseline,
    hooks: Object.keys(baseline.expected).length,
    cases: baseline.cases.length,
    bytes: utf8ByteLength(text),
  };
}

export const checkAsdfGetQueryTermReference = checkTypedGetQueryTermReference;
export const updateAsdfGetQueryTermReference = updateTypedGetQueryTermReference;

function validateReferenceBaseline(value) {
  assertKnownFields(
    value,
    [
      "version",
      "kind",
      "generatorSha256",
      "harnessSha256",
      "pairSha256",
      "hookManifestSha256",
      "sidecarSha256",
      "cases",
      "catalog",
      "expected",
    ],
    "typed trigger reference baseline",
  );
  if (
    value.version !== REFERENCE_BASELINE_VERSION ||
    value.kind !== REFERENCE_BASELINE_KIND
  ) {
    throw new Error(
      "typed trigger reference baseline version or kind is invalid",
    );
  }
  for (const field of [
    "generatorSha256",
    "harnessSha256",
    "pairSha256",
    "hookManifestSha256",
    "sidecarSha256",
  ]) {
    assertSha(value[field], `baseline ${field}`);
  }
  assertCorpus(value.cases);
  assertKnownFields(
    value.catalog,
    ["version", "kind", "contracts", "hooks"],
    "baseline catalog",
  );
  if (
    value.catalog.version !== TYPED_HOOK_SIDECAR_VERSION ||
    value.catalog.kind !== TYPED_HOOK_SIDECAR_KIND ||
    !isRecord(value.catalog.contracts) ||
    !isRecord(value.catalog.hooks)
  ) {
    throw new Error("baseline catalog schema is invalid");
  }
  assertKnownFields(value.catalog.contracts, ["trigger"], "baseline contracts");
  assertKnownFields(
    value.catalog.contracts.trigger,
    ["irVersion", "params", "resultType"],
    "baseline trigger contract",
  );
  if (
    value.catalog.contracts.trigger.irVersion !== 1 ||
    stableJson(value.catalog.contracts.trigger.params) !==
      stableJson(["string", "string"]) ||
    value.catalog.contracts.trigger.resultType !== "bool"
  ) {
    throw new Error("baseline trigger contract is invalid");
  }
  for (const [id, entry] of Object.entries(value.catalog.hooks)) {
    if (
      typeof id !== "string" ||
      !id ||
      id.includes("\0") ||
      id.includes("\\") ||
      utf8ByteLength(id) > TYPED_HOOK_ID_MAX_BYTES
    ) {
      throw new Error("baseline catalog contains an invalid hook id");
    }
    assertKnownFields(
      entry,
      [
        "module",
        "moduleSha256",
        "path",
        "sourceField",
        "functionBodySha256",
        "descriptor",
      ],
      `baseline catalog hook ${id}`,
    );
    if (
      typeof entry.module !== "string" ||
      !/^[^/\\\0]+\.js$/.test(entry.module) ||
      entry.module === ".js" ||
      entry.module === "..js" ||
      utf8ByteLength(entry.module) > TYPED_HOOK_MODULE_MAX_BYTES
    ) {
      throw new Error(`baseline catalog hook ${id} has an invalid module`);
    }
    assertSha(entry.moduleSha256, `baseline catalog hook ${id}.moduleSha256`);
    assertSha(
      entry.functionBodySha256,
      `baseline catalog hook ${id}.functionBodySha256`,
    );
    if (
      typeof entry.path !== "string" ||
      !/^root(?:\.[A-Za-z_$][\w$]*|\[\d+\])*$/.test(entry.path) ||
      utf8ByteLength(entry.path) > TYPED_HOOK_PATH_MAX_BYTES ||
      entry.sourceField !== "trigger"
    ) {
      throw new Error(`baseline catalog hook ${id} provenance is invalid`);
    }
    if (!isRecord(entry.descriptor)) {
      throw new Error(`baseline catalog hook ${id} descriptor is invalid`);
    }
    try {
      validateTypedHookIr(entry.descriptor);
      if (
        utf8ByteLength(JSON.stringify(entry.descriptor)) >
        TYPED_HOOK_DESCRIPTOR_MAX_BYTES
      ) {
        throw new Error("descriptor exceeds its serialized size limit");
      }
    } catch (error) {
      throw new Error(`baseline catalog hook ${id} descriptor is invalid`, {
        cause: error,
      });
    }
  }
  if (!isRecord(value.expected))
    throw new Error("baseline expected must be an object");
  const catalogIds = sortedIds(Object.keys(value.catalog.hooks));
  const expectedIds = sortedIds(Object.keys(value.expected));
  if (
    catalogIds.length !== expectedIds.length ||
    catalogIds.some((id, index) => id !== expectedIds[index])
  ) {
    throw new Error("baseline expected ids do not match catalog ids");
  }
  for (const id of expectedIds) {
    const values = value.expected[id];
    if (!Array.isArray(values) || values.length !== value.cases.length) {
      throw new Error(`baseline expected values for ${id} are incomplete`);
    }
    if (values.some((entry) => typeof entry !== "boolean")) {
      throw new Error(`baseline expected values for ${id} must be boolean`);
    }
  }
  const bytes = utf8ByteLength(JSON.stringify(value));
  if (bytes > MAX_BASELINE_BYTES) {
    throw new Error(
      `typed trigger reference baseline exceeds ${MAX_BASELINE_BYTES}`,
    );
  }
  return value;
}

export async function checkTypedTriggerReference(options = {}) {
  rejectPublicAuditOption(options, "checkTypedTriggerReference", [
    "sourceRoot",
    "irRoot",
    "baselinePath",
    "cases",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    baselinePath = defaultBaseline,
    cases = INPUT_CORPUS,
  } = options;
  const baselineText = await readSafeAbsolute(
    baselinePath,
    "typed trigger reference baseline",
  );
  if (utf8ByteLength(baselineText) > MAX_BASELINE_BYTES) {
    throw new Error("typed trigger reference baseline is too large");
  }
  let baseline;
  try {
    baseline = JSON.parse(baselineText);
  } catch (error) {
    throw new Error("typed trigger reference baseline is invalid JSON", {
      cause: error,
    });
  }
  validateReferenceBaseline(baseline);
  const actual = await buildTypedTriggerReference({
    sourceRoot,
    irRoot,
    cases,
  });
  const expectedText = `${JSON.stringify(actual)}\n`;
  if (baselineText !== expectedText) {
    throw new Error(
      "typed trigger reference baseline is stale or differs from the current source/module probes",
    );
  }
  return {
    baseline,
    hooks: Object.keys(actual.expected).length,
    cases: actual.cases.length,
  };
}

export async function updateTypedTriggerReference(options = {}) {
  rejectPublicAuditOption(options, "updateTypedTriggerReference", [
    "sourceRoot",
    "irRoot",
    "baselinePath",
    "cases",
  ]);
  const {
    sourceRoot = defaultSourceRoot,
    irRoot = defaultIrRoot,
    baselinePath = defaultBaseline,
    cases = INPUT_CORPUS,
  } = options;
  const baseline = await buildTypedTriggerReference({
    sourceRoot,
    irRoot,
    cases,
  });
  const text = `${JSON.stringify(baseline)}\n`;
  await writeSafeAbsolute(
    baselinePath,
    text,
    "typed trigger reference baseline",
  );
  return {
    baseline,
    hooks: Object.keys(baseline.expected).length,
    cases: baseline.cases.length,
    bytes: utf8ByteLength(text),
  };
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const update = process.argv.includes("--update");
  const check = process.argv.includes("--check");
  const getQueryTerm = process.argv.includes("--get-query-term");
  const modeCount = Number(update) + Number(check);
  const getOption = (name, fallback) => {
    const prefix = `${name}=`;
    const value = process.argv.find((arg) => arg.startsWith(prefix));
    return value ? value.slice(prefix.length) : fallback;
  };
  if (modeCount !== 1) {
    process.stderr.write(
      "error: choose exactly one of --update or --check; baseline is never changed by default\n",
    );
    process.exitCode = 2;
  } else {
    try {
      const options = {
        sourceRoot: getOption("--source-root", defaultSourceRoot),
        irRoot: getOption("--ir-root", defaultIrRoot),
        baselinePath: getOption(
          "--baseline",
          getQueryTerm ? defaultGetQueryTermBaseline : defaultBaseline,
        ),
      };
      const result = getQueryTerm
        ? update
          ? await updateTypedGetQueryTermReference(options)
          : await checkTypedGetQueryTermReference(options)
        : update
          ? await updateTypedTriggerReference(options)
          : await checkTypedTriggerReference(options);
      process.stdout.write(
        `${update ? "Updated" : "Verified"} ${getQueryTerm ? "typed getQueryTerm" : "typed trigger"} reference: ${result.hooks} hooks x ${result.cases} cases${result.bytes ? ` (${result.bytes} bytes)` : ""}\n`,
      );
    } catch (error) {
      process.stderr.write(
        `typed trigger reference ${update ? "update" : "check"} failed: ${error instanceof Error ? error.message : error}\n`,
      );
      process.exitCode = 1;
    }
  }
}
