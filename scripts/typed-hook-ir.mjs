#!/usr/bin/env node
/**
 * Build the small, closed expression language used by native hook adapters.
 *
 * This module is intentionally independent from the spec compiler.  It does
 * not evaluate a hook and it does not know command names or hook ids.  A
 * caller gives it one function expression; if every node is representable in
 * the typed language it returns deterministic JSON-shaped data.  Otherwise it
 * throws so the caller can keep the hook on the compatibility path.
 */
import * as acorn from "acorn";

import {
  TypedHookInlineError,
  resolveHookHelpers,
} from "./typed-hook-inline.mjs";
import {
  assertTypedRegexLiteral,
  regexMatch,
  regexMatchAll,
  regexReplace,
  regexTest,
  stringSplitRegex,
  typedRegexFromAcornLiteral,
} from "./typed-regex.mjs";

export const TYPED_HOOK_IR_VERSION = 1;
export const TYPED_HOOK_IR_KIND = "typed-hook-expression";

export const TYPED_VALUE_TYPES = Object.freeze([
  "string",
  "bool",
  "integer",
  "string-array",
  "json",
  "json-array",
  "suggestion",
  "suggestion-array",
  "string-record",
  "string-set",
  "value-array",
  "regex",
  "null",
]);

export const TYPED_HOOK_CONTRACTS = Object.freeze({
  trigger: Object.freeze({
    params: Object.freeze(["string", "string"]),
    resultType: "bool",
  }),
  getQueryTerm: Object.freeze({
    params: Object.freeze(["string"]),
    resultType: "string",
  }),
  postProcess: Object.freeze({
    params: Object.freeze(["string", "string-array"]),
    resultType: "suggestion-array",
  }),
  script: Object.freeze({
    params: Object.freeze(["string-array"]),
    resultType: "string-array",
  }),
  filterTemplateSuggestions: Object.freeze({
    params: Object.freeze(["suggestion-array"]),
    resultType: "suggestion-array",
  }),
});

export const TYPED_HOOK_SIDECAR_FIELDS = Object.freeze(
  Object.keys(TYPED_HOOK_CONTRACTS),
);

/** Sidecar `contracts` block: every T2.3 side-effect-free field. */
export function typedHookSidecarContracts() {
  return Object.fromEntries(
    Object.entries(TYPED_HOOK_CONTRACTS).map(([field, contract]) => [
      field,
      {
        irVersion: TYPED_HOOK_IR_VERSION,
        params: [...contract.params],
        resultType: contract.resultType,
      },
    ]),
  );
}

/** Compile or return `null` when the body is outside the typed language. */
export function tryCompileTypedHook(options) {
  try {
    return compileTypedHook(options);
  } catch (error) {
    if (error instanceof TypedHookCompileError) return null;
    throw error;
  }
}

export const TYPED_EXPRESSION_OPERATIONS = Object.freeze([
  "arg",
  "string",
  "bool",
  "integer",
  "null",
  "array",
  "length",
  "string-includes",
  "string-index-of",
  "string-last-index-of",
  "string-slice",
  "string-slice-range",
  "string-slice-after-first",
  "string-substring",
  "string-split",
  "string-trim",
  "string-trim-start",
  "string-trim-end",
  "string-replace",
  "string-replace-all",
  "string-starts-with",
  "string-ends-with",
  "string-to-lower",
  "string-to-upper",
  "string-pad-start",
  "string-pad-end",
  "string-repeat",
  "string-concat",
  "string-char-at",
  "string-at",
  "array-includes",
  "strict-eq",
  "strict-ne",
  "add",
  "sub",
  "mul",
  "lt",
  "le",
  "gt",
  "ge",
  "not",
  "nullish",
  "and",
  "or",
  "if",
  "lambda",
  "var",
  "let",
  "block",
  "return",
  "catch-return",
  "break",
  "continue",
  "try",
  "for-of",
  "assign-var",
  "assign-prop",
  "seq",
  "truthy",
  "loose-eq",
  "loose-ne",
  "typeof",
  "object",
  "spread",
  "object-assign",
  "get",
  "object-keys",
  "object-entries",
  "array-map",
  "array-filter",
  "array-flat-map",
  "array-slice",
  "array-join",
  "array-some",
  "array-every",
  "array-find",
  "array-find-index",
  "array-index-of",
  "array-concat",
  "array-reverse",
  "array-sort",
  "array-index",
  "array-push",
  "array-pop",
  "array-entries",
  "json-parse",
  "json-get",
  "json-array-items",
  "json-as-string",
  "json-as-number",
  "json-as-bool",
  "regex-test",
  "regex-match",
  "regex-match-all",
  "regex-replace",
  "string-split-regex",
  "math-max",
  "string-set",
  "regex",
  "while",
  "array-flat",
  "json-object",
  "to-string",
  "json-stringify",
  "locale-compare",
  "array-from",
  "string-split-limit",
  "array-shift",
  "string-index-of-from",
  "array-index-of-from",
  "string-set-add",
  "object-values",
  "regex-search",
]);

const TYPE = Object.freeze({
  STRING: "string",
  BOOL: "bool",
  INTEGER: "integer",
  STRING_ARRAY: "string-array",
  JSON: "json",
  JSON_ARRAY: "json-array",
  SUGGESTION: "suggestion",
  SUGGESTION_ARRAY: "suggestion-array",
  STRING_RECORD: "string-record",
  STRING_SET: "string-set",
  VALUE_ARRAY: "value-array",
  REGEX: "regex",
  NULL: "null",
});

const SUGGESTION_KEYS = Object.freeze([
  "name",
  "displayName",
  "insertValue",
  "description",
  "icon",
  "priority",
  "hidden",
  "isDangerous",
  "type",
  "args",
  "replaceValue",
  "deprecated",
]);

const BUILTIN_IDENTIFIERS = Object.freeze({
  undefined: { type: TYPE.NULL, expr: { op: "null" } },
  NaN: null,
  Infinity: null,
  Boolean: { kind: "boolean-fn" },
  Number: { kind: "number-fn" },
  String: { kind: "string-fn" },
  parseInt: { kind: "number-fn" },
  Array: { kind: "array-ns" },
  JSON: { kind: "json-ns" },
  Object: { kind: "object-ns" },
  Math: { kind: "math-ns" },
  Set: { kind: "set-ctor" },
});

export const MAX_NODES = 512;
export const MAX_DEPTH = 24;
const MAX_SOURCE_BYTES = 128 * 1024;
const MAX_STRING_CODE_UNITS = 32 * 1024;
const MAX_SERIALIZED_IR_BYTES = 256 * 1024;

// `integer` means a JavaScript safe integer, not an arbitrary Rust i64.  A
// native evaluator must reject an input or intermediate result outside this
// range (including `add`) rather than silently diverging from Number.
export const MIN_SAFE_INTEGER = Number.MIN_SAFE_INTEGER;
export const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;

const OP_KEYS = Object.freeze({
  arg: ["op", "index"],
  string: ["op", "value"],
  bool: ["op", "value"],
  integer: ["op", "value"],
  null: ["op"],
  array: ["op", "items"],
  length: ["op", "value"],
  "string-includes": ["op", "value", "needle"],
  "string-index-of": ["op", "value", "needle"],
  "string-last-index-of": ["op", "value", "needle"],
  "string-slice": ["op", "value", "start"],
  "string-slice-range": ["op", "value", "start", "end"],
  "string-slice-after-first": ["op", "value", "needle"],
  "string-substring": ["op", "value", "start", "end"],
  "string-split": ["op", "value", "separator"],
  "string-trim": ["op", "value"],
  "string-trim-start": ["op", "value"],
  "string-trim-end": ["op", "value"],
  "string-replace": ["op", "value", "needle", "replacement"],
  "string-replace-all": ["op", "value", "needle", "replacement"],
  "string-starts-with": ["op", "value", "needle"],
  "string-ends-with": ["op", "value", "needle"],
  "string-to-lower": ["op", "value"],
  "string-to-upper": ["op", "value"],
  "string-pad-start": ["op", "value", "target", "pad"],
  "string-pad-end": ["op", "value", "target", "pad"],
  "string-repeat": ["op", "value", "count"],
  "string-concat": ["op", "parts"],
  "string-char-at": ["op", "value", "index"],
  "string-at": ["op", "value", "index"],
  "array-includes": ["op", "value", "needle"],
  "strict-eq": ["op", "left", "right"],
  "strict-ne": ["op", "left", "right"],
  add: ["op", "left", "right"],
  sub: ["op", "left", "right"],
  mul: ["op", "left", "right"],
  lt: ["op", "left", "right"],
  le: ["op", "left", "right"],
  gt: ["op", "left", "right"],
  ge: ["op", "left", "right"],
  not: ["op", "value"],
  nullish: ["op", "left", "right"],
  and: ["op", "left", "right"],
  or: ["op", "left", "right"],
  if: ["op", "condition", "then", "else"],
  lambda: ["op", "params", "body"],
  var: ["op", "name"],
  let: ["op", "name", "value", "body"],
  block: ["op", "items"],
  return: ["op", "value"],
  "catch-return": ["op", "body"],
  break: ["op"],
  continue: ["op"],
  try: ["op", "body", "catch"],
  "for-of": ["op", "names", "value", "body"],
  "assign-var": ["op", "name", "value"],
  "assign-prop": ["op", "object", "key", "value"],
  seq: ["op", "items"],
  truthy: ["op", "value"],
  "loose-eq": ["op", "left", "right"],
  "loose-ne": ["op", "left", "right"],
  typeof: ["op", "value"],
  object: ["op", "fields"],
  spread: ["op", "value", "fields"],
  "object-assign": ["op", "parts"],
  get: ["op", "value", "key"],
  "object-keys": ["op", "value"],
  "object-entries": ["op", "value"],
  "array-map": ["op", "value", "fn"],
  "array-filter": ["op", "value", "fn"],
  "array-flat-map": ["op", "value", "fn"],
  "array-slice": ["op", "value", "start", "end"],
  "array-join": ["op", "value", "separator"],
  "array-some": ["op", "value", "fn"],
  "array-every": ["op", "value", "fn"],
  "array-find": ["op", "value", "fn"],
  "array-find-index": ["op", "value", "fn"],
  "array-index-of": ["op", "value", "needle"],
  "array-concat": ["op", "parts"],
  "array-reverse": ["op", "value"],
  "array-sort": ["op", "value", "fn"],
  "array-index": ["op", "value", "index"],
  "array-push": ["op", "name", "item"],
  "array-pop": ["op", "value"],
  "array-entries": ["op", "value"],
  "json-parse": ["op", "value"],
  "json-get": ["op", "value", "key"],
  "json-array-items": ["op", "value"],
  "json-as-string": ["op", "value"],
  "json-as-number": ["op", "value"],
  "json-as-bool": ["op", "value"],
  "regex-test": ["op", "value", "pattern", "flags"],
  "regex-match": ["op", "value", "pattern", "flags"],
  "regex-match-all": ["op", "value", "pattern", "flags"],
  "regex-replace": ["op", "value", "pattern", "flags", "replacement"],
  "string-split-regex": ["op", "value", "pattern", "flags"],
  "math-max": ["op", "values"],
  "string-set": ["op", "value"],
  regex: ["op", "pattern", "flags"],
  while: ["op", "condition", "body"],
  "array-flat": ["op", "value", "depth"],
  "json-object": ["op", "fields"],
  "to-string": ["op", "value"],
  "json-stringify": ["op", "value"],
  "locale-compare": ["op", "left", "right"],
  "array-from": ["op", "value"],
  "string-split-limit": ["op", "value", "separator", "limit"],
  "array-shift": ["op", "name"],
  "string-index-of-from": ["op", "value", "needle", "start"],
  "array-index-of-from": ["op", "value", "needle", "start"],
  "string-set-add": ["op", "name", "item"],
  "object-values": ["op", "value"],
  "regex-search": ["op", "value", "pattern", "flags"],
});

export class TypedHookCompileError extends Error {
  constructor(message, { code = "unsupported", nodeType = null } = {}) {
    super(message);
    this.name = "TypedHookCompileError";
    this.code = code;
    this.nodeType = nodeType;
  }
}

function fail(message, options = {}) {
  throw new TypedHookCompileError(message, options);
}

function exactKeys(value, allowed, path) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    fail(`${path} must be an object`, { code: "schema" });
  }
  const actual = Object.keys(value).sort();
  const expected = [...allowed].sort();
  if (
    actual.length !== expected.length ||
    actual.some((key, index) => key !== expected[index])
  ) {
    fail(
      `${path} has unexpected keys; expected ${expected.join(",")}, got ${actual.join(",")}`,
      { code: "schema" },
    );
  }
}

function assertType(type, path) {
  if (!TYPED_VALUE_TYPES.includes(type)) {
    fail(`${path} has unsupported type ${String(type)}`, { code: "schema" });
  }
}

function assertInteger(value, path) {
  if (!Number.isSafeInteger(value))
    fail(`${path} must be a safe integer`, { code: "schema" });
}

function assertString(value, path) {
  if (typeof value !== "string")
    fail(`${path} must be a string`, { code: "schema" });
  if (value.length > MAX_STRING_CODE_UNITS) {
    fail(`${path} exceeds the UTF-16 code-unit limit`, {
      code: "complexity",
    });
  }
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        fail(`${path} contains an unpaired UTF-16 surrogate`, {
          code: "schema",
        });
      }
      index += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      fail(`${path} contains an unpaired UTF-16 surrogate`, {
        code: "schema",
      });
    }
  }
}

function assertSerializedSize(value, path) {
  const bytes = Buffer.byteLength(JSON.stringify(value), "utf8");
  if (bytes > MAX_SERIALIZED_IR_BYTES) {
    fail(`${path} exceeds the serialized IR byte limit`, {
      code: "complexity",
    });
  }
}

function assertIndex(value, path) {
  assertInteger(value, path);
  if (value < 0) fail(`${path} must be non-negative`, { code: "schema" });
}

function literalExpression(value, path) {
  if (value === null) {
    return { type: TYPE.NULL, expr: { op: "null" } };
  }
  if (typeof value === "string") {
    assertString(value, path);
    return { type: TYPE.STRING, expr: { op: "string", value } };
  }
  if (typeof value === "boolean") {
    return { type: TYPE.BOOL, expr: { op: "bool", value } };
  }
  if (typeof value === "number" && Number.isSafeInteger(value)) {
    return { type: TYPE.INTEGER, expr: { op: "integer", value } };
  }
  fail(`${path} is not a supported literal`, {
    code: "unsupported-literal",
  });
}

function unwrapFunction(ast, body) {
  if (ast.type !== "Program" || ast.body.length !== 1) {
    fail("hook must contain exactly one function expression", {
      code: "function-shape",
    });
  }
  const statement = ast.body[0];
  if (statement.type !== "ExpressionStatement") {
    fail("hook must be an arrow or function expression", {
      code: "function-shape",
      nodeType: statement.type,
    });
  }
  const fn = statement.expression;
  if (
    fn.type !== "ArrowFunctionExpression" &&
    fn.type !== "FunctionExpression"
  ) {
    fail("hook must be an arrow or function expression", {
      code: "function-shape",
      nodeType: fn.type,
    });
  }
  if (fn.async || fn.generator) {
    fail("async and generator hooks are not representable", {
      code: "async",
      nodeType: fn.type,
    });
  }
  const names = [];
  for (const parameter of fn.params) {
    if (parameter.type === "Identifier") {
      names.push(parameter.name);
      continue;
    }
    if (parameter.type === "ArrayPattern" || parameter.type === "ObjectPattern") {
      names.push(...bindPatternNames(parameter, null, new Map(), "param"));
      continue;
    }
    if (
      parameter.type === "AssignmentPattern" &&
      (parameter.left.type === "Identifier" ||
        parameter.left.type === "ArrayPattern" ||
        parameter.left.type === "ObjectPattern")
    ) {
      if (parameter.left.type === "Identifier") names.push(parameter.left.name);
      else names.push(...bindPatternNames(parameter.left, null, new Map(), "param"));
      continue;
    }
    fail("hook parameters must be simple identifiers or destructuring patterns", {
      code: "parameter-shape",
    });
  }
  if (new Set(names).size !== names.length) {
    fail("duplicate hook parameter names are not representable", {
      code: "parameter-shape",
    });
  }
  return { fn, names, body };
}

function parseFunctionBody(body) {
  if (typeof body !== "string" || !body.trim()) {
    fail("hook body must be a non-empty string", { code: "input" });
  }
  if (Buffer.byteLength(body, "utf8") > MAX_SOURCE_BYTES) {
    fail("hook body exceeds the source byte limit", { code: "complexity" });
  }
  let ast;
  try {
    ast = acorn.parse(`(${body})`, {
      ecmaVersion: "latest",
      sourceType: "module",
    });
  } catch (error) {
    fail(`hook body is not valid JavaScript: ${error.message}`, {
      code: "syntax",
    });
  }
  return unwrapFunction(ast, body);
}

function propertyName(member) {
  if (member.type !== "MemberExpression" || member.computed) {
    fail("computed member access is not representable", {
      code: "dynamic-property",
      nodeType: member.type,
    });
  }
  if (member.property.type !== "Identifier") {
    fail("member property must be an identifier", {
      code: "dynamic-property",
    });
  }
  return member.property.name;
}

function cloneEnv(environment) {
  return new Map(environment);
}

function envGet(environment, name) {
  return environment.get(name) ?? null;
}

function isArrayType(type) {
  return (
    type === TYPE.STRING_ARRAY ||
    type === TYPE.SUGGESTION_ARRAY ||
    type === TYPE.JSON_ARRAY ||
    type === TYPE.VALUE_ARRAY
  );
}

function arrayElementType(type) {
  if (type === TYPE.STRING_ARRAY) return TYPE.STRING;
  if (type === TYPE.SUGGESTION_ARRAY) return TYPE.SUGGESTION;
  if (type === TYPE.JSON_ARRAY) return TYPE.JSON;
  if (type === TYPE.VALUE_ARRAY) return null;
  return null;
}

function arrayTypeForElement(type) {
  if (type === TYPE.STRING) return TYPE.STRING_ARRAY;
  if (type === TYPE.SUGGESTION) return TYPE.SUGGESTION_ARRAY;
  if (type === TYPE.JSON) return TYPE.JSON_ARRAY;
  return TYPE.VALUE_ARRAY;
}

function isJsonLike(type) {
  return (
    type === TYPE.JSON ||
    type === TYPE.JSON_ARRAY ||
    type === TYPE.STRING_RECORD ||
    type === TYPE.VALUE_ARRAY ||
    type === TYPE.NULL ||
    type == null
  );
}

function unifyTypes(left, right, expectedType = null) {
  if (expectedType) {
    if (left === expectedType || right === expectedType) return expectedType;
    if (isArrayType(expectedType) && (isArrayType(left) || left === TYPE.NULL || left == null)) {
      return expectedType;
    }
    if (isArrayType(expectedType) && (isArrayType(right) || right === TYPE.NULL || right == null)) {
      return expectedType;
    }
  }
  if (left === right) return left;
  if (left == null) return right;
  if (right == null) return left;
  if (left === TYPE.NULL) return right;
  if (right === TYPE.NULL) return left;
  if (isArrayType(left) && isArrayType(right)) {
    if (left === TYPE.SUGGESTION_ARRAY || right === TYPE.SUGGESTION_ARRAY) {
      return TYPE.SUGGESTION_ARRAY;
    }
    if (left === TYPE.STRING_ARRAY && right === TYPE.STRING_ARRAY) {
      return TYPE.STRING_ARRAY;
    }
    return TYPE.VALUE_ARRAY;
  }
  if (isArrayType(left) && right === TYPE.NULL) return left;
  if (isArrayType(right) && left === TYPE.NULL) return right;
  if (left === TYPE.BOOL || right === TYPE.BOOL) {
    if (expectedType === TYPE.BOOL) return TYPE.BOOL;
  }
  if (left === TYPE.INTEGER && right === TYPE.BOOL) return TYPE.INTEGER;
  if (left === TYPE.BOOL && right === TYPE.INTEGER) return TYPE.INTEGER;
  if (isJsonLike(left) || isJsonLike(right)) return expectedType ?? TYPE.JSON;
  return expectedType ?? left;
}

function jsonReceiver(receiver) {
  if (receiver.type === TYPE.JSON || receiver.type === TYPE.JSON_ARRAY) {
    return {
      type: TYPE.JSON_ARRAY,
      expr: { op: "json-array-items", value: receiver.expr },
    };
  }
  return receiver;
}

function flattenStatement(node) {
  if (!node) return [];
  if (node.type === "BlockStatement") return node.body.flatMap(flattenStatement);
  return [node];
}

function compileTruthy(node, environment, state, depth) {
  const value = compileExpression(node, environment, null, state, depth + 1);
  if (value.type === TYPE.BOOL) return value;
  return {
    type: TYPE.BOOL,
    expr: { op: "truthy", value: value.expr },
  };
}

function asStringExpr(value) {
  if (value.type === TYPE.STRING) return value.expr;
  return { op: "to-string", value: value.expr };
}

function compileRegexLiteral(node) {
  const regex = typedRegexFromAcornLiteral(node);
  return regex;
}

function bindPatternNames(pattern, valueType, environment, kind = "let") {
  if (pattern.type === "Identifier") {
    environment.set(pattern.name, { kind, type: valueType, name: pattern.name });
    return [pattern.name];
  }
  if (
    pattern.type === "ArrayPattern" &&
    pattern.elements.length <= 8 &&
    pattern.elements.every(
      (element) =>
        !element ||
        element.type === "Identifier" ||
        (element.type === "RestElement" && element.argument?.type === "Identifier"),
    )
  ) {
    const elementType = isArrayType(valueType) ? arrayElementType(valueType) : valueType;
    const restType = isArrayType(valueType) ? valueType : arrayTypeForElement(valueType);
    const names = [];
    pattern.elements.forEach((element) => {
      if (element?.type === "Identifier") {
        environment.set(element.name, {
          kind,
          type: elementType,
          name: element.name,
        });
        names.push(element.name);
      }
      if (element?.type === "RestElement" && element.argument?.type === "Identifier") {
        environment.set(element.argument.name, {
          kind,
          type: restType ?? TYPE.VALUE_ARRAY,
          name: element.argument.name,
        });
        names.push(element.argument.name);
      }
    });
    return names;
  }
  if (pattern.type === "ObjectPattern") {
    const names = [];
    for (const property of pattern.properties) {
      if (property.type !== "Property" || property.computed) {
        fail("destructuring pattern is not representable", {
          code: "parameter-shape",
        });
      }
      const key =
        property.key.type === "Identifier" ? property.key.name : null;
      const value = property.value;
      const bound =
        value.type === "Identifier"
          ? value
          : value.type === "AssignmentPattern" && value.left.type === "Identifier"
            ? value.left
            : null;
      if (!key || !bound) {
        fail("destructuring pattern is not representable", {
          code: "parameter-shape",
        });
      }
      const fieldType =
        valueType === TYPE.SUGGESTION && SUGGESTION_KEYS.includes(key)
          ? key === "priority"
            ? TYPE.INTEGER
            : key === "hidden" || key === "isDangerous" || key === "deprecated"
              ? TYPE.BOOL
              : TYPE.STRING
          : null;
      environment.set(bound.name, { kind, type: fieldType, name: bound.name, key });
      names.push(bound.name);
    }
    return names;
  }
  fail("hook parameters must be simple identifiers", {
    code: "parameter-shape",
  });
}

function compileLambda(node, environment, elementType, state, depth, secondType = TYPE.INTEGER) {
  if (
    node.type !== "ArrowFunctionExpression" &&
    node.type !== "FunctionExpression"
  ) {
    fail("array callback must be a function expression", {
      code: "unknown-call",
      nodeType: node.type,
    });
  }
  if (node.async || node.generator) {
    fail("async and generator lambdas are not representable", {
      code: "async",
    });
  }
  const childEnv = cloneEnv(environment);
  const params = [];
  const wraps = [];
  node.params.forEach((param, index) => {
    const type = index === 0 ? elementType : secondType;
    if (param.type === "Identifier") {
      childEnv.set(param.name, { kind: "lambda", type, name: param.name });
      params.push(param.name);
      return;
    }
    const synthetic = `$${index}`;
    params.push(synthetic);
    childEnv.set(synthetic, { kind: "lambda", type, name: synthetic });
    bindPatternNames(param, type, childEnv, "lambda");
    wraps.push({ param, synthetic });
  });
  const body = compileFunctionBody(node, childEnv, null, state, depth + 1);
  let expr = body.expr;
  for (const wrap of [...wraps].reverse()) {
    expr = wrapPatternLets(wrap.param, { op: "var", name: wrap.synthetic }, expr);
  }
  return {
    type: null,
    expr: {
      op: "lambda",
      params,
      body: expr,
    },
    resultType: body.type,
  };
}

function wrapPatternLets(pattern, source, body) {
  let expr = body;
  if (pattern.type === "ArrayPattern") {
    pattern.elements.forEach((element, elementIndex) => {
      if (element?.type === "Identifier") {
        expr = {
          op: "let",
          name: element.name,
          value: {
            op: "array-index",
            value: source,
            index: { op: "integer", value: elementIndex },
          },
          body: expr,
        };
      }
      if (element?.type === "RestElement" && element.argument?.type === "Identifier") {
        expr = {
          op: "let",
          name: element.argument.name,
          value: {
            op: "array-slice",
            value: source,
            start: { op: "integer", value: elementIndex },
            end: { op: "length", value: source },
          },
          body: expr,
        };
      }
    });
    return expr;
  }
  if (pattern.type === "ObjectPattern") {
    for (const property of pattern.properties) {
      if (property.type !== "Property" || property.computed) continue;
      const key =
        property.key.type === "Identifier" ? property.key.name : property.key.value;
      const defaultRight =
        property.value.type === "AssignmentPattern" ? property.value.right : null;
      const target =
        property.value.type === "Identifier"
          ? property.value.name
          : property.value.type === "AssignmentPattern" &&
              property.value.left.type === "Identifier"
            ? property.value.left.name
            : null;
      if (!key || !target) continue;
      const fetched = {
        op: "get",
        value: source,
        key: { op: "string", value: key },
      };
      expr = {
        op: "let",
        name: target,
        value: defaultRight
          ? {
              op: "nullish",
              left: fetched,
              right: { op: "null" },
            }
          : fetched,
        body: expr,
      };
    }
  }
  return expr;
}

function compileFunctionBody(fn, environment, expectedType, state, depth) {
  if (fn.body.type === "BlockStatement") {
    return compileStatements(fn.body.body, environment, expectedType, state, depth);
  }
  return compileExpression(fn.body, environment, expectedType, state, depth);
}

function compileStatements(statements, environment, expectedType, state, depth) {
  state.nodes += 1;
  if (state.nodes > MAX_NODES || depth > MAX_DEPTH) {
    fail("typed hook expression exceeds complexity limits", {
      code: "complexity",
    });
  }
  const items = statements.flatMap(flattenStatement).filter((statement) => {
    return statement && statement.type !== "EmptyStatement";
  });
  if (items.length === 0) {
    const empty = { type: TYPE.NULL, expr: { op: "null" } };
    if (expectedType && expectedType !== TYPE.NULL) {
      if (isArrayType(expectedType)) {
        return { type: expectedType, expr: { op: "array", items: [] } };
      }
      fail("empty block does not match expected type", { code: "type-mismatch" });
    }
    return empty;
  }
  const head = items[0];
  const tail = items.slice(1);
  if (head.type === "VariableDeclaration") {
    let currentEnv = environment;
    let wrapped = null;
    const declarators = head.declarations;
    if (declarators.length === 0) {
      return compileStatements(tail, environment, expectedType, state, depth);
    }
    const compileDecl = (index, env) => {
      const declarator = declarators[index];
      const initNode = declarator.init;
      if (
        declarator.id.type === "ArrayPattern" ||
        declarator.id.type === "ObjectPattern"
      ) {
        const init = initNode
          ? compileExpression(initNode, env, null, state, depth + 1)
          : { type: TYPE.NULL, expr: { op: "null" } };
        const nextEnv = cloneEnv(env);
        const names = bindPatternNames(
          declarator.id,
          arrayElementType(init.type) ?? TYPE.JSON,
          nextEnv,
          "let",
        );
        const body =
          index + 1 < declarators.length
            ? compileDecl(index + 1, nextEnv)
            : compileStatements(tail, nextEnv, expectedType, state, depth + 1);
        let expr = body.expr;
        if (declarator.id.type === "ArrayPattern") {
          declarator.id.elements.forEach((element, elementIndex) => {
            if (element?.type === "Identifier") {
              expr = {
                op: "let",
                name: element.name,
                value: {
                  op: "array-index",
                  value: init.expr,
                  index: { op: "integer", value: elementIndex },
                },
                body: expr,
              };
            }
            if (element?.type === "RestElement" && element.argument?.type === "Identifier") {
              expr = {
                op: "let",
                name: element.argument.name,
                value: {
                  op: "array-slice",
                  value: init.expr,
                  start: { op: "integer", value: elementIndex },
                  end: { op: "length", value: init.expr },
                },
                body: expr,
              };
            }
          });
        } else {
          for (const property of declarator.id.properties) {
            if (property.type !== "Property" || property.computed) continue;
            const key =
              property.key.type === "Identifier"
                ? property.key.name
                : property.key.value;
            const defaultValue =
              property.value.type === "AssignmentPattern"
                ? compileExpression(property.value.right, env, null, state, depth + 1)
                : null;
            const target =
              property.value.type === "Identifier"
                ? property.value.name
                : property.value.type === "AssignmentPattern" &&
                    property.value.left.type === "Identifier"
                  ? property.value.left.name
                  : null;
            if (key && target) {
              const fetched = {
                op: "get",
                value: init.expr,
                key: { op: "string", value: key },
              };
              expr = {
                op: "let",
                name: target,
                value: defaultValue
                  ? { op: "nullish", left: fetched, right: defaultValue.expr }
                  : fetched,
                body: expr,
              };
            }
          }
        }
        return { type: body.type, expr };
      }
      if (declarator.id.type !== "Identifier") {
        fail("let bindings must be simple identifiers", {
          code: "parameter-shape",
        });
      }
      if (
        initNode &&
        (initNode.type === "ArrowFunctionExpression" ||
          initNode.type === "FunctionExpression")
      ) {
        const nextEnv = cloneEnv(env);
        nextEnv.set(declarator.id.name, {
          kind: "helper",
          helper: {
            kind: "function",
            node: initNode,
            name: declarator.id.name,
            nodes: 0,
          },
          name: declarator.id.name,
        });
        const body =
          index + 1 < declarators.length
            ? compileDecl(index + 1, nextEnv)
            : compileStatements(tail, nextEnv, expectedType, state, depth + 1);
        return {
          type: body.type,
          expr: {
            op: "let",
            name: declarator.id.name,
            value: { op: "null" },
            body: body.expr,
          },
        };
      }
      const init = initNode
        ? compileExpression(initNode, env, null, state, depth + 1)
        : { type: TYPE.NULL, expr: { op: "null" } };
      const nextEnv = cloneEnv(env);
      nextEnv.set(declarator.id.name, {
        kind: "let",
        type: init.type,
        name: declarator.id.name,
        pattern: init.pattern,
        flags: init.flags,
      });
      const body =
        index + 1 < declarators.length
          ? compileDecl(index + 1, nextEnv)
          : compileStatements(tail, nextEnv, expectedType, state, depth + 1);
      return {
        type: body.type,
        expr: {
          op: "let",
          name: declarator.id.name,
          value: init.expr,
          body: body.expr,
        },
      };
    };
    wrapped = compileDecl(0, currentEnv);
    return wrapped;
  }
  if (head.type === "FunctionDeclaration") {
    fail("nested function declarations are not representable", {
      code: "unsupported-syntax",
      nodeType: head.type,
    });
  }
  if (head.type === "ReturnStatement") {
    const value = head.argument
      ? compileExpression(head.argument, environment, expectedType, state, depth + 1)
      : { type: TYPE.NULL, expr: { op: "null" } };
    if (tail.length === 0 && depth === 0) return value;
    return {
      type: value.type,
      expr: { op: "return", value: value.expr },
    };
  }
  if (head.type === "BreakStatement") {
    if (head.label) {
      fail("labeled break is not representable", { code: "unsupported-syntax" });
    }
    return { type: TYPE.NULL, expr: { op: "break" } };
  }
  if (head.type === "ContinueStatement") {
    if (head.label) {
      fail("labeled continue is not representable", { code: "unsupported-syntax" });
    }
    return { type: TYPE.NULL, expr: { op: "continue" } };
  }
  if (head.type === "IfStatement") {
    const condition = compileTruthy(head.test, environment, state, depth);
    const consequent = compileStatements(
      flattenStatement(head.consequent),
      environment,
      expectedType,
      state,
      depth + 1,
    );
    const alternate = head.alternate
      ? compileStatements(
          flattenStatement(head.alternate),
          environment,
          expectedType,
          state,
          depth + 1,
        )
      : { type: TYPE.NULL, expr: { op: "null" } };
    const branch = {
      type: unifyTypes(consequent.type, alternate.type, expectedType),
      expr: {
        op: "if",
        condition: condition.expr,
        then: consequent.expr,
        else: alternate.expr,
      },
    };
    if (tail.length === 0) return branch;
    const after = compileStatements(tail, environment, expectedType, state, depth + 1);
    return {
      type: after.type ?? branch.type,
      expr: {
        op: "seq",
        items: [branch.expr, after.expr],
      },
    };
  }
  if (head.type === "TryStatement") {
    if (head.finalizer) {
      fail("try/finally is not representable", { code: "unsupported-syntax" });
    }
    const handler = head.handler;
    if (!handler || handler.type !== "CatchClause") {
      fail("try must have a catch clause", { code: "unsupported-syntax" });
    }
    const tryBody = compileStatements(
      flattenStatement(head.block),
      environment,
      expectedType,
      state,
      depth + 1,
    );
    const catchEnv = cloneEnv(environment);
    if (handler.param?.type === "Identifier") {
      catchEnv.set(handler.param.name, {
        kind: "let",
        type: TYPE.NULL,
        name: handler.param.name,
      });
    }
    const catchStatements = flattenStatement(handler.body).filter((statement) => {
      if (statement.type !== "ExpressionStatement") return true;
      const expression = statement.expression;
      if (
        expression.type === "CallExpression" &&
        expression.callee.type === "MemberExpression" &&
        expression.callee.object.type === "Identifier" &&
        expression.callee.object.name === "console"
      ) {
        return false;
      }
      return true;
    });
    const catchBody = compileStatements(
      catchStatements,
      catchEnv,
      expectedType,
      state,
      depth + 1,
    );
    const expr = { op: "try", body: tryBody.expr, catch: catchBody.expr };
    if (tail.length === 0) {
      return { type: tryBody.type ?? catchBody.type, expr };
    }
    return {
      type: tryBody.type ?? catchBody.type,
      expr: {
        op: "seq",
        items: [
          expr,
          compileStatements(tail, environment, expectedType, state, depth + 1).expr,
        ],
      },
    };
  }
  if (head.type === "ForOfStatement") {
    if (head.await) {
      fail("for-await-of is not representable", { code: "async" });
    }
    const left = head.left;
    let pattern = left;
    if (left.type === "VariableDeclaration") {
      if (left.declarations.length !== 1) {
        fail("for-of must bind one name", { code: "unsupported-syntax" });
      }
      pattern = left.declarations[0].id;
    }
    const iterable = compileExpression(head.right, environment, null, state, depth + 1);
    const bodyEnv = cloneEnv(environment);
    const names = bindPatternNames(
      pattern,
      arrayElementType(iterable.type),
      bodyEnv,
      "let",
    );
    const body = compileStatements(
      flattenStatement(head.body),
      bodyEnv,
      null,
      state,
      depth + 1,
    );
    const loop = {
      type: TYPE.NULL,
      expr: {
        op: "for-of",
        names,
        value: iterable.expr,
        body: body.expr,
      },
    };
    if (tail.length === 0) return loop;
    return {
      type: expectedType ?? TYPE.NULL,
      expr: {
        op: "seq",
        items: [
          loop.expr,
          compileStatements(tail, environment, expectedType, state, depth + 1).expr,
        ],
      },
    };
  }
  if (head.type === "ForStatement" || head.type === "WhileStatement") {
    const initStatements =
      head.type === "ForStatement" && head.init
        ? head.init.type === "VariableDeclaration"
          ? [head.init]
          : [{ type: "ExpressionStatement", expression: head.init }]
        : [];
    const test = head.test ?? { type: "Literal", value: true };
    const bodyStatements = [
      ...flattenStatement(head.body),
      ...(head.type === "ForStatement" && head.update
        ? [{ type: "ExpressionStatement", expression: head.update }]
        : []),
    ];
    if (initStatements.length > 0) {
      return compileStatements(
        [...initStatements, { type: "WhileStatement", test, body: { type: "BlockStatement", body: bodyStatements } }, ...tail],
        environment,
        expectedType,
        state,
        depth,
      );
    }
    const condition = compileTruthy(test, environment, state, depth);
    const body = compileStatements(bodyStatements, environment, null, state, depth + 1);
    const loop = {
      type: TYPE.NULL,
      expr: { op: "while", condition: condition.expr, body: body.expr },
    };
    if (tail.length === 0) return loop;
    return {
      type: expectedType ?? TYPE.NULL,
      expr: {
        op: "seq",
        items: [
          loop.expr,
          compileStatements(tail, environment, expectedType, state, depth + 1).expr,
        ],
      },
    };
  }
  if (head.type === "ExpressionStatement") {
    const value = compileExpression(head.expression, environment, null, state, depth + 1);
    if (tail.length === 0) {
      if (expectedType && value.type !== expectedType && value.type !== TYPE.NULL) {
        if (isArrayType(expectedType) && value.type === TYPE.STRING_ARRAY && value.expr.op === "array" && value.expr.items.length === 0) {
          return { type: expectedType, expr: value.expr };
        }
      }
      return expectedType ? compileExpression(head.expression, environment, expectedType, state, depth + 1) : value;
    }
    const rest = compileStatements(tail, environment, expectedType, state, depth + 1);
    return {
      type: rest.type,
      expr: { op: "seq", items: [value.expr, rest.expr] },
    };
  }
  fail(`statement ${head.type} is not representable`, {
    code: "unsupported-syntax",
    nodeType: head.type,
  });
}

function compileHelperCall(
  helperFn,
  argumentNodes,
  environment,
  expectedType,
  state,
  depth,
  child,
) {
  const helperEnv = cloneEnv(environment);
  const bindings = [];
  helperFn.params.forEach((param, index) => {
    if (param.type === "RestElement" && param.argument.type === "Identifier") {
      const restArgs = argumentNodes.slice(index).map((argument) => child(argument));
      helperEnv.set(param.argument.name, {
        kind: "let",
        type: TYPE.VALUE_ARRAY,
        name: param.argument.name,
      });
      bindings.push({
        name: param.argument.name,
        expr: { op: "array", items: restArgs.map((item) => item.expr) },
      });
      return;
    }
    if (param.type === "AssignmentPattern" && param.left.type === "Identifier") {
      const argument =
        index < argumentNodes.length
          ? child(argumentNodes[index])
          : compileExpression(param.right, environment, null, state, depth + 1);
      helperEnv.set(param.left.name, {
        kind: "let",
        type: argument.type,
        name: param.left.name,
      });
      bindings.push({ name: param.left.name, expr: argument.expr });
      return;
    }
    if (param.type !== "Identifier") {
      fail("helper parameters must be simple identifiers", {
        code: "parameter-shape",
      });
    }
    if (index >= argumentNodes.length) {
      fail("helper is missing an argument", { code: "call-arity" });
    }
    const argument = child(argumentNodes[index]);
    helperEnv.set(param.name, {
      kind: "let",
      type: argument.type,
      name: param.name,
    });
    bindings.push({ name: param.name, expr: argument.expr });
  });
  let body = compileFunctionBody(
    helperFn,
    helperEnv,
    expectedType,
    state,
    depth + 1,
  );
  body = {
    type: body.type,
    expr: { op: "catch-return", body: body.expr },
  };
  for (let index = bindings.length - 1; index >= 0; index -= 1) {
    body = {
      type: body.type,
      expr: {
        op: "let",
        name: bindings[index].name,
        value: bindings[index].expr,
        body: body.expr,
      },
    };
  }
  return body;
}

function compileExpression(node, environment, expectedType, state, depth = 0) {
  if (!node || typeof node.type !== "string") {
    fail("missing expression node", { code: "unsupported-syntax" });
  }
  state.nodes += 1;
  if (state.nodes > MAX_NODES || depth > MAX_DEPTH) {
    fail("typed hook expression exceeds complexity limits", {
      code: "complexity",
    });
  }
  const child = (childNode, childExpected = null) =>
    compileExpression(childNode, environment, childExpected, state, depth + 1);
  const finish = (result) => {
    if (expectedType && result.type !== expectedType) {
      const unified = unifyTypes(result.type, expectedType, expectedType);
      if (unified === expectedType) {
        return { ...result, type: expectedType };
      }
      fail(
        `expression type ${result.type} does not match expected ${expectedType}`,
        { code: "type-mismatch", nodeType: node.type },
      );
    }
    return result;
  };

  switch (node.type) {
    case "Identifier": {
      const binding = envGet(environment, node.name);
      if (binding?.kind === "param") {
        return finish({
          type: binding.type,
          expr: { op: "arg", index: binding.index },
        });
      }
      if (binding?.kind === "let" || binding?.kind === "lambda") {
        return finish({
          type: binding.type,
          expr: { op: "var", name: binding.name ?? node.name },
          pattern: binding.pattern,
          flags: binding.flags,
        });
      }
      if (binding?.kind === "helper") {
        if (binding.helper.kind === "expr") {
          return finish(
            compileExpression(binding.helper.node, environment, expectedType, state, depth + 1),
          );
        }
        if (binding.helper.kind === "function") {
          fail(`helper ${node.name} must be called`, {
            code: "unknown-call",
            nodeType: node.type,
          });
        }
      }
      const builtin = BUILTIN_IDENTIFIERS[node.name];
      if (builtin?.expr) return finish(builtin);
      if (builtin) {
        fail(`identifier ${node.name} is only valid as a callee`, {
          code: "free-variable",
          nodeType: node.type,
        });
      }
      fail(`free identifier ${node.name} is not representable`, {
        code: "free-variable",
        nodeType: node.type,
      });
    }
    case "Literal": {
      if (node.bigint != null) {
        fail("bigint literals are unsupported", {
          code: "unsupported-literal",
          nodeType: node.type,
        });
      }
      if (node.regex) {
        const regex = compileRegexLiteral(node);
        return finish({
          type: TYPE.REGEX,
          expr: { op: "regex", pattern: regex.pattern, flags: regex.flags },
          pattern: regex.pattern,
          flags: regex.flags,
        });
      }
      return finish(literalExpression(node.value, "literal"));
    }
    case "UnaryExpression": {
      if (node.operator === "void") {
        child(node.argument);
        return finish({ type: TYPE.NULL, expr: { op: "null" } });
      }
      if (node.operator === "typeof") {
        const value = child(node.argument);
        return finish({
          type: TYPE.STRING,
          expr: { op: "typeof", value: value.expr },
        });
      }
      if (node.operator === "-") {
        if (
          node.argument.type === "Literal" &&
          typeof node.argument.value === "number" &&
          Number.isSafeInteger(node.argument.value)
        ) {
          const value = -node.argument.value;
          if (!Number.isSafeInteger(value)) {
            fail("unary negation overflowed the JavaScript safe integer range", {
              code: "overflow",
              nodeType: node.type,
            });
          }
          return finish({
            type: TYPE.INTEGER,
            expr: { op: "integer", value },
          });
        }
        const operand = child(node.argument, TYPE.INTEGER);
        return finish({
          type: TYPE.INTEGER,
          expr: {
            op: "sub",
            left: { op: "integer", value: 0 },
            right: operand.expr,
          },
        });
      }
      if (node.operator !== "!") {
        fail(`unary operator ${node.operator} is unsupported`, {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      // Fold `!0` / `!1` / `!true` so the existing trigger inventory stays a
      // bool literal.  Any other operand is a typed `not` of a bool.
      if (node.argument.type === "Literal") {
        if (typeof node.argument.value === "number") {
          return finish({
            type: TYPE.BOOL,
            expr: { op: "bool", value: !Boolean(node.argument.value) },
          });
        }
        if (typeof node.argument.value === "boolean") {
          return finish({
            type: TYPE.BOOL,
            expr: { op: "bool", value: !node.argument.value },
          });
        }
      }
      const operand = compileTruthy(node.argument, environment, state, depth);
      return finish({
        type: TYPE.BOOL,
        expr: { op: "not", value: operand.expr },
      });
    }
    case "MemberExpression": {
      if (node.computed) {
        const receiver = child(node.object);
        const index = child(node.property);
        if (
          isArrayType(receiver.type) ||
          receiver.type === TYPE.JSON ||
          receiver.type === TYPE.STRING ||
          isJsonLike(receiver.type)
        ) {
          if (
            index.type !== TYPE.INTEGER &&
            index.type !== TYPE.STRING &&
            !isJsonLike(index.type)
          ) {
            fail("computed access requires an integer or string index", {
              code: "type-mismatch",
              nodeType: node.type,
            });
          }
          const resultType =
            arrayElementType(receiver.type) ??
            (index.type === TYPE.STRING ? TYPE.JSON : TYPE.JSON);
          return finish({
            type: resultType,
            expr: {
              op: "array-index",
              value: receiver.expr,
              index: index.expr,
            },
          });
        }
        fail("computed member access is not representable", {
          code: "dynamic-property",
          nodeType: node.type,
        });
      }
      const name = propertyName(node);
      if (
        node.object.type === "Identifier" &&
        envGet(environment, node.object.name)?.kind === "helper"
      ) {
        const helper = envGet(environment, node.object.name).helper;
        if (helper.kind === "expr" && helper.node.type === "ObjectExpression") {
          const property = helper.node.properties.find(
            (entry) =>
              entry.type === "Property" &&
              !entry.computed &&
              ((entry.key.type === "Identifier" && entry.key.name === name) ||
                (entry.key.type === "Literal" && entry.key.value === name)),
          );
          if (property) {
            return finish(child(property.value));
          }
        }
      }
      const receiver = child(node.object);
      if (name === "length") {
        if (
          receiver.type !== TYPE.STRING &&
          !isArrayType(receiver.type) &&
          !isJsonLike(receiver.type)
        ) {
          fail(".length is supported only on strings and arrays", {
            code: "type-mismatch",
            nodeType: node.type,
          });
        }
        return finish({
          type: TYPE.INTEGER,
          expr: { op: "length", value: receiver.expr },
        });
      }
      if (receiver.type === TYPE.SUGGESTION && SUGGESTION_KEYS.includes(name)) {
        const fieldType =
          name === "priority"
            ? TYPE.INTEGER
            : name === "hidden" || name === "isDangerous" || name === "deprecated"
              ? TYPE.BOOL
              : TYPE.STRING;
        return finish({
          type: fieldType,
          expr: { op: "get", value: receiver.expr, key: { op: "string", value: name } },
        });
      }
      if (isJsonLike(receiver.type) || receiver.type === TYPE.SUGGESTION) {
        return finish({
          type: TYPE.JSON,
          expr: { op: "json-get", value: receiver.expr, key: { op: "string", value: name } },
        });
      }
      fail(`property .${name} is unsupported`, {
        code: "unsupported-property",
        nodeType: node.type,
      });
    }
    case "CallExpression": {
      const spreadArguments = node.arguments.filter(
        (argument) => argument.type === "SpreadElement",
      );
      if (
        spreadArguments.length > 0 &&
        !(
          node.callee.type === "MemberExpression" &&
          node.callee.object.type === "Identifier" &&
          node.callee.object.name === "Math" &&
          !node.callee.computed &&
          node.callee.property.type === "Identifier" &&
          node.callee.property.name === "max" &&
          node.arguments.length === 1
        )
      ) {
        fail("spread calls are unsupported", {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      const optionalCall = node.optional || node.callee.optional === true;
      if (node.callee.type === "Identifier") {
        const calleeName = node.callee.name;
        const binding = envGet(environment, calleeName);
        if (binding?.kind === "helper" && binding.helper.kind === "function") {
          return finish(
            compileHelperCall(
              binding.helper.node,
              node.arguments,
              environment,
              expectedType,
              state,
              depth,
              child,
            ),
          );
        }
        if (calleeName === "Boolean") {
          if (node.arguments.length !== 1) {
            fail("Boolean expects one argument", { code: "call-arity" });
          }
          return finish(compileTruthy(node.arguments[0], environment, state, depth));
        }
        if (calleeName === "String") {
          if (node.arguments.length !== 1) {
            fail("String expects one argument", { code: "call-arity" });
          }
          return finish({
            type: TYPE.STRING,
            expr: { op: "to-string", value: child(node.arguments[0]).expr },
          });
        }
        if (
          calleeName === "Array" &&
          node.arguments.length >= 1
        ) {
          fail("Array(...) construction is not representable", { code: "unknown-call" });
        }
        if (calleeName === "Number" || calleeName === "parseInt") {
          if (node.arguments.length < 1) {
            fail(`${calleeName} expects an argument`, { code: "call-arity" });
          }
          return finish({
            type: TYPE.INTEGER,
            expr: { op: "json-as-number", value: child(node.arguments[0]).expr },
          });
        }
        fail("only allowlisted methods may be called", {
          code: "unknown-call",
          nodeType: node.type,
        });
      }
      if (
        node.callee.type === "ArrowFunctionExpression" ||
        node.callee.type === "FunctionExpression"
      ) {
        return finish(
          compileHelperCall(
            node.callee,
            node.arguments,
            environment,
            expectedType,
            state,
            depth,
            child,
          ),
        );
      }
      if (node.callee.type !== "MemberExpression") {
        fail("only allowlisted methods may be called", {
          code: "unknown-call",
          nodeType: node.type,
        });
      }
      if (
        node.callee.object.type === "Identifier" &&
        node.callee.object.name === "console"
      ) {
        node.arguments.forEach((argument) => {
          if (argument.type !== "SpreadElement") child(argument);
        });
        return finish({ type: TYPE.NULL, expr: { op: "null" } });
      }
      if (
        node.callee.object.type === "Identifier" &&
        node.callee.object.name === "JSON" &&
        !node.callee.computed &&
        node.callee.property.type === "Identifier"
      ) {
        if (node.callee.property.name === "parse") {
          if (node.arguments.length !== 1) {
            fail("JSON.parse expects exactly one argument", { code: "call-arity" });
          }
          const value = child(node.arguments[0], TYPE.STRING);
          return finish({
            type: TYPE.JSON,
            expr: { op: "json-parse", value: value.expr },
          });
        }
        if (node.callee.property.name === "stringify") {
          if (node.arguments.length < 1) {
            fail("JSON.stringify expects an argument", { code: "call-arity" });
          }
          return finish({
            type: TYPE.STRING,
            expr: { op: "json-stringify", value: child(node.arguments[0]).expr },
          });
        }
        fail("only JSON.parse and JSON.stringify are representable", { code: "unknown-call" });
      }
      if (
        node.callee.object.type === "Identifier" &&
        node.callee.object.name === "Object" &&
        !node.callee.computed &&
        node.callee.property.type === "Identifier"
      ) {
        const methodName = node.callee.property.name;
        if (methodName === "keys") {
          if (node.arguments.length !== 1) {
            fail("Object.keys expects exactly one argument", { code: "call-arity" });
          }
          const value = child(node.arguments[0]);
          return finish({
            type: TYPE.STRING_ARRAY,
            expr: { op: "object-keys", value: value.expr },
          });
        }
        if (methodName === "entries") {
          if (node.arguments.length !== 1) {
            fail("Object.entries expects exactly one argument", { code: "call-arity" });
          }
          const value = child(node.arguments[0]);
          return finish({
            type: TYPE.VALUE_ARRAY,
            expr: { op: "object-entries", value: value.expr },
          });
        }
        if (methodName === "values") {
          if (node.arguments.length !== 1) {
            fail("Object.values expects exactly one argument", { code: "call-arity" });
          }
          const value = child(node.arguments[0]);
          return finish({
            type: TYPE.VALUE_ARRAY,
            expr: { op: "object-values", value: value.expr },
          });
        }
        fail(`Object.${methodName} is not representable`, { code: "unknown-call" });
      }
      if (
        node.callee.object.type === "Identifier" &&
        node.callee.object.name === "Array" &&
        !node.callee.computed &&
        node.callee.property.type === "Identifier" &&
        node.callee.property.name === "from"
      ) {
        if (node.arguments.length < 1) {
          fail("Array.from expects an argument", { code: "call-arity" });
        }
        if (
          node.arguments.length >= 2 &&
          (node.arguments[1].type === "ArrowFunctionExpression" ||
            node.arguments[1].type === "FunctionExpression")
        ) {
          const source = child(node.arguments[0]);
          const mapped = compileLambda(
            node.arguments[1],
            environment,
            TYPE.JSON,
            state,
            depth,
          );
          return finish({
            type: arrayTypeForElement(mapped.resultType),
            expr: {
              op: "array-map",
              value: { op: "array-from", value: source.expr },
              fn: mapped.expr,
            },
          });
        }
        return finish({
          type: TYPE.VALUE_ARRAY,
          expr: { op: "array-from", value: child(node.arguments[0]).expr },
        });
      }
      if (
        node.callee.object.type === "Identifier" &&
        node.callee.object.name === "Math" &&
        !node.callee.computed &&
        node.callee.property.type === "Identifier" &&
        (node.callee.property.name === "max" || node.callee.property.name === "min")
      ) {
        const valuesExpr =
          node.arguments.length === 1 && node.arguments[0].type === "SpreadElement"
            ? child(node.arguments[0].argument).expr
            : {
                op: "array",
                items: node.arguments.map((argument) => child(argument, TYPE.INTEGER).expr),
              };
        if (node.callee.property.name === "min") {
          return finish({
            type: TYPE.INTEGER,
            expr: {
              op: "sub",
              left: { op: "integer", value: 0 },
              right: {
                op: "math-max",
                values: {
                  op: "array-map",
                  value: valuesExpr,
                  fn: {
                    op: "lambda",
                    params: ["$n"],
                    body: {
                      op: "sub",
                      left: { op: "integer", value: 0 },
                      right: { op: "var", name: "$n" },
                    },
                  },
                },
              },
            },
          });
        }
        return finish({
          type: TYPE.INTEGER,
          expr: { op: "math-max", values: valuesExpr },
        });
      }
      if (
        node.callee.object.type === "Identifier" &&
        envGet(environment, node.callee.object.name)?.kind === "helper" &&
        !node.callee.computed
      ) {
        const helper = envGet(environment, node.callee.object.name).helper;
        const methodName = propertyName(node.callee);
        if (helper.kind === "expr" && helper.node.type === "ObjectExpression") {
          const property = helper.node.properties.find(
            (entry) =>
              entry.type === "Property" &&
              !entry.computed &&
              entry.key.type === "Identifier" &&
              entry.key.name === methodName,
          );
          if (property && (property.value.type === "FunctionExpression" || property.value.type === "ArrowFunctionExpression")) {
            return finish(
              compileHelperCall(
                property.value,
                node.arguments,
                environment,
                expectedType,
                state,
                depth,
                child,
              ),
            );
          }
        }
      }
      const method = propertyName(node.callee);
      let receiver = child(node.callee.object);
      const zeroArguments = () => {
        if (node.arguments.length !== 0) {
          fail(`${method} expects no arguments`, {
            code: "call-arity",
            nodeType: node.type,
          });
        }
      };
      const oneArgument = (expected) => {
        if (node.arguments.length !== 1) {
          fail(`${method} expects exactly one argument`, {
            code: "call-arity",
            nodeType: node.type,
          });
        }
        return child(node.arguments[0], expected);
      };
      const requireStringReceiver = () => {
        if (receiver.type === TYPE.STRING) return;
        if (isJsonLike(receiver.type)) {
          receiver = {
            type: TYPE.STRING,
            expr: { op: "to-string", value: receiver.expr },
          };
          return;
        }
        fail(`.${method} is supported only on strings`, {
          code: "type-mismatch",
          nodeType: node.type,
        });
      };
      const literalNeedle = (argument) => {
        if (argument.expr.op !== "string") {
          fail(`.${method} needle must be a string literal`, {
            code: "unsupported-syntax",
            nodeType: node.type,
          });
        }
        return argument;
      };
      if (method === "includes") {
        const needle = oneArgument(TYPE.STRING);
        if (receiver.type === TYPE.STRING) {
          return finish({
            type: TYPE.BOOL,
            expr: {
              op: "string-includes",
              value: receiver.expr,
              needle: needle.expr,
            },
          });
        }
        if (receiver.type === TYPE.STRING_ARRAY) {
          return finish({
            type: TYPE.BOOL,
            expr: {
              op: "array-includes",
              value: receiver.expr,
              needle: needle.expr,
            },
          });
        }
        if (isArrayType(receiver.type) || isJsonLike(receiver.type)) {
          return finish({
            type: TYPE.BOOL,
            expr: {
              op: "array-includes",
              value: receiver.expr,
              needle: needle.expr,
            },
          });
        }
        fail(".includes is supported only on strings and string arrays", {
          code: "type-mismatch",
          nodeType: node.type,
        });
      }
      if (method === "indexOf") {
        if (node.arguments.length < 1 || node.arguments.length > 2) {
          fail("indexOf expects one or two arguments", {
            code: "call-arity",
            nodeType: node.type,
          });
        }
        if (receiver.type === TYPE.STRING || isJsonLike(receiver.type)) {
          if (receiver.type !== TYPE.STRING) requireStringReceiver();
          const needle = child(node.arguments[0], TYPE.STRING);
          if (node.arguments.length === 2) {
            const start = child(node.arguments[1], TYPE.INTEGER);
            return finish({
              type: TYPE.INTEGER,
              expr: {
                op: "string-index-of-from",
                value: receiver.expr,
                needle: needle.expr,
                start: start.expr,
              },
            });
          }
          return finish({
            type: TYPE.INTEGER,
            expr: {
              op: "string-index-of",
              value: receiver.expr,
              needle: needle.expr,
            },
          });
        }
        if (isArrayType(receiver.type) || isJsonLike(receiver.type)) {
          const needle = child(node.arguments[0]);
          if (node.arguments.length === 2) {
            const start = child(node.arguments[1], TYPE.INTEGER);
            return finish({
              type: TYPE.INTEGER,
              expr: {
                op: "array-index-of-from",
                value: receiver.expr,
                needle: needle.expr,
                start: start.expr,
              },
            });
          }
          return finish({
            type: TYPE.INTEGER,
            expr: {
              op: "array-index-of",
              value: receiver.expr,
              needle: needle.expr,
            },
          });
        }
        fail(".indexOf is supported only on strings and arrays", {
          code: "type-mismatch",
          nodeType: node.type,
        });
      }
      if (method === "slice") {
        if (node.arguments.length < 1 || node.arguments.length > 2) {
          fail("slice expects one or two arguments", {
            code: "call-arity",
            nodeType: node.type,
          });
        }
        const start = child(node.arguments[0], TYPE.INTEGER);
        if (receiver.type === TYPE.STRING) {
          if (node.arguments.length === 1) {
            return finish({
              type: TYPE.STRING,
              expr: { op: "string-slice", value: receiver.expr, start: start.expr },
            });
          }
          const end = child(node.arguments[1], TYPE.INTEGER);
          return finish({
            type: TYPE.STRING,
            expr: {
              op: "string-slice-range",
              value: receiver.expr,
              start: start.expr,
              end: end.expr,
            },
          });
        }
        if (isArrayType(receiver.type)) {
          const end =
            node.arguments.length === 2
              ? child(node.arguments[1], TYPE.INTEGER)
              : { expr: { op: "length", value: receiver.expr } };
          return finish({
            type: receiver.type,
            expr: {
              op: "array-slice",
              value: receiver.expr,
              start: start.expr,
              end: end.expr,
            },
          });
        }
        if (isJsonLike(receiver.type)) {
          receiver = jsonReceiver(receiver);
          const end =
            node.arguments.length === 2
              ? child(node.arguments[1], TYPE.INTEGER)
              : { expr: { op: "length", value: receiver.expr } };
          return finish({
            type: TYPE.JSON_ARRAY,
            expr: {
              op: "array-slice",
              value: receiver.expr,
              start: start.expr,
              end: end.expr,
            },
          });
        }
        fail(".slice is supported only on strings and arrays", {
          code: "type-mismatch",
          nodeType: node.type,
        });
      }
      if (method === "split") {
        requireStringReceiver();
        if (node.arguments.length < 1 || node.arguments.length > 2) {
          fail("split expects one or two arguments", { code: "call-arity" });
        }
        if (node.arguments[0].type === "Literal" && node.arguments[0].regex) {
          const regex = compileRegexLiteral(node.arguments[0]);
          return finish({
            type: TYPE.STRING_ARRAY,
            expr: {
              op: "string-split-regex",
              value: receiver.expr,
              pattern: regex.pattern,
              flags: regex.flags,
            },
          });
        }
        const separator = child(node.arguments[0], TYPE.STRING);
        if (node.arguments.length === 2) {
          const limit = child(node.arguments[1], TYPE.INTEGER);
          return finish({
            type: TYPE.STRING_ARRAY,
            expr: {
              op: "string-split-limit",
              value: receiver.expr,
              separator: separator.expr,
              limit: limit.expr,
            },
          });
        }
        return finish({
          type: TYPE.STRING_ARRAY,
          expr: {
            op: "string-split",
            value: receiver.expr,
            separator: separator.expr,
          },
        });
      }
      if (method === "lastIndexOf") {
        requireStringReceiver();
        const needle = oneArgument(TYPE.STRING);
        return finish({
          type: TYPE.INTEGER,
          expr: {
            op: "string-last-index-of",
            value: receiver.expr,
            needle: needle.expr,
          },
        });
      }
      if (method === "startsWith" || method === "endsWith") {
        requireStringReceiver();
        const needle = oneArgument(TYPE.STRING);
        return finish({
          type: TYPE.BOOL,
          expr: {
            op: method === "startsWith" ? "string-starts-with" : "string-ends-with",
            value: receiver.expr,
            needle: needle.expr,
          },
        });
      }
      if (method === "trim" || method === "trimStart" || method === "trimEnd") {
        requireStringReceiver();
        zeroArguments();
        const op =
          method === "trim"
            ? "string-trim"
            : method === "trimStart"
              ? "string-trim-start"
              : "string-trim-end";
        const compiled = {
          type: TYPE.STRING,
          expr: { op, value: receiver.expr },
        };
        if (optionalCall) {
          return finish({
            type: TYPE.STRING,
            expr: {
              op: "if",
              condition: {
                op: "not",
                value: {
                  op: "strict-eq",
                  left: receiver.expr,
                  right: { op: "null" },
                },
              },
              then: compiled.expr,
              else: { op: "null" },
            },
          });
        }
        return finish(compiled);
      }
      if (method === "toLowerCase" || method === "toUpperCase") {
        requireStringReceiver();
        zeroArguments();
        return finish({
          type: TYPE.STRING,
          expr: {
            op: method === "toLowerCase" ? "string-to-lower" : "string-to-upper",
            value: receiver.expr,
          },
        });
      }
      if (method === "replace" || method === "replaceAll") {
        requireStringReceiver();
        if (node.arguments.length !== 2) {
          fail(`${method} expects exactly two arguments`, {
            code: "call-arity",
            nodeType: node.type,
          });
        }
        if (node.arguments[0].type === "Literal" && node.arguments[0].regex) {
          const regex = compileRegexLiteral(node.arguments[0]);
          const replacement = child(node.arguments[1], TYPE.STRING);
          return finish({
            type: TYPE.STRING,
            expr: {
              op: "regex-replace",
              value: receiver.expr,
              pattern: regex.pattern,
              flags: method === "replaceAll" && !regex.flags.includes("g")
                ? `${regex.flags}g`
                : regex.flags,
              replacement: replacement.expr,
            },
          });
        }
        const needle = literalNeedle(child(node.arguments[0], TYPE.STRING));
        const replacement = child(node.arguments[1], TYPE.STRING);
        return finish({
          type: TYPE.STRING,
          expr: {
            op: method === "replace" ? "string-replace" : "string-replace-all",
            value: receiver.expr,
            needle: needle.expr,
            replacement: replacement.expr,
          },
        });
      }
      if (method === "substring") {
        requireStringReceiver();
        if (node.arguments.length < 1 || node.arguments.length > 2) {
          fail("substring expects one or two arguments", {
            code: "call-arity",
            nodeType: node.type,
          });
        }
        const start = child(node.arguments[0], TYPE.INTEGER);
        const end =
          node.arguments.length === 2
            ? child(node.arguments[1], TYPE.INTEGER)
            : { expr: { op: "length", value: receiver.expr } };
        return finish({
          type: TYPE.STRING,
          expr: {
            op: "string-substring",
            value: receiver.expr,
            start: start.expr,
            end: end.expr,
          },
        });
      }
      if (method === "padStart" || method === "padEnd") {
        requireStringReceiver();
        if (node.arguments.length < 1 || node.arguments.length > 2) {
          fail(`${method} expects one or two arguments`, {
            code: "call-arity",
            nodeType: node.type,
          });
        }
        const target = child(node.arguments[0], TYPE.INTEGER);
        const pad =
          node.arguments.length === 2
            ? child(node.arguments[1], TYPE.STRING)
            : { expr: { op: "string", value: " " } };
        return finish({
          type: TYPE.STRING,
          expr: {
            op: method === "padStart" ? "string-pad-start" : "string-pad-end",
            value: receiver.expr,
            target: target.expr,
            pad: pad.expr,
          },
        });
      }
      if (method === "repeat") {
        requireStringReceiver();
        const count = oneArgument(TYPE.INTEGER);
        return finish({
          type: TYPE.STRING,
          expr: {
            op: "string-repeat",
            value: receiver.expr,
            count: count.expr,
          },
        });
      }
      if (method === "concat" && receiver.type === TYPE.STRING) {
        if (node.arguments.some((argument) => argument.type === "SpreadElement")) {
          fail("spread concat is unsupported", {
            code: "unsupported-syntax",
            nodeType: node.type,
          });
        }
        const parts = [
          receiver.expr,
          ...node.arguments.map((argument) => child(argument, TYPE.STRING).expr),
        ];
        return finish({
          type: TYPE.STRING,
          expr: { op: "string-concat", parts },
        });
      }
        if (method === "toString") {
          zeroArguments();
          return finish({
            type: TYPE.STRING,
            expr: { op: "to-string", value: receiver.expr },
          });
        }
        if (method === "localeCompare") {
          const other = oneArgument(TYPE.STRING);
          return finish({
            type: TYPE.INTEGER,
            expr: {
              op: "locale-compare",
              left: asStringExpr(receiver),
              right: other.expr,
            },
          });
        }
        if (method === "charAt" || method === "at") {
          const index = oneArgument(TYPE.INTEGER);
          if (method === "at" && (isArrayType(receiver.type) || isJsonLike(receiver.type))) {
            return finish({
              type: arrayElementType(receiver.type) ?? TYPE.JSON,
              expr: {
                op: "array-index",
                value: receiver.expr,
                index: index.expr,
              },
            });
          }
          requireStringReceiver();
          return finish({
            type: TYPE.STRING,
            expr: {
              op: method === "charAt" ? "string-char-at" : "string-at",
              value: receiver.expr,
              index: index.expr,
            },
          });
        }
      if (method === "map" || method === "filter" || method === "flatMap" || method === "forEach") {
        if (!isArrayType(receiver.type) && !isJsonLike(receiver.type)) {
          fail(`.${method} is supported only on arrays`, {
            code: "type-mismatch",
          });
        }
        receiver = isArrayType(receiver.type) ? receiver : jsonReceiver(receiver);
        if (node.arguments.length !== 1) {
          fail(`${method} expects exactly one argument`, { code: "call-arity" });
        }
        if (
          node.arguments[0].type === "Identifier" &&
          node.arguments[0].name === "Boolean" &&
          method === "filter"
        ) {
          return finish({
            type: receiver.type === TYPE.JSON ? TYPE.VALUE_ARRAY : receiver.type,
            expr: {
              op: "array-filter",
              value: receiver.expr,
              fn: {
                op: "lambda",
                params: ["$item"],
                body: { op: "truthy", value: { op: "var", name: "$item" } },
              },
            },
          });
        }
        const elementType =
          arrayElementType(receiver.type === TYPE.JSON ? TYPE.JSON_ARRAY : receiver.type);
        let callbackNode = node.arguments[0];
        if (callbackNode.type === "Identifier") {
          const binding = envGet(environment, callbackNode.name);
          if (binding?.kind === "helper" && binding.helper.kind === "function") {
            callbackNode = binding.helper.node;
          }
        }
        const fn = compileLambda(
          callbackNode,
          environment,
          elementType,
          state,
          depth,
        );
        const resultType =
          method === "filter"
            ? receiver.type
            : arrayTypeForElement(fn.resultType);
        const op =
          method === "filter"
            ? "array-filter"
            : method === "flatMap"
              ? "array-flat-map"
              : "array-map";
        return finish({
          type: resultType,
          expr: { op, value: receiver.expr, fn: fn.expr },
        });
      }
      if (
        method === "some" ||
        method === "every" ||
        method === "find" ||
        method === "findIndex"
      ) {
        if (!isArrayType(receiver.type) && !isJsonLike(receiver.type)) {
          fail(`.${method} is supported only on arrays`, { code: "type-mismatch" });
        }
        receiver = isArrayType(receiver.type) ? receiver : jsonReceiver(receiver);
        const fn = compileLambda(
          node.arguments[0],
          environment,
          arrayElementType(receiver.type),
          state,
          depth,
        );
        const resultType =
          method === "findIndex"
            ? TYPE.INTEGER
            : method === "find"
              ? arrayElementType(receiver.type)
              : TYPE.BOOL;
        const op =
          method === "some"
            ? "array-some"
            : method === "every"
              ? "array-every"
              : method === "find"
                ? "array-find"
                : "array-find-index";
        return finish({
          type: resultType,
          expr: { op, value: receiver.expr, fn: fn.expr },
        });
      }
      if (method === "join") {
        if (!isArrayType(receiver.type) && !isJsonLike(receiver.type)) {
          fail(".join is supported only on arrays", { code: "type-mismatch" });
        }
        const separator =
          node.arguments.length === 0
            ? { expr: { op: "string", value: "," } }
            : child(node.arguments[0], TYPE.STRING);
        return finish({
          type: TYPE.STRING,
          expr: { op: "array-join", value: receiver.expr, separator: separator.expr },
        });
      }
      if (method === "concat") {
        if (receiver.type === TYPE.STRING) {
          if (node.arguments.some((argument) => argument.type === "SpreadElement")) {
            fail("spread concat is unsupported", {
              code: "unsupported-syntax",
              nodeType: node.type,
            });
          }
          const parts = [
            receiver.expr,
            ...node.arguments.map((argument) => child(argument, TYPE.STRING).expr),
          ];
          return finish({
            type: TYPE.STRING,
            expr: { op: "string-concat", parts },
          });
        }
        if (isArrayType(receiver.type)) {
          const parts = [
            receiver.expr,
            ...node.arguments.map((argument) => child(argument).expr),
          ];
          return finish({
            type: receiver.type,
            expr: { op: "array-concat", parts },
          });
        }
      }
      if (method === "reverse" || method === "sort") {
        if (!isArrayType(receiver.type) && !isJsonLike(receiver.type)) {
          fail(`.${method} is supported only on arrays`, { code: "type-mismatch" });
        }
        receiver = isArrayType(receiver.type) ? receiver : jsonReceiver(receiver);
        if (method === "reverse") {
          zeroArguments();
          return finish({
            type: receiver.type,
            expr: { op: "array-reverse", value: receiver.expr },
          });
        }
        if (node.arguments.length === 0) {
          return finish({
            type: receiver.type,
            expr: {
              op: "array-sort",
              value: receiver.expr,
              fn: { op: "lambda", params: ["a", "b"], body: { op: "integer", value: 0 } },
            },
          });
        }
        let comparator = node.arguments[0];
        if (comparator.type === "Identifier") {
          const binding = envGet(environment, comparator.name);
          if (binding?.kind === "helper" && binding.helper.kind === "function") {
            comparator = binding.helper.node;
          }
        }
        const fn = compileLambda(
          comparator,
          environment,
          arrayElementType(receiver.type),
          state,
          depth,
          arrayElementType(receiver.type),
        );
        return finish({
          type: receiver.type,
          expr: { op: "array-sort", value: receiver.expr, fn: fn.expr },
        });
      }
      if (method === "pop") {
        if (!isArrayType(receiver.type)) {
          fail(".pop is supported only on arrays", { code: "type-mismatch" });
        }
        zeroArguments();
        return finish({
          type: arrayElementType(receiver.type) ?? TYPE.JSON,
          expr: { op: "array-pop", value: receiver.expr },
        });
      }
      if (method === "push") {
        if (node.callee.object.type !== "Identifier") {
          fail(".push must mutate a bound name", { code: "unsupported-syntax" });
        }
        const item = oneArgument();
        return finish({
          type: TYPE.INTEGER,
          expr: {
            op: "array-push",
            name: node.callee.object.name,
            item: item.expr,
          },
        });
      }
      if (method === "entries") {
        if (!isArrayType(receiver.type)) {
          fail(".entries is supported only on arrays", { code: "type-mismatch" });
        }
        zeroArguments();
        return finish({
          type: TYPE.VALUE_ARRAY,
          expr: { op: "array-entries", value: receiver.expr },
        });
      }
      if (method === "has") {
        if (receiver.type !== TYPE.STRING_SET) {
          fail(".has is supported only on string sets", { code: "type-mismatch" });
        }
        const needle = oneArgument(TYPE.STRING);
        return finish({
          type: TYPE.BOOL,
          expr: {
            op: "array-includes",
            value: receiver.expr,
            needle: needle.expr,
          },
        });
      }
      if (method === "add") {
        if (receiver.type !== TYPE.STRING_SET) {
          fail(".add is supported only on string sets", { code: "type-mismatch" });
        }
        if (node.callee.object.type !== "Identifier") {
          fail(".add must mutate a bound name", { code: "unsupported-syntax" });
        }
        const item = oneArgument(TYPE.STRING);
        return finish({
          type: TYPE.STRING_SET,
          expr: {
            op: "string-set-add",
            name: node.callee.object.name,
            item: item.expr,
          },
        });
      }
      if (method === "shift") {
        zeroArguments();
        if (
          node.callee.object.type === "Identifier" &&
          envGet(environment, node.callee.object.name)?.kind === "let"
        ) {
          return finish({
            type: arrayElementType(receiver.type) ?? TYPE.JSON,
            expr: { op: "array-shift", name: node.callee.object.name },
          });
        }
        return finish({
          type: arrayElementType(receiver.type) ?? TYPE.JSON,
          expr: {
            op: "array-index",
            value: receiver.expr,
            index: { op: "integer", value: 0 },
          },
        });
      }
      if (method === "splice") {
        if (node.arguments.length < 1 || node.arguments.length > 2) {
          fail("splice expects one or two arguments", { code: "call-arity" });
        }
        const start = child(node.arguments[0], TYPE.INTEGER);
        const end =
          node.arguments.length === 2
            ? {
                op: "add",
                left: start.expr,
                right: child(node.arguments[1], TYPE.INTEGER).expr,
              }
            : { op: "length", value: receiver.expr };
        return finish({
          type: isArrayType(receiver.type) ? receiver.type : TYPE.VALUE_ARRAY,
          expr: {
            op: "array-slice",
            value: receiver.expr,
            start: start.expr,
            end,
          },
        });
      }
      if (method === "test" || method === "exec") {
        let pattern = receiver.pattern;
        let flags = receiver.flags;
        if (receiver.type === TYPE.REGEX && (pattern == null || flags == null)) {
          fail("RegExp methods require a regex literal operand", {
            code: "unknown-call",
          });
        }
        if (receiver.type !== TYPE.REGEX) {
          fail("RegExp methods require a regex literal operand", {
            code: "unknown-call",
          });
        }
        const value = oneArgument(TYPE.STRING);
        if (method === "test") {
          return finish({
            type: TYPE.BOOL,
            expr: {
              op: "regex-test",
              value: value.expr,
              pattern,
              flags: flags ?? "",
            },
          });
        }
        return finish({
          type: TYPE.VALUE_ARRAY,
          expr: {
            op: "regex-match",
            value: value.expr,
            pattern,
            flags: flags ?? "",
          },
        });
      }
      if (method === "flat") {
        if (!isArrayType(receiver.type) && !isJsonLike(receiver.type)) {
          fail(".flat is supported only on arrays", { code: "type-mismatch" });
        }
        const depth =
          node.arguments.length === 0
            ? { type: TYPE.INTEGER, expr: { op: "integer", value: 1 } }
            : child(node.arguments[0], TYPE.INTEGER);
        return finish({
          type: TYPE.VALUE_ARRAY,
          expr: { op: "array-flat", value: receiver.expr, depth: depth.expr },
        });
      }
      if (method === "match" || method === "matchAll" || method === "search") {
        requireStringReceiver();
        if (node.arguments.length !== 1) {
          fail(`${method} expects exactly one argument`, { code: "call-arity" });
        }
        let regex;
        if (node.arguments[0].type === "Literal" && node.arguments[0].regex) {
          regex = compileRegexLiteral(node.arguments[0]);
        } else if (
          node.arguments[0].type === "Literal" &&
          typeof node.arguments[0].value === "string"
        ) {
          regex = assertTypedRegexLiteral(node.arguments[0].value, "");
        } else if (node.arguments[0].type === "Identifier") {
          const binding = envGet(environment, node.arguments[0].name);
          if (binding?.type === TYPE.REGEX && binding.pattern != null) {
            regex = { pattern: binding.pattern, flags: binding.flags ?? "" };
          } else if (
            binding?.kind === "helper" &&
            binding.helper.kind === "expr" &&
            binding.helper.node.type === "Literal" &&
            binding.helper.node.regex
          ) {
            regex = compileRegexLiteral(binding.helper.node);
          }
        }
        if (!regex) {
          fail(`${method} requires a regular expression literal`, {
            code: "unsupported-syntax",
          });
        }
        if (method === "search") {
          return finish({
            type: TYPE.INTEGER,
            expr: {
              op: "regex-search",
              value: receiver.expr,
              pattern: regex.pattern,
              flags: regex.flags,
            },
          });
        }
        if (method === "match") {
          return finish({
            type: TYPE.VALUE_ARRAY,
            expr: {
              op: "regex-match",
              value: receiver.expr,
              pattern: regex.pattern,
              flags: regex.flags,
            },
          });
        }
        if (method === "matchAll") {
          return finish({
            type: TYPE.VALUE_ARRAY,
            expr: {
              op: "regex-match-all",
              value: receiver.expr,
              pattern: regex.pattern,
              flags: regex.flags,
            },
          });
        }
      }
      if (method === "includes") {
        const needle = oneArgument(TYPE.STRING);
        if (receiver.type === TYPE.STRING) {
          return finish({
            type: TYPE.BOOL,
            expr: {
              op: "string-includes",
              value: receiver.expr,
              needle: needle.expr,
            },
          });
        }
        if (isArrayType(receiver.type)) {
          return finish({
            type: TYPE.BOOL,
            expr: {
              op: "array-includes",
              value: receiver.expr,
              needle: needle.expr,
            },
          });
        }
      }
      fail(`method .${method} is not in the typed hook allowlist`, {
        code: "unknown-call",
        nodeType: node.type,
      });
    }
    case "BinaryExpression": {
      if (
        !["===", "!==", "==", "!=", ">", ">=", "<", "<=", "+", "-", "*"].includes(
          node.operator,
        )
      ) {
        fail(`binary operator ${node.operator} is unsupported`, {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      if (node.operator === "+") {
        const left = child(node.left);
        const right = child(node.right);
        if (left.type === TYPE.INTEGER && right.type === TYPE.INTEGER) {
          return finish({
            type: TYPE.INTEGER,
            expr: { op: "add", left: left.expr, right: right.expr },
          });
        }
        return finish({
          type: TYPE.STRING,
          expr: {
            op: "string-concat",
            parts: [asStringExpr(left), asStringExpr(right)],
          },
        });
      }
      if (node.operator === "-" || node.operator === "*") {
        const asInteger = (value) => {
          if (value.type === TYPE.INTEGER) return value.expr;
          return { op: "json-as-number", value: value.expr };
        };
        const left = child(node.left);
        const right = child(node.right);
        return finish({
          type: TYPE.INTEGER,
          expr: {
            op: node.operator === "-" ? "sub" : "mul",
            left: asInteger(left),
            right: asInteger(right),
          },
        });
      }
      if (["<", "<=", ">", ">="].includes(node.operator)) {
        const left = child(node.left, TYPE.INTEGER);
        const right = child(node.right, TYPE.INTEGER);
        const op =
          node.operator === "<"
            ? "lt"
            : node.operator === "<="
              ? "le"
              : node.operator === ">"
                ? "gt"
                : "ge";
        return finish({
          type: TYPE.BOOL,
          expr: { op, left: left.expr, right: right.expr },
        });
      }
      const left = child(node.left);
      const right = child(node.right);
      if (node.operator === "==" || node.operator === "!=") {
        return finish({
          type: TYPE.BOOL,
          expr: {
            op: node.operator === "==" ? "loose-eq" : "loose-ne",
            left: left.expr,
            right: right.expr,
          },
        });
      }
      if (left.type === TYPE.STRING_ARRAY || right.type === TYPE.STRING_ARRAY) {
        fail("strict equality on arrays is not representable", {
          code: "type-mismatch",
          nodeType: node.type,
        });
      }
      return finish({
        type: TYPE.BOOL,
        expr: {
          op: node.operator === "===" ? "strict-eq" : "strict-ne",
          left: left.expr,
          right: right.expr,
        },
      });
    }
    case "LogicalExpression": {
      if (node.operator === "??") {
        const left = child(node.left);
        const right = child(node.right);
        let resultType = left.type;
        if (left.type === TYPE.NULL) resultType = right.type;
        else if (right.type === TYPE.NULL) resultType = left.type;
        else if (left.type !== right.type) {
          resultType = unifyTypes(left.type, right.type, expectedType);
        }
        return finish({
          type: resultType,
          expr: { op: "nullish", left: left.expr, right: right.expr },
        });
      }
      if (node.operator !== "&&" && node.operator !== "||") {
        fail(`logical operator ${node.operator} is unsupported`, {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      const left = child(node.left);
      const right = child(node.right, expectedType);
      const resultType =
        expectedType === TYPE.BOOL
          ? TYPE.BOOL
          : unifyTypes(left.type, right.type, expectedType);
      return finish({
        type: resultType,
        expr: {
          op: node.operator === "&&" ? "and" : "or",
          left:
            expectedType === TYPE.BOOL && left.type !== TYPE.BOOL
              ? { op: "truthy", value: left.expr }
              : left.expr,
          right:
            expectedType === TYPE.BOOL && right.type !== TYPE.BOOL
              ? { op: "truthy", value: right.expr }
              : right.expr,
        },
      });
    }
    case "TemplateLiteral": {
      if (node.expressions.length !== node.quasis.length - 1) {
        fail("template literal is malformed", {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      const parts = [];
      for (let index = 0; index < node.quasis.length; index += 1) {
        const cooked = node.quasis[index].value.cooked;
        if (typeof cooked !== "string") {
          fail("template quasi must be a cooked string", {
            code: "unsupported-literal",
            nodeType: node.type,
          });
        }
        if (cooked.length > 0) {
          assertString(cooked, "template");
          parts.push({ op: "string", value: cooked });
        }
        if (index < node.expressions.length) {
          parts.push(asStringExpr(child(node.expressions[index])));
        }
      }
      if (parts.length === 0) {
        return finish({ type: TYPE.STRING, expr: { op: "string", value: "" } });
      }
      if (parts.length === 1 && parts[0].op === "string") {
        return finish({ type: TYPE.STRING, expr: parts[0] });
      }
      return finish({
        type: TYPE.STRING,
        expr: { op: "string-concat", parts },
      });
    }
    case "ConditionalExpression": {
      const condition = compileTruthy(node.test, environment, state, depth);
      const consequent = child(node.consequent);
      const alternate = child(node.alternate);
      const resultType = unifyTypes(consequent.type, alternate.type, expectedType);
      return finish({
        type: resultType,
        expr: {
          op: "if",
          condition: condition.expr,
          then: consequent.expr,
          else: alternate.expr,
        },
      });
    }
    case "ArrayExpression": {
      if (node.elements.some((element) => element == null)) {
        fail("array holes are unsupported", {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      if (node.elements.some((element) => element.type === "SpreadElement")) {
        const parts = node.elements.map((element) => {
          if (element.type === "SpreadElement") return child(element.argument);
          return child(element);
        });
        const resultType =
          expectedType && isArrayType(expectedType)
            ? expectedType
            : parts.find((part) => isArrayType(part.type))?.type ?? TYPE.VALUE_ARRAY;
        return finish({
          type: resultType,
          expr: { op: "array-concat", parts: parts.map((part) => part.expr) },
        });
      }
      if (node.elements.length === 0) {
        const type = isArrayType(expectedType)
          ? expectedType
          : expectedType === TYPE.NULL
            ? TYPE.STRING_ARRAY
            : TYPE.STRING_ARRAY;
        return finish({
          type,
          expr: { op: "array", items: [] },
        });
      }
      const items = node.elements.map((element) => child(element));
      const itemType = items[0].type;
      const homogeneous = items.every((item) => item.type === itemType);
      const type = homogeneous
        ? arrayTypeForElement(itemType)
        : TYPE.VALUE_ARRAY;
      return finish({
        type: expectedType && isArrayType(expectedType) ? expectedType : type,
        expr: { op: "array", items: items.map((item) => item.expr) },
      });
    }
    case "ObjectExpression": {
      if (node.properties.length === 0) {
        return finish({
          type:
            expectedType === TYPE.STRING_RECORD || expectedType === TYPE.SUGGESTION
              ? expectedType
              : TYPE.JSON,
          expr: { op: "json-object", fields: [] },
        });
      }
      const parts = [];
      let fields = [];
      let jsonFields = false;
      const flushFields = () => {
        if (fields.length === 0) return;
        parts.push({
          op: jsonFields ? "json-object" : "object",
          fields,
        });
        fields = [];
        jsonFields = false;
      };
      for (const property of node.properties) {
        if (property.type === "SpreadElement") {
          const spread = child(property.argument);
          if (spread.type === TYPE.NULL) {
            continue;
          }
          if (
            spread.type !== TYPE.SUGGESTION &&
            spread.type !== TYPE.JSON &&
            spread.type !== TYPE.STRING_RECORD &&
            spread.type != null
          ) {
            fail("spread is only allowed for suggestion objects", {
              code: "type-mismatch",
            });
          }
          flushFields();
          parts.push(spread.expr);
          continue;
        }
        if (property.type !== "Property" || property.computed || property.kind !== "init") {
          fail("object property is not representable", {
            code: "unsupported-syntax",
            nodeType: property.type,
          });
        }
        const key =
          property.key.type === "Identifier"
            ? property.key.name
            : property.key.type === "Literal" && typeof property.key.value === "string"
              ? property.key.value
              : null;
        if (!key) {
          fail(`object key ${String(key)} is not representable`, {
            code: "unsupported-syntax",
          });
        }
        if (!SUGGESTION_KEYS.includes(key)) jsonFields = true;
        fields.push({
          key,
          value: child(property.value).expr,
        });
      }
      flushFields();
      if (parts.length === 0) {
        return finish({
          type: TYPE.JSON,
          expr: { op: "json-object", fields: [] },
        });
      }
      if (parts.length === 1) {
        const only = parts[0];
        if (only.op === "object" || only.op === "json-object") {
          return finish({
            type: only.op === "json-object" ? TYPE.JSON : TYPE.SUGGESTION,
            expr: only,
          });
        }
        return finish({ type: TYPE.SUGGESTION, expr: only });
      }
      if (
        parts.length === 2 &&
        parts[1].op === "object"
      ) {
        return finish({
          type: TYPE.SUGGESTION,
          expr: { op: "spread", value: parts[0], fields: parts[1].fields },
        });
      }
      return finish({
        type: TYPE.SUGGESTION,
        expr: { op: "object-assign", parts },
      });
    }
    case "AssignmentExpression": {
      if (node.left.type === "Identifier") {
        const binding = envGet(environment, node.left.name);
        if (!binding || (binding.kind !== "let" && binding.kind !== "lambda" && binding.kind !== "param")) {
          fail("assignment target is not a let binding", {
            code: "free-variable",
          });
        }
        const expected =
          binding.type && binding.type !== TYPE.NULL ? binding.type : null;
        let value;
        if (node.operator === "+=") {
          const right = child(node.right);
          if (right.type === TYPE.INTEGER && (binding.type === TYPE.INTEGER || binding.type === TYPE.NULL)) {
            value = {
              type: TYPE.INTEGER,
              expr: {
                op: "add",
                left: { op: "var", name: node.left.name },
                right: right.expr,
              },
            };
          } else {
            value = {
              type: TYPE.STRING,
              expr: {
                op: "string-concat",
                parts: [
                  { op: "to-string", value: { op: "var", name: node.left.name } },
                  asStringExpr(right),
                ],
              },
            };
          }
        } else if (node.operator === "-=") {
          const right = child(node.right, TYPE.INTEGER);
          value = {
            type: TYPE.INTEGER,
            expr: {
              op: "sub",
              left: { op: "var", name: node.left.name },
              right: right.expr,
            },
          };
        } else if (node.operator !== "=") {
          fail(`assignment operator ${node.operator} is unsupported`, {
            code: "unsupported-syntax",
          });
        } else {
          value = child(node.right, expected);
        }
        binding.type = value.type ?? binding.type;
        if (binding.kind === "param") {
          if (state.assignedParams instanceof Map && typeof binding.index === "number") {
            state.assignedParams.set(node.left.name, binding.index);
          }
          binding.kind = "let";
          binding.name = node.left.name;
        }
        return finish({
          type: value.type,
          expr: { op: "assign-var", name: node.left.name, value: value.expr },
        });
      }
      if (node.left.type === "MemberExpression" && !node.left.computed) {
        const object = child(node.left.object);
        const key = propertyName(node.left);
        const value = child(node.right);
        return finish({
          type: value.type,
          expr: {
            op: "assign-prop",
            object: object.expr,
            key: { op: "string", value: key },
            value: value.expr,
          },
        });
      }
      fail("assignment target is not representable", {
        code: "unsupported-syntax",
        nodeType: node.left.type,
      });
    }
    case "SequenceExpression": {
      const items = node.expressions.map((expression) => child(expression));
      return finish({
        type: items[items.length - 1].type,
        expr: { op: "seq", items: items.map((item) => item.expr) },
      });
    }
    case "ChainExpression": {
      return finish(child(node.expression, expectedType));
    }
    case "UpdateExpression": {
      if (node.argument.type !== "Identifier") {
        fail("update expressions must target a bound name", {
          code: "unsupported-syntax",
        });
      }
      const binding = envGet(environment, node.argument.name);
      if (!binding || (binding.kind !== "let" && binding.kind !== "lambda")) {
        fail("update target is not a let binding", { code: "free-variable" });
      }
      const next = {
        op: node.operator === "++" ? "add" : "sub",
        left: { op: "var", name: node.argument.name },
        right: { op: "integer", value: 1 },
      };
      const assign = { op: "assign-var", name: node.argument.name, value: next };
      if (node.prefix) {
        return finish({ type: TYPE.INTEGER, expr: assign });
      }
      return finish({
        type: TYPE.INTEGER,
        expr: {
          op: "let",
          name: "$old",
          value: { op: "var", name: node.argument.name },
          body: { op: "seq", items: [assign, { op: "var", name: "$old" }] },
        },
      });
    }
    case "NewExpression": {
      if (node.callee.type === "Identifier" && node.callee.name === "RegExp") {
        if (node.arguments.length < 1 || node.arguments.length > 2) {
          fail("RegExp expects one or two arguments", { code: "call-arity" });
        }
        if (
          node.arguments[0].type !== "Literal" ||
          typeof node.arguments[0].value !== "string"
        ) {
          fail("RegExp pattern must be a string literal", {
            code: "unsupported-syntax",
          });
        }
        const flags =
          node.arguments.length === 2
            ? node.arguments[1].type === "Literal" &&
              typeof node.arguments[1].value === "string"
              ? node.arguments[1].value
              : null
            : "";
        if (flags == null) {
          fail("RegExp flags must be a string literal", {
            code: "unsupported-syntax",
          });
        }
        const regex = assertTypedRegexLiteral(node.arguments[0].value, flags);
        return finish({
          type: TYPE.REGEX,
          expr: { op: "regex", pattern: regex.pattern, flags: regex.flags },
          pattern: regex.pattern,
          flags: regex.flags,
        });
      }
      if (node.callee.type !== "Identifier" || node.callee.name !== "Set") {
        fail("only new Set is representable", { code: "unknown-call" });
      }
      if (node.arguments.length > 1) {
        fail("Set expects at most one argument", { code: "call-arity" });
      }
      const value =
        node.arguments.length === 0
          ? { type: TYPE.STRING_ARRAY, expr: { op: "array", items: [] } }
          : child(node.arguments[0]);
      return finish({
        type: TYPE.STRING_SET,
        expr: { op: "string-set", value: value.expr },
      });
    }
    default:
      fail(`AST node ${node.type} is not representable`, {
        code: "unsupported-syntax",
        nodeType: node.type,
      });
  }
}

function compileFunction({ body, parameterTypes, resultType, helpers = new Map() }) {
  if (!Array.isArray(parameterTypes)) {
    fail("parameterTypes must be a type array", { code: "input" });
  }
  parameterTypes.forEach((type, index) =>
    assertType(type, `parameterTypes[${index}]`),
  );
  assertType(resultType, "resultType");
  const { fn } = parseFunctionBody(body);
  if (fn.params.length > parameterTypes.length) {
    fail(
      `hook declares ${fn.params.length} parameters but contract provides ${parameterTypes.length}`,
      { code: "parameter-count" },
    );
  }
  const environment = new Map();
  fn.params.forEach((param, index) => {
    const type = parameterTypes[index];
    if (param.type === "Identifier") {
      environment.set(param.name, {
        kind: "param",
        index,
        type,
        name: param.name,
      });
      return;
    }
    if (
      param.type === "AssignmentPattern" &&
      param.left.type === "Identifier"
    ) {
      environment.set(param.left.name, {
        kind: "let",
        type,
        name: param.left.name,
      });
      return;
    }
    bindPatternNames(param, type, environment, "let");
  });
  for (const [name, helper] of helpers) {
    if (!environment.has(name)) {
      environment.set(name, { kind: "helper", helper, name });
    }
  }
  const state = { nodes: 0, assignedParams: new Map() };
  const result = compileFunctionBody(fn, environment, resultType, state, 0);
  let expression = result.expr;
  const wrapLet = (name, value) => {
    expression = { op: "let", name, value, body: expression };
  };
  fn.params.forEach((param, index) => {
    if (param.type === "ArrayPattern") {
      param.elements.forEach((element, elementIndex) => {
        if (element?.type === "Identifier") {
          wrapLet(element.name, {
            op: "array-index",
            value: { op: "arg", index },
            index: { op: "integer", value: elementIndex },
          });
        }
        if (element?.type === "RestElement" && element.argument?.type === "Identifier") {
          wrapLet(element.argument.name, {
            op: "array-slice",
            value: { op: "arg", index },
            start: { op: "integer", value: elementIndex },
            end: { op: "length", value: { op: "arg", index } },
          });
        }
      });
    }
    if (
      param.type === "AssignmentPattern" &&
      param.left.type === "Identifier"
    ) {
      wrapLet(param.left.name, {
        op: "nullish",
        left: { op: "arg", index },
        right: compileExpression(
          param.right,
          environment,
          parameterTypes[index],
          state,
          0,
        ).expr,
      });
    }
    if (param.type === "ObjectPattern") {
      for (const property of param.properties) {
        if (property.type !== "Property" || property.computed) continue;
        const key =
          property.key.type === "Identifier" ? property.key.name : property.key.value;
        const target =
          property.value.type === "Identifier"
            ? property.value.name
            : property.value.type === "AssignmentPattern" &&
                property.value.left.type === "Identifier"
              ? property.value.left.name
              : null;
        if (key && target) {
          wrapLet(target, {
            op: "get",
            value: { op: "arg", index },
            key: { op: "string", value: key },
          });
        }
      }
    }
  });
  for (const [name, index] of state.assignedParams ?? []) {
    wrapLet(name, { op: "arg", index });
  }
  if (resultType === TYPE.STRING_ARRAY && result.type === TYPE.STRING) {
    expression = {
      op: "array",
      items: [
        { op: "string", value: "sh" },
        { op: "string", value: "-c" },
        expression,
      ],
    };
  } else if (resultType === TYPE.STRING_ARRAY && result.type === TYPE.NULL) {
    expression = { op: "array", items: [] };
  } else if (
    resultType &&
    result.type &&
    result.type !== resultType &&
    unifyTypes(result.type, resultType, resultType) !== resultType
  ) {
    fail(
      `expression type ${result.type} does not match expected ${resultType}`,
      { code: "type-mismatch" },
    );
  }
  return { expression, nodes: state.nodes };
}

/**
 * Compile getQueryTerm through the general expression compiler.  The closed
 * asdf `includes("latest") ? slice(indexOf(":")+1)` shape is now just one
 * successful program that uses `add` + `string-slice`.
 */
export function compileTypedGetQueryTerm({ body } = {}) {
  return compileTypedHook({ body, sourceField: "getQueryTerm" });
}

/**
 * Compile one hook body under a named hook contract.
 *
 * The returned descriptor intentionally contains no source hash or hook id;
 * those are provenance fields owned by the surrounding compiler manifest.
 */
export function compileTypedHook({
  body,
  sourceField = "trigger",
  resultType,
  moduleSource,
  helperLiterals,
} = {}) {
  const contract =
    typeof sourceField === "string" &&
    Object.hasOwn(TYPED_HOOK_CONTRACTS, sourceField)
      ? TYPED_HOOK_CONTRACTS[sourceField]
      : undefined;
  if (!contract) {
    fail(`no typed hook contract exists for ${sourceField}`, {
      code: "unknown-field",
    });
  }
  if (resultType !== undefined && resultType !== contract.resultType) {
    fail(
      `${sourceField} requires result type ${contract.resultType}, got ${resultType}`,
      { code: "type-mismatch" },
    );
  }
  let helpers = new Map();
  if (moduleSource) {
    try {
      helpers = resolveHookHelpers({ body, moduleSource, helperLiterals }).helpers;
    } catch (error) {
      if (error instanceof TypedHookInlineError) {
        fail(error.message, { code: error.code });
      }
      throw error;
    }
  }
  const compiled = compileFunction({
    body,
    parameterTypes: contract.params,
    resultType: contract.resultType,
    helpers,
  });
  const descriptor = {
    version: TYPED_HOOK_IR_VERSION,
    kind: TYPED_HOOK_IR_KIND,
    sourceField,
    resultType: contract.resultType,
    params: contract.params.map((type, index) => ({ index, type })),
    expr: compiled.expression,
  };
  validateTypedHookIr(descriptor);
  return descriptor;
}

/**
 * Compile a function using an explicit positional type contract.  This keeps
 * the expression compiler testable for reusable operations such as
 * Array.includes before a corresponding hook field is admitted.
 */
export function compileTypedExpression({
  body,
  parameterTypes,
  resultType,
} = {}) {
  const compiled = compileFunction({ body, parameterTypes, resultType });
  const result = {
    expr: compiled.expression,
    resultType,
    nodes: compiled.nodes,
  };
  assertSerializedSize(result, "typed expression");
  return result;
}

function validateExpression(
  node,
  expectedType,
  parameterTypes,
  path = "expr",
  state = { nodes: 0 },
  depth = 0,
) {
  if (!node || typeof node !== "object" || Array.isArray(node)) {
    fail(`${path} must be an expression object`, { code: "schema" });
  }
  state.nodes += 1;
  if (state.nodes > MAX_NODES || depth > MAX_DEPTH) {
    fail(`${path} exceeds node limit`, { code: "complexity" });
  }
  const op = node.op;
  if (!TYPED_EXPRESSION_OPERATIONS.includes(op) || !OP_KEYS[op]) {
    fail(`${path}.op ${String(op)} is unsupported`, { code: "schema" });
  }
  exactKeys(node, OP_KEYS[op], path);
  const child = (value, type, key) =>
    validateExpression(
      value,
      type,
      parameterTypes,
      `${path}.${key}`,
      state,
      depth + 1,
    );
  const ensureResult = (type) => {
    if (expectedType && type !== expectedType) {
      fail(`${path} has type ${type}, expected ${expectedType}`, {
        code: "type-mismatch",
      });
    }
    return type;
  };
  switch (op) {
    case "arg":
      assertIndex(node.index, `${path}.index`);
      if (node.index >= parameterTypes.length) {
        fail(`${path}.index is outside the parameter contract`, {
          code: "schema",
        });
      }
      return ensureResult(parameterTypes[node.index]);
    case "string":
      assertString(node.value, `${path}.value`);
      return ensureResult(TYPE.STRING);
    case "bool":
      if (typeof node.value !== "boolean")
        fail(`${path}.value must be boolean`, { code: "schema" });
      return ensureResult(TYPE.BOOL);
    case "integer":
      assertInteger(node.value, `${path}.value`);
      return ensureResult(TYPE.INTEGER);
    case "null":
      return ensureResult(TYPE.NULL);
    case "array":
      if (!Array.isArray(node.items))
        fail(`${path}.items must be an array`, { code: "schema" });
      const itemTypes = node.items.map((item, index) => child(item, null, `items[${index}]`));
      if (expectedType && isArrayType(expectedType)) return ensureResult(expectedType);
      if (itemTypes.length === 0) return TYPE.STRING_ARRAY;
      if (itemTypes.every((type) => type === TYPE.STRING)) return TYPE.STRING_ARRAY;
      if (itemTypes.every((type) => type === TYPE.SUGGESTION)) return TYPE.SUGGESTION_ARRAY;
      return TYPE.VALUE_ARRAY;
    case "length": {
      child(node.value, null, "value");
      return ensureResult(TYPE.INTEGER);
    }
    case "string-includes":
    case "string-index-of":
    case "string-last-index-of":
    case "string-starts-with":
    case "string-ends-with": {
      child(node.value, TYPE.STRING, "value");
      child(node.needle, TYPE.STRING, "needle");
      return ensureResult(
        op === "string-includes" ||
          op === "string-starts-with" ||
          op === "string-ends-with"
          ? TYPE.BOOL
          : TYPE.INTEGER,
      );
    }
    case "string-slice":
      child(node.value, TYPE.STRING, "value");
      child(node.start, TYPE.INTEGER, "start");
      return ensureResult(TYPE.STRING);
    case "string-substring":
      child(node.value, TYPE.STRING, "value");
      child(node.start, TYPE.INTEGER, "start");
      child(node.end, TYPE.INTEGER, "end");
      return ensureResult(TYPE.STRING);
    case "string-trim":
    case "string-trim-start":
    case "string-trim-end":
    case "string-to-lower":
    case "string-to-upper":
      child(node.value, TYPE.STRING, "value");
      return ensureResult(TYPE.STRING);
    case "string-replace":
    case "string-replace-all":
      child(node.value, TYPE.STRING, "value");
      child(node.needle, TYPE.STRING, "needle");
      if (node.needle.op !== "string") {
        fail(`${path}.needle must be a string literal`, { code: "schema" });
      }
      child(node.replacement, TYPE.STRING, "replacement");
      return ensureResult(TYPE.STRING);
    case "string-pad-start":
    case "string-pad-end":
      child(node.value, TYPE.STRING, "value");
      child(node.target, TYPE.INTEGER, "target");
      child(node.pad, TYPE.STRING, "pad");
      return ensureResult(TYPE.STRING);
    case "string-repeat":
      child(node.value, TYPE.STRING, "value");
      child(node.count, TYPE.INTEGER, "count");
      return ensureResult(TYPE.STRING);
    case "string-concat":
      if (!Array.isArray(node.parts) || node.parts.length === 0) {
        fail(`${path}.parts must be a non-empty array`, { code: "schema" });
      }
      node.parts.forEach((part, index) =>
        child(part, TYPE.STRING, `parts[${index}]`),
      );
      return ensureResult(TYPE.STRING);
    case "string-char-at":
    case "string-at":
      child(node.value, TYPE.STRING, "value");
      child(node.index, TYPE.INTEGER, "index");
      return ensureResult(TYPE.STRING);
    case "string-slice-after-first":
      child(node.value, TYPE.STRING, "value");
      child(node.needle, TYPE.STRING, "needle");
      if (
        node.needle.op !== "string" ||
        node.needle.value !== ":"
      ) {
        fail(
          `${path}.needle must be the closed getQueryTerm colon literal`,
          { code: "schema" },
        );
      }
      return ensureResult(TYPE.STRING);
    case "string-split":
      child(node.value, TYPE.STRING, "value");
      child(node.separator, TYPE.STRING, "separator");
      return ensureResult(TYPE.STRING_ARRAY);
    case "array-includes":
      child(node.value, null, "value");
      child(node.needle, null, "needle");
      return ensureResult(TYPE.BOOL);
    case "strict-eq":
    case "strict-ne": {
      child(node.left, null, "left");
      child(node.right, null, "right");
      return ensureResult(TYPE.BOOL);
    }
    case "add":
    case "sub":
    case "mul":
      child(node.left, TYPE.INTEGER, "left");
      child(node.right, TYPE.INTEGER, "right");
      return ensureResult(TYPE.INTEGER);
    case "lt":
    case "le":
    case "gt":
    case "ge":
      child(node.left, TYPE.INTEGER, "left");
      child(node.right, TYPE.INTEGER, "right");
      return ensureResult(TYPE.BOOL);
    case "not":
      child(node.value, TYPE.BOOL, "value");
      return ensureResult(TYPE.BOOL);
    case "nullish": {
      const leftType = child(node.left, null, "left");
      const rightType = child(node.right, null, "right");
      let resultType = leftType;
      if (leftType === TYPE.NULL) resultType = rightType;
      else if (rightType === TYPE.NULL) resultType = leftType;
      else if (leftType !== rightType) {
        resultType = unifyTypes(leftType, rightType, expectedType) ?? TYPE.JSON;
      }
      return expectedType ?? resultType;
    }
    case "and":
    case "or":
      child(node.left, null, "left");
      child(node.right, null, "right");
      return expectedType ?? TYPE.JSON;
    case "if": {
      child(node.condition, TYPE.BOOL, "condition");
      child(node.then, null, "then");
      child(node.else, null, "else");
      return expectedType ?? TYPE.JSON;
    }
    case "lambda": {
      if (!Array.isArray(node.params) || node.params.some((name) => typeof name !== "string")) {
        fail(`${path}.params must be a string array`, { code: "schema" });
      }
      child(node.body, null, "body");
      return ensureResult(null);
    }
    case "var":
      assertString(node.name, `${path}.name`);
      return expectedType ?? null;
    case "let":
      assertString(node.name, `${path}.name`);
      child(node.value, null, "value");
      return ensureResult(child(node.body, expectedType, "body"));
    case "block":
    case "seq":
      if (!Array.isArray(node.items)) {
        fail(`${path}.items must be an array`, { code: "schema" });
      }
      node.items.forEach((item, index) => child(item, null, `items[${index}]`));
      return expectedType ?? TYPE.JSON;
    case "return":
      return ensureResult(child(node.value, expectedType, "value"));
    case "catch-return":
      return ensureResult(child(node.body, expectedType, "body"));
    case "break":
    case "continue":
      return TYPE.NULL;
    case "try":
      child(node.body, expectedType, "body");
      return ensureResult(child(node.catch, expectedType, "catch"));
    case "for-of":
      if (!Array.isArray(node.names) || node.names.some((name) => typeof name !== "string")) {
        fail(`${path}.names must be a string array`, { code: "schema" });
      }
      child(node.value, null, "value");
      child(node.body, null, "body");
      return TYPE.NULL;
    case "assign-var":
      assertString(node.name, `${path}.name`);
      return ensureResult(child(node.value, expectedType, "value"));
    case "assign-prop":
      child(node.object, null, "object");
      child(node.key, TYPE.STRING, "key");
      return ensureResult(child(node.value, null, "value"));
    case "truthy":
      child(node.value, null, "value");
      return ensureResult(TYPE.BOOL);
    case "loose-eq":
    case "loose-ne":
      child(node.left, null, "left");
      child(node.right, null, "right");
      return ensureResult(TYPE.BOOL);
    case "typeof":
      child(node.value, null, "value");
      return ensureResult(TYPE.STRING);
    case "object-assign":
      if (!Array.isArray(node.parts)) {
        fail(`${path}.parts must be an array`, { code: "schema" });
      }
      node.parts.forEach((part, index) => child(part, null, `parts[${index}]`));
      return expectedType ?? TYPE.SUGGESTION;
    case "object":
    case "spread":
    case "json-object":
      if (op === "spread") child(node.value, null, "value");
      if (!Array.isArray(node.fields)) {
        fail(`${path}.fields must be an array`, { code: "schema" });
      }
      node.fields.forEach((field, index) => {
        if (!field || typeof field !== "object") {
          fail(`${path}.fields[${index}] must be an object`, { code: "schema" });
        }
        if (typeof field.key !== "string") {
          fail(`${path}.fields[${index}].key must be a string`, { code: "schema" });
        }
        if (
          op !== "json-object" &&
          node.fields.length > 0 &&
          !SUGGESTION_KEYS.includes(field.key)
        ) {
          fail(`${path}.fields[${index}].key is not a suggestion field`, {
            code: "schema",
          });
        }
        child(field.value, null, `fields[${index}].value`);
      });
      return ensureResult(op === "json-object" ? TYPE.JSON : TYPE.SUGGESTION);
    case "get":
    case "json-get":
      child(node.value, null, "value");
      child(node.key, TYPE.STRING, "key");
      return expectedType ?? TYPE.JSON;
    case "object-keys":
      child(node.value, null, "value");
      return ensureResult(TYPE.STRING_ARRAY);
    case "object-entries":
    case "array-entries":
      child(node.value, null, "value");
      return ensureResult(TYPE.VALUE_ARRAY);
    case "array-map":
    case "array-filter":
    case "array-flat-map":
    case "array-some":
    case "array-every":
    case "array-find":
    case "array-find-index":
    case "array-sort":
      child(node.value, null, "value");
      child(node.fn, null, "fn");
      if (op === "array-some" || op === "array-every") return ensureResult(TYPE.BOOL);
      if (op === "array-find-index") return ensureResult(TYPE.INTEGER);
      return expectedType ?? TYPE.VALUE_ARRAY;
    case "array-slice":
      child(node.value, null, "value");
      child(node.start, TYPE.INTEGER, "start");
      child(node.end, TYPE.INTEGER, "end");
      return expectedType ?? TYPE.VALUE_ARRAY;
    case "array-join":
      child(node.value, null, "value");
      child(node.separator, TYPE.STRING, "separator");
      return ensureResult(TYPE.STRING);
    case "array-index-of":
      child(node.value, null, "value");
      child(node.needle, null, "needle");
      return ensureResult(TYPE.INTEGER);
    case "array-concat":
      if (!Array.isArray(node.parts)) {
        fail(`${path}.parts must be an array`, { code: "schema" });
      }
      node.parts.forEach((part, index) => child(part, null, `parts[${index}]`));
      return expectedType ?? TYPE.VALUE_ARRAY;
    case "array-reverse":
    case "array-pop":
      child(node.value, null, "value");
      return expectedType ?? TYPE.VALUE_ARRAY;
    case "array-index":
      child(node.value, null, "value");
      child(node.index, null, "index");
      return expectedType ?? TYPE.JSON;
    case "array-push":
      assertString(node.name, `${path}.name`);
      child(node.item, null, "item");
      return ensureResult(TYPE.INTEGER);
    case "json-parse":
    case "json-array-items":
    case "json-as-string":
    case "json-as-number":
    case "json-as-bool":
      child(node.value, null, "value");
      if (op === "json-as-string") return ensureResult(TYPE.STRING);
      if (op === "json-as-number") return ensureResult(TYPE.INTEGER);
      if (op === "json-as-bool") return ensureResult(TYPE.BOOL);
      if (op === "json-array-items") return ensureResult(TYPE.JSON_ARRAY);
      return ensureResult(TYPE.JSON);
    case "regex-test":
      child(node.value, TYPE.STRING, "value");
      assertString(node.pattern, `${path}.pattern`);
      assertString(node.flags, `${path}.flags`);
      assertTypedRegexLiteral(node.pattern, node.flags);
      return ensureResult(TYPE.BOOL);
    case "regex-match":
    case "regex-match-all":
    case "string-split-regex":
      child(node.value, TYPE.STRING, "value");
      assertString(node.pattern, `${path}.pattern`);
      assertString(node.flags, `${path}.flags`);
      assertTypedRegexLiteral(node.pattern, node.flags);
      return ensureResult(op === "string-split-regex" ? TYPE.STRING_ARRAY : TYPE.VALUE_ARRAY);
    case "regex-replace":
      child(node.value, TYPE.STRING, "value");
      assertString(node.pattern, `${path}.pattern`);
      assertString(node.flags, `${path}.flags`);
      assertTypedRegexLiteral(node.pattern, node.flags);
      child(node.replacement, TYPE.STRING, "replacement");
      return ensureResult(TYPE.STRING);
    case "math-max":
      child(node.values, null, "values");
      return ensureResult(TYPE.INTEGER);
    case "string-set":
      child(node.value, null, "value");
      return ensureResult(TYPE.STRING_SET);
    case "regex":
      assertString(node.pattern, `${path}.pattern`);
      assertString(node.flags, `${path}.flags`);
      assertTypedRegexLiteral(node.pattern, node.flags);
      return ensureResult(TYPE.REGEX);
    case "while":
      child(node.condition, TYPE.BOOL, "condition");
      child(node.body, null, "body");
      return TYPE.NULL;
    case "array-flat":
      child(node.value, null, "value");
      child(node.depth, TYPE.INTEGER, "depth");
      return expectedType ?? TYPE.VALUE_ARRAY;
    case "to-string":
      child(node.value, null, "value");
      return ensureResult(TYPE.STRING);
    case "json-stringify":
      child(node.value, null, "value");
      return ensureResult(TYPE.STRING);
    case "locale-compare":
      child(node.left, TYPE.STRING, "left");
      child(node.right, TYPE.STRING, "right");
      return ensureResult(TYPE.INTEGER);
    case "array-from":
      child(node.value, null, "value");
      return expectedType ?? TYPE.VALUE_ARRAY;
    case "string-split-limit":
      child(node.value, TYPE.STRING, "value");
      child(node.separator, TYPE.STRING, "separator");
      child(node.limit, TYPE.INTEGER, "limit");
      return ensureResult(TYPE.STRING_ARRAY);
    case "array-shift":
      assertString(node.name, `${path}.name`);
      return expectedType ?? TYPE.JSON;
    case "string-index-of-from":
      child(node.value, TYPE.STRING, "value");
      child(node.needle, TYPE.STRING, "needle");
      child(node.start, TYPE.INTEGER, "start");
      return ensureResult(TYPE.INTEGER);
    case "array-index-of-from":
      child(node.value, null, "value");
      child(node.needle, null, "needle");
      child(node.start, TYPE.INTEGER, "start");
      return ensureResult(TYPE.INTEGER);
    case "string-set-add":
      assertString(node.name, `${path}.name`);
      child(node.item, TYPE.STRING, "item");
      return ensureResult(TYPE.STRING_SET);
    case "object-values":
      child(node.value, null, "value");
      return ensureResult(TYPE.VALUE_ARRAY);
    case "regex-search":
      child(node.value, TYPE.STRING, "value");
      assertString(node.pattern, `${path}.pattern`);
      assertString(node.flags, `${path}.flags`);
      assertTypedRegexLiteral(node.pattern, node.flags);
      return ensureResult(TYPE.INTEGER);
    case "string-slice-range":
      child(node.value, TYPE.STRING, "value");
      child(node.start, TYPE.INTEGER, "start");
      child(node.end, TYPE.INTEGER, "end");
      return ensureResult(TYPE.STRING);
    default:
      fail(`unhandled expression operation ${op}`, { code: "schema" });
  }
}

/** Validate a generated descriptor using the same closed schema as the compiler. */
export function validateTypedHookIr(value) {
  exactKeys(
    value,
    ["version", "kind", "sourceField", "resultType", "params", "expr"],
    "hook",
  );
  if (value.version !== TYPED_HOOK_IR_VERSION)
    fail("hook.version is unsupported", { code: "schema" });
  if (value.kind !== TYPED_HOOK_IR_KIND)
    fail("hook.kind is unsupported", { code: "schema" });
  const contract =
    typeof value.sourceField === "string" &&
    Object.hasOwn(TYPED_HOOK_CONTRACTS, value.sourceField)
      ? TYPED_HOOK_CONTRACTS[value.sourceField]
      : undefined;
  if (!contract)
    fail(`hook.sourceField ${String(value.sourceField)} is unsupported`, {
      code: "schema",
    });
  if (value.resultType !== contract.resultType)
    fail("hook.resultType does not match field contract", { code: "schema" });
  if (
    !Array.isArray(value.params) ||
    value.params.length !== contract.params.length
  ) {
    fail("hook.params does not match field contract", { code: "schema" });
  }
  value.params.forEach((param, index) => {
    exactKeys(param, ["index", "type"], `hook.params[${index}]`);
    if (param.index !== index || param.type !== contract.params[index]) {
      fail(`hook.params[${index}] does not match field contract`, {
        code: "schema",
      });
    }
  });
  validateExpression(value.expr, value.resultType, contract.params);
  assertSerializedSize(value, "hook");
  return true;
}

function safeIntegerResult(value, path) {
  if (!Number.isSafeInteger(value)) {
    fail(`${path} overflowed the JavaScript safe integer range`, {
      code: "overflow",
    });
  }
  return value;
}

function jsSlice(value, start) {
  const length = value.length;
  const index = start < 0 ? Math.max(length + start, 0) : Math.min(start, length);
  return value.slice(index);
}

function jsSubstring(value, start, end) {
  const length = value.length;
  const clamp = (n) => {
    if (n < 0) return 0;
    if (n > length) return length;
    return n;
  };
  let from = clamp(start);
  let to = clamp(end);
  if (from > to) {
    const swap = from;
    from = to;
    to = swap;
  }
  return value.slice(from, to);
}

function jsLastIndexOf(value, needle) {
  return value.lastIndexOf(needle);
}

function boundedString(value, path) {
  if (value.length > MAX_STRING_CODE_UNITS) {
    fail(`${path} exceeds the UTF-16 code-unit limit`, {
      code: "complexity",
    });
  }
  return value;
}

function jsReplace(value, needle, replacement, all) {
  return boundedString(
    all ? value.replaceAll(needle, replacement) : value.replace(needle, replacement),
    all ? "string-replace-all" : "string-replace",
  );
}

function jsPad(value, target, pad, end) {
  if (target <= value.length) return value;
  if (pad.length === 0) return value;
  if (target > MAX_STRING_CODE_UNITS) {
    fail("string-pad exceeds the UTF-16 code-unit limit", {
      code: "complexity",
    });
  }
  const needed = target - value.length;
  let fill = "";
  while (fill.length < needed) fill += pad;
  fill = fill.slice(0, needed);
  return end ? value + fill : fill + value;
}

function jsCharAt(value, index) {
  if (index < 0 || index >= value.length) return "";
  return value.charAt(index);
}

function jsAt(value, index) {
  const actual = index < 0 ? value.length + index : index;
  if (actual < 0 || actual >= value.length) return "";
  return value.charAt(actual);
}

function jsRepeat(value, count) {
  if (count < 0) {
    fail("string-repeat count must be non-negative", { code: "overflow" });
  }
  const units = safeIntegerResult(value.length * count, "string-repeat");
  if (units > MAX_STRING_CODE_UNITS) {
    fail("string-repeat exceeds the UTF-16 code-unit limit", {
      code: "complexity",
    });
  }
  return value.repeat(count);
}

class TypedCompletion {
  constructor(kind, value = null) {
    this.kind = kind;
    this.value = value;
  }
}

function asArray(value) {
  if (Array.isArray(value)) return value;
  if (value instanceof Set) return [...value];
  if (value && typeof value === "object" && Array.isArray(value.items)) {
    return value.items;
  }
  return null;
}

function applyLambda(fn, values, args, locals) {
  if (!fn || fn.op !== "lambda") {
    fail("array callback is not a lambda", { code: "schema" });
  }
  const childLocals = new Map(locals);
  fn.params.forEach((name, index) => {
    childLocals.set(name, values[index]);
  });
  try {
    return evaluateExpression(fn.body, args, childLocals);
  } catch (error) {
    if (error instanceof TypedCompletion && error.kind === "return") {
      return error.value;
    }
    throw error;
  }
}

function isTruthy(value) {
  return Boolean(value);
}

function evaluateExpression(node, args, locals = new Map()) {
  const ev = (child) => evaluateExpression(child, args, locals);
  switch (node.op) {
    case "arg":
      return args[node.index];
    case "string":
      return node.value;
    case "bool":
      return node.value;
    case "integer":
      return node.value;
    case "null":
      return null;
    case "array":
      return node.items.map((item) => ev(item));
    case "length": {
      const value = ev(node.value);
      if (typeof value === "string" || Array.isArray(value)) {
        return safeIntegerResult(value.length, "length");
      }
      if (value instanceof Set) return safeIntegerResult(value.size, "length");
      if (value && typeof value === "object") {
        return safeIntegerResult(Object.keys(value).length, "length");
      }
      return 0;
    }
    case "string-includes":
      return ev(node.value).includes(
        ev(node.needle),
      );
    case "string-index-of":
      return ev(node.value).indexOf(
        ev(node.needle),
      );
    case "string-last-index-of":
      return jsLastIndexOf(
        ev(node.value),
        ev(node.needle),
      );
    case "string-slice":
      return jsSlice(
        ev(node.value),
        ev(node.start),
      );
    case "string-slice-after-first": {
      const value = ev(node.value);
      const needle = ev(node.needle);
      const index = value.indexOf(needle);
      return index === -1 ? value : jsSlice(value, index + 1);
    }
    case "string-substring":
      return jsSubstring(
        ev(node.value),
        ev(node.start),
        ev(node.end),
      );
    case "string-split":
      return ev(node.value).split(
        ev(node.separator),
      );
    case "string-trim": {
      const value = ev(node.value);
      return value == null ? null : String(value).trim();
    }
    case "string-trim-start": {
      const value = ev(node.value);
      return value == null ? null : String(value).trimStart();
    }
    case "string-trim-end": {
      const value = ev(node.value);
      return value == null ? null : String(value).trimEnd();
    }
    case "string-replace":
      return jsReplace(
        ev(node.value),
        ev(node.needle),
        ev(node.replacement),
        false,
      );
    case "string-replace-all":
      return jsReplace(
        ev(node.value),
        ev(node.needle),
        ev(node.replacement),
        true,
      );
    case "string-starts-with":
      return ev(node.value).startsWith(
        ev(node.needle),
      );
    case "string-ends-with":
      return ev(node.value).endsWith(
        ev(node.needle),
      );
    case "string-to-lower":
      return ev(node.value).toLowerCase();
    case "string-to-upper":
      return ev(node.value).toUpperCase();
    case "string-pad-start":
      return jsPad(
        ev(node.value),
        ev(node.target),
        ev(node.pad),
        false,
      );
    case "string-pad-end":
      return jsPad(
        ev(node.value),
        ev(node.target),
        ev(node.pad),
        true,
      );
    case "string-repeat":
      return jsRepeat(
        ev(node.value),
        ev(node.count),
      );
    case "string-concat":
      return boundedString(
        node.parts
          .map((part) => ev(part))
          .join(""),
        "string-concat",
      );
    case "string-char-at":
      return jsCharAt(
        ev(node.value),
        ev(node.index),
      );
    case "string-at":
      return jsAt(
        ev(node.value),
        ev(node.index),
      );
    case "array-includes": {
      const value = ev(node.value);
      const needle = ev(node.needle);
      if (value instanceof Set) return value.has(needle);
      return (asArray(value) ?? []).includes(needle);
    }
    case "strict-eq":
      return (
        ev(node.left) ===
        ev(node.right)
      );
    case "strict-ne":
      return (
        ev(node.left) !==
        ev(node.right)
      );
    case "add":
      return safeIntegerResult(
        ev(node.left) +
          ev(node.right),
        "add",
      );
    case "sub":
      return safeIntegerResult(
        ev(node.left) -
          ev(node.right),
        "sub",
      );
    case "mul":
      return safeIntegerResult(
        ev(node.left) *
          ev(node.right),
        "mul",
      );
    case "lt":
      return (
        ev(node.left) <
        ev(node.right)
      );
    case "le":
      return (
        ev(node.left) <=
        ev(node.right)
      );
    case "gt":
      return (
        ev(node.left) >
        ev(node.right)
      );
    case "ge":
      return (
        ev(node.left) >=
        ev(node.right)
      );
    case "not":
      return !ev(node.value);
    case "nullish": {
      const left = ev(node.left);
      return left === null ? ev(node.right) : left;
    }
    case "and":
      return (
        ev(node.left) &&
        ev(node.right)
      );
    case "or": {
      const left = ev(node.left);
      if (left) return left;
      return ev(node.right);
    }
    case "if":
      return ev(node.condition) ? ev(node.then) : ev(node.else);
    case "lambda":
      return node;
    case "var":
      if (!locals.has(node.name)) {
        fail(`unbound variable ${node.name}`, { code: "free-variable" });
      }
      return locals.get(node.name);
    case "let": {
      const value = ev(node.value);
      const had = locals.has(node.name);
      const previous = locals.get(node.name);
      locals.set(node.name, value);
      try {
        return ev(node.body);
      } finally {
        if (had) locals.set(node.name, previous);
        else locals.delete(node.name);
      }
    }
    case "block":
    case "seq": {
      let last = null;
      for (const item of node.items) last = ev(item);
      return last;
    }
    case "return":
      throw new TypedCompletion("return", ev(node.value));
    case "catch-return":
      try {
        return ev(node.body);
      } catch (error) {
        if (error instanceof TypedCompletion && error.kind === "return") {
          return error.value;
        }
        throw error;
      }
    case "break":
      throw new TypedCompletion("break");
    case "continue":
      throw new TypedCompletion("continue");
    case "try":
      try {
        return ev(node.body);
      } catch (error) {
        if (error instanceof TypedCompletion) throw error;
        if (error instanceof TypedHookCompileError) throw error;
        return ev(node.catch);
      }
    case "for-of": {
      const iterable = ev(node.value);
      const items = asArray(iterable) ?? (iterable && typeof iterable[Symbol.iterator] === "function"
        ? [...iterable]
        : []);
      for (const item of items) {
        if (node.names.length === 2 && Array.isArray(item)) {
          locals.set(node.names[0], item[0]);
          locals.set(node.names[1], item[1]);
        } else {
          locals.set(node.names[0], item);
        }
        try {
          ev(node.body);
        } catch (error) {
          if (error instanceof TypedCompletion && error.kind === "continue") continue;
          if (error instanceof TypedCompletion && error.kind === "break") break;
          throw error;
        }
      }
      return null;
    }
    case "assign-var": {
      const value = ev(node.value);
      locals.set(node.name, value);
      return value;
    }
    case "assign-prop": {
      const object = ev(node.object);
      const key = ev(node.key);
      const value = ev(node.value);
      if (object && typeof object === "object") object[key] = value;
      return value;
    }
    case "truthy":
      return isTruthy(ev(node.value));
    case "loose-eq":
      return ev(node.left) == ev(node.right);
    case "loose-ne":
      return ev(node.left) != ev(node.right);
    case "typeof":
      return typeof ev(node.value);
    case "object": {
      const object = {};
      for (const field of node.fields) object[field.key] = ev(field.value);
      return object;
    }
    case "spread": {
      const object = { ...ev(node.value) };
      for (const field of node.fields) object[field.key] = ev(field.value);
      return object;
    }
    case "object-assign": {
      const object = {};
      for (const part of node.parts) Object.assign(object, ev(part) ?? {});
      return object;
    }
    case "get":
    case "json-get": {
      const value = ev(node.value);
      const key = ev(node.key);
      if (value == null) return null;
      return value[key] ?? null;
    }
    case "object-keys": {
      const value = ev(node.value);
      return value && typeof value === "object" ? Object.keys(value) : [];
    }
    case "object-entries": {
      const value = ev(node.value);
      return value && typeof value === "object" ? Object.entries(value) : [];
    }
    case "array-entries": {
      const value = asArray(ev(node.value)) ?? [];
      return value.map((item, index) => [index, item]);
    }
    case "array-map":
      return (asArray(ev(node.value)) ?? []).map((item, index) =>
        applyLambda(node.fn, [item, index], args, locals),
      );
    case "array-filter":
      return (asArray(ev(node.value)) ?? []).filter((item, index) =>
        isTruthy(applyLambda(node.fn, [item, index], args, locals)),
      );
    case "array-flat-map":
      return (asArray(ev(node.value)) ?? []).flatMap((item, index) => {
        const mapped = applyLambda(node.fn, [item, index], args, locals);
        return asArray(mapped) ?? [mapped];
      });
    case "array-some":
      return (asArray(ev(node.value)) ?? []).some((item, index) =>
        isTruthy(applyLambda(node.fn, [item, index], args, locals)),
      );
    case "array-every":
      return (asArray(ev(node.value)) ?? []).every((item, index) =>
        isTruthy(applyLambda(node.fn, [item, index], args, locals)),
      );
    case "array-find":
      return (
        (asArray(ev(node.value)) ?? []).find((item, index) =>
          isTruthy(applyLambda(node.fn, [item, index], args, locals)),
        ) ?? null
      );
    case "array-find-index":
      return (asArray(ev(node.value)) ?? []).findIndex((item, index) =>
        isTruthy(applyLambda(node.fn, [item, index], args, locals)),
      );
    case "array-sort": {
      const value = [...(asArray(ev(node.value)) ?? [])];
      value.sort((left, right) => {
        const result = applyLambda(node.fn, [left, right], args, locals);
        return typeof result === "number" ? result : 0;
      });
      return value;
    }
    case "array-slice": {
      const value = asArray(ev(node.value)) ?? [];
      return value.slice(ev(node.start), ev(node.end));
    }
    case "array-join":
      return (asArray(ev(node.value)) ?? []).join(ev(node.separator));
    case "array-index-of": {
      const value = asArray(ev(node.value)) ?? [];
      return value.indexOf(ev(node.needle));
    }
    case "array-concat": {
      const parts = node.parts.map((part) => ev(part));
      return parts.flatMap((part) => asArray(part) ?? [part]);
    }
    case "array-reverse":
      return [...(asArray(ev(node.value)) ?? [])].reverse();
    case "array-pop": {
      const value = [...(asArray(ev(node.value)) ?? [])];
      return value.pop() ?? null;
    }
    case "array-index": {
      const value = ev(node.value);
      const index = ev(node.index);
      if (value == null) return null;
      return value[index] ?? null;
    }
    case "array-push": {
      const current = locals.get(node.name);
      const list = asArray(current) ?? [];
      list.push(ev(node.item));
      locals.set(node.name, list);
      return list.length;
    }
    case "json-parse":
      try {
        return JSON.parse(ev(node.value));
      } catch {
        return null;
      }
    case "json-array-items": {
      const value = ev(node.value);
      return Array.isArray(value) ? value : [];
    }
    case "json-as-string": {
      const value = ev(node.value);
      return typeof value === "string" ? value : null;
    }
    case "json-as-number": {
      const value = ev(node.value);
      if (Number.isSafeInteger(value)) return value;
      const parsed = Number(value);
      return Number.isSafeInteger(parsed) ? parsed : null;
    }
    case "json-as-bool": {
      const value = ev(node.value);
      return typeof value === "boolean" ? value : null;
    }
    case "regex-test":
      return regexTest(node.pattern, node.flags, ev(node.value));
    case "regex-match":
      return regexMatch(node.pattern, node.flags, ev(node.value));
    case "regex-match-all":
      return regexMatchAll(node.pattern, node.flags, ev(node.value));
    case "regex-replace":
      return boundedString(
        regexReplace(node.pattern, node.flags, ev(node.value), ev(node.replacement)),
        "regex-replace",
      );
    case "string-split-regex":
      return stringSplitRegex(node.pattern, node.flags, ev(node.value));
    case "math-max": {
      const values = ev(node.values);
      const numbers = (asArray(values) ?? [values]).map((item) => Number(item));
      return safeIntegerResult(Math.max(...numbers), "math-max");
    }
    case "string-set":
      return new Set(asArray(ev(node.value)) ?? []);
    case "string-slice-range":
      return ev(node.value).slice(ev(node.start), ev(node.end));
    case "regex":
      return { __typedRegex: true, pattern: node.pattern, flags: node.flags };
    case "while": {
      let guard = 0;
      while (isTruthy(ev(node.condition))) {
        guard += 1;
        if (guard > 100_000) {
          fail("while loop exceeded iteration cap", { code: "complexity" });
        }
        try {
          ev(node.body);
        } catch (error) {
          if (error instanceof TypedCompletion && error.kind === "continue") continue;
          if (error instanceof TypedCompletion && error.kind === "break") break;
          throw error;
        }
      }
      return null;
    }
    case "array-flat": {
      const value = asArray(ev(node.value)) ?? [];
      return value.flat(ev(node.depth));
    }
    case "json-object": {
      const object = {};
      for (const field of node.fields) object[field.key] = ev(field.value);
      return object;
    }
    case "to-string": {
      const value = ev(node.value);
      if (value == null) return "";
      return String(value);
    }
    case "json-stringify":
      return JSON.stringify(ev(node.value));
    case "locale-compare":
      return ev(node.left).localeCompare(ev(node.right));
    case "array-from": {
      const value = ev(node.value);
      if (value && typeof value === "object" && !Array.isArray(value) && !(value instanceof Set) && "length" in value) {
        const count = Number(value.length);
        if (!Number.isSafeInteger(count) || count <= 0) return [];
        return Array.from({ length: Math.min(count, 10_000) });
      }
      return Array.from(value ?? []);
    }
    case "string-split-limit":
      return ev(node.value).split(ev(node.separator), ev(node.limit));
    case "array-shift": {
      const current = locals.get(node.name);
      const list = asArray(current) ?? [];
      const first = list.shift() ?? null;
      locals.set(node.name, list);
      return first;
    }
    case "string-index-of-from":
      return ev(node.value).indexOf(ev(node.needle), ev(node.start));
    case "array-index-of-from": {
      const value = asArray(ev(node.value)) ?? [];
      return value.indexOf(ev(node.needle), ev(node.start));
    }
    case "string-set-add": {
      const current = locals.get(node.name);
      const set = current instanceof Set ? current : new Set(asArray(current) ?? []);
      set.add(ev(node.item));
      locals.set(node.name, set);
      return set;
    }
    case "object-values": {
      const value = ev(node.value);
      if (value instanceof Map) return [...value.values()];
      return Object.values(value ?? {});
    }
    case "regex-search":
      return ev(node.value).search(new RegExp(node.pattern, node.flags));
    default:
      fail(`unhandled expression operation ${node.op}`, { code: "schema" });
  }
}

/**
 * Evaluate a validated typed-hook descriptor.  Arguments are raw JS values
 * matching the field contract (strings, bools, integers, string arrays, or
 * null).  The result is the same closed JSON-friendly value the Rust
 * evaluator must produce.
 */
function assertEvaluateArg(value, type, index) {
  switch (type) {
    case TYPE.STRING:
      if (typeof value !== "string") {
        fail(`args[${index}] must be a string`, { code: "input" });
      }
      return;
    case TYPE.BOOL:
      if (typeof value !== "boolean") {
        fail(`args[${index}] must be a bool`, { code: "input" });
      }
      return;
    case TYPE.INTEGER:
      if (!Number.isSafeInteger(value)) {
        fail(`args[${index}] must be a safe integer`, { code: "input" });
      }
      return;
    case TYPE.STRING_ARRAY:
      if (
        !Array.isArray(value) ||
        value.some((item) => typeof item !== "string")
      ) {
        fail(`args[${index}] must be a string array`, { code: "input" });
      }
      return;
    case TYPE.NULL:
      if (value !== null) {
        fail(`args[${index}] must be null`, { code: "input" });
      }
      return;
    case TYPE.SUGGESTION_ARRAY:
      if (!Array.isArray(value)) {
        fail(`args[${index}] must be a suggestion array`, { code: "input" });
      }
      return;
    case TYPE.JSON:
    case TYPE.JSON_ARRAY:
    case TYPE.SUGGESTION:
    case TYPE.STRING_RECORD:
    case TYPE.STRING_SET:
    case TYPE.VALUE_ARRAY:
    case TYPE.REGEX:
      return;
    default:
      fail(`args[${index}] type ${type} is compile-only until later ops`, {
        code: "input",
      });
  }
}

export function evaluateTypedHook(descriptor, args) {
  validateTypedHookIr(descriptor);
  if (!Array.isArray(args) || args.length !== descriptor.params.length) {
    fail("evaluateTypedHook args must match the field contract", {
      code: "input",
    });
  }
  descriptor.params.forEach((param, index) =>
    assertEvaluateArg(args[index], param.type, index),
  );
  try {
    return evaluateExpression(descriptor.expr, args, new Map());
  } catch (error) {
    if (error instanceof TypedCompletion && error.kind === "return") {
      return error.value;
    }
    throw error;
  }
}

function normalizeSuggestionValueJson(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return value;
  }
  const out = {};
  for (const [key, child] of Object.entries(value)) {
    if (child == null) continue;
    if (key === "priority" && typeof child !== "number") continue;
    if (Array.isArray(child) && child.length === 0) continue;
    if (key !== "name" && typeof child === "string" && child === "") continue;
    out[key] = child;
  }
  return out;
}

function normalizeSuggestionArrayJson(value) {
  if (!Array.isArray(value)) return value;
  return value.map(normalizeSuggestionValueJson).filter((item) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) return false;
    const name = item.name;
    if (typeof name === "string") return name !== "undefined";
    if (Array.isArray(name)) return name.length > 0;
    return name != null;
  });
}

/** JSON shape `evaluate_typed_hook_json` compares against on the Rust side. */
export function evaluateTypedHookJson(descriptor, args) {
  const value = evaluateTypedHook(descriptor, args);
  const cloned = JSON.parse(JSON.stringify(value));
  if (descriptor.resultType === "suggestion-array") {
    return normalizeSuggestionArrayJson(cloned);
  }
  if (descriptor.resultType === "suggestion") {
    return normalizeSuggestionValueJson(cloned);
  }
  return cloned;
}
