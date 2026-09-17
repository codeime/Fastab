import assert from "node:assert/strict";
import test from "node:test";

import {
  TYPED_HOOK_IR_KIND,
  TYPED_HOOK_IR_VERSION,
  TypedHookCompileError,
  compileTypedExpression,
  compileTypedGetQueryTerm,
  compileTypedHook,
  validateTypedHookIr,
} from "./typed-hook-ir.mjs";

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

test("the closed getQueryTerm shape replaces generic integer addition", () => {
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
      op: "string-slice-after-first",
      value: { op: "arg", index: 0 },
      needle: { op: "string", value: ":" },
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
  assert.throws(
    () =>
      compileTypedExpression({
        body: `value=>value.indexOf(":")+1`,
        parameterTypes: ["string"],
        resultType: "integer",
      }),
    (error) => error.code === "unsupported-syntax",
  );
});

test("getQueryTerm research matching is exact and fails closed", () => {
  const unsupported = [
    `value=>value.includes("latest")?value.slice(value.lastIndexOf(":")+1):value`,
    `value=>value.includes("latest")?value.slice(value.indexOf(":")+2):value`,
    `value=>value.includes("latest")?value.slice(1+value.indexOf(":")):value`,
    `value=>value.includes("latest")?value.slice(value.indexOf(":")):value`,
    `value=>value.includes("latest")?value.slice(value.indexOf("/")+1):value`,
    `value=>value.includes("latest")?value.slice(value.indexOf(":")+1):""`,
    `(value,other)=>value.includes("latest")?value.slice(value.indexOf(":")+1):value`,
    `value=>value.includes("LATEST")?value.slice(value.indexOf(":")+1):value`,
  ];
  for (const body of unsupported) {
    assert.throws(
      () => compileTypedGetQueryTerm({ body }),
      (error) =>
        error instanceof TypedHookCompileError &&
        ["function-shape", "parameter-count"].includes(error.code),
      body,
    );
  }
  assert.throws(
    () => compileTypedHook({ body: `value=>value`, sourceField: "getQueryTerm" }),
    (error) => error instanceof TypedHookCompileError && error.code === "function-shape",
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
