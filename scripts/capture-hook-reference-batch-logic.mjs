/**
 * Pure classification and scheduling logic for the reference batch report.
 *
 * Keeping this module free of filesystem, VM, and probe code makes the safety
 * contract testable without adding a caller-controlled probe seam to the
 * production batch API.
 */
import { comparePath } from "./spec-pair.mjs";

const KNOWN_RESULT_STATUSES = new Set([
  "success",
  "error",
  "timeout",
  "pending",
  "output-limit",
  "worker-failed",
  "worker-result-invalid",
  "unserializable",
  "batch-error",
]);

export function canonicalize(value) {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.keys(value)
      .sort(comparePath)
      .map((key) => [key, canonicalize(value[key])]),
  );
}

export function stableJson(value) {
  return JSON.stringify(canonicalize(value));
}

export function resultCategory(result) {
  if (result?.status === "success") return "success";
  if (result?.status === "timeout") return "timeout";
  if (result?.status === "pending" || result?.pending === true) {
    return "pending";
  }
  if (KNOWN_RESULT_STATUSES.has(result?.status)) return "error";
  return "unknown";
}

export function runClassification(run) {
  return {
    category: resultCategory(run),
    status: run?.status ?? "unknown",
    stage: run?.stage ?? null,
    errorClass: run?.errorClass ?? null,
  };
}

function sameRunClassification(left, right) {
  return stableJson(runClassification(left)) === stableJson(runClassification(right));
}

export function comparePathRuns(runs) {
  const first = runs[0];
  const second = runs[1];
  if (!first || !second) {
    return {
      stable: false,
      category: "unknown",
      status: "unknown",
      outputEqual: null,
      traceEqual: null,
      errorEqual: false,
    };
  }
  const firstCategory = resultCategory(first);
  const secondCategory = resultCategory(second);
  const bothSuccessful =
    firstCategory === "success" && secondCategory === "success";
  const outputEqual = bothSuccessful
    ? stableJson(first.value) === stableJson(second.value)
    : null;
  const traceEqual =
    stableJson(first.execTrace ?? []) === stableJson(second.execTrace ?? []);
  const errorEqual =
    firstCategory !== "success" &&
    secondCategory !== "success" &&
    sameRunClassification(first, second);
  const stable =
    sameRunClassification(first, second) &&
    (bothSuccessful ? outputEqual && traceEqual : errorEqual && traceEqual);
  return {
    stable,
    category: firstCategory === secondCategory ? firstCategory : "unknown",
    status:
      first?.status === second?.status ? (first?.status ?? "unknown") : "mixed",
    stage: first?.stage === second?.stage ? (first?.stage ?? null) : "mixed",
    errorClass:
      first?.errorClass === second?.errorClass
        ? (first?.errorClass ?? null)
        : "mixed",
    outputEqual,
    traceEqual,
    errorEqual,
  };
}

export function compareRuns(sourceRuns, moduleRuns) {
  const source = comparePathRuns(sourceRuns);
  const module = comparePathRuns(moduleRuns);
  const sourceMetadataVerified = sourceRuns.every(
    (run) => run.metadataVerified === true,
  );
  const moduleMetadataVerified = moduleRuns.every(
    (run) => run.metadataVerified === true,
  );
  const metadataVerified = sourceMetadataVerified && moduleMetadataVerified;
  const bothStable = source.stable && module.stable;
  const bothSuccessful =
    bothStable &&
    source.category === "success" &&
    module.category === "success";
  const outputEqual = bothSuccessful
    ? stableJson(sourceRuns[0]?.value) === stableJson(moduleRuns[0]?.value)
    : null;
  const crossPathClassificationEqual =
    bothStable &&
    stableJson(runClassification(sourceRuns[0])) ===
      stableJson(runClassification(moduleRuns[0]));
  const crossPathTraceEqual =
    bothStable &&
    stableJson(sourceRuns[0]?.execTrace ?? []) ===
      stableJson(moduleRuns[0]?.execTrace ?? []);
  const crossPathErrorEqual =
    bothStable &&
    !bothSuccessful &&
    crossPathClassificationEqual &&
    crossPathTraceEqual;
  const differingTraceOutput =
    bothStable &&
    (crossPathTraceEqual === false ||
      (bothSuccessful && outputEqual === false));
  const differingStatus = bothStable && !crossPathClassificationEqual;
  const crossPathDifference = differingStatus || differingTraceOutput;
  const unstable = !source.stable || !module.stable;
  let status = "inconclusive";
  if (unstable || !metadataVerified || crossPathDifference) {
    status = "divergent";
  } else if (bothSuccessful && outputEqual && crossPathTraceEqual) {
    status = "synthetic-source-module-confirmed";
  }
  return {
    status,
    source,
    module,
    metadataVerified,
    sourceMetadataVerified,
    moduleMetadataVerified,
    outputEqual,
    traceEqual: crossPathTraceEqual,
    crossPathClassificationEqual,
    crossPathErrorEqual,
    crossPathTraceEqual,
    differingStatus,
    differingTraceOutput,
    crossPathDifference,
    unstable,
    baselineConfirmed:
      status === "synthetic-source-module-confirmed" && metadataVerified,
  };
}

export async function runWithConcurrency(
  items,
  concurrency,
  fn,
  onError = (error) => ({
    status: "batch-error",
    errorClass: error?.name || "Error",
    message: error?.message || String(error),
  }),
) {
  const output = new Array(items.length);
  let cursor = 0;
  async function worker() {
    while (true) {
      const index = cursor++;
      if (index >= items.length) return;
      try {
        output[index] = await fn(items[index], index);
      } catch (error) {
        output[index] = onError(error);
      }
    }
  }
  await Promise.all(
    Array.from({ length: Math.min(concurrency, items.length) }, () => worker()),
  );
  return output;
}

/**
 * Compare the evidence captured before and after the workers.  The batch
 * caller supplies the filesystem snapshot (including IR digests); this pure
 * decision point is what turns an in-flight IR/module replacement into an
 * inconclusive report instead of allowing a stale baseline to pass.
 */
export function assessEvidenceStability({
  harnessBefore,
  harnessAfter,
  auditBefore,
  auditAfter,
  auditFileBefore = null,
  auditFileAfter = null,
  artifactsBefore,
  artifactsAfter,
  artifactsError = null,
}) {
  const harnessStable = stableJson(harnessBefore) === stableJson(harnessAfter);
  const auditStable =
    auditBefore === auditAfter && auditFileBefore === auditFileAfter;
  const artifactsStable =
    !artifactsError &&
    stableJson(artifactsBefore) === stableJson(artifactsAfter);
  let artifactsDrift = artifactsError;
  if (!artifactsDrift && !artifactsStable) {
    artifactsDrift = "audited source/module evidence changed during run";
  }
  return {
    harnessStable,
    auditStable,
    artifactsStable,
    artifactsDrift,
    evidenceStable: harnessStable && auditStable && artifactsStable,
  };
}

export function aggregateStats(instanceReports, probesPerPath = 2, probesPerInstance = 4) {
  const runs = instanceReports.flatMap((report) => [
    ...report.runs.source,
    ...report.runs.module,
  ]);
  const sourceRuns = instanceReports.flatMap((report) => report.runs.source);
  const moduleRuns = instanceReports.flatMap((report) => report.runs.module);
  const sourceShaVerified = sourceRuns.filter(
    (run) => run.sourceShaVerified === true,
  ).length;
  const moduleShaVerified = moduleRuns.filter(
    (run) => run.moduleShaVerified === true,
  ).length;
  const manifestShaVerified = moduleRuns.filter(
    (run) => run.manifestShaVerified === true,
  ).length;
  const metadataVerified = runs.filter(
    (run) => run.metadataVerified === true,
  ).length;
  const errors = runs.filter((run) => run.category === "error").length;
  const pending = runs.filter((run) => run.category === "pending").length;
  const timeout = runs.filter((run) => run.category === "timeout").length;
  const unknown = runs.filter((run) => run.category === "unknown").length;
  const differingTraceOutput = instanceReports.filter(
    (report) => report.comparison.differingTraceOutput,
  ).length;
  const differingStatus = instanceReports.filter(
    (report) => report.comparison.differingStatus,
  ).length;
  const crossPathDifferences = instanceReports.filter(
    (report) => report.comparison.crossPathDifference,
  ).length;
  const successfulProbes = runs.filter(
    (run) => run.category === "success",
  ).length;
  const confirmedInstances = instanceReports.filter(
    (report) => report.comparison.baselineConfirmed,
  ).length;
  const unstableInstances = instanceReports.filter(
    (report) => report.comparison.unstable,
  ).length;
  return {
    selectedInstances: instanceReports.length,
    attemptedProbes: runs.length,
    expectedProbes: instanceReports.length * probesPerInstance,
    expectedProbesPerPath: instanceReports.length * probesPerPath,
    successfulProbes,
    sourceShaVerified,
    moduleShaVerified,
    manifestShaVerified,
    metadataVerified,
    errors,
    differingTraceOutput,
    differingStatus,
    crossPathDifferences,
    pending,
    timeout,
    unknown,
    confirmedInstances,
    inconclusiveInstances: instanceReports.filter(
      (report) => report.comparison.status === "inconclusive",
    ).length,
    divergentInstances: instanceReports.filter(
      (report) => report.comparison.status === "divergent",
    ).length,
    unstableInstances,
  };
}
