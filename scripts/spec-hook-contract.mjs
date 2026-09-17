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
