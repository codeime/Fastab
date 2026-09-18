import assert from "node:assert/strict";
import test from "node:test";

import { readFile } from "node:fs/promises";

import {
  MAX_DEPTH,
  MAX_NODES,
  TYPED_EXPRESSION_OPERATIONS,
  TYPED_HOOK_CONTRACTS,
  TYPED_HOOK_IR_KIND,
  TYPED_HOOK_IR_VERSION,
  TYPED_VALUE_TYPES,
  TypedHookCompileError,
  compileTypedExpression,
  compileTypedGetQueryTerm,
  compileTypedHook,
  evaluateTypedHook,
  validateTypedHookIr,
} from "./typed-hook-ir.mjs";
import {
  TYPED_IR_V2_OPS_PATH,
  buildTypedIrV2OpsGolden,
  writeTypedIrV2OpsGolden,
} from "./typed-ir-v2-ops.mjs";

function trigger(body) {
  return compileTypedHook({ body, sourceField: "trigger" });
}

test("compiles all nine trigger body shapes into the closed IR", () => {
  const cases = [
    {
      body: `(e,t)=>e.length===0||t.length===0&&e.length>0`,
      expr: {
        op: "or",
        left: {
          op: "strict-eq",
          left: { op: "length", value: { op: "arg", index: 0 } },
          right: { op: "integer", value: 0 },
        },
        right: {
          op: "and",
          left: {
            op: "strict-eq",
            left: { op: "length", value: { op: "arg", index: 1 } },
            right: { op: "integer", value: 0 },
          },
          right: {
            op: "gt",
            left: { op: "length", value: { op: "arg", index: 0 } },
            right: { op: "integer", value: 0 },
          },
        },
      },
    },
    {
      body: `(t,i)=>t.split(",").length!==i.split(",").length`,
      expr: {
        op: "strict-ne",
        left: {
          op: "length",
          value: {
            op: "string-split",
            value: { op: "arg", index: 0 },
            separator: { op: "string", value: "," },
          },
        },
        right: {
          op: "length",
          value: {
            op: "string-split",
            value: { op: "arg", index: 1 },
            separator: { op: "string", value: "," },
          },
        },
      },
    },
    {
      body: `()=>!0`,
      expr: { op: "bool", value: true },
    },
    {
      body: `(r,e)=>r.indexOf("/")!==e.indexOf("/")`,
      expr: {
        op: "strict-ne",
        left: {
          op: "string-index-of",
          value: { op: "arg", index: 0 },
          needle: { op: "string", value: "/" },
        },
        right: {
          op: "string-index-of",
          value: { op: "arg", index: 1 },
          needle: { op: "string", value: "/" },
        },
      },
    },
    {
      body: `e=>e==="-g"||e==="--global"`,
      expr: {
        op: "or",
        left: {
          op: "strict-eq",
          left: { op: "arg", index: 0 },
          right: { op: "string", value: "-g" },
        },
        right: {
          op: "strict-eq",
          left: { op: "arg", index: 0 },
          right: { op: "string", value: "--global" },
        },
      },
    },
    {
      body: `(n,t)=>n.split(":").length!==t.split(":").length`,
      expr: {
        op: "strict-ne",
        left: {
          op: "length",
          value: {
            op: "string-split",
            value: { op: "arg", index: 0 },
            separator: { op: "string", value: ":" },
          },
        },
        right: {
          op: "length",
          value: {
            op: "string-split",
            value: { op: "arg", index: 1 },
            separator: { op: "string", value: ":" },
          },
        },
      },
    },
    {
      body: `a=>a==="-g"||a==="--global"`,
      expr: {
        op: "or",
        left: {
          op: "strict-eq",
          left: { op: "arg", index: 0 },
          right: { op: "string", value: "-g" },
        },
        right: {
          op: "strict-eq",
          left: { op: "arg", index: 0 },
          right: { op: "string", value: "--global" },
        },
      },
    },
    {
      body: `function(){return!0}`,
      expr: { op: "bool", value: true },
    },
    {
      body: `(n,e)=>n.length===0&&e.length>0`,
      expr: {
        op: "and",
        left: {
          op: "strict-eq",
          left: { op: "length", value: { op: "arg", index: 0 } },
          right: { op: "integer", value: 0 },
        },
        right: {
          op: "gt",
          left: { op: "length", value: { op: "arg", index: 1 } },
          right: { op: "integer", value: 0 },
        },
      },
    },
  ];

  for (const { body, expr } of cases) {
    const ir = trigger(body);
    assert.equal(ir.version, TYPED_HOOK_IR_VERSION);
    assert.equal(ir.kind, TYPED_HOOK_IR_KIND);
    assert.equal(ir.sourceField, "trigger");
    assert.equal(ir.resultType, "bool");
    assert.deepEqual(ir.params, [
      { index: 0, type: "string" },
      { index: 1, type: "string" },
    ]);
    assert.deepEqual(ir.expr, expr);
    assert.equal(validateTypedHookIr(ir), true);
  }
});

test("parameter names do not affect the canonical expression", () => {
  const left = trigger(
    `(search,previous)=>search.split(",").length!==previous.split(",").length`,
  );
  const right = trigger(`(a,b)=>a.split(",").length!==b.split(",").length`);
  assert.deepEqual(left.expr, right.expr);
});

test("logical operators retain short-circuit tree shape", () => {
  const ir = trigger(`(a,b)=>a.length===0||b.length===0&&a.length>0`);
  assert.equal(ir.expr.op, "or");
  assert.equal(ir.expr.right.op, "and");
  assert.equal(ir.expr.left.op, "strict-eq");
});

test("split expressions retain a literal separator and length operation", () => {
  const comma = trigger(`(a,b)=>a.split(",").length!==b.split(",").length`);
  const colon = trigger(`(a,b)=>a.split(":").length!==b.split(":").length`);
  assert.equal(comma.expr.left.value.separator.value, ",");
  assert.equal(colon.expr.left.value.separator.value, ":");
  assert.notDeepEqual(comma.expr, colon.expr);
});

test("array includes is available in the generic positional compiler", () => {
  const result = compileTypedExpression({
    body: `items=>items.includes("--cached")`,
    parameterTypes: ["string-array"],
    resultType: "bool",
  });
  assert.deepEqual(result.expr, {
    op: "array-includes",
    value: { op: "arg", index: 0 },
    needle: { op: "string", value: "--cached" },
  });
});

test("getQueryTerm compiles through add and string-slice", () => {
  const result = compileTypedGetQueryTerm({
    body: `value=>value.includes("latest")?value.slice(value.indexOf(":")+1):value`,
  });
  assert.deepEqual(result.expr, {
    op: "if",
    condition: {
      op: "string-includes",
      value: { op: "arg", index: 0 },
      needle: { op: "string", value: "latest" },
    },
    then: {
      op: "string-slice",
      value: { op: "arg", index: 0 },
      start: {
        op: "add",
        left: {
          op: "string-index-of",
          value: { op: "arg", index: 0 },
          needle: { op: "string", value: ":" },
        },
        right: { op: "integer", value: 1 },
      },
    },
    else: { op: "arg", index: 0 },
  });
  assert.equal(result.sourceField, "getQueryTerm");
  assert.equal(result.resultType, "string");
  assert.deepEqual(result.params, [{ index: 0, type: "string" }]);
  assert.deepEqual(
    compileTypedHook({
      body: `n=>n.includes('latest')?n.slice(n.indexOf(':')+1):n`,
      sourceField: "getQueryTerm",
    }),
    result,
  );
  assert.deepEqual(
    compileTypedExpression({
      body: `value=>value.indexOf(":")+1`,
      parameterTypes: ["string"],
      resultType: "integer",
    }).expr,
    {
      op: "add",
      left: {
        op: "string-index-of",
        value: { op: "arg", index: 0 },
        needle: { op: "string", value: ":" },
      },
      right: { op: "integer", value: 1 },
    },
  );
});

test("getQueryTerm identity and lastIndexOf shapes compile", () => {
  assert.deepEqual(
    compileTypedHook({ body: `value=>value`, sourceField: "getQueryTerm" }).expr,
    { op: "arg", index: 0 },
  );
  const last = compileTypedGetQueryTerm({
    body: `value=>value.slice(value.lastIndexOf(":")+1)`,
  });
  assert.equal(last.expr.op, "string-slice");
  assert.equal(last.expr.start.left.op, "string-last-index-of");
  assert.throws(
    () =>
      compileTypedHook({
        body: `(value,other)=>value`,
        sourceField: "getQueryTerm",
      }),
    (error) => error instanceof TypedHookCompileError && error.code === "parameter-count",
  );
});

test("array literals have a typed string-array result", () => {
  const result = compileTypedExpression({
    body: `()=>["bash","-c"]`,
    parameterTypes: [],
    resultType: "string-array",
  });
  assert.deepEqual(result.expr, {
    op: "array",
    items: [
      { op: "string", value: "bash" },
      { op: "string", value: "-c" },
    ],
  });
});

test("constant !0 is folded without a runtime not operator", () => {
  assert.deepEqual(trigger(`function (ignored) { return !0; }`).expr, {
    op: "bool",
    value: true,
  });
});

test("unsupported syntax and free variables fail closed", () => {
  const cases = [
    [`(a,b)=>unknown`, "free-variable"],
    [`(a,b)=>a.foo`, "unsupported-property"],
    [`(a,b)=>a[0]`, "dynamic-property"],
    [`(a,b)=>a.match(/x/)`, "unknown-call"],
    [`async (a,b)=>true`, "async"],
    [`(a,b)=>{const value=true;return value}`, "function-shape"],
  ];
  for (const [body, code] of cases) {
    assert.throws(
      () => trigger(body),
      (error) => {
        assert.ok(error instanceof TypedHookCompileError);
        if (code) assert.equal(error.code, code);
        return true;
      },
    );
  }
});

test("if is lazy in the expression shape and keeps branch types", () => {
  const ir = trigger(`(a,b)=>a.length===0?true:false`);
  assert.deepEqual(ir.expr, {
    op: "if",
    condition: {
      op: "strict-eq",
      left: { op: "length", value: { op: "arg", index: 0 } },
      right: { op: "integer", value: 0 },
    },
    then: { op: "bool", value: true },
    else: { op: "bool", value: false },
  });
});

test("result type and field contracts are strict", () => {
  assert.throws(
    () =>
      compileTypedHook({
        body: `()=>true`,
        sourceField: "trigger",
        resultType: "string",
      }),
    (error) => error.code === "type-mismatch",
  );
  assert.throws(
    () => compileTypedHook({ body: `()=>true`, sourceField: "postProcess" }),
    (error) => error.code === "type-mismatch",
  );
  assert.throws(
    () => compileTypedHook({ body: `()=>true`, sourceField: "unknownField" }),
    (error) => error.code === "unknown-field",
  );
  assert.throws(
    () =>
      compileTypedHook({
        body: `value=>value.includes("latest")?value.slice(value.indexOf(":")+1):value`,
        sourceField: "getQueryTerm",
        resultType: "bool",
      }),
    (error) => error.code === "type-mismatch",
  );
  const tampered = trigger(`()=>true`);
  tampered.expr.extra = true;
  assert.throws(
    () => validateTypedHookIr(tampered),
    (error) => error.code === "schema",
  );
  assert.throws(
    () =>
      compileTypedExpression({
        body: `items=>items===items`,
        parameterTypes: ["string-array"],
        resultType: "bool",
      }),
    (error) => error.code === "type-mismatch",
  );
});

test("output is deterministic across repeated compilation", () => {
  const body = `(searchTerm, previousSearchTerm) => searchTerm.indexOf(":") !== previousSearchTerm.indexOf(":")`;
  const first = trigger(body);
  const second = trigger(body);
  assert.deepEqual(first, second);
  assert.equal(JSON.stringify(first), JSON.stringify(second));
  assert.equal(validateTypedHookIr(JSON.parse(JSON.stringify(first))), true);
});

test("source, literal, and serialized IR sizes are bounded", () => {
  assert.throws(
    () => trigger(`()=>true/*${"x".repeat(128 * 1024)}*/`),
    (error) => error.code === "complexity" && /source byte/.test(error.message),
  );
  assert.throws(
    () => trigger(`()=>${JSON.stringify("x".repeat(32 * 1024 + 1))}==="x"`),
    (error) =>
      error.code === "complexity" && /UTF-16 code-unit/.test(error.message),
  );

  const literal = "x".repeat(32 * 1024);
  const comparison = () => ({
    op: "strict-eq",
    left: { op: "string", value: literal },
    right: { op: "string", value: literal },
  });
  const oversized = trigger(`()=>true`);
  oversized.expr = {
    op: "and",
    left: {
      op: "and",
      left: comparison(),
      right: comparison(),
    },
    right: {
      op: "and",
      left: comparison(),
      right: comparison(),
    },
  };
  assert.throws(
    () => validateTypedHookIr(oversized),
    (error) =>
      error.code === "complexity" && /serialized IR/.test(error.message),
  );
});

test("integer and string literals stay inside the cross-language value contract", () => {
  assert.throws(
    () =>
      compileTypedExpression({
        body: `()=>9007199254740992`,
        parameterTypes: [],
        resultType: "integer",
      }),
    (error) => error.code === "unsupported-literal",
  );
  assert.throws(
    () =>
      compileTypedExpression({
        body: `()=>"\\ud800"`,
        parameterTypes: [],
        resultType: "string",
      }),
    (error) => error.code === "schema" && /unpaired/.test(error.message),
  );
  assert.equal(
    compileTypedExpression({
      body: `()=>"😀"`,
      parameterTypes: [],
      resultType: "string",
    }).expr.value,
    "😀",
  );
});

test("typed IR v2 exports the expanded value, op, and complexity contract", () => {
  assert.equal(MAX_NODES, 512);
  assert.equal(MAX_DEPTH, 24);
  for (const type of [
    "json",
    "suggestion",
    "suggestion-array",
    "string-record",
    "null",
  ]) {
    assert.ok(TYPED_VALUE_TYPES.includes(type), type);
  }
  for (const op of [
    "null",
    "string-trim",
    "string-trim-start",
    "string-trim-end",
    "string-replace",
    "string-replace-all",
    "string-starts-with",
    "string-ends-with",
    "string-substring",
    "string-last-index-of",
    "string-to-lower",
    "string-to-upper",
    "string-pad-start",
    "string-pad-end",
    "string-repeat",
    "string-concat",
    "string-char-at",
    "string-at",
    "add",
    "sub",
    "mul",
    "lt",
    "le",
    "ge",
    "not",
    "nullish",
  ]) {
    assert.ok(TYPED_EXPRESSION_OPERATIONS.includes(op), op);
  }
  assert.deepEqual(TYPED_HOOK_CONTRACTS.postProcess, {
    params: ["string", "string-array"],
    resultType: "suggestion-array",
  });
  assert.deepEqual(TYPED_HOOK_CONTRACTS.script, {
    params: ["string-array"],
    resultType: "string-array",
  });
  assert.deepEqual(TYPED_HOOK_CONTRACTS.filterTemplateSuggestions, {
    params: ["suggestion-array"],
    resultType: "suggestion-array",
  });
});

test("new string, numeric, and nullish operations compile to closed ops", () => {
  const cases = [
    [
      `value=>value.trim()`,
      "getQueryTerm",
      { op: "string-trim", value: { op: "arg", index: 0 } },
    ],
    [
      `value=>value.trimStart()`,
      "getQueryTerm",
      { op: "string-trim-start", value: { op: "arg", index: 0 } },
    ],
    [
      `value=>value.trimEnd()`,
      "getQueryTerm",
      { op: "string-trim-end", value: { op: "arg", index: 0 } },
    ],
    [
      `value=>value.replace("-","_")`,
      "getQueryTerm",
      {
        op: "string-replace",
        value: { op: "arg", index: 0 },
        needle: { op: "string", value: "-" },
        replacement: { op: "string", value: "_" },
      },
    ],
    [
      `value=>value.replaceAll("-","_")`,
      "getQueryTerm",
      {
        op: "string-replace-all",
        value: { op: "arg", index: 0 },
        needle: { op: "string", value: "-" },
        replacement: { op: "string", value: "_" },
      },
    ],
    [
      `value=>value.substring(1,3)`,
      "getQueryTerm",
      {
        op: "string-substring",
        value: { op: "arg", index: 0 },
        start: { op: "integer", value: 1 },
        end: { op: "integer", value: 3 },
      },
    ],
    [
      `value=>value.toLowerCase()`,
      "getQueryTerm",
      { op: "string-to-lower", value: { op: "arg", index: 0 } },
    ],
    [
      `value=>value.toUpperCase()`,
      "getQueryTerm",
      { op: "string-to-upper", value: { op: "arg", index: 0 } },
    ],
    [
      `value=>value.padStart(4,"0")`,
      "getQueryTerm",
      {
        op: "string-pad-start",
        value: { op: "arg", index: 0 },
        target: { op: "integer", value: 4 },
        pad: { op: "string", value: "0" },
      },
    ],
    [
      `value=>value.padEnd(3)`,
      "getQueryTerm",
      {
        op: "string-pad-end",
        value: { op: "arg", index: 0 },
        target: { op: "integer", value: 3 },
        pad: { op: "string", value: " " },
      },
    ],
    [
      `value=>value.repeat(2)`,
      "getQueryTerm",
      {
        op: "string-repeat",
        value: { op: "arg", index: 0 },
        count: { op: "integer", value: 2 },
      },
    ],
    [
      `value=>value.concat("!")`,
      "getQueryTerm",
      {
        op: "string-concat",
        parts: [{ op: "arg", index: 0 }, { op: "string", value: "!" }],
      },
    ],
    [
      `value=>value.charAt(1)`,
      "getQueryTerm",
      {
        op: "string-char-at",
        value: { op: "arg", index: 0 },
        index: { op: "integer", value: 1 },
      },
    ],
    [
      `value=>value.at(-1)`,
      "getQueryTerm",
      {
        op: "string-at",
        value: { op: "arg", index: 0 },
        index: { op: "integer", value: -1 },
      },
    ],
    [
      `value=>value.slice(-1)`,
      "getQueryTerm",
      {
        op: "string-slice",
        value: { op: "arg", index: 0 },
        start: { op: "integer", value: -1 },
      },
    ],
    [
      `value=>\`x\${value}y\``,
      "getQueryTerm",
      {
        op: "string-concat",
        parts: [
          { op: "string", value: "x" },
          { op: "arg", index: 0 },
          { op: "string", value: "y" },
        ],
      },
    ],
    [
      `(a,b)=>a.startsWith(b)`,
      "trigger",
      {
        op: "string-starts-with",
        value: { op: "arg", index: 0 },
        needle: { op: "arg", index: 1 },
      },
    ],
    [
      `(a,b)=>a.endsWith("/")`,
      "trigger",
      {
        op: "string-ends-with",
        value: { op: "arg", index: 0 },
        needle: { op: "string", value: "/" },
      },
    ],
    [
      `(a,b)=>!a.startsWith("x")`,
      "trigger",
      {
        op: "not",
        value: {
          op: "string-starts-with",
          value: { op: "arg", index: 0 },
          needle: { op: "string", value: "x" },
        },
      },
    ],
    [
      `(a,b)=>a.length<b.length`,
      "trigger",
      {
        op: "lt",
        left: { op: "length", value: { op: "arg", index: 0 } },
        right: { op: "length", value: { op: "arg", index: 1 } },
      },
    ],
    [
      `(a,b)=>a.length<=1`,
      "trigger",
      {
        op: "le",
        left: { op: "length", value: { op: "arg", index: 0 } },
        right: { op: "integer", value: 1 },
      },
    ],
    [
      `(a,b)=>a.length>=2`,
      "trigger",
      {
        op: "ge",
        left: { op: "length", value: { op: "arg", index: 0 } },
        right: { op: "integer", value: 2 },
      },
    ],
    [
      `(a,b)=>a.length-1===0`,
      "trigger",
      {
        op: "strict-eq",
        left: {
          op: "sub",
          left: { op: "length", value: { op: "arg", index: 0 } },
          right: { op: "integer", value: 1 },
        },
        right: { op: "integer", value: 0 },
      },
    ],
    [
      `(a,b)=>a.length*2===4`,
      "trigger",
      {
        op: "strict-eq",
        left: {
          op: "mul",
          left: { op: "length", value: { op: "arg", index: 0 } },
          right: { op: "integer", value: 2 },
        },
        right: { op: "integer", value: 4 },
      },
    ],
    [
      `tokens=>["echo","-n"]`,
      "script",
      {
        op: "array",
        items: [
          { op: "string", value: "echo" },
          { op: "string", value: "-n" },
        ],
      },
    ],
    [
      `suggestions=>suggestions`,
      "filterTemplateSuggestions",
      { op: "arg", index: 0 },
    ],
  ];

  for (const [body, sourceField, expr] of cases) {
    assert.deepEqual(
      compileTypedHook({ body, sourceField }).expr,
      expr,
      body,
    );
  }

  const nullish = compileTypedHook({
    body: `(a,b)=>a.startsWith("x")??true`,
    sourceField: "trigger",
  });
  assert.deepEqual(nullish.expr, {
    op: "nullish",
    left: {
      op: "string-starts-with",
      value: { op: "arg", index: 0 },
      needle: { op: "string", value: "x" },
    },
    right: { op: "bool", value: true },
  });
});

test("replace needles must be string literals and arithmetic stays fail-closed", () => {
  assert.throws(
    () =>
      compileTypedHook({
        body: `value=>value.replace(value,"x")`,
        sourceField: "getQueryTerm",
      }),
    (error) => error.code === "unsupported-syntax",
  );
  assert.throws(
    () =>
      compileTypedHook({
        body: `(a,b)=>a+1===b.length`,
        sourceField: "trigger",
      }),
    (error) => error.code === "type-mismatch",
  );
  assert.throws(
    () =>
      compileTypedExpression({
        body: `()=>1??"x"`,
        parameterTypes: [],
        resultType: "integer",
      }),
    (error) => error.code === "type-mismatch",
  );
});

test("evaluateTypedHook matches JavaScript UTF-16 string and safe-integer arithmetic", () => {
  const query = (body, value) =>
    evaluateTypedHook(
      compileTypedHook({ body, sourceField: "getQueryTerm" }),
      [value],
    );
  const triggerEval = (body, left, right) =>
    evaluateTypedHook(compileTypedHook({ body, sourceField: "trigger" }), [
      left,
      right,
    ]);

  assert.equal(query(`value=>value.trim()`, "  hi  "), "hi");
  assert.equal(query(`value=>value.trimStart()`, "  hi  "), "hi  ");
  assert.equal(query(`value=>value.trimEnd()`, "  hi  "), "  hi");
  assert.equal(query(`value=>value.replace("-","_")`, "a-b-c"), "a_b-c");
  assert.equal(query(`value=>value.replaceAll("-","_")`, "a-b-c"), "a_b_c");
  assert.equal(query(`value=>value.replaceAll("","-")`, "ab"), "-a-b-");
  assert.equal(query(`value=>value.replace("","-")`, "ab"), "-ab");
  assert.equal(query(`value=>value.toLowerCase()`, "AbC"), "abc");
  assert.equal(query(`value=>value.toUpperCase()`, "AbC"), "ABC");
  assert.equal(query(`value=>value.substring(1,3)`, "abcd"), "bc");
  assert.equal(query(`value=>value.substring(3,1)`, "abcd"), "bc");
  assert.equal(query(`value=>value.substring(2)`, "abcdef"), "cdef");
  assert.equal(query(`value=>value.padStart(4,"0")`, "12"), "0012");
  assert.equal(query(`value=>value.padEnd(4,".")`, "12"), "12..");
  assert.equal(query(`value=>value.padStart(2,"0")`, "abcd"), "abcd");
  assert.equal(query(`value=>value.padStart(4,"")`, "12"), "12");
  assert.equal(query(`value=>value.repeat(3)`, "ab"), "ababab");
  assert.equal(query(`value=>value.concat("!")`, "hi"), "hi!");
  assert.equal(query(`value=>value.charAt(1)`, "abc"), "b");
  assert.equal(query(`value=>value.charAt(-1)`, "abc"), "");
  assert.equal(query(`value=>value.at(-1)`, "abc"), "c");
  assert.equal(query(`value=>value.at(8)`, "abc"), "");
  assert.equal(query(`value=>\`x\${value}y\``, "mid"), "xmidy");
  assert.equal(query(`value=>value.substring(1,3)`, "a😀b"), "😀");
  assert.equal(query(`value=>value.padStart(3,".")`, "😀"), ".😀");
  assert.equal(query(`value=>value.trim()`, "\u0085hi"), "\u0085hi");
  assert.equal(
    query(
      `value=>value.includes("latest")?value.slice(value.indexOf(":")+1):value`,
      "nodejs:latest",
    ),
    "latest",
  );
  assert.equal(
    query(`value=>value.slice(value.lastIndexOf(":")+1)`, "a:b:c"),
    "c",
  );

  assert.equal(triggerEval(`(a,b)=>a.startsWith(b)`, "hello", "he"), true);
  assert.equal(triggerEval(`(a,b)=>a.endsWith("/")`, "src/", "x"), true);
  assert.equal(triggerEval(`(a,b)=>!a.startsWith("x")`, "hello", "x"), true);
  assert.equal(triggerEval(`(a,b)=>a.length<b.length`, "a", "bb"), true);
  assert.equal(triggerEval(`(a,b)=>a.length<=1`, "a", "z"), true);
  assert.equal(triggerEval(`(a,b)=>a.length>=2`, "ab", "z"), true);
  assert.equal(triggerEval(`(a,b)=>a.length-1===0`, "x", "z"), true);
  assert.equal(triggerEval(`(a,b)=>a.length*2===4`, "ab", "z"), true);
  assert.equal(
    triggerEval(`(a,b)=>a.lastIndexOf("/")!==b.lastIndexOf("/")`, "a/b", "ab"),
    true,
  );
  assert.deepEqual(
    evaluateTypedHook(
      compileTypedHook({
        body: `tokens=>["echo","-n"]`,
        sourceField: "script",
      }),
      [[]],
    ),
    ["echo", "-n"],
  );

  const nullishFallback = compileTypedHook({
    body: `value=>value`,
    sourceField: "getQueryTerm",
  });
  nullishFallback.expr = {
    op: "nullish",
    left: { op: "null" },
    right: { op: "arg", index: 0 },
  };
  assert.equal(evaluateTypedHook(nullishFallback, ["fallback"]), "fallback");

  const overflow = compileTypedHook({
    body: `(a,b)=>a.length===0`,
    sourceField: "trigger",
  });
  overflow.expr = {
    op: "strict-eq",
    left: {
      op: "add",
      left: { op: "integer", value: Number.MAX_SAFE_INTEGER },
      right: { op: "integer", value: 1 },
    },
    right: { op: "integer", value: 0 },
  };
  assert.throws(
    () => evaluateTypedHook(overflow, ["", ""]),
    (error) => error.code === "overflow",
  );
  const repeatOverflow = compileTypedHook({
    body: `value=>value.repeat(0)`,
    sourceField: "getQueryTerm",
  });
  repeatOverflow.expr.count = { op: "integer", value: -1 };
  assert.throws(
    () => evaluateTypedHook(repeatOverflow, ["x"]),
    (error) => error.code === "overflow",
  );
  assert.throws(
    () =>
      evaluateTypedHook(
        compileTypedHook({ body: `value=>value`, sourceField: "getQueryTerm" }),
        [1],
      ),
    (error) => error.code === "input",
  );
  const hugePad = compileTypedHook({
    body: `value=>value.padStart(4,"0")`,
    sourceField: "getQueryTerm",
  });
  hugePad.expr.target = { op: "integer", value: 32 * 1024 + 1 };
  assert.throws(
    () => evaluateTypedHook(hugePad, ["x"]),
    (error) => error.code === "complexity",
  );
});

test("cross-language v2 op golden matches compile and evaluate", async () => {
  const generated = buildTypedIrV2OpsGolden();
  if (process.env.EC_TYPED_IR_V2_UPDATE === "1") {
    await writeTypedIrV2OpsGolden();
  }
  const committed = JSON.parse(await readFile(TYPED_IR_V2_OPS_PATH, "utf8"));
  assert.deepEqual(committed, generated);
  for (const entry of committed.cases) {
    assert.equal(validateTypedHookIr(entry.descriptor), true);
    assert.deepEqual(
      evaluateTypedHook(entry.descriptor, entry.args),
      entry.expected,
      entry.id,
    );
  }
});
