import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { auditSpecsHooks } from "./audit-spec-hooks.mjs";
import { compileSpecsIr } from "./compile-spec-ir.mjs";
import {
  checkNativeHookInventory,
  classifyHookBody,
  classifyNativeHooks,
  inventoryFromReport,
  INVENTORY_KIND,
  INVENTORY_VERSION,
  updateNativeHookInventory,
} from "./classify-native-hooks.mjs";
import {
  KNOWN_UNAPPLIED_VERSION_DIFFS,
  KNOWN_VERSION_SELECTORS,
} from "./spec-hook-contract.mjs";

test("classifies pure, input, command, closure, complex, and gated syntax bodies", () => {
  const pure = classifyHookBody({ field: "jsTrigger", body: "() => !0" });
  assert.equal(pure.status, "typed-ir-research-candidate");
  assert.equal(pure.buildTimePureStatic, true);
  assert.equal(pure.nativeExecutable, false);
  assert.deepEqual(pure.freeVariables, []);
  assert.deepEqual(pure.dependencies, {
    input: false,
    command: false,
    environment: false,
    buildTimePureStatic: true,
  });

  const input = classifyHookBody({
    field: "jsGetQueryTerm",
    body: '(term) => term.slice(term.indexOf(":" ) + 1)',
  });
  assert.equal(input.status, "typed-ir-research-candidate");
  assert.equal(input.buildTimePureStatic, false);
  assert.equal(input.dependencies.input, true);
  assert.equal(input.dependencies.command, false);
  assert.equal(input.dependencies.environment, false);

  const command = classifyHookBody({
    field: "jsCustom",
    body: 'async (tokens, exec) => (await exec({ command: "list" })).stdout',
  });
  assert.equal(command.status, "requires-native-adapter");
  assert.equal(command.dependencies.command, true);
  assert.ok(command.risks.includes("async"));
  assert.ok(command.risks.includes("exec"));

  const closure = classifyHookBody({
    field: "jsPostProcess",
    body: "(rows) => rows.map((row) => External(row))",
  });
  assert.equal(closure.status, "requires-native-adapter");
  assert.deepEqual(closure.freeVariables, ["External"]);
  assert.ok(closure.risks.includes("unbound-identifiers"));
  assert.ok(closure.risks.includes("unsupported-syntax"));

  const complexPure = classifyHookBody({
    field: "jsPostProcess",
    body: "(rows) => { const mapped = rows.map((row) => ({ ...row })); return mapped; }",
  });
  assert.equal(complexPure.status, "requires-native-adapter");
  assert.deepEqual(complexPure.freeVariables, []);
  assert.equal(complexPure.nativeExecutable, false);
  assert.ok(complexPure.risks.includes("unsupported-syntax"));

  const global = classifyHookBody({
    field: "jsTrigger",
    body: "() => globalThis.location",
  });
  assert.equal(global.status, "requires-native-adapter");
  assert.equal(global.dependencies.environment, true);
  assert.ok(global.risks.includes("global-this"));
  assert.equal(global.buildTimePureStatic, false);

  const regexpV = classifyHookBody({
    field: "jsTrigger",
    body: "term => /[a-z]/v.test(term)",
  });
  assert.equal(regexpV.status, "syntax-or-analysis-failure");
  assert.equal(regexpV.failureKind, "unsupported-runtime-syntax");
  assert.ok(regexpV.risks.includes("unsupported-syntax"));
  assert.deepEqual(regexpV.unsupportedRuntimeSyntax, ["regexp-v"]);

  const invalid = classifyHookBody({
    field: "jsCustom",
    body: "(tokens =>",
  });
  assert.equal(invalid.status, "syntax-or-analysis-failure");
  assert.equal(invalid.failureKind, "syntax");
});

test("compiles a real helper fixture and reports deterministic native readiness", async () => {
  const sourceRoot = await mkdtemp(join(tmpdir(), "easy-complete-native-src-"));
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-native-ir-"));
  try {
    await writeFile(
      join(sourceRoot, "fixture.js"),
      `function filepaths() {
        return {
          trigger: (before, after) => before.length !== after.length,
          getQueryTerm: (term) => term.slice(term.lastIndexOf("/") + 1),
          custom: async (_tokens, exec) => {
            const markers = ["-1ApL", ".DS_Store"];
            return exec({ command: "ls", args: markers });
          },
        };
      }
      export default {
        name: "fixture",
        args: [
          { name: "path", generators: filepaths() },
          { name: "constant", generators: { trigger: () => !0 } },
          { name: "term", generators: { getQueryTerm: (term) => term.slice(1) } },
          { name: "rows", generators: { postProcess: (rows) => rows } },
          {
            name: "remote",
            generators: {
              custom: async (_tokens, exec) =>
                (await exec({ command: "echo", args: ["ok"] })).stdout,
            },
          },
        ],
      };\n`,
    );
    const compiled = await compileSpecsIr({
      srcDir: sourceRoot,
      outDir: irRoot,
    });
    assert.equal(compiled.compiled, 1);
    const ir = JSON.parse(await readFile(join(irRoot, "fixture.json"), "utf8"));
    assert.equal("jsCustom" in ir.args[0].generators, false);
    assert.equal("jsTrigger" in ir.args[0].generators, false);
    assert.equal("jsGetQueryTerm" in ir.args[0].generators, false);

    const first = await classifyNativeHooks({ sourceRoot, irRoot });
    const second = await classifyNativeHooks({ sourceRoot, irRoot });
    assert.deepEqual(first, second);
    assert.equal(first.coverage.extractedHooks, 5);
    assert.equal(first.coverage.uniqueBodies, 4);
    assert.equal(first.coverage.nativeFilepathsRewrite, 3);
    assert.equal(first.coverage.unclassifiedHooks, 0);
    assert.equal(first.gate.classificationComplete, true);
    assert.equal(first.gate.pathSwitchAllowed, false);
    assert.ok(first.gate.blockers.includes("requires-native-adapter"));
    assert.ok(first.gate.blockers.includes("typed-ir-research-candidate"));
    assert.equal(first.nativeFilepathsRewrites.byField.trigger, 1);
    assert.equal(first.nativeFilepathsRewrites.byField.getQueryTerm, 1);
    assert.equal(first.nativeFilepathsRewrites.byField.custom, 1);
    assert.equal(first.counts.extractedHooks["typed-ir-research-candidate"], 4);
    assert.equal(first.counts.extractedHooks["requires-native-adapter"], 1);

    // The versioned-spec allowlist describes the bundled tree, so a fixture
    // tree reports every entry as absent but still lists it: the gap is a
    // property of the compiler, not of one source tree.
    assert.equal(first.versionedSpecs.status, "unadapted");
    assert.ok(first.gate.blockers.includes("versioned-spec-behaviour-unadapted"));
    assert.deepEqual(
      first.versionedSpecs.selectors.map((entry) => entry.file),
      [...KNOWN_VERSION_SELECTORS].sort(),
    );
    assert.ok(first.versionedSpecs.selectors.every((entry) => !entry.present));
    assert.deepEqual(
      first.versionedSpecs.unappliedDiffs.map((entry) => entry.file),
      Object.keys(KNOWN_UNAPPLIED_VERSION_DIFFS).sort(),
    );
    assert.ok(
      first.versionedSpecs.unappliedDiffs.every(
        (entry) =>
          !entry.present &&
          entry.versions.every((item) => item.functions === null),
      ),
    );

    // The committed inventory is a compact, deterministic projection: one row
    // per distinct body with no per-hook rows, and check/update round-trip.
    const inventory = inventoryFromReport(first);
    assert.equal(inventory.version, INVENTORY_VERSION);
    assert.equal(inventory.kind, INVENTORY_KIND);
    assert.equal(inventory.bodyGroups.length, first.coverage.uniqueBodies);
    assert.equal("hooks" in inventory, false);
    assert.equal("errors" in inventory, false);
    for (const group of inventory.bodyGroups) {
      assert.ok(group.sampleHookIds.length >= 1 && group.sampleHookIds.length <= 3);
      assert.equal(typeof group.hookCount, "number");
      assert.equal(typeof group.status, "string");
    }
    assert.deepEqual(inventoryFromReport(second), inventory);
    const inventoryPath = join(irRoot, "inventory.json");
    await assert.rejects(
      checkNativeHookInventory({ report: first, inventoryPath }),
      /native hook inventory is missing/,
    );
    await updateNativeHookInventory({ report: first, inventoryPath });
    await checkNativeHookInventory({ report: second, inventoryPath });
    assert.deepEqual(
      JSON.parse(await readFile(inventoryPath, "utf8")),
      inventory,
    );
    await writeFile(inventoryPath, `${JSON.stringify(inventory)}\n`);
    await assert.rejects(
      checkNativeHookInventory({ report: first, inventoryPath }),
      /native hook inventory .* is stale/,
    );
  } finally {
    await Promise.all([
      rm(sourceRoot, { recursive: true, force: true }),
      rm(irRoot, { recursive: true, force: true }),
    ]);
  }
});

test("an orphan extracted file is unclassified and closes the migration gate", async () => {
  const sourceRoot = await mkdtemp(join(tmpdir(), "easy-complete-native-src-"));
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-native-ir-"));
  try {
    await writeFile(
      join(sourceRoot, "fixture.js"),
      'export default { name: "fixture", args: [{ name: "value", generators: { trigger: () => !0 } }] };\n',
    );
    await compileSpecsIr({ srcDir: sourceRoot, outDir: irRoot });
    await writeFile(
      join(irRoot, "hooks", "orphan.js"),
      "export default () => !0;\n",
    );
    const report = await classifyNativeHooks({ sourceRoot, irRoot });
    assert.equal(report.coverage.unclassifiedHooks, 1);
    assert.equal(report.gate.classificationComplete, false);
    assert.equal(report.gate.pathSwitchAllowed, false);
    assert.ok(report.gate.blockers.includes("unclassified"));
    assert.deepEqual(
      report.hooks
        .filter((hook) => hook.file === "orphan.js")
        .map((hook) => ({
          status: hook.status,
          reasonCodes: hook.reasonCodes,
        })),
      [{ status: "unclassified", reasonCodes: ["orphan-hook-file"] }],
    );
  } finally {
    await Promise.all([
      rm(sourceRoot, { recursive: true, force: true }),
      rm(irRoot, { recursive: true, force: true }),
    ]);
  }
});

test("full pinned bundle is covered and remains gated", async () => {
  const report = await classifyNativeHooks();
  // The committed inventory must describe this exact bundle; CI runs the
  // same check so a specs update or classifier change is reviewed as a diff.
  await checkNativeHookInventory({ report });
  assert.equal(report.versionedSpecs.status, "unadapted");
  assert.ok(report.versionedSpecs.selectors.every((entry) => entry.present));
  assert.ok(
    report.versionedSpecs.unappliedDiffs.every(
      (entry) =>
        entry.present &&
        entry.versions.every((item) => Number.isInteger(item.functions)),
    ),
  );
  assert.ok(report.versionedSpecs.totals.functionsInUnappliedDiffs > 0);
  const audit = await auditSpecsHooks();
  const auditedUniqueBodies = Object.values(
    audit.hooks.uniqueBodyCounts,
  ).reduce((sum, count) => sum + count, 0);
  const auditedNativeRewrites = Object.values(
    audit.source.nativeRewriteCounts,
  ).reduce((sum, count) => sum + count, 0);
  assert.equal(report.coverage.extractedHooks, audit.hooks.files);
  assert.equal(report.coverage.uniqueBodies, auditedUniqueBodies);
  assert.equal(report.coverage.hookFilesOnDisk, audit.hooks.files);
  assert.equal(report.coverage.nativeFilepathsRewrite, auditedNativeRewrites);
  assert.equal(report.coverage.unclassifiedHooks, 0);
  assert.equal(report.gate.classificationComplete, true);
  assert.equal(report.gate.pathSwitchAllowed, false);
  assert.ok(report.gate.blockers.includes("output-baseline-not-established"));
  for (const status of [
    "requires-native-adapter",
    "typed-ir-research-candidate",
  ]) {
    assert.equal(
      report.gate.blockers.includes(status),
      report.counts.uniqueBodies[status] > 0,
    );
  }
  assert.equal(report.hooks.length, report.coverage.extractedHooks);
  assert.equal(report.bodyGroups.length, report.coverage.uniqueBodies);
  assert.equal(
    Object.values(report.counts.uniqueBodies).reduce(
      (sum, count) => sum + count,
      0,
    ),
    report.coverage.uniqueBodies,
  );
});
