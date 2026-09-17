import assert from "node:assert/strict";
import {
  mkdtemp,
  readFile,
  rename as fsRename,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  captureHookModuleReference,
  captureHookReference,
} from "./capture-hook-reference.mjs";
import { compileSpecsIr } from "./compile-spec-ir.mjs";
import { createPairMarker, writePairMarker } from "./spec-pair.mjs";
import { withReferenceAudit } from "./reference-audit-worker.mjs";
import {
  DEFAULT_LIMIT,
  MAX_CONCURRENCY,
  PROBES_PER_INSTANCE,
  PROBES_PER_PATH,
  captureHookReferenceBatch,
  compactBatchReport,
  parseBatchArgs,
  writeBatchReport,
} from "./capture-hook-reference-batch.mjs";
import {
  assessEvidenceStability,
  comparePathRuns,
  compareRuns,
  runWithConcurrency,
} from "./capture-hook-reference-batch-logic.mjs";

async function withFixture(body, run) {
  const sourceRoot = await mkdtemp(join(tmpdir(), "easy-complete-batch-src-"));
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-batch-ir-"));
  try {
    const files = typeof body === "string" ? { "fixture.js": body } : body;
    await Promise.all(
      Object.entries(files).map(([file, contents]) =>
        writeFile(join(sourceRoot, file), contents),
      ),
    );
    await compileSpecsIr({ srcDir: sourceRoot, outDir: irRoot });
    await withReferenceAudit({ sourceRoot, irRoot }, (audit) =>
      run({ sourceRoot, irRoot, audit }),
    );
  } finally {
    await Promise.all([
      rm(sourceRoot, { recursive: true, force: true }),
      rm(irRoot, { recursive: true, force: true }),
    ]);
  }
}

const allFieldsFixture = `
export default {
  name: "fixture",
  loadSpec: async (token, exec) => {
    const output = await exec({ command: "synthetic-command", args: [token] });
    return { name: "loaded-" + output.stdout };
  },
  generateSpec: async (_tokens, _exec) => ({ name: "generated" }),
  parserDirectives: {
    alias: async (token, _exec) => "alias-" + token,
  },
  args: [{
    name: "value",
    generators: {
      trigger: (current, previous) => current !== previous,
      getQueryTerm: (term) => term.slice(term.indexOf(":") + 1),
      script: (tokens) => ["fixture", ...tokens],
      postProcess: (stdout, tokens) => [{ name: stdout + tokens[0] }],
      custom: (tokens, _exec, context) => [{
        name: context.currentProcess + ":" + tokens[0],
      }],
      filterTemplateSuggestions: (suggestions) =>
        suggestions.filter((suggestion) => suggestion.name !== "other.bin"),
    },
  }],
};
`;

test("probes every fixture field twice and records deterministic provenance", async () => {
  await withFixture(allFieldsFixture, async ({ sourceRoot, irRoot, audit }) => {
    // Argument-level getQueryTerm is intentionally extracted twice by the
    // compiler; the batch still selects one deterministic instance per field.
    assert.equal(audit.hookManifest.length, 10);
    const report = await captureHookReferenceBatch({
      sourceRoot,
      irRoot,
      limit: 9,
      concurrency: 2,
      timeoutMs: 3000,
    });

    assert.equal(report.ok, true);
    assert.equal(report.selection.selectedInstances, 9);
    assert.equal(report.stats.attemptedProbes, 36);
    assert.equal(report.stats.expectedProbes, 36);
    assert.equal(report.stats.expectedProbesPerPath, 18);
    assert.equal(report.stats.sourceShaVerified, 18);
    assert.equal(report.stats.moduleShaVerified, 18);
    assert.equal(report.stats.manifestShaVerified, 18);
    assert.equal(report.stats.metadataVerified, 36);
    assert.equal(PROBES_PER_PATH, 2);
    assert.equal(PROBES_PER_INSTANCE, 4);
    assert.equal(report.stats.errors, 0);
    assert.equal(report.stats.pending, 0);
    assert.equal(report.stats.timeout, 0);
    assert.equal(report.stats.unknown, 0);
    assert.equal(report.stats.differingTraceOutput, 0);
    const traced = report.instances.find(
      (instance) => instance.sourceField === "loadSpec",
    );
    assert.equal(traced.runs.source[0].execTrace.length, 1);
    assert.deepEqual(
      traced.runs.source[0].execTrace,
      traced.runs.module[0].execTrace,
    );
    assert.equal(report.baseline.status, "synthetic-source-module-confirmed");
    assert.equal(report.baseline.isRealCliBaseline, false);
    assert.equal(report.contract.realCliFunctionalEquivalence, false);
    assert.equal(report.contract.networkIsolation, false);
    assert.equal(report.provenance.manifest.totalInstances, 10);
    assert.match(
      report.provenance.manifest.hookModulesManifestSha256,
      /^[a-f0-9]{64}$/,
    );
    assert.equal(
      report.provenance.manifest.manifestSha256,
      audit.hookModules.manifestSha256,
    );
    assert.match(report.provenance.manifest.manifestSha256, /^[a-f0-9]{64}$/);
    assert.match(report.provenance.fixture.matrixSha256, /^[a-f0-9]{64}$/);
    assert.match(report.provenance.node.version, /^v\d+\.\d+\.\d+/);
    assert.deepEqual(
      new Set(report.instances.map((instance) => instance.sourceField)),
      new Set([
        "loadSpec",
        "trigger",
        "alias",
        "getQueryTerm",
        "generateSpec",
        "script",
        "postProcess",
        "custom",
        "filterTemplateSuggestions",
      ]),
    );
    const customFixture = report.instances.find(
      (instance) => instance.sourceField === "custom",
    );
    assert.deepEqual(customFixture.fixture.args[2], {
      currentWorkingDirectory: "/__easy_complete_reference__",
      currentProcess: "synthetic-shell",
      searchTerm: "synthetic-search-term",
      sshPrefix: "",
      environmentVariables: {
        EC_REFERENCE_FIXTURE: "1",
        HOME: "/__easy_complete_home__",
        PATH: "/usr/bin:/bin",
        LANG: "C.UTF-8",
      },
      isDangerous: false,
    });

    const compact = compactBatchReport(report);
    assert.equal("instances" in compact, false);
    assert.equal("selectedHookIds" in compact.selection, false);
    assert.equal(compact.stats.attemptedProbes, 36);
  });
});

test("rejects caller-supplied full, compact, and file-backed audits", async () => {
  await withFixture(allFieldsFixture, async ({ sourceRoot, irRoot, audit }) => {
    await assert.rejects(
      captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        audit,
        limit: 1,
        concurrency: 1,
      }),
      /no longer accepts an audit report/,
    );
    await assert.rejects(
      captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        audit: { ok: true, reproducibility: {} },
        limit: 1,
        concurrency: 1,
      }),
      /no longer accepts an audit report/,
    );
    await assert.rejects(
      captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        auditPath: "/tmp/stale-audit.json",
        limit: 1,
        concurrency: 1,
      }),
      /no longer accepts auditPath/,
    );
  });
});

test("fresh audit supports a custom hooks root", async () => {
  await withFixture(allFieldsFixture, async ({ sourceRoot, irRoot, audit }) => {
    const customHooksRoot = await mkdtemp(
      join(tmpdir(), "easy-complete-custom-hooks-"),
    );
    try {
      await rm(customHooksRoot, { recursive: true, force: true });
      await fsRename(join(irRoot, "hooks"), customHooksRoot);
      await writePairMarker(
        irRoot,
        await createPairMarker({ sourceRoot, irRoot }),
      );
      await withReferenceAudit(
        { sourceRoot, irRoot, hooksRoot: customHooksRoot },
        async (customAudit) => {
          assert.equal(customAudit.ok, true);
          const report = await captureHookReferenceBatch({
            sourceRoot,
            irRoot,
            hooksRoot: customHooksRoot,
            limit: 1,
            concurrency: 1,
          });
          assert.equal(report.ok, true);
          assert.equal(report.selection.selectedInstances, 1);
        },
      );
    } finally {
      await fsRename(customHooksRoot, join(irRoot, "hooks"));
      await writePairMarker(
        irRoot,
        await createPairMarker({ sourceRoot, irRoot }),
      );
      await rm(customHooksRoot, { recursive: true, force: true });
    }
  });
});

test("a spinning hook is timeout or path-divergent, never a baseline", async () => {
  await withFixture(
    `export default {
      name: "fixture",
      args: { name: "value", generators: { trigger() { while (true) {} } } },
    };\n`,
    async ({ sourceRoot, irRoot }) => {
      const report = await captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        limit: 1,
        concurrency: 1,
        timeoutMs: 500,
      });
      assert.equal(report.ok, false);
      assert.equal(report.stats.timeout, 4);
      assert.equal(report.stats.errors, 0);
      assert.equal(report.stats.confirmedInstances, 0);
      assert.equal(report.baseline.status, "not-established");
      assert.equal(report.instances[0].comparison.status, "divergent");
      assert.equal(report.instances[0].comparison.baselineConfirmed, false);
    },
  );
});

test("same-text hooks keep distinct closures on their generated module paths", async () => {
  await withFixture(
    {
      "one.js": `const prefix = "one";
export default {
  name: "one",
  args: { name: "value", generators: { trigger: function () { return prefix; } } },
};
`,
      "two.js": `const prefix = "two";
export default {
  name: "two",
  args: { name: "value", generators: { trigger: function () { return prefix; } } },
};
`,
    },
    async ({ sourceRoot, irRoot }) => {
      const report = await captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        limit: 2,
        concurrency: 1,
      });
      assert.equal(report.ok, true);
      assert.equal(report.stats.confirmedInstances, 2);
      assert.equal(
        report.instances[0].functionBodySha256,
        report.instances[1].functionBodySha256,
      );
      assert.equal(
        new Set(report.instances.map((instance) => instance.module.file)).size,
        2,
      );
      assert.deepEqual(
        report.instances.map((instance) => instance.comparison.outputEqual),
        [true, true],
      );
      assert.deepEqual(
        new Set(
          report.instances.map((instance) => instance.runs.source[0].value),
        ),
        new Set(["one", "two"]),
      );
    },
  );
});

test("a tampered generated module fails closed before a parity probe", async () => {
  await withFixture(allFieldsFixture, async ({ sourceRoot, irRoot, audit }) => {
    const moduleManifest = JSON.parse(
      await readFile(join(irRoot, "hook-modules.json"), "utf8"),
    );
    const moduleFile = Object.keys(moduleManifest.modules)[0];
    const modulePath = join(irRoot, "source-modules", moduleFile);
    const original = await readFile(modulePath, "utf8");
    await writeFile(modulePath, `${original}\n`);
    await assert.rejects(
      captureHookModuleReference({
        hookId: audit.hookManifest[0].id,
        sourceRoot,
        irRoot,
      }),
      /module.*SHA differs/,
    );
    await assert.rejects(
      captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        limit: 1,
      }),
      /strict source\/IR audit failed|module.*SHA differs/,
    );
    await writeFile(modulePath, original);
  });
});

test("same stable source/module errors remain inconclusive, never confirmed", async () => {
  await withFixture(
    `export default {
      name: "fixture",
      args: { name: "value", generators: { trigger() { throw new Error("fixture error"); } } },
    };\n`,
    async ({ sourceRoot, irRoot }) => {
      const report = await captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        limit: 1,
        concurrency: 1,
      });
      assert.equal(report.ok, false);
      assert.equal(report.stats.errors, 4);
      assert.equal(report.stats.confirmedInstances, 0);
      assert.equal(report.stats.divergentInstances, 0);
      assert.equal(report.instances[0].comparison.status, "inconclusive");
      assert.equal(report.instances[0].comparison.source.stable, true);
      assert.equal(report.instances[0].comparison.module.stable, true);
      assert.equal(report.instances[0].comparison.source.errorEqual, true);
      assert.equal(report.instances[0].comparison.module.errorEqual, true);
      assert.equal(report.instances[0].comparison.crossPathErrorEqual, true);
    },
  );
});

test("public batch API rejects injected probes", async () => {
  await withFixture(
    `export default {
      name: "fixture",
      args: { name: "value", generators: { trigger() { return true; } } },
    };\n`,
    async ({ sourceRoot, irRoot }) => {
      await assert.rejects(
        captureHookReferenceBatch({
          sourceRoot,
          irRoot,
          limit: 1,
          sourceProbe: captureHookReference,
        }),
        /unknown field sourceProbe/,
      );
    },
  );
});

test("comparison logic marks cross-path differences and repeat instability divergent", () => {
  const run = (value, extra = {}) => ({
    status: "success",
    value,
    execTrace: [],
    metadataVerified: true,
    ...extra,
  });
  const crossPath = compareRuns(
    [run({ answer: "source" }), run({ answer: "source" })],
    [run({ answer: "module" }), run({ answer: "module" })],
  );
  assert.equal(crossPath.status, "divergent");
  assert.equal(crossPath.crossPathDifference, true);
  assert.equal(crossPath.differingTraceOutput, true);

  const unstable = compareRuns(
    [run("first"), run("second")],
    [run("module"), run("module")],
  );
  assert.equal(unstable.status, "divergent");
  assert.equal(unstable.unstable, true);
  assert.equal(unstable.source.stable, false);
  assert.equal(comparePathRuns([run("error"), run("error")]).stable, true);
});

test("runner preserves order and evidence logic rejects an in-flight IR digest change", async () => {
  const seen = await runWithConcurrency(
    [1, 2, 3, 4],
    2,
    async (value) => {
      if (value === 3) throw new Error("synthetic worker failure");
      return value * 2;
    },
    (error) => ({ status: "batch-error", message: error.message }),
  );
  assert.deepEqual(seen, [2, 4, { status: "batch-error", message: "synthetic worker failure" }, 8]);

  const evidence = assessEvidenceStability({
    harnessBefore: { harness: "same" },
    harnessAfter: { harness: "same" },
    auditBefore: "audit-before",
    auditAfter: "audit-before",
    artifactsBefore: { irFiles: [{ file: "fixture.json", sha256: "old" }] },
    artifactsAfter: { irFiles: [{ file: "fixture.json", sha256: "new" }] },
  });
  assert.equal(evidence.artifactsStable, false);
  assert.equal(evidence.evidenceStable, false);
  assert.equal(
    evidence.artifactsDrift,
    "audited source/module evidence changed during run",
  );
});

test("changed IR or extracted hook cannot reuse an audited source mapping", async () => {
  await withFixture(allFieldsFixture, async ({ sourceRoot, irRoot, audit }) => {
    const irPath = join(irRoot, "fixture.json");
    const originalIr = await readFile(irPath, "utf8");
    await writeFile(irPath, `${originalIr} \n`);
    await assert.rejects(
      captureHookReferenceBatch({ sourceRoot, irRoot, limit: 1 }),
      /strict source\/IR audit failed|IR .* SHA differs/,
    );
    await writeFile(irPath, originalIr);
    const first = audit.hookManifest.find(
      (item) => item.field === "jsLoadSpec",
    );
    assert.ok(first);
    const hookPath = join(irRoot, "hooks", first.file);
    const originalHook = await readFile(hookPath, "utf8");
    await writeFile(hookPath, `${originalHook} \n`);
    await assert.rejects(
      captureHookReferenceBatch({ sourceRoot, irRoot, limit: 1 }),
      /strict source\/IR audit failed|hook .* SHA differs/,
    );
    await writeFile(hookPath, originalHook);
  });
});

test("public batch API rejects injected probe overrides", async () => {
  await withFixture(allFieldsFixture, async ({ sourceRoot, irRoot }) => {
    await assert.rejects(
      captureHookReferenceBatch({
        sourceRoot,
        irRoot,
        limit: 1,
        probe: captureHookReference,
      }),
      /unknown field probe/,
    );
  });
});

test("detailed output is exclusive-create and the stdout shape is thin", async () => {
  await withFixture(allFieldsFixture, async ({ sourceRoot, irRoot }) => {
    const report = await captureHookReferenceBatch({
      sourceRoot,
      irRoot,
      limit: 1,
      concurrency: 1,
      timeoutMs: 3000,
    });
    const outputRoot = await mkdtemp(
      join(tmpdir(), "easy-complete-batch-out-"),
    );
    try {
      const outputPath = join(outputRoot, "diagnostic.json");
      await writeBatchReport(outputPath, report);
      const first = await readFile(outputPath, "utf8");
      assert.match(first, /"instances"/);
      await assert.rejects(
        writeBatchReport(outputPath, report),
        /refusing to overwrite existing report/,
      );
      assert.equal(await readFile(outputPath, "utf8"), first);
      assert.equal((await stat(outputPath)).isFile(), true);
    } finally {
      await rm(outputRoot, { recursive: true, force: true });
    }
  });
});

test("CLI options keep default selection controlled and cap concurrency", () => {
  const defaults = parseBatchArgs([]);
  assert.equal(defaults.limit, DEFAULT_LIMIT);
  assert.equal(defaults.all, false);
  assert.equal(defaults.concurrency, MAX_CONCURRENCY);
  assert.equal(parseBatchArgs(["--all"]).all, true);
  assert.equal(parseBatchArgs(["--limit=3", "--concurrency", "1"]).limit, 3);
  assert.equal(
    parseBatchArgs(["--hooks-root=/tmp/custom-hooks"]).hooksRoot,
    "/tmp/custom-hooks",
  );
  assert.throws(
    () => parseBatchArgs(["--all", "--limit", "3"]),
    /cannot be combined/,
  );
  assert.throws(() => parseBatchArgs(["--concurrency", "5"]), /1..4/);
});
