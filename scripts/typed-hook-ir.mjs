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

export const TYPED_HOOK_IR_VERSION = 1;
export const TYPED_HOOK_IR_KIND = "typed-hook-expression";

export const TYPED_VALUE_TYPES = Object.freeze([
  "string",
  "bool",
  "integer",
  "string-array",
]);

// Trigger is the production typed-hook slice.  `getQueryTerm` is a research
// contract only: its descriptor is captured in a separate differential
// baseline and is deliberately not emitted into the production sidecar or
// selected by the runtime.  Keeping the contract here lets the compiler and
// Rust research evaluator share one closed schema while an unsupported field
// still fails closed instead of producing an untyped node.
export const TYPED_HOOK_CONTRACTS = Object.freeze({
  trigger: Object.freeze({
    params: Object.freeze(["string", "string"]),
    resultType: "bool",
  }),
  getQueryTerm: Object.freeze({
    params: Object.freeze(["string"]),
    resultType: "string",
  }),
});

export const TYPED_EXPRESSION_OPERATIONS = Object.freeze([
  "arg",
  "string",
  "bool",
  "integer",
  "array",
  "length",
  "string-includes",
  "string-index-of",
  "string-slice",
  "string-slice-after-first",
  "string-split",
  "array-includes",
  "strict-eq",
  "strict-ne",
  "gt",
  "and",
  "or",
  "if",
]);

const TYPE = Object.freeze({
  STRING: "string",
  BOOL: "bool",
  INTEGER: "integer",
  STRING_ARRAY: "string-array",
});

const MAX_NODES = 64;
const MAX_DEPTH = 12;
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
  array: ["op", "items"],
  length: ["op", "value"],
  "string-includes": ["op", "value", "needle"],
  "string-index-of": ["op", "value", "needle"],
  "string-slice": ["op", "value", "start"],
  "string-slice-after-first": ["op", "value", "needle"],
  "string-split": ["op", "value", "separator"],
  "array-includes": ["op", "value", "needle"],
  "strict-eq": ["op", "left", "right"],
  "strict-ne": ["op", "left", "right"],
  gt: ["op", "left", "right"],
  and: ["op", "left", "right"],
  or: ["op", "left", "right"],
  if: ["op", "condition", "then", "else"],
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
  if (fn.params.some((parameter) => parameter.type !== "Identifier")) {
    fail("hook parameters must be simple identifiers", {
      code: "parameter-shape",
    });
  }
  const names = fn.params.map((parameter) => parameter.name);
  if (new Set(names).size !== names.length) {
    fail("duplicate hook parameter names are not representable", {
      code: "parameter-shape",
    });
  }
  let expression = fn.body;
  if (fn.body.type === "BlockStatement") {
    if (
      fn.body.body.length !== 1 ||
      fn.body.body[0].type !== "ReturnStatement" ||
      fn.body.body[0].argument == null
    ) {
      fail("function hook must have one return statement", {
        code: "function-shape",
        nodeType: fn.body.type,
      });
    }
    expression = fn.body.body[0].argument;
  }
  if (!expression || expression.type === "BlockStatement") {
    fail("hook must return an expression", { code: "function-shape" });
  }
  return { fn, names, expression, body };
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
      fail(
        `expression type ${result.type} does not match expected ${expectedType}`,
        { code: "type-mismatch", nodeType: node.type },
      );
    }
    return result;
  };

  switch (node.type) {
    case "Identifier": {
      const parameter = environment.get(node.name);
      if (!parameter) {
        fail(`free identifier ${node.name} is not representable`, {
          code: "free-variable",
          nodeType: node.type,
        });
      }
      return finish({
        type: parameter.type,
        expr: { op: "arg", index: parameter.index },
      });
    }
    case "Literal": {
      if (node.regex || node.bigint != null || node.value === null) {
        fail("regular expression, bigint, and null literals are unsupported", {
          code: "unsupported-literal",
          nodeType: node.type,
        });
      }
      return finish(literalExpression(node.value, "literal"));
    }
    case "UnaryExpression": {
      // The current trigger inventory contains `!0` only.  Fold numeric
      // constants at build time instead of adding JS truthiness to runtime.
      if (node.operator !== "!") {
        fail(`unary operator ${node.operator} is unsupported`, {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      if (
        node.argument.type !== "Literal" ||
        typeof node.argument.value !== "number"
      ) {
        fail("only constant numeric negation can be folded", {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      const value = Boolean(node.argument.value);
      return finish({ type: TYPE.BOOL, expr: { op: "bool", value: !value } });
    }
    case "MemberExpression": {
      const name = propertyName(node);
      if (name !== "length") {
        fail(`property .${name} is unsupported`, {
          code: "unsupported-property",
          nodeType: node.type,
        });
      }
      const receiver = child(node.object);
      if (
        receiver.type !== TYPE.STRING &&
        receiver.type !== TYPE.STRING_ARRAY
      ) {
        fail(".length is supported only on strings and string arrays", {
          code: "type-mismatch",
          nodeType: node.type,
        });
      }
      return finish({
        type: TYPE.INTEGER,
        expr: { op: "length", value: receiver.expr },
      });
    }
    case "CallExpression": {
      if (
        node.optional ||
        node.arguments.some((argument) => argument.type === "SpreadElement")
      ) {
        fail("optional and spread calls are unsupported", {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      if (node.callee.type !== "MemberExpression") {
        fail("only allowlisted methods may be called", {
          code: "unknown-call",
          nodeType: node.type,
        });
      }
      const method = propertyName(node.callee);
      const receiver = child(node.callee.object);
      const oneArgument = (expected) => {
        if (node.arguments.length !== 1) {
          fail(`${method} expects exactly one argument`, {
            code: "call-arity",
            nodeType: node.type,
          });
        }
        return child(node.arguments[0], expected);
      };
      if (method === "includes") {
        const needle = oneArgument(
          receiver.type === TYPE.STRING ? TYPE.STRING : TYPE.STRING,
        );
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
        fail(".includes is supported only on strings and string arrays", {
          code: "type-mismatch",
          nodeType: node.type,
        });
      }
      if (method === "indexOf") {
        if (receiver.type !== TYPE.STRING) {
          fail(".indexOf is supported only on strings", {
            code: "type-mismatch",
            nodeType: node.type,
          });
        }
        const needle = oneArgument(TYPE.STRING);
        return finish({
          type: TYPE.INTEGER,
          expr: {
            op: "string-index-of",
            value: receiver.expr,
            needle: needle.expr,
          },
        });
      }
      if (method === "slice") {
        if (receiver.type !== TYPE.STRING) {
          fail(".slice is supported only on strings", {
            code: "type-mismatch",
            nodeType: node.type,
          });
        }
        const start = oneArgument(TYPE.INTEGER);
        return finish({
          type: TYPE.STRING,
          expr: { op: "string-slice", value: receiver.expr, start: start.expr },
        });
      }
      if (method === "split") {
        if (receiver.type !== TYPE.STRING) {
          fail(".split is supported only on strings", {
            code: "type-mismatch",
            nodeType: node.type,
          });
        }
        const separator = oneArgument(TYPE.STRING);
        return finish({
          type: TYPE.STRING_ARRAY,
          expr: {
            op: "string-split",
            value: receiver.expr,
            separator: separator.expr,
          },
        });
      }
      fail(`method .${method} is not in the typed hook allowlist`, {
        code: "unknown-call",
        nodeType: node.type,
      });
    }
    case "BinaryExpression": {
      if (!["===", "!==", ">"].includes(node.operator)) {
        fail(`binary operator ${node.operator} is unsupported`, {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      if (node.operator === ">") {
        const left = child(node.left, TYPE.INTEGER);
        const right = child(node.right, TYPE.INTEGER);
        return finish({
          type: TYPE.BOOL,
          expr: { op: "gt", left: left.expr, right: right.expr },
        });
      }
      const left = child(node.left);
      const right = child(node.right, left.type);
      if (left.type === TYPE.STRING_ARRAY) {
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
      if (node.operator !== "&&" && node.operator !== "||") {
        fail(`logical operator ${node.operator} is unsupported`, {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      const left = child(node.left, TYPE.BOOL);
      const right = child(node.right, TYPE.BOOL);
      return finish({
        type: TYPE.BOOL,
        expr: {
          op: node.operator === "&&" ? "and" : "or",
          left: left.expr,
          right: right.expr,
        },
      });
    }
    case "ConditionalExpression": {
      const condition = child(node.test, TYPE.BOOL);
      const consequent = child(node.consequent);
      const alternate = child(node.alternate, consequent.type);
      return finish({
        type: consequent.type,
        expr: {
          op: "if",
          condition: condition.expr,
          then: consequent.expr,
          else: alternate.expr,
        },
      });
    }
    case "ArrayExpression": {
      if (
        node.elements.some(
          (element) => element == null || element.type === "SpreadElement",
        )
      ) {
        fail("array holes and spreads are unsupported", {
          code: "unsupported-syntax",
          nodeType: node.type,
        });
      }
      if (node.elements.length === 0) {
        return finish({
          type: TYPE.STRING_ARRAY,
          expr: { op: "array", items: [] },
        });
      }
      const items = node.elements.map((element) => child(element, TYPE.STRING));
      return finish({
        type: TYPE.STRING_ARRAY,
        expr: { op: "array", items: items.map((item) => item.expr) },
      });
    }
    default:
      fail(`AST node ${node.type} is not representable`, {
        code: "unsupported-syntax",
        nodeType: node.type,
      });
  }
}

function compileFunction({ body, parameterTypes, resultType }) {
  if (!Array.isArray(parameterTypes)) {
    fail("parameterTypes must be a type array", { code: "input" });
  }
  parameterTypes.forEach((type, index) =>
    assertType(type, `parameterTypes[${index}]`),
  );
  assertType(resultType, "resultType");
  const { fn, names, expression } = parseFunctionBody(body);
  if (fn.params.length > parameterTypes.length) {
    fail(
      `hook declares ${fn.params.length} parameters but contract provides ${parameterTypes.length}`,
      { code: "parameter-count" },
    );
  }
  const environment = new Map(
    names.map((name, index) => [name, { index, type: parameterTypes[index] }]),
  );
  const state = { nodes: 0 };
  const result = compileExpression(expression, environment, resultType, state);
  return { expression: result.expr, nodes: state.nodes };
}

function isStringLiteral(node, value) {
  return (
    node?.type === "Literal" &&
    typeof node.value === "string" &&
    node.value === value &&
    !node.regex &&
    node.bigint == null
  );
}

function isIntegerLiteral(node, value) {
  return (
    node?.type === "Literal" &&
    typeof node.value === "number" &&
    Number.isSafeInteger(node.value) &&
    node.value === value &&
    !node.regex &&
    node.bigint == null
  );
}

function isInputIdentifier(node, name) {
  return node?.type === "Identifier" && node.name === name;
}

function memberCall(node, receiverName, method, argumentPredicate) {
  if (
    node?.type !== "CallExpression" ||
    node.optional ||
    node.callee?.type !== "MemberExpression" ||
    node.callee.computed ||
    node.callee.object?.type !== "Identifier" ||
    node.callee.object.name !== receiverName ||
    node.callee.property?.type !== "Identifier" ||
    node.callee.property.name !== method ||
    node.arguments.length !== 1 ||
    node.arguments[0]?.type === "SpreadElement" ||
    !argumentPredicate(node.arguments[0])
  ) {
    return false;
  }
  return true;
}

/**
 * Match the one deliberately closed getQueryTerm research shape.
 *
 * This is intentionally separate from the general expression compiler.  In
 * particular, accepting a general integer `+` here would make a future hook
 * look equivalent while changing JavaScript coercion/overflow semantics.  The
 * only accepted source shape is:
 *
 *   n => n.includes("latest") ? n.slice(n.indexOf(":") + 1) : n
 */
function assertClosedGetQueryTermShape(body) {
  const { fn, names, expression } = parseFunctionBody(body);
  if (fn.params.length !== 1) {
    fail("getQueryTerm research hook must have exactly one parameter", {
      code: "parameter-count",
    });
  }
  const [name] = names;
  if (expression.type !== "ConditionalExpression") {
    fail("getQueryTerm research hook has an unsupported function shape", {
      code: "function-shape",
    });
  }
  if (
    !memberCall(
      expression.test,
      name,
      "includes",
      (argument) => isStringLiteral(argument, "latest"),
    )
  ) {
    fail("getQueryTerm research hook must test includes(\"latest\")", {
      code: "function-shape",
    });
  }

  const consequent = expression.consequent;
  if (
    consequent?.type !== "CallExpression" ||
    consequent.optional ||
    consequent.callee?.type !== "MemberExpression" ||
    consequent.callee.computed ||
    !isInputIdentifier(consequent.callee.object, name) ||
    consequent.callee.property?.type !== "Identifier" ||
    consequent.callee.property.name !== "slice" ||
    consequent.arguments.length !== 1
  ) {
    fail("getQueryTerm research hook must slice its input", {
      code: "function-shape",
    });
  }
  const start = consequent.arguments[0];
  if (
    start?.type !== "BinaryExpression" ||
    start.operator !== "+" ||
    !memberCall(
      start.left,
      name,
      "indexOf",
      (argument) => isStringLiteral(argument, ":"),
    ) ||
    !isIntegerLiteral(start.right, 1)
  ) {
    fail(
      "getQueryTerm research hook must use input.indexOf(\":\") + 1 as slice start",
      { code: "function-shape" },
    );
  }
  if (!isInputIdentifier(expression.alternate, name)) {
    fail("getQueryTerm research hook must return its input on the false branch", {
      code: "function-shape",
    });
  }
  return name;
}

/**
 * Compile the research-only getQueryTerm shape.  This is not used by the
 * production sidecar writer; callers must separately restrict which hook ids
 * are admitted to a baseline.
 */
export function compileTypedGetQueryTerm({ body } = {}) {
  assertClosedGetQueryTermShape(body);
  const descriptor = {
    version: TYPED_HOOK_IR_VERSION,
    kind: TYPED_HOOK_IR_KIND,
    sourceField: "getQueryTerm",
    resultType: "string",
    params: [{ index: 0, type: TYPE.STRING }],
    expr: {
      op: "if",
      condition: {
        op: "string-includes",
        value: { op: "arg", index: 0 },
        needle: { op: "string", value: "latest" },
      },
      then: {
        op: "string-slice-after-first",
        value: { op: "arg", index: 0 },
        needle: { op: "string", value: ":" },
      },
      else: { op: "arg", index: 0 },
    },
  };
  validateTypedHookIr(descriptor);
  return descriptor;
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
  if (sourceField === "getQueryTerm") {
    if (resultType !== undefined && resultType !== contract.resultType) {
      fail(
        `${sourceField} requires result type ${contract.resultType}, got ${resultType}`,
        { code: "type-mismatch" },
      );
    }
    return compileTypedGetQueryTerm({ body });
  }
  if (resultType !== undefined && resultType !== contract.resultType) {
    fail(
      `${sourceField} requires result type ${contract.resultType}, got ${resultType}`,
      { code: "type-mismatch" },
    );
  }
  const compiled = compileFunction({
    body,
    parameterTypes: contract.params,
    resultType: contract.resultType,
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
    case "array":
      if (!Array.isArray(node.items))
        fail(`${path}.items must be an array`, { code: "schema" });
      node.items.forEach((item, index) =>
        child(item, TYPE.STRING, `items[${index}]`),
      );
      return ensureResult(TYPE.STRING_ARRAY);
    case "length": {
      const type = child(node.value, null, "value");
      if (type !== TYPE.STRING && type !== TYPE.STRING_ARRAY) {
        fail(`${path}.value must be a string or string-array`, {
          code: "type-mismatch",
        });
      }
      return ensureResult(TYPE.INTEGER);
    }
    case "string-includes":
    case "string-index-of": {
      child(node.value, TYPE.STRING, "value");
      child(node.needle, TYPE.STRING, "needle");
      return ensureResult(op === "string-includes" ? TYPE.BOOL : TYPE.INTEGER);
    }
    case "string-slice":
      child(node.value, TYPE.STRING, "value");
      child(node.start, TYPE.INTEGER, "start");
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
      child(node.value, TYPE.STRING_ARRAY, "value");
      child(node.needle, TYPE.STRING, "needle");
      return ensureResult(TYPE.BOOL);
    case "strict-eq":
    case "strict-ne": {
      const leftType = child(node.left, null, "left");
      child(node.right, leftType, "right");
      if (leftType === TYPE.STRING_ARRAY) {
        fail(`${path} cannot compare string arrays by identity`, {
          code: "type-mismatch",
        });
      }
      return ensureResult(TYPE.BOOL);
    }
    case "gt":
      child(node.left, TYPE.INTEGER, "left");
      child(node.right, TYPE.INTEGER, "right");
      return ensureResult(TYPE.BOOL);
    case "and":
    case "or":
      child(node.left, TYPE.BOOL, "left");
      child(node.right, TYPE.BOOL, "right");
      return ensureResult(TYPE.BOOL);
    case "if": {
      child(node.condition, TYPE.BOOL, "condition");
      const thenType = child(node.then, null, "then");
      child(node.else, thenType, "else");
      return ensureResult(thenType);
    }
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
