import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { compileSpecsIr } from "./compile-spec-ir.mjs";
import { withReferenceAudit } from "./reference-audit-worker.mjs";
import { captureHookReference } from "./capture-hook-reference.mjs";
import {
  checkHookBaselines,
  updateHookBaselines,
} from "./capture-hook-baseline.mjs";

async function withFixture(body, run) {
  const sourceRoot = await mkdtemp(join(tmpdir(), "easy-complete-baseline-src-"));
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-baseline-ir-"));
  const inputsDir = await mkdtemp(join(tmpdir(), "easy-complete-baseline-in-"));
  const baselinesDir = await mkdtemp(join(tmpdir(), "easy-complete-baseline-out-"));
  try {
    await writeFile(join(sourceRoot, "factory.js"), body);
    await compileSpecsIr({ srcDir: sourceRoot, outDir: irRoot });
    await run({ sourceRoot, irRoot, inputsDir, baselinesDir });
  } finally {
    await Promise.all([
      rm(sourceRoot, { recursive: true, force: true }),
      rm(irRoot, { recursive: true, force: true }),
      rm(inputsDir, { recursive: true, force: true }),
      rm(baselinesDir, { recursive: true, force: true }),
    ]);
  }
}

function factorySource() {
  return `export default {
    name: "factory",
    args: [
      {
        name: "rows",
        generators: {
          postProcess(out) {
            return out.trim() ? [{ name: out.trim() }] : [];
          },
          custom: async function(tokens, exec) {
            const extra = await exec({ command: "list", args: tokens });
            return [{ name: extra.stdout.trim() || tokens[0] }];
          },
        },
      },
    ],
  };\n`;
}

test("captures a small fixture and --check fails after tampering", async () => {
  await withFixture(factorySource(), async (roots) => {
    const updated = await updateHookBaselines(roots);
    assert.ok(updated.count >= 2, "postProcess and custom bodies");
    const checked = await checkHookBaselines(roots);
    assert.equal(checked.count, updated.count);

    const fields = await readdir(roots.baselinesDir);
    assert.ok(fields.includes("postProcess"));
    const names = await readdir(join(roots.baselinesDir, "postProcess"));
    const file = join(roots.baselinesDir, "postProcess", names[0]);
    const original = await readFile(file, "utf8");
    await writeFile(file, original.replace("factory", "tampered"));
    await assert.rejects(() => checkHookBaselines(roots), /stale|drift/);
  });
});

test("mock delayMs triggers a parent timeout", async () => {
  await withFixture(factorySource(), async ({ sourceRoot, irRoot }) => {
    await withReferenceAudit({ sourceRoot, irRoot }, async (audit) => {
      const hookId = audit.sourceToIr[0].hookInstances.custom[0].id;
      const result = await captureHookReference({
        hookId,
        args: [["factory"], { $referenceExec: true }, {
          currentWorkingDirectory: "/repo",
          currentProcess: "zsh",
          sshPrefix: "",
          environmentVariables: { HOME: "/home/user" },
          searchTerm: "",
          isDangerous: false,
        }],
        mockExecRules: [{ command: "list", args: ["factory"], delayMs: 10_000, stdout: "x", stderr: "", status: 0 }],
        timeoutMs: 50,
        sourceRoot,
        irRoot,
      });
      assert.equal(result.status, "timeout");
    });
  });
});
