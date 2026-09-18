#!/usr/bin/env node
/**
 * Classify extracted Fig hook bodies for the native migration.
 *
 * This is a build-time inventory only. It never evaluates hook JavaScript and
 * it does not select a runtime implementation. A body is `typed-ir` only when
 * `compileTypedHook` succeeds for its field contract; every other valid body
 * stays `requires-native-adapter` until a named adapter exists. The report is
 * deterministic so it can be used as an input to that work.
 */
import { createHash } from "node:crypto";
import { lstat, readdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import * as acorn from "acorn";
import * as eslintScope from "eslint-scope";

import {
  auditSpecsHooks,
  freeVariableCandidates,
} from "./audit-spec-hooks.mjs";
import {
  SUPPORTED_HOOK_FIELDS,
  SUPPORTED_IR_HOOK_FIELDS,
} from "./compile-spec-ir.mjs";
import {
  countFunctionsInValue,
  KNOWN_UNAPPLIED_VERSION_DIFFS,
  KNOWN_VERSION_SELECTORS,
} from "./spec-hook-contract.mjs";
import { comparePath } from "./spec-pair.mjs";
import {
  TYPED_HOOK_CONTRACTS,
  compileTypedHook,
} from "./typed-hook-ir.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const defaultSourceRoot = join(repoDir, "bundle", "specs");
const defaultIrRoot = join(repoDir, "bundle", "specs-ir");
// The committed, reviewable inventory. CI recomputes it from the bundled
// source/IR pair and fails on drift, so a specs update or compiler change
// that moves a hook between classes shows up in review instead of silently
// changing what the native migration still owes.
const defaultInventoryPath = join(
  repoDir,
  "crates",
  "ec_engine",
  "testdata",
  "native-hooks",
  "inventory.json",
);
const defaultBaselineRoot = join(
  repoDir,
  "crates",
  "ec_engine",
  "testdata",
  "native-hooks",
  "baseline",
);
const OUTPUT_BASELINE_BLOCKER = "output-baseline-not-established";

export const INVENTORY_VERSION = 1;
export const INVENTORY_KIND = "native-hook-inventory";
const VERSIONED_SPEC_BLOCKER = "versioned-spec-behaviour-unadapted";

export const CLASSIFICATION_STATUSES = Object.freeze([
  "typed-ir",
  "native-filepaths-rewrite",
  "requires-native-adapter",
  "syntax-or-analysis-failure",
  "unclassified",
]);

const STATUS_ORDER = new Map(
  CLASSIFICATION_STATUSES.map((status, index) => [status, index]),
);
const TYPED_IR_STATUS = CLASSIFICATION_STATUSES[0];
const NATIVE_FILEPATHS_STATUS = CLASSIFICATION_STATUSES[1];
const ADAPTER_STATUS = CLASSIFICATION_STATUSES[2];
const FAILURE_STATUS = CLASSIFICATION_STATUSES[3];
const UNCLASSIFIED_STATUS = CLASSIFICATION_STATUSES[4];

const IR_TO_SOURCE_FIELD = Object.fromEntries(
  Object.entries(SUPPORTED_HOOK_FIELDS).map(([source, ir]) => [ir, source]),
);
const SOURCE_FIELDS = Object.keys(SUPPORTED_HOOK_FIELDS);
const COMMAND_PARAMETER_FIELDS = new Set(["custom", "alias", "generateSpec"]);
const ENVIRONMENT_PARAMETER_FIELDS = new Set(["custom"]);

// Calls in this set are deterministic string/array reads. This is a syntax
// gate for the typed-IR compiler, not a claim that the native engine already
// implements these methods.
const PURE_METHODS = new Set([
  "at",
  "charAt",
  "endsWith",
  "includes",
  "indexOf",
  "lastIndexOf",
  "concat",
  "padEnd",
  "padStart",
  "repeat",
  "replace",
  "replaceAll",
  "slice",
  "split",
  "startsWith",
  "substring",
  "substr",
  "toLowerCase",
  "toUpperCase",
  "trim",
  "trimEnd",
  "trimStart",
]);

const CANDIDATE_NODE_TYPES = new Set([
  "ArrayExpression",
  "ArrowFunctionExpression",
  "BinaryExpression",
  "BlockStatement",
  "CallExpression",
  "ChainExpression",
  "ConditionalExpression",
  "ExpressionStatement",
  "FunctionExpression",
  "Identifier",
  "Literal",
  "LogicalExpression",
  "MemberExpression",
  "ObjectExpression",
  "Property",
  "Program",
  "ReturnStatement",
  "TemplateElement",
  "TemplateLiteral",
  "UnaryExpression",
]);

const RISK_NAMES = Object.freeze([
  "unbound-identifiers",
  "this",
  "exec",
  "process",
  "async",
  "fig",
  "require",
  "window",
  "intl",
  "console",
  "nondeterministic",
  "global-this",
  "dynamic-property",
  "regexp-v",
  "unsupported-syntax",
  "complexity",
]);

const COMPLEXITY_LIMITS = Object.freeze({
  maxNodes: 48,
  maxDepth: 10,
  maxCalls: 3,
  maxMembers: 6,
});

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function sortStrings(values) {
  return [...new Set(values)].sort(comparePath);
}

function sortObjectKeys(value) {
  if (Array.isArray(value)) return value.map(sortObjectKeys);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.keys(value)
      .sort(comparePath)
      .map((key) => [key, sortObjectKeys(value[key])]),
  );
}

function emptyStatusCounts() {
  return Object.fromEntries(
    CLASSIFICATION_STATUSES.map((status) => [status, 0]),
  );
}

function emptyFieldStatusCounts() {
  return Object.fromEntries(
    SUPPORTED_IR_HOOK_FIELDS.map((field) => [field, emptyStatusCounts()]),
  );
}

function bodyFromHookFile(source) {
  if (typeof source !== "string" || !source.startsWith("export default ")) {
    return null;
  }
  return source
    .slice("export default ".length)
    .replace(/;\n$/, "")
    .replace(/;$/, "");
}

async function walkHookFiles(dir) {
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
  entries.sort((left, right) => comparePath(left.name, right.name));
  const files = [];
  for (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      const nested = await walkHookFiles(full);
      files.push(...nested.map((file) => join(entry.name, file)));
    } else if (entry.isFile() && entry.name.endsWith(".js")) {
      files.push(entry.name);
    }
  }
  return files.sort(comparePath);
}

function unwrapFunction(ast) {
  let node = ast;
  if (node?.type === "Program" && node.body.length === 1) {
    node = node.body[0];
  }
  if (node?.type === "ExpressionStatement") node = node.expression;
  return node;
}

function patternNames(pattern, names = []) {
  if (!pattern) return names;
  switch (pattern.type) {
    case "Identifier":
      names.push(pattern.name);
      break;
    case "AssignmentPattern":
      patternNames(pattern.left, names);
      break;
    case "RestElement":
      patternNames(pattern.argument, names);
      break;
    case "ArrayPattern":
      pattern.elements.forEach((item) => patternNames(item, names));
      break;
    case "ObjectPattern":
      pattern.properties.forEach((property) => {
        if (property.type === "RestElement")
          patternNames(property.argument, names);
        else patternNames(property.value, names);
      });
      break;
    default:
      break;
  }
  return names;
}

function isPropertyKey(node, parent) {
  return (
    (parent?.type === "Property" && parent.key === node && !parent.computed) ||
    (parent?.type === "MemberExpression" &&
      parent.property === node &&
      !parent.computed) ||
    (parent?.type === "MethodDefinition" &&
      parent.key === node &&
      !parent.computed)
  );
}

function isBindingIdentifier(node, parent) {
  if (!parent) return false;
  if (
    (parent.type === "ArrowFunctionExpression" ||
      parent.type === "FunctionExpression" ||
      parent.type === "FunctionDeclaration") &&
    parent.params.some((parameter) => containsNode(parameter, node))
  ) {
    return true;
  }
  if (
    (parent.type === "VariableDeclarator" && parent.id === node) ||
    (parent.type === "FunctionDeclaration" && parent.id === node) ||
    (parent.type === "FunctionExpression" && parent.id === node) ||
    (parent.type === "ClassDeclaration" && parent.id === node) ||
    (parent.type === "ClassExpression" && parent.id === node)
  ) {
    return true;
  }
  if (
    (parent.type === "RestElement" && parent.argument === node) ||
    (parent.type === "AssignmentPattern" && parent.left === node)
  ) {
    return true;
  }
  if (
    (parent.type === "ArrayPattern" && parent.elements.includes(node)) ||
    (parent.type === "ObjectPattern" &&
      parent.properties.some(
        (property) =>
          property === node ||
          (property.type === "Property" && property.value === node),
      ))
  ) {
    return true;
  }
  return false;
}

function containsNode(root, target) {
  if (!root || typeof root !== "object") return false;
  if (root === target) return true;
  return Object.entries(root).some(([key, child]) => {
    if (key === "start" || key === "end" || key === "loc" || key === "range") {
      return false;
    }
    if (Array.isArray(child))
      return child.some((item) => containsNode(item, target));
    return containsNode(child, target);
  });
}

function walkAst(node, visitor, parent = null, depth = 0) {
  if (!node || typeof node !== "object" || typeof node.type !== "string")
    return;
  visitor(node, parent, depth);
  for (const [key, child] of Object.entries(node)) {
    if (key === "start" || key === "end" || key === "loc" || key === "range") {
      continue;
    }
    if (Array.isArray(child)) {
      child.forEach((item) => walkAst(item, visitor, node, depth + 1));
    } else if (child && typeof child === "object") {
      walkAst(child, visitor, node, depth + 1);
    }
  }
}

function callMethodName(node) {
  if (node?.type !== "CallExpression") return null;
  const callee = node.callee;
  if (callee?.type === "MemberExpression" && !callee.computed) {
    return callee.property?.type === "Identifier" ? callee.property.name : null;
  }
  return null;
}

function functionParameterNames(fn) {
  return (fn?.params ?? []).map((pattern) => patternNames(pattern));
}

function usesAnyIdentifier(ast, names) {
  if (!names.length) return false;
  const wanted = new Set(names);
  let found = false;
  walkAst(ast, (node, parent) => {
    if (
      !found &&
      node.type === "Identifier" &&
      wanted.has(node.name) &&
      !isPropertyKey(node, parent) &&
      !isBindingIdentifier(node, parent)
    ) {
      found = true;
    }
  });
  return found;
}

function isTrivialFunctionBody(body) {
  return (
    body?.type === "BlockStatement" &&
    body.body.length === 1 &&
    body.body[0].type === "ReturnStatement" &&
    body.body[0].argument != null
  );
}

function analyzeAst(ast, field, body) {
  const fn = unwrapFunction(ast);
  const metrics = {
    nodeCount: 0,
    maxDepth: 0,
    callCount: 0,
    memberCount: 0,
    functionCount: 0,
    nodeTypes: {},
  };
  const unsupportedNodes = new Set();
  const disallowedCalls = new Set();
  const risks = new Set();
  let usesThis = false;
  let usesExec = false;
  let usesProcess = false;
  let usesAsync = false;
  let usesFig = false;
  let usesRequire = false;
  let usesWindow = false;
  let usesIntl = false;
  let usesConsole = false;
  let usesNondeterministic = false;
  let usesGlobalThis = false;
  let usesDynamicProperty = false;
  const unsupportedRuntimeSyntax = new Set();

  walkAst(ast, (node, parent, depth) => {
    metrics.nodeCount += 1;
    metrics.maxDepth = Math.max(metrics.maxDepth, depth);
    metrics.nodeTypes[node.type] = (metrics.nodeTypes[node.type] ?? 0) + 1;
    if (!CANDIDATE_NODE_TYPES.has(node.type)) unsupportedNodes.add(node.type);
    if (node.type === "CallExpression") {
      metrics.callCount += 1;
      const method = callMethodName(node);
      if (!method || !PURE_METHODS.has(method))
        disallowedCalls.add(method ?? "call");
      const callee = node.callee;
      if (
        callee?.type === "Identifier" &&
        (callee.name === "exec" || callee.name === "executeCommand")
      ) {
        usesExec = true;
      }
      if (
        callee?.type === "MemberExpression" &&
        callee.property?.type === "Identifier" &&
        (callee.property.name === "exec" ||
          callee.property.name === "executeCommand")
      ) {
        usesExec = true;
      }
    }
    if (node.type === "MemberExpression") metrics.memberCount += 1;
    if (node.type === "MemberExpression" && node.computed) {
      usesDynamicProperty = true;
    }
    if (
      node.type === "ArrowFunctionExpression" ||
      node.type === "FunctionExpression" ||
      node.type === "FunctionDeclaration"
    ) {
      metrics.functionCount += 1;
      if (node.async || node.generator) usesAsync = true;
    }
    if (node.type === "AwaitExpression" || node.type === "YieldExpression") {
      usesAsync = true;
    }
    if (node.type === "ThisExpression") usesThis = true;
    if (node.type === "Literal" && node.regex?.flags?.includes("v")) {
      unsupportedRuntimeSyntax.add("regexp-v");
    }
    if (node.type === "Identifier") {
      if (node.name === "process") usesProcess = true;
      if (node.name === "fig") usesFig = true;
      if (node.name === "require") usesRequire = true;
      if (node.name === "window") usesWindow = true;
      if (node.name === "Intl") usesIntl = true;
      if (node.name === "console") usesConsole = true;
      if (node.name === "globalThis") usesGlobalThis = true;
      if (node.name === "Date") usesNondeterministic = true;
      if (node.name === "performance") usesNondeterministic = true;
    }
    if (
      node.type === "MemberExpression" &&
      node.object?.type === "Identifier"
    ) {
      if (node.object.name === "Math" && node.property?.name === "random") {
        usesNondeterministic = true;
      }
    }
  });

  let sortedFreeVariables;
  try {
    // Keep free-variable semantics aligned with the source audit's shared
    // standard-global contract while still running the pinned scope walker
    // here as an independent analysis health check.
    eslintScope.analyze(ast, {
      ecmaVersion: 2024,
      sourceType: "module",
    });
    sortedFreeVariables = sortStrings(freeVariableCandidates(body));
  } catch (error) {
    return {
      status: FAILURE_STATUS,
      failureKind: "analysis",
      analysisError: error.message,
      risks: ["analysis-failure"],
      freeVariables: [],
      dependencies: {
        input: false,
        command: false,
        environment: false,
        buildTimePureStatic: false,
      },
      metrics,
    };
  }

  if (sortedFreeVariables.length) risks.add("unbound-identifiers");
  if (usesThis) risks.add("this");
  if (usesExec) risks.add("exec");
  if (usesProcess) risks.add("process");
  if (usesAsync) risks.add("async");
  if (usesFig) risks.add("fig");
  if (usesRequire) risks.add("require");
  if (usesWindow) risks.add("window");
  if (usesIntl) risks.add("intl");
  if (usesConsole) risks.add("console");
  if (usesNondeterministic) risks.add("nondeterministic");
  if (usesGlobalThis) risks.add("global-this");
  if (usesDynamicProperty) risks.add("dynamic-property");
  if (unsupportedRuntimeSyntax.size) risks.add("unsupported-syntax");

  const parameterNames = functionParameterNames(fn);
  const sourceField = IR_TO_SOURCE_FIELD[field] ?? field;
  const commandIndex = COMMAND_PARAMETER_FIELDS.has(sourceField) ? 1 : -1;
  const environmentIndex = ENVIRONMENT_PARAMETER_FIELDS.has(sourceField)
    ? 2
    : -1;
  const inputNames = parameterNames
    .filter((_, index) => index !== commandIndex && index !== environmentIndex)
    .flat();
  const commandNames = parameterNames[commandIndex] ?? [];
  const environmentNames = parameterNames[environmentIndex] ?? [];
  const dependencies = {
    input: usesAnyIdentifier(ast, inputNames),
    command: usesAnyIdentifier(ast, commandNames),
    environment: usesAnyIdentifier(ast, environmentNames),
    buildTimePureStatic: false,
  };
  if (usesProcess || usesGlobalThis) dependencies.environment = true;

  const outerFunction =
    fn?.type === "ArrowFunctionExpression" || fn?.type === "FunctionExpression";
  if (!outerFunction) unsupportedNodes.add(fn?.type ?? "missing-function");
  if (metrics.functionCount > 1) unsupportedNodes.add("nested-function");
  if (fn?.body?.type === "BlockStatement" && !isTrivialFunctionBody(fn.body)) {
    unsupportedNodes.add("non-trivial-block");
  }
  if (fn?.body?.type === "BlockStatement") {
    const returned = fn.body.body[0]?.argument;
    if (returned) {
      walkAst(returned, (node) => {
        if (!CANDIDATE_NODE_TYPES.has(node.type))
          unsupportedNodes.add(node.type);
      });
    }
  }
  if (metrics.nodeCount > COMPLEXITY_LIMITS.maxNodes) risks.add("complexity");
  if (metrics.maxDepth > COMPLEXITY_LIMITS.maxDepth) risks.add("complexity");
  if (metrics.callCount > COMPLEXITY_LIMITS.maxCalls) risks.add("complexity");
  if (metrics.memberCount > COMPLEXITY_LIMITS.maxMembers)
    risks.add("complexity");
  if (unsupportedNodes.size || disallowedCalls.size) {
    risks.add("unsupported-syntax");
  }
  if (risks.has("complexity")) risks.add("complexity");

  const blockingRisks = new Set([
    "unbound-identifiers",
    "this",
    "exec",
    "process",
    "async",
    "fig",
    "require",
    "window",
    "intl",
    "console",
    "nondeterministic",
    "global-this",
    "dynamic-property",
    "unsupported-syntax",
    "complexity",
  ]);
  if (unsupportedRuntimeSyntax.size) {
    return {
      status: FAILURE_STATUS,
      failureKind: "unsupported-runtime-syntax",
      analysisError:
        "runtime syntax is not supported by the phase-1 QuickJS gate",
      researchCandidate: false,
      nativeExecutable: false,
      buildTimePureStatic: false,
      freeVariables: sortedFreeVariables,
      risks: sortStrings(risks),
      unsupportedRuntimeSyntax: sortStrings(unsupportedRuntimeSyntax),
      unsupportedNodes: sortStrings(unsupportedNodes),
      disallowedCalls: sortStrings(disallowedCalls),
      dependencies,
      metrics: {
        ...metrics,
        nodeTypes: Object.fromEntries(
          Object.entries(metrics.nodeTypes).sort(([left], [right]) =>
            comparePath(left, right),
          ),
        ),
      },
      parameterCount: fn?.params?.length ?? null,
      functionType: fn?.type ?? null,
      field,
    };
  }
  const candidate =
    outerFunction &&
    ![...risks].some((risk) => blockingRisks.has(risk)) &&
    disallowedCalls.size === 0;
  const buildTimePureStatic =
    candidate &&
    (fn.params?.length ?? 0) === 0 &&
    !dependencies.input &&
    !dependencies.command &&
    !dependencies.environment &&
    sortedFreeVariables.length === 0;
  dependencies.buildTimePureStatic = buildTimePureStatic;
  return {
    status: ADAPTER_STATUS,
    researchCandidate: false,
    nativeExecutable: false,
    buildTimePureStatic,
    freeVariables: sortedFreeVariables,
    risks: sortStrings(risks),
    unsupportedNodes: sortStrings(unsupportedNodes),
    disallowedCalls: sortStrings(disallowedCalls),
    dependencies,
    metrics: {
      ...metrics,
      nodeTypes: Object.fromEntries(
        Object.entries(metrics.nodeTypes).sort(([left], [right]) =>
          comparePath(left, right),
        ),
      ),
    },
    parameterCount: fn?.params?.length ?? null,
    functionType: fn?.type ?? null,
    field,
  };
}

/**
 * Analyze and classify one extracted body without evaluating it.
 * Exported for focused fixtures; the full report uses the same function.
 */
export function classifyHookBody({ body, field, moduleSource }) {
  if (typeof body !== "string" || !body.trim()) {
    return {
      status: FAILURE_STATUS,
      failureKind: "syntax",
      analysisError: "hook body is empty",
      risks: ["syntax-failure"],
      freeVariables: [],
      dependencies: {
        input: false,
        command: false,
        environment: false,
        buildTimePureStatic: false,
      },
      metrics: null,
      field,
    };
  }
  let ast;
  try {
    ast = acorn.parse(`(${body})`, {
      ecmaVersion: "latest",
      sourceType: "module",
      ranges: true,
    });
  } catch (error) {
    return {
      status: FAILURE_STATUS,
      failureKind: "syntax",
      analysisError: error.message,
      risks: ["syntax-failure"],
      freeVariables: [],
      dependencies: {
        input: false,
        command: false,
        environment: false,
        buildTimePureStatic: false,
      },
      metrics: null,
      field,
    };
  }
  return upgradeWithTypedCompile(analyzeAst(ast, field, body), body, field, moduleSource);
}

function upgradeWithTypedCompile(analysis, body, field, moduleSource) {
  if (analysis.status === FAILURE_STATUS) return analysis;
  const sourceField = IR_TO_SOURCE_FIELD[field] ?? field;
  if (!Object.hasOwn(TYPED_HOOK_CONTRACTS, sourceField)) return analysis;
  try {
    compileTypedHook({ body, sourceField, moduleSource });
  } catch {
    return analysis;
  }
  return {
    ...analysis,
    status: TYPED_IR_STATUS,
    researchCandidate: false,
  };
}

function nativeRewriteSummary(audit) {
  const byField = Object.fromEntries(
    SOURCE_FIELDS.map((field) => [
      field,
      audit.source?.nativeRewriteCounts?.[field] ?? 0,
    ]),
  );
  const sources = (audit.sourceToIr ?? [])
    .map((record) => {
      const fields = Object.fromEntries(
        SOURCE_FIELDS.filter(
          (field) => (record.nativeRewriteCounts?.[field] ?? 0) > 0,
        ).map((field) => [field, record.nativeRewriteCounts[field]]),
      );
      return Object.keys(fields).length
        ? {
            source: record.source,
            fields,
            total: Object.values(fields).reduce((sum, value) => sum + value, 0),
          }
        : null;
    })
    .filter(Boolean)
    .sort((left, right) => comparePath(left.source, right.source));
  return {
    status: NATIVE_FILEPATHS_STATUS,
    byField,
    total: Object.values(byField).reduce((sum, value) => sum + value, 0),
    sourceCount: sources.length,
    sources,
  };
}

function addLocation(locationsById, location) {
  if (!location.id) return;
  const locations = locationsById.get(location.id) ?? [];
  if (
    !locations.some(
      (existing) =>
        existing.source === location.source && existing.path === location.path,
    )
  ) {
    locations.push(location);
  }
  locationsById.set(location.id, locations);
}

function sourceLocations(audit) {
  const locationsById = new Map();
  for (const record of audit.sourceToIr ?? []) {
    for (const sourceField of SOURCE_FIELDS) {
      for (const instance of record.hookInstances?.[sourceField] ?? []) {
        addLocation(locationsById, {
          source: record.source,
          path: instance.path,
          sourceField,
          sha256: instance.sha256,
          id: instance.id,
        });
      }
    }
  }
  for (const locations of locationsById.values()) {
    locations.sort(
      (left, right) =>
        comparePath(left.source, right.source) ||
        comparePath(left.path, right.path),
    );
  }
  return locationsById;
}

function sanitizeAuditErrors(errors, roots) {
  const replacements = [
    [roots.sourceRoot, "<source>"],
    [roots.irRoot, "<ir>"],
    [roots.hooksRoot, "<hooks>"],
  ].sort((left, right) => right[0].length - left[0].length);
  function sanitize(value) {
    if (typeof value === "string") {
      return replacements.reduce(
        (text, [from, to]) => text.replaceAll(from, to),
        value,
      );
    }
    if (Array.isArray(value)) return value.map(sanitize);
    if (!value || typeof value !== "object") return value;
    return Object.fromEntries(
      Object.entries(value).map(([key, child]) => [key, sanitize(child)]),
    );
  }
  return sanitize(errors);
}

function statusCounts(hooks) {
  const counts = emptyStatusCounts();
  hooks.forEach((hook) => {
    counts[hook.status] = (counts[hook.status] ?? 0) + 1;
  });
  return counts;
}

function bodyStatusCounts(groups) {
  return statusCounts(groups);
}

function countDependencies(items) {
  return {
    buildTimePureStatic: items.filter(
      (item) => item.dependencies?.buildTimePureStatic,
    ).length,
    noRuntimeInputCommandEnvironment: items.filter(
      (item) =>
        !item.dependencies?.input &&
        !item.dependencies?.command &&
        !item.dependencies?.environment,
    ).length,
    input: items.filter((item) => item.dependencies?.input).length,
    command: items.filter((item) => item.dependencies?.command).length,
    environment: items.filter((item) => item.dependencies?.environment).length,
  };
}

function auditIssueCount(errors) {
  return Object.values(errors ?? {}).reduce(
    (count, value) => count + (Array.isArray(value) ? value.length : 0),
    0,
  );
}

async function isRegularFile(path) {
  try {
    const info = await lstat(path);
    return info.isFile();
  } catch {
    return false;
  }
}

/**
 * Inventory of diff-versioned spec behaviour the compiler does not adapt yet.
 *
 * Both lists come from the reviewed allowlist in spec-hook-contract.mjs; the
 * compiler already fails closed when the bundle drifts from it. The version
 * diffs are imported here only to count the functions they carry (hooks the
 * WebView could run after merging a diff). No hook is invoked.
 */
async function versionedSpecInventory(sourceRoot) {
  const selectors = [];
  for (const file of [...KNOWN_VERSION_SELECTORS].sort(comparePath)) {
    selectors.push({
      file,
      present: await isRegularFile(join(sourceRoot, file)),
      resolution: "highest-version-file",
    });
  }
  const unappliedDiffs = [];
  for (const [file, versions] of Object.entries(
    KNOWN_UNAPPLIED_VERSION_DIFFS,
  ).sort(([left], [right]) => comparePath(left, right))) {
    const path = join(sourceRoot, file);
    let namespace = null;
    if (await isRegularFile(path)) {
      try {
        namespace = await import(pathToFileURL(path).href);
      } catch {
        namespace = null;
      }
    }
    unappliedDiffs.push({
      file,
      present: namespace !== null,
      versions: [...versions].map((version) => ({
        version,
        functions:
          namespace === null
            ? null
            : countFunctionsInValue(namespace.versions?.[version]),
      })),
    });
  }
  const diffCount = unappliedDiffs.reduce(
    (sum, entry) => sum + entry.versions.length,
    0,
  );
  const functionCount = unappliedDiffs.reduce(
    (sum, entry) =>
      sum +
      entry.versions.reduce((inner, item) => inner + (item.functions ?? 0), 0),
    0,
  );
  return {
    status: selectors.length || diffCount ? "unadapted" : "none",
    selectors,
    unappliedDiffs,
    totals: {
      selectors: selectors.length,
      unappliedDiffs: diffCount,
      functionsInUnappliedDiffs: functionCount,
    },
  };
}

/**
 * Build the deterministic readiness report for all referenced extracted hooks.
 * No hook body is executed by this function.
 */
async function readBaselineCoverage(group, baselineRoot) {
  const file = join(baselineRoot, group.sourceField, `${group.bodySha256}.json`);
  try {
    const value = JSON.parse(await readFile(file, "utf8"));
    const cases = Array.isArray(value.cases) ? value.cases.length : 0;
    const covered =
      value.field === group.sourceField &&
      value.bodySha256 === group.bodySha256 &&
      cases >= 3;
    return { baselineCovered: covered, baselineCases: cases };
  } catch {
    return { baselineCovered: false, baselineCases: 0 };
  }
}

export function outputBaselineFromCoverage(coveredUniqueBodies, totalUniqueBodies) {
  const covered = coveredUniqueBodies;
  const total = totalUniqueBodies;
  return {
    status: covered === total ? "established" : "partial",
    coveredUniqueBodies: covered,
    totalUniqueBodies: total,
  };
}

export async function classifyNativeHooks({
  sourceRoot = process.env.EC_SPECS_SRC || defaultSourceRoot,
  irRoot = process.env.EC_SPECS_IR || defaultIrRoot,
  hooksRoot = process.env.EC_SPECS_HOOKS || join(irRoot, "hooks"),
  baselineRoot = defaultBaselineRoot,
} = {}) {
  const audit = await auditSpecsHooks({ sourceRoot, irRoot, hooksRoot });
  const locationsById = sourceLocations(audit);
  const manifest = [...(audit.hookManifest ?? [])].sort((left, right) =>
    comparePath(left.id, right.id),
  );
  const hookFilesOnDisk = await walkHookFiles(hooksRoot);
  const manifestFiles = new Set(manifest.map((hook) => hook.file));
  const hooks = [];
  const groupsByHash = new Map();
  const readErrors = [];
  let hookModules = {};
  try {
    hookModules =
      JSON.parse(await readFile(join(irRoot, "hook-modules.json"), "utf8"))
        .hooks ?? {};
  } catch {
    hookModules = {};
  }
  const moduleSourceCache = new Map();

  for (const entry of manifest) {
    const field = entry.field;
    const sourceField = IR_TO_SOURCE_FIELD[field] ?? null;
    if (!sourceField) {
      hooks.push({
        id: entry.id,
        file: entry.file,
        field,
        sourceField: null,
        bodySha256: null,
        status: UNCLASSIFIED_STATUS,
        reasonCodes: ["unknown-field"],
        researchCandidate: false,
        nativeExecutable: false,
        dependencies: {
          input: false,
          command: false,
          environment: false,
          buildTimePureStatic: false,
        },
        locations: locationsById.get(entry.id) ?? [],
      });
      continue;
    }
    let text;
    try {
      text = await readFile(join(hooksRoot, entry.file), "utf8");
    } catch (error) {
      readErrors.push({ file: entry.file, message: error.message });
      hooks.push({
        id: entry.id,
        file: entry.file,
        field,
        sourceField,
        bodySha256: null,
        status: UNCLASSIFIED_STATUS,
        reasonCodes: ["missing-hook-file"],
        researchCandidate: false,
        nativeExecutable: false,
        dependencies: {
          input: false,
          command: false,
          environment: false,
          buildTimePureStatic: false,
        },
        locations: locationsById.get(entry.id) ?? [],
      });
      continue;
    }
    const body = bodyFromHookFile(text);
    const bodySha256 = body == null ? null : sha256(body);
    const moduleName = hookModules[entry.id]?.module;
    let moduleSource;
    if (moduleName) {
      if (!moduleSourceCache.has(moduleName)) {
        try {
          moduleSourceCache.set(
            moduleName,
            await readFile(join(irRoot, "source-modules", moduleName), "utf8"),
          );
        } catch {
          moduleSourceCache.set(moduleName, "");
        }
      }
      moduleSource = moduleSourceCache.get(moduleName) || undefined;
    }
    const analysis = classifyHookBody({ body, field, moduleSource });
    const hook = {
      id: entry.id,
      file: entry.file,
      field,
      sourceField,
      bodySha256,
      status: analysis.status,
      ...(analysis.failureKind ? { failureKind: analysis.failureKind } : {}),
      ...(analysis.analysisError
        ? { analysisError: analysis.analysisError }
        : {}),
      ...(analysis.reasonCodes ? { reasonCodes: analysis.reasonCodes } : {}),
      ...(analysis.unsupportedRuntimeSyntax
        ? { unsupportedRuntimeSyntax: analysis.unsupportedRuntimeSyntax }
        : {}),
      researchCandidate: analysis.researchCandidate === true,
      nativeExecutable: analysis.nativeExecutable === true,
      buildTimePureStatic: analysis.buildTimePureStatic === true,
      freeVariables: analysis.freeVariables ?? [],
      risks: analysis.risks ?? [],
      dependencies: analysis.dependencies,
      metrics: analysis.metrics,
      locations: locationsById.get(entry.id) ?? [],
    };
    hooks.push(hook);
    if (bodySha256) {
      const key = `${field}\0${bodySha256}`;
      const group = groupsByHash.get(key) ?? {
        bodySha256,
        field,
        sourceField,
        hookIds: [],
        statuses: new Set(),
        analyses: [],
      };
      group.hookIds.push(entry.id);
      group.statuses.add(analysis.status);
      group.analyses.push(analysis);
      groupsByHash.set(key, group);
    }
  }

  // A hook file without an IR reference cannot be assigned a field or a
  // native implementation. Keep it visible and fail closed.
  for (const file of hookFilesOnDisk) {
    if (manifestFiles.has(file)) continue;
    hooks.push({
      id: null,
      file,
      field: null,
      sourceField: null,
      bodySha256: null,
      status: UNCLASSIFIED_STATUS,
      reasonCodes: ["orphan-hook-file"],
      researchCandidate: false,
      nativeExecutable: false,
      dependencies: {
        input: false,
        command: false,
        environment: false,
        buildTimePureStatic: false,
      },
      locations: [],
    });
  }

  hooks.sort(
    (left, right) =>
      comparePath(left.id ?? "", right.id ?? "") ||
      comparePath(left.file, right.file),
  );
  const groups = [...groupsByHash.values()]
    .map((group) => {
      const analyses = group.analyses;
      const statuses = [...group.statuses].sort(
        (left, right) => STATUS_ORDER.get(left) - STATUS_ORDER.get(right),
      );
      const analysis = analyses[0];
      return {
        bodySha256: group.bodySha256,
        field: group.field,
        sourceField: group.sourceField,
        hookCount: group.hookIds.length,
        hookIds: [...group.hookIds].sort(comparePath),
        statuses,
        status: statuses.length === 1 ? statuses[0] : UNCLASSIFIED_STATUS,
        ...(statuses.length > 1
          ? { reasonCodes: ["conflicting-body-classification"] }
          : {}),
        researchCandidate:
          analysis.researchCandidate === true && statuses.length === 1,
        nativeExecutable: false,
        buildTimePureStatic:
          analysis.buildTimePureStatic === true && statuses.length === 1,
        freeVariables: analysis.freeVariables ?? [],
        risks: analysis.risks ?? [],
        dependencies: analysis.dependencies,
        metrics: analysis.metrics,
      };
    })
    .sort(
      (left, right) =>
        comparePath(left.field, right.field) ||
        comparePath(left.bodySha256, right.bodySha256),
    );

  // A body hash must have one field and one classification. A collision or a
  // malformed report is not silently collapsed into an adapter bucket.
  const conflictingGroups = groups.filter(
    (group) => group.status === UNCLASSIFIED_STATUS,
  );
  for (const group of conflictingGroups) {
    for (const hook of hooks) {
      if (hook.bodySha256 === group.bodySha256 && hook.field === group.field) {
        hook.status = UNCLASSIFIED_STATUS;
        hook.reasonCodes = ["conflicting-body-classification"];
        hook.researchCandidate = false;
        hook.nativeExecutable = false;
        hook.buildTimePureStatic = false;
      }
    }
  }

  for (const group of groups) {
    Object.assign(group, await readBaselineCoverage(group, baselineRoot));
  }

  const extractedHooks = hooks.filter((hook) => hook.id != null);
  const classifiedHooks = hooks.filter(
    (hook) => hook.status !== UNCLASSIFIED_STATUS,
  );
  const countsByField = emptyFieldStatusCounts();
  for (const hook of extractedHooks) {
    if (!countsByField[hook.field])
      countsByField[hook.field] = emptyStatusCounts();
    countsByField[hook.field][hook.status] += 1;
  }
  const counts = {
    hooks: statusCounts(hooks),
    extractedHooks: statusCounts(extractedHooks),
    uniqueBodies: bodyStatusCounts(groups),
    byField: countsByField,
    dependencies: {
      byHook: countDependencies(extractedHooks),
      byBody: countDependencies(groups),
    },
  };
  const nativeRewrites = nativeRewriteSummary(audit);
  const versionedSpecs = await versionedSpecInventory(sourceRoot);
  const auditErrors = sanitizeAuditErrors(audit.errors, {
    sourceRoot,
    irRoot,
    hooksRoot,
  });
  const structuralErrorCount =
    auditIssueCount(audit.errors) + readErrors.length;
  const unclassifiedCount = hooks.length - classifiedHooks.length;
  const gateBlockers = [];
  if (counts.extractedHooks[ADAPTER_STATUS] > 0)
    gateBlockers.push(ADAPTER_STATUS);
  if (counts.extractedHooks[FAILURE_STATUS] > 0)
    gateBlockers.push(FAILURE_STATUS);
  if (unclassifiedCount > 0) gateBlockers.push(UNCLASSIFIED_STATUS);
  const coveredUniqueBodies = groups.filter((group) => group.baselineCovered).length;
  const outputBaseline = outputBaselineFromCoverage(
    coveredUniqueBodies,
    groups.length,
  );
  if (outputBaseline.status !== "established") {
    gateBlockers.push(OUTPUT_BASELINE_BLOCKER);
  }
  if (structuralErrorCount > 0) gateBlockers.push("audit-errors");
  if (versionedSpecs.status !== "none") gateBlockers.push(VERSIONED_SPEC_BLOCKER);
  const report = {
    version: 1,
    kind: "native-hook-readiness",
    contract: {
      statuses: CLASSIFICATION_STATUSES,
      researchOnlyStatuses: [],
      nativeExecutableStatuses: [NATIVE_FILEPATHS_STATUS],
      failClosedStatuses: [FAILURE_STATUS, UNCLASSIFIED_STATUS],
      outputBaselineRequired: true,
      outputBaselineStatus: outputBaseline.status,
      riskNames: [...RISK_NAMES].sort(comparePath),
      candidatePolicy: {
        noFreeVariables: true,
        noThis: true,
        noAsync: true,
        noCommandOrEnvironment: true,
        allowedNodeTypes: [...CANDIDATE_NODE_TYPES].sort(comparePath),
        pureMethods: [...PURE_METHODS].sort(comparePath),
        complexityLimits: { ...COMPLEXITY_LIMITS },
      },
    },
    coverage: {
      extractedHooks: extractedHooks.length,
      uniqueBodies: groups.length,
      hookFilesOnDisk: hookFilesOnDisk.length,
      classifiedHooks: classifiedHooks.length,
      unclassifiedHooks: unclassifiedCount,
      nativeFilepathsRewrite: nativeRewrites.total,
      auditErrors: structuralErrorCount,
    },
    counts,
    nativeFilepathsRewrites: nativeRewrites,
    versionedSpecs,
    outputBaseline,
    hooks,
    bodyGroups: groups,
    gate: {
      classificationComplete:
        unclassifiedCount === 0 && structuralErrorCount === 0,
      pathSwitchAllowed:
        unclassifiedCount === 0 &&
        structuralErrorCount === 0 &&
        counts.extractedHooks[ADAPTER_STATUS] === 0 &&
        counts.extractedHooks[FAILURE_STATUS] === 0 &&
        counts.uniqueBodies[ADAPTER_STATUS] === 0 &&
        counts.uniqueBodies[FAILURE_STATUS] === 0 &&
        versionedSpecs.status === "none" &&
        outputBaseline.status === "established",
      researchOnlyStatuses: [],
      blockers: sortStrings(gateBlockers),
      allBundledHooksMustPass: true,
      outputBaselineRequired: true,
      outputBaselineEstablished: outputBaseline.status === "established",
    },
    errors: {
      audit: auditErrors,
      read: readErrors.sort((left, right) =>
        comparePath(JSON.stringify(left), JSON.stringify(right)),
      ),
    },
  };
  const canonicalReport = sortObjectKeys(report);
  canonicalReport.reproducibility = {
    algorithm: "sha256",
    reportSha256: sha256(JSON.stringify(canonicalReport)),
  };
  return canonicalReport;
}

function outputPathFromArgs() {
  const index = process.argv.indexOf("--out");
  if (index >= 0 && process.argv[index + 1]) return process.argv[index + 1];
  return process.env.EC_NATIVE_HOOK_CLASSIFICATION_OUT || null;
}

export function compactClassificationReport(report) {
  return {
    version: report.version,
    kind: report.kind,
    coverage: report.coverage,
    counts: report.counts,
    nativeFilepathsRewrites: {
      status: report.nativeFilepathsRewrites.status,
      byField: report.nativeFilepathsRewrites.byField,
      total: report.nativeFilepathsRewrites.total,
      sourceCount: report.nativeFilepathsRewrites.sourceCount,
    },
    versionedSpecs: {
      status: report.versionedSpecs.status,
      totals: report.versionedSpecs.totals,
    },
    gate: report.gate,
    errors: {
      audit: Object.fromEntries(
        Object.entries(report.errors.audit).map(([name, entries]) => [
          name,
          { count: entries.length, samples: entries.slice(0, 3) },
        ]),
      ),
      read: {
        count: report.errors.read.length,
        samples: report.errors.read.slice(0, 3),
      },
    },
    reproducibility: report.reproducibility,
  };
}

/**
 * The committed per-class inventory: every distinct hook body with its field,
 * classification, risks, dependencies and a few representative hook ids, plus
 * the unadapted versioned-spec behaviour and the gate. Per-hook rows and audit
 * error samples are left to the full `--out` report so this stays small
 * enough to review in a diff.
 */
export function inventoryFromReport(report) {
  return sortObjectKeys({
    version: INVENTORY_VERSION,
    kind: INVENTORY_KIND,
    coverage: report.coverage,
    counts: report.counts,
    nativeFilepathsRewrites: {
      byField: report.nativeFilepathsRewrites.byField,
      total: report.nativeFilepathsRewrites.total,
      sourceCount: report.nativeFilepathsRewrites.sourceCount,
    },
    versionedSpecs: report.versionedSpecs,
    outputBaseline: report.outputBaseline,
    gate: report.gate,
    bodyGroups: report.bodyGroups.map((group) => ({
      bodySha256: group.bodySha256,
      field: group.field,
      sourceField: group.sourceField,
      hookCount: group.hookCount,
      sampleHookIds: group.hookIds.slice(0, 3),
      status: group.status,
      ...(group.reasonCodes ? { reasonCodes: group.reasonCodes } : {}),
      risks: group.risks,
      freeVariables: group.freeVariables,
      dependencies: group.dependencies,
      nodeCount: group.metrics?.nodeCount ?? null,
      baselineCovered: group.baselineCovered === true,
      baselineCases: Number.isInteger(group.baselineCases) ? group.baselineCases : 0,
    })),
  });
}

function inventoryText(report) {
  return `${JSON.stringify(inventoryFromReport(report), null, 2)}\n`;
}

export async function checkNativeHookInventory({
  report,
  inventoryPath = defaultInventoryPath,
} = {}) {
  const current = report ?? (await classifyNativeHooks());
  let committed;
  try {
    committed = await readFile(inventoryPath, "utf8");
  } catch (error) {
    throw new Error(
      `native hook inventory is missing at ${inventoryPath}; run \`node scripts/classify-native-hooks.mjs --update\``,
      { cause: error },
    );
  }
  if (committed !== inventoryText(current)) {
    throw new Error(
      `native hook inventory at ${inventoryPath} is stale; run \`node scripts/classify-native-hooks.mjs --update\` and review the diff`,
    );
  }
  return { report: current, inventoryPath };
}

export async function updateNativeHookInventory({
  report,
  inventoryPath = defaultInventoryPath,
} = {}) {
  const current = report ?? (await classifyNativeHooks());
  await writeFile(inventoryPath, inventoryText(current));
  return { report: current, inventoryPath };
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const check = process.argv.includes("--check");
  const update = process.argv.includes("--update");
  if (check && update) {
    process.stderr.write("error: choose at most one of --check or --update\n");
    process.exitCode = 2;
  } else {
    const report = await classifyNativeHooks();
    const outputPath = outputPathFromArgs();
    // Full hook/body entries are deliberately written only when --out (or the
    // equivalent environment variable) is supplied. This keeps a normal CLI
    // invocation reviewable while the exported API retains the full manifest.
    if (outputPath) {
      await writeFile(outputPath, `${JSON.stringify(report, null, 2)}\n`);
    }
    if (update) {
      const { inventoryPath } = await updateNativeHookInventory({ report });
      process.stdout.write(
        `Updated native hook inventory: ${report.coverage.uniqueBodies} bodies / ${report.coverage.extractedHooks} hooks -> ${inventoryPath}\n`,
      );
    } else if (check) {
      try {
        await checkNativeHookInventory({ report });
        process.stdout.write(
          `Verified native hook inventory: ${report.coverage.uniqueBodies} bodies / ${report.coverage.extractedHooks} hooks\n`,
        );
      } catch (error) {
        process.stderr.write(
          `native hook inventory check failed: ${error instanceof Error ? error.message : error}\n`,
        );
        process.exitCode = 1;
      }
    } else {
      const output = compactClassificationReport(report);
      process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
    }
    if (!report.gate.classificationComplete) process.exitCode = 1;
  }
}
