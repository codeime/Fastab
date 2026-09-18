#!/usr/bin/env node
/**
 * Named native-adapter catalog shared by classify and compile-spec-ir.
 *
 * `adapters.json` is generated from the Rust registry
 * (`cargo run -p ec_engine --example dump-adapters`) and is the only
 * allowlist that turns a typed-compile failure into `native-adapter`
 * instead of `requires-native-adapter` / a compiler hard error.
 */
import { readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { comparePath } from "./spec-pair.mjs";

const repoDir = join(dirname(fileURLToPath(import.meta.url)), "..");

export const NATIVE_ADAPTER_CATALOG_VERSION = 1;
export const NATIVE_ADAPTER_CATALOG_KIND = "native-hook-adapters";
export const SIDE_EFFECT_FREE_ADAPTER_FIELDS = Object.freeze([
  "trigger",
  "getQueryTerm",
  "postProcess",
  "script",
  "filterTemplateSuggestions",
]);

export const defaultAdaptersPath = join(
  repoDir,
  "crates",
  "ec_engine",
  "testdata",
  "native-hooks",
  "adapters.json",
);

function adapterPath() {
  return process.env.EC_NATIVE_ADAPTERS_JSON || defaultAdaptersPath;
}

export function adapterKey(field, bodySha256) {
  return `${field}\0${bodySha256}`;
}

export function validateAdapterCatalog(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("native adapter catalog must be an object");
  }
  if (value.version !== NATIVE_ADAPTER_CATALOG_VERSION) {
    throw new Error(
      `native adapter catalog version ${value.version} is unsupported`,
    );
  }
  if (value.kind !== NATIVE_ADAPTER_CATALOG_KIND) {
    throw new Error(`native adapter catalog kind ${value.kind} is unsupported`);
  }
  if (!Array.isArray(value.adapters)) {
    throw new Error("native adapter catalog adapters must be an array");
  }
  const keys = new Set();
  for (const entry of value.adapters) {
    if (!entry || typeof entry !== "object") {
      throw new Error("native adapter entry must be an object");
    }
    if (typeof entry.bodySha256 !== "string" || !/^[0-9a-f]{64}$/.test(entry.bodySha256)) {
      throw new Error("native adapter bodySha256 must be a 64-character hex digest");
    }
    if (typeof entry.field !== "string" || !entry.field) {
      throw new Error("native adapter field must be a non-empty string");
    }
    if (typeof entry.representativeHookId !== "string" || !entry.representativeHookId) {
      throw new Error("native adapter representativeHookId must be a non-empty string");
    }
    if (typeof entry.reason !== "string" || !entry.reason) {
      throw new Error("native adapter reason must be a non-empty string");
    }
    const key = adapterKey(entry.field, entry.bodySha256);
    if (keys.has(key)) {
      throw new Error(`duplicate native adapter ${entry.field}:${entry.bodySha256}`);
    }
    keys.add(key);
  }
  return value;
}

export function catalogIndex(catalog) {
  return new Map(
    catalog.adapters.map((entry) => [
      adapterKey(entry.field, entry.bodySha256),
      entry,
    ]),
  );
}

export async function loadNativeHookAdapters(path = adapterPath()) {
  let text;
  try {
    text = await readFile(path, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") {
      return validateAdapterCatalog({
        version: NATIVE_ADAPTER_CATALOG_VERSION,
        kind: NATIVE_ADAPTER_CATALOG_KIND,
        adapters: [],
      });
    }
    throw error;
  }
  return validateAdapterCatalog(JSON.parse(text));
}

export function isRegisteredNativeAdapter(bodySha256, field, catalog) {
  if (!catalog) return false;
  return catalogIndex(catalog).has(adapterKey(field, bodySha256));
}

export function allowUnadaptedHooks() {
  return process.env.EC_ALLOW_UNADAPTED === "1";
}

export function formatUnadaptedHookError(unadapted) {
  const sample = unadapted
    .slice(0, 8)
    .map((entry) => `${entry.id} (${entry.field})`)
    .join(", ");
  const extra =
    unadapted.length > 8 ? ` and ${unadapted.length - 8} more` : "";
  return (
    `typed compile failed for ${unadapted.length} side-effect-free hook(s) ` +
    `with no named adapter: ${sample}${extra}. Register a native adapter ` +
    `in crates/ec_engine/src/native_adapters or set EC_ALLOW_UNADAPTED=1 to stage.`
  );
}

export function catalogText(catalog) {
  const normalized = {
    version: NATIVE_ADAPTER_CATALOG_VERSION,
    kind: NATIVE_ADAPTER_CATALOG_KIND,
    adapters: [...catalog.adapters].sort(
      (left, right) =>
        comparePath(left.field, right.field) ||
        comparePath(left.bodySha256, right.bodySha256),
    ),
  };
  return `${JSON.stringify(normalized, null, 2)}\n`;
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const catalog = await loadNativeHookAdapters();
  if (process.argv.includes("--write")) {
    await writeFile(adapterPath(), catalogText(catalog));
  }
  process.stdout.write(
    `${catalog.adapters.length} named native adapters in ${adapterPath()}\n`,
  );
}
