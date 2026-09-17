/**
 * Shared publication contract for the bundled source/IR pair.
 *
 * `bundle/specs` and `bundle/specs-ir` cannot be renamed as one filesystem
 * object.  The IR marker therefore commits the digest of both trees.  A
 * reader accepts a pair only when the marker and both on-disk trees agree;
 * during a two-directory publication every intermediate state is rejected.
 *
 * This module deliberately has no project dependencies.  It is imported by
 * the compiler, audit, sync script, and their isolated fixture copies.
 */
import { AsyncLocalStorage } from "node:async_hooks";
import { createHash, randomUUID } from "node:crypto";
import {
  cp,
  link,
  mkdir,
  mkdtemp,
  open,
  lstat,
  readFile,
  readdir,
  realpath,
  rename,
  rmdir,
  rm,
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

export const PAIR_MARKER_NAME = ".spec-pair.json";
export const PAIR_FORMAT = 1;
export const PAIR_KIND = "easy-complete-spec-pair";
export const SOURCE_MANIFEST_NAME = ".source-manifest.json";
export const PAIR_LOCK_NAME = ".spec-pair.lock";
export const PAIR_JOURNAL_NAME = ".spec-pair.journal.json";
export const PAIR_JOURNAL_SCHEMA = "easy-complete-spec-pair-journal";
export const PAIR_JOURNAL_VERSION = 1;

const LOCK_CANDIDATE_SUFFIX = ".new.";
const LOCK_STALE_SUFFIX = ".stale.";
const pairLockContext = new AsyncLocalStorage();
const pairLockStates = new WeakMap();

const DIRECTORY_TOPOLOGY_SHA256_FIELD = "directoryTopologySha256";
const TREE_IDENTITY_FIELD = "treeIdentity";

export function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

// Do not use localeCompare or JS's UTF-16 `<` here. The pair digest is a
// cross-language contract and Rust orders `String` by UTF-8 bytes. Buffer
// comparison keeps supplementary characters and BMP code points aligned.
export function comparePath(left, right) {
  return Buffer.compare(Buffer.from(left), Buffer.from(right));
}

// macOS exposes these two stable compatibility aliases. They are not user
// supplied publication parents; allowing only the exact, verified aliases
// keeps temporary fixture paths usable while a custom symlink remains fatal.
async function isKnownSystemAlias(path) {
  const target =
    process.platform === "darwin"
      ? { "/var": "/private/var", "/tmp": "/private/tmp" }[path]
      : undefined;
  if (!target) return false;
  try {
    return (await realpath(path)) === target;
  } catch {
    return false;
  }
}

export async function assertNoSymlinkInPath(path) {
  const absolute = resolve(path);
  let current = absolute.startsWith("/") ? "/" : "";
  const components = absolute.split("/").filter(Boolean);
  for (const component of components) {
    current = current ? join(current, component) : component;
    try {
      const info = await lstat(current);
      if (info.isSymbolicLink() && !(await isKnownSystemAlias(current))) {
        throw new Error(
          `refusing to replace bundled specs destination through a symbolic link: ${current}`,
        );
      }
    } catch (error) {
      if (error?.code === "ENOENT") {
        // The remaining components cannot contain an existing symlink if this
        // component is absent. Keep the eventual mkdir/rename path private.
        break;
      }
      throw error;
    }
  }
}

export function isSha256(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

function isNonNegativeSafeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

const ORDINARY_NON_NEGATIVE_INTEGER_TOKEN = /^(?:0|[1-9][0-9]*)$/;

function assertNoDuplicateJsonObjectKeys(source, label) {
  let offset = 0;
  const skipWhitespace = () => {
    while (/\s/.test(source[offset] ?? "")) offset += 1;
  };
  const readString = () => {
    const start = offset;
    offset += 1;
    while (offset < source.length) {
      if (source[offset] === "\\") {
        offset += 2;
        continue;
      }
      if (source[offset] === '"') {
        offset += 1;
        return JSON.parse(source.slice(start, offset));
      }
      offset += 1;
    }
    throw new SyntaxError(`unterminated string in ${label}`);
  };
  const readValue = () => {
    skipWhitespace();
    if (source[offset] === "{") {
      offset += 1;
      skipWhitespace();
      const keys = new Set();
      if (source[offset] === "}") {
        offset += 1;
        return;
      }
      while (offset < source.length) {
        if (source[offset] !== '"') {
          throw new SyntaxError(`expected object key in ${label}`);
        }
        const key = readString();
        if (keys.has(key)) {
          throw new Error(`${label} contains duplicate object key ${JSON.stringify(key)}`);
        }
        keys.add(key);
        skipWhitespace();
        if (source[offset] !== ":") {
          throw new SyntaxError(`expected colon in ${label}`);
        }
        offset += 1;
        readValue();
        skipWhitespace();
        if (source[offset] === "}") {
          offset += 1;
          return;
        }
        if (source[offset] !== ",") {
          throw new SyntaxError(`expected object separator in ${label}`);
        }
        offset += 1;
        skipWhitespace();
      }
      throw new SyntaxError(`unterminated object in ${label}`);
    }
    if (source[offset] === "[") {
      offset += 1;
      skipWhitespace();
      if (source[offset] === "]") {
        offset += 1;
        return;
      }
      while (offset < source.length) {
        readValue();
        skipWhitespace();
        if (source[offset] === "]") {
          offset += 1;
          return;
        }
        if (source[offset] !== ",") {
          throw new SyntaxError(`expected array separator in ${label}`);
        }
        offset += 1;
      }
      throw new SyntaxError(`unterminated array in ${label}`);
    }
    if (source[offset] === '"') {
      readString();
      return;
    }
    const start = offset;
    while (
      offset < source.length &&
      !/[\s,\]}]/.test(source[offset])
    ) {
      offset += 1;
    }
    if (offset === start) throw new SyntaxError(`expected value in ${label}`);
  };

  readValue();
  skipWhitespace();
  if (offset !== source.length) {
    throw new SyntaxError(`unexpected trailing data in ${label}`);
  }
}

function assertOrdinaryIntegerToken(key, source) {
  if (!ORDINARY_NON_NEGATIVE_INTEGER_TOKEN.test(source)) {
    throw new Error(
      `pair marker ${key} must use an ordinary non-negative decimal integer token`,
    );
  }
}

/**
 * Parse a marker while retaining the source spelling of numeric fields.
 * JSON.parse normally turns `1e0` into the same Number as `1`; the Rust
 * reader intentionally accepts only ordinary decimal integer tokens, so the
 * lexical form has to be checked before the value is validated.
 */
export function parsePairMarker(text) {
  let value;
  try {
    assertNoDuplicateJsonObjectKeys(text, "pair marker");
    value = JSON.parse(text, (key, current, context) => {
      if (key === "format" || key === "fileCount") {
        if (!context || typeof context.source !== "string") {
          throw new Error(
            "pair marker numeric token source is unavailable in this Node.js runtime",
          );
        }
        assertOrdinaryIntegerToken(key, context.source);
      }
      return current;
    });
  } catch (error) {
    if (error instanceof SyntaxError) {
      throw new Error("invalid .spec-pair.json");
    }
    throw error;
  }
  return validatePairMarker(value);
}

function markerValues(marker) {
  return [
    marker.source.treeSha256,
    marker.source.fileCount,
    marker.source.manifestSha256 ?? null,
    marker.ir.treeSha256,
    marker.ir.fileCount,
  ];
}

function canonicalPairDigest(marker) {
  const [
    sourceTreeSha256,
    sourceFileCount,
    sourceManifestSha256,
    irTreeSha256,
    irFileCount,
  ] = markerValues(marker);
  return [
    `format=${PAIR_FORMAT}`,
    `kind=${PAIR_KIND}`,
    `source.treeSha256=${sourceTreeSha256}`,
    `source.fileCount=${sourceFileCount}`,
    `source.manifestSha256=${sourceManifestSha256 ?? ""}`,
    `ir.treeSha256=${irTreeSha256}`,
    `ir.fileCount=${irFileCount}`,
    "",
  ].join("\n");
}

function assertKnownFields(object, fields, label) {
  if (!object || typeof object !== "object" || Array.isArray(object)) {
    throw new Error(`${label} must be an object`);
  }
  const allowed = new Set(fields);
  for (const field of Object.keys(object)) {
    if (!allowed.has(field)) {
      throw new Error(`${label} contains unknown field ${field}`);
    }
  }
}

export function validatePairMarker(value) {
  assertKnownFields(
    value,
    ["format", "kind", "source", "ir", "pairSha256"],
    "pair marker",
  );
  if (value.format !== PAIR_FORMAT) {
    throw new Error(
      `pair marker format ${value.format ?? "unknown"} is unsupported`,
    );
  }
  if (value.kind !== PAIR_KIND) {
    throw new Error(
      `pair marker kind ${value.kind ?? "unknown"} is unsupported`,
    );
  }
  assertKnownFields(
    value.source,
    ["treeSha256", "fileCount", "manifestSha256"],
    "pair marker source",
  );
  assertKnownFields(value.ir, ["treeSha256", "fileCount"], "pair marker ir");
  if (!isSha256(value.source.treeSha256)) {
    throw new Error(
      "pair marker source.treeSha256 must be a lowercase SHA-256 digest",
    );
  }
  if (!isNonNegativeSafeInteger(value.source.fileCount)) {
    throw new Error(
      "pair marker source.fileCount must be a non-negative safe integer",
    );
  }
  if (
    value.source.manifestSha256 !== null &&
    !isSha256(value.source.manifestSha256)
  ) {
    throw new Error(
      "pair marker source.manifestSha256 must be null or a lowercase SHA-256 digest",
    );
  }
  if (!isSha256(value.ir.treeSha256)) {
    throw new Error(
      "pair marker ir.treeSha256 must be a lowercase SHA-256 digest",
    );
  }
  if (!isNonNegativeSafeInteger(value.ir.fileCount)) {
    throw new Error(
      "pair marker ir.fileCount must be a non-negative safe integer",
    );
  }
  if (!isSha256(value.pairSha256)) {
    throw new Error(
      "pair marker pairSha256 must be a lowercase SHA-256 digest",
    );
  }
  if (sha256(canonicalPairDigest(value)) !== value.pairSha256) {
    throw new Error("pair marker pairSha256 does not match its contents");
  }
  return value;
}

async function digestTreeWalk(
  root,
  excludedRootNames,
  current,
  files,
  directories = null,
) {
  const entries = await readdir(current, { withFileTypes: true });
  entries.sort((left, right) => comparePath(left.name, right.name));
  for (const entry of entries) {
    if (current === root && excludedRootNames.has(entry.name)) continue;
    const full = join(current, entry.name);
    const relativePath = relative(root, full).split("\\").join("/");
    // Dirent is only a snapshot from readdir. Re-lstat the path before
    // reading it so a replaced symlink or special entry cannot be followed
    // into a foreign tree between enumeration and digesting.
    const info = await lstat(full);
    if (info.isDirectory()) {
      if (directories) directories.push(relativePath);
      await digestTreeWalk(root, excludedRootNames, full, files, directories);
      continue;
    }
    if (!info.isFile()) {
      throw new Error(`pair tree contains a symlink or special entry: ${full}`);
    }
    files.push({ path: relativePath, digest: sha256(await readFile(full)) });
  }
}

/**
 * Digest every regular file in a generated tree in stable path order.
 * Root marker files can be excluded because their contents contain the digest
 * they commit; a source manifest is metadata and is intentionally excluded
 * from the source tree digest for compatibility with the existing manifest.
 */
export async function digestTree(
  root,
  { exclude = [], includeDirectories = false } = {},
) {
  const rootInfo = await lstat(root);
  if (rootInfo.isSymbolicLink()) {
    throw new Error(`pair tree cannot be a symbolic link: ${root}`);
  }
  if (!rootInfo.isDirectory()) {
    throw new Error(`pair tree is not a directory: ${root}`);
  }
  const files = [];
  const directories = includeDirectories ? [] : null;
  await digestTreeWalk(root, new Set(exclude), root, files, directories);
  files.sort((left, right) => comparePath(left.path, right.path));
  const canonical = files
    .map(({ path, digest }) => `${path}\0${digest}\n`)
    .join("");
  const result = { digest: sha256(Buffer.from(canonical)), count: files.length };
  if (directories) {
    directories.sort(comparePath);
    // Keep the original newline-delimited digest for journals written before
    // the collision-free topology field was introduced.  Current journals
    // authenticate the length-prefixed form below; a filename may contain a
    // newline, so a bare `${path}\n` list is not injective.
    const legacyTopology = directories.map((path) => `${path}\n`).join("");
    const topology = directories
      .map((path) => `${Buffer.byteLength(path, "utf8")}:${path}\n`)
      .join("");
    result.directorySha256 = sha256(
      Buffer.from(legacyTopology),
    );
    result[DIRECTORY_TOPOLOGY_SHA256_FIELD] = sha256(Buffer.from(topology));
  }
  return result;
}

async function fileDigestIfPresent(path) {
  try {
    const info = await lstat(path);
    if (info.isSymbolicLink() || !info.isFile()) {
      throw new Error(`pair tree contains a symlink or special entry: ${path}`);
    }
    return sha256(await readFile(path));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

export async function createPairMarker({ sourceRoot, irRoot }) {
  const source = await digestTree(sourceRoot, {
    exclude: [SOURCE_MANIFEST_NAME],
  });
  const ir = await digestTree(irRoot, { exclude: [PAIR_MARKER_NAME] });
  const marker = {
    format: PAIR_FORMAT,
    kind: PAIR_KIND,
    source: {
      treeSha256: source.digest,
      fileCount: source.count,
      manifestSha256: await fileDigestIfPresent(
        join(sourceRoot, SOURCE_MANIFEST_NAME),
      ),
    },
    ir: {
      treeSha256: ir.digest,
      fileCount: ir.count,
    },
  };
  marker.pairSha256 = sha256(canonicalPairDigest(marker));
  return validatePairMarker(marker);
}

export async function writePairMarker(irRoot, marker) {
  validatePairMarker(marker);
  await writeFile(
    join(irRoot, PAIR_MARKER_NAME),
    `${JSON.stringify(marker)}\n`,
  );
}

async function readPairMarker(irRoot) {
  let value;
  try {
    const info = await lstat(join(irRoot, PAIR_MARKER_NAME));
    if (info.isSymbolicLink() || !info.isFile()) {
      throw new Error(`pair marker is a symlink or special entry in ${irRoot}`);
    }
    return parsePairMarker(
      await readFile(join(irRoot, PAIR_MARKER_NAME), "utf8"),
    );
  } catch (error) {
    if (error?.code === "ENOENT") {
      throw new Error(`missing ${PAIR_MARKER_NAME} in ${irRoot}`);
    }
    if (error?.message === "invalid .spec-pair.json") {
      throw new Error(`invalid ${PAIR_MARKER_NAME} in ${irRoot}`);
    }
    throw error;
  }
}

/**
 * Verify a complete source/IR pair. With `irOnly`, the source side is not
 * required; this is the check used after copying resources into the .app.
 * The marker is read twice so a concurrent IR replacement cannot be mistaken
 * for a stable tree even when the first and second directory digests happen
 * to come from different generations.
 */
export async function verifyPair({ sourceRoot, irRoot, irOnly = false } = {}) {
  const before = await readPairMarker(irRoot);
  const source = irOnly
    ? null
    : await digestTree(sourceRoot, { exclude: [SOURCE_MANIFEST_NAME] });
  const sourceManifestSha256 = irOnly
    ? null
    : await fileDigestIfPresent(join(sourceRoot, SOURCE_MANIFEST_NAME));
  const ir = await digestTree(irRoot, { exclude: [PAIR_MARKER_NAME] });
  const after = await readPairMarker(irRoot);
  if (JSON.stringify(before) !== JSON.stringify(after)) {
    throw new Error(`pair marker changed while reading ${irRoot}`);
  }
  if (
    !irOnly &&
    (source.digest !== before.source.treeSha256 ||
      source.count !== before.source.fileCount ||
      sourceManifestSha256 !== before.source.manifestSha256)
  ) {
    throw new Error(
      `source tree does not match ${PAIR_MARKER_NAME} in ${irRoot}`,
    );
  }
  if (ir.digest !== before.ir.treeSha256 || ir.count !== before.ir.fileCount) {
    throw new Error(`IR tree does not match ${PAIR_MARKER_NAME} in ${irRoot}`);
  }
  return before;
}

const PAIR_JOURNAL_PHASES = new Set([
  "prepared",
  "source-backed-up",
  "ir-backed-up",
  "source-published",
  "ir-published",
  // A failed final audit may have moved the unverified generation back to
  // its exact stage while restoring the old canonical state.  Keeping this
  // explicit phase lets the next invocation resume that stage without
  // mistaking the rollback for a fresh publication.
  "rolled-back",
  "verified",
  "cleanup-source",
  "cleanup",
]);

const PAIR_JOURNAL_PHASE_ORDER = [
  "prepared",
  "source-backed-up",
  "ir-backed-up",
  "source-published",
  "ir-published",
  "rolled-back",
  "verified",
  "cleanup-source",
  "cleanup",
];

function journalPhaseAtLeast(journal, phase) {
  return (
    PAIR_JOURNAL_PHASE_ORDER.indexOf(journal.phase) >=
    PAIR_JOURNAL_PHASE_ORDER.indexOf(phase)
  );
}

// This is deliberately test-only.  A broad production-facing alias could
// turn an inherited environment variable into an unexpected process exit.
const PUBLICATION_FAILPOINT_ENV_NAME = "EC_TEST_SPECS_PUBLISH_FAILPOINT";
// A deliberately private test seam for deterministic TOCTOU coverage.  It is
// active only in Node's test runner and only for one exact managed root path;
// no publication API exposes an arbitrary replacement callback.
const REMOVE_CAPTURE_REPLACEMENT_ENV_NAME =
  "EC_TEST_SPECS_REMOVE_AFTER_CAPTURE_REPLACE_ROOT";
const REMOVE_CAPTURE_FILE_REPLACEMENT_ENV_NAME =
  "EC_TEST_SPECS_REMOVE_AFTER_CAPTURE_REPLACE_FILE";

function journalPathForLock(lockPath) {
  return join(dirname(resolve(lockPath)), PAIR_JOURNAL_NAME);
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function journalIdentity(info) {
  return {
    kind: "directory",
    dev: String(info.dev),
    ino: String(info.ino),
  };
}

function journalFileIdentity(info) {
  return {
    kind: "file",
    dev: String(info.dev),
    ino: String(info.ino),
  };
}

function identitiesEqual(left, right) {
  return (
    isPlainObject(left) &&
    isPlainObject(right) &&
    left.kind === right.kind &&
    left.dev === right.dev &&
    left.ino === right.ino
  );
}

async function directoryIdentity(path, label = path) {
  const info = await lstat(path);
  if (info.isSymbolicLink() || !info.isDirectory()) {
    throw new Error(`${label} is not a regular directory: ${path}`);
  }
  return journalIdentity(info);
}

async function optionalDirectoryIdentity(path, label = path) {
  try {
    return await directoryIdentity(path, label);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function journalDigest(value, label, { nullable = false } = {}) {
  if (nullable && value === null) return;
  if (!isSha256(value)) {
    throw new Error(`${label} must be a lowercase SHA-256 digest`);
  }
}

function validateJournalIdentity(value, label) {
  if (value === null) return;
  assertKnownFields(value, ["kind", "dev", "ino"], label);
  if (
    value.kind !== "directory" ||
    typeof value.dev !== "string" ||
    !value.dev ||
    typeof value.ino !== "string" ||
    !value.ino
  ) {
    throw new Error(`${label} is not a directory identity`);
  }
}

function validateJournalPathRecord(value, label, { nullable = false } = {}) {
  if (nullable && value === null) return;
  assertKnownFields(value, ["path", "identity"], label);
  if (
    typeof value.path !== "string" ||
    !isAbsolute(value.path) ||
    resolve(value.path) !== value.path
  ) {
    throw new Error(`${label}.path must be an absolute normalized path`);
  }
  validateJournalIdentity(value.identity, `${label}.identity`);
}

function validateTreeRelativePath(value, label) {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.includes("\0") ||
    value.startsWith("/") ||
    value.split("/").some((part) => !part || part === "." || part === "..")
  ) {
    throw new Error(`${label} must be a relative managed-tree path`);
  }
}

function validateTreeIdentity(value, label) {
  assertKnownFields(value, ["files", "directories"], label);
  if (!Array.isArray(value.files) || !Array.isArray(value.directories)) {
    throw new Error(`${label} entries must be arrays`);
  }
  const validateEntries = (entries, entryLabel, includeDigest) => {
    const seen = new Set();
    let previous = null;
    for (const [index, entry] of entries.entries()) {
      assertKnownFields(
        entry,
        includeDigest
          ? ["path", "sha256", "dev", "ino"]
          : ["path", "dev", "ino"],
        `${entryLabel}[${index}]`,
      );
      validateTreeRelativePath(entry.path, `${entryLabel}[${index}].path`);
      if (seen.has(entry.path)) {
        throw new Error(`${entryLabel} contains duplicate path ${entry.path}`);
      }
      seen.add(entry.path);
      if (previous !== null && comparePath(previous, entry.path) >= 0) {
        throw new Error(`${entryLabel} must be sorted by path`);
      }
      previous = entry.path;
      if (includeDigest && !isSha256(entry.sha256)) {
        throw new Error(`${entryLabel}[${index}].sha256 must be a SHA-256 digest`);
      }
      if (
        typeof entry.dev !== "string" ||
        !entry.dev ||
        typeof entry.ino !== "string" ||
        !entry.ino
      ) {
        throw new Error(`${entryLabel}[${index}] has an invalid file identity`);
      }
    }
  };
  validateEntries(value.files, `${label}.files`, true);
  validateEntries(value.directories, `${label}.directories`, false);
}

function validateJournalDescriptor(value, label, { nullable = false } = {}) {
  if (nullable && value === null) return;
  assertKnownFields(
    value,
    [
      "treeSha256",
      "fileCount",
      "metadataSha256",
      "directorySha256",
      DIRECTORY_TOPOLOGY_SHA256_FIELD,
      TREE_IDENTITY_FIELD,
    ],
    label,
  );
  journalDigest(value.treeSha256, `${label}.treeSha256`);
  if (!isNonNegativeSafeInteger(value.fileCount)) {
    throw new Error(`${label}.fileCount must be a non-negative safe integer`);
  }
  if (value.metadataSha256 !== null) {
    journalDigest(value.metadataSha256, `${label}.metadataSha256`);
  }
  // Journals written before directory topology was added remain readable for
  // recovery.  Their cleanup path is deliberately more conservative (see
  // removeManagedDirectory): an uncommitted empty directory is never removed.
  if (Object.hasOwn(value, "directorySha256")) {
    journalDigest(value.directorySha256, `${label}.directorySha256`);
  }
  if (Object.hasOwn(value, DIRECTORY_TOPOLOGY_SHA256_FIELD)) {
    journalDigest(
      value[DIRECTORY_TOPOLOGY_SHA256_FIELD],
      `${label}.${DIRECTORY_TOPOLOGY_SHA256_FIELD}`,
    );
  }
  if (Object.hasOwn(value, TREE_IDENTITY_FIELD)) {
    validateTreeIdentity(value[TREE_IDENTITY_FIELD], `${label}.${TREE_IDENTITY_FIELD}`);
  }
}

function validateJournalSides(
  value,
  label,
  { sourceNullable = false, descriptorNullable = false } = {},
) {
  assertKnownFields(value, ["source", "ir"], label);
  validateJournalDescriptor(value.source, `${label}.source`, {
    nullable: sourceNullable || descriptorNullable,
  });
  validateJournalDescriptor(value.ir, `${label}.ir`, {
    nullable: descriptorNullable,
  });
}

function validateJournalPaths(value, label, { sourceNullable = false } = {}) {
  assertKnownFields(value, ["source", "ir"], label);
  validateJournalPathRecord(value.source, `${label}.source`, {
    nullable: sourceNullable,
  });
  validateJournalPathRecord(value.ir, `${label}.ir`);
}

function validatePairJournal(value) {
  assertKnownFields(
    value,
    [
      "schema",
      "version",
      "operation",
      "oldPairSha256",
      "newPairSha256",
      "oldMarker",
      "newMarker",
      "verification",
      "oldSides",
      "newSides",
      "canonical",
      "stage",
      "backup",
      "phase",
    ],
    "spec pair journal",
  );
  if (value.schema !== PAIR_JOURNAL_SCHEMA) {
    throw new Error(`spec pair journal schema is unsupported`);
  }
  if (value.version !== PAIR_JOURNAL_VERSION) {
    throw new Error(`spec pair journal version is unsupported`);
  }
  if (value.operation !== "pair" && value.operation !== "ir") {
    throw new Error(`spec pair journal operation is unsupported`);
  }
  if (value.verification !== "pair" && value.verification !== "callback") {
    throw new Error(`spec pair journal verification mode is unsupported`);
  }
  journalDigest(value.oldPairSha256, "spec pair journal oldPairSha256", {
    nullable: true,
  });
  journalDigest(value.newPairSha256, "spec pair journal newPairSha256");
  if (value.oldMarker !== null) validatePairMarker(value.oldMarker);
  validatePairMarker(value.newMarker);
  if (
    (value.oldMarker === null) !== (value.oldPairSha256 === null) ||
    (value.oldMarker && value.oldMarker.pairSha256 !== value.oldPairSha256) ||
    value.newMarker.pairSha256 !== value.newPairSha256
  ) {
    throw new Error("spec pair journal pair digest does not match its marker");
  }
  const sourceNullable = value.operation === "ir";
  validateJournalSides(value.oldSides, "spec pair journal oldSides", {
    sourceNullable,
    descriptorNullable: true,
  });
  validateJournalSides(value.newSides, "spec pair journal newSides", {
    sourceNullable,
  });
  validateJournalPaths(value.canonical, "spec pair journal canonical", {
    sourceNullable,
  });
  validateJournalPaths(value.stage, "spec pair journal stage", {
    sourceNullable,
  });
  validateJournalPaths(value.backup, "spec pair journal backup", {
    sourceNullable,
  });
  if (!PAIR_JOURNAL_PHASES.has(value.phase)) {
    throw new Error(`spec pair journal phase is unsupported: ${value.phase}`);
  }
  return value;
}

async function readJournalRecord(journalPath) {
  try {
    const info = await lstat(journalPath);
    if (info.isSymbolicLink() || !info.isFile()) {
      throw new Error(`spec pair journal is a symlink or special entry`);
    }
    let value;
    try {
      value = JSON.parse(await readFile(journalPath, "utf8"));
    } catch (error) {
      if (error instanceof SyntaxError) {
        throw new Error(`invalid spec pair journal JSON`);
      }
      throw error;
    }
    const after = await lstat(journalPath);
    if (
      after.isSymbolicLink() ||
      !after.isFile() ||
      !identitiesEqual(journalFileIdentity(info), journalFileIdentity(after))
    ) {
      throw new Error(`spec pair journal changed while reading`);
    }
    return {
      value: validatePairJournal(value),
      identity: journalFileIdentity(after),
    };
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

export async function readPairJournal(lockPath) {
  const record = await readJournalRecord(journalPathForLock(lockPath));
  return record?.value ?? null;
}

async function syncDirectory(path) {
  let handle;
  try {
    handle = await open(path, "r");
    await handle.sync();
  } finally {
    if (handle) await handle.close().catch(() => {});
  }
}

async function writeJournalFile(journalPath, journal) {
  validatePairJournal(journal);
  await assertNoSymlinkInPath(journalPath);
  const parent = dirname(journalPath);
  await mkdir(parent, { recursive: true });
  const temporary = join(
    parent,
    `.${basename(journalPath)}.tmp-${process.pid}-${randomUUID()}`,
  );
  let handle;
  try {
    handle = await open(temporary, "wx", 0o600);
    await handle.writeFile(`${JSON.stringify(journal)}\n`, "utf8");
    await handle.sync();
    await handle.close();
    handle = null;
    await rename(temporary, journalPath);
    await syncDirectory(parent);
    await directoryIdentity(parent, `spec pair journal parent directory`);
    // The journal itself is a file; its directory identity above is only a
    // durability check. Return its file identity for guarded cleanup.
    const journalInfo = await lstat(journalPath);
    if (journalInfo.isSymbolicLink() || !journalInfo.isFile()) {
      throw new Error(`spec pair journal is a symlink or special entry`);
    }
    return journalFileIdentity(journalInfo);
  } catch (error) {
    if (handle) await handle.close().catch(() => {});
    await rm(temporary, { force: true }).catch(() => {});
    throw error;
  }
}

async function clearJournalFile(journalPath, expectedIdentity = null) {
  const info = await lstat(journalPath);
  if (info.isSymbolicLink() || !info.isFile()) {
    throw new Error(`refusing to remove a malformed spec pair journal`);
  }
  if (expectedIdentity && !identitiesEqual(journalFileIdentity(info), expectedIdentity)) {
    throw new Error(`refusing to remove a changed spec pair journal`);
  }
  await unlink(journalPath);
  await syncDirectory(dirname(journalPath));
}

async function rootDescriptor(path, side) {
  const tree = await digestTree(path, {
    exclude: [side === "source" ? SOURCE_MANIFEST_NAME : PAIR_MARKER_NAME],
    includeDirectories: true,
  });
  return {
    treeSha256: tree.digest,
    fileCount: tree.count,
    metadataSha256: await fileDigestIfPresent(
      join(path, side === "source" ? SOURCE_MANIFEST_NAME : PAIR_MARKER_NAME),
    ),
    directorySha256: tree.directorySha256,
    [DIRECTORY_TOPOLOGY_SHA256_FIELD]: tree[DIRECTORY_TOPOLOGY_SHA256_FIELD],
  };
}

function descriptorsEqual(left, right) {
  const filesEqual =
    !!left &&
    !!right &&
    left.treeSha256 === right.treeSha256 &&
    left.fileCount === right.fileCount &&
    left.metadataSha256 === right.metadataSha256;
  if (!filesEqual) return false;
  // The right-hand descriptor is the journal's promise.  A legacy journal
  // has no topology field, so keep its file-level compatibility; current
  // journals require the collision-free topology digest as well.  The old
  // field remains as a compatibility check for journals written before the
  // length-prefixed field was added.
  const hasCurrentTopology =
    Object.hasOwn(right, "directorySha256") &&
    Object.hasOwn(right, DIRECTORY_TOPOLOGY_SHA256_FIELD);
  return (
    (!Object.hasOwn(right, "directorySha256") ||
      left.directorySha256 === right.directorySha256) &&
    (!hasCurrentTopology ||
      left[DIRECTORY_TOPOLOGY_SHA256_FIELD] ===
        right[DIRECTORY_TOPOLOGY_SHA256_FIELD])
  );
}

function pathRecord(path, identity = null) {
  return { path: resolve(path), identity };
}

function sideNames(journal) {
  return journal.operation === "ir" ? ["ir"] : ["source", "ir"];
}

function sideValue(journal, group, side) {
  return journal[group][side];
}

function pathValue(journal, group, side) {
  return journal[group][side];
}

async function inspectPathRecord(record, label) {
  await assertNoSymlinkInPath(record.path);
  try {
    const info = await lstat(record.path);
    if (info.isSymbolicLink() || !info.isDirectory()) {
      throw new Error(`${label} is not a regular directory: ${record.path}`);
    }
    const identity = journalIdentity(info);
    if (!record.identity) {
      throw new Error(
        `${label} has no recorded identity for present directory: ${record.path}`,
      );
    }
    if (!identitiesEqual(record.identity, identity)) {
      throw new Error(`${label} identity changed: ${record.path}`);
    }
    return { present: true, identity };
  } catch (error) {
    if (error?.code === "ENOENT") return { present: false, identity: null };
    throw error;
  }
}

async function matchesSide(record, side, descriptor, label, marker = null) {
  const state = await inspectPathRecord(record, label);
  if (!state.present) return false;
  const actual = await rootDescriptor(record.path, side);
  if (!descriptorsEqual(actual, descriptor)) return false;
  if (side === "ir" && marker === null) {
    // A null old marker is the explicit legacy markerless case.  Do not let a
    // corrupt/present marker masquerade as that generation merely because its
    // tree digest was captured in the journal.
    try {
      const info = await lstat(join(record.path, PAIR_MARKER_NAME));
      if (info.isSymbolicLink() || !info.isFile()) return false;
      return false;
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
  }
  if (side === "ir" && marker) {
    try {
      const current = await readPairMarker(record.path);
      if (JSON.stringify(current) !== JSON.stringify(marker)) return false;
    } catch {
      return false;
    }
  }
  return true;
}

async function classifySide(journal, group, side, { identicalAs = "same" } = {}) {
  const record = pathValue(journal, group, side);
  const state = await inspectPathRecord(record, `${group}.${side}`);
  if (!state.present) return "missing";
  const oldDescriptor = sideValue(journal, "oldSides", side);
  const newDescriptor = sideValue(journal, "newSides", side);
  const oldMarker = side === "ir" ? journal.oldMarker : null;
  const newMarker = side === "ir" ? journal.newMarker : null;
  const oldMatches =
    oldDescriptor &&
    (await matchesSide(record, side, oldDescriptor, `${group}.${side}`, oldMarker));
  const newMatches =
    newDescriptor &&
    (await matchesSide(record, side, newDescriptor, `${group}.${side}`, newMarker));
  if (oldMatches && newMatches) return identicalAs;
  if (oldMatches) return "old";
  if (newMatches) return "new";
  throw new Error(`unrecognized ${group}.${side} contents: ${record.path}`);
}

async function pairAt(journal, group) {
  if (journal.operation === "ir") {
    const state = await inspectPathRecord(
      pathValue(journal, group, "ir"),
      `${group}.ir`,
    );
    if (!state.present) return "missing";
    try {
      const marker = await verifyPair({
        irRoot: pathValue(journal, group, "ir").path,
        irOnly: true,
      });
      if (
        marker.pairSha256 === journal.oldPairSha256 &&
        marker.pairSha256 !== journal.newPairSha256
      ) {
        return "old";
      }
      if (marker.pairSha256 === journal.newPairSha256) return "new";
    } catch {
      // Individual side classification below produces the fail-closed error.
    }
    if (!journal.oldPairSha256) {
      try {
        const sideState = await classifySide(journal, group, "ir");
        if (sideState === "old" || sideState === "new") return sideState;
      } catch {
        // Return invalid below; callers must not infer a generation from an
        // incomplete or unrecognized canonical tree.
      }
    }
    return "invalid";
  }
  const source = await inspectPathRecord(
    pathValue(journal, group, "source"),
    `${group}.source`,
  );
  const ir = await inspectPathRecord(
    pathValue(journal, group, "ir"),
    `${group}.ir`,
  );
  if (!source.present || !ir.present) return "missing";
  try {
    const marker = await verifyPair({
      sourceRoot: pathValue(journal, group, "source").path,
      irRoot: pathValue(journal, group, "ir").path,
    });
    if (
      journal.oldPairSha256 &&
      marker.pairSha256 === journal.oldPairSha256 &&
      marker.pairSha256 !== journal.newPairSha256
    ) {
      return "old";
    }
    if (marker.pairSha256 === journal.newPairSha256) return "new";
  } catch {
    // Individual side classification below produces the fail-closed error.
  }
  if (!journal.oldPairSha256) {
    try {
      const sourceState = await classifySide(journal, group, "source");
      const irState = await classifySide(journal, group, "ir");
      const sourceOld = sourceState === "old" || sourceState === "same";
      const sourceNew = sourceState === "new" || sourceState === "same";
      const irOld = irState === "old" || irState === "same";
      const irNew = irState === "new" || irState === "same";
      if (sourceOld && irOld) return "old";
      if (sourceNew && irNew) return "new";
    } catch {
      // Return invalid below; callers must not infer a generation from an
      // incomplete or unrecognized canonical tree.
    }
  }
  return "invalid";
}

async function captureTree(root) {
  const files = [];
  const directories = [];
  async function walk(current, relativePath = "") {
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((left, right) => comparePath(left.name, right.name));
    for (const entry of entries) {
      const full = join(current, entry.name);
      const path = relativePath ? `${relativePath}/${entry.name}` : entry.name;
      const info = await lstat(full);
      if (info.isDirectory()) {
        directories.push({
          path,
          dev: String(info.dev),
          ino: String(info.ino),
        });
        await walk(full, path);
      } else if (info.isFile()) {
        if (info.nlink !== 1) {
          throw new Error(`managed publication tree contains a hard-linked file: ${full}`);
        }
        files.push({
          path,
          sha256: sha256(await readFile(full)),
          dev: String(info.dev),
          ino: String(info.ino),
        });
      } else {
        throw new Error(`managed publication tree contains a link or special entry: ${full}`);
      }
    }
  }
  await walk(root);
  directories.sort((left, right) => comparePath(left.path, right.path));
  files.sort((left, right) => comparePath(left.path, right.path));
  return { files, directories };
}

function treeIdentityFromSnapshot(snapshot) {
  return {
    files: snapshot.files.map((file) => ({ ...file })),
    directories: snapshot.directories.map((directory) => ({ ...directory })),
  };
}

function treeIdentitiesEqual(left, right) {
  if (
    !left ||
    !right ||
    left.files.length !== right.files.length ||
    left.directories.length !== right.directories.length
  ) {
    return false;
  }
  const entriesEqual = (leftEntries, rightEntries, includeDigest) =>
    leftEntries.every((leftEntry, index) => {
      const rightEntry = rightEntries[index];
      return (
        leftEntry.path === rightEntry.path &&
        leftEntry.dev === rightEntry.dev &&
        leftEntry.ino === rightEntry.ino &&
        (!includeDigest || leftEntry.sha256 === rightEntry.sha256)
      );
    });
  return (
    entriesEqual(left.files, right.files, true) &&
    entriesEqual(left.directories, right.directories, false)
  );
}

function assertManagedTreeIdentity(snapshot, descriptor, label) {
  // A descriptor with no topology field is an older file-only journal.  Keep
  // its existing conservative empty-directory rule rather than interpreting
  // a newly added identity payload as a promise that the old journal never
  // made.
  if (!Object.hasOwn(descriptor, "directorySha256")) return;
  if (!Object.hasOwn(descriptor, TREE_IDENTITY_FIELD)) return;
  if (!treeIdentitiesEqual(snapshot, descriptor[TREE_IDENTITY_FIELD])) {
    throw new Error(`refusing to remove changed ${label} internal identity`);
  }
}

async function descriptorWithTreeIdentity(path, side) {
  const beforeIdentity = await directoryIdentity(path, `${side} tree`);
  const descriptor = await rootDescriptor(path, side);
  const snapshot = await captureTree(path);
  const afterIdentity = await directoryIdentity(path, `${side} tree`);
  if (!identitiesEqual(beforeIdentity, afterIdentity)) {
    throw new Error(`${side} tree changed while recording its journal descriptor`);
  }
  const current = await rootDescriptor(path, side);
  if (!descriptorsEqual(current, descriptor)) {
    throw new Error(`${side} tree changed while recording its journal descriptor`);
  }
  return {
    ...descriptor,
    [TREE_IDENTITY_FIELD]: treeIdentityFromSnapshot(snapshot),
  };
}

function hasEmptyDirectory(snapshot) {
  return snapshot.directories.some((directory) => {
    const prefix = `${directory.path}/`;
    return (
      !snapshot.files.some((file) => file.path.startsWith(prefix)) &&
      !snapshot.directories.some((child) => child.path.startsWith(prefix))
    );
  });
}

function assertLegacyTopology(snapshot, descriptor, label) {
  if (
    !Object.hasOwn(descriptor, "directorySha256") &&
    hasEmptyDirectory(snapshot)
  ) {
    throw new Error(`refusing to remove uncommitted empty directory in ${label}`);
  }
}

async function assertManagedRootIdentity(record, expectedIdentity, label) {
  const state = await inspectPathRecord(record, label);
  if (!state.present || !identitiesEqual(state.identity, expectedIdentity)) {
    throw new Error(`refusing to remove changed ${label}: ${record.path}`);
  }
}

async function assertLegacyDirectoryTopology(record, descriptor, label) {
  if (!descriptor || Object.hasOwn(descriptor, "directorySha256")) return;
  const state = await inspectPathRecord(record, label);
  if (!state.present) return;
  const snapshot = await captureTree(record.path);
  assertLegacyTopology(snapshot, descriptor, label);
  await assertManagedRootIdentity(record, state.identity, label);
}

async function replaceRootAfterCaptureForTest(record) {
  if (
    !process.env.NODE_TEST_CONTEXT ||
    process.env[REMOVE_CAPTURE_REPLACEMENT_ENV_NAME] !== record.path
  ) {
    return;
  }
  const suffix = `${process.pid}-${randomUUID()}`;
  const clone = `${record.path}.test-clone-${suffix}`;
  const displaced = `${record.path}.test-original-${suffix}`;
  await cp(record.path, clone, { recursive: true, force: false });
  await rename(record.path, displaced);
  await rename(clone, record.path);
}

async function replaceFileAfterCaptureForTest(record, snapshot) {
  if (
    !process.env.NODE_TEST_CONTEXT ||
    process.env[REMOVE_CAPTURE_FILE_REPLACEMENT_ENV_NAME] !== record.path ||
    snapshot.files.length === 0
  ) {
    return;
  }
  const full = join(record.path, snapshot.files[0].path);
  const suffix = `${process.pid}-${randomUUID()}`;
  const clone = `${record.path}.test-file-clone-${suffix}`;
  const displaced = `${record.path}.test-file-original-${suffix}`;
  await cp(full, clone, { force: false });
  await rename(full, displaced);
  await rename(clone, full);
}

async function removeManagedDirectory(record, side, descriptor, marker, label) {
  const state = await inspectPathRecord(record, label);
  if (!state.present) return;
  if (!(await matchesSide(record, side, descriptor, label, marker))) {
    throw new Error(`refusing to remove changed ${label}: ${record.path}`);
  }
  const snapshot = await captureTree(record.path);
  await replaceRootAfterCaptureForTest(record);
  await replaceFileAfterCaptureForTest(record, snapshot);
  // A descriptor written by this version commits the complete directory
  // topology.  Older journals only committed regular files; refuse to
  // remove any empty directory in that legacy case instead of silently
  // deleting topology that was never promised by the descriptor.
  assertLegacyTopology(snapshot, descriptor, label);
  // Current journals also commit every managed child identity.  This check is
  // deliberately against the persisted snapshot, not the just-captured
  // inode values, so a same-content replacement cannot become deletion
  // authorization merely by happening before captureTree runs.
  assertManagedTreeIdentity(snapshot, descriptor, label);
  // Re-check the root inode after the recursive capture.  A same-content
  // replacement at the managed path must never turn a stale snapshot into a
  // deletion authorization.
  await assertManagedRootIdentity(record, state.identity, label);
  // The metadata file is excluded from the pair tree digest, so verify it
  // explicitly before deleting the exact captured paths.
  const actual = await rootDescriptor(record.path, side);
  if (!descriptorsEqual(actual, descriptor)) {
    throw new Error(`refusing to remove changed ${label}: ${record.path}`);
  }
  for (const file of snapshot.files) {
    await assertManagedRootIdentity(record, state.identity, label);
    const full = join(record.path, file.path);
    const info = await lstat(full);
    if (
      !info.isFile() ||
      info.nlink !== 1 ||
      String(info.dev) !== file.dev ||
      String(info.ino) !== file.ino ||
      sha256(await readFile(full)) !== file.sha256
    ) {
      throw new Error(`refusing to remove changed ${label} file: ${full}`);
    }
    await unlink(join(record.path, file.path));
  }
  for (const path of [...snapshot.directories].sort(
    (left, right) =>
      right.path.split("/").length - left.path.split("/").length ||
      comparePath(right.path, left.path),
  )) {
    await assertManagedRootIdentity(record, state.identity, label);
    await rmdir(join(record.path, path.path));
  }
  await assertManagedRootIdentity(record, state.identity, label);
  const remaining = await readdir(record.path);
  if (remaining.length) {
    throw new Error(`refusing to remove changed ${label}: ${record.path}`);
  }
  await assertManagedRootIdentity(record, state.identity, label);
  await rmdir(record.path);
  await syncDirectory(dirname(record.path));
}

function failpointConfigured(label) {
  const configured = process.env[PUBLICATION_FAILPOINT_ENV_NAME];
  if (!configured) return false;
  const normalize = (value) =>
    value
      .toLowerCase()
      .replace(/^after[-_:]/, "")
      .replace(/^rename[-_:]/, "")
      .replace(/[-_:]after$/, "")
      .replace(/[-_:]rename$/, "")
      .replaceAll("_", "-");
  return configured
    .split(",")
    .map((value) => normalize(value.trim()))
    .some((value) => value === normalize(label) || value === "all");
}

async function publicationFailpoint(label) {
  if (!failpointConfigured(label)) return;
  process.stderr.write(`spec pair publication failpoint: ${label}\n`);
  // An immediate exit deliberately skips JavaScript finally blocks. This is
  // the subprocess crash harness used by the publication matrix.
  process.exitCode = 97;
  process.exit(97);
}

function setPathIdentity(journal, group, side, identity) {
  journal[group][side].identity = identity;
}

async function renameDirectoryBoundary(sourceRecord, targetRecord, label) {
  const sourceState = await inspectPathRecord(sourceRecord, `${label} source`);
  if (!sourceState.present) {
    throw new Error(`${label} source is missing: ${sourceRecord.path}`);
  }
  const targetState = await inspectPathRecord(targetRecord, `${label} target`);
  if (targetState.present) {
    throw new Error(`${label} target already exists: ${targetRecord.path}`);
  }
  await rename(sourceRecord.path, targetRecord.path);
  await syncDirectory(dirname(sourceRecord.path));
  if (dirname(sourceRecord.path) !== dirname(targetRecord.path)) {
    await syncDirectory(dirname(targetRecord.path));
  }
}

async function writeJournalPhase(journalPath, journal, phase) {
  journal.phase = phase;
  return writeJournalFile(journalPath, journal);
}

async function currentOldPair(journal, canonical) {
  let marker;
  try {
    marker = await readPairMarker(canonical.ir.path);
  } catch (error) {
    if (error?.message === `missing ${PAIR_MARKER_NAME} in ${canonical.ir.path}`) {
      return null;
    }
    throw error;
  }
  try {
    const verified =
      journal.operation === "ir"
        ? await verifyPair({ irRoot: canonical.ir.path, irOnly: true })
        : await verifyPair({
            sourceRoot: canonical.source.path,
            irRoot: canonical.ir.path,
          });
    if (verified.pairSha256 !== marker.pairSha256) {
      throw new Error(`canonical pair changed while reading its marker`);
    }
    return verified;
  } catch (error) {
    throw new Error(
      `existing canonical ${journal.operation === "ir" ? "IR" : "source/IR pair"} is invalid`,
      { cause: error },
    );
  }
}

async function makeJournal({
  operation,
  canonical,
  stage,
  backup,
  oldSides,
  newSides,
  newMarker,
  oldMarker,
  verification,
}) {
  const journal = {
    schema: PAIR_JOURNAL_SCHEMA,
    version: PAIR_JOURNAL_VERSION,
    operation,
    oldPairSha256: oldMarker?.pairSha256 ?? null,
    newPairSha256: newMarker.pairSha256,
    oldMarker: oldMarker ?? null,
    newMarker,
    verification,
    oldSides,
    newSides,
    canonical,
    stage,
    backup,
    phase: "prepared",
  };
  return validatePairJournal(journal);
}

async function createPublicationBackup(canonicalPath) {
  const parent = dirname(resolve(canonicalPath));
  await assertNoSymlinkInPath(parent);
  await mkdir(parent, { recursive: true });
  const temporary = await mkdtemp(
    join(parent, `.${basename(canonicalPath)}.backup-`),
  );
  // mkdtemp gives us a unique name. Remove only this just-created empty
  // directory; the journal will later own the exact path and identity.
  await rmdir(temporary);
  await syncDirectory(parent);
  return temporary;
}

async function pathPresent(record) {
  return (await inspectPathRecord(record, `managed path ${record.path}`)).present;
}

async function cleanupJournalArtifacts(journal, journalPath, journalIdentityValue) {
  for (const side of sideNames(journal)) {
    const stageRecord = pathValue(journal, "stage", side);
    const stageDescriptor = sideValue(journal, "newSides", side);
    const stageMarker = side === "ir" ? journal.newMarker : null;
    await removeManagedDirectory(
      stageRecord,
      side,
      stageDescriptor,
      stageMarker,
      `staged ${side} tree`,
    );
    setPathIdentity(journal, "stage", side, null);

    const backupRecord = pathValue(journal, "backup", side);
    if (await pathPresent(backupRecord)) {
      const backupDescriptor = sideValue(journal, "oldSides", side);
      if (!backupDescriptor) {
        throw new Error(`unexpected ${side} backup without an old tree`);
      }
      const backupMarker = side === "ir" ? journal.oldMarker : null;
      await removeManagedDirectory(
        backupRecord,
        side,
        backupDescriptor,
        backupMarker,
        `old ${side} backup`,
      );
    }
    setPathIdentity(journal, "backup", side, null);
  }
  await clearJournalFile(journalPath, journalIdentityValue);
}

async function recoverNewCanonical(journal, journalPath, journalIdentityValue) {
  // A complete canonical new pair is commit-worthy only after the final
  // verification boundary was durably recorded.  Before that point an audit
  // can still reject the pair, so the old backup path gets first refusal.
  if (!journalPhaseAtLeast(journal, "verified")) return false;
  const canonicalState = await pairAt(journal, "canonical");
  if (canonicalState !== "new") return false;
  await cleanupJournalArtifacts(journal, journalPath, journalIdentityValue);
  return true;
}

async function hasNewStageArtifacts(journal) {
  let found = false;
  for (const side of sideNames(journal)) {
    const record = pathValue(journal, "stage", side);
    const state = await inspectPathRecord(record, `new ${side} stage`);
    if (!state.present) continue;
    if (
      !(await matchesSide(
        record,
        side,
        sideValue(journal, "newSides", side),
        `new ${side} stage`,
        side === "ir" ? journal.newMarker : null,
      ))
    ) {
      throw new Error(`unrecognized staged ${side} tree: ${record.path}`);
    }
    found = true;
  }
  return found;
}

async function recoverOldBackups(
  journal,
  journalPath,
  journalIdentityValue,
  { preserveStage = false } = {},
) {
  const states = new Map();

  // First audit every side without changing anything.  An old side with a
  // null descriptor means that side did not exist before this transaction;
  // its backup must therefore be absent and its restored canonical state must
  // remain absent.  A descriptor-bearing side can already be restored at the
  // canonical path (for example, after a previous recovery pass), so a
  // missing backup is not by itself incomplete.
  for (const side of sideNames(journal)) {
    const canonicalRecord = pathValue(journal, "canonical", side);
    const stageRecord = pathValue(journal, "stage", side);
    const backupRecord = pathValue(journal, "backup", side);
    const descriptor = sideValue(journal, "oldSides", side);
    const marker = side === "ir" ? journal.oldMarker : null;
    const canonicalState = await classifySide(journal, "canonical", side);
    const stageState = await classifySide(journal, "stage", side, {
      identicalAs: "new",
    });
    await assertLegacyDirectoryTopology(
      canonicalRecord,
      canonicalState === "new"
        ? sideValue(journal, "newSides", side)
        : descriptor,
      `canonical ${side} tree`,
    );
    if (stageState === "new") {
      await assertLegacyDirectoryTopology(
        stageRecord,
        sideValue(journal, "newSides", side),
        `new ${side} stage`,
      );
    }
    const backupInfo = await inspectPathRecord(
      backupRecord,
      `old ${side} backup`,
    );
    if (backupInfo.present) {
      if (!descriptor) {
        throw new Error(`unexpected ${side} backup without an old tree`);
      }
      if (
        !(await matchesSide(
          backupRecord,
          side,
          descriptor,
          `old ${side} backup`,
          marker,
        ))
      ) {
        throw new Error(
          `refusing to rollback changed old ${side} backup: ${backupRecord.path}`,
        );
      }
      if (!Object.hasOwn(descriptor, "directorySha256")) {
        await assertLegacyDirectoryTopology(
          backupRecord,
          descriptor,
          `old ${side} backup`,
        );
      }
    }
    if (stageState !== "missing" && stageState !== "new") {
      throw new Error(`refusing to rollback unrecognized staged ${side} tree`);
    }
    if (
      canonicalState !== "missing" &&
      canonicalState !== "old" &&
      canonicalState !== "new" &&
      canonicalState !== "same"
    ) {
      throw new Error(`refusing to rollback unrecognized canonical ${side} tree`);
    }
    if (descriptor) {
      if (
        !backupInfo.present &&
        canonicalState !== "old" &&
        canonicalState !== "same"
      ) {
        return false;
      }
    } else if (canonicalState === "old") {
      throw new Error(`canonical ${side} has an unexpected old tree`);
    }
    states.set(side, {
      canonicalState,
      stageState,
      backupPresent: backupInfo.present,
      descriptor,
      marker,
      // A side whose old and new descriptors are identical is safe at the
      // canonical path in either generation.  Keep it there during rollback;
      // the other side (whose digest distinguishes the pair) is enough to
      // drive the forward/reverse decision.
      publishedNew: canonicalState === "new",
    });
  }

  let currentJournalIdentity = journalIdentityValue;
  const persist = async () => {
    currentJournalIdentity = await writeJournalFile(journalPath, journal);
  };

  // Hide every already-published new side at its journaled stage.  If both
  // paths contain the known new generation, remove only that exact managed
  // stage before moving the canonical directory; no unknown tree is touched.
  for (const side of sideNames(journal)) {
    const state = states.get(side);
    if (!state.publishedNew) continue;
    const canonicalRecord = pathValue(journal, "canonical", side);
    const stageRecord = pathValue(journal, "stage", side);
    if (state.stageState === "new") {
      await removeManagedDirectory(
        stageRecord,
        side,
        sideValue(journal, "newSides", side),
        side === "ir" ? journal.newMarker : null,
        `duplicate staged ${side} tree`,
      );
      setPathIdentity(journal, "stage", side, null);
      await persist();
    }
    const canonicalInfo = await inspectPathRecord(
      canonicalRecord,
      `rollback ${side} publish source`,
    );
    if (!canonicalInfo.present) {
      throw new Error(`rollback ${side} publish source is missing`);
    }
    setPathIdentity(journal, "stage", side, canonicalInfo.identity);
    await persist();
    await renameDirectoryBoundary(
      canonicalRecord,
      stageRecord,
      `rollback ${side} publish`,
    );
    // Keep the canonical record bound to the published inode while its path
    // is absent.  It will be rebound to the old inode immediately before an
    // old-backup restore; leaving it null would authorize a foreign
    // same-content directory during recovery.
    setPathIdentity(
      journal,
      "stage",
      side,
      await directoryIdentity(stageRecord.path),
    );
    await persist();
  }

  // Restore each descriptor-bearing old side.  Null old descriptors are
  // intentionally left missing at canonical; a first publication must not
  // manufacture an empty or foreign replacement directory.
  for (const side of sideNames(journal)) {
    const state = states.get(side);
    const canonicalRecord = pathValue(journal, "canonical", side);
    const backupRecord = pathValue(journal, "backup", side);
    if (!state.descriptor) continue;
    const canonicalState = await classifySide(journal, "canonical", side);
    if (state.backupPresent) {
      if (canonicalState === "old" || canonicalState === "same") {
        await removeManagedDirectory(
          backupRecord,
          side,
          state.descriptor,
          state.marker,
          `duplicate old ${side} backup`,
        );
        setPathIdentity(journal, "backup", side, null);
        await persist();
      } else if (canonicalState === "missing") {
        const backupState = await inspectPathRecord(
          backupRecord,
          `old ${side} backup`,
        );
        if (!backupState.present) {
          throw new Error(`old ${side} backup disappeared during rollback`);
        }
        // Record the inode at the destination before moving the backup.  If
        // the process dies after rename but before the next journal write,
        // recovery can distinguish this exact old tree from an impostor.
        setPathIdentity(journal, "canonical", side, backupState.identity);
        await persist();
        await renameDirectoryBoundary(
          backupRecord,
          canonicalRecord,
          `rollback ${side} backup`,
        );
        setPathIdentity(journal, "backup", side, null);
        setPathIdentity(
          journal,
          "canonical",
          side,
          await directoryIdentity(canonicalRecord.path),
        );
        await persist();
      } else {
        throw new Error(
          `refusing to restore old ${side} over unrecognized canonical tree`,
        );
      }
    }
  }

  // Verify the old state side-by-side rather than requiring a marker-bearing
  // pair.  This accepts the legacy markerless IR generation and the valid
  // source-only/empty states represented by null old descriptors.
  for (const side of sideNames(journal)) {
    const state = await classifySide(journal, "canonical", side);
    const expectedOld = Boolean(sideValue(journal, "oldSides", side));
    if (
      (expectedOld && state !== "old" && state !== "same") ||
      (!expectedOld && state !== "missing")
    ) {
      throw new Error(`failed to restore the old ${side} canonical state`);
    }
  }

  if (preserveStage) {
    journal.phase = "rolled-back";
    await persist();
    return true;
  }

  // Any staged side left here must be the known new tree. Remove it only
  // after every old side has been verified; a foreign/tampered stage fails
  // closed and leaves the journal plus all artifacts intact.
  for (const side of sideNames(journal)) {
    const stageRecord = pathValue(journal, "stage", side);
    if (await pathPresent(stageRecord)) {
      await removeManagedDirectory(
        stageRecord,
        side,
        sideValue(journal, "newSides", side),
        side === "ir" ? journal.newMarker : null,
        `rolled-back staged ${side} tree`,
      );
      setPathIdentity(journal, "stage", side, null);
      await persist();
    }
  }
  await clearJournalFile(journalPath, currentJournalIdentity);
  return true;
}

async function recoverCompleteStage(
  journal,
  journalPath,
  journalIdentityValue,
  verifyPublished = null,
) {
  const stageStates = new Map();
  const canonicalStates = new Map();
  let hasNewStage = false;
  for (const side of sideNames(journal)) {
    const stageRecord = pathValue(journal, "stage", side);
    const canonicalRecord = pathValue(journal, "canonical", side);
    const stageState = (await pathPresent(stageRecord))
      ? (await matchesSide(
          stageRecord,
          side,
          sideValue(journal, "newSides", side),
          `new ${side} stage`,
          side === "ir" ? journal.newMarker : null,
        ))
        ? "new"
        : (() => {
            throw new Error(`unrecognized staged ${side} tree: ${stageRecord.path}`);
          })()
      : "missing";
    if (stageState === "new") {
      await assertLegacyDirectoryTopology(
        stageRecord,
        sideValue(journal, "newSides", side),
        `new ${side} stage`,
      );
    }
    if (stageState === "new") hasNewStage = true;
    stageStates.set(side, stageState);
    const canonicalState = await classifySide(journal, "canonical", side);
    canonicalStates.set(side, canonicalState);
    await assertLegacyDirectoryTopology(
      canonicalRecord,
      canonicalState === "new"
        ? sideValue(journal, "newSides", side)
        : sideValue(journal, "oldSides", side),
      `canonical ${side} tree`,
    );
    // A crash after publishing one side can leave that side new at canonical
    // while the other side is still a complete stage. That is a resumable
    // forward state, including the first publication where no old backup
    // exists.
    if (
      stageState !== "new" &&
      canonicalState !== "new" &&
      canonicalState !== "same"
    ) {
      return false;
    }
  }

  // A complete canonical new pair before the verification phase is still a
  // recoverable transaction: re-run the journal's declared verification (or
  // require the caller's final-audit callback) before recording `verified`.
  // Other partial states without a new stage remain fail-closed.
  const hasCompleteNewCanonical =
    !hasNewStage &&
    [...canonicalStates.values()].every(
      (state) => state === "new" || state === "same",
    );
  if (
    !hasNewStage &&
    !hasCompleteNewCanonical &&
    !journalPhaseAtLeast(journal, "verified")
  ) {
    return false;
  }

  let currentJournalIdentity = journalIdentityValue;
  const persist = async () => {
    currentJournalIdentity = await writeJournalFile(journalPath, journal);
  };

  // A complete new stage is safe to continue. Any old canonical side is
  // moved to its exact journaled backup first; unknown contents are never
  // overwritten or recursively removed.
  for (const side of sideNames(journal)) {
    const canonicalRecord = pathValue(journal, "canonical", side);
    const backupRecord = pathValue(journal, "backup", side);
    const canonicalState = canonicalStates.get(side);
    if (
      canonicalState === "old" ||
      (canonicalState === "same" &&
        sideValue(journal, "oldSides", side) &&
        stageStates.get(side) === "new")
    ) {
      if (await pathPresent(backupRecord)) {
        if (
          !(await matchesSide(
            backupRecord,
            side,
            sideValue(journal, "oldSides", side),
            `old ${side} backup`,
            side === "ir" ? journal.oldMarker : null,
          ))
        ) {
          throw new Error(`refusing to replace changed old ${side} backup`);
        }
        await assertLegacyDirectoryTopology(
          backupRecord,
          sideValue(journal, "oldSides", side),
          `old ${side} backup`,
        );
      } else {
        const canonicalInfo = await inspectPathRecord(
          canonicalRecord,
          `resume ${side} backup source`,
        );
        if (!canonicalInfo.present) {
          throw new Error(`resume ${side} backup source is missing`);
        }
        setPathIdentity(journal, "backup", side, canonicalInfo.identity);
        await persist();
        await renameDirectoryBoundary(
          canonicalRecord,
          backupRecord,
          `resume ${side} backup`,
        );
        setPathIdentity(journal, "backup", side, await directoryIdentity(backupRecord.path));
        await persist();
      }
    } else if (
      canonicalState !== "missing" &&
      canonicalState !== "new" &&
      canonicalState !== "same"
    ) {
      throw new Error(`refusing to resume over unrecognized canonical ${side} tree`);
    }
  }
  for (const side of sideNames(journal)) {
    const stageRecord = pathValue(journal, "stage", side);
    const canonicalRecord = pathValue(journal, "canonical", side);
    if (stageStates.get(side) === "new" && !(await pathPresent(canonicalRecord))) {
      const stageInfo = await inspectPathRecord(
        stageRecord,
        `resume ${side} publish source`,
      );
      if (!stageInfo.present) {
        throw new Error(`resume ${side} publish source is missing`);
      }
      setPathIdentity(journal, "canonical", side, stageInfo.identity);
      await persist();
      await renameDirectoryBoundary(
        stageRecord,
        canonicalRecord,
        `resume ${side} publish`,
      );
      setPathIdentity(journal, "stage", side, null);
      setPathIdentity(journal, "canonical", side, await directoryIdentity(canonicalRecord.path));
      await persist();
    }
  }
  const state = await pairAt(journal, "canonical");
  if (state !== "new") {
    throw new Error(`resumed publication did not produce a complete new pair`);
  }
  if (!journalPhaseAtLeast(journal, "verified")) {
    if (journal.verification === "callback") {
      if (!verifyPublished) {
        throw new Error(
          `resumed publication requires its final audit; retaining publication artifacts`,
        );
      }
      try {
        await verifyPublished();
        const auditedState = await pairAt(journal, "canonical");
        if (auditedState !== "new") {
          throw new Error(`published pair changed during final audit`);
        }
      } catch (error) {
        // The callback audits the final canonical paths, so a rejection can
        // happen only after the staged generation has been made visible. Put
        // that generation back behind the journal before propagating the
        // audit error; otherwise a failed recovery would leave new content
        // runtime-visible even though `verified` was never recorded.
        const current = await readJournalRecord(journalPath);
        if (!current) throw error;
        try {
          const rolledBack = await recoverOldBackups(
            current.value,
            journalPath,
            current.identity,
            { preserveStage: true },
          );
          if (!rolledBack) {
            throw new Error(
              `could not restore the old canonical state after final audit failure`,
            );
          }
        } catch (rollbackError) {
          throw new AggregateError(
            [error, rollbackError],
            `final audit failed and its publication could not be rolled back safely`,
          );
        }
        throw error;
      }
    }
    currentJournalIdentity = await writeJournalPhase(
      journalPath,
      journal,
      "verified",
    );
  }
  await cleanupJournalArtifacts(journal, journalPath, currentJournalIdentity);
  return true;
}

async function reportOrphanedArtifacts(lockPath) {
  const parent = dirname(resolve(lockPath));
  let entries;
  try {
    entries = await readdir(parent, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  const names = entries
    .map((entry) => entry.name)
    .filter((name) =>
      /^\.[^/]+\.(?:tmp|staging|preflight|backup)-/.test(name),
    )
    .sort(comparePath);
  for (const name of names) {
    process.stderr.write(
      `warning: found orphaned specs publication artifact without journal (left untouched): ${join(parent, name)}\n`,
    );
  }
}

function isWithinOrSame(candidate, parent) {
  const relation = relative(parent, candidate);
  return (
    relation === "" ||
    (!isAbsolute(relation) &&
      relation !== ".." &&
      !relation.startsWith(`..${sep}`))
  );
}

function comparisonPathKey(path) {
  const normalized = path.normalize("NFD");
  return process.platform === "darwin" ? normalized.toLowerCase() : normalized;
}

async function realPathForComparison(path) {
  let current = resolve(path);
  const missing = [];
  while (true) {
    try {
      const resolvedPath = await realpath(current);
      return comparisonPathKey(
        missing.reduceRight((parent, component) => join(parent, component), resolvedPath),
      );
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      const parent = dirname(current);
      if (parent === current) return comparisonPathKey(resolve(path));
      missing.push(basename(current));
      current = parent;
    }
  }
}

async function pathComparisonRecord(record) {
  await assertNoSymlinkInPath(record.path);
  let identity = null;
  try {
    const info = await lstat(record.path);
    if (info.isSymbolicLink()) {
      throw new Error(`refusing to compare a symbolic-link publication path: ${record.path}`);
    }
    if (info.isDirectory()) identity = journalIdentity(info);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  return { path: await realPathForComparison(record.path), identity };
}

async function assertPathRecordsAreDistinct(records) {
  const actual = await Promise.all(records.map(pathComparisonRecord));
  for (let leftIndex = 0; leftIndex < records.length; leftIndex += 1) {
    for (let rightIndex = leftIndex + 1; rightIndex < records.length; rightIndex += 1) {
      const left = actual[leftIndex];
      const right = actual[rightIndex];
      if (
        (left.identity &&
          right.identity &&
          identitiesEqual(left.identity, right.identity)) ||
        isWithinOrSame(left.path, right.path) ||
        isWithinOrSame(right.path, left.path)
      ) {
        throw new Error(
          `spec pair journal paths overlap: ${records[leftIndex].path}`,
        );
      }
    }
  }
}

async function assertJournalPathsAreDistinct(journal) {
  const records = [];
  for (const group of ["canonical", "stage", "backup"]) {
    for (const side of sideNames(journal)) {
      const record = pathValue(journal, group, side);
      if (!record) continue;
      records.push({ group, side, path: record.path });
    }
  }
  await assertPathRecordsAreDistinct(records);
}

export async function recoverPairPublication(
  lockPath,
  { verifyPublished = null } = {},
) {
  const journalPath = journalPathForLock(lockPath);
  const record = await readJournalRecord(journalPath);
  if (!record) {
    await reportOrphanedArtifacts(lockPath);
    return { status: "none" };
  }
  const journal = record.value;
  await assertJournalPathsAreDistinct(journal);
  for (const group of ["canonical", "stage", "backup"]) {
    for (const side of sideNames(journal)) {
      await assertNoSymlinkInPath(pathValue(journal, group, side).path);
    }
  }

  // A callback-backed transaction may have been deliberately rolled back to
  // the old canonical state after its semantic audit rejected the new pair.
  // Keep that state fail-closed until the caller supplies the same final-audit
  // capability; with it, the complete staged generation can proceed.
  if (journal.phase === "rolled-back") {
    if (journal.verification === "callback" && !verifyPublished) {
      throw new Error(
        `resumed publication requires its final audit; retaining publication artifacts`,
      );
    }
    if (
      await recoverCompleteStage(
        journal,
        journalPath,
        record.identity,
        verifyPublished,
      )
    ) {
      return { status: "resumed-stage" };
    }
    throw new Error(
      `spec pair journal does not describe a complete new stage after rollback; retaining publication artifacts`,
    );
  }

  if (await recoverNewCanonical(journal, journalPath, record.identity)) {
    return { status: "completed-new" };
  }
  // Never leave an unverified callback-backed generation visible merely
  // because this invocation lacks the semantic-audit callback.  Roll every
  // published new side back to its exact stage (and restore any descriptored
  // old side, or keep null old sides absent) before returning the guarded
  // error.  The next invocation with the callback can then resume safely.
  if (
    !journalPhaseAtLeast(journal, "verified") &&
    journal.verification === "callback" &&
    !verifyPublished
  ) {
    if (await recoverOldBackups(journal, journalPath, record.identity, { preserveStage: true })) {
      throw new Error(
        `resumed publication requires its final audit; retaining publication artifacts`,
      );
    }
  }
  // A partial publication with at least one complete new stage is explicitly
  // resumable.  This includes the first publication after source has already
  // been renamed to canonical while the new IR is still staged.  A complete
  // canonical new pair with no stage is handled below so an unverified pair
  // cannot bypass the final audit.
  const hasOldDescriptor = sideNames(journal).some((side) =>
    Boolean(sideValue(journal, "oldSides", side)),
  );
  const hasStage = await hasNewStageArtifacts(journal);
  if (
    hasStage &&
    !(journal.verification === "callback" && !verifyPublished)
  ) {
    if (
      await recoverCompleteStage(
        journal,
        journalPath,
        record.identity,
        verifyPublished,
      )
    ) {
      return { status: "resumed-stage" };
    }
  }

  // If the old generation has any descriptor, it can be restored even when
  // one old side never existed.  Callback-backed transactions preserve the
  // known new stage after rollback so a subsequent invocation with the final
  // audit callback can retry it; compile-only transactions clear it after
  // proving the old state.
  if (
    !journalPhaseAtLeast(journal, "verified") &&
    hasOldDescriptor &&
    (await recoverOldBackups(journal, journalPath, record.identity, {
      preserveStage: journal.verification === "callback",
    }))
  ) {
    if (journal.verification === "callback") {
      if (!verifyPublished) {
        throw new Error(
          `resumed publication requires its final audit; retaining publication artifacts`,
        );
      }
      const current = await readJournalRecord(journalPath);
      if (!current) {
        throw new Error(`spec pair journal disappeared during rollback`);
      }
      if (
        await recoverCompleteStage(
          current.value,
          journalPath,
          current.identity,
          verifyPublished,
        )
      ) {
        return { status: "resumed-stage" };
      }
      throw new Error(
        `spec pair journal does not describe a complete new stage after rollback; retaining publication artifacts`,
      );
    }
    return { status: "rolled-back-old" };
  }

  // A no-old-generation first publication can have a complete canonical new
  // pair and no stage after a crash at the final-audit boundary.  Structural
  // verification is sufficient for compile-only mode; sync mode must supply
  // its callback.  If the pair is not complete, this returns false and the
  // final fallback below fails closed.
  if (
    !hasStage &&
    !hasOldDescriptor &&
    (await recoverCompleteStage(
      journal,
      journalPath,
      record.identity,
      verifyPublished,
    ))
  ) {
    return { status: "resumed-stage" };
  }
  if (
    hasOldDescriptor &&
    (await recoverOldBackups(journal, journalPath, record.identity))
  ) {
    return { status: "rolled-back-old" };
  }
  throw new Error(
    `spec pair journal does not describe a complete new stage or old backup; retaining publication artifacts`,
  );
}

export async function pairJournalExists(lockPath) {
  try {
    const info = await lstat(journalPathForLock(lockPath));
    return info.isFile() && !info.isSymbolicLink();
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

export async function publishPairDirectories({
  sourceStage = null,
  irStage,
  sourceCanonical = null,
  irCanonical,
  lockPath,
  verifyPublished = null,
} = {}) {
  const operation = sourceStage === null ? "ir" : "pair";
  if (operation === "pair" && sourceCanonical === null) {
    throw new Error("source canonical path is required for pair publication");
  }
  for (const path of [sourceStage, irStage, sourceCanonical, irCanonical]) {
    if (path !== null) await assertNoSymlinkInPath(path);
  }
  const canonical = {
    source:
      operation === "ir"
        ? null
        : pathRecord(sourceCanonical, await optionalDirectoryIdentity(sourceCanonical)),
    ir: pathRecord(irCanonical, await optionalDirectoryIdentity(irCanonical)),
  };
  const stage = {
    source:
      operation === "ir"
        ? null
        : pathRecord(sourceStage, await directoryIdentity(sourceStage, "source stage")),
    ir: pathRecord(irStage, await directoryIdentity(irStage, "IR stage")),
  };
  // Reject aliases, case-folded collisions, and parent/child relationships
  // before creating any backup artifact.  This is intentionally based on the
  // resolved filesystem paths and current inode identities rather than only
  // the caller's spelling of each path.
  await assertPathRecordsAreDistinct(
    [canonical.source, canonical.ir, stage.source, stage.ir].filter(Boolean),
  );
  const newMarker =
    operation === "ir"
      ? await verifyPair({ irRoot: irStage, irOnly: true })
      : await verifyPair({ sourceRoot: sourceStage, irRoot: irStage });
  const newSides = {
    source:
      operation === "ir"
        ? null
        : await descriptorWithTreeIdentity(sourceStage, "source"),
    ir: await descriptorWithTreeIdentity(irStage, "ir"),
  };
  // The journal descriptor is richer than the cross-language marker (it also
  // commits directory topology), but its file/metadata portion must still be
  // exactly the generation authenticated by that marker.
  if (
    operation === "pair" &&
    (newSides.source.treeSha256 !== newMarker.source.treeSha256 ||
      newSides.source.fileCount !== newMarker.source.fileCount ||
      newSides.source.metadataSha256 !== newMarker.source.manifestSha256)
  ) {
    throw new Error(`source stage changed while creating publication journal`);
  }
  if (
    newSides.ir.treeSha256 !== newMarker.ir.treeSha256 ||
    newSides.ir.fileCount !== newMarker.ir.fileCount ||
    newSides.ir.metadataSha256 !== sha256(`${JSON.stringify(newMarker)}\n`)
  ) {
    throw new Error(`IR stage changed while creating publication journal`);
  }
  const oldMarker =
    operation === "ir"
      ? await currentOldPair({ operation }, { ir: canonical.ir })
      : await currentOldPair({ operation }, canonical);
  const oldSides = {
    source:
      operation === "ir"
        ? null
        : canonical.source.identity
          ? await descriptorWithTreeIdentity(sourceCanonical, "source")
          : null,
    ir: canonical.ir.identity
      ? await descriptorWithTreeIdentity(irCanonical, "ir")
      : null,
  };

  await assertNoSymlinkInPath(irCanonical);
  const backup = {
    source: null,
    ir: null,
  };
  if (operation === "pair") {
    const sourceBackup = await createPublicationBackup(sourceCanonical);
    backup.source = pathRecord(sourceBackup, null);
  }
  const irBackup = await createPublicationBackup(irCanonical);
  backup.ir = pathRecord(irBackup, null);
  const journal = await makeJournal({
    operation,
    canonical,
    stage,
    backup,
    oldSides,
    newSides,
    newMarker,
    oldMarker,
    verification: verifyPublished ? "callback" : "pair",
  });
  // Validate every exact path before any publication rename.  This catches a
  // caller misconfiguration (including a stage/canonical parent relation) at
  // the transaction boundary, not only when a stale journal is recovered.
  await assertJournalPathsAreDistinct(journal);
  const journalPath = journalPathForLock(lockPath);
  let journalIdentityValue = await writeJournalFile(journalPath, journal);

  const update = async (phase) => {
    journalIdentityValue = await writeJournalPhase(journalPath, journal, phase);
  };

  const rollbackAfterFailure = async () => {
    const current = await readJournalRecord(journalPath);
    if (!current) return false;
    return recoverOldBackups(
      current.value,
      journalPath,
      current.identity,
      { preserveStage: current.value.verification === "callback" },
    );
  };

  try {
  for (const side of sideNames(journal)) {
    const canonicalRecord = pathValue(journal, "canonical", side);
    const backupRecord = pathValue(journal, "backup", side);
    const canonicalState = await inspectPathRecord(
      canonicalRecord,
      `canonical ${side} tree`,
    );
    if (canonicalState.present) {
      // Persist the inode that the rename is about to move before changing
      // either path.  A crash in the rename-to-journal window therefore
      // cannot leave an identity-null backup that content matching might
      // mistake for a trusted tree.
      setPathIdentity(journal, "backup", side, canonicalState.identity);
      await update(journal.phase);
      await renameDirectoryBoundary(
        canonicalRecord,
        backupRecord,
        `backup ${side}`,
      );
      await publicationFailpoint(`${side}-backup`);
      setPathIdentity(journal, "backup", side, await directoryIdentity(backupRecord.path));
    }
    await update(side === "source" ? "source-backed-up" : "ir-backed-up");
  }

  for (const side of sideNames(journal)) {
    const stageRecord = pathValue(journal, "stage", side);
    const canonicalRecord = pathValue(journal, "canonical", side);
    const stageState = await inspectPathRecord(
      stageRecord,
      `staged ${side} tree`,
    );
    if (!stageState.present) {
      throw new Error(`staged ${side} tree is missing: ${stageRecord.path}`);
    }
    // The canonical destination is absent after its old generation was
    // backed up.  Record the stage inode at that destination before rename;
    // recovery can then reject a same-content foreign replacement there.
    setPathIdentity(journal, "canonical", side, stageState.identity);
    await update(journal.phase);
    await renameDirectoryBoundary(stageRecord, canonicalRecord, `publish ${side}`);
    await publicationFailpoint(`${side}-publish`);
    setPathIdentity(journal, "stage", side, null);
    setPathIdentity(journal, "canonical", side, await directoryIdentity(canonicalRecord.path));
    await update(side === "source" ? "source-published" : "ir-published");
  }

  const finalMarker =
    operation === "ir"
      ? await verifyPair({ irRoot: irCanonical, irOnly: true })
      : await verifyPair({ sourceRoot: sourceCanonical, irRoot: irCanonical });
  if (finalMarker.pairSha256 !== journal.newPairSha256) {
    throw new Error(`published pair digest does not match the journal`);
  }
  await publicationFailpoint("audit");
  if (verifyPublished) {
    await verifyPublished();
    // Close the callback window before recording `verified`: a verifier is
    // allowed to inspect the final paths, but it must not be able to mutate a
    // different generation into the transaction after the earlier audit.
    const auditedState = await pairAt(journal, "canonical");
    if (auditedState !== "new") {
      throw new Error(`published pair changed during final audit`);
    }
  }
  await publicationFailpoint("verify");
  await update("verified");

  for (const side of sideNames(journal)) {
    const backupRecord = pathValue(journal, "backup", side);
    if (await pathPresent(backupRecord)) {
      await removeManagedDirectory(
        backupRecord,
        side,
        sideValue(journal, "oldSides", side),
        side === "ir" ? journal.oldMarker : null,
        `old ${side} backup`,
      );
      setPathIdentity(journal, "backup", side, null);
    }
    if (side === "source") {
      await update("cleanup-source");
      await publicationFailpoint("cleanup-source");
    }
  }
  await publicationFailpoint("cleanup");
  await update("cleanup");
  await clearJournalFile(journalPath, journalIdentityValue);
  } catch (error) {
    try {
      // A validation/audit error is an ordinary failed transaction, so make
      // the old pair immediately usable when both exact old backups still
      // prove complete. Crash failpoints call process.exit and never reach
      // this branch; their journal is recovered by the next invocation.
      await rollbackAfterFailure();
    } catch (rollbackError) {
      throw new AggregateError(
        [error, rollbackError],
        "failed to publish source/IR and restore the previous pair",
      );
    }
    throw error;
  }
}

async function readOwner(lockPath, { throwOnMissing = false } = {}) {
  try {
    const info = await lstat(lockPath);
    if (info.isSymbolicLink() || !info.isFile()) return null;
    const owner = JSON.parse(await readFile(lockPath, "utf8"));
    if (
      !owner ||
      typeof owner !== "object" ||
      !Number.isSafeInteger(owner.pid) ||
      owner.pid <= 0 ||
      typeof owner.token !== "string" ||
      !owner.token
    ) {
      return null;
    }
    return owner;
  } catch (error) {
    if (error?.code === "ENOENT") {
      if (throwOnMissing) throw error;
      return null;
    }
    if (error instanceof SyntaxError) return null;
    throw error;
  }
}

function processIsAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // EPERM means the process exists but is owned by another user. Treat it as
    // live; deleting that lock would permit concurrent writers.
    if (error?.code === "EPERM") return true;
    if (error?.code === "ESRCH") return false;
    throw error;
  }
}

function siblingPath(lockPath, suffix, token) {
  return join(dirname(lockPath), `${basename(lockPath)}${suffix}${token}`);
}

async function listSiblingPaths(lockPath, suffix) {
  const parent = dirname(lockPath);
  const prefix = `${basename(lockPath)}${suffix}`;
  const entries = await readdir(parent, { withFileTypes: true });
  return entries
    .filter((entry) => entry.name.startsWith(prefix))
    .sort((left, right) => comparePath(left.name, right.name))
    .map((entry) => join(parent, entry.name));
}

function candidateIdentity(lockPath, path) {
  const prefix = `${basename(lockPath)}${LOCK_CANDIDATE_SUFFIX}`;
  const name = basename(path);
  if (!name.startsWith(prefix)) return null;
  const suffix = name.slice(prefix.length);
  const separator = suffix.indexOf(".");
  if (separator <= 0 || separator === suffix.length - 1) return null;
  const pidText = suffix.slice(0, separator);
  const token = suffix.slice(separator + 1);
  if (!/^(?:0|[1-9][0-9]*)$/.test(pidText) || !token) return null;
  const pid = Number(pidText);
  if (!Number.isSafeInteger(pid) || pid <= 0) return null;
  return { pid, token };
}

async function recoverDeadCandidates(lockPath) {
  for (const path of await listSiblingPaths(lockPath, LOCK_CANDIDATE_SUFFIX)) {
    let info;
    try {
      info = await lstat(path);
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    if (info.isSymbolicLink() || !info.isFile()) {
      throw new Error(`refusing to use malformed lock candidate ${path}`);
    }
    let owner;
    try {
      owner = await readOwner(path, { throwOnMissing: true });
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    const identity = owner
      ? { pid: owner.pid, token: owner.token }
      : candidateIdentity(lockPath, path);
    if (!identity || processIsAlive(identity.pid)) continue;
    // The candidate name contains a random token and is never reused by a
    // later acquisition. Removing this exact path safely reaps a process that
    // crashed before or immediately after publishing its hard link.
    try {
      await rm(path, { force: false });
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
  }
}

async function removeStaleSibling(path) {
  let info;
  try {
    info = await lstat(path);
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  if (info.isSymbolicLink() || !info.isFile()) {
    throw new Error(`refusing to remove malformed lock ${path}`);
  }
  let owner;
  try {
    owner = await readOwner(path, { throwOnMissing: true });
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  if (!owner) {
    throw new Error(`refusing to remove malformed lock ${path}`);
  }
  if (processIsAlive(owner.pid)) {
    throw new Error(
      `spec pair publication is already locked by pid ${owner.pid}`,
    );
  }
  // A stale sibling has a unique, immutable name. Removing this exact path
  // cannot target a later owner that has acquired the canonical lock path.
  try {
    await rm(path, { force: false });
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}

async function recoverStaleSiblings(lockPath) {
  for (const path of await listSiblingPaths(lockPath, LOCK_STALE_SUFFIX)) {
    await removeStaleSibling(path);
  }
}

/**
 * Move a stale lock to a unique quarantine path before inspecting/removing it.
 * The move is the exclusive claim: if another process has already recovered
 * the old lock and installed a new owner, this call either sees ENOENT or
 * quarantines that new owner. In the latter case the mismatching file is left
 * in quarantine, where every reader still treats its live owner as the lock;
 * it is never deleted as if it were the stale owner we observed earlier.
 */
async function removeStaleLock(lockPath, owner) {
  let lockInfo;
  try {
    lockInfo = await lstat(lockPath);
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  if (lockInfo.isSymbolicLink() || !lockInfo.isFile()) {
    throw new Error(`refusing to remove malformed lock ${lockPath}`);
  }
  let current;
  try {
    current = await readOwner(lockPath, { throwOnMissing: true });
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  if (!current || current.token !== owner.token || current.pid !== owner.pid) {
    throw new Error(`refusing to remove changed or malformed lock ${lockPath}`);
  }
  const quarantinePath = siblingPath(lockPath, LOCK_STALE_SUFFIX, randomUUID());
  try {
    await rename(lockPath, quarantinePath);
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  let quarantined;
  try {
    quarantined = await readOwner(quarantinePath, { throwOnMissing: true });
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
  if (
    !quarantined ||
    quarantined.token !== owner.token ||
    quarantined.pid !== owner.pid
  ) {
    // Keep a replacement/live owner reachable via its quarantine path. The
    // next acquisition will inspect it and either reject or clean it by the
    // exact unique pathname.
    throw new Error(`refusing to remove changed or malformed lock ${lockPath}`);
  }
  if (processIsAlive(quarantined.pid)) {
    throw new Error(
      `spec pair publication is already locked by pid ${quarantined.pid}`,
    );
  }
  try {
    await rm(quarantinePath, { force: false });
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  return true;
}

/**
 * Acquire a crash-recoverable process lock. The owner JSON is fully written
 * into a private candidate before an atomic hard-link publishes it at the
 * canonical path, so there is no mkdir/owner publication gap to strand an
 * initializing lock. A live owner always wins; a dead owner is moved to a
 * unique quarantine path before recovery.
 */
export async function acquirePairLock(lockPath) {
  await mkdir(dirname(lockPath), { recursive: true });
  const owner = Object.freeze({
    pid: process.pid,
    token: randomUUID(),
    startedAt: new Date().toISOString(),
  });
  const ownerPid = owner.pid;
  const ownerToken = owner.token;
  const candidatePath = siblingPath(
    lockPath,
    LOCK_CANDIDATE_SUFFIX,
    `${ownerPid}.${ownerToken}`,
  );
  await writeFile(candidatePath, `${JSON.stringify(owner)}\n`, {
    flag: "wx",
    mode: 0o600,
  });
  let acquired = false;
  let proof;
  try {
    await recoverDeadCandidates(lockPath);
    while (true) {
      try {
        await recoverStaleSiblings(lockPath);
        await link(candidatePath, lockPath);
        acquired = true;
        break;
      } catch (error) {
        if (error?.code !== "EEXIST") {
          throw error;
        }
        let lockInfo;
        try {
          lockInfo = await lstat(lockPath);
        } catch (error) {
          if (error?.code === "ENOENT") continue;
          throw error;
        }
        if (lockInfo.isSymbolicLink() || !lockInfo.isFile()) {
          throw new Error(`refusing to use malformed lock ${lockPath}`);
        }
        let current;
        try {
          current = await readOwner(lockPath, { throwOnMissing: true });
        } catch (error) {
          if (error?.code === "ENOENT") continue;
          throw error;
        }
        if (!current) {
          throw new Error(`refusing to use malformed lock ${lockPath}`);
        }
        if (processIsAlive(current.pid)) {
          throw new Error(
            `spec pair publication is already locked by pid ${current.pid}`,
          );
        }
        await removeStaleLock(lockPath, current);
      }
    }
    const lockInfo = await lstat(lockPath);
    if (lockInfo.isSymbolicLink() || !lockInfo.isFile()) {
      throw new Error(`acquired spec pair lock is not a regular file: ${lockPath}`);
    }
    proof = Object.freeze({
      pid: ownerPid,
      token: ownerToken,
      identity: Object.freeze(journalFileIdentity(lockInfo)),
    });
  } catch (error) {
    await rm(candidatePath, { force: true }).catch(() => {});
    if (acquired) {
      const current = await readOwner(lockPath).catch(() => null);
      if (current?.pid === ownerPid && current?.token === ownerToken) {
        await rm(lockPath, { force: false }).catch(() => {});
      }
    }
    throw error;
  }
  if (acquired) await rm(candidatePath, { force: true }).catch(() => {});
  const state = { status: "held", error: null };
  const lock = {
    path: lockPath,
    owner,
    proof,
    get released() {
      return state.status === "released";
    },
    get releaseFailed() {
      return state.status === "failed";
    },
    async release() {
      if (state.status === "released") return;
      if (state.status === "failed") throw state.error;
      try {
        const candidates = [
          lockPath,
          ...(await listSiblingPaths(lockPath, LOCK_STALE_SUFFIX)),
        ];
        const owned = [];
        for (const path of candidates) {
          const current = await readOwner(path);
          if (
            current &&
            current.pid === ownerPid &&
            current.token === ownerToken
          ) {
            owned.push(path);
          }
        }
        if (owned.length !== 1) {
          throw new Error(
            `refusing to release a lock not owned by this process: ${lockPath}`,
          );
        }
        await rm(owned[0], { force: false });
        state.status = "released";
      } catch (error) {
        state.status = "failed";
        state.error = error;
        throw error;
      }
    },
  };
  Object.freeze(lock);
  pairLockStates.set(lock, state);
  return lock;
}

export async function verifyPairLockProof(lockPath, proof) {
  assertKnownFields(proof, ["pid", "token", "identity"], "pair lock proof");
  if (
    !Number.isSafeInteger(proof.pid) ||
    proof.pid <= 0 ||
    typeof proof.token !== "string" ||
    !proof.token
  ) {
    throw new Error("pair lock proof owner is invalid");
  }
  assertKnownFields(
    proof.identity,
    ["kind", "dev", "ino"],
    "pair lock proof identity",
  );
  if (
    proof.identity.kind !== "file" ||
    typeof proof.identity.dev !== "string" ||
    !proof.identity.dev ||
    typeof proof.identity.ino !== "string" ||
    !proof.identity.ino
  ) {
    throw new Error("pair lock proof identity must be a file identity");
  }
  const before = await lstat(lockPath);
  if (
    before.isSymbolicLink() ||
    !before.isFile() ||
    !identitiesEqual(journalFileIdentity(before), proof.identity)
  ) {
    throw new Error("pair lock proof does not match the lock file");
  }
  const current = await readOwner(lockPath);
  const after = await lstat(lockPath);
  if (
    after.isSymbolicLink() ||
    !after.isFile() ||
    !current ||
    current.pid !== proof.pid ||
    current.token !== proof.token ||
    !processIsAlive(current.pid) ||
    !identitiesEqual(journalFileIdentity(after), proof.identity)
  ) {
    throw new Error("pair lock proof owner or identity changed");
  }
  return true;
}

async function pairLockPathKey(lockPath) {
  const lexical = resolve(lockPath);
  let physical = lexical;
  try {
    const info = await lstat(lexical);
    if (info.isSymbolicLink()) physical = await realpath(lexical);
    const physicalInfo =
      physical === lexical ? info : await lstat(physical);
    if (!physicalInfo.isSymbolicLink() && physicalInfo.isFile()) {
      return `inode:${String(physicalInfo.dev)}:${String(physicalInfo.ino)}`;
    }
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  // The lock file may not exist before acquisition.  realPathForComparison
  // resolves existing ancestors and folds only the known Darwin aliases plus
  // the platform's filesystem spelling, so /tmp and /private/tmp converge.
  return `path:${await realPathForComparison(lexical)}`;
}

function invalidatePairLock(lock, error) {
  const state = pairLockStates.get(lock);
  if (!state || state.status !== "held") return;
  state.status = "failed";
  state.error = error;
}

export async function withPairLock(
  lockPath,
  fn,
  options = {},
) {
  assertKnownFields(options, ["verifyPublished"], "pair lock options");
  const { verifyPublished = null } = options;
  const normalizedLockPath = await pairLockPathKey(lockPath);
  const active = pairLockContext.getStore();
  if (active?.path === normalizedLockPath && !active.lock.released) {
    if (active.lock.releaseFailed) {
      throw new Error("pair lock context is invalid after release failure");
    }
    try {
      await verifyPairLockProof(lockPath, active.lock.proof);
    } catch (error) {
      const invalid = new Error("pair lock context proof is no longer valid", {
        cause: error,
      });
      invalidatePairLock(active.lock, invalid);
      throw invalid;
    }
    if (verifyPublished) {
      throw new Error("a nested pair lock cannot replace the outer recovery verifier");
    }
    return fn(active.lock);
  }
  const lock = await acquirePairLock(lockPath);
  try {
    const acquiredPath = await pairLockPathKey(lockPath);
    return await pairLockContext.run(
      { path: acquiredPath, lock },
      async () => {
        await recoverPairPublication(lockPath, { verifyPublished });
        return fn(lock);
      },
    );
  } finally {
    await lock.release();
  }
}

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const isMain =
  process.argv[1] &&
  fileURLToPath(import.meta.url) === resolve(process.argv[1]);

if (isMain) {
  const sourceRoot =
    process.env.EC_SPECS_SRC || join(repoDir, "bundle", "specs");
  const irRoot = process.env.EC_SPECS_IR || join(repoDir, "bundle", "specs-ir");
  const irOnly = process.argv.includes("--ir-only");
  try {
    await withPairLock(join(repoDir, "bundle", PAIR_LOCK_NAME), () =>
      verifyPair({ sourceRoot, irRoot, irOnly }),
    );
    process.stdout.write(
      `Verified ${irOnly ? "IR" : "source/IR pair"}: ${irRoot}\n`,
    );
  } catch (error) {
    process.stderr.write(
      `Spec pair verification failed: ${error instanceof Error ? error.message : error}\n`,
    );
    process.exitCode = 1;
  }
}
