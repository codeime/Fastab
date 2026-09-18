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

function objectPropertyValue(object, name) {
  if (!object || object.type !== "ObjectExpression") return null;
  for (const property of object.properties) {
    if (property.type !== "Property" || property.computed) continue;
    const key =
      property.key.type === "Identifier"
        ? property.key.name
        : property.key.type === "Literal"
          ? property.key.value
          : null;
    if (key === name) return property.value;
  }
  return null;
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
  const values = calls
    .map((call) => argumentForParam(fn, call, name))
    .filter((value) => value && value.type !== "SpreadElement");
  if (values.length === 0) {
    if (parameterLooksLikeObject(fn, name)) return emptyObjectExpression();
    return null;
  }
  if (values.every((value) => value.type === "ObjectExpression")) {
    if (values.length === 1) return values[0];
    return emptyObjectExpression();
  }
  if (
    values.every(
      (value) =>
        value.type === "Literal" &&
        values[0].type === "Literal" &&
        value.value === values[0].value,
    )
  ) {
    return values[0];
  }
  const fromDefault = parameterDefault(fn, name);
  return fromDefault ?? values[0];
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
export function resolveHookHelpers({ body, moduleSource } = {}) {
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
    const def = variable.defs[0];
    const init =
      definitionInit(def) ??
      (def?.type === "Parameter"
        ? resolveFactoryArgument(moduleAst, def, name)
        : null);
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
