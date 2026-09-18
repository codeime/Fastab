import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  realpath,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(
  new URL("./sync-bundled-specs.mjs", import.meta.url),
);
const pairScriptPath = fileURLToPath(
  new URL("./spec-pair.mjs", import.meta.url),
);
const compilerPath = fileURLToPath(
  new URL("./compile-spec-ir.mjs", import.meta.url),
);
const auditPath = fileURLToPath(
  new URL("./audit-spec-hooks.mjs", import.meta.url),
);
const helperPath = fileURLToPath(
  new URL("./filepaths-helper.mjs", import.meta.url),
);
const hookContractPath = fileURLToPath(
  new URL("./spec-hook-contract.mjs", import.meta.url),
);
const typedHookIrPath = fileURLToPath(
  new URL("./typed-hook-ir.mjs", import.meta.url),
);
const typedHookInlinePath = fileURLToPath(
  new URL("./typed-hook-inline.mjs", import.meta.url),
);
const typedRegexPath = fileURLToPath(
  new URL("./typed-regex.mjs", import.meta.url),
);
const referenceSafeIoPath = fileURLToPath(
  new URL("./reference-safe-io.mjs", import.meta.url),
);
const pairPath = fileURLToPath(new URL("./spec-pair.mjs", import.meta.url));
const repoNodeModules = fileURLToPath(
  new URL("../node_modules/", import.meta.url),
);
const packageName = "@chen86860/autocomplete-specs";

function lockfile(version) {
  return `lockfileVersion: '9.0'\nimporters:\n  .:\n    devDependencies:\n      '${packageName}':\n        specifier: 1.0.0\n        version: ${version}\n`;
}

function run(script, args, extraEnvironment = {}, nodeArguments = []) {
  const environment = Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) => !key.startsWith("BUNDLED_SPECS_"),
    ),
  );
  Object.assign(environment, extraEnvironment);
  try {
    return {
      status: 0,
      output: execFileSync(
        process.execPath,
        [...nodeArguments, script, ...args],
        {
          env: environment,
          encoding: "utf8",
          stdio: ["ignore", "pipe", "pipe"],
        },
      ),
    };
  } catch (error) {
    return {
      status: error.status,
      output: `${error.stdout ?? ""}${error.stderr ?? ""}`,
    };
  }
}

test("sync guards empty packages, missing icons, and lock drift without replacing a good bundle", async (t) => {
  // macOS exposes the temporary tree as /var -> /private/var. Keep fixture
  // paths on the real side so the production symlink-parent guard tests only
  // the aliases created by this test, not the OS compatibility alias.
  const root = await realpath(
    await mkdtemp(join(tmpdir(), "easy-complete-sync-test-")),
  );
  t.after(() => rm(root, { recursive: true, force: true }));
  const script = join(root, "scripts", "sync-bundled-specs.mjs");
  const packageRoot = join(
    root,
    "node_modules",
    "@chen86860",
    "autocomplete-specs",
  );
  const bundle = join(root, "bundle", "specs");
  await mkdir(dirname(script), { recursive: true });
  await copyFile(scriptPath, script);
  await copyFile(compilerPath, join(root, "scripts", "compile-spec-ir.mjs"));
  await copyFile(auditPath, join(root, "scripts", "audit-spec-hooks.mjs"));
  await copyFile(helperPath, join(root, "scripts", "filepaths-helper.mjs"));
  await copyFile(
    hookContractPath,
    join(root, "scripts", "spec-hook-contract.mjs"),
  );
  await copyFile(
    typedHookIrPath,
    join(root, "scripts", "typed-hook-ir.mjs"),
  );
  await copyFile(
    typedHookInlinePath,
    join(root, "scripts", "typed-hook-inline.mjs"),
  );
  await copyFile(typedRegexPath, join(root, "scripts", "typed-regex.mjs"));
  await copyFile(
    referenceSafeIoPath,
    join(root, "scripts", "reference-safe-io.mjs"),
  );
  await copyFile(pairPath, join(root, "scripts", "spec-pair.mjs"));
  await mkdir(join(packageRoot, "build"), { recursive: true });
  for (const dependency of ["acorn", "eslint-scope"]) {
    await symlink(
      join(repoNodeModules, dependency),
      join(root, "node_modules", dependency),
    );
  }
  await mkdir(bundle, { recursive: true });
  const legacyIr = join(root, "bundle", "specs-ir");
  await mkdir(legacyIr, { recursive: true });
  await writeFile(join(legacyIr, "legacy-sentinel.txt"), "replace me\n");
  await writeFile(
    join(root, "package.json"),
    JSON.stringify({ devDependencies: { [packageName]: "1.0.0" } }),
  );
  await writeFile(join(root, "pnpm-lock.yaml"), lockfile("1.0.0"));
  await writeFile(
    join(root, "specs.config.json"),
    JSON.stringify({ exclude: [], icons: [] }),
  );
  await writeFile(
    join(packageRoot, "package.json"),
    JSON.stringify({
      name: packageName,
      version: "1.0.0",
      main: "build/index.js",
    }),
  );
  await writeFile(
    join(packageRoot, "build", "index.js"),
    "module.exports = {};\n",
  );
  await writeFile(
    join(packageRoot, "build", "demo.js"),
    "module.exports = { name: 'demo' };\n",
  );
  await writeFile(join(bundle, "index.json"), '{"completions":["old"]}\n');

  assert.notEqual(
    run(script, ["--check"]).status,
    0,
    "no manifest is not fresh",
  );
  const initialSync = run(script, []);
  assert.equal(initialSync.status, 0, initialSync.output);
  assert.equal(run(script, ["--check"]).status, 0);
  assert.deepEqual(
    JSON.parse(await readFile(join(bundle, "index.json"), "utf8")).completions,
    ["demo"],
  );
  const goodIndex = await readFile(join(bundle, "index.json"));
  const goodDemo = await readFile(join(bundle, "demo.js"));
  const goodManifest = await readFile(join(bundle, ".source-manifest.json"));
  const goodIrIndex = await readFile(
    join(root, "bundle", "specs-ir", "index.json"),
  );
  const pairMarkerPath = join(root, "bundle", "specs-ir", ".spec-pair.json");
  const goodPairMarker = await readFile(pairMarkerPath);

  // Removed options must fail closed without touching the published pair.
  const removedNoCompile = run(script, ["--no-compile"]);
  assert.notEqual(removedNoCompile.status, 0);
  assert.match(removedNoCompile.output, /Unknown option --no-compile/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(await readFile(pairMarkerPath), goodPairMarker);

  await writeFile(
    join(packageRoot, "build", "demo.js"),
    "module.exports = { name: 'changed' };\n",
  );
  const changedSync = run(script, []);
  assert.equal(changedSync.status, 0, changedSync.output);
  const changedPair = run(pairScriptPath, [], {
    EC_SPECS_SRC: bundle,
    EC_SPECS_IR: join(root, "bundle", "specs-ir"),
  });
  assert.equal(changedPair.status, 0, changedPair.output);
  assert.deepEqual(
    JSON.parse(await readFile(join(bundle, "index.json"), "utf8")).completions,
    ["demo"],
  );
  await writeFile(
    join(packageRoot, "build", "demo.js"),
    "module.exports = { name: 'demo' };\n",
  );
  assert.equal(run(script, []).status, 0);

  // A marker-bearing IR tree is owned only when its marker verifies. Restore
  // the known-good marker after the check so the rest of this fixture
  // continues from a valid published pair.
  await writeFile(pairMarkerPath, "{}\n");
  const refusedForeignIr = run(script, []);
  assert.notEqual(refusedForeignIr.status, 0);
  assert.match(
    refusedForeignIr.output,
    /compiled IR destination without a valid \.spec-pair\.json/,
  );
  await writeFile(pairMarkerPath, goodPairMarker);

  // The destination overrides are symmetric: a canonical source cannot be
  // published with a custom IR because that would leave the canonical pair
  // stale. Nothing in the canonical pair may change on this rejected run.
  const reverseCustomIr = join(root, "reverse-custom-ir");
  const canonicalSourceWithCustomIr = run(script, [], {
    EC_SPECS_IR: reverseCustomIr,
  });
  assert.notEqual(canonicalSourceWithCustomIr.status, 0);
  assert.match(
    canonicalSourceWithCustomIr.output,
    /custom outputs require both destinations/,
  );
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(await readFile(join(bundle, "demo.js")), goodDemo);
  assert.deepEqual(
    await readFile(join(root, "bundle", "specs-ir", "index.json")),
    goodIrIndex,
  );
  assert.deepEqual(await readFile(pairMarkerPath), goodPairMarker);
  const canonicalPairAfterReverse = run(pairScriptPath, [], {
    EC_SPECS_SRC: bundle,
    EC_SPECS_IR: join(root, "bundle", "specs-ir"),
  });
  assert.equal(
    canonicalPairAfterReverse.status,
    0,
    canonicalPairAfterReverse.output,
  );

  const missingCustomIrDestination = join(root, "missing-custom-ir-bundle");
  const missingCustomIr = run(script, [], {
    BUNDLED_SPECS_DIR: missingCustomIrDestination,
  });
  assert.notEqual(missingCustomIr.status, 0);
  assert.match(
    missingCustomIr.output,
    /custom outputs require both destinations/,
  );
  assert.equal(
    await readFile(join(missingCustomIrDestination, "index.json")).catch(
      () => null,
    ),
    null,
  );

  const foreignDestination = join(root, "foreign-bundle");
  const foreignIr = join(root, "foreign-ir");
  const foreignSentinel = join(foreignDestination, "USER_SENTINEL");
  await mkdir(foreignDestination);
  await writeFile(foreignSentinel, "must survive\n");
  const refusedForeignDestination = run(script, [], {
    BUNDLED_SPECS_DIR: foreignDestination,
    EC_SPECS_IR: foreignIr,
  });
  assert.notEqual(refusedForeignDestination.status, 0);
  assert.match(
    refusedForeignDestination.output,
    /refusing to replace non-empty custom bundled specs destination/,
  );
  assert.equal(await readFile(foreignSentinel, "utf8"), "must survive\n");

  const managedDestination = join(root, "managed-bundle");
  const managedIr = join(root, "managed-ir");
  await mkdir(managedIr);
  const markerlessIrSentinel = join(managedIr, "legacy-sentinel.txt");
  await writeFile(markerlessIrSentinel, "must survive\n");
  const refusedMarkerlessCustomIr = run(script, [], {
    BUNDLED_SPECS_DIR: managedDestination,
    EC_SPECS_IR: managedIr,
  });
  assert.notEqual(refusedMarkerlessCustomIr.status, 0);
  assert.match(
    refusedMarkerlessCustomIr.output,
    /compiled IR destination without a valid \.spec-pair\.json/,
  );
  assert.equal(await readFile(markerlessIrSentinel, "utf8"), "must survive\n");
  await rm(managedIr, { recursive: true, force: true });

  for (let attempt = 0; attempt < 2; attempt += 1) {
    const managedSync = run(script, [], {
      BUNDLED_SPECS_DIR: managedDestination,
      EC_SPECS_IR: managedIr,
    });
    assert.equal(managedSync.status, 0, managedSync.output);
  }
  const managedSentinel = join(managedDestination, "USER_MODIFICATION");
  await writeFile(managedSentinel, "must also survive\n");
  const refusedModifiedDestination = run(script, [], {
    BUNDLED_SPECS_DIR: managedDestination,
    EC_SPECS_IR: managedIr,
  });
  assert.notEqual(refusedModifiedDestination.status, 0);
  assert.match(
    refusedModifiedDestination.output,
    /refusing to replace modified custom bundled specs destination/,
  );
  assert.equal(await readFile(managedSentinel, "utf8"), "must also survive\n");

  await rm(managedSentinel);
  const emptyUserDirectory = join(managedDestination, "USER_EMPTY_DIR");
  await mkdir(emptyUserDirectory);
  const refusedEmptyDirectory = run(script, [], {
    BUNDLED_SPECS_DIR: managedDestination,
    EC_SPECS_IR: managedIr,
  });
  assert.notEqual(refusedEmptyDirectory.status, 0);
  assert.match(refusedEmptyDirectory.output, /untracked empty directory/);
  assert.deepEqual(await readdir(emptyUserDirectory), []);

  const aliasDestination = join(root, "alias-bundle");
  const aliasIr = join(root, "alias-ir");
  const aliasSync = run(script, [], {
    BUNDLED_SPECS_DIR: `${aliasDestination}/.`,
    EC_SPECS_IR: aliasIr,
  });
  assert.equal(aliasSync.status, 0, aliasSync.output);
  assert.deepEqual(
    JSON.parse(await readFile(join(aliasDestination, "index.json"), "utf8"))
      .completions,
    ["demo"],
  );

  // A custom source destination is paired with the custom output itself and
  // must not rewrite the default source tree. Change the package content so
  // the two output trees cannot accidentally pass by having identical bytes.
  const defaultDemo = await readFile(join(bundle, "demo.js"));
  await writeFile(
    join(packageRoot, "build", "demo.js"),
    "module.exports = { name: 'custom' };\n",
  );
  const customSync = run(script, [], {
    BUNDLED_SPECS_DIR: aliasDestination,
    EC_SPECS_IR: aliasIr,
  });
  assert.equal(customSync.status, 0, customSync.output);
  assert.deepEqual(await readFile(join(bundle, "demo.js")), defaultDemo);
  assert.deepEqual(await readFile(pairMarkerPath), goodPairMarker);
  const defaultPairAfterCustom = run(pairScriptPath, [], {
    EC_SPECS_SRC: bundle,
    EC_SPECS_IR: join(root, "bundle", "specs-ir"),
  });
  assert.equal(defaultPairAfterCustom.status, 0, defaultPairAfterCustom.output);
  const customPair = run(pairScriptPath, [], {
    EC_SPECS_SRC: aliasDestination,
    EC_SPECS_IR: aliasIr,
  });
  assert.equal(customPair.status, 0, customPair.output);
  assert.deepEqual(
    await readFile(join(aliasDestination, "demo.js")),
    Buffer.from("module.exports = { name: 'custom' };\n"),
  );
  await writeFile(
    join(packageRoot, "build", "demo.js"),
    "module.exports = { name: 'demo' };\n",
  );

  // A POSIX filename can contain a literal backslash.  It must be rejected
  // while still raw; converting it to `/` would turn this entry into an escape
  // path and could publish bytes outside the staging root.
  const backslashSpec = join(
    packageRoot,
    "build",
    "..\\..\\outside.js",
  );
  await writeFile(backslashSpec, "module.exports = { name: 'outside' };\n");
  const refusedBackslashSpec = run(script, []);
  assert.notEqual(refusedBackslashSpec.status, 0);
  assert.match(refusedBackslashSpec.output, /unsafe bundled spec asset path/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(await readFile(pairMarkerPath), goodPairMarker);
  await rm(backslashSpec);

  // Source symlinks must not be followed by the package sync.  Keep the
  // target outside the package tree so a successful copy would be observable.
  const linkedSpecTarget = join(root, "linked-spec-target.js");
  const linkedSpec = join(packageRoot, "build", "linked.js");
  await writeFile(linkedSpecTarget, "module.exports = { name: 'linked' };\n");
  await symlink(linkedSpecTarget, linkedSpec);
  const refusedLinkedSpec = run(script, []);
  assert.notEqual(refusedLinkedSpec.status, 0);
  assert.match(
    refusedLinkedSpec.output,
    /source tree contains a symlink or special entry/,
  );
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(await readFile(pairMarkerPath), goodPairMarker);
  await rm(linkedSpec, { force: true });
  await rm(linkedSpecTarget, { force: true });

  // Icons are input assets too; a symlink must fail even when it is not the
  // currently selected icon, otherwise an exclusion/config change could make
  // the same package unsafe on a later sync.
  const iconTarget = join(root, "icon-target.png");
  const iconLink = join(packageRoot, "icons", "linked.png");
  await mkdir(join(packageRoot, "icons"), { recursive: true });
  await writeFile(iconTarget, Buffer.from("linked icon\n"));
  await symlink(iconTarget, iconLink);
  const refusedLinkedIcon = run(script, []);
  assert.notEqual(refusedLinkedIcon.status, 0);
  assert.match(
    refusedLinkedIcon.output,
    /icon tree contains a symlink or special entry/,
  );
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(await readFile(pairMarkerPath), goodPairMarker);
  await rm(iconLink, { force: true });
  await rm(join(packageRoot, "icons"), { recursive: true, force: true });
  await rm(iconTarget, { force: true });

  if (process.platform !== "win32") {
    // A FIFO is neither a regular file nor a directory and must be rejected,
    // rather than silently ignored by the source-tree walker.
    const specialSpec = join(packageRoot, "build", "special.js");
    execFileSync("mkfifo", [specialSpec]);
    const refusedSpecialSpec = run(script, []);
    assert.notEqual(refusedSpecialSpec.status, 0);
    assert.match(
      refusedSpecialSpec.output,
      /source tree contains a symlink or special entry/,
    );
    assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
    assert.deepEqual(await readFile(pairMarkerPath), goodPairMarker);
    await rm(specialSpec, { force: true });
  }

  const symlinkTarget = join(root, "symlink-target");
  const symlinkDestination = join(root, "symlink-bundle");
  await mkdir(symlinkTarget);
  await writeFile(join(symlinkTarget, "USER_SENTINEL"), "linked\n");
  await symlink(symlinkTarget, symlinkDestination);
  const refusedSymlink = run(script, [], {
    BUNDLED_SPECS_DIR: symlinkDestination,
    EC_SPECS_IR: join(root, "symlink-ir"),
  });
  assert.notEqual(refusedSymlink.status, 0);
  assert.match(refusedSymlink.output, /through a symbolic link/);
  assert.equal(
    await readFile(join(symlinkTarget, "USER_SENTINEL"), "utf8"),
    "linked\n",
  );

  const danglingDestination = join(root, "dangling-bundle");
  await symlink(join(root, "missing-bundle-target"), danglingDestination);
  const refusedDanglingSymlink = run(script, [], {
    BUNDLED_SPECS_DIR: danglingDestination,
    EC_SPECS_IR: join(root, "dangling-ir"),
  });
  assert.notEqual(refusedDanglingSymlink.status, 0);
  assert.match(refusedDanglingSymlink.output, /through a symbolic link/);

  const symlinkParentTarget = join(root, "real-bundle-parent");
  const symlinkParent = join(root, "linked-bundle-parent");
  await mkdir(symlinkParentTarget);
  await writeFile(join(symlinkParentTarget, "USER_SENTINEL"), "parent\n");
  await symlink(symlinkParentTarget, symlinkParent);
  const refusedSymlinkParent = run(script, [], {
    BUNDLED_SPECS_DIR: join(symlinkParent, "nested-bundle"),
    EC_SPECS_IR: join(root, "symlink-parent-ir"),
  });
  assert.notEqual(refusedSymlinkParent.status, 0);
  assert.match(refusedSymlinkParent.output, /through a symbolic link/);
  assert.equal(
    await readFile(join(symlinkParentTarget, "USER_SENTINEL"), "utf8"),
    "parent\n",
  );

  for (const [overlappingDestination, overlappingIr] of [
    [join(root, "bundle"), join(root, "bundle", "custom-ir")],
    [
      join(root, "bundle", "specs-ir", "nested"),
      join(root, "bundle", "specs-ir", "nested", "other-ir"),
    ],
  ]) {
    const unsafePath = run(script, [], {
      BUNDLED_SPECS_DIR: overlappingDestination,
      EC_SPECS_IR: overlappingIr,
    });
    assert.notEqual(unsafePath.status, 0);
    assert.match(unsafePath.output, /destinations cannot overlap/);
  }
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(
    await readFile(join(root, "bundle", "specs-ir", "index.json")),
    goodIrIndex,
  );

  const mockFetch = join(root, "mock-cdn-fetch.mjs");
  await writeFile(
    mockFetch,
    `globalThis.fetch = async () => new Response(JSON.stringify({ completions: ["../escape"], diffVersionedCompletions: [] }), { status: 200 });\n`,
  );
  const unsafeCdnIndex = run(script, [], { BUNDLED_SPECS_SOURCE: "cdn" }, [
    "--import",
    mockFetch,
  ]);
  assert.notEqual(unsafeCdnIndex.status, 0);
  assert.match(unsafeCdnIndex.output, /unsafe bundled spec asset path/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.equal(
    await readFile(join(root, "bundle", "escape.js")).catch(() => null),
    null,
  );

  await writeFile(join(root, "pnpm-lock.yaml"), lockfile("0.9.0"));
  const mismatchedPin = run(script, []);
  assert.notEqual(mismatchedPin.status, 0);
  assert.match(
    mismatchedPin.output,
    /root lock version .* does not match installed/,
  );
  const emptyPackageOverride = run(script, [], {
    BUNDLED_SPECS_PACKAGE: "",
  });
  assert.notEqual(emptyPackageOverride.status, 0);
  assert.match(
    emptyPackageOverride.output,
    /root lock version .* does not match installed/,
  );
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(
    await readFile(join(bundle, ".source-manifest.json")),
    goodManifest,
  );

  await writeFile(join(root, "pnpm-lock.yaml"), lockfile("1.0.0"));
  await rm(join(packageRoot, "build", "demo.js"));
  const emptyPackage = run(script, []);
  assert.notEqual(emptyPackage.status, 0);
  assert.match(emptyPackage.output, /no bundled completions after filtering/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(
    await readFile(join(bundle, ".source-manifest.json")),
    goodManifest,
  );

  await writeFile(
    join(packageRoot, "build", "demo.js"),
    "module.exports = { name: 'demo' };\n",
  );
  await writeFile(
    join(root, "specs.config.json"),
    JSON.stringify({ exclude: [], icons: ["alert"] }),
  );
  const missingIcon = run(script, []);
  assert.notEqual(missingIcon.status, 0);
  assert.match(missingIcon.output, /missing required icons: alert/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(
    await readFile(join(bundle, ".source-manifest.json")),
    goodManifest,
  );

  await writeFile(
    join(root, "specs.config.json"),
    JSON.stringify({ exclude: [], icons: [] }),
  );
  await writeFile(
    join(packageRoot, "build", "broken.js"),
    "module.exports = =;\n",
  );
  const brokenSpec = run(script, []);
  assert.notEqual(brokenSpec.status, 0);
  assert.match(brokenSpec.output, /spec compilation failed closed/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(
    await readFile(join(bundle, ".source-manifest.json")),
    goodManifest,
  );
  assert.deepEqual(
    await readFile(join(root, "bundle", "specs-ir", "index.json")),
    goodIrIndex,
  );

  const tarSource = join(root, "tar-source", "package");
  await mkdir(join(tarSource, "build"), { recursive: true });
  await writeFile(
    join(tarSource, "package.json"),
    JSON.stringify({
      name: packageName,
      version: "1.0.0",
      main: "build/index.js",
    }),
  );
  await writeFile(
    join(tarSource, "build", "index.js"),
    "module.exports = {};\n",
  );
  await writeFile(
    join(tarSource, "build", "demo.js"),
    "module.exports = { name: 'demo' };\n",
  );
  const fetchTar = join(root, "mock-tar-fetch.mjs");
  await writeFile(
    fetchTar,
    `import { readFile } from "node:fs/promises";\nglobalThis.fetch = async () => new Response(await readFile(process.env.EC_TEST_TARBALL_PATH), { status: 200 });\n`,
  );
  await symlink("../../escape", join(tarSource, "build", "bad.js"));
  const unsafeTar = join(root, "unsafe-package.tgz");
  execFileSync("tar", [
    "-czf",
    unsafeTar,
    "-C",
    join(root, "tar-source"),
    "package",
  ]);
  const linkedArchive = run(
    script,
    [],
    {
      BUNDLED_SPECS_SOURCE: "npm",
      BUNDLED_SPECS_PACKAGE_TARBALL: "https://fixture.invalid/specs.tgz",
      EC_TEST_TARBALL_PATH: unsafeTar,
    },
    ["--import", fetchTar],
  );
  assert.notEqual(linkedArchive.status, 0);
  assert.match(linkedArchive.output, /unsupported link or special entry/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), goodIndex);
  assert.deepEqual(
    await readFile(join(root, "bundle", "specs-ir", "index.json")),
    goodIrIndex,
  );

  await rm(join(tarSource, "build", "bad.js"));
  const safeTar = join(root, "safe-package.tgz");
  execFileSync("tar", [
    "-czf",
    safeTar,
    "-C",
    join(root, "tar-source"),
    "package",
  ]);
  const safeArchive = run(
    script,
    [],
    {
      BUNDLED_SPECS_SOURCE: "npm",
      BUNDLED_SPECS_PACKAGE_TARBALL: "https://fixture.invalid/specs.tgz",
      EC_TEST_TARBALL_PATH: safeTar,
    },
    ["--import", fetchTar],
  );
  assert.equal(safeArchive.status, 0, safeArchive.output);
  assert.equal(
    JSON.parse(await readFile(join(bundle, ".source-manifest.json"))).source
      .mode,
    "npm",
  );
  const finalPair = run(pairScriptPath, [], {
    EC_SPECS_SRC: bundle,
    EC_SPECS_IR: join(root, "bundle", "specs-ir"),
  });
  assert.equal(finalPair.status, 0, finalPair.output);

  // A final-audit failure is deliberately recoverable state, so leave this
  // scenario last: the next invocation must supply the matching verifier
  // rather than treating an unrelated input as permission to discard it.
  await rm(join(packageRoot, "build", "broken.js"));
  await writeFile(
    join(packageRoot, "build", "package.json"),
    JSON.stringify({ type: "module" }),
  );
  await writeFile(
    join(packageRoot, "build", "demo.js"),
    `export default { name: "demo" };\n`,
  );
  await writeFile(
    join(packageRoot, "build", "drift.js"),
    `const staged = new Error().stack.includes(".staging-");
export default {
  name: "drift",
  args: [{
    name: "target",
    generators: {
      script: "printf x",
      postProcess: staged
        ? function stagedRows() { return [{ name: "staged" }]; }
        : function publishedRows() { return [{ name: "published" }]; }
    }
  }]
};\n`,
  );
  const stableIndex = await readFile(join(bundle, "index.json"));
  const stableManifest = await readFile(join(bundle, ".source-manifest.json"));
  const stableIrIndex = await readFile(
    join(root, "bundle", "specs-ir", "index.json"),
  );
  const pathSensitive = run(script, []);
  assert.notEqual(pathSensitive.status, 0);
  assert.match(pathSensitive.output, /Published spec IR audit failed/);
  assert.deepEqual(await readFile(join(bundle, "index.json")), stableIndex);
  assert.deepEqual(
    await readFile(join(bundle, ".source-manifest.json")),
    stableManifest,
  );
  assert.deepEqual(
    await readFile(join(root, "bundle", "specs-ir", "index.json")),
    stableIrIndex,
  );
});
