import assert from "node:assert/strict";
import { lstat, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  ASDF_GET_QUERY_TERM_CANDIDATE_IDS,
  buildTypedGetQueryTermReference,
  buildTypedTriggerReference,
  GET_QUERY_TERM_INPUT_CORPUS,
  checkTypedTriggerReference,
  INPUT_CORPUS,
  updateTypedTriggerReference,
} from "./capture-typed-trigger-reference.mjs";
import { compileSpecsIr } from "./compile-spec-ir.mjs";

async function withTypedFixture(run) {
  const sourceRoot = await mkdtemp(join(tmpdir(), "easy-complete-typed-src-"));
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-typed-ir-"));
  const baselineRoot = await mkdtemp(
    join(tmpdir(), "easy-complete-typed-baseline-"),
  );
  try {
    await writeFile(
      join(sourceRoot, "factory.js"),
      `export default {
         name: "factory",
         args: { generators: {
           trigger(search, previous) { return search.includes(previous); }
         } }
       };\n`,
    );
    await compileSpecsIr({ srcDir: sourceRoot, outDir: irRoot });
    await run({
      sourceRoot,
      irRoot,
      baselinePath: join(baselineRoot, "reference.json"),
    });
  } finally {
    await Promise.all([
      rm(sourceRoot, { recursive: true, force: true }),
      rm(irRoot, { recursive: true, force: true }),
      rm(baselineRoot, { recursive: true, force: true }),
    ]);
  }
}

test("reference baseline covers every audited typed hook and corpus case", async () => {
  await withTypedFixture(async ({ sourceRoot, irRoot }) => {
    const baseline = await buildTypedTriggerReference({
      sourceRoot,
      irRoot,
    });
    const sidecar = JSON.parse(
      await readFile(join(irRoot, "typed-hooks.json"), "utf8"),
    );
    assert.deepEqual(
      Object.keys(baseline.catalog.hooks),
      Object.keys(sidecar.hooks),
    );
    assert.deepEqual(
      Object.keys(baseline.expected),
      Object.keys(sidecar.hooks),
    );
    assert.equal(baseline.cases.length, INPUT_CORPUS.length);
    assert.ok(
      Object.values(baseline.expected).every(
        (values) => values.length === baseline.cases.length,
      ),
    );
  });
});

test("typed reference APIs reject caller-supplied audit reports", async () => {
  await withTypedFixture(async ({ sourceRoot, irRoot }) => {
    const apis = [
      buildTypedTriggerReference,
      checkTypedTriggerReference,
      updateTypedTriggerReference,
    ];
    for (const api of apis) {
      for (const field of ["audit", "auditPath", "probe", "sourceProbe", "moduleProbe", "unexpected"]) {
        await assert.rejects(
          api({ sourceRoot, irRoot, [field]: true }),
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
});

test("reference corpus uses the Rust-safe id and UTF-16 string contract", async () => {
  const invalidIds = [
    "case/name",
    "case\\name",
    `case${String.fromCharCode(0x00)}`,
    `case${String.fromCharCode(0x7f)}`,
    `case${String.fromCharCode(0x80)}`,
    `case${String.fromCharCode(0x9f)}`,
    `case${String.fromCharCode(0xd800)}`,
    `case${String.fromCharCode(0xdc00)}`,
  ];
  for (const id of invalidIds) {
    await assert.rejects(
      buildTypedTriggerReference({
        cases: [{ id, args: ["", ""] }],
      }),
      /unique safe strings/,
    );
  }

  for (const value of [String.fromCharCode(0xd800), String.fromCharCode(0xdc00)]) {
    await assert.rejects(
      buildTypedTriggerReference({
        cases: [{ id: "safe", args: [value, ""] }],
      }),
      /two string arguments/,
    );
  }

  // Valid supplementary and composed/decomposed Unicode remains accepted by
  // the corpus validator; this does not execute the real CLI.
  await withTypedFixture(async ({ sourceRoot, irRoot }) => {
    const result = await buildTypedTriggerReference({
      sourceRoot,
      irRoot,
      cases: [
        { id: "emoji", args: ["😀/", "é/"] },
        { id: "composed", args: ["café/", "café/"] },
      ],
    });
    assert.equal(result.cases.length, 2);
  });
});

test("update is explicit and check rejects a tampered or stale baseline", async () => {
  await withTypedFixture(async ({ sourceRoot, irRoot, baselinePath }) => {
    await assert.rejects(
      checkTypedTriggerReference({ sourceRoot, irRoot, baselinePath }),
      /ENOENT/,
    );
    const updated = await updateTypedTriggerReference({
      sourceRoot,
      irRoot,
      baselinePath,
    });
    assert.equal(updated.hooks, 1);
    assert.equal(updated.cases, INPUT_CORPUS.length);
    await checkTypedTriggerReference({ sourceRoot, irRoot, baselinePath });

    const original = await readFile(baselinePath, "utf8");
    const tampered = JSON.parse(original);
    tampered.expected[Object.keys(tampered.expected)[0]][0] =
      !tampered.expected[Object.keys(tampered.expected)[0]][0];
    await writeFile(baselinePath, `${JSON.stringify(tampered)}\n`);
    await assert.rejects(
      checkTypedTriggerReference({ sourceRoot, irRoot, baselinePath }),
      /stale or differs/,
    );
    await writeFile(baselinePath, original);

    const extra = JSON.parse(original);
    extra.unexpected = true;
    await writeFile(baselinePath, `${JSON.stringify(extra)}\n`);
    await assert.rejects(
      checkTypedTriggerReference({ sourceRoot, irRoot, baselinePath }),
      /contains unknown field unexpected/,
    );
  });
});

test("source and closure-module changes fail the strict reference check", async () => {
  await withTypedFixture(async ({ sourceRoot, irRoot, baselinePath }) => {
    await updateTypedTriggerReference({ sourceRoot, irRoot, baselinePath });
    const sourcePath = join(sourceRoot, "factory.js");
    const source = await readFile(sourcePath, "utf8");
    await writeFile(sourcePath, `${source}// source drift\n`);
    await assert.rejects(
      checkTypedTriggerReference({ sourceRoot, irRoot, baselinePath }),
      /strict source\/IR audit failed|source tree does not match \.spec-pair\.json/,
    );
    await writeFile(sourcePath, source);

    const manifest = JSON.parse(
      await readFile(join(irRoot, "hook-modules.json"), "utf8"),
    );
    const module = Object.values(manifest.hooks)[0].module;
    const modulePath = join(irRoot, "source-modules", module);
    const moduleSource = await readFile(modulePath, "utf8");
    const moduleInfo = await lstat(modulePath);
    assert.equal(moduleInfo.isFile(), true);
    await writeFile(modulePath, `${moduleSource}// module drift\n`);
    await assert.rejects(
      checkTypedTriggerReference({ sourceRoot, irRoot, baselinePath }),
      /strict source\/IR audit failed|IR tree does not match \.spec-pair\.json/,
    );
    await writeFile(modulePath, moduleSource);
    await checkTypedTriggerReference({ sourceRoot, irRoot, baselinePath });
  });
});

test("asdf getQueryTerm research baseline covers exactly two source/closure candidates", async () => {
  const baseline = await buildTypedGetQueryTermReference();
  assert.deepEqual(Object.keys(baseline.candidates), ASDF_GET_QUERY_TERM_CANDIDATE_IDS);
  assert.deepEqual(Object.keys(baseline.expected), ASDF_GET_QUERY_TERM_CANDIDATE_IDS);
  assert.equal(baseline.cases.length, GET_QUERY_TERM_INPUT_CORPUS.length);
  assert.deepEqual(
    baseline.expected[ASDF_GET_QUERY_TERM_CANDIDATE_IDS[0]],
    baseline.expected[ASDF_GET_QUERY_TERM_CANDIDATE_IDS[1]],
  );
  for (const candidate of Object.values(baseline.candidates)) {
    assert.equal(candidate.sourceField, "getQueryTerm");
    assert.equal(candidate.descriptor.sourceField, "getQueryTerm");
    assert.equal(candidate.descriptor.expr.then.op, "string-slice-after-first");
  }
});
