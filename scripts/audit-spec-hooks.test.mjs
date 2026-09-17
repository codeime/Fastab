import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import {
  link,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { promisify } from "node:util";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

import {
  compileSpecsIr,
  HOOK_MODULE_MANIFEST,
  HOOK_MODULES_DIR,
  hookFileName,
  TYPED_HOOK_SIDECAR,
} from "./compile-spec-ir.mjs";
import { auditSpecsHooks, safeWriteAuditReport } from "./audit-spec-hooks.mjs";
import { functionSource } from "./filepaths-helper.mjs";
import { createPairMarker, writePairMarker } from "./spec-pair.mjs";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const execFileAsync = promisify(execFile);

async function createFixture() {
  const sourceRoot = await mkdtemp(
    join(tmpdir(), "easy-complete-hook-audit-src-"),
  );
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-hook-audit-ir-"));
  const source = `export default {
    name: "sample",
    args: [{ generators: { postProcess: (stdout) => [{ name: stdout }] } }],
  };\n`;
  const id = "sample#postProcess#1";
  const moduleFile = "0123456789abcdef01234567.js";
  const moduleText = `export default (() => {
    const suffix = "-closure";
    return Object.freeze({
      ${JSON.stringify(id)}: (stdout) => [{ name: stdout + suffix }],
    });
  })();\n`;

  await writeFile(join(sourceRoot, "sample.js"), source);
  const imported = await import(
    `${pathToFileURL(join(sourceRoot, "sample.js")).href}?fixture=${Date.now()}`
  );
  const hookBody = functionSource(
    imported.default.args[0].generators.postProcess,
  );
  assert.equal(typeof hookBody, "string");
  await mkdir(join(irRoot, "hooks"));
  await mkdir(join(irRoot, HOOK_MODULES_DIR));
  await writeFile(
    join(irRoot, "sample.json"),
    `${JSON.stringify({
      name: "sample",
      args: [{ generators: { jsPostProcess: id } }],
    })}\n`,
  );
  await writeFile(
    join(irRoot, "hooks", hookFileName(id)),
    `export default ${hookBody};\n`,
  );
  await writeFile(join(irRoot, HOOK_MODULES_DIR, moduleFile), moduleText);
  const moduleSha256 = sha256(moduleText);
  const manifest = {
    version: 1,
    kind: "closure-preserving-hook-modules",
    hooks: {
      [id]: {
        module: moduleFile,
        moduleSha256,
        path: "root.args[0].generators.postProcess",
        sourceField: "postProcess",
        functionBodySha256: sha256(hookBody),
      },
    },
    modules: {
      [moduleFile]: {
        source: "sample.js",
        sourceSha256: sha256(source),
        moduleSha256,
        hookIds: [id],
      },
    },
  };
  const manifestPath = join(irRoot, HOOK_MODULE_MANIFEST);
  await writeFile(manifestPath, `${JSON.stringify(manifest)}\n`);
  await writeFile(
    join(irRoot, TYPED_HOOK_SIDECAR),
    `${JSON.stringify({
      version: 1,
      kind: "typed-hook-expressions",
      contracts: {
        trigger: {
          irVersion: 1,
          params: ["string", "string"],
          resultType: "bool",
        },
      },
      hooks: {},
    })}\n`,
  );
  await writePairMarker(irRoot, await createPairMarker({ sourceRoot, irRoot }));
  return {
    sourceRoot,
    irRoot,
    manifestPath,
    modulePath: join(irRoot, HOOK_MODULES_DIR, moduleFile),
    id,
    moduleFile,
    hookBody,
    async readManifest() {
      return JSON.parse(await readFile(manifestPath, "utf8"));
    },
    async writeManifest(next) {
      await writeFile(manifestPath, `${JSON.stringify(next)}\n`);
    },
    async cleanup() {
      await Promise.all([
        rm(sourceRoot, { recursive: true, force: true }),
        rm(irRoot, { recursive: true, force: true }),
      ]);
    },
  };
}

async function audit(fixture, options = {}) {
  return auditSpecsHooks({
    sourceRoot: fixture.sourceRoot,
    irRoot: fixture.irRoot,
    ...options,
  });
}

async function createTypedCompiledFixture() {
  const sourceRoot = await mkdtemp(
    join(tmpdir(), "easy-complete-typed-audit-src-"),
  );
  const irRoot = await mkdtemp(join(tmpdir(), "easy-complete-typed-audit-ir-"));
  await writeFile(
    join(sourceRoot, "typed.js"),
    `const legacy = (value) => external(value);
      export default {
        name: "typed",
        args: [{ generators: [
          { trigger: (before, after) => before.length !== after.length },
          { trigger: legacy },
        ] }],
      };\n`,
  );
  await compileSpecsIr({ srcDir: sourceRoot, outDir: irRoot });
  const sidecarPath = join(irRoot, TYPED_HOOK_SIDECAR);
  return {
    sourceRoot,
    irRoot,
    sidecarPath,
    async readSidecar() {
      return JSON.parse(await readFile(sidecarPath, "utf8"));
    },
    async writeSidecar(value) {
      await writeFile(sidecarPath, `${JSON.stringify(value)}\n`);
    },
    async cleanup() {
      await Promise.all([
        rm(sourceRoot, { recursive: true, force: true }),
        rm(irRoot, { recursive: true, force: true }),
      ]);
    },
  };
}

test("audit validates closure-preserving hook module manifest v1", async () => {
  const fixture = await createFixture();
  try {
    const report = await audit(fixture);
    assert.equal(report.ok, true);
    assert.deepEqual(report.hookModules, {
      validated: true,
      manifest: HOOK_MODULE_MANIFEST,
      directory: HOOK_MODULES_DIR,
      manifestHooks: 1,
      manifestModules: 1,
      filesOnDisk: 1,
      manifestSha256: sha256(await readFile(fixture.manifestPath, "utf8")),
    });
    assert.deepEqual(report.sourceToIr[0].hookInstances.postProcess, [
      {
        path: "root.args[0].generators.postProcess",
        sha256: sha256(fixture.hookBody),
        functionBodySha256: sha256(fixture.hookBody),
        sourceField: "postProcess",
        id: fixture.id,
        file: hookFileName(fixture.id),
      },
    ]);
  } finally {
    await fixture.cleanup();
  }
});

test("public audit rejects caller-supplied lockHeld instead of trusting it", async () => {
  const fixture = await createFixture();
  try {
    await assert.rejects(
      audit(fixture, { pair: "skip", lockHeld: true }),
      /auditSpecsHooks contains unknown field lockHeld/,
    );
  } finally {
    await fixture.cleanup();
  }
});

test("audit output rejects redirects and special targets without touching sentinels", async () => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-audit-output-"));
  const outsideSentinel = join(root, "..", "audit-output-hardlink-sentinel");
  const report = { ok: true, marker: "fresh" };
  try {
    const existing = join(root, "existing.json");
    await writeFile(existing, "sentinel\n");
    await safeWriteAuditReport(existing, report, { root });
    assert.deepEqual(JSON.parse(await readFile(existing, "utf8")), report);

    const missing = join(root, "missing.json");
    await safeWriteAuditReport(missing, report, { root });
    assert.deepEqual(JSON.parse(await readFile(missing, "utf8")), report);

    const sentinel = join(root, "sentinel.txt");
    const target = join(root, "redirect.json");
    await writeFile(sentinel, "must-survive\n");
    await symlink(sentinel, target);
    await assert.rejects(
      safeWriteAuditReport(target, report, { root }),
      /symbolic link/,
    );
    assert.equal(await readFile(sentinel, "utf8"), "must-survive\n");

    const realParent = join(root, "real-parent");
    const linkedParent = join(root, "linked-parent");
    await mkdir(realParent);
    await symlink(realParent, linkedParent);
    const linkedTarget = join(linkedParent, "report.json");
    await assert.rejects(
      safeWriteAuditReport(linkedTarget, report, { root }),
      /symbolic-link ancestor/,
    );
    await assert.rejects(
      safeWriteAuditReport(join(root, "..", "outside.json"), report, { root }),
      /escapes the repository root/,
    );

    const fifo = join(root, "report.fifo");
    await execFileAsync("mkfifo", [fifo]);
    await assert.rejects(
      safeWriteAuditReport(fifo, report, { root }),
      /special entry/,
    );
    assert.equal((await lstat(fifo)).isFIFO(), true);

    const hardlink = join(root, "hardlink.json");
    await writeFile(outsideSentinel, "must-survive-hardlink\n");
    await link(outsideSentinel, hardlink);
    await assert.rejects(
      safeWriteAuditReport(hardlink, report, { root }),
      /must not be a hard link/,
    );
    assert.equal(await readFile(outsideSentinel, "utf8"), "must-survive-hardlink\n");
    await rm(outsideSentinel, { force: true });
  } finally {
    await Promise.all([
      rm(root, { recursive: true, force: true }),
      rm(outsideSentinel, { force: true }),
    ]);
  }
});

test("audit rejects a raw POSIX backslash name before normalization", async () => {
  const fixture = await createFixture();
  const marker = join(fixture.sourceRoot, "..", "audit-backslash-marker");
  try {
    await writeFile(
      join(fixture.sourceRoot, "..\\escape.js"),
      `import { writeFileSync } from "node:fs";
writeFileSync(${JSON.stringify(marker)}, "executed");
export default { name: "escape", args: [] };
`,
    );
    const report = await audit(fixture, { pair: "skip" });
    assert.equal(report.ok, false);
    assert.ok(
      report.errors.sourceReadErrors.some(({ message }) =>
        message.includes("literal backslash before normalization"),
      ),
    );
    await assert.rejects(lstat(marker), { code: "ENOENT" });
  } finally {
    await fixture.cleanup();
  }
});

test("audit fails explicitly on symlink and FIFO tree entries", async () => {
  for (const kind of ["symlink", "fifo"]) {
    const fixture = await createFixture();
    const outside = join(fixture.sourceRoot, "..", `audit-${kind}-outside.js`);
    const entry = join(fixture.sourceRoot, `entry-${kind}.js`);
    try {
      if (kind === "symlink") {
        await writeFile(outside, "export default { name: \"outside\" };\n");
        await symlink(outside, entry);
      } else {
        await execFileAsync("mkfifo", [entry]);
      }
      const report = await audit(fixture, { pair: "skip" });
      assert.equal(report.ok, false);
      assert.ok(
        report.errors.sourceReadErrors.some(({ message }) =>
          message.includes(kind === "symlink" ? "symbolic link" : "special entry"),
        ),
      );
    } finally {
      await fixture.cleanup();
      await rm(outside, { force: true });
    }
  }
});

test("audit recomputes the typed trigger set and rejects sidecar drift", async () => {
  const fixture = await createTypedCompiledFixture();
  try {
    const baseline = await fixture.readSidecar();
    const baselineText = await readFile(fixture.sidecarPath, "utf8");
    const healthy = await audit(fixture);
    assert.equal(healthy.ok, true, JSON.stringify(healthy.errors, null, 2));
    assert.deepEqual(
      {
        hooks: healthy.typedHooks.hooks,
        eligibleHooks: healthy.typedHooks.eligibleHooks,
        unsupportedHooks: healthy.typedHooks.unsupportedHooks,
      },
      { hooks: 1, eligibleHooks: 1, unsupportedHooks: 1 },
    );

    for (const nonCanonical of [
      baselineText.replace('"version":1', '"version":1e0'),
      baselineText.replace('"version":1', '"version":1.0'),
    ]) {
      assert.notEqual(nonCanonical, baselineText);
      await writeFile(fixture.sidecarPath, nonCanonical);
      const lexicalReport = await audit(fixture, { pair: "skip" });
      assert.equal(lexicalReport.ok, false);
      assert.ok(
        lexicalReport.errors.invalidTypedHookSidecar.some(({ reason }) =>
          reason.includes("canonical compact JSON"),
        ),
      );
    }

    const invalidUtf8 = Buffer.from(baselineText);
    const kindValueOffset = invalidUtf8.indexOf(
      Buffer.from("typed-hook-expressions"),
    );
    assert.notEqual(kindValueOffset, -1);
    invalidUtf8[kindValueOffset] = 0x80;
    await writeFile(fixture.sidecarPath, invalidUtf8);
    const invalidUtf8Report = await audit(fixture, { pair: "skip" });
    assert.equal(invalidUtf8Report.ok, false);
    assert.ok(
      invalidUtf8Report.errors.invalidTypedHookSidecar.some(({ reason }) =>
        reason.includes("not valid UTF-8"),
      ),
    );

    await writeFile(fixture.sidecarPath, baselineText);

    await rm(fixture.sidecarPath);
    const missing = await audit(fixture, { pair: "skip" });
    assert.equal(missing.ok, false);
    assert.equal(missing.errors.missingTypedHookSidecar.length, 1);
    await fixture.writeSidecar(baseline);

    const orphan = structuredClone(baseline);
    orphan.hooks["typed#trigger#orphan"] =
      orphan.hooks[Object.keys(orphan.hooks)[0]];
    await fixture.writeSidecar(orphan);
    const orphanReport = await audit(fixture, { pair: "skip" });
    assert.equal(orphanReport.ok, false);
    assert.ok(
      orphanReport.errors.orphanTypedHooks.some(
        ({ id }) => id === "typed#trigger#orphan",
      ),
    );
    await fixture.writeSidecar(baseline);

    const provenance = structuredClone(baseline);
    const [typedId] = Object.keys(provenance.hooks);
    provenance.hooks[typedId].path = "root.args[0].generators[1].trigger";
    await fixture.writeSidecar(provenance);
    const provenanceReport = await audit(fixture, { pair: "skip" });
    assert.equal(provenanceReport.ok, false);
    assert.ok(
      provenanceReport.errors.typedHookMismatches.some(
        ({ id }) => id === typedId,
      ),
    );
    await fixture.writeSidecar(baseline);

    const tamperedDescriptor = structuredClone(baseline);
    tamperedDescriptor.hooks[typedId].descriptor.expr = {
      op: "bool",
      value: false,
    };
    await fixture.writeSidecar(tamperedDescriptor);
    const tamperedReport = await audit(fixture, { pair: "skip" });
    assert.equal(tamperedReport.ok, false);
    assert.ok(
      tamperedReport.errors.typedHookMismatches.some(
        ({ id }) => id === typedId,
      ),
    );

    const invalidShape = structuredClone(baseline);
    invalidShape.hooks[typedId].module = ".js";
    invalidShape.hooks[typedId].path = "not-a-source-path";
    invalidShape.hooks[typedId].unexpected = true;
    invalidShape.hooks[""] = structuredClone(invalidShape.hooks[typedId]);
    await fixture.writeSidecar(invalidShape);
    const invalidShapeReport = await audit(fixture, { pair: "skip" });
    assert.equal(invalidShapeReport.ok, false);
    assert.ok(
      invalidShapeReport.errors.invalidTypedHookSidecar.some(
        ({ field }) => field === `hooks.${typedId}.module`,
      ),
    );
    assert.ok(
      invalidShapeReport.errors.invalidTypedHookSidecar.some(
        ({ field }) => field === `hooks.${typedId}.path`,
      ),
    );
    assert.ok(
      invalidShapeReport.errors.invalidTypedHookSidecar.some(
        ({ field }) => field === `typed hooks.${typedId}.unexpected`,
      ),
    );
    assert.equal(
      invalidShapeReport.errors.invalidHookModuleManifest.some(
        ({ field }) => field === `typed hooks.${typedId}.unexpected`,
      ),
      false,
    );
    assert.ok(
      invalidShapeReport.errors.invalidTypedHookSidecar.some(
        ({ field }) => field === "hooks.",
      ),
    );

    const invalidModule = structuredClone(baseline);
    invalidModule.hooks[typedId].module = "..js";
    await fixture.writeSidecar(invalidModule);
    const invalidModuleReport = await audit(fixture, { pair: "skip" });
    assert.equal(invalidModuleReport.ok, false);
    assert.ok(
      invalidModuleReport.errors.invalidTypedHookSidecar.some(
        ({ field }) => field === `hooks.${typedId}.module`,
      ),
    );

    const unknownContract = structuredClone(baseline);
    unknownContract.contracts.extra = true;
    await fixture.writeSidecar(unknownContract);
    const unknownContractReport = await audit(fixture, { pair: "skip" });
    assert.equal(unknownContractReport.ok, false);
    assert.ok(
      unknownContractReport.errors.invalidTypedHookSidecar.some(
        ({ field }) => field === "typed hook sidecar.contracts.extra",
      ),
    );
    assert.equal(
      unknownContractReport.errors.invalidHookModuleManifest.some(
        ({ field }) => field === "typed hook sidecar.contracts.extra",
      ),
      false,
    );
  } finally {
    await fixture.cleanup();
  }
});

test("audit has an explicit pre-manifest opt-out for compiler staging", async () => {
  const fixture = await createFixture();
  try {
    await rm(fixture.manifestPath);
    const report = await audit(fixture, {
      validateHookModules: false,
      validateTypedHooks: false,
      pair: "skip",
    });
    assert.equal(report.ok, true);
    assert.equal(report.hookModules.validated, false);
    assert.equal(report.errors.missingHookModuleManifest.length, 0);
  } finally {
    await fixture.cleanup();
  }
});

test("audit fails when the module manifest is missing or a module is tampered", async () => {
  const fixture = await createFixture();
  try {
    await rm(fixture.manifestPath);
    const missing = await audit(fixture);
    assert.equal(missing.ok, false);
    assert.ok(missing.errors.missingHookModuleManifest.length > 0);

    const replacement = await createFixture();
    try {
      await writeFile(replacement.modulePath, "tampered module\n");
      const tampered = await audit(replacement);
      assert.equal(tampered.ok, false);
      assert.ok(
        tampered.errors.hookModuleMismatches.some(
          (entry) =>
            entry.file === replacement.moduleFile &&
            entry.reason.includes("SHA-256"),
        ),
      );
    } finally {
      await replacement.cleanup();
    }
  } finally {
    await fixture.cleanup();
  }
});

test("audit requires exact hook-to-module mappings and rejects orphan files", async () => {
  const fixture = await createFixture();
  try {
    const manifest = await fixture.readManifest();
    const hookDescriptor = manifest.hooks[fixture.id];
    delete manifest.hooks[fixture.id];
    await writeFile(
      join(fixture.irRoot, HOOK_MODULES_DIR, "orphan.js"),
      "export default (() => ({}))();\n",
    );
    await fixture.writeManifest(manifest);
    const report = await audit(fixture);
    assert.equal(report.ok, false);
    assert.ok(
      report.errors.hookModuleMismatches.some((entry) =>
        entry.reason.includes("missing from manifest hooks"),
      ),
    );
    assert.ok(
      report.errors.orphanHookModules.some(
        (entry) => entry.file === "orphan.js",
      ),
    );

    const missingMetadata = await fixture.readManifest();
    missingMetadata.hooks[fixture.id] = hookDescriptor;
    delete missingMetadata.modules[fixture.moduleFile];
    await fixture.writeManifest(missingMetadata);
    const reportWithoutMetadata = await audit(fixture);
    assert.equal(reportWithoutMetadata.ok, false);
    assert.ok(
      reportWithoutMetadata.errors.hookModuleMismatches.some((entry) =>
        entry.reason.includes("missing from manifest modules"),
      ),
    );
  } finally {
    await fixture.cleanup();
  }
});

test("audit rejects module path traversal and metadata hook-id drift", async () => {
  const fixture = await createFixture();
  try {
    const manifest = await fixture.readManifest();
    manifest.hooks[fixture.id].module = "../outside.js";
    await fixture.writeManifest(manifest);
    const traversal = await audit(fixture);
    assert.equal(traversal.ok, false);
    assert.ok(
      traversal.errors.invalidHookModuleManifest.some((entry) =>
        entry.reason.includes("root-relative .js filename"),
      ),
    );

    manifest.hooks[fixture.id].module = fixture.moduleFile;
    manifest.modules[fixture.moduleFile].hookIds = [];
    await fixture.writeManifest(manifest);
    const report = await audit(fixture);
    assert.equal(report.ok, false);
    assert.ok(
      report.errors.hookModuleMismatches.some((entry) =>
        entry.reason.includes("hookIds do not match"),
      ),
    );
  } finally {
    await fixture.cleanup();
  }
});

test("audit rejects compiler provenance tampering in path, field, or body hash", async () => {
  const fixture = await createFixture();
  try {
    const baseline = await fixture.readManifest();
    const tampering = [
      {
        name: "path",
        mutate: (manifest) => {
          manifest.hooks[fixture.id].path =
            "root.args[1].generators.postProcess";
        },
        reason: "path does not resolve",
      },
      {
        name: "sourceField",
        mutate: (manifest) => {
          manifest.hooks[fixture.id].sourceField = "custom";
        },
        reason: "sourceField does not match",
      },
      {
        name: "functionBodySha256",
        mutate: (manifest) => {
          manifest.hooks[fixture.id].functionBodySha256 = "0".repeat(64);
        },
        reason: "function body SHA-256",
      },
    ];
    for (const { mutate, reason } of tampering) {
      const manifest = structuredClone(baseline);
      mutate(manifest);
      await fixture.writeManifest(manifest);
      const report = await audit(fixture);
      assert.equal(report.ok, false);
      assert.ok(
        [
          ...report.errors.hookModuleMismatches,
          ...report.errors.sourceHookMismatches,
        ].some(
          (entry) => entry.id === fixture.id && entry.reason.includes(reason),
        ),
      );
    }
  } finally {
    await fixture.cleanup();
  }
});
