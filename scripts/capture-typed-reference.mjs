#!/usr/bin/env node
/**
 * Generalized typed-hook reference capture.
 *
 * `--field trigger` keeps the existing VM differential baseline. Other
 * side-effect-free fields evaluate sidecar descriptors with the typed
 * interpreter so a 2k-hook postProcess catalog does not spawn VM workers.
 * `--get-query-term` remains the closed asdf source/module review.
 */
import { pathToFileURL } from "node:url";

import { runCaptureCli } from "./capture-typed-trigger-reference.mjs";

export {
  FIELD_REFERENCE_BASELINE_KIND,
  FIELD_REFERENCE_BASELINE_VERSION,
  FIELD_REFERENCE_CORPORA,
  buildTypedFieldReference,
  buildTypedGetQueryTermReference,
  buildTypedTriggerReference,
  checkTypedFieldReference,
  checkTypedGetQueryTermReference,
  checkTypedTriggerReference,
  defaultFieldBaselinePath,
  updateTypedFieldReference,
  updateTypedGetQueryTermReference,
  updateTypedTriggerReference,
} from "./capture-typed-trigger-reference.mjs";

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  await runCaptureCli(process.argv);
}
