import assert from "node:assert/strict";
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";

import {
  captureHookModuleReferenceBatch,
  captureHookModuleReference,
  captureHookReferenceBatch,
  captureHookReference,
} from "./capture-hook-reference.mjs";
import { compileSpecsIr } from "./compile-spec-ir.mjs";
import { withReferenceAudit } from "./reference-audit-worker.mjs";

async function withReferenceFixture(body, run, extraFiles = {}) {
  const sourceRoot = await mkdtemp(
    join(tmpdir(), "easy-complete-reference-src-"),
  );
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-reference-ir-"));
  try {
    await writeFile(join(sourceRoot, "factory.js"), body);
    for (const [name, content] of Object.entries(extraFiles)) {
      const file = join(sourceRoot, name);
      await mkdir(dirname(file), { recursive: true });
      await writeFile(file, content);
    }
    await compileSpecsIr({ srcDir: sourceRoot, outDir: irRoot });
    await withReferenceAudit({ sourceRoot, irRoot }, async (audit) => {
      assert.equal(audit.ok, true);
      await run({ sourceRoot, irRoot, audit });
    });
  } finally {
    await Promise.all([
      rm(sourceRoot, { recursive: true, force: true }),
      rm(irRoot, { recursive: true, force: true }),
    ]);
  }
}

test("original source closure makes identical hook text produce different outputs", async () => {
  await withReferenceFixture(
    `function generator(prefix) {
       return { postProcess(stdout) { return [{ name: prefix + stdout }]; } };
     }
     export default {
       name: "factory",
       args: [
         { name: "one", generators: generator("A") },
         { name: "two", generators: generator("B") },
       ],
     };\n`,
    async ({ sourceRoot, irRoot, audit }) => {
      const instances = audit.sourceToIr[0].hookInstances.postProcess;
      assert.equal(instances.length, 2);
      assert.equal(instances[0].sha256, instances[1].sha256);
      const outputs = await Promise.all(
        instances.map((instance) =>
          captureHookReference({
            hookId: instance.id,
            args: ["x", []],
            sourceRoot,
            irRoot,
          }),
        ),
      );
      assert.deepEqual(
        outputs.map((output) => output.value),
        [[{ name: "Ax" }], [{ name: "Bx" }]],
      );
      assert.ok(
        outputs.every(
          (output) =>
            output.status === "success" &&
            output.referenceKind === "synthetic-probe-not-real-cli-baseline" &&
            output.execTrace.length === 0,
        ),
      );
    },
  );
});

test("generated closure modules match source output for same-text closures", async () => {
  await withReferenceFixture(
    `function generator(prefix) {
       return {
         marker: prefix,
         postProcess(stdout) { return [{ name: prefix + stdout }]; },
         custom(tokens) { return [{ name: this.marker + tokens[0] }]; },
       };
     }
     export default {
       name: "factory",
       args: [
         { name: "one", generators: generator("A") },
         { name: "two", generators: generator("B") },
       ],
     };
`,
    async ({ sourceRoot, irRoot, audit }) => {
      const instances = audit.sourceToIr[0].hookInstances;
      for (const field of ["postProcess", "custom"]) {
        assert.equal(instances[field].length, 2);
        assert.equal(instances[field][0].sha256, instances[field][1].sha256);
        for (const instance of instances[field]) {
          const args =
            field === "postProcess"
              ? ["x", []]
              : [["x"], { $referenceExec: true }];
          const [source, generated] = await Promise.all([
            captureHookReference({
              hookId: instance.id,
              args,
              sourceRoot,
              irRoot,
            }),
            captureHookModuleReference({
              hookId: instance.id,
              args,
              sourceRoot,
              irRoot,
            }),
          ]);
          assert.equal(source.status, "success");
          assert.equal(generated.status, "success");
          assert.deepEqual(generated.value, source.value);
          assert.deepEqual(generated.execTrace, source.execTrace);
          assert.equal(generated.moduleShaVerified, true);
        }
      }
    },
  );
});

test("batch probes load each path once and retain independent traces/results", async () => {
  await withReferenceFixture(
    `function generator(prefix) {
       return {
         custom: async function(tokens, exec) {
           const result = await exec({ command: "fixture", args: [tokens[0]] });
           return [{ name: prefix + result.stdout }];
         },
       };
     }
     export default {
       name: "factory",
       args: [
         { name: "one", generators: generator("A") },
         { name: "two", generators: generator("B") },
       ],
     };
`,
    async ({ sourceRoot, irRoot, audit }) => {
      const instances = audit.sourceToIr[0].hookInstances.custom;
      const invocations = [
        {
          args: [["first"], { $referenceExec: true }],
          mockExecRules: [
            {
              command: "fixture",
              args: ["first"],
              stdout: "-1",
            },
          ],
        },
        {
          args: [["second"], { $referenceExec: true }],
          mockExecRules: [
            {
              command: "fixture",
              args: ["second"],
              stdout: "-2",
            },
          ],
        },
      ];
      for (const instance of instances) {
        const hookId = instance.id;
        const [source, module] = await Promise.all([
          captureHookReferenceBatch({
            hookId,
            invocations,
            sourceRoot,
            irRoot,
          }),
          captureHookModuleReferenceBatch({
            hookId,
            invocations,
            sourceRoot,
            irRoot,
          }),
        ]);
        assert.equal(source.status, "batch-success");
        assert.equal(module.status, "batch-success");
        assert.equal(source.runs.length, invocations.length);
        assert.equal(module.runs.length, invocations.length);
        assert.deepEqual(
          source.runs.map((run) => run.value),
          module.runs.map((run) => run.value),
        );
        assert.deepEqual(
          source.runs.map((run) => run.execTrace),
          module.runs.map((run) => run.execTrace),
        );
        assert.deepEqual(
          source.runs.map((run) => run.execTrace),
          [
            [{ command: "fixture", args: ["first"] }],
            [{ command: "fixture", args: ["second"] }],
          ],
        );
        assert.ok(
          source.runs.every(
            (run) => run.status === "success" && run.sourceShaVerified === true,
          ),
        );
        assert.ok(
          module.runs.every(
            (run) => run.status === "success" && run.moduleShaVerified === true,
          ),
        );
      }
    },
  );
});

test("batch probe rejects empty, oversized, and malformed invocation lists", async () => {
  await assert.rejects(
    captureHookReferenceBatch({ hookId: "fixture#trigger#0", invocations: [] }),
    /non-empty array/,
  );
  await assert.rejects(
    captureHookReferenceBatch({
      hookId: "fixture#trigger#0",
      invocations: Array.from({ length: 257 }, () => ({
        args: [],
        mockExecRules: [],
      })),
    }),
    /exceeds 256 entries/,
  );
  await assert.rejects(
    captureHookReferenceBatch({
      hookId: "fixture#trigger#0",
      invocations: [{ args: ["ok"], mockExecRules: [], extra: true }],
    }),
    /only args and mockExecRules arrays/,
  );
  await assert.rejects(
    captureHookReferenceBatch({
      hookId: "fixture#trigger#0",
      invocations: [{ args: ["x".repeat(1_500_001)], mockExecRules: [] }],
    }),
    /1500000-byte input limit/,
  );
});

test("batch probe fails closed when a hook exceeds the worker deadline", async () => {
  await withReferenceFixture(
    `export default { name: "factory", args: { generators: {
       trigger() { while (true) {} }
     } } };\n`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.trigger[0].id;
      const invocations = [{ args: ["a", "b"], mockExecRules: [] }];
      const source = await captureHookReferenceBatch({
        hookId,
        invocations,
        sourceRoot,
        irRoot,
        timeoutMs: 300,
      });
      assert.equal(source.status, "timeout");
      assert.deepEqual(source.runs, []);

      const module = await captureHookModuleReferenceBatch({
        hookId,
        invocations,
        sourceRoot,
        irRoot,
        timeoutMs: 300,
      });
      assert.equal(module.status, "timeout");
      assert.deepEqual(module.runs, []);
    },
  );
});

test("generated module reference fails closed on manifest or module tampering", async () => {
  await withReferenceFixture(
    `const suffix = "-kept";
     export default { name: "factory", args: { generators: {
       postProcess(stdout) { return [{ name: stdout + suffix }]; },
     } } };
`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.postProcess[0].id;
      const manifestPath = join(irRoot, "hook-modules.json");
      const originalManifest = await readFile(manifestPath, "utf8");
      await rm(manifestPath);
      await assert.rejects(
        captureHookModuleReference({ hookId, sourceRoot, irRoot }),
        /manifest is unavailable/,
      );
      await writeFile(manifestPath, originalManifest);

      await writeFile(manifestPath, `${originalManifest}tampered`);
      await assert.rejects(
        captureHookModuleReference({ hookId, sourceRoot, irRoot }),
        /manifest SHA differs from audit/,
      );
      await writeFile(manifestPath, originalManifest);

      const manifest = JSON.parse(originalManifest);
      const moduleFile = manifest.hooks[hookId].module;
      const modulePath = join(irRoot, "source-modules", moduleFile);
      const originalModule = await readFile(modulePath, "utf8");
      await writeFile(modulePath, `${originalModule}tampered`);
      await assert.rejects(
        captureHookModuleReference({ hookId, sourceRoot, irRoot }),
        /module SHA differs from manifest/,
      );
      await writeFile(modulePath, originalModule);

      const missingHookManifest = JSON.parse(originalManifest);
      delete missingHookManifest.hooks[hookId];
      const missingHookText = `${JSON.stringify(missingHookManifest)}\n`;
      await writeFile(manifestPath, missingHookText);
      await assert.rejects(
        captureHookModuleReference({
          hookId,
          sourceRoot,
          irRoot,
        }),
        /manifest SHA differs from audit/,
      );

      await writeFile(manifestPath, originalManifest);
      manifest.hooks[hookId].module = "../outside.js";
      await writeFile(manifestPath, `${JSON.stringify(manifest)}\n`);
      await assert.rejects(
        captureHookModuleReference({
          hookId,
          sourceRoot,
          irRoot,
        }),
        /manifest SHA differs from audit/,
      );

      for (const { field, value, error } of [
        {
          field: "path",
          value: "root.args[99].generators.postProcess",
          error: /module path differs from the audit/,
        },
        {
          field: "sourceField",
          value: "custom",
          error: /module source field differs from the audit/,
        },
        {
          field: "functionBodySha256",
          value: "0".repeat(64),
          error: /module function body SHA differs from the audit/,
        },
      ]) {
        const provenanceManifest = JSON.parse(originalManifest);
        provenanceManifest.hooks[hookId][field] = value;
        const provenanceText = `${JSON.stringify(provenanceManifest)}\n`;
        await writeFile(manifestPath, provenanceText);
        await assert.rejects(
          captureHookModuleReference({
            hookId,
            sourceRoot,
            irRoot,
          }),
          /manifest SHA differs from audit/,
        );
      }
      await writeFile(manifestPath, originalManifest);
    },
  );
});

test("reference probe rejects a caller-supplied stale audit", async () => {
  await assert.rejects(
    captureHookReference({
      hookId: "fixture#custom#0",
      audit: { ok: true, hookManifest: [], sourceToIr: [] },
    }),
    /no longer accepts an audit report/,
  );
});

test("all single and batch probe APIs reject audit and injection fields", async () => {
  const apis = [
    captureHookReference,
    captureHookModuleReference,
    captureHookReferenceBatch,
    captureHookModuleReferenceBatch,
  ];
  for (const api of apis) {
    for (const field of ["audit", "auditPath", "probe", "sourceProbe", "moduleProbe", "unexpected"]) {
      await assert.rejects(
        api({ [field]: true }),
        new RegExp(
          field === "audit"
            ? "no longer accepts an audit report"
            : field === "auditPath"
              ? "no longer accepts auditPath"
              : `unknown field ${field}`,
        ),
      );
    }
  }
});

test("changed module closure with unchanged hook body cannot pass source SHA verification", async () => {
  await withReferenceFixture(
    `const prefix = "A";
     export default { name: "factory", args: { generators: {
       postProcess(stdout) { return [{ name: prefix + stdout }]; },
    } } };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.postProcess[0].id;
      const sourcePath = join(sourceRoot, "factory.js");
      const originalSource = await readFile(sourcePath, "utf8");
      await writeFile(
        sourcePath,
        `const prefix = "B";
         export default { name: "factory", args: { generators: {
           postProcess(stdout) { return [{ name: prefix + stdout }]; },
         } } };`,
      );
      const result = await captureHookReference({
        hookId,
        args: ["x", []],
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "error");
      assert.equal(result.stage, "reference");
      assert.notEqual(result.sourceShaVerified, true);
      await writeFile(sourcePath, originalSource);
    },
  );
});

test("test-only marker text stays literal when it comes from source", async () => {
  await withReferenceFixture(
    `export default { name: "factory", generateSpec: () => ({
       name: "dynamic",
       note: { $ecReferenceSignature: "test-only", $ecReferenceKind: "function", source: "real value" },
     }) };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.generateSpec[0].id;
      const result = await captureHookReference({
        hookId,
        args: [[]],
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "success");
      assert.deepEqual(result.value.note, {
        $ecReferenceKind: "function",
        $ecReferenceSignature: "test-only",
        source: "real value",
      });
    },
  );
});

test("serialization failure retains mocked exec trace", async () => {
  await withReferenceFixture(
    `export default { name: "factory", args: { generators: {
       custom: async function(tokens, exec) {
         await exec({ command: "fixture", args: [tokens[0]] });
         const rows = [{ name: "value" }];
         rows.push(rows);
         return rows;
       }
     } } };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.custom[0].id;
      const result = await captureHookReference({
        hookId,
        args: [["key"], { $referenceExec: true }],
        mockExecRules: [{ command: "fixture", args: ["key"] }],
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "error");
      assert.equal(result.stage, "serialize");
      assert.equal(result.errorClass, "TypeError");
      assert.deepEqual(result.execTrace, [
        { command: "fixture", args: ["key"] },
      ]);
    },
  );
});

test("custom source owner, shell context, and exec fixture are retained without running a CLI", async () => {
  await withReferenceFixture(
    `export default {
       name: "factory",
       args: {
         name: "remote",
         generators: {
           marker: "@",
           custom: async function(tokens, exec, context) {
             const output = await exec({ command: "fixture", args: [tokens[0]], cwd: context.currentWorkingDirectory });
             return [{ name: this.marker + context.environmentVariables.MARKER + output.stdout }];
           },
         },
       },
     };\n`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.custom[0].id;
      const base = {
        hookId,
        args: [
          ["key"],
          { $referenceExec: true },
          {
            currentWorkingDirectory: "/fixture/project",
            currentProcess: "/bin/zsh",
            environmentVariables: { MARKER: "M" },
          },
        ],
        sourceRoot,
        irRoot,
      };
      const success = await captureHookReference({
        ...base,
        mockExecRules: [
          {
            command: "fixture",
            args: ["key"],
            cwd: "/fixture/project",
            status: 0,
            stdout: "value",
          },
        ],
      });
      assert.equal(success.status, "success");
      assert.deepEqual(success.value, [{ name: "@Mvalue" }]);
      assert.deepEqual(success.execTrace, [
        { command: "fixture", args: ["key"], cwd: "/fixture/project" },
      ]);

      const unmatched = await captureHookReference(base);
      assert.equal(unmatched.status, "error");
      assert.equal(unmatched.errorClass, "UnmockedCommand");
      assert.deepEqual(unmatched.execTrace, [
        { command: "fixture", args: ["key"], cwd: "/fixture/project" },
      ]);
      const wrongDirectory = await captureHookReference({
        ...base,
        mockExecRules: [
          {
            command: "fixture",
            args: ["key"],
            cwd: "/fixture/other",
            stdout: "value",
          },
        ],
      });
      assert.equal(wrongDirectory.errorClass, "UnmockedCommand");
      const rejected = await captureHookReference({
        ...base,
        mockExecRules: [
          {
            command: "fixture",
            args: ["key"],
            cwd: "/fixture/project",
            kind: "reject",
          },
        ],
      });
      assert.equal(rejected.errorClass, "FixtureRejected");
    },
  );
});

test("VM source cannot alter frozen primordials or mutate a captured exec request", async () => {
  await withReferenceFixture(
    `export default { name: "factory", args: { generators: {
       custom: async function(tokens, exec) {
         let blocked = 0;
         for (const mutate of [
           () => { Array.prototype.push = () => {}; },
           () => { Promise.prototype.then = () => Promise.resolve([{ name: "forged" }]); },
           () => { Function.prototype.call = () => "forged"; },
           () => { JSON.stringify = () => '"forged-json"'; },
         ]) {
           try { mutate(); } catch { blocked += 1; }
         }
         const request = { command: "fixture", args: [tokens[0]] };
         await exec(request);
         request.command = "evil";
         request.args = ["changed"];
         return [{ name: "real-" + blocked, nested: () => "kept" }];
       }
     } } };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.custom[0].id;
      const result = await captureHookReference({
        hookId,
        args: [["key"], { $referenceExec: true }],
        mockExecRules: [{ command: "fixture", args: ["key"] }],
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "success");
      assert.equal(result.value[0].name, "real-4");
      assert.equal(result.value[0].nested.kind, "function");
      assert.match(result.value[0].nested.bodySha256, /^[a-f0-9]{64}$/);
      assert.deepEqual(result.execTrace, [
        { command: "fixture", args: ["key"] },
      ]);
    },
  );
});

test("preoccupied non-configurable globals cannot capture private bridge values", async () => {
  await withReferenceFixture(
    `const captured = [];
     for (const name of ["__referenceInvoke", "__fn", "__receiver", "__argsJson",
       "__execRulesJson", "__referenceSerialize", "__result", "__referenceSignature"]) {
       Object.defineProperty(globalThis, name, {
         configurable: false,
         get() { return "occupied"; },
         set(value) { captured.push(typeof value); },
       });
     }
     export default { name: "factory", args: { generators: {
       custom: async function(tokens, exec) {
         await exec({ command: "fixture", args: [tokens[0]] });
         return [{ name: "captures-" + captured.length }];
       }
     } } };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.custom[0].id;
      const result = await captureHookReference({
        hookId,
        args: [["key"], { $referenceExec: true }],
        mockExecRules: [{ command: "fixture", args: ["key"] }],
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "success");
      assert.deepEqual(result.value, [{ name: "captures-0" }]);
      assert.deepEqual(result.execTrace, [
        { command: "fixture", args: ["key"] },
      ]);
    },
  );
});

test("parent watchdog stops a spinning source hook", async () => {
  await withReferenceFixture(
    `export default { name: "factory", args: { generators: {
       trigger() { while (true) {} }
     } } };\n`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.trigger[0].id;
      const result = await captureHookReference({
        hookId,
        args: ["a", ""],
        timeoutMs: 500,
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "timeout");
    },
  );
});

test("an unresolved mocked command is stopped by the parent deadline", async () => {
  await withReferenceFixture(
    `export default { name: "factory", args: { generators: {
       custom: async function(tokens, exec) {
         await exec({ command: "fixture", args: [tokens[0]] });
         return [{ name: "never" }];
       }
     } } };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.custom[0].id;
      const result = await captureHookReference({
        hookId,
        args: [["key"], { $referenceExec: true }],
        mockExecRules: [{ command: "fixture", args: ["key"], kind: "pending" }],
        timeoutMs: 300,
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "timeout");
    },
  );
});

test("generated spec functions remain visible as hashes instead of disappearing", async () => {
  await withReferenceFixture(
    `export default { name: "factory", generateSpec: () => ({
       name: "dynamic",
       tag: Symbol("fixture"),
       args: { name: "value", generators: { custom: () => [{ name: "hint" }] } }
     }) };\n`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.generateSpec[0].id;
      const result = await captureHookReference({
        hookId,
        args: [[]],
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "success");
      assert.equal(result.value.name, "dynamic");
      assert.deepEqual(result.value.tag, {
        kind: "symbol",
        value: "Symbol(fixture)",
      });
      assert.equal(result.value.args.generators.custom.kind, "function");
      assert.match(
        result.value.args.generators.custom.bodySha256,
        /^[a-f0-9]{64}$/,
      );
    },
  );
});

test("special result values are typed while hidden fields fail closed", async () => {
  await withReferenceFixture(
    `export default { name: "factory", generateSpec: () => ({
       name: "dynamic", signedZero: -0, failure: new Error("fixture failed"),
       hidden: Object.defineProperty({ name: "row" }, "secret", { value: 42 }),
     }) };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.generateSpec[0].id;
      const blocked = await captureHookReference({
        hookId,
        args: [[]],
        sourceRoot,
        irRoot,
      });
      assert.equal(blocked.status, "error");
      assert.equal(blocked.stage, "serialize");
      assert.equal(Object.hasOwn(blocked, "value"), false);
    },
  );
  await withReferenceFixture(
    `export default { name: "factory", generateSpec: () => ({
       name: "dynamic", signedZero: -0, failure: new Error("fixture failed"),
     }) };`,
    async ({ sourceRoot, irRoot, audit }) => {
      const hookId = audit.sourceToIr[0].hookInstances.generateSpec[0].id;
      const result = await captureHookReference({
        hookId,
        args: [[]],
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "success");
      assert.deepEqual(result.value.signedZero, { kind: "negative-zero" });
      assert.equal(result.value.failure.kind, "error");
      assert.equal(result.value.failure.name, "Error");
      assert.equal(result.value.failure.message, "fixture failed");
      assert.equal(typeof result.value.failure.stack, "string");
    },
  );
});
