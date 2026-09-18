#!/usr/bin/env node
/**
 * Resolve module-level helpers so a extracted hook body can compile as typed IR.
 *
 * A hook's free identifiers are walked through eslint-scope against the
 * closure-preserving source module.  Literal bindings fold; function bindings
 * are returned as AST nodes so the compiler can β-reduce them at a call site.
 * Helpers that mention a host object fail closed.
 */
import * as acorn from "acorn";
import * as eslintScope from "eslint-scope";

export const MAX_INLINE_NODES = 2048;

export const HOST_IDENTIFIERS = Object.freeze([
  "fig",
  "window",
  "process",
  "require",
  "console",
  "Intl",
]);

const HOST_MEMBER = Object.freeze({
  Date: Object.freeze(["now"]),
  Math: Object.freeze(["random"]),
});

export class TypedHookInlineError extends Error {
  constructor(message, { code = "helper-inline" } = {}) {
    super(message);
    this.name = "TypedHookInlineError";
    this.code = code;
  }
}

function fail(message, code = "helper-inline") {
  throw new TypedHookInlineError(message, { code });
}

function walkAst(node, visit) {
  if (!node || typeof node !== "object") return;
  visit(node);
  for (const key of Object.keys(node)) {
    if (key === "start" || key === "end" || key === "loc" || key === "range") {
      continue;
    }
    const value = node[key];
    if (Array.isArray(value)) {
      for (const child of value) walkAst(child, visit);
    } else if (value && typeof value === "object" && typeof value.type === "string") {
      walkAst(value, visit);
    }
  }
}

function countNodes(node) {
  let count = 0;
  walkAst(node, () => {
    count += 1;
  });
  return count;
}

export function helperReferencesHost(node) {
  let host = null;
  walkAst(node, (child) => {
    if (host) return;
    if (child.type === "Identifier" && HOST_IDENTIFIERS.includes(child.name)) {
      host = child.name;
    }
    if (
      child.type === "MemberExpression" &&
      !child.computed &&
      child.object?.type === "Identifier" &&
      child.property?.type === "Identifier" &&
      HOST_MEMBER[child.object.name]?.includes(child.property.name)
    ) {
      host = `${child.object.name}.${child.property.name}`;
    }
  });
  return host;
}

function normalizeFunctionSource(source) {
  return String(source)
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\/\/[^\n]*/g, "")
    .replace(/\s+/g, "");
}

function parseModule(moduleSource) {
  if (typeof moduleSource !== "string" || !moduleSource.trim()) {
    fail("module source is required to inline helpers", "input");
  }
  try {
    return acorn.parse(moduleSource, {
      ecmaVersion: "latest",
      sourceType: "module",
      ranges: true,
    });
  } catch (error) {
    fail(`module source is not valid JavaScript: ${error.message}`, "syntax");
  }
}

function parseHook(body) {
  try {
    return acorn.parse(`(${body})`, {
      ecmaVersion: "latest",
      sourceType: "module",
      ranges: true,
    });
  } catch (error) {
    fail(`hook body is not valid JavaScript: ${error.message}`, "syntax");
  }
}

function unwrapHookFunction(ast) {
  const statement = ast.body?.[0];
  const expression = statement?.type === "ExpressionStatement" ? statement.expression : null;
  if (
    expression?.type === "ArrowFunctionExpression" ||
    expression?.type === "FunctionExpression"
  ) {
    return expression;
  }
  fail("hook must be an arrow or function expression", "function-shape");
}

function functionNodes(ast) {
  const nodes = [];
  walkAst(ast, (node) => {
    if (
      node.type === "ArrowFunctionExpression" ||
      node.type === "FunctionExpression" ||
      node.type === "FunctionDeclaration"
    ) {
      nodes.push(node);
    }
  });
  return nodes;
}

function findHookOccurrences(moduleAst, moduleSource, body) {
  const wanted = normalizeFunctionSource(body);
  return functionNodes(moduleAst).filter((node) => {
    const slice = moduleSource.slice(node.start, node.end);
    return normalizeFunctionSource(slice) === wanted;
  });
}

function enclosingFunction(node, parents) {
  for (let index = parents.length - 1; index >= 0; index -= 1) {
    const parent = parents[index];
    if (
      parent.type === "FunctionDeclaration" ||
      parent.type === "FunctionExpression" ||
      parent.type === "ArrowFunctionExpression"
    ) {
      return parent;
    }
  }
  return null;
}

function objectPatternDefault(pattern, name) {
  if (pattern.type !== "ObjectPattern") return null;
  for (const property of pattern.properties) {
    if (property.type !== "Property") continue;
    const value = property.value;
    if (
      value.type === "AssignmentPattern" &&
      value.left.type === "Identifier" &&
      value.left.name === name
    ) {
      return value.right;
    }
  }
  return null;
}

function parameterDefault(fn, name) {
  if (!fn?.params) return null;
  for (const param of fn.params) {
    if (
      param.type === "AssignmentPattern" &&
      param.left.type === "Identifier" &&
      param.left.name === name
    ) {
      return param.right;
    }
    const fromObject = objectPatternDefault(param, name);
    if (fromObject) return fromObject;
    if (param.type === "AssignmentPattern") {
      const nested = objectPatternDefault(param.left, name);
      if (nested) return nested;
    }
  }
  return null;
}

function isFunctionNode(node) {
  return (
    node &&
    (node.type === "FunctionDeclaration" ||
      node.type === "FunctionExpression" ||
      node.type === "ArrowFunctionExpression")
  );
}

function definitionFunction(def) {
  if (!def) return null;
  if (isFunctionNode(def.parent)) return def.parent;
  if (isFunctionNode(def.node)) return def.node;
  return null;
}

function definitionName(def) {
  if (typeof def?.name === "string") return def.name;
  if (def?.name?.type === "Identifier") return def.name.name;
  if (def?.node?.type === "Identifier") return def.node.name;
  return null;
}

function definitionInit(def) {
  if (!def) return null;
  if (def.type === "Variable") {
    if (def.node?.init) return def.node.init;
    if (def.parent?.type === "VariableDeclarator" && def.parent.init) {
      return def.parent.init;
    }
    return null;
  }
  if (def.type === "FunctionName") {
    return def.node ?? def.parent;
  }
  if (def.type === "Parameter") {
    const fn = definitionFunction(def);
    const name = definitionName(def);
    return parameterDefault(fn, name);
  }
  return null;
}

function functionBindingNames(moduleAst, fn) {
  const names = new Set();
  if (fn.id?.name) names.add(fn.id.name);
  walkAst(moduleAst, (node) => {
    if (
      node.type === "VariableDeclarator" &&
      node.init === fn &&
      node.id.type === "Identifier"
    ) {
      names.add(node.id.name);
    }
    if (node.type === "AssignmentExpression" && node.right === fn) {
      if (node.left.type === "Identifier") names.add(node.left.name);
      if (
        node.left.type === "MemberExpression" &&
        !node.left.computed &&
        node.left.property.type === "Identifier"
      ) {
        names.add(node.left.property.name);
      }
    }
  });
  return names;
}

function calleeNames(node) {
  if (node.type === "Identifier") return [node.name];
  if (node.type === "MemberExpression" && !node.computed && node.property.type === "Identifier") {
    return [node.property.name];
  }
  if (
    node.type === "SequenceExpression" &&
    node.expressions.length > 0
  ) {
    return calleeNames(node.expressions[node.expressions.length - 1]);
  }
  return [];
}

function objectPropertyKey(property) {
  if (!property || property.type !== "Property" || property.computed) return null;
  if (property.key.type === "Identifier") return property.key.name;
  if (property.key.type === "Literal") return property.key.value;
  return null;
}

function objectPropertyValue(object, name) {
  if (!object || object.type !== "ObjectExpression") return null;
  for (const property of object.properties) {
    if (objectPropertyKey(property) === name) return property.value;
  }
  return null;
}

function astNodesAgree(left, right) {
  if (!left || !right || left.type !== right.type) return false;
  if (left.type === "Literal") {
    return Object.is(left.value, right.value);
  }
  if (left.type === "Identifier") {
    return left.name === right.name;
  }
  if (left.type === "ArrayExpression") {
    return (
      left.elements.length === right.elements.length &&
      left.elements.every((item, index) => astNodesAgree(item, right.elements[index]))
    );
  }
  if (left.type === "ObjectExpression") {
    if (left.properties.length !== right.properties.length) return false;
    return left.properties.every((property) => {
      const key = objectPropertyKey(property);
      return key != null && astNodesAgree(property.value, objectPropertyValue(right, key));
    });
  }
  if (left.type === "UnaryExpression") {
    return left.operator === right.operator && astNodesAgree(left.argument, right.argument);
  }
  return false;
}

function agreeObjectLiterals(objects) {
  if (!Array.isArray(objects) || objects.length === 0) return emptyObjectExpression();
  if (objects.length === 1) return objects[0];
  const keys = [];
  for (const property of objects[0].properties) {
    const key = objectPropertyKey(property);
    if (key == null || keys.includes(key)) continue;
    keys.push(key);
  }
  const properties = [];
  for (const key of keys) {
    const values = objects.map((object) => objectPropertyValue(object, key));
    if (values.some((value) => !value)) continue;
    if (!values.every((value) => astNodesAgree(value, values[0]))) continue;
    properties.push({
      type: "Property",
      start: 0,
      end: 0,
      method: false,
      shorthand: false,
      computed: false,
      kind: "init",
      key: { type: "Identifier", start: 0, end: 0, name: String(key) },
      value: values[0],
    });
  }
  return {
    type: "ObjectExpression",
    start: 0,
    end: 0,
    properties,
  };
}

function argumentForParam(fn, call, name) {
  if (!fn?.params) return null;
  for (let index = 0; index < fn.params.length; index += 1) {
    const param = fn.params[index];
    if (param.type === "Identifier" && param.name === name) {
      return call.arguments[index] ?? parameterDefault(fn, name);
    }
    if (param.type === "AssignmentPattern" && param.left.type === "Identifier" && param.left.name === name) {
      return call.arguments[index] ?? param.right;
    }
    const object =
      param.type === "ObjectPattern"
        ? param
        : param.type === "AssignmentPattern" && param.left.type === "ObjectPattern"
          ? param.left
          : null;
    if (object) {
      const argument = call.arguments[index] ?? param.right ?? { type: "ObjectExpression", properties: [] };
      const fromObject = objectPropertyValue(argument, objectPatternKey(object, name));
      if (fromObject) return fromObject;
      const fallback = objectPatternDefault(object, name);
      if (fallback) return fallback;
    }
  }
  return null;
}

function objectPatternKey(pattern, name) {
  for (const property of pattern.properties) {
    if (property.type !== "Property") continue;
    const value = property.value;
    const bound =
      value.type === "Identifier"
        ? value.name
        : value.type === "AssignmentPattern" && value.left.type === "Identifier"
          ? value.left.name
          : null;
    if (bound === name) {
      return property.key.type === "Identifier" ? property.key.name : property.key.value;
    }
  }
  return name;
}

function emptyObjectExpression() {
  return { type: "ObjectExpression", properties: [], start: 0, end: 2 };
}

function undefinedLiteral() {
  return { type: "Literal", start: 0, end: 4, value: null, raw: "null" };
}

function resolveFactoryArgument(moduleAst, def, name) {
  const fn = definitionFunction(def);
  if (!fn) return null;
  const names = functionBindingNames(moduleAst, fn);
  const calls = [];
  walkAst(moduleAst, (node) => {
    if (node.type !== "CallExpression") return;
    const called = calleeNames(node.callee);
    if (called.some((callee) => names.has(callee))) calls.push(node);
  });
  const fromDefault = parameterDefault(fn, name) ?? undefinedLiteral();
  if (calls.length === 0) {
    return fromDefault;
  }
  const values = calls.map((call) => {
    const argument = argumentForParam(fn, call, name);
    if (!argument || argument.type === "SpreadElement") return undefinedLiteral();
    return argument;
  });
  if (values.every((value) => value.type === "ObjectExpression")) {
    return agreeObjectLiterals(values);
  }
  const objectValues = values.filter((value) => value.type === "ObjectExpression");
  if (
    objectValues.length > 0 &&
    values.every((value) => value.type === "ObjectExpression" || isNullishAst(value))
  ) {
    return agreeObjectLiterals(objectValues);
  }
  if (values.every((value) => astNodesAgree(value, values[0]))) {
    return values[0];
  }
  return fromDefault;
}

function isNullishAst(node) {
  return (
    !node ||
    (node.type === "Identifier" && node.name === "undefined") ||
    (node.type === "Literal" && (node.value === null || node.value === undefined)) ||
    (node.type === "UnaryExpression" && node.operator === "void")
  );
}

function parameterLooksLikeObject(fn, name) {
  if (!fn?.params) return false;
  return fn.params.some((param) => {
    const object =
      param.type === "ObjectPattern"
        ? param
        : param.type === "AssignmentPattern" && param.left.type === "ObjectPattern"
          ? param.left
          : null;
    if (!object) return false;
    return object.properties.some((property) => {
      if (property.type !== "Property") return false;
      const value = property.value;
      return (
        (value.type === "Identifier" && value.name === name) ||
        (value.type === "AssignmentPattern" &&
          value.left.type === "Identifier" &&
          value.left.name === name)
      );
    });
  });
}

function analyzeScope(ast) {
  return eslintScope.analyze(ast, {
    ecmaVersion: 2024,
    sourceType: "module",
  });
}

function scopeForNode(scopeManager, node) {
  return (
    scopeManager.acquire(node) ||
    scopeManager.acquire(node, true) ||
    (node.type === "Program" ? scopeManager.globalScope : null)
  );
}

function resolveThroughScope(scope, name) {
  let current = scope;
  while (current) {
    const variable = current.set.get(name);
    if (variable) return variable;
    current = current.upper;
  }
  return null;
}

const KNOWN_BUILTINS = new Set([
  "undefined",
  "NaN",
  "Infinity",
  "Boolean",
  "Number",
  "String",
  "JSON",
  "Object",
  "Math",
  "Set",
  "Map",
  "Array",
  "RegExp",
  "Error",
  "console",
]);

function isPropertyName(node, parent) {
  if (!parent) return false;
  if (
    parent.type === "MemberExpression" &&
    parent.property === node &&
    !parent.computed
  ) {
    return true;
  }
  if (
    parent.type === "Property" &&
    parent.key === node &&
    !parent.computed
  ) {
    return true;
  }
  if (parent.type === "LabeledStatement" && parent.label === node) return true;
  return false;
}

function hookFreeNames(body) {
  const ast = parseHook(body);
  const fn = unwrapHookFunction(ast);
  const scopeManager = analyzeScope(ast);
  const scope =
    scopeManager.acquire(fn) ||
    scopeManager.acquire(fn, true) ||
    scopeManager.globalScope;
  const names = new Set();
  for (const ref of scope.through) {
    const name = ref.identifier?.name;
    if (!name || KNOWN_BUILTINS.has(name)) continue;
    names.add(name);
  }
  return [...names];
}

function helperRecord(name, init, moduleSource) {
  if (!init) {
    fail(`helper ${name} has no statically visible initializer`, "free-variable");
  }
  const host = helperReferencesHost(init);
  if (host) {
    fail(`helper ${name} references host object ${host}`, "host-object");
  }
  if (isFunctionNode(init)) {
    const recursive = (() => {
      let found = false;
      walkAst(init.body, (node) => {
        if (node.type === "Identifier" && node.name === name) found = true;
      });
      return found;
    })();
    if (recursive) {
      fail(`helper ${name} is recursive`, "recursive-helper");
    }
    return {
      kind: "function",
      name,
      node: init,
      source: moduleSource.slice(init.start, init.end),
      nodes: countNodes(init),
    };
  }
  return {
    kind: "expr",
    name,
    node: init,
    source: moduleSource.slice(init.start, init.end),
    nodes: countNodes(init),
  };
}

function topLevelFunctionScope(moduleAst, scopeManager) {
  const programScope = scopeManager.globalScope;
  const iife = moduleAst.body?.find(
    (statement) =>
      statement.type === "ExportDefaultDeclaration" &&
      statement.declaration?.type === "CallExpression" &&
      (statement.declaration.callee.type === "FunctionExpression" ||
        statement.declaration.callee.type === "ArrowFunctionExpression"),
  );
  if (iife) {
    return (
      scopeManager.acquire(iife.declaration.callee) ||
      scopeManager.acquire(iife.declaration.callee, true) ||
      programScope
    );
  }
  return programScope;
}

/**
 * Resolve the free identifiers of `body` against `moduleSource`.
 *
 * @returns {{ helpers: Map<string, object>, inlinedNodes: number }}
 */
export function resolveHookHelpers({ body, moduleSource, helperLiterals } = {}) {
  if (typeof body !== "string" || !body.trim()) {
    fail("hook body must be a non-empty string", "input");
  }
  if (!moduleSource) {
    return { helpers: new Map(), inlinedNodes: 0 };
  }
  const names = hookFreeNames(body);
  if (names.length === 0) {
    return { helpers: new Map(), inlinedNodes: 0 };
  }
  const moduleAst = parseModule(moduleSource);
  const scopeManager = analyzeScope(moduleAst);
  const occurrences = findHookOccurrences(moduleAst, moduleSource, body);
  const helpers = new Map();
  let inlinedNodes = 0;

  const bindName = (name, variable) => {
    if (!variable || helpers.has(name)) return;
    if (helperLiterals && Object.hasOwn(helperLiterals, name)) {
      const value = helperLiterals[name];
      const usable =
        typeof value === "string" ||
        typeof value === "boolean" ||
        (typeof value === "number" && Number.isSafeInteger(value)) ||
        value === null ||
        (value && typeof value === "object" && !Array.isArray(value));
      if (usable) {
        const record = helperRecord(name, plainToAst(value), JSON.stringify(value));
        helpers.set(name, record);
        return;
      }
    }
    const def = variable.defs[0];
    const init =
      def?.type === "Parameter"
        ? (resolveFactoryArgument(moduleAst, def, name) ?? definitionInit(def))
        : definitionInit(def);
    const record = helperRecord(name, init, moduleSource);
    inlinedNodes += record.nodes;
    if (inlinedNodes > MAX_INLINE_NODES) {
      fail("inlined helper AST exceeds the 2048-node cap", "complexity");
    }
    helpers.set(name, record);
  };

  if (occurrences.length > 0) {
    const hookNode = occurrences[0];
    const scope =
      scopeForNode(scopeManager, hookNode) ||
      scopeForNode(scopeManager, hookNode.body) ||
      scopeManager.globalScope;
    for (const name of names) {
      const variable = resolveThroughScope(scope, name);
      if (!variable) {
        fail(`free identifier ${name} is not representable`, "free-variable");
      }
      bindName(name, variable);
    }
    return { helpers, inlinedNodes };
  }

  const fallbackScope = topLevelFunctionScope(moduleAst, scopeManager);
  for (const name of names) {
    const variable = resolveThroughScope(fallbackScope, name);
    if (!variable) {
      fail(`free identifier ${name} is not representable`, "free-variable");
    }
    bindName(name, variable);
  }
  return { helpers, inlinedNodes };
}

const COMMON_SEPARATORS = Object.freeze([":", "=", "/", ",", "."]);
const COMMON_EXTRAS = Object.freeze([{ isDangerous: true }]);

function objectPatternKeyForParam(fn, name) {
  if (!fn?.params) return null;
  for (const param of fn.params) {
    const object =
      param.type === "ObjectPattern"
        ? param
        : param.type === "AssignmentPattern" && param.left.type === "ObjectPattern"
          ? param.left
          : null;
    if (!object) continue;
    const key = objectPatternKey(object, name);
    if (key != null) return key;
  }
  return null;
}

function objectExpressionToPlain(node) {
  if (!node || node.type !== "ObjectExpression") return undefined;
  const out = {};
  for (const property of node.properties) {
    if (property.type !== "Property" || property.computed) return undefined;
    const key = objectPropertyKey(property);
    const value = literalFactoryValue(property.value);
    if (key == null || value === undefined) return undefined;
    out[key] = value;
  }
  return out;
}

function literalNode(value) {
  return {
    type: "Literal",
    start: 0,
    end: 0,
    value,
    raw: JSON.stringify(value),
  };
}

function plainToAst(value) {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return {
      type: "ObjectExpression",
      start: 0,
      end: 0,
      properties: Object.entries(value).map(([key, child]) => ({
        type: "Property",
        start: 0,
        end: 0,
        method: false,
        shorthand: false,
        computed: false,
        kind: "init",
        key: { type: "Identifier", start: 0, end: 0, name: key },
        value: plainToAst(child),
      })),
    };
  }
  return literalNode(value);
}

function literalFactoryValue(node) {
  if (!node) return undefined;
  if (node.type === "Literal") {
    if (
      typeof node.value === "string" ||
      typeof node.value === "boolean" ||
      node.value === null ||
      (typeof node.value === "number" && Number.isSafeInteger(node.value))
    ) {
      return node.value;
    }
    return undefined;
  }
  if (
    node.type === "UnaryExpression" &&
    node.operator === "!" &&
    node.argument?.type === "Literal" &&
    (typeof node.argument.value === "number" || typeof node.argument.value === "boolean")
  ) {
    return !node.argument.value;
  }
  if (node.type === "UnaryExpression" && node.operator === "void") {
    return null;
  }
  if (node.type === "Identifier" && node.name === "undefined") {
    return null;
  }
  return objectExpressionToPlain(node);
}

function collectFactoryLiterals(moduleAst, fn, name) {
  const values = [];
  const bindingNames = fn ? functionBindingNames(moduleAst, fn) : new Set();
  if (fn) {
    walkAst(moduleAst, (node) => {
      if (node.type !== "CallExpression") return;
      const called = calleeNames(node.callee);
      if (!called.some((callee) => bindingNames.has(callee))) return;
      const argument = argumentForParam(fn, node, name);
      const value = literalFactoryValue(argument);
      if (value !== undefined) values.push(value);
    });
  }
  const patternKey = objectPatternKeyForParam(fn, name);
  if (patternKey) {
    walkAst(moduleAst, (node) => {
      if (node.type !== "ObjectExpression") return;
      const argument = objectPropertyValue(node, patternKey);
      const value = literalFactoryValue(argument);
      if (value !== undefined) values.push(value);
    });
  }
  const fromDefault = literalFactoryValue(parameterDefault(fn, name));
  if (fromDefault !== undefined) values.push(fromDefault);
  return values;
}

function uniquePreserve(values) {
  const seen = new Set();
  const unique = [];
  for (const value of values) {
    const key =
      value && typeof value === "object"
        ? `obj:${JSON.stringify(value)}`
        : `${typeof value}:${String(value)}`;
    if (seen.has(key)) continue;
    seen.add(key);
    unique.push(value);
  }
  return unique;
}

export function factoryHelperCandidates({ body, moduleSource } = {}) {
  if (!moduleSource) return {};
  const names = hookFreeNames(body);
  if (names.length === 0) return {};
  const moduleAst = parseModule(moduleSource);
  const scopeManager = analyzeScope(moduleAst);
  const occurrences = findHookOccurrences(moduleAst, moduleSource, body);
  const hookNode = occurrences[0];
  const scope = hookNode
    ? scopeForNode(scopeManager, hookNode) ||
      scopeForNode(scopeManager, hookNode.body) ||
      scopeManager.globalScope
    : topLevelFunctionScope(moduleAst, scopeManager);
  const candidates = {};
  for (const name of names) {
    const variable = resolveThroughScope(scope, name);
    const def = variable?.defs?.[0];
    if (def?.type !== "Parameter") continue;
    const fn = definitionFunction(def);
    const unique = uniquePreserve(collectFactoryLiterals(moduleAst, fn, name));
    const strings = unique.filter((value) => typeof value === "string");
    const objectLike = parameterLooksLikeObject(fn, name);
    const boolLike =
      unique.some((value) => typeof value === "boolean" || value === null) ||
      (objectLike && strings.length === 0);
    if (strings.some((value) => value.length === 1) || (objectLike && strings.length > 0)) {
      for (const extra of COMMON_SEPARATORS) {
        if (!strings.includes(extra)) unique.push(extra);
      }
    }
    if (objectLike) {
      for (const extra of COMMON_EXTRAS) {
        if (!unique.some((value) => JSON.stringify(value) === JSON.stringify(extra))) {
          unique.unshift(extra);
        }
      }
    }
    if (boolLike || unique.length === 0) {
      for (const extra of [false, true, null]) {
        if (!unique.some((value) => Object.is(value, extra))) unique.push(extra);
      }
    }
    if (unique.length === 0) {
      unique.push(...COMMON_SEPARATORS);
    }
    const objects = unique.filter((value) => value && typeof value === "object");
    const rest = unique.filter((value) => !(value && typeof value === "object"));
    candidates[name] = uniquePreserve([...objects, ...rest]);
  }
  return candidates;
}

export function factoryStringCandidates({ body, moduleSource } = {}) {
  const candidates = factoryHelperCandidates({ body, moduleSource });
  const strings = {};
  for (const [name, values] of Object.entries(candidates)) {
    const only = values.filter((value) => typeof value === "string");
    if (only.length > 0) strings[name] = only;
  }
  return strings;
}

export function inlineHookHelpers({ body, moduleSource } = {}) {
  const resolved = resolveHookHelpers({ body, moduleSource });
  return { body, ...resolved };
}

export {
  countNodes,
  findHookOccurrences,
  hookFreeNames,
  parseModule,
  walkAst,
};
