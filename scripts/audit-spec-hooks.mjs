#!/usr/bin/env node
/**
 * Audit the source-spec -> static IR -> extracted-hook pipeline.
 *
 * The report is deliberately derived from the files on disk. It must not
 * encode today's spec or hook counts: a changed specs package should either
 * produce a different, reproducible report or fail with the exact missing
 * edge. The audit is read-only; compilation remains the producer of the IR.
 */
import { createHash } from "node:crypto";
import { constants } from "node:fs";
import {
  lstat,
  open,
  readdir,
  realpath,
} from "node:fs/promises";
import {
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import vm, { Script } from "node:vm";
import * as acorn from "acorn";
import * as eslintScope from "eslint-scope";

import { functionSource, isFilepathsHelper } from "./filepaths-helper.mjs";
import { writeReferenceFile } from "./reference-safe-io.mjs";
import {
  hookFileName,
  HOOK_MODULE_MANIFEST,
  HOOK_MODULES_DIR,
  KNOWN_NON_SPEC_FILES,
  SUPPORTED_HOOK_FIELDS,
  SUPPORTED_IR_HOOK_FIELDS,
  TYPED_HOOK_CATALOG_MAX_BYTES,
  TYPED_HOOK_CATALOG_MAX_HOOKS,
  TYPED_HOOK_DESCRIPTOR_MAX_BYTES,
  TYPED_HOOK_ID_MAX_BYTES,
  TYPED_HOOK_MODULE_MAX_BYTES,
  TYPED_HOOK_PATH_MAX_BYTES,
  TYPED_HOOK_SIDECAR,
  TYPED_HOOK_SIDECAR_KIND,
  TYPED_HOOK_SIDECAR_VERSION,
  utf8ByteLength,
} from "./spec-hook-contract.mjs";
import {
  applySpecDiff,
  derivedVersionIrBaseSource,
  derivedVersionIrFamily,
  isDerivedVersionIr,
  versionDiffKeys,
} from "./spec-versions.mjs";
import {
  PAIR_LOCK_NAME,
  PAIR_MARKER_NAME,
  comparePath,
  verifyPair,
  verifyPairLockProof,
  withPairLock,
} from "./spec-pair.mjs";
import {
  TYPED_HOOK_SIDECAR_FIELDS,
  tryCompileTypedHook,
  typedHookSidecarContracts,
  validateTypedHookIr,
} from "./typed-hook-ir.mjs";
import {
  isRegisteredNativeAdapter,
  loadNativeHookAdapters,
} from "./native-hook-adapters.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const sourceDir = process.env.EC_SPECS_SRC || join(repoDir, "bundle", "specs");
const irDir = process.env.EC_SPECS_IR || join(repoDir, "bundle", "specs-ir");
const hooksDir = join(irDir, "hooks");
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

// filepaths()/folders() are intentionally folded into native IR. Their
// custom, trigger, and getQueryTerm functions are therefore expected to be
// present in source but absent from extracted JS hooks.
const NATIVE_HELPER_FIELDS = new Set(["custom", "trigger", "getQueryTerm"]);
const SOURCE_FIELDS = Object.keys(SUPPORTED_HOOK_FIELDS);
const SOURCE_TO_IR = SUPPORTED_HOOK_FIELDS;
const IR_TO_SOURCE = Object.fromEntries(
  Object.entries(SUPPORTED_HOOK_FIELDS).map(([source, ir]) => [ir, source]),
);
const RISK_PATTERNS = [
  ["fig-global", /\bfig\./],
  ["require", /\brequire\s*\(/],
  ["intl", /\bIntl\./],
  ["process", /\bprocess\b/],
  ["window", /\bwindow\b/],
  ["execute-command", /\b(?:executeCommand|exec)\s*\(/],
];
// ESLint's scope walker reports truly unbound identifiers. These standard
// ECMAScript globals and the console injected by JsHost do not indicate a
// module closure; every other name needs a build-time/native decision.
const HOOK_GLOBALS = new Set([
  "Array",
  "ArrayBuffer",
  "AggregateError",
  "Atomics",
  "BigInt",
  "BigInt64Array",
  "BigUint64Array",
  "Boolean",
  "Date",
  "DataView",
  "Error",
  "EvalError",
  "FinalizationRegistry",
  "Float32Array",
  "Float64Array",
  "Function",
  "Infinity",
  "Int8Array",
  "Int16Array",
  "Int32Array",
  "Intl",
  "JSON",
  "Map",
  "Math",
  "NaN",
  "Number",
  "Object",
  "Promise",
  "Proxy",
  "RangeError",
  "ReferenceError",
  "RegExp",
  "Reflect",
  "Set",
  "SharedArrayBuffer",
  "String",
  "Symbol",
  "SyntaxError",
  "TypeError",
  "URIError",
  "Uint8Array",
  "Uint8ClampedArray",
  "Uint16Array",
  "Uint32Array",
  "WeakRef",
  "WeakMap",
  "WeakSet",
  "WebAssembly",
  "console",
  "decodeURI",
  "decodeURIComponent",
  "encodeURI",
  "encodeURIComponent",
  "escape",
  "eval",
  "globalThis",
  "isFinite",
  "isNaN",
  "parseFloat",
  "parseInt",
  "undefined",
  "unescape",
]);
const RISK_PATTERN_NAMES = [
  ...RISK_PATTERNS.map(([name]) => name),
  "free-variable-candidate",
];

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function analyzeHookBody(body) {
  const ast = acorn.parse("(" + body + ")", {
    ecmaVersion: "latest",
    sourceType: "module",
    ranges: true,
  });
  const unsupportedRuntimeSyntax = [];
  function visit(node) {
    if (!node || typeof node !== "object") return;
    if (Array.isArray(node)) {
      for (const child of node) visit(child);
      return;
    }
    if (node.type === "Literal" && node.regex?.flags?.includes("v")) {
      unsupportedRuntimeSyntax.push(
        "RegExp v flag is not supported by QuickJS",
      );
    }
    for (const value of Object.values(node)) visit(value);
  }
  visit(ast);
  const scope = eslintScope.analyze(ast, {
    ecmaVersion: 2024,
    sourceType: "module",
  });
  return {
    freeVariables: [
      ...new Set(
        scope.globalScope.through
          .map((reference) => reference.identifier.name)
          .filter((name) => !HOOK_GLOBALS.has(name)),
      ),
    ].sort(comparePath),
    unsupportedRuntimeSyntax: [...new Set(unsupportedRuntimeSyntax)],
  };
}

function freeVariableCandidates(body) {
  return analyzeHookBody(body).freeVariables;
}

function emptyFieldMap() {
  return Object.fromEntries(SOURCE_FIELDS.map((field) => [field, []]));
}

function emptyIrFieldMap() {
  return Object.fromEntries(
    SUPPORTED_IR_HOOK_FIELDS.map((field) => [field, []]),
  );
}

function normalizeRelativePath(value) {
  return value.replaceAll("\\", "/");
}

function isInside(root, target) {
  const path = relative(resolve(root), resolve(target));
  return (
    path === "" ||
    (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`))
  );
}

function isKnownSystemAlias(path) {
  const absolute = resolve(path);
  for (const [alias, target] of KNOWN_SYSTEM_ALIASES) {
    if (absolute === alias || absolute.startsWith(`${alias}${sep}`)) {
      return { alias, target: target + absolute.slice(alias.length) };
    }
  }
  return null;
}

function pathsMatchKnownAlias(path, canonical) {
  const alias = isKnownSystemAlias(path);
  return alias != null && resolve(canonical) === resolve(alias.target);
}

async function assertNoSymlinkAncestors(
  path,
  label,
  { allowMissingLeaf = false, ancestorRoot = null } = {},
) {
  const absolute = resolve(path);
  const floor = ancestorRoot ? resolve(ancestorRoot) : null;
  if (floor && !isInside(floor, absolute)) {
    throw new Error(`${label} escapes its approved ancestor root`);
  }
  const suffix = floor ? relative(floor, absolute) : absolute;
  const components = suffix.split(sep).filter(Boolean);
  let current = floor ?? (absolute.startsWith(sep) ? sep : "");
  const floorInfo = floor ? await lstat(floor) : null;
  if (floorInfo?.isSymbolicLink()) {
    const canonical = await realpath(floor).catch(() => null);
    if (!canonical || !pathsMatchKnownAlias(floor, canonical)) {
      throw new Error(`${label} contains a symbolic-link ancestor: ${floor}`);
    }
  }
  if (floorInfo && !floorInfo.isDirectory() && components.length > 0) {
    throw new Error(`${label} ancestor is not a directory: ${floor}`);
  }
  for (let index = 0; index < components.length; index += 1) {
    current = current ? join(current, components[index]) : components[index];
    try {
      const info = await lstat(current);
      if (info.isSymbolicLink()) {
        const canonical = await realpath(current).catch(() => null);
        if (canonical && pathsMatchKnownAlias(current, canonical)) continue;
        throw new Error(
          `${label} contains a symbolic-link ancestor: ${current}`,
        );
      }
      if (index < components.length - 1 && !info.isDirectory()) {
        throw new Error(`${label} ancestor is not a directory: ${current}`);
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

async function safeReadRegularFile(path, label, { root, encoding = "utf8" } = {}) {
  const absolute = resolve(path);
  const approvedRoot = root
    ? resolve(isKnownSystemAlias(root)?.target ?? root)
    : null;
  if (root && !isInside(root, absolute)) {
    throw new Error(`${label} escapes its approved root`);
  }
  // The parent worker has already lstat'ed every ancestor of each approved
  // root. In the restricted child, starting this pass at that exact root is
  // also necessary: Node's permission model intentionally denies lstat on
  // unrelated parents such as /Users and /Users/<user>.
  await assertNoSymlinkAncestors(absolute, label, { ancestorRoot: root });
  const before = await lstat(absolute);
  if (before.isSymbolicLink()) {
    throw new Error(`${label} is a symbolic link`);
  }
  if (!before.isFile()) {
    throw new Error(`${label} is a special or non-regular entry`);
  }
  const canonical = await realpath(absolute);
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
    return await handle.readFile({ encoding });
  } finally {
    await handle?.close().catch(() => {});
  }
}

/**
 * Evaluate exactly the bytes returned by safeReadRegularFile.
 *
 * The old audit path read a source file, then imported its pathname.  A
 * publisher (or an adversarial fixture) could replace that pathname in the
 * gap, so the code that was audited was not necessarily the code executed.
 * SourceTextModule keeps the audited bytes in memory and resolves only
 * relative imports from the same approved source root, reading each imported
 * module through the same identity-checked helper.  The data URL fallback is
 * only used by direct callers on Node builds without VM modules and refuses
 * modules which require a linker.
 */
async function importAuditedSourceModule(
  source,
  approvedSource,
  sourceRoot,
  label,
  auditedSources,
) {
  const sourcePath = resolve(approvedSource);
  const SourceTextModule = vm.SourceTextModule;
  if (typeof SourceTextModule !== "function") {
    const parsed = acorn.parse(source, {
      ecmaVersion: "latest",
      sourceType: "module",
    });
    const containsDynamicImport = (value) => {
      if (!value || typeof value !== "object") return false;
      if (value.type === "ImportExpression") return true;
      return Object.values(value).some((child) =>
        Array.isArray(child)
          ? child.some(containsDynamicImport)
          : containsDynamicImport(child),
      );
    };
    if (
      parsed.body.some((node) => node.type === "ImportDeclaration") ||
      containsDynamicImport(parsed)
    ) {
      throw new Error(
        `${label} uses imports but this Node runtime does not expose vm.SourceTextModule`,
      );
    }
    const edits = [];
    for (const node of parsed.body) {
      if (node.type === "ExportDefaultDeclaration") {
        edits.push({
          start: node.start,
          end: node.declaration.start,
          text: "globalThis.__auditDefault = ",
        });
      } else if (node.type === "ExportNamedDeclaration") {
        if (node.source) {
          throw new Error(
            `${label} re-exports a module but this Node runtime does not expose vm.SourceTextModule`,
          );
        }
        if (node.declaration) {
          edits.push({
            start: node.start,
            end: node.declaration.start,
            text: "",
          });
          continue;
        }
        const assignments = [];
        for (const specifier of node.specifiers ?? []) {
          const exported =
            specifier.exported?.name ?? specifier.exported?.value;
          if (specifier.local?.type !== "Identifier") {
            if (exported === "default") {
              throw new Error(`${label} has a non-identifier default export`);
            }
            continue;
          }
          const local = source.slice(
            specifier.local.start,
            specifier.local.end,
          );
          if (exported === "default") {
            assignments.push(`globalThis.__auditDefault = ${local};`);
          } else if (exported === "versions") {
            assignments.push(`globalThis.__auditVersions = ${local};`);
          }
        }
        edits.push({
          start: node.start,
          end: node.end,
          text: assignments.join(""),
        });
      } else if (node.type === "ExportAllDeclaration") {
        throw new Error(
          `${label} re-exports a module but this Node runtime does not expose vm.SourceTextModule`,
        );
      }
    }
    edits.sort((left, right) => right.start - left.start);
    let isolatedSource = source;
    for (const edit of edits) {
      isolatedSource =
        isolatedSource.slice(0, edit.start) +
        edit.text +
        isolatedSource.slice(edit.end);
    }
    // `const versions` is script-local and would otherwise be dropped. The
    // named export is what compile-spec-ir applies; keep it on the namespace
    // so derived IR can be matched against the merged trees.
    isolatedSource = `${isolatedSource}\n;if (typeof versions !== "undefined") globalThis.__auditVersions = versions;`;
    const commonJsModule = { exports: {} };
    const context = vm.createContext({
      console: Object.freeze({
        log() {},
        info() {},
        warn() {},
        error() {},
      }),
      module: commonJsModule,
      exports: commonJsModule.exports,
    });
    const script = new Script(isolatedSource, { filename: sourcePath });
    script.runInContext(context, { timeout: 5000 });
    if (!("__auditDefault" in context) && commonJsModule.exports !== undefined) {
      context.__auditDefault = commonJsModule.exports;
    }
    if (!("__auditDefault" in context)) {
      throw new Error(`${label} has no default export`);
    }
    return {
      default: context.__auditDefault,
      versions: context.__auditVersions,
    };
  }

  const context = vm.createContext({
    console: Object.freeze({
      log() {},
      info() {},
      warn() {},
      error() {},
    }),
  });
  const modules = new Map();
  const load = async (modulePath, moduleSource) => {
    const absolute = resolve(modulePath);
    const cached = modules.get(absolute);
    if (cached) return cached;
    const module = new SourceTextModule(moduleSource, {
      context,
      identifier: pathToFileURL(absolute).href,
      initializeImportMeta(meta) {
        meta.url = pathToFileURL(absolute).href;
      },
    });
    modules.set(absolute, module);
    return module;
  };
  const rootModule = await load(sourcePath, source);
  await rootModule.link(async (specifier, referencingModule) => {
    if (
      typeof specifier !== "string" ||
      (!specifier.startsWith("./") && !specifier.startsWith("../"))
    ) {
      throw new Error(`${label} imports a non-relative module: ${specifier}`);
    }
    const referencingPath = fileURLToPath(referencingModule.identifier);
    const importedPath = resolve(dirname(referencingPath), specifier);
    if (!isInside(sourceRoot, importedPath)) {
      throw new Error(`${label} import escapes its approved source root`);
    }
    await assertNoSymlinkAncestors(importedPath, `${label} import`, {
      ancestorRoot: sourceRoot,
    });
    const importedKey = resolve(importedPath);
    let importedSource = auditedSources.get(importedKey);
    if (importedSource === undefined) {
      importedSource = await safeReadRegularFile(
        importedPath,
        `${label} import ${specifier}`,
        { root: sourceRoot },
      );
      auditedSources.set(importedKey, importedSource);
    }
    return load(importedPath, importedSource);
  });
  await rootModule.evaluate({ timeout: 5000 });
  return rootModule.namespace;
}

async function assertRegularDirectoryRoot(path, label) {
  const absolute = resolve(path);
  const info = await lstat(absolute);
  if (info.isSymbolicLink()) {
    throw new Error(`${label} is a symbolic link`);
  }
  if (!info.isDirectory()) {
    throw new Error(`${label} is not a regular directory`);
  }
  const canonical = await realpath(absolute);
  if (canonical !== absolute && !pathsMatchKnownAlias(absolute, canonical)) {
    throw new Error(`${label} contains a symbolic-link ancestor`);
  }
  return absolute;
}

async function walkFiles(dir, extension, { skipRootIndex = false } = {}) {
  let entries;
  const root = await assertRegularDirectoryRoot(dir, `${extension} tree`);
  try {
    entries = await readdir(root, { withFileTypes: true });
  } catch (error) {
    throw new Error(`cannot read ${extension} tree ${root}: ${error.message}`, {
      cause: error,
    });
  }
  entries.sort((left, right) => comparePath(left.name, right.name));
  const files = [];
  for (const entry of entries) {
    // Check the raw POSIX name before normalization. On macOS a literal
    // `..\\escape.js` is one filename, but normalizing it first would turn it
    // into an apparent parent traversal.
    if (entry.name.includes("\\")) {
      throw new Error(
        `${extension} tree contains a literal backslash before normalization: ${entry.name}`,
      );
    }
    const full = join(root, entry.name);
    if (!isInside(root, full)) {
      throw new Error(`${extension} tree entry escapes its root: ${entry.name}`);
    }
    const info = await lstat(full);
    if (info.isSymbolicLink()) {
      throw new Error(`${extension} tree contains a symbolic link: ${full}`);
    }
    if (info.isDirectory()) {
      if (entry.name === "hooks") continue;
      const canonical = await realpath(full);
      const canonicalRoot = resolve(isKnownSystemAlias(root)?.target ?? root);
      if (
        (canonical !== resolve(full) && !pathsMatchKnownAlias(full, canonical)) ||
        !isInside(canonicalRoot, canonical)
      ) {
        throw new Error(`${extension} tree directory escapes its root: ${full}`);
      }
      const nested = await walkFiles(full, extension, { skipRootIndex: false });
      files.push(...nested.map((file) => join(entry.name, file)));
      continue;
    }
    if (!info.isFile()) {
      throw new Error(`${extension} tree contains a special entry: ${full}`);
    }
    if (!entry.name.endsWith(extension)) continue;
    if (skipRootIndex && entry.name === `index${extension}`) continue;
    files.push(entry.name);
  }
  return files
    .map(normalizeRelativePath)
    .sort(comparePath);
}

function isObject(value) {
  return value !== null && typeof value === "object";
}

function isRecord(value) {
  return isObject(value) && !Array.isArray(value);
}

function isSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

function typedHookContracts() {
  return typedHookSidecarContracts();
}

function canonicalJson(value) {
  if (Array.isArray(value)) {
    return `[${value.map((item) => canonicalJson(item)).join(",")}]`;
  }
  if (isRecord(value)) {
    return `{${Object.keys(value)
      .sort(comparePath)
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function typedHookStringError(value, label, maxBytes) {
  if (typeof value !== "string" || !value || value.includes("\0")) {
    return `${label} must be a string without NUL`;
  }
  if (utf8ByteLength(value) > maxBytes) {
    return `${label} exceeds the ${maxBytes}-byte UTF-8 limit`;
  }
  return null;
}

function validateTypedHookEntryShape(id, entry, errors) {
  const idError = typedHookStringError(
    id,
    "typed hook id",
    TYPED_HOOK_ID_MAX_BYTES,
  );
  if (idError || !id || id.includes("\\")) {
    errors.invalidTypedHookSidecar.push({
      field: `hooks.${id}`,
      reason: idError ?? "typed hook id must not contain a backslash",
    });
  }
  if (!isRecord(entry)) {
    errors.invalidTypedHookSidecar.push({
      field: `hooks.${id}`,
      reason: "typed hook entry must be an object",
    });
    return false;
  }
  rejectUnknownFields(
    errors,
    `typed hooks.${id}`,
    entry,
    [
      "module",
      "moduleSha256",
      "path",
      "sourceField",
      "functionBodySha256",
      "descriptor",
    ],
    "invalidTypedHookSidecar",
  );
  if (
    typeof entry.module !== "string" ||
    !/^[^/\\\0]+\.js$/.test(entry.module) ||
    entry.module === ".js" ||
    entry.module === "..js"
  ) {
    errors.invalidTypedHookSidecar.push({
      field: `hooks.${id}.module`,
      reason: "module must be one safe .js basename",
    });
  } else if (utf8ByteLength(entry.module) > TYPED_HOOK_MODULE_MAX_BYTES) {
    errors.invalidTypedHookSidecar.push({
      field: `hooks.${id}.module`,
      reason: `module exceeds the ${TYPED_HOOK_MODULE_MAX_BYTES}-byte UTF-8 limit`,
    });
  }
  for (const field of ["moduleSha256", "functionBodySha256"]) {
    if (!isSha256(entry[field])) {
      errors.invalidTypedHookSidecar.push({
        field: `hooks.${id}.${field}`,
        reason: `${field} must be a lowercase SHA-256 digest`,
      });
    }
  }
  const pathError = typedHookStringError(
    entry.path,
    `typed hook ${id} path`,
    TYPED_HOOK_PATH_MAX_BYTES,
  );
  if (pathError || !isSafeSourceHookPath(entry.path)) {
    errors.invalidTypedHookSidecar.push({
      field: `hooks.${id}.path`,
      reason:
        pathError ?? "path must be a normalized source object property path",
    });
  }
  if (!TYPED_HOOK_SIDECAR_FIELDS.includes(entry.sourceField)) {
    errors.invalidTypedHookSidecar.push({
      field: `hooks.${id}.sourceField`,
      reason: `sourceField must be one of ${TYPED_HOOK_SIDECAR_FIELDS.join(", ")}`,
    });
  }
  if (!isRecord(entry.descriptor)) {
    errors.invalidTypedHookSidecar.push({
      field: `hooks.${id}.descriptor`,
      reason: "descriptor must be an object",
    });
  } else {
    try {
      validateTypedHookIr(entry.descriptor);
      if (
        utf8ByteLength(JSON.stringify(entry.descriptor)) >
        TYPED_HOOK_DESCRIPTOR_MAX_BYTES
      ) {
        errors.invalidTypedHookSidecar.push({
          field: `hooks.${id}.descriptor`,
          reason: `descriptor exceeds the ${TYPED_HOOK_DESCRIPTOR_MAX_BYTES}-byte UTF-8 limit`,
        });
      }
    } catch (error) {
      errors.invalidTypedHookSidecar.push({
        field: `hooks.${id}.descriptor`,
        reason: "descriptor is not valid typed hook IR",
        message: error.message,
      });
    }
  }
  return true;
}

function validateTypedHookAdapterShape(id, entry, errors) {
  const idError = typedHookStringError(
    id,
    "typed hook adapter id",
    TYPED_HOOK_ID_MAX_BYTES,
  );
  if (idError || !id || id.includes("\\")) {
    errors.invalidTypedHookSidecar.push({
      field: `adapters.${id}`,
      reason: idError ?? "typed hook adapter id must not contain a backslash",
    });
  }
  if (!isRecord(entry)) {
    errors.invalidTypedHookSidecar.push({
      field: `adapters.${id}`,
      reason: "adapter binding must be an object",
    });
    return false;
  }
  rejectUnknownFields(
    errors,
    `typed adapters.${id}`,
    entry,
    ["path", "sourceField", "functionBodySha256"],
    "invalidTypedHookSidecar",
  );
  if (!isSha256(entry.functionBodySha256)) {
    errors.invalidTypedHookSidecar.push({
      field: `adapters.${id}.functionBodySha256`,
      reason: "functionBodySha256 must be a lowercase SHA-256 digest",
    });
  }
  const pathError = typedHookStringError(
    entry.path,
    `typed hook adapter ${id} path`,
    TYPED_HOOK_PATH_MAX_BYTES,
  );
  if (pathError || !isSafeSourceHookPath(entry.path)) {
    errors.invalidTypedHookSidecar.push({
      field: `adapters.${id}.path`,
      reason:
        pathError ?? "path must be a normalized source object property path",
    });
  }
  if (!TYPED_HOOK_SIDECAR_FIELDS.includes(entry.sourceField)) {
    errors.invalidTypedHookSidecar.push({
      field: `adapters.${id}.sourceField`,
      reason: `sourceField must be one of ${TYPED_HOOK_SIDECAR_FIELDS.join(", ")}`,
    });
  }
  return true;
}

async function validateTypedHookSidecar({
  irRoot,
  sourceRecords,
  refsById,
  manifestHooks,
  errors,
  enabled,
}) {
  const summary = {
    validated: Boolean(enabled),
    file: TYPED_HOOK_SIDECAR,
    hooks: 0,
    eligibleHooks: 0,
    unsupportedHooks: 0,
    sidecarSha256: null,
    bytes: 0,
  };
  if (!enabled) return summary;

  const sidecarPath = join(irRoot, TYPED_HOOK_SIDECAR);
  let text;
  try {
    const info = await lstat(sidecarPath);
    if (!info.isFile() || info.isSymbolicLink()) {
      throw new Error("typed hook sidecar is a symlink or special entry");
    }
    const bytes = await safeReadRegularFile(sidecarPath, "typed hook sidecar", {
      root: irRoot,
      encoding: null,
    });
    summary.sidecarSha256 = sha256(bytes);
    summary.bytes = bytes.length;
    try {
      text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    } catch (error) {
      errors.invalidTypedHookSidecar.push({
        file: TYPED_HOOK_SIDECAR,
        reason: "typed hook sidecar is not valid UTF-8",
        message: error.message,
      });
      return summary;
    }
  } catch (error) {
    if (error?.code === "ENOENT") {
      errors.missingTypedHookSidecar.push({
        file: TYPED_HOOK_SIDECAR,
        reason: "typed hook sidecar is missing",
      });
    } else {
      errors.invalidTypedHookSidecar.push({
        file: TYPED_HOOK_SIDECAR,
        reason: "typed hook sidecar cannot be read",
        message: error.message,
      });
    }
    return summary;
  }
  if (summary.bytes > TYPED_HOOK_CATALOG_MAX_BYTES) {
    errors.invalidTypedHookSidecar.push({
      file: TYPED_HOOK_SIDECAR,
      reason: `sidecar exceeds the ${TYPED_HOOK_CATALOG_MAX_BYTES}-byte UTF-8 limit`,
    });
  }

  let sidecar;
  try {
    sidecar = JSON.parse(text);
  } catch (error) {
    errors.invalidTypedHookSidecar.push({
      file: TYPED_HOOK_SIDECAR,
      reason: "sidecar is not valid JSON",
      message: error.message,
    });
    return summary;
  }
  // The native reader deserializes integer fields into Rust `u64`/`i64`, so
  // JSON number spellings such as `1e0` and `1.0` must not pass the Node audit
  // as integer 1. The compiler always writes compact JSON plus one newline;
  // requiring that exact re-serialization also rejects duplicate keys and
  // other non-canonical lexemes before this sidecar can be admitted to the
  // cross-language contract.
  if (text !== `${JSON.stringify(sidecar)}\n`) {
    errors.invalidTypedHookSidecar.push({
      file: TYPED_HOOK_SIDECAR,
      reason:
        "sidecar must use the compiler's canonical compact JSON encoding",
    });
  }
  if (!isRecord(sidecar)) {
    errors.invalidTypedHookSidecar.push({
      file: TYPED_HOOK_SIDECAR,
      reason: "sidecar root must be an object",
    });
    return summary;
  }
  rejectUnknownFields(
    errors,
    "typed hook sidecar",
    sidecar,
    ["version", "kind", "contracts", "hooks", "adapters"],
    "invalidTypedHookSidecar",
  );
  if (sidecar.version !== TYPED_HOOK_SIDECAR_VERSION) {
    errors.invalidTypedHookSidecar.push({
      field: "version",
      reason: "sidecar version must be 1",
      value: sidecar.version ?? null,
    });
  }
  if (sidecar.kind !== TYPED_HOOK_SIDECAR_KIND) {
    errors.invalidTypedHookSidecar.push({
      field: "kind",
      reason: "sidecar kind is unsupported",
      value: sidecar.kind ?? null,
    });
  }
  const contracts = isRecord(sidecar.contracts) ? sidecar.contracts : null;
  if (!contracts) {
    errors.invalidTypedHookSidecar.push({
      field: "contracts",
      reason: "sidecar contracts must be an object",
    });
  } else {
    rejectUnknownFields(
      errors,
      "typed hook sidecar.contracts",
      contracts,
      [...TYPED_HOOK_SIDECAR_FIELDS],
      "invalidTypedHookSidecar",
    );
    for (const field of TYPED_HOOK_SIDECAR_FIELDS) {
      if (isRecord(contracts[field])) {
        rejectUnknownFields(
          errors,
          `typed hook sidecar.contracts.${field}`,
          contracts[field],
          ["irVersion", "params", "resultType"],
          "invalidTypedHookSidecar",
        );
      }
    }
    if (canonicalJson(contracts) !== canonicalJson(typedHookContracts())) {
      errors.invalidTypedHookSidecar.push({
        field: "contracts",
        reason: "sidecar contracts do not match the compiler contract",
      });
    }
  }
  const actualHooks = isRecord(sidecar.hooks) ? sidecar.hooks : null;
  if (!actualHooks) {
    errors.invalidTypedHookSidecar.push({
      field: "hooks",
      reason: "sidecar hooks must be an object",
    });
    return summary;
  }
  const actualAdapters = isRecord(sidecar.adapters) ? sidecar.adapters : {};
  if (sidecar.adapters != null && !isRecord(sidecar.adapters)) {
    errors.invalidTypedHookSidecar.push({
      field: "adapters",
      reason: "sidecar adapters must be an object",
    });
    return summary;
  }
  const actualEntries = Object.entries(actualHooks);
  const actualAdapterEntries = Object.entries(actualAdapters);
  summary.hooks = actualEntries.length;
  summary.adapters = actualAdapterEntries.length;
  if (summary.hooks + summary.adapters > TYPED_HOOK_CATALOG_MAX_HOOKS) {
    errors.invalidTypedHookSidecar.push({
      field: "hooks",
      reason: `sidecar exceeds the ${TYPED_HOOK_CATALOG_MAX_HOOKS}-hook limit`,
    });
  }
  for (const [id, entry] of actualEntries) {
    validateTypedHookEntryShape(id, entry, errors);
  }
  for (const [id, entry] of actualAdapterEntries) {
    validateTypedHookAdapterShape(id, entry, errors);
    if (Object.hasOwn(actualHooks, id)) {
      errors.invalidTypedHookSidecar.push({
        id,
        reason: "hook cannot be both a typed descriptor and an adapter binding",
      });
    }
  }

  const adapterCatalog = await loadNativeHookAdapters();
  const sidecarIrFields = new Set(
    TYPED_HOOK_SIDECAR_FIELDS.map((field) => SUPPORTED_HOOK_FIELDS[field]),
  );
  let unsupportedHooks = 0;
  for (const [id, ref] of refsById) {
    if (!sidecarIrFields.has(ref.field)) continue;
    const typed = Object.hasOwn(actualHooks, id) ? actualHooks[id] : undefined;
    const adapter = Object.hasOwn(actualAdapters, id)
      ? actualAdapters[id]
      : undefined;
    if (!isRecord(typed) && !isRecord(adapter)) {
      unsupportedHooks += 1;
      continue;
    }
    const entry = isRecord(typed) ? typed : adapter;
    const sourceField = IR_TO_SOURCE[ref.field] ?? null;
    if (!TYPED_HOOK_SIDECAR_FIELDS.includes(sourceField)) continue;
    const source = sourceRelForIr(ref.ir);
    const sourceRecord = sourceRecords.get(source);
    const sourceFunction = findSourceFunction(
      sourceRecord,
      sourceField,
      entry.path,
      entry.functionBodySha256,
    );
    if (!sourceFunction) {
      errors.typedHookMismatches.push({
        id,
        reason: "sidecar path does not resolve to a source function",
        path: entry.path ?? null,
      });
      continue;
    }
    if (entry.sourceField !== sourceField) {
      errors.typedHookMismatches.push({
        id,
        reason: "sidecar sourceField does not match the IR hook field",
        expected: sourceField,
        actual: entry.sourceField ?? null,
      });
    }
    if (entry.functionBodySha256 !== sourceFunction.sha256) {
      errors.typedHookMismatches.push({
        id,
        reason: "sidecar function body SHA-256 does not match source",
        expected: sourceFunction.sha256,
        actual: entry.functionBodySha256 ?? null,
      });
    }
    const body = sourceFunction.source ?? functionSource(sourceFunction.identity);
    if (isRecord(typed) && body) {
      const recomputed = tryCompileTypedHook({
        body,
        sourceField,
        moduleSource: "",
      });
      if (
        recomputed &&
        canonicalJson(recomputed) !== canonicalJson(typed.descriptor)
      ) {
        errors.typedHookMismatches.push({
          id,
          reason:
            "sidecar provenance or descriptor does not match the compiler identity binding",
          expected: recomputed,
          actual: typed.descriptor,
        });
      }
    }
    if (
      isRecord(adapter) &&
      !isRegisteredNativeAdapter(
        adapter.functionBodySha256,
        sourceField,
        adapterCatalog,
      )
    ) {
      errors.typedHookMismatches.push({
        id,
        reason: "sidecar adapter is not a registered native adapter",
        functionBodySha256: adapter.functionBodySha256 ?? null,
      });
    }
  }
  summary.eligibleHooks = summary.hooks;
  summary.unsupportedHooks = unsupportedHooks;

  for (const id of Object.keys(actualHooks)) {
    if (!refsById.has(id)) {
      errors.orphanTypedHooks.push({
        id,
        reason: "sidecar hook is not an eligible audited typed binding",
      });
    }
  }
  for (const id of Object.keys(actualAdapters)) {
    if (!refsById.has(id)) {
      errors.orphanTypedHooks.push({
        id,
        reason: "sidecar adapter is not an eligible audited hook binding",
      });
    }
  }
  return summary;
}

// Module names are written by the compiler at the root of source-modules.
// Keep this check independent from path.resolve/join: a malformed manifest
// must never be able to make the audit read outside the generated tree.
function isSafeModuleFile(value) {
  return (
    typeof value === "string" &&
    /^[^/\\\0]+\.js$/.test(value) &&
    value !== ".js" &&
    value !== "..js"
  );
}

function isSafeRelativeSource(value) {
  if (typeof value !== "string" || value.length === 0) return false;
  // Manifest paths are already a POSIX publication format. Reject a raw
  // backslash before any normalization; accepting `..\\escape.js` here would
  // make the metadata disagree with the walker's literal filename boundary.
  if (value.includes("\\")) return false;
  const normalized = value;
  if (normalized.startsWith("/") || normalized.includes("\0")) return false;
  return !normalized
    .split("/")
    .some((segment) => segment === ".." || segment === "");
}

function isSafeSourceHookPath(value) {
  return (
    typeof value === "string" &&
    /^root(?:\.[A-Za-z_$][\w$]*|\[\d+\])*$/.test(value)
  );
}

function sourceRelForIr(ir) {
  if (isDerivedVersionIr(ir)) {
    return derivedVersionIrBaseSource(ir);
  }
  return String(ir).replace(/\.json$/, ".js");
}

function findSourceFunction(
  sourceRecord,
  sourceField,
  path,
  functionBodySha256,
) {
  if (!sourceRecord || !SOURCE_FIELDS.includes(sourceField)) return undefined;
  const pools = [
    sourceRecord.functions?.[sourceField] ?? [],
    sourceRecord.mergedFunctions?.[sourceField] ?? [],
  ];
  if (typeof path === "string" && path) {
    for (const pool of pools) {
      const byPath = pool.find((candidate) => candidate.path === path);
      if (byPath) return byPath;
    }
    return undefined;
  }
  if (typeof functionBodySha256 === "string" && functionBodySha256) {
    for (const pool of pools) {
      const byHash = pool.find(
        (candidate) => candidate.sha256 === functionBodySha256,
      );
      if (byHash) return byHash;
    }
  }
  return undefined;
}

function irRecordsForSource(irRecords, source) {
  return [...irRecords.entries()]
    .filter(([ir]) => sourceRelForIr(ir) === source)
    .sort(([left], [right]) => {
      const leftDerived = isDerivedVersionIr(left);
      const rightDerived = isDerivedVersionIr(right);
      if (leftDerived !== rightDerived) return leftDerived ? 1 : -1;
      return comparePath(left, right);
    });
}

function uniqueFamilyRefs(familyRecords) {
  const byId = new Map();
  for (const [, irRecord] of familyRecords) {
    for (const ref of irRecord.refs ?? []) {
      if (!byId.has(ref.id)) byId.set(ref.id, ref);
    }
  }
  return [...byId.values()];
}

function familySourceInstances(record, sourceField) {
  const seen = new Set();
  const out = [];
  for (const pool of [
    record.functions?.[sourceField] ?? [],
    record.mergedFunctions?.[sourceField] ?? [],
  ]) {
    for (const item of compilerFunctionInstances(pool, {
      includeNative: false,
    })) {
      const key = `${item.path}\0${item.sha256}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(item);
    }
  }
  return out;
}

function collectMergedVersionFunctions(baseSpec, versions, errors, sourceRel) {
  const mergedFunctions = emptyFieldMap();
  if (!isRecord(versions) || !isObject(baseSpec) || Array.isArray(baseSpec)) {
    return mergedFunctions;
  }
  let current = baseSpec;
  for (const version of versionDiffKeys(versions)) {
    current = applySpecDiff(current, versions[version]);
    for (const item of collectFunctions(current)) {
      const functionRecord = sourceFunctionRecord(item);
      functionRecord.mergeThrough = version;
      addFunctionRecord(mergedFunctions, functionRecord);
      if (!SOURCE_FIELDS.includes(item.field)) {
        errors.unknownFunctionFields.push({
          source: sourceRel,
          path: item.path,
          field: item.field,
          version,
        });
      } else if (!functionRecord.extractable) {
        errors.unextractableFunctions.push({
          source: sourceRel,
          path: item.path,
          field: item.field,
          version,
        });
      }
    }
  }
  return sortFunctionRecords(mergedFunctions);
}

function sortedStrings(values) {
  return [...values].sort(comparePath);
}

function sameStringSet(left, right) {
  const leftValues = sortedStrings(left);
  const rightValues = sortedStrings(right);
  return (
    leftValues.length === rightValues.length &&
    leftValues.every((value, index) => value === rightValues[index])
  );
}

function rejectUnknownFields(
  errors,
  field,
  value,
  allowed,
  errorField = "invalidHookModuleManifest",
) {
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) {
      errors[errorField].push({
        field: `${field}.${key}`,
        reason: "manifest contains an unknown field",
      });
    }
  }
}

function collectFunctions(
  value,
  path = "root",
  parent = null,
  key = "",
  output = [],
  ancestors = new WeakSet(),
) {
  if (typeof value === "function") {
    const nativeRewrite =
      parent && isFilepathsHelper(parent) && NATIVE_HELPER_FIELDS.has(key);
    output.push({
      path,
      field: key,
      fn: value,
      nativeRewrite: Boolean(nativeRewrite),
    });
    return output;
  }
  if (!isObject(value) || ancestors.has(value)) return output;
  ancestors.add(value);
  if (Array.isArray(value)) {
    value.forEach((item, index) =>
      collectFunctions(
        item,
        `${path}[${index}]`,
        value,
        String(index),
        output,
        ancestors,
      ),
    );
  } else {
    Object.keys(value)
      .sort(comparePath)
      .forEach((name) =>
        collectFunctions(
          value[name],
          `${path}.${name}`,
          value,
          name,
          output,
          ancestors,
        ),
      );
  }
  ancestors.delete(value);
  return output;
}

function namesOf(value) {
  if (value == null || value === "") return [];
  if (Array.isArray(value)) return value.flatMap(namesOf);
  if (isObject(value)) return "name" in value ? namesOf(value.name) : [];
  return [String(value)];
}

function sourceFunctionRecord(item) {
  const source = functionSource(item.fn);
  // convertArg() first lets copySuggestionMetadata() inspect a generator and
  // then converts that same generator. A function-valued getQueryTerm on an
  // argument-level generator is consequently extracted twice. Keep this
  // explicit compiler behavior in the source-instance model; counting only
  // object identities would make a dropped second extraction invisible.
  const compilerExtractionCount =
    item.field === "getQueryTerm" &&
    /\.args(?:\[|\.)/.test(item.path) &&
    /\.generators?(?:\[|\.)/.test(item.path)
      ? 2
      : 1;
  return {
    identity: item.fn,
    field: item.field,
    path: item.path,
    sha256: source ? sha256(source) : null,
    source: source || null,
    extractable: Boolean(source),
    nativeRewrite: item.nativeRewrite,
    compilerExtractionCount,
  };
}

function sourceFunctionInstances(records, { includeNative = true } = {}) {
  return records.filter((record) => includeNative || !record.nativeRewrite);
}

function compilerFunctionInstances(records, { includeNative = true } = {}) {
  return sourceFunctionInstances(records, { includeNative }).flatMap((record) =>
    Array.from({ length: record.compilerExtractionCount ?? 1 }, () => record),
  );
}

function countCompilerInstances(fields, { includeNative = true } = {}) {
  return Object.fromEntries(
    SOURCE_FIELDS.map((field) => [
      field,
      compilerFunctionInstances(fields[field] ?? [], { includeNative }).length,
    ]),
  );
}

function countNativeRewrites(fields) {
  return Object.fromEntries(
    SOURCE_FIELDS.map((field) => [
      field,
      (fields[field] ?? []).filter((record) => record.nativeRewrite).length,
    ]),
  );
}

function addFunctionRecord(target, record) {
  if (!target[record.field]) target[record.field] = [];
  target[record.field].push(record);
}

function sortFunctionRecords(fields) {
  for (const records of Object.values(fields)) {
    records.sort(
      (left, right) =>
        comparePath(left.path, right.path) ||
        comparePath(left.sha256 ?? "", right.sha256 ?? ""),
    );
  }
  return fields;
}

function countRecords(fields) {
  return Object.fromEntries(
    SOURCE_FIELDS.map((field) => [
      field,
      sourceFunctionInstances(fields[field] ?? []).length,
    ]),
  );
}

function uniqueRecordHashes(fields, { includeNative = true } = {}) {
  return Object.fromEntries(
    SOURCE_FIELDS.map((field) => [
      field,
      [
        ...new Set(
          sourceFunctionInstances(fields[field] ?? [], { includeNative })
            .map((record) => record.sha256)
            .filter(Boolean),
        ),
      ].sort(comparePath),
    ]),
  );
}

function collectIrHooks(
  value,
  path = "root",
  output = [],
  unknown = [],
  ancestors = new WeakSet(),
) {
  if (!isObject(value) || ancestors.has(value)) return;
  ancestors.add(value);
  if (Array.isArray(value)) {
    value.forEach((item, index) =>
      collectIrHooks(item, `${path}[${index}]`, output, unknown, ancestors),
    );
  } else {
    Object.keys(value)
      .sort(comparePath)
      .forEach((field) => {
        const child = value[field];
        if (/^js[A-Z]/.test(field)) {
          if (SUPPORTED_IR_HOOK_FIELDS.includes(field)) {
            if (typeof child === "string" && child) {
              output.push({ field, id: child, path: `${path}.${field}` });
            } else {
              unknown.push({
                field,
                path: `${path}.${field}`,
                reason: "hook reference is not a non-empty string",
              });
            }
          } else {
            unknown.push({
              field,
              path: `${path}.${field}`,
              reason: "unknown IR hook field",
            });
          }
        }
        collectIrHooks(child, `${path}.${field}`, output, unknown, ancestors);
      });
  }
  ancestors.delete(value);
}

function bodyFromHookFile(source) {
  if (!source.startsWith("export default ")) return null;
  return source
    .slice("export default ".length)
    .replace(/;\n$/, "")
    .replace(/;$/, "");
}

function sortHookRefs(refs) {
  refs.sort(
    (left, right) =>
      comparePath(left.field, right.field) ||
      comparePath(left.id, right.id) ||
      comparePath(left.path, right.path),
  );
  return refs;
}

async function readManifestHooksForMapping(irRoot) {
  try {
    const text = await safeReadRegularFile(
      join(irRoot, HOOK_MODULE_MANIFEST),
      "closure-preserving hook module manifest",
      { root: irRoot },
    );
    const manifest = JSON.parse(text);
    return isRecord(manifest) && isRecord(manifest.hooks)
      ? manifest.hooks
      : null;
  } catch {
    // validateHookModules() reports the precise parse/read error. The source
    // mapping simply falls back to the pre-manifest body check in this case;
    // that fallback is never used to publish a module manifest.
    return null;
  }
}

function fieldCountsFromRefs(refs) {
  return Object.fromEntries(
    SUPPORTED_IR_HOOK_FIELDS.map((field) => [
      field,
      refs.filter((ref) => ref.field === field).length,
    ]),
  );
}

function uniqueIdCountsFromRefs(refs) {
  return Object.fromEntries(
    SUPPORTED_IR_HOOK_FIELDS.map((field) => [
      field,
      new Set(refs.filter((ref) => ref.field === field).map((ref) => ref.id))
        .size,
    ]),
  );
}

async function validateSourceIndex(sourceRoot, errors) {
  const indexPath = join(sourceRoot, "index.json");
  let text;
  try {
    text = await safeReadRegularFile(indexPath, "source index", {
      root: sourceRoot,
    });
  } catch (error) {
    if (error.code === "ENOENT") return null;
    errors.sourceReadErrors.push({
      sourceIndex: indexPath,
      message: error.message,
    });
    return null;
  }
  let index;
  try {
    index = JSON.parse(text);
  } catch (error) {
    errors.sourceReadErrors.push({
      sourceIndex: indexPath,
      message: `cannot parse source index: ${error.message}`,
    });
    return null;
  }
  if (!index || typeof index !== "object" || Array.isArray(index)) {
    errors.sourceReadErrors.push({
      sourceIndex: indexPath,
      message: "source index must be a JSON object",
    });
    return null;
  }
  const completions = index.completions;
  if (
    !Array.isArray(completions) ||
    completions.length === 0 ||
    completions.some(
      (item) => typeof item !== "string" || item.trim().length === 0,
    )
  ) {
    errors.sourceReadErrors.push({
      sourceIndex: indexPath,
      message: "source index completions must be a non-empty array of strings",
    });
  }
  const diffVersionedCompletions = index.diffVersionedCompletions;
  if (
    "diffVersionedCompletions" in index &&
    (!Array.isArray(diffVersionedCompletions) ||
      diffVersionedCompletions.some(
        (item) => typeof item !== "string" || item.trim().length === 0,
      ))
  ) {
    errors.sourceReadErrors.push({
      sourceIndex: indexPath,
      message:
        "source index diffVersionedCompletions must be an array of strings",
    });
  }
  return {
    completions: Array.isArray(completions) ? completions : [],
    diffVersionedCompletions: Array.isArray(diffVersionedCompletions)
      ? diffVersionedCompletions
      : [],
  };
}

function uniqueHashesByIrField(refs, hookFiles) {
  return Object.fromEntries(
    SUPPORTED_IR_HOOK_FIELDS.map((field) => [
      field,
      [
        ...new Set(
          refs
            .filter((ref) => ref.field === field)
            .map((ref) => hookFiles.get(ref.id)?.body)
            .filter(Boolean)
            .map(sha256),
        ),
      ].sort(comparePath),
    ]),
  );
}

function countIrKeys(records, keys) {
  const counts = Object.fromEntries(keys.map((key) => [key, 0]));
  function visit(value, ancestors = new WeakSet()) {
    if (!isObject(value) || ancestors.has(value)) return;
    ancestors.add(value);
    if (Array.isArray(value)) {
      value.forEach((item) => visit(item, ancestors));
    } else {
      for (const [key, child] of Object.entries(value)) {
        if (Object.hasOwn(counts, key)) counts[key] += 1;
        visit(child, ancestors);
      }
    }
    ancestors.delete(value);
  }
  for (const record of records.values()) visit(record.value);
  return counts;
}

function addHookModuleMismatch(errors, entry) {
  errors.hookModuleMismatches.push(entry);
}

async function leftoverPathExists(path) {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function rejectLeftoverRuntimeJs(irRoot, errors) {
  if (await leftoverPathExists(join(irRoot, HOOK_MODULE_MANIFEST))) {
    errors.leftoverHookModuleManifest.push({
      file: HOOK_MODULE_MANIFEST,
      reason: "runtime JS hook manifest must not be published",
    });
  }
  if (await leftoverPathExists(join(irRoot, HOOK_MODULES_DIR))) {
    errors.leftoverHookModules.push({
      directory: HOOK_MODULES_DIR,
      reason: "runtime JS source-modules directory must not be published",
    });
  }
  if (await leftoverPathExists(join(irRoot, "hooks"))) {
    errors.leftoverHookFiles.push({
      directory: "hooks",
      reason: "runtime JS hooks directory must not be published",
    });
  }
}

async function readSidecarMapping(irRoot) {
  try {
    const text = await safeReadRegularFile(
      join(irRoot, TYPED_HOOK_SIDECAR),
      "typed hook sidecar",
      { root: irRoot },
    );
    const sidecar = JSON.parse(text);
    if (!isRecord(sidecar)) return null;
    const mapping = {};
    for (const [id, entry] of Object.entries(sidecar.hooks ?? {})) {
      if (isRecord(entry)) mapping[id] = entry;
    }
    for (const [id, entry] of Object.entries(sidecar.adapters ?? {})) {
      if (isRecord(entry) && !Object.hasOwn(mapping, id)) mapping[id] = entry;
    }
    return mapping;
  } catch {
    return null;
  }
}

async function validateHookModules({ irRoot, errors, enabled }) {
  const summary = {
    validated: Boolean(enabled),
    leftoverRuntimeJs: false,
    leftoverHookFiles: errors.leftoverHookFiles.length,
    leftoverHookModules: errors.leftoverHookModules.length,
    leftoverHookModuleManifest: errors.leftoverHookModuleManifest.length,
  };
  if (!enabled) return summary;
  summary.leftoverRuntimeJs =
    summary.leftoverHookFiles +
      summary.leftoverHookModules +
      summary.leftoverHookModuleManifest >
    0;
  return summary;
}

async function auditSpecsHooksUnlocked({
  sourceRoot = sourceDir,
  irRoot = irDir,
  hooksRoot = join(irRoot, "hooks"),
  validateHookModules: validateHookModulesEnabled = true,
  validateTypedHooks: validateTypedHooksEnabled = true,
  pair: pairMode = "strict",
} = {}) {
  const errors = {
    pair: [],
    sourceReadErrors: [],
    sourceNonSpecs: [],
    unexpectedSkippedSpecs: [],
    missingIrSpecs: [],
    orphanIrSpecs: [],
    unknownFunctionFields: [],
    unextractableFunctions: [],
    unknownIrHookFields: [],
    invalidHookRefs: [],
    missingHookFiles: [],
    malformedHookFiles: [],
    hookAnalysisErrors: [],
    orphanHookFiles: [],
    sourceHookMismatches: [],
    missingHookModuleManifest: [],
    invalidHookModuleManifest: [],
    missingHookModules: [],
    orphanHookModules: [],
    hookModuleMismatches: [],
    leftoverHookFiles: [],
    leftoverHookModules: [],
    leftoverHookModuleManifest: [],
    missingTypedHookSidecar: [],
    invalidTypedHookSidecar: [],
    missingTypedHooks: [],
    orphanTypedHooks: [],
    typedHookMismatches: [],
  };

  if (!["strict", "skip", "ir-only"].includes(pairMode)) {
    throw new Error(`unsupported audit pair mode: ${pairMode}`);
  }
  if (
    (validateHookModulesEnabled && !validateTypedHooksEnabled) ||
    (!validateHookModulesEnabled && validateTypedHooksEnabled)
  ) {
    throw new Error(
      "validateHookModules and validateTypedHooks must be disabled together for the compiler pre-manifest audit",
    );
  }

  if (pairMode !== "skip") {
    try {
      await verifyPair({ sourceRoot, irRoot, irOnly: pairMode === "ir-only" });
    } catch (error) {
      errors.pair.push({
        sourceRoot,
        irRoot,
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }
  await rejectLeftoverRuntimeJs(irRoot, errors);

  async function readTree(root, extension, options, label) {
    try {
      return await walkFiles(root, extension, options);
    } catch (error) {
      errors.sourceReadErrors.push({
        [label]: root,
        message: error.message,
      });
      return [];
    }
  }

  const sourceFiles = await readTree(
    sourceRoot,
    ".js",
    { skipRootIndex: true },
    "sourceRoot",
  );
  const irFiles = (
    await readTree(irRoot, ".json", { skipRootIndex: true }, "irRoot")
  ).filter(
    (file) =>
      file !== HOOK_MODULE_MANIFEST &&
      file !== PAIR_MARKER_NAME &&
      file !== TYPED_HOOK_SIDECAR,
  );
  const hookFilesOnDisk = [];
  const sourceIndex = await validateSourceIndex(sourceRoot, errors);

  if (!sourceFiles.length) {
    errors.unexpectedSkippedSpecs.push({
      sourceRoot,
      reason: "source tree contains no JavaScript specs",
    });
  }
  if (!irFiles.length) {
    errors.missingIrSpecs.push({
      irRoot,
      reason: "IR tree contains no JSON specs",
    });
  }

  // Keep the identity-checked bytes for the whole audit generation. Module
  // evaluation must consume these bytes, not reopen a source pathname that a
  // concurrent publisher could have replaced after its provenance record.
  const auditedSourceBytes = new Map();
  const sourceRecords = new Map();
  for (const file of sourceFiles) {
    const rel = normalizeRelativePath(file);
    const full = join(sourceRoot, file);
    const sourceKey = resolve(full);
    let source = auditedSourceBytes.get(sourceKey);
    if (source === undefined) {
      source = await safeReadRegularFile(full, `source ${rel}`, {
        root: sourceRoot,
      }).catch((error) => {
        errors.sourceReadErrors.push({ source: rel, message: error.message });
        return null;
      });
      if (source !== null) auditedSourceBytes.set(sourceKey, source);
    }
    if (source == null) continue;
    const record = {
      source: rel,
      sourceSha256: sha256(source),
      allowlisted: KNOWN_NON_SPEC_FILES.has(rel),
      functions: emptyFieldMap(),
      mergedFunctions: emptyFieldMap(),
      moduleError: null,
      isSpec: false,
      names: [],
    };
    if (!record.allowlisted) {
      try {
        const approvedSource = resolve(full);
        if (!isInside(sourceRoot, approvedSource)) {
          throw new Error("source module escapes its approved root");
        }
        // Evaluate the exact bytes returned above. Never import the
        // replaceable pathname after auditing it: that would re-open a
        // potentially swapped source file and invalidate its SHA/identity.
        const mod = await importAuditedSourceModule(
          source,
          approvedSource,
          sourceRoot,
          `source ${rel}`,
          auditedSourceBytes,
        );
        const value = mod.default ?? mod;
        record.names = namesOf(value);
        record.isSpec =
          isObject(value) && !Array.isArray(value) && record.names.length > 0;
        if (!record.isSpec) errors.sourceNonSpecs.push({ source: rel });
        for (const item of collectFunctions(value)) {
          const functionRecord = sourceFunctionRecord(item);
          addFunctionRecord(record.functions, functionRecord);
          if (!SOURCE_FIELDS.includes(item.field)) {
            errors.unknownFunctionFields.push({
              source: rel,
              path: item.path,
              field: item.field,
            });
          } else if (!functionRecord.extractable) {
            errors.unextractableFunctions.push({
              source: rel,
              path: item.path,
              field: item.field,
            });
          }
        }
        sortFunctionRecords(record.functions);
        record.mergedFunctions = collectMergedVersionFunctions(
          value,
          isRecord(mod.versions) ? mod.versions : null,
          errors,
          rel,
        );
      } catch (error) {
        record.moduleError = error.message;
        errors.sourceReadErrors.push({ source: rel, message: error.message });
      }
    }
    sourceRecords.set(rel, record);
  }
  if (sourceIndex) {
    const sourcePaths = new Set(sourceFiles.map(normalizeRelativePath));
    const declaredNames = new Set(
      [...sourceRecords.values()].flatMap((record) => record.names),
    );
    const indexedNames = new Set([
      ...sourceIndex.completions,
      ...sourceIndex.diffVersionedCompletions,
    ]);
    for (const name of indexedNames) {
      if (
        sourcePaths.has(`${name}.js`) ||
        sourcePaths.has(`${name}/index.js`) ||
        declaredNames.has(name)
      ) {
        continue;
      }
      errors.sourceReadErrors.push({
        sourceIndex: join(sourceRoot, "index.json"),
        name,
        message: `indexed completion ${name} has no source module or declared alias`,
      });
    }
  }
  if (
    sourceFiles.length > 0 &&
    ![...sourceRecords.values()].some((record) => record.isSpec)
  ) {
    errors.unexpectedSkippedSpecs.push({
      sourceRoot,
      reason: "source tree contains no command specs",
    });
  }

  const irRecords = new Map();
  const allRefs = [];
  for (const file of irFiles) {
    const rel = normalizeRelativePath(file);
    const full = join(irRoot, file);
    const text = await safeReadRegularFile(full, `IR ${rel}`, {
      root: irRoot,
    }).catch((error) => {
      errors.sourceReadErrors.push({ ir: rel, message: error.message });
      return null;
    });
    if (text == null) continue;
    let value;
    try {
      value = JSON.parse(text);
    } catch (error) {
      errors.sourceReadErrors.push({ ir: rel, message: error.message });
      continue;
    }
    const refs = [];
    const unknown = [];
    collectIrHooks(value, "root", refs, unknown);
    errors.unknownIrHookFields.push(
      ...unknown.map((item) => ({ ir: rel, ...item })),
    );
    sortHookRefs(refs);
    refs.forEach((ref) => allRefs.push({ ...ref, ir: rel }));
    irRecords.set(rel, {
      ir: rel,
      irSha256: sha256(text),
      value,
      refs,
    });
  }

  const hookFiles = new Map();
  for (const file of hookFilesOnDisk) {
    const rel = normalizeRelativePath(file);
    const text = await safeReadRegularFile(
      join(hooksRoot, file),
      `hook ${rel}`,
      { root: hooksRoot },
    ).catch((error) => {
      errors.malformedHookFiles.push({ hook: rel, message: error.message });
      return null;
    });
    if (text == null) continue;
    const body = bodyFromHookFile(text);
    if (body == null) {
      errors.malformedHookFiles.push({
        hook: rel,
        reason: "hook file must start with export default",
      });
      continue;
    }
    try {
      // The desktop host strips `export default` and evaluates `(body)`.
      // Existence alone is not enough: a method shorthand would fail every
      // completion request while appearing to be a healthy hook asset.
      new Script(`(${body})`, { filename: rel });
    } catch (error) {
      errors.malformedHookFiles.push({
        hook: rel,
        reason: "hook body is not a standalone JavaScript expression",
        message: error.message,
      });
      continue;
    }
    hookFiles.set(rel, { sha256: sha256(text), body });
  }

  const refsById = new Map();
  const idsByFilename = new Map();
  const locationsById = new Map();
  for (const ref of allRefs) {
    const filename = hookFileName(ref.id);
    const existing = refsById.get(ref.id);
    if (existing && existing.field !== ref.field) {
      errors.invalidHookRefs.push({
        id: ref.id,
        reason: `referenced as both ${existing.field} and ${ref.field}`,
      });
    }
    const previousId = idsByFilename.get(filename);
    if (previousId && previousId !== ref.id) {
      errors.invalidHookRefs.push({
        id: ref.id,
        file: filename,
        reason: `hook filename collides with ${previousId}`,
      });
    }
    idsByFilename.set(filename, ref.id);
    const locations = locationsById.get(ref.id) ?? new Set();
    locations.add(ref.ir);
    locationsById.set(ref.id, locations);
    const expectedSpecId = ref.ir.replace(/\.json$/, "");
    const separator = ref.id.indexOf("#");
    const hookFamily = ref.id.slice(0, separator);
    const irFamily = derivedVersionIrFamily(ref.ir);
    if (
      separator <= 0 ||
      (hookFamily !== expectedSpecId &&
        !(isDerivedVersionIr(ref.ir) && hookFamily === irFamily))
    ) {
      errors.invalidHookRefs.push({
        id: ref.id,
        ir: ref.ir,
        reason: `hook id does not belong to ${expectedSpecId}`,
      });
    }
    refsById.set(ref.id, ref);
  }

  for (const [id, locations] of locationsById) {
    if (locations.size <= 1) continue;
    const families = new Set(
      [...locations].map((ir) => derivedVersionIrFamily(ir)),
    );
    const separator = id.indexOf("#");
    const hookFamily = separator > 0 ? id.slice(0, separator) : "";
    if (families.size === 1 && families.has(hookFamily)) continue;
    errors.invalidHookRefs.push({
      id,
      reason: `hook id is referenced by multiple IR specs: ${[...locations]
        .sort(comparePath)
        .join(", ")}`,
    });
  }

  const expectedHookFiles = new Set(
    [...refsById.keys()].map((id) => hookFileName(id)),
  );
  for (const file of hookFiles.keys()) {
    if (!expectedHookFiles.has(file)) errors.orphanHookFiles.push(file);
  }

  const sidecarMapping = validateTypedHooksEnabled
    ? await readSidecarMapping(irRoot)
    : null;
  const fallbackBodies = new Map();
  for (const [source, record] of sourceRecords) {
    if (record.allowlisted || !record.isSpec) continue;
    const familyRefs = uniqueFamilyRefs(irRecordsForSource(irRecords, source));
    for (const sourceField of SOURCE_FIELDS) {
      const irField = SOURCE_TO_IR[sourceField];
      const candidates = [
        ...new Map(
          familyRefs
            .filter((ref) => ref.field === irField)
            .map((ref) => [ref.id, ref]),
        ).values(),
      ];
      const baseInstances = compilerFunctionInstances(
        record.functions[sourceField] ?? [],
        { includeNative: false },
      );
      if (candidates.length !== baseInstances.length) continue;
      candidates.forEach((ref, index) => {
        fallbackBodies.set(ref.id, baseInstances[index]);
      });
    }
  }
  const hookBodiesById = new Map();
  for (const [id, ref] of refsById) {
    const mapping = sidecarMapping?.[id];
    const sourceFunction =
      findSourceFunction(
        sourceRecords.get(sourceRelForIr(ref.ir)),
        IR_TO_SOURCE[ref.field],
        mapping?.path ?? null,
        mapping?.functionBodySha256 ?? null,
      ) ?? fallbackBodies.get(id);
    const body = sourceFunction?.source ?? null;
    hookBodiesById.set(id, {
      body,
      sha256: body ? sha256(`export default ${body};\n`) : null,
      path: mapping?.path ?? sourceFunction?.path ?? null,
      functionBodySha256:
        mapping?.functionBodySha256 ?? (body ? sha256(body) : null),
      module: mapping?.module ?? null,
      moduleSha256: mapping?.moduleSha256 ?? null,
    });
  }
  const typedHooks = await validateTypedHookSidecar({
    irRoot,
    sourceRecords,
    refsById,
    errors,
    enabled: validateTypedHooksEnabled,
  });
  const sourceToIr = [];
  for (const [source, record] of sourceRecords) {
    const ir = source.replace(/\.js$/, ".json");
    const irRecord = irRecords.get(ir);
    const sourceHashes = uniqueRecordHashes(record.functions, {
      includeNative: false,
    });
    if (record.allowlisted) {
      sourceToIr.push({
        source,
        sourceSha256: record.sourceSha256,
        status: "allowlisted-non-spec",
        ir: null,
        functions: countRecords(record.functions),
        compilerExtractionCounts: countCompilerInstances(record.functions, {
          includeNative: false,
        }),
        nativeRewriteCounts: countNativeRewrites(record.functions),
        uniqueFunctionHashes: sourceHashes,
        hooks: emptyIrFieldMap(),
        uniqueHooks: emptyIrFieldMap(),
        hookInstances: emptyFieldMap(),
      });
      continue;
    }
    if (!irRecord) {
      errors.missingIrSpecs.push(source);
      errors.unexpectedSkippedSpecs.push(source);
      sourceToIr.push({
        source,
        sourceSha256: record.sourceSha256,
        status: "missing-ir",
        ir: null,
        functions: countRecords(record.functions),
        compilerExtractionCounts: countCompilerInstances(record.functions, {
          includeNative: false,
        }),
        nativeRewriteCounts: countNativeRewrites(record.functions),
        uniqueFunctionHashes: sourceHashes,
        hooks: emptyIrFieldMap(),
        uniqueHooks: emptyIrFieldMap(),
        hookInstances: emptyFieldMap(),
      });
      continue;
    }
    const familyRecords = irRecordsForSource(irRecords, source);
    const familyRefs = uniqueFamilyRefs(familyRecords);
    const refsByField = emptyIrFieldMap();
    const refsByFieldRecords = emptyIrFieldMap();
    for (const ref of familyRefs) {
      refsByField[ref.field].push(ref.id);
      refsByFieldRecords[ref.field].push(ref);
    }
    for (const values of Object.values(refsByField)) values.sort(comparePath);
    const uniqueHooks = Object.fromEntries(
      SUPPORTED_IR_HOOK_FIELDS.map((field) => [
        field,
        [...new Set(refsByField[field])].sort(comparePath),
      ]),
    );
    const hookInstances = emptyFieldMap();
    for (const sourceField of SOURCE_FIELDS) {
      const irField = SOURCE_TO_IR[sourceField];
      const candidates = [
        ...new Map(
          refsByFieldRecords[irField].map((ref) => [ref.id, ref]),
        ).values(),
      ];
      const sourceInstances = familySourceInstances(record, sourceField);
      const baseInstances = compilerFunctionInstances(
        record.functions[sourceField] ?? [],
        { includeNative: false },
      );
      const hasDerivedFamily = familyRecords.some(([familyIr]) =>
        isDerivedVersionIr(familyIr),
      );
      // Version diffs add hook ids that exist only on derived IR. Those are
      // matched through mergedFunctions / findSourceFunction below; the 1:1
      // base-file count would false-fail as soon as a merged-only id appears.
      if (!hasDerivedFamily && candidates.length !== baseInstances.length) {
        errors.sourceHookMismatches.push({
          source,
          ir,
          field: sourceField,
          irField,
          reason: `compiler hook instances (${baseInstances.length}) do not match IR hook ids (${candidates.length})`,
        });
      }
      if (sidecarMapping) {
        for (const [index, ref] of candidates.entries()) {
          const descriptor = Object.hasOwn(sidecarMapping, ref.id)
            ? sidecarMapping[ref.id]
            : undefined;
          if (!descriptor || !isRecord(descriptor)) {
            // Factory leftovers and unregistered bodies stay off the sidecar.
            // Bind them from the compiler's source-order zip so capture and
            // classify still see a path and body SHA.
            const functionRecord = sourceInstances[index];
            hookInstances[sourceField].push({
              path: functionRecord?.path ?? null,
              sha256: functionRecord?.sha256 ?? null,
              functionBodySha256: functionRecord?.sha256 ?? null,
              sourceField,
              id: ref.id,
              file: hookFileName(ref.id),
            });
            continue;
          }
          const instance = {
            path: descriptor.path ?? null,
            sha256: descriptor.functionBodySha256 ?? null,
            functionBodySha256: descriptor.functionBodySha256 ?? null,
            sourceField,
            id: ref.id,
            file: hookFileName(ref.id),
          };
          if (descriptor.sourceField !== sourceField) {
            errors.sourceHookMismatches.push({
              source,
              ir,
              field: sourceField,
              irField,
              id: ref.id,
              reason: "sidecar sourceField does not match source field",
              expected: sourceField,
              actual: descriptor.sourceField ?? null,
            });
          }
          const sourceFunction = findSourceFunction(
            record,
            sourceField,
            descriptor.path,
            descriptor.functionBodySha256,
          );
          if (!sourceFunction) {
            errors.sourceHookMismatches.push({
              source,
              ir,
              field: sourceField,
              irField,
              id: ref.id,
              path: descriptor.path ?? null,
              reason: "sidecar path does not resolve to a source function",
            });
          } else if (descriptor.functionBodySha256 !== sourceFunction.sha256) {
            errors.sourceHookMismatches.push({
              source,
              ir,
              field: sourceField,
              irField,
              id: ref.id,
              path: descriptor.path,
              reason: "sidecar function body SHA-256 does not match source",
              expected: sourceFunction.sha256,
              actual: descriptor.functionBodySha256 ?? null,
            });
          }
          hookInstances[sourceField].push(instance);
        }
        continue;
      }
      if (hasDerivedFamily) {
        for (const ref of candidates) {
          hookInstances[sourceField].push({
            path: null,
            sha256: null,
            functionBodySha256: null,
            sourceField,
            id: ref.id,
            file: hookFileName(ref.id),
          });
        }
        continue;
      }
      if (candidates.length === sourceInstances.length) {
        for (const [index, functionRecord] of sourceInstances.entries()) {
          const id = candidates[index].id;
          hookInstances[sourceField].push({
            path: functionRecord.path,
            sha256: functionRecord.sha256,
            functionBodySha256: functionRecord.sha256,
            sourceField,
            id,
            file: hookFileName(id),
          });
        }
      } else {
        for (const functionRecord of sourceInstances) {
          hookInstances[sourceField].push({
            path: functionRecord.path,
            sha256: functionRecord.sha256,
            functionBodySha256: functionRecord.sha256,
            sourceField,
            id: null,
            file: null,
          });
        }
      }
    }
    for (const values of Object.values(hookInstances)) {
      values.sort(
        (left, right) =>
          comparePath(left.path, right.path) ||
          comparePath(left.id ?? "", right.id ?? ""),
      );
    }
    sourceToIr.push({
      source,
      sourceSha256: record.sourceSha256,
      status: "compiled",
      ir,
      irSha256: irRecord.irSha256,
      functions: countRecords(record.functions),
      compilerExtractionCounts: countCompilerInstances(record.functions, {
        includeNative: false,
      }),
      nativeRewriteCounts: countNativeRewrites(record.functions),
      uniqueFunctionHashes: sourceHashes,
      hooks: refsByField,
      uniqueHooks,
      hookInstances,
    });
  }

  for (const ir of irRecords.keys()) {
    if (isDerivedVersionIr(ir)) continue;
    const source = ir.replace(/\.json$/, ".js");
    if (!sourceRecords.has(source)) errors.orphanIrSpecs.push(ir);
  }

  const hookModules = await validateHookModules({
    irRoot,
    errors,
    enabled: validateHookModulesEnabled,
  });

  for (const values of Object.values(errors)) {
    if (!Array.isArray(values)) continue;
    values.sort((left, right) =>
      comparePath(JSON.stringify(left), JSON.stringify(right)),
    );
  }
  sourceToIr.sort((left, right) => comparePath(left.source, right.source));

  const sourceFunctionRecords = emptyFieldMap();
  for (const record of sourceRecords.values()) {
    for (const field of SOURCE_FIELDS) {
      sourceFunctionRecords[field].push(...(record.functions[field] ?? []));
    }
  }
  const refs = sortHookRefs(allRefs);
  const riskHooks = [];
  for (const [id, ref] of refsById) {
    const file = hookFileName(id);
    const hook = hookBodiesById.get(id);
    if (!hook?.body) continue;
    const hits = RISK_PATTERNS.filter(([, pattern]) =>
      pattern.test(hook.body),
    ).map(([name]) => name);
    let freeVariables = [];
    try {
      const analysis = analyzeHookBody(hook.body);
      freeVariables = analysis.freeVariables;
      for (const message of analysis.unsupportedRuntimeSyntax) {
        errors.hookAnalysisErrors.push({ hook: file, message });
      }
    } catch (error) {
      errors.hookAnalysisErrors.push({
        hook: file,
        message: error.message,
      });
    }
    if (freeVariables.length) hits.push("free-variable-candidate");
    if (hits.length) {
      riskHooks.push({
        id,
        file,
        field: ref.field,
        hits,
        ...(freeVariables.length ? { freeVariables } : {}),
      });
    }
  }
  riskHooks.sort((left, right) => comparePath(left.id, right.id));
  const riskPatternCounts = Object.fromEntries(
    RISK_PATTERN_NAMES.map((name) => [
      name,
      riskHooks.filter((hook) => hook.hits.includes(name)).length,
    ]),
  );
  const riskSamplesByPattern = Object.fromEntries(
    RISK_PATTERN_NAMES.map((name) => [
      name,
      riskHooks.filter((hook) => hook.hits.includes(name)).slice(0, 5),
    ]),
  );
  const irRiskSignals = countIrKeys(irRecords, [
    "script",
    "jsScript",
    "scriptTimeout",
    "cacheKey",
    "cacheTtl",
    "cacheStrategy",
    "cacheByDirectory",
  ]);
  const uniqueSourceHashes = uniqueRecordHashes(sourceFunctionRecords, {
    includeNative: false,
  });
  const report = {
    version: 1,
    supportedHookFields: { ...SUPPORTED_HOOK_FIELDS },
    knownNonSpecFiles: [...KNOWN_NON_SPEC_FILES].sort(comparePath),
    source: {
      files: sourceFiles.length,
      specs: [...sourceRecords.values()].filter((record) => record.isSpec)
        .length,
      allowlistedNonSpecs: [...sourceRecords.values()].filter(
        (record) => record.allowlisted,
      ).length,
      functionCounts: countRecords(sourceFunctionRecords),
      compilerExtractionCounts: countCompilerInstances(sourceFunctionRecords, {
        includeNative: false,
      }),
      nativeRewriteCounts: countNativeRewrites(sourceFunctionRecords),
      uniqueFunctionCounts: Object.fromEntries(
        SOURCE_FIELDS.map((field) => [field, uniqueSourceHashes[field].length]),
      ),
    },
    ir: {
      files: irRecords.size,
      hookReferenceCounts: fieldCountsFromRefs(refs),
      hookIdCounts: uniqueIdCountsFromRefs(refs),
      hookReferences: refs.length,
      riskSignals: irRiskSignals,
    },
    hooks: {
      files: hookFilesOnDisk.length,
      referenced: refsById.size,
      uniqueBodyCounts: Object.fromEntries(
        SUPPORTED_IR_HOOK_FIELDS.map((field) => [
          field,
          uniqueHashesByIrField(refs, hookBodiesById)[field].length,
        ]),
      ),
      riskPatterns: RISK_PATTERN_NAMES,
      riskPatternCounts,
      riskSamplesByPattern,
      riskyHooks: riskHooks.slice(0, 40),
      riskyHookCount: riskHooks.length,
    },
    hookModules,
    typedHooks,
    hookManifest: [...refsById.keys()]
      .sort(comparePath)
      .map((id) => {
        const ref = refsById.get(id);
        const hook = hookBodiesById.get(id) ?? {};
        return {
          id,
          file: hookFileName(id),
          field: ref.field,
          ir: ref.ir,
          sha256: hook.sha256 ?? null,
          functionBodySha256: hook.functionBodySha256 ?? null,
          path: hook.path ?? null,
          body: hook.body ?? null,
          module: hook.module ?? null,
          moduleSha256: hook.moduleSha256 ?? null,
        };
      }),
    sourceToIr,
    errors,
  };
  report.ok = Object.values(errors).every(
    (value) => !Array.isArray(value) || value.length === 0,
  );
  report.reproducibility = {
    algorithm: "sha256",
    generatorSha256: sha256(
      await safeReadRegularFile(
        fileURLToPath(import.meta.url),
        "audit generator",
        { root: dirname(fileURLToPath(import.meta.url)) },
      ),
    ),
    manifestSha256: sha256(JSON.stringify(report)),
  };
  return report;
}

const PUBLIC_AUDIT_OPTIONS = [
  "sourceRoot",
  "irRoot",
  "hooksRoot",
  "validateHookModules",
  "validateTypedHooks",
  "pair",
];

function rejectUnknownAuditOptions(options, label, allowed = PUBLIC_AUDIT_OPTIONS) {
  if (!isRecord(options)) {
    throw new Error(`${label} options must be an object`);
  }
  for (const key of Object.keys(options)) {
    if (!allowed.includes(key)) {
      throw new Error(`${label} contains unknown field ${key}`);
    }
  }
}

/** Public same-process audit. Pair-lock nesting is handled by spec-pair's ALS. */
async function auditSpecsHooks(options = {}) {
  rejectUnknownAuditOptions(options, "auditSpecsHooks");
  return withPairLock(pairLockPath, () => auditSpecsHooksUnlocked(options));
}

/**
 * Restricted entry used only by reference-audit-worker's child process.
 * `lockProof` is not a public audit option: it is supplied out-of-band by the
 * parent and must identify the lock owner as this child's parent process.
 */
export async function auditSpecsHooksForReferenceWorker(
  options = {},
  lockProof,
) {
  rejectUnknownAuditOptions(options, "restricted audit", [
    "sourceRoot",
    "irRoot",
    "hooksRoot",
  ]);
  if (!isRecord(lockProof) || lockProof.pid !== process.ppid) {
    throw new Error("restricted audit requires a pair lock proof from its parent");
  }
  await verifyPairLockProof(pairLockPath, lockProof);
  return auditSpecsHooksUnlocked({ ...options, pair: "skip" });
}

function compactAuditReport(report) {
  const errors = Object.fromEntries(
    Object.entries(report.errors).map(([name, entries]) => [
      name,
      {
        count: entries.length,
        samples: entries.slice(0, 3),
      },
    ]),
  );
  return {
    version: report.version,
    ok: report.ok,
    source: report.source,
    ir: report.ir,
    hooks: {
      files: report.hooks.files,
      referenced: report.hooks.referenced,
      uniqueBodyCounts: report.hooks.uniqueBodyCounts,
      riskPatterns: report.hooks.riskPatterns,
      riskPatternCounts: report.hooks.riskPatternCounts,
      freeVariableSamples:
        report.hooks.riskSamplesByPattern["free-variable-candidate"],
      riskyHookCount: report.hooks.riskyHookCount,
    },
    hookModules: report.hookModules,
    typedHooks: report.typedHooks,
    errors,
    reproducibility: report.reproducibility,
  };
}

/**
 * Write a full audit report only inside the repository's real tree. Existing
 * regular files are overwritten through an O_NOFOLLOW, identity-checked
 * handle; missing files are created exclusively. A symlink, FIFO, device, or
 * ancestor redirect is rejected before any open, so EC_SPECS_AUDIT_OUT cannot
 * turn the read-only audit into an arbitrary filesystem write.
 */
export async function safeWriteAuditReport(
  outputPath,
  report,
  { root = repoDir } = {},
) {
  if (
    typeof outputPath !== "string" ||
    !outputPath ||
    outputPath.includes("\0") ||
    outputPath.includes("\\")
  ) {
    throw new Error("EC_SPECS_AUDIT_OUT must be a non-empty POSIX path");
  }
  const approvedRoot = resolve(root);
  const target = isAbsolute(outputPath)
    ? resolve(outputPath)
    : resolve(approvedRoot, outputPath);
  if (!isInside(approvedRoot, target)) {
    throw new Error("EC_SPECS_AUDIT_OUT escapes the repository root");
  }
  return writeReferenceFile(
    target,
    `${JSON.stringify(report, null, 2)}\n`,
    { root: approvedRoot, overwrite: true, label: "EC_SPECS_AUDIT_OUT" },
  );
}

export { auditSpecsHooks, freeVariableCandidates };

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const report = await auditSpecsHooks();
  if (process.env.EC_SPECS_AUDIT_OUT) {
    await safeWriteAuditReport(
      process.env.EC_SPECS_AUDIT_OUT,
      report,
    );
  }
  const outputReport = process.argv.includes("--full")
    ? report
    : compactAuditReport(report);
  const output = `${JSON.stringify(outputReport, null, 2)}\n`;
  process.stdout.write(output);
  if (!report.ok) {
    process.stderr.write("error: spec hook audit failed\n");
    process.exitCode = 1;
  }
}
