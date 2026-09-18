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
  "json",
  "suggestion",
  "suggestion-array",
  "string-record",
  "null",
]);

// Trigger remains the production sidecar field.  The other contracts are the
// T2.1/T2.3 typed-hook shapes: `compileTypedHook` accepts them so research
// and tests can compile those fields, but the production sidecar writer still
// emits trigger only until T2.3.
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
]);

const TYPE = Object.freeze({
  STRING: "string",
  BOOL: "bool",
  INTEGER: "integer",
  STRING_ARRAY: "string-array",
  JSON: "json",
  SUGGESTION: "suggestion",
  SUGGESTION_ARRAY: "suggestion-array",
  STRING_RECORD: "string-record",
  NULL: "null",
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
      if (node.regex || node.bigint != null) {
        fail("regular expression and bigint literals are unsupported", {
          code: "unsupported-literal",
          nodeType: node.type,
        });
      }
      return finish(literalExpression(node.value, "literal"));
    }
    case "UnaryExpression": {
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
      const operand = child(node.argument, TYPE.BOOL);
      return finish({
        type: TYPE.BOOL,
        expr: { op: "not", value: operand.expr },
      });
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
        if (receiver.type !== TYPE.STRING) {
          fail(`.${method} is supported only on strings`, {
            code: "type-mismatch",
            nodeType: node.type,
          });
        }
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
        return finish({
          type: TYPE.STRING,
          expr: { op, value: receiver.expr },
        });
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
      if (method === "concat") {
        requireStringReceiver();
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
      if (method === "charAt" || method === "at") {
        requireStringReceiver();
        const index = oneArgument(TYPE.INTEGER);
        return finish({
          type: TYPE.STRING,
          expr: {
            op: method === "charAt" ? "string-char-at" : "string-at",
            value: receiver.expr,
            index: index.expr,
          },
        });
      }
      fail(`method .${method} is not in the typed hook allowlist`, {
        code: "unknown-call",
        nodeType: node.type,
      });
    }
    case "BinaryExpression": {
      if (
        !["===", "!==", ">", ">=", "<", "<=", "+", "-", "*"].includes(
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
        if (left.type === TYPE.STRING && right.type === TYPE.STRING) {
          return finish({
            type: TYPE.STRING,
            expr: { op: "string-concat", parts: [left.expr, right.expr] },
          });
        }
        fail("addition requires two integers or two strings", {
          code: "type-mismatch",
          nodeType: node.type,
        });
      }
      if (node.operator === "-" || node.operator === "*") {
        const left = child(node.left, TYPE.INTEGER);
        const right = child(node.right, TYPE.INTEGER);
        return finish({
          type: TYPE.INTEGER,
          expr: {
            op: node.operator === "-" ? "sub" : "mul",
            left: left.expr,
            right: right.expr,
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
      if (node.operator === "??") {
        const left = child(node.left);
        const right = child(node.right);
        let resultType = left.type;
        if (left.type === TYPE.NULL) resultType = right.type;
        else if (right.type === TYPE.NULL) resultType = left.type;
        else if (left.type !== right.type) {
          fail("?? operands must share a type or be null", {
            code: "type-mismatch",
            nodeType: node.type,
          });
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
          parts.push(child(node.expressions[index], TYPE.STRING).expr);
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
    case "null":
      return ensureResult(TYPE.NULL);
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
        fail(`${path} ?? operands must share a type or be null`, {
          code: "type-mismatch",
        });
      }
      return ensureResult(resultType);
    }
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

function evaluateExpression(node, arguments_) {
  switch (node.op) {
    case "arg":
      return arguments_[node.index];
    case "string":
      return node.value;
    case "bool":
      return node.value;
    case "integer":
      return node.value;
    case "null":
      return null;
    case "array":
      return node.items.map((item) => evaluateExpression(item, arguments_));
    case "length": {
      const value = evaluateExpression(node.value, arguments_);
      return safeIntegerResult(value.length, "length");
    }
    case "string-includes":
      return evaluateExpression(node.value, arguments_).includes(
        evaluateExpression(node.needle, arguments_),
      );
    case "string-index-of":
      return evaluateExpression(node.value, arguments_).indexOf(
        evaluateExpression(node.needle, arguments_),
      );
    case "string-last-index-of":
      return jsLastIndexOf(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.needle, arguments_),
      );
    case "string-slice":
      return jsSlice(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.start, arguments_),
      );
    case "string-slice-after-first": {
      const value = evaluateExpression(node.value, arguments_);
      const needle = evaluateExpression(node.needle, arguments_);
      const index = value.indexOf(needle);
      return index === -1 ? value : jsSlice(value, index + 1);
    }
    case "string-substring":
      return jsSubstring(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.start, arguments_),
        evaluateExpression(node.end, arguments_),
      );
    case "string-split":
      return evaluateExpression(node.value, arguments_).split(
        evaluateExpression(node.separator, arguments_),
      );
    case "string-trim":
      return evaluateExpression(node.value, arguments_).trim();
    case "string-trim-start":
      return evaluateExpression(node.value, arguments_).trimStart();
    case "string-trim-end":
      return evaluateExpression(node.value, arguments_).trimEnd();
    case "string-replace":
      return jsReplace(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.needle, arguments_),
        evaluateExpression(node.replacement, arguments_),
        false,
      );
    case "string-replace-all":
      return jsReplace(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.needle, arguments_),
        evaluateExpression(node.replacement, arguments_),
        true,
      );
    case "string-starts-with":
      return evaluateExpression(node.value, arguments_).startsWith(
        evaluateExpression(node.needle, arguments_),
      );
    case "string-ends-with":
      return evaluateExpression(node.value, arguments_).endsWith(
        evaluateExpression(node.needle, arguments_),
      );
    case "string-to-lower":
      return evaluateExpression(node.value, arguments_).toLowerCase();
    case "string-to-upper":
      return evaluateExpression(node.value, arguments_).toUpperCase();
    case "string-pad-start":
      return jsPad(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.target, arguments_),
        evaluateExpression(node.pad, arguments_),
        false,
      );
    case "string-pad-end":
      return jsPad(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.target, arguments_),
        evaluateExpression(node.pad, arguments_),
        true,
      );
    case "string-repeat":
      return jsRepeat(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.count, arguments_),
      );
    case "string-concat":
      return boundedString(
        node.parts
          .map((part) => evaluateExpression(part, arguments_))
          .join(""),
        "string-concat",
      );
    case "string-char-at":
      return jsCharAt(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.index, arguments_),
      );
    case "string-at":
      return jsAt(
        evaluateExpression(node.value, arguments_),
        evaluateExpression(node.index, arguments_),
      );
    case "array-includes":
      return evaluateExpression(node.value, arguments_).includes(
        evaluateExpression(node.needle, arguments_),
      );
    case "strict-eq":
      return (
        evaluateExpression(node.left, arguments_) ===
        evaluateExpression(node.right, arguments_)
      );
    case "strict-ne":
      return (
        evaluateExpression(node.left, arguments_) !==
        evaluateExpression(node.right, arguments_)
      );
    case "add":
      return safeIntegerResult(
        evaluateExpression(node.left, arguments_) +
          evaluateExpression(node.right, arguments_),
        "add",
      );
    case "sub":
      return safeIntegerResult(
        evaluateExpression(node.left, arguments_) -
          evaluateExpression(node.right, arguments_),
        "sub",
      );
    case "mul":
      return safeIntegerResult(
        evaluateExpression(node.left, arguments_) *
          evaluateExpression(node.right, arguments_),
        "mul",
      );
    case "lt":
      return (
        evaluateExpression(node.left, arguments_) <
        evaluateExpression(node.right, arguments_)
      );
    case "le":
      return (
        evaluateExpression(node.left, arguments_) <=
        evaluateExpression(node.right, arguments_)
      );
    case "gt":
      return (
        evaluateExpression(node.left, arguments_) >
        evaluateExpression(node.right, arguments_)
      );
    case "ge":
      return (
        evaluateExpression(node.left, arguments_) >=
        evaluateExpression(node.right, arguments_)
      );
    case "not":
      return !evaluateExpression(node.value, arguments_);
    case "nullish": {
      const left = evaluateExpression(node.left, arguments_);
      return left === null ? evaluateExpression(node.right, arguments_) : left;
    }
    case "and":
      return (
        evaluateExpression(node.left, arguments_) &&
        evaluateExpression(node.right, arguments_)
      );
    case "or":
      return (
        evaluateExpression(node.left, arguments_) ||
        evaluateExpression(node.right, arguments_)
      );
    case "if":
      return evaluateExpression(node.condition, arguments_)
        ? evaluateExpression(node.then, arguments_)
        : evaluateExpression(node.else, arguments_);
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
  return evaluateExpression(descriptor.expr, args);
}
