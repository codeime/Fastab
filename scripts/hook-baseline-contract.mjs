/**
 * Closed JSON contract for per-function-body hook output baselines.
 *
 * JavaScript and Rust parse the same shape. A new field, a missing required
 * field, or an `args` vector that does not match the hook signature must fail
 * closed — the native migration must not invent a looser harness later.
 */
import { SUPPORTED_HOOK_FIELDS } from "./spec-hook-contract.mjs";

export const BASELINE_VERSION = 1;
export const BASELINE_KIND = "native-hook-baseline";

export const BASELINE_ROOT_SEGMENTS = Object.freeze([
  "crates",
  "fastab_engine",
  "testdata",
  "native-hooks",
  "baseline",
]);

export const EXPECTED_KINDS = Object.freeze([
  "suggestions",
  "string",
  "bool",
  "argv",
  "spec",
  "error",
  "timeout",
]);

// Fig.Suggestion spellings the engine already reads (`suggestion_from_json`)
// plus the remaining documented Fig fields. Defaulted keys may be omitted.
export const FIG_SUGGESTION_FIELDS = Object.freeze([
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
  "shouldAddSpace",
]);

export const CONTEXT_FIELDS = Object.freeze([
  "currentWorkingDirectory",
  "currentProcess",
  "sshPrefix",
  "environmentVariables",
  "searchTerm",
  "isDangerous",
]);

export const EXEC_FIELDS = Object.freeze([
  "command",
  "args",
  "stdout",
  "stderr",
  "status",
  "delayMs",
]);
export const EXEC_COMMAND_FIELDS = Object.freeze([
  "command",
  "args",
  "stdout",
  "stderr",
  "status",
]);

export const BASELINE_FIELDS = Object.freeze([
  "version",
  "kind",
  "field",
  "bodySha256",
  "representativeHookId",
  "hookCount",
  "cases",
]);

export const CASE_FIELDS = Object.freeze(["id", "args", "exec", "context", "timeoutMs", "expected"]);
export const INPUT_CASE_FIELDS = Object.freeze([
  "id",
  "args",
  "exec",
  "context",
  "timeoutMs",
  "synthetic",
]);
export const INPUT_BASELINE_FIELDS = BASELINE_FIELDS.filter((key) => key !== "cases");

export const ARG_TYPES = Object.freeze(["string", "string-array", "suggestion-array"]);

/**
 * Per-field hook signature. `params` is the `args` vector (executeCommand and
 * context are not positional entries; they live on `exec` / `context`).
 * `execIndex` is the JS parameter index of `executeCommand`, or `null`.
 */
export const FIELD_CONTRACTS = Object.freeze({
  postProcess: Object.freeze({
    params: Object.freeze(["string", "string-array"]),
    execIndex: null,
    resultKind: "suggestions",
  }),
  custom: Object.freeze({
    params: Object.freeze(["string-array"]),
    execIndex: 1,
    resultKind: "suggestions",
  }),
  getQueryTerm: Object.freeze({
    params: Object.freeze(["string"]),
    execIndex: null,
    resultKind: "string",
  }),
  trigger: Object.freeze({
    params: Object.freeze(["string", "string"]),
    execIndex: null,
    resultKind: "bool",
  }),
  script: Object.freeze({
    params: Object.freeze(["string-array"]),
    execIndex: null,
    resultKind: "argv",
  }),
  generateSpec: Object.freeze({
    params: Object.freeze(["string-array"]),
    execIndex: 1,
    resultKind: "spec",
  }),
  filterTemplateSuggestions: Object.freeze({
    params: Object.freeze(["suggestion-array"]),
    execIndex: null,
    resultKind: "suggestions",
  }),
  alias: Object.freeze({
    params: Object.freeze(["string"]),
    execIndex: 1,
    resultKind: "string",
  }),
  loadSpec: Object.freeze({
    params: Object.freeze(["string"]),
    execIndex: 1,
    resultKind: "spec",
  }),
});

const SOURCE_FIELDS = Object.freeze(Object.keys(SUPPORTED_HOOK_FIELDS));
const FIELD_CONTRACT_KEYS = Object.freeze(Object.keys(FIELD_CONTRACTS));

if (SOURCE_FIELDS.length !== FIELD_CONTRACT_KEYS.length) {
  throw new Error("FIELD_CONTRACTS must cover every SUPPORTED_HOOK_FIELDS key");
}
for (const field of SOURCE_FIELDS) {
  if (!Object.hasOwn(FIELD_CONTRACTS, field)) {
    throw new Error(`FIELD_CONTRACTS is missing ${field}`);
  }
}

const BASELINE_FIELD_SET = new Set(BASELINE_FIELDS);
const CASE_FIELD_SET = new Set(CASE_FIELDS);
const FIG_SUGGESTION_FIELD_SET = new Set(FIG_SUGGESTION_FIELDS);
const CONTEXT_FIELD_SET = new Set(CONTEXT_FIELDS);
const EXEC_FIELD_SET = new Set(EXEC_FIELDS);
const EXPECTED_KIND_SET = new Set(EXPECTED_KINDS);
const EXPECTED_FIELD_SET = new Set(["kind", "value"]);
const SHA256_RE = /^[0-9a-f]{64}$/;

export function baselineRelativePath(field, bodySha256) {
  return [...BASELINE_ROOT_SEGMENTS, field, `${bodySha256}.json`].join("/");
}

export function fieldContract(field) {
  const contract = FIELD_CONTRACTS[field];
  if (!contract) {
    throw new Error(`unknown hook field ${JSON.stringify(field)}`);
  }
  return contract;
}

export function validateBaseline(value) {
  assertPlainObject(value, "baseline");
  assertKnownKeys(value, BASELINE_FIELD_SET, "baseline");
  assertRequiredKeys(value, BASELINE_FIELDS, "baseline");

  if (value.version !== BASELINE_VERSION) {
    throw new Error(`baseline.version must be ${BASELINE_VERSION}`);
  }
  if (value.kind !== BASELINE_KIND) {
    throw new Error(`baseline.kind must be ${JSON.stringify(BASELINE_KIND)}`);
  }
  if (!Object.hasOwn(FIELD_CONTRACTS, value.field)) {
    throw new Error(`baseline.field ${JSON.stringify(value.field)} is not a supported hook field`);
  }
  if (typeof value.bodySha256 !== "string" || !SHA256_RE.test(value.bodySha256)) {
    throw new Error("baseline.bodySha256 must be a 64-character lowercase hex digest");
  }
  if (typeof value.representativeHookId !== "string" || value.representativeHookId.length === 0) {
    throw new Error("baseline.representativeHookId must be a non-empty string");
  }
  if (!Number.isInteger(value.hookCount) || value.hookCount < 1) {
    throw new Error("baseline.hookCount must be a positive integer");
  }
  if (!Array.isArray(value.cases)) {
    throw new Error("baseline.cases must be an array");
  }

  const caseIds = new Set();
  value.cases.forEach((item, index) => {
    validateCase(item, value.field, `baseline.cases[${index}]`);
    if (caseIds.has(item.id)) {
      throw new Error(`baseline.cases[${index}].id ${JSON.stringify(item.id)} is duplicated`);
    }
    caseIds.add(item.id);
  });
  return value;
}

/**
 * Input fixtures are baselines without `expected`. `synthetic` marks a case
 * whose `normal` stdout did not come from the T1.3 CLI sample library.
 */
export function validateInputFixture(value) {
  assertPlainObject(value, "input");
  assertKnownKeys(value, new Set([...INPUT_BASELINE_FIELDS, "cases"]), "input");
  assertRequiredKeys(value, [...INPUT_BASELINE_FIELDS, "cases"], "input");
  const baseline = {
    ...value,
    cases: value.cases.map((item, index) => {
      const itemPath = `input.cases[${index}]`;
      assertPlainObject(item, itemPath);
      assertKnownKeys(item, new Set(INPUT_CASE_FIELDS), itemPath);
      const { synthetic: _synthetic, ...rest } = item;
      if (Object.hasOwn(item, "synthetic") && item.synthetic !== true) {
        throw new Error(`${itemPath}.synthetic must be true when present`);
      }
      return {
        ...rest,
        expected: { kind: "timeout" },
      };
    }),
  };
  validateBaseline(baseline);
  return value;
}

function validateCase(value, field, path) {
  assertPlainObject(value, path);
  assertKnownKeys(value, CASE_FIELD_SET, path);
  assertRequiredKeys(value, CASE_FIELDS, path);

  if (typeof value.id !== "string" || value.id.length === 0) {
    throw new Error(`${path}.id must be a non-empty string`);
  }
  validateArgs(value.args, field, `${path}.args`);
  validateExec(value.exec, `${path}.exec`);
  validateContext(value.context, `${path}.context`);
  if (!Number.isInteger(value.timeoutMs) || value.timeoutMs < 0) {
    throw new Error(`${path}.timeoutMs must be a non-negative integer`);
  }
  validateExpected(value.expected, field, `${path}.expected`);
}

function validateArgs(args, field, path) {
  if (!Array.isArray(args)) {
    throw new Error(`${path} must be an array`);
  }
  const contract = fieldContract(field);
  if (args.length !== contract.params.length) {
    throw new Error(
      `${path} has ${args.length} value(s); ${field} requires ${contract.params.length}`,
    );
  }
  args.forEach((arg, index) => {
    validateArgValue(arg, contract.params[index], `${path}[${index}]`);
  });
}

function validateArgValue(value, type, path) {
  switch (type) {
    case "string":
      if (typeof value !== "string") {
        throw new Error(`${path} must be a string`);
      }
      return;
    case "string-array":
      if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
        throw new Error(`${path} must be an array of strings`);
      }
      return;
    case "suggestion-array":
      if (!Array.isArray(value)) {
        throw new Error(`${path} must be an array of Fig suggestions`);
      }
      value.forEach((item, index) => validateSuggestion(item, `${path}[${index}]`));
      return;
    default:
      throw new Error(`${path} has unknown argument type ${JSON.stringify(type)}`);
  }
}

function validateExec(value, path) {
  if (!Array.isArray(value)) {
    throw new Error(`${path} must be an array`);
  }
  value.forEach((item, index) => {
    const itemPath = `${path}[${index}]`;
    assertPlainObject(item, itemPath);
    assertKnownKeys(item, EXEC_FIELD_SET, itemPath);
    const hasDelay = Object.hasOwn(item, "delayMs");
    const hasCommand = Object.hasOwn(item, "command");
    if (hasDelay) {
      if (!Number.isInteger(item.delayMs) || item.delayMs < 0) {
        throw new Error(`${itemPath}.delayMs must be a non-negative integer`);
      }
    }
    if (hasCommand) {
      assertRequiredKeys(item, EXEC_COMMAND_FIELDS, itemPath);
      if (typeof item.command !== "string" || item.command.length === 0) {
        throw new Error(`${itemPath}.command must be a non-empty string`);
      }
      if (!Array.isArray(item.args) || item.args.some((arg) => typeof arg !== "string")) {
        throw new Error(`${itemPath}.args must be an array of strings`);
      }
      if (typeof item.stdout !== "string") {
        throw new Error(`${itemPath}.stdout must be a string`);
      }
      if (typeof item.stderr !== "string") {
        throw new Error(`${itemPath}.stderr must be a string`);
      }
      if (!Number.isInteger(item.status)) {
        throw new Error(`${itemPath}.status must be an integer`);
      }
      return;
    }
    if (!hasDelay) {
      throw new Error(`${itemPath} must have command or delayMs`);
    }
  });
}

function validateContext(value, path) {
  assertPlainObject(value, path);
  assertKnownKeys(value, CONTEXT_FIELD_SET, path);
  assertRequiredKeys(value, CONTEXT_FIELDS, path);
  if (typeof value.currentWorkingDirectory !== "string") {
    throw new Error(`${path}.currentWorkingDirectory must be a string`);
  }
  if (typeof value.currentProcess !== "string") {
    throw new Error(`${path}.currentProcess must be a string`);
  }
  if (typeof value.sshPrefix !== "string") {
    throw new Error(`${path}.sshPrefix must be a string`);
  }
  if (typeof value.searchTerm !== "string") {
    throw new Error(`${path}.searchTerm must be a string`);
  }
  if (typeof value.isDangerous !== "boolean") {
    throw new Error(`${path}.isDangerous must be a boolean`);
  }
  assertPlainObject(value.environmentVariables, `${path}.environmentVariables`);
  for (const [key, entry] of Object.entries(value.environmentVariables)) {
    if (typeof key !== "string" || key.length === 0) {
      throw new Error(`${path}.environmentVariables has an empty key`);
    }
    if (typeof entry !== "string") {
      throw new Error(`${path}.environmentVariables[${JSON.stringify(key)}] must be a string`);
    }
  }
}

function validateExpected(value, field, path) {
  assertPlainObject(value, path);
  assertKnownKeys(value, EXPECTED_FIELD_SET, path);
  if (!Object.hasOwn(value, "kind")) {
    throw new Error(`${path} is missing required field "kind"`);
  }
  if (!EXPECTED_KIND_SET.has(value.kind)) {
    throw new Error(`${path}.kind ${JSON.stringify(value.kind)} is not a supported expected kind`);
  }

  const contract = fieldContract(field);
  if (value.kind !== "error" && value.kind !== "timeout" && value.kind !== contract.resultKind) {
    throw new Error(
      `${path}.kind ${JSON.stringify(value.kind)} does not match ${field} result ${JSON.stringify(contract.resultKind)}`,
    );
  }

  if (value.kind === "timeout") {
    if (Object.hasOwn(value, "value") && value.value !== null) {
      throw new Error(`${path}.value must be omitted or null when kind is "timeout"`);
    }
    return;
  }
  if (!Object.hasOwn(value, "value")) {
    throw new Error(`${path} is missing required field "value"`);
  }
  validateExpectedValue(value.value, value.kind, `${path}.value`);
}

function validateExpectedValue(value, kind, path) {
  switch (kind) {
    case "suggestions":
      if (!Array.isArray(value)) {
        throw new Error(`${path} must be an array of Fig suggestions`);
      }
      value.forEach((item, index) => validateSuggestion(item, `${path}[${index}]`));
      return;
    case "string":
    case "error":
      if (typeof value !== "string") {
        throw new Error(`${path} must be a string`);
      }
      return;
    case "bool":
      if (typeof value !== "boolean") {
        throw new Error(`${path} must be a boolean`);
      }
      return;
    case "argv":
      validateArgv(value, path);
      return;
    case "spec":
      validateSpec(value, path);
      return;
    default:
      throw new Error(`${path} has unhandled expected kind ${JSON.stringify(kind)}`);
  }
}

function validateArgv(value, path) {
  if (typeof value === "string") {
    return;
  }
  if (Array.isArray(value)) {
    if (value.some((item) => typeof item !== "string")) {
      throw new Error(`${path} array form must contain only strings`);
    }
    return;
  }
  assertPlainObject(value, path);
  assertKnownKeys(value, new Set(["command", "args"]), path);
  assertRequiredKeys(value, ["command", "args"], path);
  if (typeof value.command !== "string" || value.command.length === 0) {
    throw new Error(`${path}.command must be a non-empty string`);
  }
  if (!Array.isArray(value.args) || value.args.some((item) => typeof item !== "string")) {
    throw new Error(`${path}.args must be an array of strings`);
  }
}

function validateSpec(value, path) {
  assertPlainObject(value, path);
  const names = figNames(value.name) ?? figNames(value.names);
  if (!names || names.length === 0) {
    throw new Error(`${path} must be Fig JSON with a non-empty name (spec_from_fig_json)`);
  }
}

function figNames(value) {
  if (typeof value === "string" && value.length > 0) {
    return [value];
  }
  if (Array.isArray(value) && value.every((item) => typeof item === "string" && item.length > 0)) {
    return value;
  }
  return null;
}

function validateSuggestion(value, path) {
  if (typeof value === "string") {
    if (value.length === 0) {
      throw new Error(`${path} string suggestion must be non-empty`);
    }
    return;
  }
  assertPlainObject(value, path);
  assertKnownKeys(value, FIG_SUGGESTION_FIELD_SET, path);
  if (!Object.hasOwn(value, "name")) {
    throw new Error(`${path} is missing required field "name"`);
  }
  if (!figNames(value.name)) {
    throw new Error(`${path}.name must be a non-empty string or an array of non-empty strings`);
  }
  validateOptionalString(value, "displayName", path);
  validateOptionalString(value, "insertValue", path);
  validateOptionalString(value, "description", path);
  validateOptionalString(value, "icon", path);
  validateOptionalString(value, "type", path);
  validateOptionalString(value, "replaceValue", path);
  if (Object.hasOwn(value, "priority") && !Number.isInteger(value.priority)) {
    throw new Error(`${path}.priority must be an integer`);
  }
  validateOptionalBoolean(value, "hidden", path);
  validateOptionalBoolean(value, "isDangerous", path);
  validateOptionalBoolean(value, "deprecated", path);
  validateOptionalBoolean(value, "shouldAddSpace", path);
}

function validateOptionalString(value, key, path) {
  if (Object.hasOwn(value, key) && typeof value[key] !== "string") {
    throw new Error(`${path}.${key} must be a string`);
  }
}

function validateOptionalBoolean(value, key, path) {
  if (Object.hasOwn(value, key) && typeof value[key] !== "boolean") {
    throw new Error(`${path}.${key} must be a boolean`);
  }
}

function assertPlainObject(value, path) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${path} must be an object`);
  }
}

function assertKnownKeys(value, allowed, path) {
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) {
      throw new Error(`${path} has unknown field ${JSON.stringify(key)}`);
    }
  }
}

function assertRequiredKeys(value, required, path) {
  for (const key of required) {
    if (!Object.hasOwn(value, key)) {
      throw new Error(`${path} is missing required field ${JSON.stringify(key)}`);
    }
  }
}
