import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import test from "node:test";

import {
  applySpecDiff as upstreamApplySpecDiff,
  getVersionFromVersionedSpec as upstreamGetVersion,
} from "../node_modules/.pnpm/@fig+autocomplete-helpers@1.0.7/node_modules/@fig/autocomplete-helpers/dist/esm/src/versions.js";

import {
  applySpecDiff,
  compileGetVersionCommand,
  compareSemver,
  derivedVersionIrRel,
  getBestVersionIndex,
  getVersionFromVersionedSpec,
  isDerivedVersionIr,
  parseVersionStdout,
  resolveVersionedPath,
  sameVersionedIrFamily,
  semverClean,
} from "./spec-versions.mjs";

test("applySpecDiff matches autocomplete-helpers 1.0.7", () => {
  const base = {
    name: "tool",
    subcommands: [
      { name: "old", description: "keep" },
      { name: "edit", description: "before", options: [{ name: "-a" }] },
    ],
    options: [{ name: "--base" }],
  };
  const diff = {
    subcommands: [
      { name: "added", description: "new" },
      { name: "edit", description: "after", options: [{ name: "-b" }] },
      { name: "old", remove: true },
    ],
    options: [{ name: "--extra" }],
  };
  assert.deepEqual(applySpecDiff(base, diff), upstreamApplySpecDiff(base, diff));
});

test("getVersionFromVersionedSpec applies every key at or below the target, or the last key", () => {
  const base = { name: "tool", subcommands: [{ name: "base" }] };
  const versions = {
    "1.2.0": { subcommands: [{ name: "mid" }] },
    "1.0.0": { subcommands: [{ name: "first" }] },
    "1.5.0": { subcommands: [{ name: "last" }] },
  };
  assert.deepEqual(
    getVersionFromVersionedSpec(base, versions, "1.2.0"),
    upstreamGetVersion(base, versions, "1.2.0"),
  );
  assert.deepEqual(
    getVersionFromVersionedSpec(base, versions, "0.9.0"),
    upstreamGetVersion(base, versions, "0.9.0"),
  );
  assert.deepEqual(
    getVersionFromVersionedSpec(base, versions, undefined),
    upstreamGetVersion(base, versions, undefined),
  );
});

test("getBestVersionIndex matches the WebView fallback to the last key", () => {
  const versions = ["8.0.0", "8.6.0"].sort(compareSemver);
  assert.equal(versions[getBestVersionIndex(versions, "8.3.0")], "8.0.0");
  assert.equal(versions[getBestVersionIndex(versions, "8.6.0")], "8.6.0");
  assert.equal(versions[getBestVersionIndex(versions, "7.0.0")], "8.6.0");
  assert.equal(versions[getBestVersionIndex(versions, undefined)], "8.6.0");
  const diffs = ["8.11.1"];
  assert.equal(diffs[getBestVersionIndex(diffs, "8.3.0")], "8.11.1");
});

test("heroku 8.3.0 selects the 8.0.0 file and the 8.11.1 diff", () => {
  const entry = {
    files: {
      "8.0.0": "heroku/8.0.0.json",
      "8.6.0": "heroku/8.6.0.json",
    },
    applied: {
      "8.0.0": { "8.11.1": "heroku/8.0.0+8.11.1.json" },
    },
  };
  assert.equal(
    resolveVersionedPath(entry, "8.3.0"),
    "heroku/8.0.0+8.11.1.json",
  );
  assert.equal(resolveVersionedPath(entry, undefined), "heroku/8.6.0.json");
});

test("bundled getVersionCommand bodies compile to reviewed selectors", async () => {
  const fig = await import(pathToFileURL("bundle/specs/fig/index.js").href);
  const heroku = await import(pathToFileURL("bundle/specs/heroku/index.js").href);
  const shopify = await import(pathToFileURL("bundle/specs/shopify/index.js").href);
  const infracost = await import(
    pathToFileURL("bundle/specs/infracost/index.js").href
  );
  const sdc = await import(
    pathToFileURL("bundle/specs/@usermn/sdc/index.js").href
  );
  assert.deepEqual(compileGetVersionCommand(fig.getVersionCommand, "fig"), {
    command: ["fig", "--version"],
    parse: "after-first-space",
  });
  assert.deepEqual(compileGetVersionCommand(heroku.getVersionCommand, "heroku"), {
    command: ["heroku", "--version"],
    parse: "regex",
    regex: "heroku\\/([0-9]+\\.[0-9]+\\.[0.9]+)",
    regexGroup: 1,
    parseFallback: "8.0.0",
  });
  assert.deepEqual(
    compileGetVersionCommand(shopify.getVersionCommand, "shopify"),
    {
      command: ["shopify", "version"],
      parse: "regex",
      regex: "\\d+\\.\\d+\\.\\d+",
      regexGroup: 0,
      parseFallback: "",
    },
  );
  assert.deepEqual(
    compileGetVersionCommand(infracost.getVersionCommand, "infracost"),
    {
      command: ["infracost", "--version"],
      parse: "semver-clean-after-space",
    },
  );
  assert.deepEqual(compileGetVersionCommand(sdc.getVersionCommand, "@usermn/sdc"), {
    command: ["npx", "@usermn/sdc", "--version"],
    parse: "stdout",
  });
  assert.throws(
    () => compileGetVersionCommand(async (exec) => exec({ command: "nope" }), "nope"),
    /not a reviewed exec\+parse selector/,
  );
});

test("version stdout parsers match the five bundled selectors", async () => {
  const fig = compileGetVersionCommand(
    (await import(pathToFileURL("bundle/specs/fig/index.js").href))
      .getVersionCommand,
    "fig",
  );
  const heroku = compileGetVersionCommand(
    (await import(pathToFileURL("bundle/specs/heroku/index.js").href))
      .getVersionCommand,
    "heroku",
  );
  assert.equal(parseVersionStdout("fig 2.16.0", fig), "2.16.0");
  assert.equal(parseVersionStdout("heroku/8.3.0 darwin-arm64", heroku), "8.3.0");
  assert.equal(parseVersionStdout("not-heroku", heroku), "8.0.0");
  assert.equal(semverClean("v0.10.30 extra"), "0.10.30");
  assert.equal(derivedVersionIrRel("heroku/8.0.0.js", "8.11.1"), "heroku/8.0.0+8.11.1.json");
  assert.equal(isDerivedVersionIr("heroku/8.0.0+8.11.1.json"), true);
  assert.equal(isDerivedVersionIr("heroku/8.0.0.json"), false);
  assert.equal(sameVersionedIrFamily("fig/1.0.0.json", "fig/1.0.0+2.16.0.json"), true);
  assert.equal(sameVersionedIrFamily("fig/1.0.0.json", "fig/2.0.0.json"), false);
});
