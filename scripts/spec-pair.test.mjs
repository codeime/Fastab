import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import {
  cp,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  PAIR_MARKER_NAME,
  acquirePairLock,
  comparePath,
  createPairMarker,
  digestTree,
  pairJournalExists,
  parsePairMarker,
  publishPairDirectories,
  readPairJournal,
  verifyPair,
  verifyPairLockProof,
  withPairLock,
  writePairMarker,
} from "./spec-pair.mjs";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");

async function makePair() {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-spec-pair-"));
  const sourceRoot = join(root, "source");
  const irRoot = join(root, "ir");
  await mkdir(sourceRoot, { recursive: true });
  await mkdir(irRoot, { recursive: true });
  await writeFile(join(sourceRoot, "z.js"), "export default { name: 'z' };\n");
  await writeFile(join(sourceRoot, "a.js"), "export default { name: 'a' };\n");
  await writeFile(join(sourceRoot, ".source-manifest.json"), '{"format":1}\n');
  await writeFile(join(irRoot, "z.json"), '{"names":["z"]}\n');
  await writeFile(join(irRoot, "a.json"), '{"names":["a"]}\n');
  const marker = await createPairMarker({ sourceRoot, irRoot });
  await writePairMarker(irRoot, marker);
  return {
    root,
    sourceRoot,
    irRoot,
    marker,
    async cleanup() {
      await rm(root, { recursive: true, force: true });
    },
  };
}

async function makePublicationFixture({
  oldPair = true,
  markerlessOldIr = false,
  sameSource = false,
  parent = tmpdir(),
  oldState = oldPair ? "pair" : "none",
} = {}) {
  const root = await mkdtemp(join(parent, "easy-complete-spec-journal-"));
  const sourceRoot = join(root, "custom-source");
  const irRoot = join(root, "custom-ir");
  const sourceStage = join(root, "custom-stage-source");
  const irStage = join(root, "custom-stage-ir");
  const lockPath = join(root, ".spec-pair.lock");
  await mkdir(sourceStage, { recursive: true });
  await mkdir(irStage, { recursive: true });
  if (oldState === "pair" || oldState === "source-only") {
    await mkdir(sourceRoot, { recursive: true });
    await writeFile(join(sourceRoot, "command.js"), "old source\n");
  }
  if (oldState === "pair") {
    await mkdir(irRoot, { recursive: true });
    await writeFile(join(irRoot, "command.json"), "old ir\n");
    if (!markerlessOldIr) {
      const oldMarker = await createPairMarker({ sourceRoot, irRoot });
      await writePairMarker(irRoot, oldMarker);
    }
  }
  await writeFile(
    join(sourceStage, "command.js"),
    sameSource ? "old source\n" : "new source\n",
  );
  await writeFile(join(irStage, "command.json"), "new ir\n");
  const newMarker = await createPairMarker({
    sourceRoot: sourceStage,
    irRoot: irStage,
  });
  await writePairMarker(irStage, newMarker);
  await writeFile(join(root, "FOREIGN_SENTINEL"), "must survive\n");
  return {
    root,
    sourceRoot,
    irRoot,
    sourceStage,
    irStage,
    lockPath,
    async cleanup() {
      await rm(root, { recursive: true, force: true });
    },
  };
}

function runPublicationChild(
  fixture,
  failpoint,
  { irOnly = false, withAudit = false } = {},
) {
  const moduleUrl = new URL("./spec-pair.mjs", import.meta.url).href;
  const auditOption = withAudit ? ",verifyPublished:async()=>{}" : "";
  const call = irOnly
    ? `await publishPairDirectories({irStage:${JSON.stringify(
        fixture.irStage,
      )},irCanonical:${JSON.stringify(fixture.irRoot)},lockPath:${JSON.stringify(
        fixture.lockPath,
      )}${auditOption}});`
    : `await publishPairDirectories({sourceStage:${JSON.stringify(
        fixture.sourceStage,
      )},irStage:${JSON.stringify(fixture.irStage)},sourceCanonical:${JSON.stringify(
        fixture.sourceRoot,
      )},irCanonical:${JSON.stringify(fixture.irRoot)},lockPath:${JSON.stringify(
        fixture.lockPath,
      )}${auditOption}});`;
  const source = `import { publishPairDirectories, withPairLock } from ${JSON.stringify(
    moduleUrl,
  )}; await withPairLock(${JSON.stringify(fixture.lockPath)}, async () => { ${call} });`;
  try {
    execFileSync(process.execPath, ["--input-type=module", "-e", source], {
      env: {
        ...process.env,
        EC_TEST_SPECS_PUBLISH_FAILPOINT: failpoint,
      },
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    return { status: 0, output: "" };
  } catch (error) {
    return {
      status: error.status,
      output: `${error.stdout ?? ""}${error.stderr ?? ""}`,
    };
  }
}

test("journal recovers every pair rename, verify, and cleanup failpoint", async (t) => {
  const failpoints = [
    "source-backup",
    "ir-backup",
    "source-publish",
    "ir-publish",
    "verify",
    "cleanup",
  ];
  for (const failpoint of failpoints) {
    const fixture = await makePublicationFixture();
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, failpoint);
    assert.notEqual(crashed.status, 0, `${failpoint}: ${crashed.output}`);
    const journal = await readPairJournal(fixture.lockPath);
    assert.ok(journal, `${failpoint}: missing journal; ${crashed.output}`);
    assert.equal(journal.newPairSha256.length, 64, failpoint);
    assert.equal(journal.canonical.source.path, fixture.sourceRoot, failpoint);
    assert.equal(journal.stage.ir.path, fixture.irStage, failpoint);
    await withPairLock(fixture.lockPath, async () => {});
    const expectedPair =
      failpoint === "source-backup" ||
      failpoint === "ir-backup" ||
      failpoint === "source-publish" ||
      failpoint === "cleanup"
        ? journal.newPairSha256
        : journal.oldPairSha256;
    assert.equal(
      (await verifyPair({
        sourceRoot: fixture.sourceRoot,
        irRoot: fixture.irRoot,
      })).pairSha256,
      expectedPair,
      failpoint,
    );
    assert.equal(await pairJournalExists(fixture.lockPath), false, failpoint);
    assert.equal(await readFile(join(fixture.root, "FOREIGN_SENTINEL"), "utf8"), "must survive\n");
    // A second acquisition is a no-op and must not rediscover or delete any
    // unrelated file.
    await withPairLock(fixture.lockPath, async () => {});
  }
});

test("journal resumes a first publication after only the source side was published", async (t) => {
  const fixture = await makePublicationFixture({ oldPair: false });
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "source-publish");
  assert.notEqual(crashed.status, 0, crashed.output);
  await withPairLock(fixture.lockPath, async () => {});
  await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("a first publication resumes only after the supplied final audit", async (t) => {
  const fixture = await makePublicationFixture({ oldPair: false });
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "source-publish", {
    withAudit: true,
  });
  assert.notEqual(crashed.status, 0, crashed.output);
  let audited = false;
  await withPairLock(
    fixture.lockPath,
    async () => {},
    {
      verifyPublished: async () => {
        audited = true;
      },
    },
  );
  assert.equal(audited, true);
  await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("a final publication audit failure hides the new pair and preserves retry state", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  await assert.rejects(
    withPairLock(fixture.lockPath, () =>
      publishPairDirectories({
        sourceStage: fixture.sourceStage,
        irStage: fixture.irStage,
        sourceCanonical: fixture.sourceRoot,
        irCanonical: fixture.irRoot,
        lockPath: fixture.lockPath,
        verifyPublished: async () => {
          throw new Error("final audit rejected published pair");
        },
      }),
    ),
    /final audit rejected published pair/,
  );
  const oldMarker = await verifyPair({
    sourceRoot: fixture.sourceRoot,
    irRoot: fixture.irRoot,
  });
  assert.notEqual(oldMarker.pairSha256, "");
  assert.equal(
    await readFile(join(fixture.sourceRoot, "command.js"), "utf8"),
    "old source\n",
  );
  assert.equal(
    await readFile(join(fixture.irRoot, "command.json"), "utf8"),
    "old ir\n",
  );
  const journal = await readPairJournal(fixture.lockPath);
  assert.equal(journal.phase, "rolled-back");
  assert.equal(await pairJournalExists(fixture.lockPath), true);
  let audited = false;
  await withPairLock(
    fixture.lockPath,
    async () => {},
    { verifyPublished: async () => { audited = true; } },
  );
  assert.equal(audited, true);
  await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("a pre-verification crash never commits a complete new pair", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  assert.equal(journal.phase, "ir-published");
  await withPairLock(fixture.lockPath, async () => {});
  assert.equal(
    await readFile(join(fixture.sourceRoot, "command.js"), "utf8"),
    "old source\n",
  );
  assert.equal(
    await readFile(join(fixture.irRoot, "command.json"), "utf8"),
    "old ir\n",
  );
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("child crashes before or during the final audit hide the new pair", async (t) => {
  for (const failpoint of ["audit", "verify"]) {
    const fixture = await makePublicationFixture();
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, failpoint, { withAudit: true });
    assert.notEqual(crashed.status, 0, `${failpoint}: ${crashed.output}`);
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /resumed publication requires its final audit/,
      failpoint,
    );
    assert.equal(
      await readFile(join(fixture.sourceRoot, "command.js"), "utf8"),
      "old source\n",
      failpoint,
    );
    assert.equal(
      await readFile(join(fixture.irRoot, "command.json"), "utf8"),
      "old ir\n",
      failpoint,
    );
    assert.equal(await pairJournalExists(fixture.lockPath), true, failpoint);
    let audited = false;
    await withPairLock(
      fixture.lockPath,
      async () => {},
      { verifyPublished: async () => { audited = true; } },
    );
    assert.equal(audited, true, failpoint);
    await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
    assert.equal(await pairJournalExists(fixture.lockPath), false, failpoint);
  }
});

test("a first publication final-audit crash resumes with the supplied verifier", async (t) => {
  for (const failpoint of ["audit", "verify"]) {
    const fixture = await makePublicationFixture({ oldPair: false });
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, failpoint, {
      withAudit: true,
    });
    assert.notEqual(crashed.status, 0, `${failpoint}: ${crashed.output}`);
    let audited = false;
    await withPairLock(
      fixture.lockPath,
      async () => {},
      {
        verifyPublished: async () => {
          audited = true;
        },
      },
    );
    assert.equal(audited, true, failpoint);
    await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
    assert.equal(await pairJournalExists(fixture.lockPath), false, failpoint);
  }
});

test("a compile-only first publication re-verifies a complete pair after a crash", async (t) => {
  const fixture = await makePublicationFixture({ oldPair: false });
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify");
  assert.notEqual(crashed.status, 0, crashed.output);
  await withPairLock(fixture.lockPath, async () => {});
  await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("a first publication crash with a missing final audit stays fail-closed", async (t) => {
  const fixture = await makePublicationFixture({ oldPair: false });
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify", { withAudit: true });
  assert.notEqual(crashed.status, 0, crashed.output);
  await assert.rejects(
    withPairLock(fixture.lockPath, async () => {}),
    /resumed publication requires its final audit/,
  );
  assert.equal(await pairJournalExists(fixture.lockPath), true);
});

test("source-only old state rolls back to a missing IR and retries from stage", async (t) => {
  const fixture = await makePublicationFixture({ oldState: "source-only" });
  t.after(() => fixture.cleanup());
  await assert.rejects(
    withPairLock(fixture.lockPath, () =>
      publishPairDirectories({
        sourceStage: fixture.sourceStage,
        irStage: fixture.irStage,
        sourceCanonical: fixture.sourceRoot,
        irCanonical: fixture.irRoot,
        lockPath: fixture.lockPath,
        verifyPublished: async () => {
          throw new Error("source-only audit rejected");
        },
      }),
    ),
    /source-only audit rejected/,
  );
  assert.equal(await readFile(join(fixture.sourceRoot, "command.js"), "utf8"), "old source\n");
  await assert.rejects(readdir(fixture.irRoot), (error) => error.code === "ENOENT");
  assert.equal((await readPairJournal(fixture.lockPath)).phase, "rolled-back");
  await assert.rejects(
    withPairLock(
      fixture.lockPath,
      async () => {},
      { verifyPublished: async () => { throw new Error("retry audit rejected"); } },
    ),
    /retry audit rejected/,
  );
  assert.equal(await readFile(join(fixture.sourceRoot, "command.js"), "utf8"), "old source\n");
  await assert.rejects(readdir(fixture.irRoot), (error) => error.code === "ENOENT");
  assert.equal((await readPairJournal(fixture.lockPath)).phase, "rolled-back");
  let audited = false;
  await withPairLock(
    fixture.lockPath,
    async () => {},
    { verifyPublished: async () => { audited = true; } },
  );
  assert.equal(audited, true);
  await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("an empty first publication hides unverified canonical trees and retries", async (t) => {
  for (const failpoint of ["audit", "verify"]) {
    const fixture = await makePublicationFixture({ oldState: "none" });
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, failpoint, { withAudit: true });
    assert.notEqual(crashed.status, 0, `${failpoint}: ${crashed.output}`);
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /resumed publication requires its final audit/,
      failpoint,
    );
    await assert.rejects(readdir(fixture.sourceRoot), (error) => error.code === "ENOENT", failpoint);
    await assert.rejects(readdir(fixture.irRoot), (error) => error.code === "ENOENT", failpoint);
    assert.equal((await readPairJournal(fixture.lockPath)).phase, "rolled-back", failpoint);
    let audited = false;
    await withPairLock(
      fixture.lockPath,
      async () => {},
      { verifyPublished: async () => { audited = true; } },
    );
    assert.equal(audited, true, failpoint);
    await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
    assert.equal(await pairJournalExists(fixture.lockPath), false, failpoint);
    assert.equal(await readFile(join(fixture.root, "FOREIGN_SENTINEL"), "utf8"), "must survive\n");
  }
});

test("an unchanged source side remains resumable when only IR changes", async (t) => {
  for (const failpoint of ["source-publish", "audit", "verify"]) {
    const fixture = await makePublicationFixture({ sameSource: true });
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, failpoint, { withAudit: true });
    assert.notEqual(crashed.status, 0, `${failpoint}: ${crashed.output}`);
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /resumed publication requires its final audit/,
      failpoint,
    );
    assert.equal(
      await readFile(join(fixture.sourceRoot, "command.js"), "utf8"),
      "old source\n",
      failpoint,
    );
    assert.equal(
      await readFile(join(fixture.irRoot, "command.json"), "utf8"),
      "old ir\n",
      failpoint,
    );
    const pending = await readPairJournal(fixture.lockPath);
    assert.equal(pending.phase, "rolled-back", failpoint);
    // The unchanged source is safe to keep at canonical while the changed IR
    // is staged; recovery treats this `same` side as usable for both
    // generations, including the source-publish boundary.
    await assert.rejects(
      withPairLock(
        fixture.lockPath,
        async () => {},
        {
          verifyPublished: async () => {
            throw new Error("identical-source audit rejected");
          },
        },
      ),
      /identical-source audit rejected/,
      failpoint,
    );
    assert.equal((await readPairJournal(fixture.lockPath)).phase, "rolled-back", failpoint);
    let audited = false;
    await withPairLock(
      fixture.lockPath,
      async () => {},
      { verifyPublished: async () => { audited = true; } },
    );
    assert.equal(audited, true, failpoint);
    await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
    assert.equal(await pairJournalExists(fixture.lockPath), false, failpoint);
  }
});

test("an unchanged source rolls back and retries after a normal callback failure", async (t) => {
  const fixture = await makePublicationFixture({ sameSource: true });
  t.after(() => fixture.cleanup());
  await assert.rejects(
    withPairLock(fixture.lockPath, () =>
      publishPairDirectories({
        sourceStage: fixture.sourceStage,
        irStage: fixture.irStage,
        sourceCanonical: fixture.sourceRoot,
        irCanonical: fixture.irRoot,
        lockPath: fixture.lockPath,
        verifyPublished: async () => {
          throw new Error("normal identical-source audit rejected");
        },
      }),
    ),
    /normal identical-source audit rejected/,
  );
  assert.equal(
    await readFile(join(fixture.sourceRoot, "command.js"), "utf8"),
    "old source\n",
  );
  assert.equal(
    await readFile(join(fixture.irRoot, "command.json"), "utf8"),
    "old ir\n",
  );
  assert.equal((await readPairJournal(fixture.lockPath)).phase, "rolled-back");

  await withPairLock(
    fixture.lockPath,
    async () => {},
    { verifyPublished: async () => {} },
  );
  await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("a recovery audit failure also restores the old canonical state", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify", { withAudit: true });
  assert.notEqual(crashed.status, 0, crashed.output);
  await assert.rejects(
    withPairLock(
      fixture.lockPath,
      async () => {},
      { verifyPublished: async () => { throw new Error("recovery audit rejected"); } },
    ),
    /recovery audit rejected/,
  );
  assert.equal(
    await readFile(join(fixture.sourceRoot, "command.js"), "utf8"),
    "old source\n",
  );
  assert.equal(
    await readFile(join(fixture.irRoot, "command.json"), "utf8"),
    "old ir\n",
  );
  assert.equal((await readPairJournal(fixture.lockPath)).phase, "rolled-back");
  await withPairLock(
    fixture.lockPath,
    async () => {},
    { verifyPublished: async () => {} },
  );
  await verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot });
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("a final-audit mutation is rejected before verified is journaled", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  await assert.rejects(
    withPairLock(fixture.lockPath, () =>
      publishPairDirectories({
        sourceStage: fixture.sourceStage,
        irStage: fixture.irStage,
        sourceCanonical: fixture.sourceRoot,
        irCanonical: fixture.irRoot,
        lockPath: fixture.lockPath,
        verifyPublished: async () => {
          await writeFile(join(fixture.irRoot, "command.json"), "callback mutation\n");
        },
      }),
    ),
    /failed to publish source\/IR and restore|published pair changed during final audit/,
  );
  assert.equal(await pairJournalExists(fixture.lockPath), true);
  assert.equal(await readFile(join(fixture.root, "FOREIGN_SENTINEL"), "utf8"), "must survive\n");
  await assert.rejects(
    verifyPair({ sourceRoot: fixture.sourceRoot, irRoot: fixture.irRoot }),
  );
});

test("markerless old IR is recoverable and a changed backup is retained", async (t) => {
  const fixture = await makePublicationFixture({ markerlessOldIr: true });
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  const backupIr = journal.backup.ir.path;
  await writeFile(join(backupIr, "command.json"), "tampered old ir\n");
  await assert.rejects(
    withPairLock(fixture.lockPath, async () => {}),
    /changed old ir backup|changed old ir backup file/,
  );
  assert.equal(await pairJournalExists(fixture.lockPath), true);
  assert.equal(await readFile(join(backupIr, "command.json"), "utf8"), "tampered old ir\n");
  // Restore the fixture's exact old bytes and prove the markerless rollback
  // path remains accepted when the backup is not modified.
  await writeFile(join(backupIr, "command.json"), "old ir\n");
  await withPairLock(fixture.lockPath, async () => {});
  assert.equal(await pairJournalExists(fixture.lockPath), false);
});

test("an incomplete stage rolls back to the markerless old IR backup", async (t) => {
  const fixture = await makePublicationFixture({ markerlessOldIr: true });
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "ir-backup", { irOnly: true });
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  await rm(fixture.irStage, { recursive: true, force: true });
  await withPairLock(fixture.lockPath, async () => {});
  assert.equal(await readFile(join(fixture.irRoot, "command.json"), "utf8"), "old ir\n");
  assert.equal(
    await readFile(join(fixture.irRoot, PAIR_MARKER_NAME), "utf8").catch(
      (error) => error.code,
    ),
    "ENOENT",
  );
  assert.equal(await pairJournalExists(fixture.lockPath), false);
  assert.equal(journal.oldPairSha256, null);
});

test("IR-only publication journals survive a crash with a markerless old tree", async (t) => {
  const failpoints = ["ir-backup", "ir-publish", "verify", "cleanup"];
  for (const failpoint of failpoints) {
    const fixture = await makePublicationFixture({ markerlessOldIr: true });
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, failpoint, { irOnly: true });
    assert.notEqual(crashed.status, 0, `${failpoint}: ${crashed.output}`);
    const journal = await readPairJournal(fixture.lockPath);
    assert.equal(journal.operation, "ir", failpoint);
    await withPairLock(fixture.lockPath, async () => {});
    if (failpoint === "ir-backup" || failpoint === "cleanup") {
      const marker = await verifyPair({ irRoot: fixture.irRoot, irOnly: true });
      assert.equal(marker.pairSha256, journal.newPairSha256, failpoint);
    } else {
      assert.equal(
        await readFile(join(fixture.irRoot, "command.json"), "utf8"),
        "old ir\n",
        failpoint,
      );
      assert.equal(
        await readFile(join(fixture.irRoot, PAIR_MARKER_NAME), "utf8").catch(
          (error) => error.code,
        ),
        "ENOENT",
        failpoint,
      );
    }
    assert.equal(await pairJournalExists(fixture.lockPath), false, failpoint);
  }
});

test("no-journal custom orphans are reported and left untouched", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-spec-orphan-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const orphan = join(root, ".custom-output.backup-orphan");
  await mkdir(orphan);
  const moduleUrl = new URL("./spec-pair.mjs", import.meta.url).href;
  const source = `import { withPairLock } from ${JSON.stringify(
    moduleUrl,
  )}; await withPairLock(${JSON.stringify(join(root, ".spec-pair.lock"))}, async () => {});`;
  const result = spawnSync(
    process.execPath,
    ["--input-type=module", "-e", source],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /orphaned specs publication artifact/);
  // The directory itself is the sentinel: no-journal recovery never removes
  // it even though its basename resembles a generated custom backup.
  assert.deepEqual(await readdir(orphan), []);
});

test("malformed journals reject parent-child and cross-side path overlap", async (t) => {
  const mutations = [
    ["stage is the canonical parent", (journal, fixture) => {
      journal.stage.source.path = fixture.root;
    }],
    ["backup is the canonical parent", (journal, fixture) => {
      journal.backup.source.path = fixture.root;
    }],
    ["source and IR paths cross", (journal) => {
      journal.stage.ir.path = journal.canonical.source.path;
    }],
  ];
  for (const [label, mutate] of mutations) {
    const fixture = await makePublicationFixture();
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, "verify");
    assert.notEqual(crashed.status, 0, `${label}: ${crashed.output}`);
    const journalPath = join(fixture.root, ".spec-pair.journal.json");
    const journal = JSON.parse(await readFile(journalPath, "utf8"));
    mutate(journal, fixture);
    await writeFile(journalPath, `${JSON.stringify(journal)}\n`);
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /spec pair journal paths overlap/,
      label,
    );
    assert.equal(await pairJournalExists(fixture.lockPath), true, label);
  }
});

test("direct publication rejects overlapping stage and canonical inputs", async (t) => {
  const fixture = await makePublicationFixture({ oldPair: false });
  t.after(() => fixture.cleanup());
  const cases = [
    ["stage and canonical are identical", fixture.sourceStage],
    ["stage is inside canonical parent", fixture.root],
  ];
  for (const [label, sourceCanonical] of cases) {
    await assert.rejects(
      publishPairDirectories({
        sourceStage: fixture.sourceStage,
        irStage: fixture.irStage,
        sourceCanonical,
        irCanonical: join(
          fixture.root,
          `${label.replaceAll(" ", "-")}-ir`,
        ),
        lockPath: fixture.lockPath,
      }),
      /spec pair journal paths overlap/,
      label,
    );
    assert.equal(await pairJournalExists(fixture.lockPath), false, label);
  }
  assert.equal(
    await readFile(join(fixture.root, "FOREIGN_SENTINEL"), "utf8"),
    "must survive\n",
  );
});

test("direct publication rejects case-folded and /tmp-aliased stage inputs", async (t) => {
  if (process.platform !== "darwin") return;
  const cases = [
    ["case-folded", (fixture) =>
      fixture.sourceStage.replace(/custom-stage-source$/, "CUSTOM-STAGE-SOURCE")],
    ["system alias", (fixture) => {
      const root = fixture.root.startsWith("/private/tmp")
        ? fixture.root.replace(/^\/private\/tmp/, "/tmp")
        : fixture.root.replace(/^\/tmp/, "/private/tmp");
      return join(root, "custom-stage-source");
    }],
  ];
  for (const [label, sourceCanonical] of cases) {
    const fixture = await makePublicationFixture({ oldPair: false, parent: "/tmp" });
    t.after(() => fixture.cleanup());
    await assert.rejects(
      publishPairDirectories({
        sourceStage: fixture.sourceStage,
        irStage: fixture.irStage,
        sourceCanonical: sourceCanonical(fixture),
        irCanonical: join(fixture.root, `${label}-canonical-ir`),
        lockPath: fixture.lockPath,
      }),
      /spec pair journal paths overlap/,
      label,
    );
    assert.equal(await readFile(join(fixture.sourceStage, "command.js"), "utf8"), "new source\n");
    assert.equal(await pairJournalExists(fixture.lockPath), false, label);
  }
});

test("journal overlap checks use case-folded and /tmp alias paths", async (t) => {
  if (process.platform !== "darwin") return;
  const cases = [
    ["case-folded", (fixture) =>
      fixture.sourceRoot.replace(/custom-source$/, "CUSTOM-SOURCE")],
    ["system alias", (fixture) => {
      const root = fixture.root.startsWith("/private/tmp")
        ? fixture.root.replace(/^\/private\/tmp/, "/tmp")
        : fixture.root.replace(/^\/tmp/, "/private/tmp");
      return join(root, "custom-source");
    }],
  ];
  for (const [label, aliasPath] of cases) {
    const fixture = await makePublicationFixture({ parent: "/tmp" });
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, "verify");
    assert.notEqual(crashed.status, 0, `${label}: ${crashed.output}`);
    const journalPath = join(fixture.root, ".spec-pair.journal.json");
    const journal = JSON.parse(await readFile(journalPath, "utf8"));
    journal.stage.source.path = aliasPath(fixture);
    await writeFile(journalPath, `${JSON.stringify(journal)}\n`);
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /spec pair journal paths overlap/,
      label,
    );
    // Recovery must reject before touching either canonical tree or its
    // managed backups, even though the alias names the same inode.
    assert.equal(await readFile(join(fixture.sourceRoot, "command.js"), "utf8"), "new source\n");
    assert.equal(await pairJournalExists(fixture.lockPath), true, label);
  }
});

test("recovery refuses a same-content clone with a new managed inode", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  const backup = journal.backup.source.path;
  const clone = `${backup}.clone`;
  const displaced = `${backup}.original`;
  const originalIdentity = await lstat(backup);
  await cp(backup, clone, { recursive: true, force: false });
  await rename(backup, displaced);
  await rename(clone, backup);
  assert.notEqual((await lstat(backup)).ino.toString(), originalIdentity.ino.toString());
  await assert.rejects(
    withPairLock(fixture.lockPath, async () => {}),
    /identity changed|changed old source backup/,
  );
  assert.equal(await readFile(join(backup, "command.js"), "utf8"), "old source\n");
  assert.equal(await pairJournalExists(fixture.lockPath), true);
});

test("recovery rejects an extra empty directory before cleanup", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  const backup = journal.backup.source.path;
  const extra = join(backup, "uncommitted-empty");
  await mkdir(extra);
  await assert.rejects(
    withPairLock(fixture.lockPath, async () => {}),
    /changed old source backup|uncommitted empty directory/,
  );
  assert.deepEqual(await readdir(extra), []);
  assert.equal(await pairJournalExists(fixture.lockPath), true);
});

test("legacy file-only descriptors refuse uncommitted empty directories", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "verify");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journalPath = join(fixture.root, ".spec-pair.journal.json");
  const journal = JSON.parse(await readFile(journalPath, "utf8"));
  for (const sides of [journal.oldSides, journal.newSides]) {
    for (const side of ["source", "ir"]) {
      if (sides[side]) delete sides[side].directorySha256;
    }
  }
  await writeFile(journalPath, `${JSON.stringify(journal)}\n`);
  const backup = journal.backup.source.path;
  const extra = join(backup, "legacy-uncommitted-empty");
  await mkdir(extra);
  await assert.rejects(
    withPairLock(fixture.lockPath, async () => {}),
    /uncommitted empty directory/,
  );
  assert.deepEqual(await readdir(extra), []);
  assert.equal(await pairJournalExists(fixture.lockPath), true);
});

test("identity-null stage and backup paths never delete foreign clones", async (t) => {
  const cases = [
    ["stage", { oldPair: true }, (fixture, journal) =>
      cp(fixture.irRoot, journal.stage.ir.path, { recursive: true, force: false })],
    ["backup", { oldState: "none" }, (fixture, journal) =>
      cp(fixture.irRoot, journal.backup.ir.path, { recursive: true, force: false })],
  ];
  for (const [label, options, installClone] of cases) {
    const fixture = await makePublicationFixture(options);
    t.after(() => fixture.cleanup());
    const crashed = runPublicationChild(fixture, "cleanup-source");
    assert.notEqual(crashed.status, 0, `${label}: ${crashed.output}`);
    const journal = await readPairJournal(fixture.lockPath);
    await installClone(fixture, journal);
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /no recorded identity|identity changed/,
      label,
    );
    assert.equal(await pairJournalExists(fixture.lockPath), true, label);
    assert.ok((await readdir(label === "stage" ? journal.stage.ir.path : journal.backup.ir.path)).length > 0, label);
  }
});

test("root inode replacement after capture is rejected before deletion", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "cleanup-source");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  const hookName = "EC_TEST_SPECS_REMOVE_AFTER_CAPTURE_REPLACE_ROOT";
  process.env[hookName] = journal.backup.ir.path;
  try {
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /identity changed|changed old ir backup|refusing to remove changed/,
    );
  } finally {
    delete process.env[hookName];
  }
  assert.equal(await readFile(join(journal.backup.ir.path, "command.json"), "utf8"), "old ir\n");
  assert.equal(await pairJournalExists(fixture.lockPath), true);
});

test("file inode replacement after capture is rejected before deletion", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const crashed = runPublicationChild(fixture, "cleanup-source");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  const hookName = "EC_TEST_SPECS_REMOVE_AFTER_CAPTURE_REPLACE_FILE";
  process.env[hookName] = journal.backup.ir.path;
  try {
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /refusing to remove changed old ir backup file/,
    );
  } finally {
    delete process.env[hookName];
  }
  assert.equal(await readFile(join(journal.backup.ir.path, "command.json"), "utf8"), "old ir\n");
  assert.equal(await pairJournalExists(fixture.lockPath), true);
});

test("recovery rejects same-content replacements of internal directory and file identities", async (t) => {
  for (const kind of ["directory", "file"]) {
    const fixture = await makePublicationFixture();
    t.after(() => fixture.cleanup());
    const nested = join(fixture.irRoot, "nested");
    const stagedNested = join(fixture.irStage, "nested");
    await mkdir(nested, { recursive: true });
    await mkdir(stagedNested, { recursive: true });
    await writeFile(join(nested, "same.txt"), "old nested\n");
    await writeFile(join(stagedNested, "same.txt"), "new nested\n");
    await writePairMarker(
      fixture.irRoot,
      await createPairMarker({
        sourceRoot: fixture.sourceRoot,
        irRoot: fixture.irRoot,
      }),
    );
    await writePairMarker(
      fixture.irStage,
      await createPairMarker({
        sourceRoot: fixture.sourceStage,
        irRoot: fixture.irStage,
      }),
    );
    const crashed = runPublicationChild(fixture, "cleanup-source");
    assert.notEqual(crashed.status, 0, `${kind}: ${crashed.output}`);
    const journal = await readPairJournal(fixture.lockPath);
    const backup = journal.backup.ir.path;
    const target =
      kind === "directory"
        ? join(backup, "nested")
        : join(backup, "nested", "same.txt");
    const clone = join(fixture.root, `${kind}.clone`);
    const displaced = join(fixture.root, `${kind}.original`);
    const originalIdentity = await lstat(target);
    await cp(target, clone, { recursive: kind === "directory", force: false });
    await rename(target, displaced);
    await rename(clone, target);
    assert.notEqual(
      (await lstat(target)).ino.toString(),
      originalIdentity.ino.toString(),
      kind,
    );
    await assert.rejects(
      withPairLock(fixture.lockPath, async () => {}),
      /internal identity|changed old ir backup/,
      kind,
    );
    assert.equal(await pairJournalExists(fixture.lockPath), true, kind);
  }
});

test("journal topology digest is collision-free for newline directory names", async (t) => {
  const fixture = await makePublicationFixture();
  t.after(() => fixture.cleanup());
  const newlineName = `a${String.fromCharCode(10)}b`;
  const oldTopology = join(fixture.sourceRoot, newlineName);
  const stagedTopology = join(fixture.sourceStage, newlineName);
  await mkdir(oldTopology);
  await mkdir(stagedTopology);
  await writePairMarker(
    fixture.irRoot,
    await createPairMarker({
      sourceRoot: fixture.sourceRoot,
      irRoot: fixture.irRoot,
    }),
  );
  await writePairMarker(
    fixture.irStage,
    await createPairMarker({
      sourceRoot: fixture.sourceStage,
      irRoot: fixture.irStage,
    }),
  );
  const crashed = runPublicationChild(fixture, "verify");
  assert.notEqual(crashed.status, 0, crashed.output);
  const journal = await readPairJournal(fixture.lockPath);
  const backup = journal.backup.source.path;
  await rename(join(backup, newlineName), join(fixture.root, "original-topology"));
  await mkdir(join(backup, "a"));
  await mkdir(join(backup, "b"));
  await assert.rejects(
    withPairLock(fixture.lockPath, async () => {}),
    /changed old source backup|internal identity/,
  );
  assert.equal(await pairJournalExists(fixture.lockPath), true);
});

test("pair marker commits stable source and IR digests and excludes metadata", async () => {
  const first = await makePair();
  const second = await makePair();
  try {
    assert.deepEqual(first.marker, second.marker);
    assert.deepEqual(await verifyPair(first), first.marker);
    assert.match(
      await readFile(join(first.irRoot, PAIR_MARKER_NAME), "utf8"),
      /easy-complete-spec-pair/,
    );

    await writeFile(join(first.sourceRoot, "a.js"), "tampered\n");
    await assert.rejects(
      verifyPair(first),
      /source tree does not match \.spec-pair\.json/,
    );
    await writeFile(
      join(first.sourceRoot, "a.js"),
      "export default { name: 'a' };\n",
    );

    await writeFile(join(first.irRoot, "a.json"), '{"names":["tampered"]}\n');
    await assert.rejects(
      verifyPair(first),
      /IR tree does not match \.spec-pair\.json/,
    );
  } finally {
    await Promise.all([first.cleanup(), second.cleanup()]);
  }
});

test("pair verification rejects marker tampering, mixed generations, and symlinks", async () => {
  const first = await makePair();
  const second = await makePair();
  try {
    await writeFile(
      join(first.irRoot, PAIR_MARKER_NAME),
      `${JSON.stringify({ ...first.marker, pairSha256: "0".repeat(64) })}\n`,
    );
    await assert.rejects(
      verifyPair(first),
      /pairSha256 does not match its contents/,
    );

    await writePairMarker(first.irRoot, first.marker);
    await writeFile(
      join(first.sourceRoot, "a.js"),
      await readFile(join(second.sourceRoot, "z.js")),
    );
    await assert.rejects(
      verifyPair(first),
      /source tree does not match \.spec-pair\.json/,
    );

    await symlink(
      join(first.sourceRoot, "z.js"),
      join(first.sourceRoot, "linked.js"),
    );
    await assert.rejects(
      verifyPair(first),
      /pair tree contains a symlink or special entry/,
    );
  } finally {
    await Promise.all([first.cleanup(), second.cleanup()]);
  }
});

test("IR-only verification works for copied app resources", async () => {
  const pair = await makePair();
  try {
    assert.equal(
      (await verifyPair({ irRoot: pair.irRoot, irOnly: true })).pairSha256,
      pair.marker.pairSha256,
    );
    await writeFile(join(pair.irRoot, "a.json"), '{"names":["changed"]}\n');
    await assert.rejects(
      verifyPair({ irRoot: pair.irRoot, irOnly: true }),
      /IR tree does not match \.spec-pair\.json/,
    );
  } finally {
    await pair.cleanup();
  }
});

test("pair marker numeric fields require ordinary decimal JSON tokens", async () => {
  const pair = await makePair();
  try {
    for (const field of ["format", "source.fileCount", "ir.fileCount"]) {
      const marker = JSON.stringify(pair.marker);
      const [parent, child] = field.split(".");
      const replacement =
        parent === "format" ? '"format":1e0' : `"${child}":1e0`;
      const source = marker.replace(
        parent === "format"
          ? /"format":1/
          : new RegExp(`"${child}":${pair.marker[parent][child]}`),
        replacement,
      );
      assert.throws(
        () => parsePairMarker(source),
        /ordinary non-negative decimal integer token/,
      );
    }
    await writeFile(
      join(pair.irRoot, PAIR_MARKER_NAME),
      JSON.stringify({ ...pair.marker, format: 1 }) + "\n",
    );
    assert.deepEqual(await verifyPair(pair), pair.marker);
  } finally {
    await pair.cleanup();
  }
});

test("pair marker rejects duplicate object keys exactly like the Rust reader", async () => {
  const pair = await makePair();
  try {
    const marker = JSON.stringify(pair.marker);
    for (const duplicate of [
      marker.replace('"format":1', '"format":1,"format":1'),
      marker.replace('"format":1', '"format":1,"\\u0066ormat":1'),
      marker.replace(
        '"treeSha256":',
        `"treeSha256":"${pair.marker.source.treeSha256}","treeSha256":`,
      ),
    ]) {
      assert.throws(() => parsePairMarker(duplicate), /duplicate object key/);
    }
  } finally {
    await pair.cleanup();
  }
});

test("pair tree ordering is UTF-8 bytewise across Unicode planes", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-spec-order-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const supplementary = "\u{10000}.txt";
  const bmp = "\u{e000}.txt";
  await writeFile(join(root, supplementary), "supplementary\n");
  await writeFile(join(root, bmp), "bmp\n");

  assert.equal(comparePath(supplementary, bmp) > 0, true);
  const files = [
    [bmp, "bmp\n"],
    [supplementary, "supplementary\n"],
  ];
  const expected = sha256(
    Buffer.from(
      files
        .sort(([left], [right]) => comparePath(left, right))
        .map(([path, value]) => `${path}\0${sha256(value)}\n`)
        .join(""),
    ),
  );
  assert.equal((await digestTree(root)).digest, expected);
});

test("pair lock rejects live owners, recovers dead owners, and refuses bad owners", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-spec-lock-"));
  const lockPath = join(root, ".spec-pair.lock");
  t.after(() => rm(root, { recursive: true, force: true }));

  const first = await acquirePairLock(lockPath);
  await assert.rejects(acquirePairLock(lockPath), /already locked by pid/);
  await first.release();

  await writeFile(
    lockPath,
    JSON.stringify({ pid: 99999999, token: "dead", startedAt: "old" }),
  );
  const recovered = await acquirePairLock(lockPath);
  await recovered.release();

  const orphanCandidate = `${lockPath}.new.99999999.crashed-candidate`;
  await writeFile(
    orphanCandidate,
    JSON.stringify({
      pid: 99999999,
      token: "crashed-candidate",
      startedAt: "old",
    }),
  );
  const afterCandidateRecovery = await acquirePairLock(lockPath);
  assert.deepEqual(
    (await readdir(root)).filter(
      (name) => name === ".spec-pair.lock.new.99999999.crashed-candidate",
    ),
    [],
  );
  await afterCandidateRecovery.release();

  await writeFile(lockPath, "not json\n");
  await assert.rejects(acquirePairLock(lockPath), /malformed lock/);

  await rm(lockPath, { recursive: true, force: true });
  const foreignLock = join(root, "foreign-lock");
  await mkdir(foreignLock);
  await symlink(foreignLock, lockPath);
  await assert.rejects(acquirePairLock(lockPath), /malformed lock/);
});

test("pair lock release never removes a lock whose owner token changed", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-spec-lock-token-"));
  const lockPath = join(root, ".spec-pair.lock");
  t.after(() => rm(root, { recursive: true, force: true }));

  const lock = await acquirePairLock(lockPath);
  await writeFile(
    lockPath,
    JSON.stringify({ pid: process.pid, token: "different", startedAt: "now" }),
  );
  await assert.rejects(lock.release(), /lock not owned by this process/);
  assert.equal(
    await readFile(lockPath, "utf8"),
    '{"pid":' + process.pid + ',"token":"different","startedAt":"now"}',
  );
});

test("pair lock keeps a live owner quarantined during stale recovery", async (t) => {
  const root = await mkdtemp(
    join(tmpdir(), "easy-complete-spec-lock-quarantine-"),
  );
  const lockPath = join(root, ".spec-pair.lock");
  t.after(() => rm(root, { recursive: true, force: true }));

  const lock = await acquirePairLock(lockPath);
  const quarantinePath = `${lockPath}.stale.replacement`;
  await rename(lockPath, quarantinePath);
  await assert.rejects(acquirePairLock(lockPath), /already locked by pid/);
  assert.equal(
    await readFile(quarantinePath, "utf8"),
    JSON.stringify(lock.owner) + "\n",
  );
  await lock.release();
  await assert.rejects(readFile(quarantinePath, "utf8"), { code: "ENOENT" });
});

test("pair lock cleans a candidate when publication fails", async (t) => {
  const root = await mkdtemp(
    join(tmpdir(), "easy-complete-spec-lock-candidate-"),
  );
  const lockPath = join(root, ".spec-pair.lock");
  t.after(() => rm(root, { recursive: true, force: true }));

  await mkdir(lockPath);
  await assert.rejects(acquirePairLock(lockPath), /malformed lock/);
  assert.deepEqual(
    (await readdir(root)).filter((name) =>
      name.startsWith(".spec-pair.lock.new."),
    ),
    [],
  );
});

test("pair lock reuses only its async owner context and exposes a verifiable proof", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-spec-lock-proof-"));
  const lockPath = join(root, ".spec-pair.lock");
  t.after(() => rm(root, { recursive: true, force: true }));
  let proof;
  let continueLateTask;
  const lateGate = new Promise((resolve) => {
    continueLateTask = resolve;
  });
  let lateTask;

  await withPairLock(lockPath, async (outer) => {
    proof = outer.proof;
    assert.equal(await verifyPairLockProof(lockPath, proof), true);
    await withPairLock(lockPath, async (inner) => {
      assert.deepEqual(inner.proof, proof);
    });
    await assert.rejects(
      withPairLock(lockPath, async () => {}, { lockHeld: true }),
      /pair lock options contains unknown field lockHeld/,
    );
    lateTask = (async () => {
      await lateGate;
      return withPairLock(lockPath, async (fresh) => fresh.proof);
    })();
  });

  await assert.rejects(verifyPairLockProof(lockPath, proof), { code: "ENOENT" });
  continueLateTask();
  const freshProof = await lateTask;
  assert.notEqual(freshProof.token, proof.token);
});

test("concurrent contenders recover one dead owner without double acquisition", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-spec-lock-race-"));
  const lockPath = join(root, ".spec-pair.lock");
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(
    lockPath,
    JSON.stringify({ pid: 99999999, token: "dead", startedAt: "old" }),
  );

  const workerSource = `
    import { acquirePairLock } from ${JSON.stringify(
      new URL("./spec-pair.mjs", import.meta.url).href,
    )};
    const lock = await acquirePairLock(process.argv[1]).catch((error) => {
      process.stdout.write("error:" + error.message + "\\n");
      process.exit(2);
    });
    if (!lock) process.exit(2);
    process.stdout.write("acquired\\n");
    process.stdin.once("data", async () => {
      await lock.release();
      process.stdout.write("released\\n");
      process.exit(0);
    });
  `;
  const workers = Array.from({ length: 8 }, () => {
    const child = spawn(
      process.execPath,
      ["--input-type=module", "-e", workerSource, lockPath],
      { stdio: ["pipe", "pipe", "pipe"] },
    );
    let output = "";
    let resolveReady;
    const ready = new Promise((resolve) => {
      resolveReady = resolve;
    });
    child.stdout.on("data", (chunk) => {
      output += chunk;
      if (output.includes("acquired\n") || output.includes("error:")) {
        resolveReady();
      }
    });
    const done = new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (code, signal) => {
        resolveReady();
        resolve({ code, signal, output });
      });
    });
    return {
      child,
      ready,
      done,
      get output() {
        return output;
      },
    };
  });
  t.after(async () => {
    for (const { child } of workers) {
      if (!child.killed) child.kill("SIGKILL");
    }
    await Promise.allSettled(workers.map(({ done }) => done));
  });

  await Promise.all(workers.map(({ ready }) => ready));
  const acquired = workers.filter(({ output }) =>
    output.includes("acquired\n"),
  );
  assert.equal(acquired.length, 1);
  assert.equal(
    workers.filter(({ output }) => output.includes("error:")).length,
    workers.length - 1,
  );
  acquired[0].child.stdin.write("release\n");
  const results = await Promise.all(workers.map(({ done }) => done));
  assert.equal(results.filter(({ code }) => code === 0).length, 1);
  assert.equal(
    results.filter(({ output }) => output.includes("released\n")).length,
    1,
  );
});
