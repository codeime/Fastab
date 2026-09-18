/**
 * Build-time port of `@fig/autocomplete-helpers` versioned-spec merge
 * (`applySpecDiff` / `getVersionFromVersionedSpec` / `getBestVersionIndex`).
 *
 * The algorithm is kept line-for-line with helpers 1.0.7. Tests compare
 * against that library so a silent drift fails closed. Runtime JS is not
 * involved: the compiler applies diffs and the Rust registry only selects
 * among the emitted IR files.
 */
const isPrimitive = (obj) => obj !== Object(obj);

const deepEqual = (left, right) => {
  if (left === right) return true;
  if (isPrimitive(left) && isPrimitive(right)) return left === right;
  if (Object.keys(left).length !== Object.keys(right).length) return false;
  for (const key in left) {
    if (!(key in right && deepEqual(left[key], right[key]))) {
      return false;
    }
  }
  return true;
};

const mergeSimpleObject = (original, diff) => ({ ...original, ...diff });

const toArray = (value) => {
  if (!value) return [];
  return Array.isArray(value) ? value : [value];
};

const namesMatch = (left, right) => {
  const leftNames = new Set(toArray(left.name));
  return toArray(right.name).some((name) => leftNames.has(name));
};

const mergeNamedArrayDiff = (current, diffs, mergeFn) => {
  const updated = [];
  const mergedIndexes = new Set();
  diffs.forEach((diff) => {
    const idx = current.findIndex(
      (curr, index) => !mergedIndexes.has(index) && namesMatch(curr, diff),
    );
    if (idx === -1) {
      updated.push(diff);
    } else {
      mergedIndexes.add(idx);
      if (!("remove" in diff)) {
        const merged = mergeFn(current[idx], diff);
        if (merged !== null) {
          updated.push(merged);
        }
      }
    }
  });
  current.forEach((curr, index) => {
    if (!mergedIndexes.has(index)) {
      updated.push(curr);
    }
  });
  return updated;
};

const mergeOrderedArrays = (current, diffs, mergeFn) =>
  diffs
    .filter((diff) => !("remove" in diff) || !diff.remove)
    .map((diff, idx) => mergeFn(current[idx], diff))
    .filter((merged) => merged !== null);

const makeNamedProcessor = (propertyMapping) => ({
  merge: (current, diff) => {
    if (diff === null) {
      return current;
    }
    const merged = { ...current, ...diff };
    for (const mapKey in propertyMapping) {
      const key = mapKey;
      const property = propertyMapping[key];
      if (diff[key] && property !== undefined) {
        merged[key] = property.merge(current[key], diff[key]);
      }
    }
    return merged;
  },
});

const argArrayProcessor = {
  merge: (current, diff) =>
    mergeOrderedArrays(toArray(current), toArray(diff), mergeSimpleObject),
};

const optionProcessor = makeNamedProcessor({
  args: argArrayProcessor,
});

const subcommandProcessor = makeNamedProcessor({
  subcommands: {
    merge: (current, diff) =>
      mergeNamedArrayDiff(toArray(current), toArray(diff), subcommandProcessor.merge),
  },
  options: {
    merge: (current, diff) =>
      mergeNamedArrayDiff(toArray(current), toArray(diff), optionProcessor.merge),
  },
  args: argArrayProcessor,
});

/**
 * npm `semver.compare` for the x.y.z (optional prerelease) keys used by the
 * bundled version files. Invalid identifiers sort after valid ones so a
 * compile-time key list cannot silently drop a reviewed version.
 */
export function compareSemver(left, right) {
  const parsedLeft = parseSemver(left);
  const parsedRight = parseSemver(right);
  if (!parsedLeft && !parsedRight) return left < right ? -1 : left > right ? 1 : 0;
  if (!parsedLeft) return 1;
  if (!parsedRight) return -1;
  for (let index = 0; index < 3; index += 1) {
    if (parsedLeft.numbers[index] !== parsedRight.numbers[index]) {
      return parsedLeft.numbers[index] > parsedRight.numbers[index] ? 1 : -1;
    }
  }
  if (!parsedLeft.prerelease && parsedRight.prerelease) return 1;
  if (parsedLeft.prerelease && !parsedRight.prerelease) return -1;
  if (!parsedLeft.prerelease && !parsedRight.prerelease) return 0;
  const leftParts = parsedLeft.prerelease.split(".");
  const rightParts = parsedRight.prerelease.split(".");
  const length = Math.max(leftParts.length, rightParts.length);
  for (let index = 0; index < length; index += 1) {
    if (index >= leftParts.length) return -1;
    if (index >= rightParts.length) return 1;
    if (leftParts[index] === rightParts[index]) continue;
    const leftNumeric = /^\d+$/.test(leftParts[index]);
    const rightNumeric = /^\d+$/.test(rightParts[index]);
    if (leftNumeric && rightNumeric) {
      return Number(leftParts[index]) > Number(rightParts[index]) ? 1 : -1;
    }
    if (leftNumeric !== rightNumeric) return leftNumeric ? -1 : 1;
    return leftParts[index] < rightParts[index] ? -1 : 1;
  }
  return 0;
}

function parseSemver(value) {
  const match = String(value ?? "").trim().match(
    /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$/,
  );
  if (!match) return null;
  return {
    numbers: [Number(match[1]), Number(match[2]), Number(match[3])],
    prerelease: match[4] ?? "",
  };
}

export function getBestVersionIndex(versions, target) {
  if (!target) return versions.length - 1;
  for (let index = versions.length - 1; index >= 0; index -= 1) {
    if (compareSemver(versions[index], target) <= 0) {
      return index;
    }
  }
  return versions.length - 1;
}

export const applySpecDiff = (spec, diff) =>
  subcommandProcessor.merge(spec, { ...diff, name: spec.name });

export function getVersionFromVersionedSpec(base, versions, target) {
  const versionNames = Object.keys(versions).sort(compareSemver);
  const versionIndex = getBestVersionIndex(versionNames, target);
  const spec = versionNames
    .slice(0, versionIndex + 1)
    .map((name) => versions[name])
    .reduce(applySpecDiff, base);
  return { spec, version: versionNames[versionIndex] };
}

export function isEmptyVersionDiff(diff) {
  return (
    diff == null ||
    typeof diff !== "object" ||
    Array.isArray(diff) ||
    Object.keys(diff).length === 0
  );
}

export function versionDiffKeys(versions) {
  if (!versions || typeof versions !== "object" || Array.isArray(versions)) {
    return [];
  }
  return Object.keys(versions).sort(compareSemver);
}

export function derivedVersionIrRel(sourceRel, diffVersion) {
  const normalized = sourceRel.replaceAll("\\", "/");
  const slash = normalized.lastIndexOf("/");
  const directory = slash === -1 ? "" : normalized.slice(0, slash);
  const stem = normalized.slice(slash + 1).replace(/\.js$/, "");
  const name = `${stem}+${diffVersion}.json`;
  return directory ? `${directory}/${name}` : name;
}

export function isDerivedVersionIr(relativeFile) {
  return /(?:^|\/)[^/]+\+[^/+]+\.json$/.test(String(relativeFile).replaceAll("\\", "/"));
}

export function derivedVersionIrFamily(relativeFile) {
  return String(relativeFile)
    .replaceAll("\\", "/")
    .replace(/\.json$/, "")
    .replace(/\+[^/]+$/, "");
}

export function derivedVersionIrBaseSource(relativeFile) {
  const family = derivedVersionIrFamily(relativeFile);
  return family ? `${family}.js` : null;
}

export function sameVersionedIrFamily(left, right) {
  if (left === right) return true;
  if (typeof left !== "string" || typeof right !== "string") return false;
  return derivedVersionIrFamily(left) === derivedVersionIrFamily(right);
}

const SEMVER_CLEAN_RE =
  /v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?/;

export function semverClean(value) {
  const text = String(value ?? "").trim().replace(/^[v=]+/, "");
  const match = text.match(SEMVER_CLEAN_RE);
  return match ? match[0] : null;
}

export function parseVersionStdout(stdout, selector) {
  const text = String(stdout ?? "");
  switch (selector.parse) {
    case "stdout":
      return text;
    case "after-first-space": {
      const index = text.indexOf(" ");
      return index === -1 ? text : text.slice(index + 1);
    }
    case "semver-clean":
      return semverClean(text);
    case "semver-clean-after-space": {
      const index = text.indexOf(" ");
      return semverClean(index === -1 ? text : text.slice(index + 1));
    }
    case "regex": {
      if (!selector.regex) return selector.parseFallback ?? null;
      const match = text.match(new RegExp(selector.regex));
      if (!match) return selector.parseFallback ?? null;
      const group = selector.regexGroup ?? 0;
      return match[group] ?? selector.parseFallback ?? null;
    }
    default:
      throw new Error(`unknown version parse kind ${selector.parse}`);
  }
}

/**
 * Compile one `getVersionCommand` into the closed selector stored in
 * `index.json`. Unknown shapes fail closed: a new helper must be reviewed
 * here instead of being dropped.
 */
export function compileGetVersionCommand(fn, commandName) {
  if (typeof fn !== "function") {
    return {
      command: [commandName, "--version"],
      parse: "semver-clean",
    };
  }
  const src = Function.prototype.toString.call(fn);
  // Normalize only `:` / `,` spacing so pretty-printed fixtures match the
  // minified bundled bodies. Do not collapse all whitespace — that turns
  // fig's `indexOf(" ")` into `indexOf("")`.
  const markers = src.replace(/\s*:\s*/g, ":").replace(/\s*,\s*/g, ",");
  if (
    markers.includes('command:"fig"') &&
    markers.includes('args:["--version"]') &&
    src.includes('indexOf(" ")')
  ) {
    return {
      command: ["fig", "--version"],
      parse: "after-first-space",
    };
  }
  if (markers.includes('command:"heroku"') && markers.includes('args:["--version"]') && src.includes(".exec(")) {
    const fallback = src.match(/return\s*\w+\s*\?\s*\w+\[1\]\s*:\s*"([^"]+)"/)?.[1] ?? "8.0.0";
    return {
      command: ["heroku", "--version"],
      parse: "regex",
      regex: "heroku\\/([0-9]+\\.[0-9]+\\.[0.9]+)",
      regexGroup: 1,
      parseFallback: fallback,
    };
  }
  if (
    markers.includes('command:"shopify"') &&
    markers.includes('args:["version"]') &&
    src.includes("\\d+\\.\\d+\\.\\d+")
  ) {
    return {
      command: ["shopify", "version"],
      parse: "regex",
      regex: "\\d+\\.\\d+\\.\\d+",
      regexGroup: 0,
      parseFallback: "",
    };
  }
  if (markers.includes('command:"infracost"') && markers.includes('args:["--version"]') && src.includes(".clean")) {
    return {
      command: ["infracost", "--version"],
      parse: "semver-clean-after-space",
    };
  }
  if (markers.includes('command:"npx"') && src.includes('"@usermn/sdc"') && markers.includes('args:["@usermn/sdc","--version"]')) {
    return {
      command: ["npx", "@usermn/sdc", "--version"],
      parse: "stdout",
    };
  }
  throw new Error(
    `${commandName} getVersionCommand is not a reviewed exec+parse selector; add an explicit compiler adapter in scripts/spec-versions.mjs`,
  );
}

export function resolveVersionedPath(entry, detectedVersion) {
  const fileVersions = Object.keys(entry.files).sort(compareSemver);
  if (!fileVersions.length) return null;
  const fileIndex = getBestVersionIndex(fileVersions, detectedVersion || undefined);
  const fileVersion = fileVersions[fileIndex];
  const filePath = entry.files[fileVersion];
  const applied = entry.applied?.[fileVersion] ?? {};
  const diffVersions = Object.keys(applied).sort(compareSemver);
  if (!diffVersions.length) return filePath;
  const diffIndex = getBestVersionIndex(diffVersions, detectedVersion || undefined);
  return applied[diffVersions[diffIndex]] ?? filePath;
}

/**
 * Standalone source injected into closure-preserving hook modules so a
 * versions-diff function is addressed on the merged object, not the default
 * export. Must stay a function declaration (no imports).
 */
export function applySpecDiffModuleSource() {
  return `function __ec_isPrimitive(obj){return obj!==Object(obj)}
function __ec_deepEqual(a,b){if(a===b)return true;if(__ec_isPrimitive(a)&&__ec_isPrimitive(b))return a===b;if(Object.keys(a).length!==Object.keys(b).length)return false;for(const k in a){if(!(k in b&&__ec_deepEqual(a[k],b[k])))return false}return true}
function __ec_mergeSimple(original,diff){return Object.assign({},original,diff)}
function __ec_toArray(x){if(!x)return[];return Array.isArray(x)?x:[x]}
function __ec_namesMatch(x,y){const names=new Set(__ec_toArray(x.name));return __ec_toArray(y.name).some((n)=>names.has(n))}
function __ec_mergeNamed(current,diffs,mergeFn){const updated=[];const merged=new Set();diffs.forEach((diff)=>{const idx=current.findIndex((curr,i)=>!merged.has(i)&&__ec_namesMatch(curr,diff));if(idx===-1)updated.push(diff);else{merged.add(idx);if(!("remove" in diff)){const next=mergeFn(current[idx],diff);if(next!==null)updated.push(next)}}});current.forEach((curr,i)=>{if(!merged.has(i))updated.push(curr)});return updated}
function __ec_mergeOrdered(current,diffs,mergeFn){return diffs.filter((d)=>!("remove" in d)||!d.remove).map((d,i)=>mergeFn(current[i],d)).filter((m)=>m!==null)}
function __ec_makeNamed(map){return{merge(current,diff){if(diff===null)return current;const merged=Object.assign({},current,diff);for(const key in map){if(diff[key]&&map[key])merged[key]=map[key].merge(current[key],diff[key])}return merged}}}
const __ec_argsProc={merge:(c,d)=>__ec_mergeOrdered(__ec_toArray(c),__ec_toArray(d),__ec_mergeSimple)};
const __ec_optionProc=__ec_makeNamed({args:__ec_argsProc});
const __ec_subProc=__ec_makeNamed({subcommands:{merge:(c,d)=>__ec_mergeNamed(__ec_toArray(c),__ec_toArray(d),(a,b)=>__ec_subProc.merge(a,b))},options:{merge:(c,d)=>__ec_mergeNamed(__ec_toArray(c),__ec_toArray(d),(a,b)=>__ec_optionProc.merge(a,b))},args:__ec_argsProc});
function __ec_applySpecDiff(spec,diff){return __ec_subProc.merge(spec,Object.assign({},diff,{name:spec.name}))}
function __ec_compareSemver(left,right){const parse=(v)=>{const m=String(v??"").trim().match(/^v?(\\d+)\\.(\\d+)\\.(\\d+)(?:-([0-9A-Za-z.-]+))?/);return m?{n:[+m[1],+m[2],+m[3]],p:m[4]??""}:null};const a=parse(left),b=parse(right);if(!a&&!b)return left<right?-1:left>right?1:0;if(!a)return 1;if(!b)return-1;for(let i=0;i<3;i++){if(a.n[i]!==b.n[i])return a.n[i]>b.n[i]?1:-1}if(!a.p&&b.p)return 1;if(a.p&&!b.p)return-1;return 0}
function __ec_mergedThrough(base,versions,through){const keys=Object.keys(versions||{}).sort(__ec_compareSemver);const end=keys.indexOf(through);const apply=end===-1?keys:keys.slice(0,end+1);return apply.reduce((spec,key)=>__ec_applySpecDiff(spec,versions[key]),base)}
`;
}
