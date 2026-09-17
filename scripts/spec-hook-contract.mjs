/**
 * Shared build-time contract for source hooks, generated IR, and runtime hook
 * module assets. Keep this module dependency-free so the compiler can invoke
 * the independent audit while it is still evaluating as the CLI entrypoint.
 */

// These files are part of the generated specs package, but are not command
// specs. They are helper modules, declaration barrels, or dynamic selectors
// that intentionally cannot be represented by one static IR node. A new
// non-spec file must either become compilable or be reviewed here deliberately.
export const KNOWN_NON_SPEC_FILES = new Set([
  "@usermn/sdc/index.js",
  "deno/config_schema.d.js",
  "deno/deno_doc.d.js",
  "deno/generators.js",
  "dynamic/index.js",
  "fig/index.js",
  "fig/shared.js",
  "heroku/index.js",
  "heroku/shared.js",
  "infracost/index.js",
  "shopify/index.js",
]);

// Diff-versioned specs are two dynamic behaviours the WebView ran at load
// time and the compiler does not reproduce yet. Both are listed here so the
// gap is explicit, reviewed, and counted by the native-hook inventory instead
// of disappearing silently when the specs package changes.
//
// 1. Version selectors (`createVersionedSpec` index files, also present in
//    KNOWN_NON_SPEC_FILES) ran `<cli> --version` and picked the version file
//    at or below the installed version. The compiled index resolves the
//    command to the highest version file, which is exactly the WebView's
//    choice when the CLI is absent or newer than every version file.
export const KNOWN_VERSION_SELECTORS = Object.freeze([
  "@usermn/sdc/index.js",
  "fig/index.js",
  "heroku/index.js",
  "infracost/index.js",
  "shopify/index.js",
]);

// 2. A version file may also export `versions`: a map from CLI version to a
//    spec diff that `getVersionFromVersionedSpec` merged over the default
//    export for every key at or below the detected version (all keys when the
//    version was unknown). The compiler emits the default export only. Every
//    non-empty diff must be listed with its exact keys; a file that exports a
//    non-empty diff not listed here — or a listed key that no longer exists —
//    fails the compile. Empty `{}` diffs are no-ops and are not listed.
export const KNOWN_UNAPPLIED_VERSION_DIFFS = Object.freeze({
  "@usermn/sdc/0.0.0.js": Object.freeze(["0.0.7"]),
  "fig/1.0.0.js": Object.freeze([
    "1.3.1",
    "1.4.0",
    "1.4.1",
    "1.4.3",
    "1.4.7",
    "1.4.10",
  ]),
  "fig/2.0.0.js": Object.freeze([
    "2.9.0",
    "2.10.0",
    "2.11.0",
    "2.13.0",
    "2.14.2",
    "2.15.0",
    "2.16.0",
  ]),
  "heroku/8.0.0.js": Object.freeze(["8.11.1"]),
});

/**
 * Non-empty `versions` diff keys exported by one spec module, in source
 * order. An absent or non-object export yields an empty list.
 */
export function unappliedVersionDiffKeys(moduleNamespace) {
  const versions = moduleNamespace?.versions;
  if (!versions || typeof versions !== "object" || Array.isArray(versions)) {
    return [];
  }
  return Object.keys(versions).filter((key) => {
    const diff = versions[key];
    return (
      diff !== null &&
      typeof diff === "object" &&
      !Array.isArray(diff) &&
      Object.keys(diff).length > 0
    );
  });
}

/**
 * Count the functions nested anywhere inside a version diff. These are hooks
 * the WebView could have run after merging the diff; none of them is
 * extracted today, so the inventory reports them as unadapted.
 */
export function countFunctionsInValue(value, seen = new Set()) {
  if (typeof value === "function") return 1;
  if (!value || typeof value !== "object" || seen.has(value)) return 0;
  seen.add(value);
  let count = 0;
  for (const child of Object.values(value)) {
    count += countFunctionsInValue(child, seen);
  }
  return count;
}

/**
 * Compare a module's exported non-empty diffs against the reviewed allowlist.
 * Returns `null` when they match and an error message otherwise.
 *
 * An unlisted non-empty diff is always an error. A listed key the module no
 * longer exports is only an error for the canonical bundle (`enforceStale`):
 * fixture trees legitimately reuse bundled file names such as
 * `heroku/8.0.0.js` without carrying the real diffs.
 */
export function describeVersionDiffAllowlistDrift(
  relativeFile,
  moduleNamespace,
  { enforceStale = true } = {},
) {
  const actual = [...unappliedVersionDiffKeys(moduleNamespace)].sort();
  const expected = [...(KNOWN_UNAPPLIED_VERSION_DIFFS[relativeFile] ?? [])].sort();
  const unlisted = actual.filter((key) => !expected.includes(key));
  const stale = enforceStale
    ? expected.filter((key) => !actual.includes(key))
    : [];
  if (unlisted.length === 0 && stale.length === 0) return null;
  const parts = [];
  if (unlisted.length) {
    parts.push(
      `exports non-empty \`versions\` diff(s) ${JSON.stringify(unlisted)} that are not applied by the compiler and are not listed in KNOWN_UNAPPLIED_VERSION_DIFFS`,
    );
  }
  if (stale.length) {
    parts.push(
      `KNOWN_UNAPPLIED_VERSION_DIFFS lists ${JSON.stringify(stale)} which the module no longer exports as non-empty diffs`,
    );
  }
  return `${relativeFile} ${parts.join("; ")}; review the entry in scripts/spec-hook-contract.mjs or add a build-time version-diff adapter`;
}

// The Rust side consumes these nine fields. Keep source and generated IR
// spellings together so compiler, audit, classifier, and fixtures cannot drift.
export const SUPPORTED_HOOK_FIELDS = Object.freeze({
  loadSpec: "jsLoadSpec",
  trigger: "jsTrigger",
  alias: "jsAlias",
  getQueryTerm: "jsGetQueryTerm",
  generateSpec: "jsGenerateSpec",
  script: "jsScript",
  postProcess: "jsPostProcess",
  custom: "jsCustom",
  filterTemplateSuggestions: "jsFilterTemplateSuggestions",
});

export const SUPPORTED_IR_HOOK_FIELDS = Object.freeze(
  Object.values(SUPPORTED_HOOK_FIELDS),
);

export const HOOK_MODULE_MANIFEST = "hook-modules.json";
export const HOOK_MODULES_DIR = "source-modules";

// The compiler emits this root sidecar for the typed-hook slice. Keep the
// catalog limits here so the producer and audit enforce one wire contract.
export const TYPED_HOOK_SIDECAR = "typed-hooks.json";
export const TYPED_HOOK_SIDECAR_VERSION = 1;
export const TYPED_HOOK_SIDECAR_KIND = "typed-hook-expressions";
export const TYPED_HOOK_CATALOG_MAX_BYTES = 64 * 1024 * 1024;
export const TYPED_HOOK_CATALOG_MAX_HOOKS = 65_536;
export const TYPED_HOOK_ID_MAX_BYTES = 4_096;
export const TYPED_HOOK_PATH_MAX_BYTES = 4_096;
export const TYPED_HOOK_MODULE_MAX_BYTES = 255;
export const TYPED_HOOK_DESCRIPTOR_MAX_BYTES = 256 * 1024;

export function utf8ByteLength(value) {
  return Buffer.byteLength(value, "utf8");
}

export function hookFileName(id) {
  return `${String(id).replace(/[^A-Za-z0-9._-]+/g, "_")}.js`;
}
