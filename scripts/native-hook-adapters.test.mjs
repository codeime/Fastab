import assert from "node:assert/strict";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  NATIVE_ADAPTER_CATALOG_KIND,
  NATIVE_ADAPTER_CATALOG_VERSION,
  adapterKey,
  allowUnadaptedHooks,
  catalogText,
  formatUnadaptedHookError,
  isRegisteredNativeAdapter,
  loadNativeHookAdapters,
  validateAdapterCatalog,
} from "./native-hook-adapters.mjs";

const SAMPLE_SHA =
  "16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad";

function sampleCatalog(adapters = [
  {
    bodySha256: SAMPLE_SHA,
    field: "filterTemplateSuggestions",
    representativeHookId: "direnv#filterTemplateSuggestions#0",
    reason: "factory helper e disagrees across call sites",
  },
]) {
  return {
    version: NATIVE_ADAPTER_CATALOG_VERSION,
    kind: NATIVE_ADAPTER_CATALOG_KIND,
    adapters,
  };
}

test("validates the native adapter catalog and rejects drift", () => {
  const catalog = validateAdapterCatalog(sampleCatalog());
  assert.equal(catalog.adapters.length, 1);
  assert.equal(
    adapterKey("filterTemplateSuggestions", SAMPLE_SHA),
    `filterTemplateSuggestions\0${SAMPLE_SHA}`,
  );
  assert.throws(
    () => validateAdapterCatalog({ ...sampleCatalog(), version: 2 }),
    /unsupported/,
  );
  assert.throws(
    () => validateAdapterCatalog({ ...sampleCatalog(), kind: "other" }),
    /unsupported/,
  );
  assert.throws(
    () =>
      validateAdapterCatalog(
        sampleCatalog([
          {
            bodySha256: "zz",
            field: "postProcess",
            representativeHookId: "x#postProcess#0",
            reason: "no",
          },
        ]),
      ),
    /64-character hex/,
  );
  assert.throws(
    () =>
      validateAdapterCatalog(
        sampleCatalog([
          ...sampleCatalog().adapters,
          {
            bodySha256: SAMPLE_SHA,
            field: "filterTemplateSuggestions",
            representativeHookId: "direnv#filterTemplateSuggestions#1",
            reason: "duplicate",
          },
        ]),
      ),
    /duplicate native adapter/,
  );
});

test("looks up registered leftovers and formats the compile hard-fail", () => {
  const catalog = sampleCatalog();
  assert.equal(
    isRegisteredNativeAdapter(SAMPLE_SHA, "filterTemplateSuggestions", catalog),
    true,
  );
  assert.equal(
    isRegisteredNativeAdapter(SAMPLE_SHA, "postProcess", catalog),
    false,
  );
  const message = formatUnadaptedHookError([
    { id: "direnv#filterTemplateSuggestions#0", field: "filterTemplateSuggestions" },
    { id: "git#postProcess#8", field: "postProcess" },
  ]);
  assert.match(message, /2 hook/);
  assert.match(message, /EC_ALLOW_UNADAPTED=1/);
});

test("loads a catalog file and writes a stable order", async () => {
  const root = await mkdtemp(join(tmpdir(), "easy-complete-adapters-"));
  const path = join(root, "adapters.json");
  try {
    const catalog = sampleCatalog([
      {
        bodySha256: "b".repeat(64),
        field: "postProcess",
        representativeHookId: "later#postProcess#0",
        reason: "later",
      },
      {
        bodySha256: "a".repeat(64),
        field: "postProcess",
        representativeHookId: "earlier#postProcess#0",
        reason: "earlier",
      },
    ]);
    await writeFile(path, JSON.stringify(catalog));
    const loaded = await loadNativeHookAdapters(path);
    assert.equal(loaded.adapters.length, 2);
    const text = catalogText(loaded);
    assert.ok(text.indexOf("a".repeat(64)) < text.indexOf("b".repeat(64)));
    assert.ok(text.endsWith("\n"));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("EC_ALLOW_UNADAPTED is the staged compile escape hatch", () => {
  const previous = process.env.EC_ALLOW_UNADAPTED;
  try {
    delete process.env.EC_ALLOW_UNADAPTED;
    assert.equal(allowUnadaptedHooks(), false);
    process.env.EC_ALLOW_UNADAPTED = "1";
    assert.equal(allowUnadaptedHooks(), true);
    process.env.EC_ALLOW_UNADAPTED = "0";
    assert.equal(allowUnadaptedHooks(), false);
  } finally {
    if (previous === undefined) delete process.env.EC_ALLOW_UNADAPTED;
    else process.env.EC_ALLOW_UNADAPTED = previous;
  }
});
