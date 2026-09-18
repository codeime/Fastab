#!/usr/bin/env node
/**
 * Literal-regex gate for typed hook IR.
 *
 * Only source-text regular expression literals are admitted.  The accepted
 * subset is the intersection of JavaScript `RegExp` (without the `v` flag)
 * and what `fancy-regex` can compile, so the JS evaluator and the Rust
 * evaluator can share one `{pattern, flags}` pair.
 */
export const TYPED_REGEX_MAX_PATTERN_UNITS = 4 * 1024;
export const TYPED_REGEX_ALLOWED_FLAGS = Object.freeze(["g", "i", "m", "s", "u", "y"]);

export class TypedRegexError extends Error {
  constructor(message, { code = "unsupported-regex" } = {}) {
    super(message);
    this.name = "TypedRegexError";
    this.code = code;
  }
}

function fail(message, code = "unsupported-regex") {
  throw new TypedRegexError(message, { code });
}

function isSafeIntegerFlagOrder(flags) {
  const allowed = new Set(TYPED_REGEX_ALLOWED_FLAGS);
  const seen = new Set();
  for (const flag of flags) {
    if (!allowed.has(flag)) return false;
    if (seen.has(flag)) return false;
    seen.add(flag);
  }
  return true;
}

/**
 * Reject constructs fancy-regex / the T2.2 contract will not honour:
 * the `v` flag, variable-length lookbehind, and `\p` / `\P` that are not
 * Unicode property escapes of the form `\p{...}` / `\P{...}`.
 */
export function assertTypedRegexLiteral(pattern, flags = "") {
  if (typeof pattern !== "string") {
    fail("regex pattern must be a string");
  }
  if (typeof flags !== "string") {
    fail("regex flags must be a string");
  }
  if (pattern.length > TYPED_REGEX_MAX_PATTERN_UNITS) {
    fail("regex pattern exceeds the UTF-16 code-unit limit", "complexity");
  }
  if (flags.includes("v")) {
    fail("regex v flag is not representable");
  }
  if (!isSafeIntegerFlagOrder(flags)) {
    fail(`regex flags ${JSON.stringify(flags)} are not representable`);
  }
  if (/(?<!\\)(?:\\\\)*\(\?</.test(pattern) || /(?<!\\)(?:\\\\)*\(\?<=/.test(pattern)) {
    fail("regex lookbehind is not representable");
  }
  const propertyEscape = /\\[pP](?!\{)/g;
  if (propertyEscape.test(pattern)) {
    fail("bare \\p / \\P escapes are not representable");
  }
  try {
    new RegExp(pattern, flags.replaceAll("y", ""));
  } catch (error) {
    fail(`regex is not valid JavaScript: ${error.message}`, "syntax");
  }
  const fancyPattern = escapeCharacterClassHyphens(pattern);
  try {
    new RegExp(fancyPattern, flags.replaceAll("y", ""));
  } catch (error) {
    fail(`regex is not valid after fancy-regex translation: ${error.message}`, "syntax");
  }
  return { pattern: fancyPattern, flags };
}

function lastClassAtom(output) {
  if (output.length >= 2 && output[output.length - 2] === "\\") return null;
  return output[output.length - 1] ?? null;
}

function isSimpleRangeEndpoint(value) {
  return typeof value === "string" && /^[A-Za-z0-9]$/.test(value);
}

/**
 * JavaScript treats `-` after a class escape (`\w`, `\d`, …) as a literal.
 * fancy-regex parses it as a range and rejects `[\w-+]`.  Escape those
 * interior hyphens; keep a simple `a-z` range as a range.
 */
export function escapeCharacterClassHyphens(pattern) {
  let out = "";
  let inClass = false;
  let atClassStart = false;
  let escaped = false;
  for (let index = 0; index < pattern.length; index += 1) {
    const ch = pattern[index];
    if (escaped) {
      out += ch;
      escaped = false;
      atClassStart = false;
      continue;
    }
    if (ch === "\\") {
      out += ch;
      escaped = true;
      continue;
    }
    if (!inClass) {
      if (ch === "[") {
        inClass = true;
        atClassStart = true;
      }
      out += ch;
      continue;
    }
    if (atClassStart && ch === "^") {
      out += ch;
      continue;
    }
    if (ch === "]" && !atClassStart) {
      inClass = false;
      out += ch;
      continue;
    }
    if (ch === "-" && !atClassStart) {
      const next = pattern[index + 1];
      const prev = lastClassAtom(out);
      const nextIsSimple = next != null && next !== "]" && isSimpleRangeEndpoint(next);
      if (!(isSimpleRangeEndpoint(prev) && nextIsSimple)) {
        out += "\\-";
        atClassStart = false;
        continue;
      }
    }
    out += ch;
    atClassStart = false;
  }
  return out;
}

export function typedRegexFromAcornLiteral(node) {
  if (!node || node.type !== "Literal" || !node.regex) {
    fail("regex operand must be a regular expression literal");
  }
  return assertTypedRegexLiteral(node.regex.pattern, node.regex.flags);
}

function applySticky(regex, flags, value, startIndex) {
  if (!flags.includes("y")) {
    regex.lastIndex = 0;
    return regex;
  }
  const copy = new RegExp(regex.source, `${regex.flags.replaceAll("y", "")}g`);
  copy.lastIndex = startIndex;
  return copy;
}

export function regexTest(pattern, flags, value) {
  const { pattern: source, flags: normalized } = assertTypedRegexLiteral(
    pattern,
    flags,
  );
  const regex = applySticky(new RegExp(source, normalized), normalized, value, 0);
  if (normalized.includes("y")) {
    const match = regex.exec(value);
    return match !== null && match.index === 0;
  }
  return regex.test(value);
}

export function regexMatch(pattern, flags, value) {
  const { pattern: source, flags: normalized } = assertTypedRegexLiteral(
    pattern,
    flags,
  );
  if (normalized.includes("g")) {
    return value.match(new RegExp(source, normalized));
  }
  const regex = applySticky(new RegExp(source, normalized), normalized, value, 0);
  const match = regex.exec(value);
  if (!match) return null;
  return [...match];
}

export function regexMatchAll(pattern, flags, value) {
  const { pattern: source, flags: normalized } = assertTypedRegexLiteral(
    pattern,
    flags,
  );
  const globalFlags = normalized.includes("g") ? normalized : `${normalized}g`;
  const regex = new RegExp(source, globalFlags.replaceAll("y", ""));
  return [...value.matchAll(regex)].map((match) => [...match]);
}

export function regexReplace(pattern, flags, value, replacement) {
  const { pattern: source, flags: normalized } = assertTypedRegexLiteral(
    pattern,
    flags,
  );
  return value.replace(new RegExp(source, normalized.replaceAll("y", "")), replacement);
}

export function stringSplitRegex(pattern, flags, value) {
  const { pattern: source, flags: normalized } = assertTypedRegexLiteral(
    pattern,
    flags,
  );
  return value.split(new RegExp(source, normalized.replaceAll("y", "")));
}
