import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { auditSpecsHooks } from "./audit-spec-hooks.mjs";
import { compileSpecsIr } from "./compile-spec-ir.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const fixtureRoot = join(
  repoDir,
  "crates",
  "fastab_engine",
  "testdata",
  "compiler-hook-chain",
);
const sourceRoot = join(fixtureRoot, "source");
const committedIrRoot = join(fixtureRoot, "specs-ir");

async function filesBelow(root, current = root) {
  const entries = await readdir(current, { withFileTypes: true });
  const files = [];
  for (const entry of entries.sort((left, right) =>
    left.name.localeCompare(right.name),
  )) {
    const path = join(current, entry.name);
    if (entry.isDirectory()) files.push(...(await filesBelow(root, path)));
    else if (entry.isFile()) files.push(relative(root, path));
  }
  return files.sort((left, right) => left.localeCompare(right));
}

async function assertTreesEqual(actualRoot, expectedRoot) {
  const actualFiles = await filesBelow(actualRoot);
  const expectedFiles = await filesBelow(expectedRoot);
  assert.deepEqual(
    actualFiles,
    expectedFiles,
    "checked-in compiler fixture has a different file set",
  );
  for (const file of expectedFiles) {
    assert.deepEqual(
      await readFile(join(actualRoot, file)),
      await readFile(join(expectedRoot, file)),
      `checked-in compiler fixture drifted at ${file}`,
    );
  }
}

test("checked-in hook chain fixture is exact current compiler output", async () => {
  const generatedIrRoot = await mkdtemp(
    join(tmpdir(), "easy-complete-compiler-hook-chain-"),
  );
  try {
    await compileSpecsIr({ srcDir: sourceRoot, outDir: generatedIrRoot });
    await assertTreesEqual(committedIrRoot, generatedIrRoot);

    for (const irRoot of [committedIrRoot, generatedIrRoot]) {
      const audit = await auditSpecsHooks({ sourceRoot, irRoot });
      assert.equal(audit.ok, true, JSON.stringify(audit.errors, null, 2));
    }

    const ir = JSON.parse(
      await readFile(join(committedIrRoot, "chain.json"), "utf8"),
    );
    const postProcessId = ir.args[0].generators[0].jsPostProcess;
    const customId = ir.args[1].generators[0].jsCustom;
    assert.equal(typeof postProcessId, "string");
    assert.equal(typeof customId, "string");

    const sidecar = JSON.parse(
      await readFile(join(committedIrRoot, "typed-hooks.json"), "utf8"),
    );
    const post = sidecar.hooks[postProcessId] ?? sidecar.adapters?.[postProcessId];
    const custom = sidecar.hooks[customId] ?? sidecar.adapters?.[customId];
    assert.ok(post, "typed postProcess must land in the sidecar");
    assert.deepEqual(
      { path: post.path, sourceField: post.sourceField },
      {
        path: "root.args[0].generators.postProcess",
        sourceField: "postProcess",
      },
    );
    assert.match(post.functionBodySha256, /^[a-f0-9]{64}$/);
    // `this.label` custom is not typed IR and is not a registered adapter.
    // The IR id stays; the sidecar must not invent a binding for it.
    assert.equal(custom, undefined);
    assert.equal(sidecar.hooks[customId], undefined);
    assert.equal(sidecar.adapters?.[customId], undefined);
  } finally {
    await rm(generatedIrRoot, { recursive: true, force: true });
  }
});
