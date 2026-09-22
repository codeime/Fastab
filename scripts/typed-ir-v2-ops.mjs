#!/usr/bin/env node
/**
 * Cross-language typed IR v2 operation goldens.
 *
 * The committed JSON is the same descriptor tree the Rust evaluator parses.
 * JavaScript compiles and evaluates the cases; Rust must produce the same
 * JSON value for each descriptor + args pair.
 */
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { compileTypedHook, evaluateTypedHook } from "./typed-hook-ir.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
export const TYPED_IR_V2_OPS_PATH = join(
  repoDir,
  "crates",
  "fastab_engine",
  "testdata",
  "native-hooks",
  "typed-ir-v2-ops.json",
);

export const TYPED_IR_V2_OPS_KIND = "typed-ir-v2-ops";
export const TYPED_IR_V2_OPS_VERSION = 1;

export const TYPED_IR_V2_OPS_CASES = Object.freeze([
  {
    id: "query-identity",
    sourceField: "getQueryTerm",
    body: "value=>value",
    args: ["keep"],
  },
  {
    id: "query-trim",
    sourceField: "getQueryTerm",
    body: "value=>value.trim()",
    args: ["  hi  "],
  },
  {
    id: "query-trim-start",
    sourceField: "getQueryTerm",
    body: "value=>value.trimStart()",
    args: ["  hi  "],
  },
  {
    id: "query-trim-end",
    sourceField: "getQueryTerm",
    body: "value=>value.trimEnd()",
    args: ["  hi  "],
  },
  {
    id: "query-trim-does-not-strip-nel",
    sourceField: "getQueryTerm",
    body: "value=>value.trim()",
    args: ["\u0085hi"],
  },
  {
    id: "query-replace",
    sourceField: "getQueryTerm",
    body: 'value=>value.replace("-","_")',
    args: ["a-b-c"],
  },
  {
    id: "query-replace-all",
    sourceField: "getQueryTerm",
    body: 'value=>value.replaceAll("-","_")',
    args: ["a-b-c"],
  },
  {
    id: "query-to-lower",
    sourceField: "getQueryTerm",
    body: "value=>value.toLowerCase()",
    args: ["AbC"],
  },
  {
    id: "query-to-upper",
    sourceField: "getQueryTerm",
    body: "value=>value.toUpperCase()",
    args: ["AbC"],
  },
  {
    id: "query-substring",
    sourceField: "getQueryTerm",
    body: "value=>value.substring(1,3)",
    args: ["abcd"],
  },
  {
    id: "query-substring-swapped",
    sourceField: "getQueryTerm",
    body: "value=>value.substring(3,1)",
    args: ["abcd"],
  },
  {
    id: "query-substring-default-end",
    sourceField: "getQueryTerm",
    body: "value=>value.substring(2)",
    args: ["abcdef"],
  },
  {
    id: "query-substring-utf16",
    sourceField: "getQueryTerm",
    body: "value=>value.substring(1,3)",
    args: ["a😀b"],
  },
  {
    id: "query-pad-start",
    sourceField: "getQueryTerm",
    body: 'value=>value.padStart(4,"0")',
    args: ["12"],
  },
  {
    id: "query-pad-end",
    sourceField: "getQueryTerm",
    body: 'value=>value.padEnd(4,".")',
    args: ["12"],
  },
  {
    id: "query-pad-start-utf16",
    sourceField: "getQueryTerm",
    body: 'value=>value.padStart(3,".")',
    args: ["😀"],
  },
  {
    id: "query-repeat",
    sourceField: "getQueryTerm",
    body: "value=>value.repeat(3)",
    args: ["ab"],
  },
  {
    id: "query-concat",
    sourceField: "getQueryTerm",
    body: 'value=>value.concat("!")',
    args: ["hi"],
  },
  {
    id: "query-char-at",
    sourceField: "getQueryTerm",
    body: "value=>value.charAt(1)",
    args: ["abc"],
  },
  {
    id: "query-at-last",
    sourceField: "getQueryTerm",
    body: "value=>value.at(-1)",
    args: ["abc"],
  },
  {
    id: "query-template-concat",
    sourceField: "getQueryTerm",
    body: "value=>`x${value}y`",
    args: ["mid"],
  },
  {
    id: "query-asdf-add-slice",
    sourceField: "getQueryTerm",
    body: 'value=>value.includes("latest")?value.slice(value.indexOf(":")+1):value',
    args: ["nodejs:latest"],
  },
  {
    id: "query-last-index-slice",
    sourceField: "getQueryTerm",
    body: 'value=>value.slice(value.lastIndexOf(":")+1)',
    args: ["a:b:c"],
  },
  {
    id: "query-starts-with-slice",
    sourceField: "getQueryTerm",
    body: 'value=>value.startsWith(".")?value.slice(1):value',
    args: [".hidden"],
  },
  {
    id: "query-ends-with-substring",
    sourceField: "getQueryTerm",
    body: 'value=>value.endsWith("/")?value.substring(0,value.length-1):value',
    args: ["src/"],
  },
  {
    id: "trigger-starts-with",
    sourceField: "trigger",
    body: "(a,b)=>a.startsWith(b)",
    args: ["hello", "he"],
  },
  {
    id: "trigger-ends-with",
    sourceField: "trigger",
    body: '(a,b)=>a.endsWith("/")',
    args: ["src/", "x"],
  },
  {
    id: "trigger-not",
    sourceField: "trigger",
    body: '(a,b)=>!a.startsWith("x")',
    args: ["hello", "x"],
  },
  {
    id: "trigger-lt",
    sourceField: "trigger",
    body: "(a,b)=>a.length<b.length",
    args: ["a", "bb"],
  },
  {
    id: "trigger-le",
    sourceField: "trigger",
    body: "(a,b)=>a.length<=1",
    args: ["a", "z"],
  },
  {
    id: "trigger-ge",
    sourceField: "trigger",
    body: "(a,b)=>a.length>=2",
    args: ["ab", "z"],
  },
  {
    id: "trigger-sub",
    sourceField: "trigger",
    body: "(a,b)=>a.length-1===0",
    args: ["x", "z"],
  },
  {
    id: "trigger-mul",
    sourceField: "trigger",
    body: "(a,b)=>a.length*2===4",
    args: ["ab", "z"],
  },
  {
    id: "trigger-last-index-of",
    sourceField: "trigger",
    body: '(a,b)=>a.lastIndexOf("/")!==b.lastIndexOf("/")',
    args: ["a/b", "ab"],
  },
  {
    id: "trigger-last-index-empty-needle",
    sourceField: "trigger",
    body: '(a,b)=>a.lastIndexOf("")===a.length',
    args: ["ab", "x"],
  },
  {
    id: "query-replace-all-empty-needle",
    sourceField: "getQueryTerm",
    body: 'value=>value.replaceAll("","-")',
    args: ["ab"],
  },
  {
    id: "script-literal-argv",
    sourceField: "script",
    body: 'tokens=>["echo","-n"]',
    args: [[]],
  },
]);

function stableJson(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

export function buildTypedIrV2OpsGolden() {
  const cases = TYPED_IR_V2_OPS_CASES.map((entry) => {
    const descriptor = compileTypedHook({
      body: entry.body,
      sourceField: entry.sourceField,
    });
    return {
      id: entry.id,
      sourceField: entry.sourceField,
      body: entry.body,
      args: entry.args,
      expected: evaluateTypedHook(descriptor, entry.args),
      descriptor,
    };
  });

  const nullishDescriptor = compileTypedHook({
    body: "value=>value",
    sourceField: "getQueryTerm",
  });
  nullishDescriptor.expr = {
    op: "nullish",
    left: { op: "null" },
    right: { op: "arg", index: 0 },
  };
  cases.push({
    id: "query-nullish-null-left",
    sourceField: "getQueryTerm",
    body: null,
    args: ["fallback"],
    expected: evaluateTypedHook(nullishDescriptor, ["fallback"]),
    descriptor: nullishDescriptor,
  });

  return {
    version: TYPED_IR_V2_OPS_VERSION,
    kind: TYPED_IR_V2_OPS_KIND,
    cases,
  };
}

export async function writeTypedIrV2OpsGolden(path = TYPED_IR_V2_OPS_PATH) {
  await mkdir(dirname(path), { recursive: true });
  const golden = buildTypedIrV2OpsGolden();
  await writeFile(path, stableJson(golden));
  return golden;
}

export async function readTypedIrV2OpsGolden(path = TYPED_IR_V2_OPS_PATH) {
  return JSON.parse(await readFile(path, "utf8"));
}

const isMain = process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];
if (isMain) {
  await writeTypedIrV2OpsGolden();
}
