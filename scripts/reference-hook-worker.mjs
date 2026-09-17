#!/usr/bin/env node
/**
 * Test-only Fig source/generated-hook reference runner. The requested module
 * is evaluated in a VM realm without Node globals or a real command executor.
 * The parent process enforces the invocation/serialization wall-clock
 * deadline; this worker separately limits VM setup and synchronous module
 * evaluation. Never bundle this file into the .app.
 */
import { createHash, randomUUID } from "node:crypto";
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
import vm from "node:vm";

import { functionSource } from "./filepaths-helper.mjs";
import { comparePath } from "./spec-pair.mjs";

const defaultSourceRoot = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "bundle",
  "specs",
);

function inside(root, target) {
  const path = relative(resolve(root), resolve(target));
  return (
    path === "" ||
    (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`))
  );
}

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

/**
 * Verify the lexical path before resolving it. Probe workers receive an
 * audited root, but this check is still needed for a direct worker invocation
 * and for a file/ancestor replacement between the audit and the read.
 */
async function assertNoSymlinkAncestors(path, label) {
  const absolute = resolve(path);
  const components = absolute.split(sep).filter(Boolean);
  let current = absolute.startsWith(sep) ? sep : "";
  for (let index = 0; index < components.length; index += 1) {
    current = current ? join(current, components[index]) : components[index];
    const info = await lstat(current);
    if (info.isSymbolicLink()) {
      const canonical = await realpath(current).catch(() => null);
      if (canonical && pathsMatchKnownAlias(current, canonical)) continue;
      throw new Error(`${label} contains a symbolic-link ancestor`);
    }
    if (index < components.length - 1 && !info.isDirectory()) {
      throw new Error(`${label} ancestor is not a directory`);
    }
  }
  return absolute;
}

/**
 * Read a regular file through an identity-checked, non-following handle.
 * O_NONBLOCK keeps a malicious FIFO from stalling the reference worker.
 */
async function readRegularFile(path, label, { root = null } = {}) {
  const absolute = resolve(path);
  const approvedRoot = root ? resolve(root) : null;
  if (approvedRoot && !inside(approvedRoot, absolute)) {
    throw new Error(`${label} escapes its approved root`);
  }
  await assertNoSymlinkAncestors(absolute, label);
  const before = await lstat(absolute);
  if (before.isSymbolicLink()) throw new Error(`${label} is a symbolic link`);
  if (!before.isFile()) throw new Error(`${label} is a special entry`);
  const canonical = await realpath(absolute);
  if (
    (canonical !== absolute && !pathsMatchKnownAlias(absolute, canonical)) ||
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
    return {
      path: canonical,
      text: await handle.readFile("utf8"),
    };
  } finally {
    await handle?.close().catch(() => {});
  }
}

function pathSegments(path) {
  if (!/^root(?:\.[A-Za-z_$][\w$]*|\[\d+\])*$/.test(path)) {
    throw new Error("invalid source hook path");
  }
  return [...path.matchAll(/\.([A-Za-z_$][\w$]*)|\[(\d+)\]/g)].map(
    (match) => match[1] ?? Number(match[2]),
  );
}

function findFunction(root, path) {
  let value = root;
  let owner = null;
  for (const segment of pathSegments(path)) {
    owner = value;
    value = value?.[segment];
  }
  if (typeof value !== "function")
    throw new Error("source path is not a function");
  return { fn: value, owner };
}

function verifyModuleSha(relativeFile, source, payload) {
  const expected = payload.sourceSha256ByModule?.[relativeFile];
  if (typeof expected !== "string" || !/^[a-f0-9]{64}$/.test(expected)) {
    throw new Error(
      "source module is missing from the precomputed SHA manifest",
    );
  }
  const actual = createHash("sha256").update(source).digest("hex");
  if (actual !== expected)
    throw new Error("source module SHA differs from audit");
}

function sourceModule(source, context, identifier, absolutePath) {
  return new vm.SourceTextModule(source, {
    context,
    identifier,
    initializeImportMeta(meta) {
      meta.url = pathToFileURL(absolutePath).href;
    },
  });
}

function failure(error, stage) {
  const errorClass =
    error?.name === "UnmockedCommand"
      ? "UnmockedCommand"
      : error?.name === "FixtureRejected"
        ? "FixtureRejected"
        : error?.name === "ReferenceError"
          ? "ReferenceError"
          : error?.name === "TypeError"
            ? "TypeError"
            : error?.name === "SyntaxError"
              ? "SyntaxError"
              : error?.name === "Error"
                ? "Error"
                : "UnknownError";
  return { status: "error", stage, errorClass };
}

function canonicalize(value, signature) {
  if (Array.isArray(value))
    return value.map((entry) => canonicalize(entry, signature));
  if (!value || typeof value !== "object") return value;
  if (
    value.$ecReferenceSignature === signature &&
    value.$ecReferenceKind === "function"
  ) {
    return {
      kind: "function",
      bodySha256: createHash("sha256").update(value.source).digest("hex"),
    };
  }
  if (value.$ecReferenceSignature === signature && value.$ecReferenceKind) {
    const rest = { ...value };
    delete rest.$ecReferenceKind;
    delete rest.$ecReferenceSignature;
    return { kind: value.$ecReferenceKind, ...rest };
  }
  return Object.fromEntries(
    Object.keys(value)
      .sort(comparePath)
      .map((key) => [key, canonicalize(value[key], signature)]),
  );
}

async function run(payload) {
  if (typeof vm.SourceTextModule !== "function") {
    throw new Error("VM modules require --experimental-vm-modules");
  }
  const mode = payload.mode ?? "source";
  if (mode !== "source" && mode !== "module") {
    throw new Error("unsupported reference worker mode");
  }
  let root;
  let source;
  let file;
  if (mode === "source") {
    const sourceRoot = payload.sourceRoot ?? defaultSourceRoot;
    await assertNoSymlinkAncestors(sourceRoot, "source root");
    root = await realpath(sourceRoot);
    await assertNoSymlinkAncestors(root, "source root");
    if (
      typeof payload.source !== "string" ||
      payload.source.includes("\\") ||
      payload.source.startsWith("/") ||
      payload.source
        .split("/")
        .some((part) => part === "" || part === "." || part === "..")
    ) {
      throw new Error("invalid relative source file");
    }
    const sourceRead = await readRegularFile(
      join(root, payload.source),
      "source module",
      { root },
    );
    file = sourceRead.path;
    if (!inside(root, file) || !file.endsWith(".js")) {
      throw new Error("source file escapes the approved root");
    }
    source = sourceRead.text;
    if (
      payload.sourceFileSha256 !==
      payload.sourceSha256ByModule?.[payload.source]
    ) {
      throw new Error("source file SHA differs from audited hook instance");
    }
    verifyModuleSha(payload.source, source, payload);
  }
  const context = vm.createContext(Object.create(null), {
    codeGeneration: { strings: false, wasm: false },
  });
  // Capture VM intrinsics before loading the requested module. The executor and its trace
  // live in a lexical closure held only by this worker, never in globalThis.
  // Exposing a host function/object in the VM would leak its constructor.
  const invoke = vm.runInContext(
    `globalThis.console = { log() {}, warn() {}, error() {} };
     (() => {
       const apply = Reflect.apply;
       const parse = JSON.parse;
       const stringify = JSON.stringify;
       const arrayMap = Array.prototype.map;
       const arrayFind = Array.prototype.find;
       const arrayPush = Array.prototype.push;
       const PromiseType = Promise;
       const ErrorType = Error;
       const defineProperty = Object.defineProperty;
       return (fn, receiver, argsJson, rulesJson) => {
         const calls = [];
         const rules = parse(rulesJson);
         const descriptor = item => stringify({
           command: item?.command,
           args: item?.args ?? [],
           cwd: item?.cwd ?? null,
           env: item?.env ?? null,
           timeout: item?.timeout ?? null,
         });
         const mockExec = async request => {
           const normalized = typeof request === "string"
             ? { command: request, args: [] } : request;
           const snapshot = parse(stringify(normalized));
           const normalizedJson = descriptor(snapshot);
           apply(arrayPush, calls, [snapshot]);
           const rule = apply(arrayFind, rules, [
             item => descriptor(item) === normalizedJson,
           ]);
           if (!rule) {
             const error = new ErrorType("unmocked command");
             defineProperty(error, "name", { value: "UnmockedCommand" });
             throw error;
           }
           if (rule.kind === "pending" || rule.kind === "timeout") {
             return await new PromiseType(() => {});
           }
           if (rule.kind === "reject") {
             const error = new ErrorType("fixture rejected command");
             defineProperty(error, "name", { value: "FixtureRejected" });
             throw error;
           }
           return { status: rule.status ?? 0, stdout: rule.stdout ?? "", stderr: rule.stderr ?? "" };
         };
         const args = apply(arrayMap, parse(argsJson), [
           item => item && typeof item === "object" && item.$referenceExec === true
             ? mockExec : item,
         ]);
         return { pending: apply(fn, receiver, args), trace: () => stringify(calls) };
       };
     })()`,
    context,
    { timeout: 1000 },
  );
  const originalFunctionToString = vm.runInContext(
    "Function.prototype.toString",
    context,
    { timeout: 1000 },
  );
  const serialize = vm.runInContext(
    `(() => {
       const stringify = JSON.stringify;
       const functionToString = Function.prototype.toString;
       const apply = Reflect.apply;
       const getOwnPropertyNames = Object.getOwnPropertyNames;
       const getOwnPropertySymbols = Object.getOwnPropertySymbols;
       const objectKeys = Object.keys;
       const objectIs = Object.is;
       const hasOwn = Object.prototype.hasOwnProperty;
       const propertyIsEnumerable = Object.prototype.propertyIsEnumerable;
       const arrayIsArray = Array.isArray;
       const arrayBufferIsView = ArrayBuffer.isView;
       const numberIsFinite = Number.isFinite;
       const numberIsNaN = Number.isNaN;
       const StringType = String;
       const DateType = Date, RegExpType = RegExp, MapType = Map, SetType = Set;
       const ErrorType = Error, ArrayBufferType = ArrayBuffer;
       return (result, signature) => stringify(result, function(key, value) {
         const original = this?.[key];
         if (original && typeof original === "object" &&
             typeof original.toJSON === "function" && !(original instanceof DateType)) {
           throw new ErrorType("unsupported source toJSON would discard result fields");
         }
         if (value && (typeof value === "object" || typeof value === "function") &&
             getOwnPropertySymbols(value).length) {
           throw new ErrorType("symbol-keyed reference result is not representable");
         }
         if (typeof value === "function") {
           const extras = getOwnPropertyNames(value).filter(name =>
             !["length", "name", "arguments", "caller", "prototype"].includes(name));
           if (objectKeys(value).length || extras.length) {
             throw new ErrorType("function result has properties requiring an explicit adapter");
           }
           return { $ecReferenceSignature: signature, $ecReferenceKind: "function",
             source: apply(functionToString, value, []) };
         }
         if (value === undefined) {
           const hole = arrayIsArray(this) && !apply(hasOwn, this, [key]);
           return { $ecReferenceSignature: signature,
             $ecReferenceKind: hole ? "array-hole" : "undefined" };
         }
         if (typeof value === "bigint") return { $ecReferenceSignature: signature, $ecReferenceKind: "bigint", value: StringType(value) };
         if (typeof value === "symbol") return { $ecReferenceSignature: signature, $ecReferenceKind: "symbol", value: StringType(value) };
         if (typeof value === "number" && objectIs(value, -0)) {
           return { $ecReferenceSignature: signature, $ecReferenceKind: "negative-zero" };
         }
         if (typeof value === "number" && !numberIsFinite(value)) {
           return { $ecReferenceSignature: signature, $ecReferenceKind: "nonfinite-number", value: StringType(value) };
         }
         if (original instanceof DateType) {
           if (getOwnPropertyNames(original).length) {
             throw new ErrorType("Date result has custom properties requiring an explicit adapter");
           }
           return { $ecReferenceSignature: signature, $ecReferenceKind: "date", value: numberIsNaN(original.getTime())
             ? "Invalid Date" : original.toISOString() };
         }
         if (value instanceof RegExpType) {
           const extras = getOwnPropertyNames(value).filter(name => name !== "lastIndex");
           if (extras.length) throw new ErrorType("RegExp result has custom properties");
           return { $ecReferenceSignature: signature, $ecReferenceKind: "regexp",
             source: value.source, flags: value.flags, lastIndex: value.lastIndex };
         }
         if (value instanceof MapType) {
           if (getOwnPropertyNames(value).length) throw new ErrorType("Map result has custom properties");
           return { $ecReferenceSignature: signature, $ecReferenceKind: "map", entries: [...value.entries()] };
         }
         if (value instanceof SetType) {
           if (getOwnPropertyNames(value).length) throw new ErrorType("Set result has custom properties");
           return { $ecReferenceSignature: signature, $ecReferenceKind: "set", values: [...value.values()] };
         }
         if (value instanceof ErrorType) {
           const extras = getOwnPropertyNames(value).filter(name =>
             !["stack", "message", "cause"].includes(name));
           if (extras.length) throw new ErrorType("Error result has custom properties");
           return { $ecReferenceSignature: signature, $ecReferenceKind: "error",
             name: value.name, message: value.message, stack: value.stack,
             ...(apply(hasOwn, value, ["cause"]) ? { cause: value.cause } : {}) };
         }
         if (value instanceof ArrayBufferType || arrayBufferIsView(value)) {
           throw new ErrorType("binary reference result requires an explicit adapter");
         }
         if (value && typeof value === "object") {
           const names = getOwnPropertyNames(value);
           const omitted = arrayIsArray(value)
             ? names.filter(name => name !== "length" && !/^(?:0|[1-9]\\d*)$/.test(name))
             : names.filter(name => !apply(propertyIsEnumerable, value, [name]));
           if (omitted.length) {
             throw new ErrorType("non-enumerable result properties require an explicit adapter");
           }
         }
         return value;
       });
     })()`,
    context,
    { timeout: 1000 },
  );
  vm.runInContext(
    `(() => {
       const freeze = Object.freeze;
       const constructors = [
         Object, Array, Function, Promise, Error, EvalError, RangeError,
         ReferenceError, SyntaxError, TypeError, URIError, Number, String,
         Boolean, BigInt, Symbol, Date, RegExp, Map, Set, WeakMap, WeakSet,
         ArrayBuffer, DataView, Int8Array, Uint8Array, Uint8ClampedArray,
         Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array,
         Float64Array, BigInt64Array, BigUint64Array,
       ];
       for (const constructor of constructors) {
         if (constructor.prototype) freeze(constructor.prototype);
         freeze(constructor);
       }
       freeze(JSON);
       freeze(Reflect);
       freeze(Math);
       freeze(console);
       Object.defineProperty(globalThis, "console", {
         value: console, writable: false, configurable: false,
       });
     })()`,
    context,
    { timeout: 1000 },
  );
  let fn;
  let receiver;
  let verified;
  if (mode === "source") {
    const sourceModuleInstance = sourceModule(
      source,
      context,
      payload.source,
      file,
    );
    const modules = new Map([[file, sourceModuleInstance]]);
    await sourceModuleInstance.link(async (specifier, referencingModule) => {
      if (!specifier.startsWith("./") && !specifier.startsWith("../")) {
        throw new Error(
          "bare and host source imports are blocked in the reference VM",
        );
      }
      const parent = join(root, referencingModule.identifier);
      const importedRead = await readRegularFile(
        join(dirname(parent), specifier),
        "relative source import",
        { root },
      );
      const imported = importedRead.path;
      if (!inside(root, imported) || !imported.endsWith(".js")) {
        throw new Error("relative source import escapes the approved root");
      }
      const cached = modules.get(imported);
      if (cached) return cached;
      const importedName = relative(root, imported).split(sep).join("/");
      const importedSource = importedRead.text;
      verifyModuleSha(importedName, importedSource, payload);
      const linked = sourceModule(
        importedSource,
        context,
        importedName,
        imported,
      );
      modules.set(imported, linked);
      return linked;
    });
    await sourceModuleInstance.evaluate({ timeout: 1000 });
    fn = findFunction(sourceModuleInstance.namespace.default, payload.path);
    receiver = payload.field === "custom" ? fn.owner : undefined;
    fn = fn.fn;
    const digest = createHash("sha256")
      .update(functionSource(fn) ?? "")
      .digest("hex");
    if (digest !== payload.sha256)
      throw new Error("source hook SHA differs from audit");
    verified = { sourceShaVerified: true };
  } else {
    await assertNoSymlinkAncestors(payload.irRoot, "IR root");
    const irRoot = await realpath(payload.irRoot);
    const moduleRootPath = join(irRoot, "source-modules");
    await assertNoSymlinkAncestors(moduleRootPath, "source module root");
    const moduleRoot = await realpath(moduleRootPath);
    if (!inside(irRoot, moduleRoot)) {
      throw new Error(
        "closure-preserving hook module directory escapes its IR root",
      );
    }
    if (
      typeof payload.module !== "string" ||
      !/^[^/\\\0]+\.js$/.test(payload.module) ||
      payload.module === ".js" ||
      payload.module === "..js"
    ) {
      throw new Error("invalid closure-preserving hook module path");
    }
    if (
      typeof payload.moduleSha256 !== "string" ||
      !/^[a-f0-9]{64}$/.test(payload.moduleSha256)
    ) {
      throw new Error("closure-preserving hook module SHA is invalid");
    }
    const moduleRead = await readRegularFile(
      join(moduleRoot, payload.module),
      "closure-preserving hook module",
      { root: moduleRoot },
    );
    file = moduleRead.path;
    if (!inside(moduleRoot, file) || !file.endsWith(".js")) {
      throw new Error("closure-preserving hook module escapes its root");
    }
    source = moduleRead.text;
    const digest = createHash("sha256").update(source).digest("hex");
    if (digest !== payload.moduleSha256) {
      throw new Error(
        "closure-preserving hook module SHA differs from manifest",
      );
    }
    const moduleInstance = sourceModule(source, context, payload.module, file);
    await moduleInstance.link(async () => {
      throw new Error(
        "closure-preserving hook modules must not import host or source modules",
      );
    });
    await moduleInstance.evaluate({ timeout: 1000 });
    const table = moduleInstance.namespace.default;
    if (!table || typeof table !== "object") {
      throw new Error("closure-preserving hook module did not export a table");
    }
    if (typeof payload.hookId !== "string" || !payload.hookId) {
      throw new Error("closure-preserving hook id is required");
    }
    fn = table[payload.hookId];
    if (typeof fn !== "function") {
      throw new Error("closure-preserving hook id is missing from its module");
    }
    // The compiler emits an arrow wrapper for `custom` hooks. It applies the
    // original function to its captured owner, so passing a second receiver
    // here would change `this` and invalidate the module parity check.
    receiver = undefined;
    verified = { moduleShaVerified: true };
  }
  if (
    vm.runInContext("Function.prototype.toString", context, {
      timeout: 1000,
    }) !== originalFunctionToString
  ) {
    throw new Error(
      "reference module changed the function-to-string intrinsic",
    );
  }
  const isBatch = Object.hasOwn(payload, "invocations");
  const invocations = isBatch ? payload.invocations : [payload];
  if (!Array.isArray(invocations) || invocations.length === 0) {
    throw new Error("reference invocation batch must be a non-empty array");
  }
  if (isBatch && invocations.length > 256) {
    throw new Error("reference invocation batch exceeds 256 entries");
  }

  const results = [];
  for (const invocation of invocations) {
    if (isBatch) {
      if (
        !invocation ||
        typeof invocation !== "object" ||
        Array.isArray(invocation) ||
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
    let call;
    try {
      call = invoke(
        fn,
        receiver,
        JSON.stringify(invocation.args ?? []),
        JSON.stringify(invocation.mockExecRules ?? []),
      );
    } catch (error) {
      results.push({ ...failure(error, "invoke"), ...verified });
      continue;
    }
    let result;
    try {
      result = await call.pending;
    } catch (error) {
      let execTrace;
      try {
        execTrace = JSON.parse(call.trace());
      } catch (traceError) {
        results.push({ ...failure(traceError, "trace"), ...verified });
        continue;
      }
      results.push({
        ...failure(error, "invoke"),
        ...verified,
        execTrace,
      });
      continue;
    }
    const signature = randomUUID();
    let execTrace;
    try {
      execTrace = JSON.parse(call.trace());
    } catch (error) {
      results.push({ ...failure(error, "trace"), ...verified });
      continue;
    }
    let serialized;
    try {
      serialized = serialize(result, signature);
    } catch (error) {
      results.push({
        ...failure(error, "serialize"),
        ...verified,
        execTrace,
      });
      continue;
    }
    if (serialized === undefined) {
      results.push({ status: "unserializable", ...verified, execTrace });
      continue;
    }
    results.push({
      status: "success",
      ...verified,
      value: canonicalize(JSON.parse(serialized), signature),
      execTrace,
    });
  }
  if (isBatch) return { status: "batch-success", ...verified, results };
  return results[0];
}

try {
  let input = "";
  for await (const chunk of process.stdin) {
    input += chunk;
    if (input.length > 2_000_000)
      throw new Error("reference fixture input exceeds 2 MB");
  }
  // A pending Promise without an event-loop handle makes Node exit with code
  // 13 before the parent watchdog can classify the run as a timeout.
  const pendingKeeper = setInterval(() => {}, 1000);
  try {
    const result = await run(JSON.parse(input));
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } finally {
    clearInterval(pendingKeeper);
  }
} catch (error) {
  process.stdout.write(`${JSON.stringify(failure(error, "reference"))}\n`);
}
