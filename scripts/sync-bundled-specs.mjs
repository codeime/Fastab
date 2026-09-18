#!/usr/bin/env node
import {
  cp,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  realpath,
  readdir,
  rm,
  rmdir,
  unlink,
  writeFile,
} from "node:fs/promises";
import {
  basename,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { execFile } from "node:child_process";
import { createRequire } from "node:module";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
import {
  PAIR_LOCK_NAME,
  PAIR_MARKER_NAME,
  assertNoSymlinkInPath,
  comparePath,
  pairJournalExists,
  publishPairDirectories,
  verifyPair,
  withPairLock,
} from "./spec-pair.mjs";

const execFileAsync = promisify(execFile);
const require = createRequire(import.meta.url);

const SPEC_BASE_URL = "https://specs.q.us-east-1.amazonaws.com/";
const SOURCE_MANIFEST_NAME = ".source-manifest.json";
const SOURCE_MANIFEST_VERSION = 1;

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const rootPackagePath = join(repoDir, "package.json");
const lockfilePath = join(repoDir, "pnpm-lock.yaml");
const configPath = join(repoDir, "specs.config.json");
const canonicalIrOutDir = join(repoDir, "bundle", "specs-ir");
const pairLockPath = join(repoDir, "bundle", PAIR_LOCK_NAME);

// Spec filtering settings live in specs.config.json so they can be edited and reviewed
// without touching this script. The package version is pinned by package.json/pnpm-lock.
// Env vars still override source settings for CI / one-off tries.
//   { "exclude": ["aws"] }
const config = JSON.parse(await readFile(configPath, "utf8"));
const rootPackage = JSON.parse(await readFile(rootPackagePath, "utf8"));

// Source: an npm package published from the forked spec repo. The package contains
// build/<name>.js (+ nested + <name>/index.js for diff-versioned), build/index.json,
// and icons/<name>.png. We install it as a normal devDependency so package.json and
// pnpm-lock.yaml are the source of truth for the pinned version.
//
// PINNED by the lockfile — NOT "latest" — so builds are reproducible: the bundle changes
// only when the dependency changes. To adopt a newer fork build:
//   1. bump @chen86860/autocomplete-specs in package.json/pnpm-lock.yaml
//   2. re-run `node scripts/sync-bundled-specs.mjs`
//   3. commit the regenerated bundle/specs together with the dependency update
// Overrides: BUNDLED_SPECS_PACKAGE=<pkg>, BUNDLED_SPECS_VERSION=<version|latest>,
// BUNDLED_SPECS_PACKAGE_TARBALL=<full-url>, BUNDLED_SPECS_NPM_REGISTRY=<registry>,
// or BUNDLED_SPECS_SOURCE=cdn to fall back to the legacy per-file CDN sync.
const SPECS_PACKAGE =
  process.env.BUNDLED_SPECS_PACKAGE ||
  config.package ||
  "@chen86860/autocomplete-specs";
const SPECS_VERSION = process.env.BUNDLED_SPECS_VERSION || config.version;
const SPECS_NPM_REGISTRY =
  process.env.BUNDLED_SPECS_NPM_REGISTRY ||
  config.registry ||
  "https://registry.npmjs.org/";
const packageTarballUrl = process.env.BUNDLED_SPECS_PACKAGE_TARBALL;
const sourceMode = process.env.BUNDLED_SPECS_SOURCE || "dependency";

const defaultOutDir = join(repoDir, "bundle", "specs");
const requestedOutDir = process.env.BUNDLED_SPECS_DIR || defaultOutDir;
const requestedIrOutDir = process.env.EC_SPECS_IR || canonicalIrOutDir;
// Named spec icons to keep. `crates/ec_gpui/src/icons.rs` embeds each of these
// with include_bytes!, so the two lists have to move together. An empty list
// keeps every icon the archive ships.
const iconNames = Array.isArray(config.icons) ? config.icons : [];

const concurrency = Number(process.env.BUNDLED_SPECS_CONCURRENCY || 16);
const maxAttempts = Number(process.env.BUNDLED_SPECS_FETCH_ATTEMPTS || 5);
// `--check` is the only supported argument and is a read-only build freshness
// gate. Every successful sync publishes the source and compiled IR together.
const scriptArgs = process.argv.slice(2);
const unknownArgument = scriptArgs.find((argument) => argument !== "--check");
if (unknownArgument) {
  throw new Error(
    `Unknown option ${unknownArgument}; only --check is supported`,
  );
}
const checkOnly = scriptArgs.includes("--check");

async function canonicalDestination(path) {
  let existing = resolve(path);
  const missing = [];
  while (true) {
    try {
      return join(await realpath(existing), ...missing);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      const parent = dirname(existing);
      if (parent === existing) throw error;
      missing.unshift(basename(existing));
      existing = parent;
    }
  }
}

function isSelfOrDescendant(parent, child) {
  const path = relative(parent, child);
  return (
    path === "" ||
    (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`))
  );
}

// Remove harmless path aliases such as `/path/.` without following a final
// symlink. Publication through a symlink is rejected below rather than
// ambiguously replacing the link or mutating its target.
const outDir = resolve(requestedOutDir);
const irOutDir = resolve(requestedIrOutDir);
await assertNoSymlinkInPath(outDir);
await assertNoSymlinkInPath(irOutDir);
const normalizedOutDir = await canonicalDestination(outDir);
const normalizedIrOutDir = await canonicalDestination(irOutDir);
const normalizedDefaultOutDir = await canonicalDestination(defaultOutDir);
const normalizedDefaultIrOutDir = await canonicalDestination(canonicalIrOutDir);
const sourceIsCanonical = normalizedOutDir === normalizedDefaultOutDir;
const irIsCanonical = normalizedIrOutDir === normalizedDefaultIrOutDir;
if (
  isSelfOrDescendant(normalizedOutDir, normalizedIrOutDir) ||
  isSelfOrDescendant(normalizedIrOutDir, normalizedOutDir)
) {
  throw new Error("bundled specs and compiled IR destinations cannot overlap");
}
if (sourceIsCanonical !== irIsCanonical) {
  throw new Error(
    "custom outputs require both destinations to be canonical or distinct custom paths",
  );
}

// Spec namespaces to exclude from the bundle (config.exclude, e.g. ["aws","gcloud","az"]).
// A namespace `ns` drops the top-level `ns` spec and everything under `ns/`. Excluded
// specs are absent from both the files on disk AND the written index.json, so the runtime
// loader never references them, and there is no network fallback to fetch them at runtime.
//
// Current repo default ["aws", "az"]: the AWS and Azure CLI specs are large and
// most users never trigger them. Edit specs.config.json to change.
// Env override BUNDLED_SPECS_EXCLUDE is comma-separated ("" = exclude nothing) and wins.
// Re-run this script after changing the list; build-app.sh also detects the
// changed config through the source manifest and resyncs before Rust builds.
const exclude = (
  process.env.BUNDLED_SPECS_EXCLUDE !== undefined
    ? process.env.BUNDLED_SPECS_EXCLUDE.split(",")
    : (config.exclude ?? [])
)
  .map((s) => s.trim())
  .filter(Boolean);

function isExcluded(name) {
  return exclude.some((ns) => name === ns || name.startsWith(`${ns}/`));
}

function urlFor(path) {
  const encodedPath = path.split("/").map(encodeURIComponent).join("/");
  return new URL(encodedPath, SPEC_BASE_URL);
}

async function fetchBytes(url) {
  let lastError;
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      const response = await fetch(url);
      if (response.ok) {
        return Buffer.from(await response.arrayBuffer());
      }
      if (response.status < 500 && response.status !== 429) {
        throw new Error(
          `Failed to fetch ${url}: ${response.status} ${response.statusText}`,
        );
      }
      lastError = new Error(
        `Failed to fetch ${url}: ${response.status} ${response.statusText}`,
      );
    } catch (err) {
      lastError = err;
    }

    if (attempt < maxAttempts) {
      await new Promise((resolve) => setTimeout(resolve, attempt * 500));
    }
  }
  throw lastError;
}

function assertSafeAssetPath(path) {
  if (
    typeof path !== "string" ||
    !path ||
    path.includes("\0") ||
    path.includes("\\") ||
    isAbsolute(path) ||
    path
      .split("/")
      .some((segment) => !segment || segment === "." || segment === "..")
  ) {
    throw new Error(`unsafe bundled spec asset path: ${path}`);
  }
}

// A relative path from a source tree is untrusted even when it came from
// `relative()`: on POSIX a filename may contain a literal backslash, and
// converting that spelling to `/` first would turn an ordinary filename into
// a different path (possibly an escape).  Validate the raw POSIX spelling
// before doing any platform separator conversion.
function relativeAssetPath(base, full) {
  const raw = relative(base, full);
  if (process.platform !== "win32") {
    assertSafeAssetPath(raw);
    return raw;
  }
  const normalized = raw.split(sep).join("/");
  assertSafeAssetPath(normalized);
  return normalized;
}

function assertPathInsideRoot(root, path, label) {
  const relation = relative(resolve(root), resolve(path));
  if (
    relation === "" ||
    isAbsolute(relation) ||
    relation === ".." ||
    relation.startsWith(`..${sep}`)
  ) {
    throw new Error(`${label} escapes its root: ${path}`);
  }
}

async function validateIgnoredSourceDirectory(dir, base) {
  const rootInfo = await lstat(dir);
  if (rootInfo.isSymbolicLink() || !rootInfo.isDirectory()) {
    throw new Error(
      `bundled specs source tree contains a symlink or special entry: ${dir}`,
    );
  }
  const entries = await readdir(dir, { withFileTypes: true });
  for (const entry of entries) {
    const full = join(dir, entry.name);
    const path = relativeAssetPath(base, full);
    const info = await lstat(full);
    if (info.isDirectory()) {
      await validateIgnoredSourceDirectory(full, base);
    } else if (!info.isFile()) {
      throw new Error(
        `bundled specs source tree contains a symlink or special entry: ${full}`,
      );
    }
  }
}

async function writeAsset(path, bytes, destinationDir = outDir) {
  assertSafeAssetPath(path);
  const destination = join(destinationDir, path);
  assertPathInsideRoot(destinationDir, destination, "bundled asset path");
  // Check both before and after mkdir.  The second check closes the window in
  // which a pre-existing or concurrently inserted symlink could be followed
  // by writeFile through a generated staging path.
  await assertNoSymlinkInPath(destination);
  await mkdir(dirname(destination), { recursive: true });
  await assertNoSymlinkInPath(destination);
  await writeFile(destination, bytes);
}

async function pathExists(path) {
  try {
    // lstat deliberately treats a dangling final symlink as an existing
    // publication target. `stat` would report ENOENT and let rename/cleanup
    // follow an alias or strand a foreign link in a fail-open state.
    await lstat(path);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function createUniqueDirectory(suffix) {
  await mkdir(dirname(outDir), { recursive: true });
  return mkdtemp(join(dirname(outDir), `.${basename(outDir)}.${suffix}-`));
}

function isSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

async function snapshotDirectory(directory) {
  const rootInfo = await lstat(directory);
  if (rootInfo.isSymbolicLink()) {
    throw new Error(
      `refusing to replace bundled specs destination through a symbolic link: ${directory}`,
    );
  }
  if (!rootInfo.isDirectory()) {
    throw new Error(
      `refusing to replace bundled specs destination because it is not a directory: ${directory}`,
    );
  }

  const files = [];
  const directories = [];
  async function walk(current, relativePath = "") {
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((left, right) => comparePath(left.name, right.name));
    for (const entry of entries) {
      const path = relativePath ? `${relativePath}/${entry.name}` : entry.name;
      assertSafeAssetPath(path);
      const full = join(current, entry.name);
      const info = await lstat(full);
      if (info.isDirectory()) {
        directories.push(path);
        await walk(full, path);
      } else if (info.isFile()) {
        files.push({ path, sha256: sha256(await readFile(full)) });
      } else {
        throw new Error(
          `refusing to replace bundled specs destination containing a link or special entry: ${full}`,
        );
      }
    }
  }
  await walk(directory);
  return { files, directories };
}

function snapshotsEqual(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

async function assertSnapshotUnchanged(directory, expected) {
  const actual = await snapshotDirectory(directory);
  if (!snapshotsEqual(actual, expected)) {
    throw new Error(
      `refusing to remove changed bundled specs backup: ${directory}`,
    );
  }
}

async function removeSnapshotDirectory(directory, snapshot) {
  await assertSnapshotUnchanged(directory, snapshot);
  // Delete only paths captured and hashed before publication. A concurrently
  // added entry is never passed to unlink and makes the final rmdir fail,
  // leaving a recoverable backup instead of recursively deleting new data.
  for (const file of snapshot.files) {
    const full = join(directory, file.path);
    if (sha256(await readFile(full)) !== file.sha256) {
      throw new Error(`refusing to remove changed backup file: ${full}`);
    }
    await unlink(full);
  }
  for (const path of [...snapshot.directories].sort(
    (left, right) => right.length - left.length || comparePath(right, left),
  )) {
    await rmdir(join(directory, path));
  }
  await rmdir(directory);
}

async function assertManagedBundleDirectory(directory) {
  const snapshot = await snapshotDirectory(directory);
  if (
    (snapshot.files.length === 0 && snapshot.directories.length === 0) ||
    normalizedOutDir === normalizedDefaultOutDir
  ) {
    return snapshot;
  }

  let manifest;
  try {
    manifest = JSON.parse(
      await readFile(join(directory, SOURCE_MANIFEST_NAME), "utf8"),
    );
  } catch {
    throw new Error(
      `refusing to replace non-empty custom bundled specs destination without a valid ${SOURCE_MANIFEST_NAME}: ${directory}`,
    );
  }

  if (
    manifest?.format !== SOURCE_MANIFEST_VERSION ||
    typeof manifest.source !== "object" ||
    manifest.source === null ||
    typeof manifest.inputs !== "object" ||
    manifest.inputs === null ||
    !isSha256(manifest.sourceTreeSha256) ||
    !Number.isSafeInteger(manifest.sourceFileCount) ||
    manifest.sourceFileCount < 0 ||
    !isSha256(manifest.bundleTreeSha256) ||
    !Number.isSafeInteger(manifest.bundleFileCount) ||
    manifest.bundleFileCount < 0
  ) {
    throw new Error(
      `refusing to replace non-empty custom bundled specs destination with an unrecognized ${SOURCE_MANIFEST_NAME}: ${directory}`,
    );
  }

  const tree = await digestBundle(directory);
  if (
    tree.digest !== manifest.bundleTreeSha256 ||
    tree.count !== manifest.bundleFileCount
  ) {
    throw new Error(
      `refusing to replace modified custom bundled specs destination: ${directory}`,
    );
  }
  const expectedDirectories = new Set();
  for (const file of snapshot.files) {
    const segments = file.path.split("/");
    for (let index = 1; index < segments.length; index += 1) {
      expectedDirectories.add(segments.slice(0, index).join("/"));
    }
  }
  if (
    snapshot.directories.some(
      (directoryPath) => !expectedDirectories.has(directoryPath),
    )
  ) {
    throw new Error(
      `refusing to replace modified custom bundled specs destination containing an untracked empty directory: ${directory}`,
    );
  }
  return snapshot;
}

async function assertManagedIrDirectory(directory) {
  const snapshot = await snapshotDirectory(directory);
  if (snapshot.files.length === 0 && snapshot.directories.length === 0) {
    return snapshot;
  }
  if (resolve(directory) === resolve(canonicalIrOutDir)) {
    try {
      await lstat(join(directory, PAIR_MARKER_NAME));
    } catch (error) {
      if (error?.code === "ENOENT") {
        // Older checkouts have a generated canonical IR tree without the pair
        // marker. Full sync has already compiled and audited the new staged
        // pair before this replacement, so this exact canonical location gets
        // a one-time migration allowance. Unknown/custom IR trees do not.
        return snapshot;
      }
      throw error;
    }
  }

  try {
    // The IR marker is the ownership record for generated resources. A valid
    // old IR can be replaced atomically, while an unknown/custom tree must
    // never be recursively moved or deleted.
    await verifyPair({ irRoot: directory, irOnly: true });
  } catch (error) {
    throw new Error(
      `refusing to replace non-empty compiled IR destination without a valid ${PAIR_MARKER_NAME}: ${directory}`,
      { cause: error },
    );
  }
  return snapshot;
}

async function auditPublishedPair() {
  const { auditSpecsHooks } = await import("./audit-spec-hooks.mjs");
  const report = await auditSpecsHooks({
    sourceRoot: outDir,
    irRoot: irOutDir,
  });
  if (!report.ok) {
    const failures = Object.entries(report.errors)
      .filter(([, entries]) => entries.length)
      .map(([name, entries]) => `${name}=${entries.length}`);
    throw new Error(`Published spec IR audit failed: ${failures.join(", ")}`);
  }
}

async function replaceBundleAndIrDirectories(bundleStage, irStage) {
  if (!(await pathExists(bundleStage)) || !(await pathExists(irStage))) {
    throw new Error("validated bundle or IR staging directory is missing");
  }
  await verifyPair({ sourceRoot: bundleStage, irRoot: irStage });
  if (await pathExists(outDir)) await assertManagedBundleDirectory(outDir);
  if (await pathExists(irOutDir)) await assertManagedIrDirectory(irOutDir);
  // Import the published source from its final path. A spec relying on
  // import.meta.url or relative files may differ from staging; that is a
  // failed transaction, not an accepted mismatch. The shared publisher keeps
  // its journal until this audit and the final pair verification succeed.
  await publishPairDirectories({
    sourceStage: bundleStage,
    irStage,
    sourceCanonical: outDir,
    irCanonical: irOutDir,
    lockPath: pairLockPath,
    verifyPublished: auditPublishedPair,
  });
}

async function runPool(items, task) {
  let next = 0;
  const workers = Array.from(
    { length: Math.min(concurrency, items.length) },
    async () => {
      while (next < items.length) {
        const item = items[next++];
        await task(item);
      }
    },
  );
  await Promise.all(workers);
}

// Recursively list every *.js file under `dir`, returning paths relative to `dir`
// (POSIX separators), skipping the icons/ subtree.
async function walkJs(dir, base = dir) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    const info = await lstat(full);
    const path = relativeAssetPath(base, full);
    if (info.isSymbolicLink() || (!info.isDirectory() && !info.isFile())) {
      throw new Error(
        `bundled specs source tree contains a symlink or special entry: ${full}`,
      );
    }
    if (entry.name === "icons" && !info.isDirectory()) {
      throw new Error(`bundled specs icons entry is not a directory: ${full}`);
    }
    if (info.isDirectory()) {
      if (entry.name === "icons") {
        await validateIgnoredSourceDirectory(full, base);
        continue;
      }
      out.push(...(await walkJs(full, base)));
    } else if (info.isFile()) {
      if (entry.name.endsWith(".js")) out.push(path);
    }
  }
  return out;
}

async function walkFiles(dir, base = dir) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    const info = await lstat(full);
    const path = relativeAssetPath(base, full);
    if (entry.name === SOURCE_MANIFEST_NAME && dir === base) {
      if (info.isSymbolicLink() || !info.isFile()) {
        throw new Error(
          `bundled source manifest is a symlink or special entry: ${full}`,
        );
      }
      continue;
    }
    if (info.isDirectory()) {
      out.push(...(await walkFiles(full, base)));
    } else if (info.isFile()) {
      out.push({ path, full });
    } else {
      throw new Error(`Unsupported non-file bundle entry: ${full}`);
    }
  }
  return out;
}

async function digestAssets(assets) {
  const entries = await Promise.all(
    assets.map(async ({ path, sourcePath, sourceRoot, bytes }) => {
      assertSafeAssetPath(path);
      if (bytes !== undefined) {
        return { path, digest: sha256(bytes) };
      }
      if (sourceRoot) {
        assertPathInsideRoot(sourceRoot, sourcePath, "bundled source asset path");
      }
      const info = await lstat(sourcePath);
      if (!info.isFile() || info.isSymbolicLink()) {
        throw new Error(
          `bundled specs source asset is a symlink or special entry: ${sourcePath}`,
        );
      }
      return { path, digest: sha256(await readFile(sourcePath)) };
    }),
  );
  entries.sort((a, b) => comparePath(a.path, b.path));
  const canonical = entries
    .map(({ path, digest }) => `${path}\0${digest}\n`)
    .join("");
  return { digest: sha256(Buffer.from(canonical)), count: entries.length };
}

async function digestBundle(dir) {
  const files = await walkFiles(dir);
  return digestAssets(
    files.map(({ path, full: sourcePath }) => ({ path, sourcePath })),
  );
}

async function writeBundleManifest({
  source,
  inputs,
  sourceTree,
  bundleTree,
  destinationDir = outDir,
}) {
  const manifest = {
    format: SOURCE_MANIFEST_VERSION,
    source,
    inputs,
    sourceTreeSha256: sourceTree.digest,
    sourceFileCount: sourceTree.count,
    bundleTreeSha256: bundleTree.digest,
    bundleFileCount: bundleTree.count,
  };
  await writeAsset(
    SOURCE_MANIFEST_NAME,
    Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`),
    destinationDir,
  );
}

async function readBundleManifest() {
  try {
    return JSON.parse(
      await readFile(join(outDir, SOURCE_MANIFEST_NAME), "utf8"),
    );
  } catch {
    throw new Error(
      `missing or invalid ${SOURCE_MANIFEST_NAME}; run node scripts/sync-bundled-specs.mjs`,
    );
  }
}

function deriveIndexFromJs(allJs) {
  const diffVersioned = new Set();
  for (const rel of allJs) {
    if (rel.endsWith("/index.js")) diffVersioned.add(dirname(rel));
  }
  const completions = new Set(diffVersioned);
  for (const rel of allJs) {
    const stem = rel.slice(0, -3); // strip .js
    if (stem.split("/").pop() !== "index") completions.add(stem);
  }
  return { completions: [...completions], diffVersioned: [...diffVersioned] };
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function readRootDependencySpecifier(packageName) {
  const sections = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
  ];
  for (const section of sections) {
    const value = rootPackage[section]?.[packageName];
    if (typeof value === "string") return value;
  }
  return undefined;
}

// Only the root importer pins the package shipped by this app. This is a
// deliberately narrow reader for pnpm's importer shape, not a YAML parser:
// a changed lockfile layout must fail closed rather than guess a version from
// another workspace or from the package snapshot section.
function readRootLockPin(lockfile, packageName) {
  const lines = lockfile.split("\n");
  const start = lines.findIndex((line) => line === "  .:");
  if (start < 0) return null;
  let section = false;
  let packageEntry = false;
  const pin = {};
  for (let index = start + 1; index < lines.length; index += 1) {
    const line = lines[index];
    if (/^ {2}\S/.test(line)) break;
    if (
      /^ {4}(?:dependencies|devDependencies|optionalDependencies|peerDependencies):$/.test(
        line,
      )
    ) {
      section = true;
      packageEntry = false;
      continue;
    }
    if (/^ {4}\S/.test(line)) {
      section = false;
      packageEntry = false;
      continue;
    }
    if (/^ {6}\S/.test(line)) {
      const key = line
        .slice(6)
        .replace(/:$/, "")
        .replace(/^['"]|['"]$/g, "");
      packageEntry = section && key === packageName;
      continue;
    }
    if (!packageEntry) continue;
    const field = line.match(/^ {8}(specifier|version):\s*(.+)$/);
    if (field) pin[field[1]] = field[2].trim().replace(/^['"]|['"]$/g, "");
  }
  return pin.specifier && pin.version ? pin : null;
}

async function readInputSnapshot(dependency) {
  const [packageBytes, lockBytes, configBytes] = await Promise.all([
    readFile(rootPackagePath),
    readFile(lockfilePath).catch(() => undefined),
    readFile(configPath),
  ]);
  return {
    packageJsonSha256: sha256(packageBytes),
    pnpmLockSha256: lockBytes ? sha256(lockBytes) : null,
    specsConfigSha256: sha256(configBytes),
    dependency: dependency ?? null,
    exclude,
    icons: iconNames,
  };
}

async function dependencySnapshot(installedVersion, { required = false } = {}) {
  const declaredSpecifier = readRootDependencySpecifier(SPECS_PACKAGE);
  const lockfile = await readFile(lockfilePath, "utf8").catch(() => null);
  const lockPin = lockfile ? readRootLockPin(lockfile, SPECS_PACKAGE) : null;

  if (required && !declaredSpecifier) {
    throw new Error(
      `${SPECS_PACKAGE} must be declared in the root package.json when using the dependency source`,
    );
  }
  if (required && !lockPin) {
    throw new Error(
      `${SPECS_PACKAGE} must have a root importer pin in pnpm-lock.yaml`,
    );
  }
  // The checked-in dependency is deliberately an exact version. A range here
  // would allow the installed package (and therefore the bundle) to drift.
  if (required && declaredSpecifier !== installedVersion) {
    throw new Error(
      `${SPECS_PACKAGE} is ${declaredSpecifier} in package.json but ${installedVersion} is installed`,
    );
  }
  if (required && lockPin.specifier !== declaredSpecifier) {
    throw new Error(
      `${SPECS_PACKAGE} root lock specifier ${lockPin.specifier} does not match package.json ${declaredSpecifier}`,
    );
  }
  if (
    required &&
    lockPin.version !== installedVersion &&
    !lockPin.version.startsWith(`${installedVersion}(`)
  ) {
    throw new Error(
      `${SPECS_PACKAGE} root lock version ${lockPin.version} does not match installed ${installedVersion}`,
    );
  }
  return {
    specifier: declaredSpecifier ?? null,
    lockVersion: lockPin?.version ?? null,
  };
}

function registryPackageUrl(registry, packageName) {
  const base = registry.endsWith("/") ? registry : `${registry}/`;
  return new URL(encodeURIComponent(packageName), base);
}

async function findPackageRoot(entryPath) {
  let current = dirname(entryPath);
  while (current !== dirname(current)) {
    try {
      const pkg = JSON.parse(
        await readFile(join(current, "package.json"), "utf8"),
      );
      if (pkg.name === SPECS_PACKAGE) {
        return { packageRoot: current, version: pkg.version };
      }
    } catch {
      // Keep walking up until we find the package root.
    }
    current = dirname(current);
  }

  throw new Error(`Unable to locate package root for ${SPECS_PACKAGE}`);
}

async function resolveInstalledPackage() {
  let entryPath;
  try {
    entryPath = require.resolve(SPECS_PACKAGE, { paths: [repoDir] });
  } catch (err) {
    throw new Error(
      `Unable to resolve ${SPECS_PACKAGE}. Run \`pnpm install\` or add it to devDependencies.`,
      { cause: err },
    );
  }

  const resolved = await findPackageRoot(entryPath);
  return { ...resolved, entryPath };
}

async function resolveNpmPackage() {
  if (packageTarballUrl) {
    return {
      version: SPECS_VERSION,
      tarball: packageTarballUrl,
      shasum: undefined,
    };
  }

  if (!SPECS_PACKAGE) {
    throw new Error("Missing specs package name");
  }
  if (!SPECS_VERSION) {
    throw new Error(
      "Missing specs package version. Set BUNDLED_SPECS_VERSION or use the default dependency source.",
    );
  }

  const metadata = JSON.parse(
    (
      await fetchBytes(registryPackageUrl(SPECS_NPM_REGISTRY, SPECS_PACKAGE))
    ).toString(),
  );
  const version =
    SPECS_VERSION === "latest" ? metadata["dist-tags"]?.latest : SPECS_VERSION;
  const release = metadata.versions?.[version];
  if (!release?.dist?.tarball) {
    throw new Error(`Unable to resolve ${SPECS_PACKAGE}@${SPECS_VERSION}`);
  }

  return {
    version,
    tarball: release.dist.tarball,
    shasum: release.dist.shasum,
  };
}

async function packageBundlePlan(packageRoot) {
  // npm package layout is build/<...>.js plus icons/<name>.png.
  await assertNoSymlinkInPath(packageRoot);
  const packageRootInfo = await lstat(packageRoot);
  if (!packageRootInfo.isDirectory() || packageRootInfo.isSymbolicLink()) {
    throw new Error(
      `installed specs package root is a symlink or special entry: ${packageRoot}`,
    );
  }
  const specsRoot = join(packageRoot, "build");
  const allJs = await walkJs(specsRoot); // relative paths, e.g. "aws/ec2.js", "az/index.js"

  // Derive index.json from the files we bundle. This intentionally does not trust
  // the package's build/index.json because package entrypoints such as dynamic/index.js
  // may exist in the tarball without being listed there.
  const { completions, diffVersioned } = deriveIndexFromJs(allJs);

  // Apply the namespace exclusion to names and to the files we copy. Also drop the
  // package's root-level `index.js` (the compiler's aggregate barrel, not a real spec).
  const keepJs = allJs.filter(
    (rel) => rel !== "index.js" && !isExcluded(rel.slice(0, -3)),
  );
  const keptCompletions = completions.filter((n) => !isExcluded(n)).sort();
  const keptDiff = diffVersioned.filter((n) => !isExcluded(n)).sort();
  if (keepJs.length === 0 || keptCompletions.length === 0) {
    throw new Error(
      `Installed ${SPECS_PACKAGE} has no bundled completions after filtering; keeping the previous bundle`,
    );
  }
  const excludedCount = completions.length - keptCompletions.length;
  const indexBytes = Buffer.from(
    JSON.stringify({
      completions: keptCompletions,
      diffVersionedCompletions: keptDiff,
    }),
  );

  // Icons are part of the source tree fingerprint as well as the copied
  // bundle. Keep the same selection rules as the historical sync path.
  const wantedIcons = new Set(iconNames);
  const iconsRoot = join(packageRoot, "icons");
  const iconFiles = [];
  try {
    const iconsRootInfo = await lstat(iconsRoot);
    if (iconsRootInfo.isSymbolicLink() || !iconsRootInfo.isDirectory()) {
      throw new Error(
        `installed specs icons tree is a symlink or special entry: ${iconsRoot}`,
      );
    }
    for (const entry of await readdir(iconsRoot, { withFileTypes: true })) {
      const sourcePath = join(iconsRoot, entry.name);
      const info = await lstat(sourcePath);
      if (info.isSymbolicLink() || !info.isFile()) {
        throw new Error(
          `installed specs icon tree contains a symlink or special entry: ${sourcePath}`,
        );
      }
      assertSafeAssetPath(`icons/${entry.name}`);
      if (!entry.name.endsWith(".png")) continue;
      if (
        wantedIcons.size &&
        !wantedIcons.has(entry.name.replace(/\.png$/, ""))
      ) {
        continue;
      }
      iconFiles.push({
        name: entry.name,
        sourcePath,
      });
    }
  } catch (error) {
    if (error?.code === "ENOENT") {
      process.stdout.write("warning: no icons/ in package\n");
    } else {
      throw error;
    }
  }
  if (wantedIcons.size) {
    const availableIcons = new Set(
      iconFiles.map(({ name }) => name.replace(/\.png$/, "")),
    );
    const missingIcons = [...wantedIcons].filter(
      (name) => !availableIcons.has(name),
    );
    if (missingIcons.length) {
      throw new Error(
        `Installed ${SPECS_PACKAGE} is missing required icons: ${missingIcons.join(", ")}; keeping the previous bundle`,
      );
    }
  }

  const assets = [
    { path: "index.json", bytes: indexBytes },
    ...keepJs.map((rel) => ({
      path: rel,
      sourcePath: join(specsRoot, rel),
      sourceRoot: specsRoot,
    })),
    ...iconFiles.map(({ name, sourcePath }) => ({
      path: `icons/${name}`,
      sourcePath,
      sourceRoot: iconsRoot,
    })),
  ];

  return {
    specsRoot,
    keepJs,
    keptCompletions,
    keptDiff,
    excludedCount,
    indexBytes,
    iconFiles,
    assets,
  };
}

async function preflightSpecIr(stagingDir) {
  // Compile and audit the replacement before touching either published tree.
  // An invalid upstream spec must never replace the last usable pair first.
  // Keep IR staging beside canonical IR so the final rename stays on one
  // filesystem. A separate /tmp volume would make publication fail EXDEV.
  await mkdir(dirname(irOutDir), { recursive: true });
  const validationRoot = await mkdtemp(
    join(dirname(irOutDir), `.${basename(irOutDir)}.preflight-`),
  );
  const validationIr = join(validationRoot, "specs-ir");
  try {
    const { compileSpecsIr } = await import("./compile-spec-ir.mjs");
    await compileSpecsIr({
      srcDir: stagingDir,
      outDir: validationIr,
    });
    const { auditSpecsHooks } = await import("./audit-spec-hooks.mjs");
    const report = await auditSpecsHooks({
      sourceRoot: stagingDir,
      irRoot: validationIr,
    });
    if (!report.ok) {
      const failures = Object.entries(report.errors)
        .filter(([, entries]) => entries.length)
        .map(([name, entries]) => `${name}=${entries.length}`);
      throw new Error(`Spec IR preflight audit failed: ${failures.join(", ")}`);
    }
    return { validationRoot, validationIr };
  } catch (error) {
    await rm(validationRoot, { recursive: true, force: true });
    throw error;
  }
}

async function syncFromPackageRoot(packageRoot, label, source = {}) {
  process.stdout.write(`Bundling ${label}\n`);

  const plan = await packageBundlePlan(packageRoot);
  const packageJsonPath = join(packageRoot, "package.json");
  const packageJsonInfo = await lstat(packageJsonPath);
  if (packageJsonInfo.isSymbolicLink() || !packageJsonInfo.isFile()) {
    throw new Error(
      `installed specs package metadata is a symlink or special entry: ${packageJsonPath}`,
    );
  }
  const packageJsonBytes = await readFile(packageJsonPath);
  const packageJsonSha256 = sha256(packageJsonBytes);
  const packageVersion = JSON.parse(packageJsonBytes).version;
  const sourceVersion = source.version ?? packageVersion;
  const dependency =
    source.mode === "dependency" && dependencyPinRequired()
      ? await dependencySnapshot(sourceVersion, { required: true })
      : await dependencySnapshot(sourceVersion);
  const inputs = await readInputSnapshot(dependency);
  const expectedTree = await digestAssets(plan.assets);
  const stageDir = await createUniqueDirectory("staging");

  let committed = false;
  let validation;
  try {
    // index.json
    await writeAsset("index.json", plan.indexBytes, stageDir);

    // spec files
    let copied = 0;
    await runPool(plan.keepJs, async (rel) => {
      assertSafeAssetPath(rel);
      const dest = join(stageDir, rel);
      assertPathInsideRoot(stageDir, dest, "bundled spec copy path");
      const sourcePath = join(plan.specsRoot, rel);
      assertPathInsideRoot(plan.specsRoot, sourcePath, "bundled spec source path");
      const sourceInfo = await lstat(sourcePath);
      if (sourceInfo.isSymbolicLink() || !sourceInfo.isFile()) {
        throw new Error(
          `installed specs source file is a symlink or special entry: ${sourcePath}`,
        );
      }
      await assertNoSymlinkInPath(dest);
      await mkdir(dirname(dest), { recursive: true });
      await assertNoSymlinkInPath(dest);
      await cp(sourcePath, dest);
      copied += 1;
      if (copied % 200 === 0 || copied === plan.keepJs.length) {
        process.stdout.write(
          `Copied ${copied}/${plan.keepJs.length} spec files\n`,
        );
      }
    });

    // icons (only those the app references, if present in the archive)
    let icons = 0;
    await runPool(plan.iconFiles, async ({ name, sourcePath }) => {
      const rel = `icons/${name}`;
      assertSafeAssetPath(rel);
      const dest = join(stageDir, rel);
      assertPathInsideRoot(stageDir, dest, "bundled icon copy path");
      const sourceInfo = await lstat(sourcePath);
      if (sourceInfo.isSymbolicLink() || !sourceInfo.isFile()) {
        throw new Error(
          `installed specs icon is a symlink or special entry: ${sourcePath}`,
        );
      }
      await assertNoSymlinkInPath(dest);
      await mkdir(dirname(dest), { recursive: true });
      await assertNoSymlinkInPath(dest);
      await cp(sourcePath, dest);
      icons += 1;
    });

    if (exclude.length) {
      process.stdout.write(
        `Excluding [${exclude.join(", ")}] — dropped ${plan.excludedCount} spec entries\n`,
      );
    }
    process.stdout.write(
      `Bundled ${plan.keptCompletions.length} specs, ${plan.keptDiff.length} diff indexes, and ${icons} icons into ${outDir}\n`,
    );

    const actualTree = await digestBundle(stageDir);
    if (actualTree.digest !== expectedTree.digest) {
      throw new Error(
        `bundle copy verification failed: expected ${expectedTree.digest}, got ${actualTree.digest}`,
      );
    }

    await writeBundleManifest({
      source: {
        mode: source.mode ?? sourceMode,
        package: SPECS_PACKAGE,
        version: sourceVersion,
        packageJsonSha256,
        ...(source.details ?? {}),
      },
      inputs,
      sourceTree: expectedTree,
      bundleTree: actualTree,
      destinationDir: stageDir,
    });
    validation = await preflightSpecIr(stageDir);
    await replaceBundleAndIrDirectories(stageDir, validation.validationIr);
    committed = true;
  } finally {
    // A callback-backed publication may retain its IR stage and journal after
    // a final-audit failure.  That validation root is then journal-owned and
    // must survive for the next locked recovery; removing it here would turn
    // a recoverable complete stage into an unexplained partial transaction.
    if (validation && !(await pairJournalExists(pairLockPath))) {
      await rm(validation.validationRoot, { recursive: true, force: true });
    }
    if (
      !committed &&
      !(await pairJournalExists(pairLockPath)) &&
      (await pathExists(stageDir))
    ) {
      await rm(stageDir, { force: true, recursive: true });
    }
  }
}

// ── Mode A: read the installed npm dependency and assemble bundle/specs ───────
async function syncFromInstalledDependency() {
  const resolved = await resolveInstalledPackage();
  const requestedVersion = SPECS_VERSION;
  if (
    requestedVersion &&
    requestedVersion !== "latest" &&
    requestedVersion !== resolved.version
  ) {
    throw new Error(
      `${SPECS_PACKAGE} requested version ${requestedVersion} but ${resolved.version} is installed`,
    );
  }
  await syncFromPackageRoot(
    resolved.packageRoot,
    `${SPECS_PACKAGE}@${resolved.version} from ${resolved.packageRoot}`,
    { mode: "dependency", version: resolved.version },
  );
}

// ── Mode B (legacy): download the npm package tarball explicitly ──────────────
async function assertSafeNpmArchive(tarPath) {
  const [{ stdout: names }, { stdout: details }] = await Promise.all([
    execFileAsync("tar", ["-tzf", tarPath]),
    execFileAsync("tar", ["-tvzf", tarPath]),
  ]);
  const entries = names.split(/\r?\n/).filter(Boolean);
  const listing = details.split(/\r?\n/).filter(Boolean);
  if (!entries.length || entries.length !== listing.length) {
    throw new Error(
      "npm specs tarball has an empty or inconsistent file listing",
    );
  }
  for (const [index, entry] of entries.entries()) {
    const normalized = entry.replace(/\/$/, "");
    if (normalized !== "package" && !normalized.startsWith("package/")) {
      throw new Error(`npm specs tarball has an unsafe entry: ${entry}`);
    }
    assertSafeAssetPath(normalized);
    const type = listing[index][0];
    if (type !== "-" && type !== "d") {
      throw new Error(
        `npm specs tarball contains an unsupported link or special entry: ${entry}`,
      );
    }
  }
}

async function syncFromNpmPackage() {
  const resolved = await resolveNpmPackage();
  process.stdout.write(
    `Downloading ${SPECS_PACKAGE}@${resolved.version} from ${resolved.tarball}\n`,
  );
  const packageBytes = await fetchBytes(resolved.tarball);
  if (resolved.shasum) {
    const actual = createHash("sha1").update(packageBytes).digest("hex");
    if (actual !== resolved.shasum) {
      throw new Error(
        `Integrity check failed for ${SPECS_PACKAGE}@${resolved.version}: expected ${resolved.shasum}, got ${actual}`,
      );
    }
  }

  const work = await mkdtemp(join(tmpdir(), "ec-specs-"));
  const tarPath = join(work, "package.tgz");
  try {
    await writeFile(tarPath, packageBytes);
    await assertSafeNpmArchive(tarPath);
    await execFileAsync("tar", ["-xzf", tarPath, "-C", work]);
    await syncFromPackageRoot(
      join(work, "package"),
      `${SPECS_PACKAGE}@${resolved.version} from ${resolved.tarball}`,
      {
        mode: "npm",
        version: resolved.version,
        details: { shasum: resolved.shasum ?? null },
      },
    );
  } finally {
    await rm(work, { force: true, recursive: true });
  }
}

// ── Mode C (legacy): fetch index.json + each spec file from the per-file CDN ──
async function syncFromCdn() {
  const index = JSON.parse((await fetchBytes(urlFor("index.json"))).toString());
  const allCompletions = Array.isArray(index.completions)
    ? index.completions
    : [];
  const allDiffVersioned = Array.isArray(index.diffVersionedCompletions)
    ? index.diffVersionedCompletions
    : [];

  const completions = allCompletions.filter((name) => !isExcluded(name));
  const diffVersioned = allDiffVersioned.filter((name) => !isExcluded(name));
  for (const name of [...completions, ...diffVersioned]) {
    assertSafeAssetPath(`${name}.js`);
  }
  if (completions.length === 0) {
    throw new Error(
      "CDN source has no bundled completions after filtering; keeping the previous bundle",
    );
  }
  const excludedCount =
    allCompletions.length -
    completions.length +
    (allDiffVersioned.length - diffVersioned.length);
  const diffVersionedSet = new Set(diffVersioned);

  const filteredIndex = {
    ...index,
    completions,
    diffVersionedCompletions: diffVersioned,
  };

  const files = [
    ...completions
      .filter((name) => !diffVersionedSet.has(name))
      .map((name) => `${name}.js`),
    ...diffVersioned.map((name) => `${name}/index.js`),
    ...iconNames.map((name) => `icons/${name}.png`),
  ];

  // Resolve local inputs before touching the current bundle. Network failures
  // and invalid repository state must leave the previous tree usable.
  const inputs = await readInputSnapshot(await dependencySnapshot(undefined));
  const stageDir = await createUniqueDirectory("staging");

  let committed = false;
  let validation;
  try {
    await writeAsset(
      "index.json",
      Buffer.from(JSON.stringify(filteredIndex)),
      stageDir,
    );
    if (exclude.length) {
      process.stdout.write(
        `Excluding [${exclude.join(", ")}] — dropped ${excludedCount} spec entries\n`,
      );
    }

    let completed = 0;
    await runPool(files, async (path) => {
      await writeAsset(path, await fetchBytes(urlFor(path)), stageDir);
      completed += 1;
      if (completed % 100 === 0 || completed === files.length) {
        process.stdout.write(
          `Synced ${completed}/${files.length} bundled spec assets\n`,
        );
      }
    });

    const tree = await digestBundle(stageDir);
    await writeBundleManifest({
      source: {
        mode: "cdn",
        package: null,
        version: null,
        baseUrl: SPEC_BASE_URL,
      },
      inputs,
      sourceTree: tree,
      bundleTree: tree,
      destinationDir: stageDir,
    });
    validation = await preflightSpecIr(stageDir);
    await replaceBundleAndIrDirectories(stageDir, validation.validationIr);
    committed = true;
  } finally {
    if (validation && !(await pairJournalExists(pairLockPath))) {
      await rm(validation.validationRoot, { recursive: true, force: true });
    }
    if (
      !committed &&
      !(await pairJournalExists(pairLockPath)) &&
      (await pathExists(stageDir))
    ) {
      await rm(stageDir, { force: true, recursive: true });
    }
  }

  process.stdout.write(
    `Bundled ${completions.length} specs, ${diffVersioned.length} diff indexes, and ${iconNames.length} icons into ${outDir}\n`,
  );
}

function dependencyPinRequired() {
  // An explicitly overridden package is intentionally a one-off source and
  // need not also be a root dependency. The default source must be pinned by
  // the root manifest and lockfile.
  return sourceMode === "dependency" && !process.env.BUNDLED_SPECS_PACKAGE;
}

async function checkInstalledDependencyFreshness() {
  if (sourceMode !== "dependency") {
    throw new Error(
      `--check only validates the installed dependency source; ${sourceMode} is an explicit sync override`,
    );
  }

  const manifest = await readBundleManifest();
  if (manifest.format !== SOURCE_MANIFEST_VERSION) {
    throw new Error(
      `${SOURCE_MANIFEST_NAME} format ${manifest.format ?? "unknown"} is not supported`,
    );
  }

  const resolved = await resolveInstalledPackage();
  if (
    SPECS_VERSION &&
    SPECS_VERSION !== "latest" &&
    SPECS_VERSION !== resolved.version
  ) {
    throw new Error(
      `${SPECS_PACKAGE} requested version ${SPECS_VERSION} but ${resolved.version} is installed`,
    );
  }
  const dependency = await dependencySnapshot(resolved.version, {
    required: dependencyPinRequired(),
  });
  const inputs = await readInputSnapshot(dependency);
  const plan = await packageBundlePlan(resolved.packageRoot);
  const sourceTree = await digestAssets(plan.assets);
  const bundleTree = await digestBundle(outDir);
  const sourcePackageJsonPath = join(resolved.packageRoot, "package.json");
  const sourcePackageJsonInfo = await lstat(sourcePackageJsonPath);
  if (sourcePackageJsonInfo.isSymbolicLink() || !sourcePackageJsonInfo.isFile()) {
    throw new Error(
      `installed specs package metadata is a symlink or special entry: ${sourcePackageJsonPath}`,
    );
  }
  const sourcePackageJsonSha256 = sha256(
    await readFile(sourcePackageJsonPath),
  );

  const mismatches = [];
  if (manifest.source?.mode !== "dependency") {
    mismatches.push("source mode");
  }
  if (manifest.source?.package !== SPECS_PACKAGE) {
    mismatches.push("source package");
  }
  if (manifest.source?.version !== resolved.version) {
    mismatches.push("source package version");
  }
  if (manifest.source?.packageJsonSha256 !== sourcePackageJsonSha256) {
    mismatches.push("source package contents");
  }
  for (const field of [
    "packageJsonSha256",
    "pnpmLockSha256",
    "specsConfigSha256",
  ]) {
    if (manifest.inputs?.[field] !== inputs[field]) {
      mismatches.push(field);
    }
  }
  if (
    JSON.stringify(manifest.inputs?.dependency ?? null) !==
    JSON.stringify(inputs.dependency)
  ) {
    mismatches.push("dependency pin");
  }
  if (JSON.stringify(manifest.inputs?.exclude) !== JSON.stringify(exclude)) {
    mismatches.push("exclude config");
  }
  if (JSON.stringify(manifest.inputs?.icons) !== JSON.stringify(iconNames)) {
    mismatches.push("icon config");
  }
  if (manifest.sourceTreeSha256 !== sourceTree.digest) {
    mismatches.push("source tree");
  }
  if (manifest.sourceFileCount !== sourceTree.count) {
    mismatches.push("source file count");
  }
  if (manifest.bundleTreeSha256 !== bundleTree.digest) {
    mismatches.push("bundle contents");
  }
  if (manifest.bundleFileCount !== bundleTree.count) {
    mismatches.push("bundle file count");
  }
  if (sourceTree.digest !== bundleTree.digest) {
    mismatches.push("source/bundle tree mismatch");
  }
  if (mismatches.length) {
    throw new Error(`bundle/specs is stale (${mismatches.join(", ")})`);
  }

  // Source freshness does not require a compiled IR tree. CI checkouts
  // gitignore bundle/specs-ir; compile-spec-ir / spec-pair verify the pair
  // after they write it. A leftover IR that does exist must still match.
  if (await pathExists(irOutDir)) {
    await verifyPair({ sourceRoot: outDir, irRoot: irOutDir });
  }

  process.stdout.write(
    `Bundled specs are fresh: ${SPECS_PACKAGE}@${resolved.version}, ${bundleTree.count} files\n`,
  );
}

await withPairLock(
  pairLockPath,
  async () => {
    if (checkOnly) {
      try {
        await checkInstalledDependencyFreshness();
      } catch (error) {
        process.stderr.write(
          `Bundled specs freshness check failed: ${error instanceof Error ? error.message : error}\n`,
        );
        process.exitCode = 1;
      }
    } else {
      if (sourceMode === "dependency") {
        await syncFromInstalledDependency();
      } else if (sourceMode === "npm") {
        await syncFromNpmPackage();
      } else if (sourceMode === "cdn") {
        await syncFromCdn();
      } else {
        throw new Error(`Unsupported BUNDLED_SPECS_SOURCE: ${sourceMode}`);
      }
    }
  },
  { verifyPublished: auditPublishedPair },
);
