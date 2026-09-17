import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import {
  BASELINE_KIND,
  BASELINE_VERSION,
  FIELD_CONTRACTS,
  baselineRelativePath,
  validateBaseline,
} from "./hook-baseline-contract.mjs";
import { SUPPORTED_HOOK_FIELDS } from "./spec-hook-contract.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const samplePath = join(
  repoDir,
  "crates",
  "ec_engine",
  "testdata",
  "native-hooks",
  "sample-baseline.json",
);

async function loadSample() {
  return JSON.parse(await readFile(samplePath, "utf8"));
}

function clone(value) {
  return structuredClone(value);
}

test("FIELD_CONTRACTS covers every supported hook field and no others", () => {
  assert.deepEqual(
    Object.keys(FIELD_CONTRACTS).sort(),
    Object.keys(SUPPORTED_HOOK_FIELDS).sort(),
  );
  assert.equal(FIELD_CONTRACTS.postProcess.params.length, 2);
  assert.equal(FIELD_CONTRACTS.custom.execIndex, 1);
  assert.equal(FIELD_CONTRACTS.trigger.execIndex, null);
  assert.equal(FIELD_CONTRACTS.filterTemplateSuggestions.params[0], "suggestion-array");
});

test("valid sample baseline passes", async () => {
  const sample = await loadSample();
  assert.equal(sample.version, BASELINE_VERSION);
  assert.equal(sample.kind, BASELINE_KIND);
  assert.equal(
    baselineRelativePath(sample.field, sample.bodySha256),
    "crates/ec_engine/testdata/native-hooks/baseline/postProcess/57ac01d9a722bb8a6f4d8edfe88b6f8d52ababb70348740e130e2c5dfee28cbf.json",
  );
  assert.equal(validateBaseline(sample), sample);
});

test("unknown fields throw at every object layer", async () => {
  const sample = await loadSample();

  const extraRoot = clone(sample);
  extraRoot.unexpected = true;
  assert.throws(() => validateBaseline(extraRoot), /unknown field "unexpected"/);

  const extraCase = clone(sample);
  extraCase.cases[0].extra = 1;
  assert.throws(() => validateBaseline(extraCase), /unknown field "extra"/);

  const extraExpected = clone(sample);
  extraExpected.cases[0].expected.note = "nope";
  assert.throws(() => validateBaseline(extraExpected), /unknown field "note"/);

  const extraSuggestion = clone(sample);
  extraSuggestion.cases[0].expected.value[0].score = 99;
  assert.throws(() => validateBaseline(extraSuggestion), /unknown field "score"/);

  const extraExec = clone(sample);
  extraExec.cases[0].exec[0].delayMs = 10;
  assert.throws(() => validateBaseline(extraExec), /unknown field "delayMs"/);

  const extraContext = clone(sample);
  extraContext.cases[0].context.hostname = "box";
  assert.throws(() => validateBaseline(extraContext), /unknown field "hostname"/);
});

test("missing required fields throw", async () => {
  const sample = await loadSample();

  for (const key of ["version", "kind", "field", "bodySha256", "representativeHookId", "hookCount", "cases"]) {
    const missing = clone(sample);
    delete missing[key];
    assert.throws(() => validateBaseline(missing), /missing required field/, key);
  }

  for (const key of ["id", "args", "exec", "context", "timeoutMs", "expected"]) {
    const missing = clone(sample);
    delete missing.cases[0][key];
    assert.throws(() => validateBaseline(missing), /missing required field/, key);
  }

  const missingKind = clone(sample);
  delete missingKind.cases[0].expected.kind;
  assert.throws(() => validateBaseline(missingKind), /missing required field "kind"/);

  const missingValue = clone(sample);
  delete missingValue.cases[0].expected.value;
  assert.throws(() => validateBaseline(missingValue), /missing required field "value"/);
});

test("args count and type must match the field contract", async () => {
  const sample = await loadSample();

  const tooFew = clone(sample);
  tooFew.cases[0].args = ["only-stdout"];
  assert.throws(() => validateBaseline(tooFew), /has 1 value\(s\); postProcess requires 2/);

  const tooMany = clone(sample);
  tooMany.cases[0].args = ["stdout", ["git"], "extra"];
  assert.throws(() => validateBaseline(tooMany), /has 3 value\(s\); postProcess requires 2/);

  const wrongType = clone(sample);
  wrongType.cases[0].args = [["git"], "checkout"];
  assert.throws(() => validateBaseline(wrongType), /must be a string/);

  const trigger = clone(sample);
  trigger.field = "trigger";
  trigger.cases[0].args = ["a", "ab"];
  trigger.cases[0].expected = { kind: "bool", value: true };
  assert.equal(validateBaseline(trigger), trigger);

  trigger.cases[0].args = ["only-one"];
  assert.throws(() => validateBaseline(trigger), /has 1 value\(s\); trigger requires 2/);
});

test("timeout expected may omit value; other kinds stay typed", async () => {
  const sample = await loadSample();
  const timeout = clone(sample);
  timeout.cases[0].id = "timeout";
  timeout.cases[0].timeoutMs = 50;
  timeout.cases[0].expected = { kind: "timeout" };
  assert.equal(validateBaseline(timeout), timeout);

  const error = clone(sample);
  error.cases[0].id = "error";
  error.cases[0].expected = { kind: "error", value: "hook threw" };
  assert.equal(validateBaseline(error), error);

  const mismatch = clone(sample);
  mismatch.field = "trigger";
  mismatch.cases[0].args = ["a", "b"];
  mismatch.cases[0].expected = { kind: "suggestions", value: [] };
  assert.throws(() => validateBaseline(mismatch), /does not match trigger result "bool"/);
});
