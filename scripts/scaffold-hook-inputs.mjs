#!/usr/bin/env node
/**
 * Write missing per-body input fixtures for hook output baselines.
 * Record mode uses the reference worker; no real command is executed.
 */
import { access, mkdir, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import { withReferenceAudit } from "./reference-audit-worker.mjs";
import {
  DEFAULT_TIMEOUT_MS,
  MIXED_TEMPLATE_SUGGESTIONS,
  cloneJson,
  defaultContext,
  fixtureIdentity,
  finalizeInputFixture,
  inputRoot,
  loadBodyGroups,
  isUsableCliSample,
  loadCliOutput,
  recordExecRules,
  repoDir,
  scriptArgvForGroup,
  stableStringify,
  timeoutCase,
  tokensForGroup,
} from "./hook-baseline-lib.mjs";

const defaultSourceRoot = join(repoDir, "bundle", "specs");
const defaultIrRoot = join(repoDir, "bundle", "specs-ir");

export async function listInputFiles(root = inputRoot) {
  const files = [];
  let fields = [];
  try {
    fields = await readdir(root, { withFileTypes: true });
  } catch {
    return files;
  }
  for (const field of fields) {
    if (!field.isDirectory()) continue;
    const names = await readdir(join(root, field.name));
    for (const name of names) {
      if (name.endsWith(".json")) files.push(`${field.name}/${name}`);
    }
  }
  return files;
}

function syntheticStdout() {
  return "main\nfeature\n";
}

function malformedStdout() {
  return "not json\n{{{";
}

async function postProcessStdout(scriptArgv) {
  if (Array.isArray(scriptArgv) && scriptArgv.length > 0) {
    const sample = await loadCliOutput(scriptArgv[0], scriptArgv.slice(1));
    if (isUsableCliSample(sample)) {
      return { stdout: sample.stdout, synthetic: false };
    }
  }
  return { stdout: syntheticStdout(), synthetic: true };
}

async function casesForGroup(report, group, irRoot, sourceRoot) {
  const field = group.sourceField;
  const hookId = group.sampleHookIds?.[0] ?? group.hookIds?.[0];
  const tokens = await tokensForGroup(report, group, irRoot);
  const root = tokens[0] ?? "cmd";
  const search = tokens[tokens.length - 1] ?? "";
  const context = defaultContext(search);
  const sub = tokens.length > 2 ? tokens[1] : "status";

  if (field === "postProcess") {
    const scriptArgv = await scriptArgvForGroup(report, group, irRoot);
    const normal = await postProcessStdout(scriptArgv);
    return [
      {
        id: "normal",
        args: [normal.stdout, [...tokens]],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
        ...(normal.synthetic ? { synthetic: true } : {}),
      },
      {
        id: "empty",
        args: ["", [...tokens]],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      },
      {
        id: "malformed",
        args: [malformedStdout(), [...tokens]],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      },
      timeoutCase(field, [normal.stdout, [...tokens]], context),
    ];
  }

  if (field === "script") {
    return [
      {
        id: "root",
        args: [[root]],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      },
      {
        id: "subcommand",
        args: [[root, sub]],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      },
      {
        id: "flag",
        args: [[root, "--help"]],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      },
      timeoutCase(field, [[root]], context),
    ];
  }

  if (field === "custom") {
    const variants = [
      { id: "root", args: [[root]] },
      { id: "subcommand", args: [[root, sub]] },
      { id: "flag", args: [[root, "--help"]] },
    ];
    const cases = [];
    for (const variant of variants) {
      const exec = await recordExecRules({
        hookId,
        field,
        args: variant.args,
        context,
        sourceRoot,
        irRoot,
      });
      cases.push({
        ...variant,
        exec,
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      });
    }
    cases.push(timeoutCase(field, [[root]], context));
    return cases;
  }

  if (field === "trigger") {
    const pairs = [
      ["a", "ab"],
      ["src/", "src"],
      ["", "x"],
      ["a/b", "a/b/"],
      ["foo:bar", "foo:"],
    ];
    return [
      ...pairs.map(([next, prev], index) => ({
        id: `pair-${index + 1}`,
        args: [next, prev],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      })),
      timeoutCase(field, ["a", "ab"], context),
    ];
  }

  if (field === "getQueryTerm") {
    const terms = ["", "a", "src/main.rs", "a:b:c", "--flag=value", "user@host:path"];
    return [
      ...terms.map((term, index) => ({
        id: `term-${index + 1}`,
        args: [term],
        exec: [],
        context: defaultContext(term),
        timeoutMs: DEFAULT_TIMEOUT_MS,
      })),
      timeoutCase(field, ["src/main.rs"], defaultContext("src/main.rs")),
    ];
  }

  if (field === "filterTemplateSuggestions") {
    return [
      {
        id: "mixed",
        args: [cloneJson(MIXED_TEMPLATE_SUGGESTIONS)],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      },
      {
        id: "empty",
        args: [[]],
        exec: [],
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      },
      timeoutCase(field, [cloneJson(MIXED_TEMPLATE_SUGGESTIONS)], context),
    ];
  }

  if (field === "alias" || field === "loadSpec") {
    const tokensFor = ["", root];
    const cases = [];
    for (const token of tokensFor) {
      const exec = await recordExecRules({
        hookId,
        field,
        args: [token],
        context: defaultContext(token),
        sourceRoot,
        irRoot,
      });
      cases.push({
        id: token ? "token" : "empty",
        args: [token],
        exec,
        context: defaultContext(token),
        timeoutMs: DEFAULT_TIMEOUT_MS,
      });
    }
    cases.push(timeoutCase(field, [root], context));
    return cases;
  }

  if (field === "generateSpec") {
    const variants = [[[root]], [[root, sub]]];
    const cases = [];
    for (const [index, args] of variants.entries()) {
      const exec = await recordExecRules({
        hookId,
        field,
        args,
        context,
        sourceRoot,
        irRoot,
      });
      cases.push({
        id: index === 0 ? "root" : "subcommand",
        args,
        exec,
        context,
        timeoutMs: DEFAULT_TIMEOUT_MS,
      });
    }
    cases.push(timeoutCase(field, [[root]], context));
    return cases;
  }

  throw new Error(`unsupported field ${field}`);
}

export async function scaffoldHookInputs({
  sourceRoot = defaultSourceRoot,
  irRoot = defaultIrRoot,
  root = inputRoot,
  refresh = false,
} = {}) {
  return withReferenceAudit({ sourceRoot, irRoot }, async () => {
    const { report, groups } = await loadBodyGroups({ sourceRoot, irRoot });
    let written = 0;
    let skipped = 0;
    for (const group of groups) {
      const dir = join(root, group.sourceField);
      const file = join(dir, `${group.bodySha256}.json`);
      await mkdir(dir, { recursive: true });
      if (!refresh) {
        try {
          await access(file);
          skipped += 1;
          continue;
        } catch {
          // missing input is the only case we fill
        }
      }
      const cases = await casesForGroup(report, group, irRoot, sourceRoot);
      const fixture = finalizeInputFixture({
        ...fixtureIdentity(group),
        cases,
      });
      await writeFile(file, stableStringify(fixture));
      written += 1;
    }
    return { written, skipped, total: groups.length };
  });
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const refresh = process.argv.includes("--refresh");
  const result = await scaffoldHookInputs({ refresh });
  process.stdout.write(
    `Scaffolded hook input fixtures: wrote ${result.written}, kept ${result.skipped}, total ${result.total}\n`,
  );
}
