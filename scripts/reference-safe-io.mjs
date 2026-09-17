/**
 * Small, dependency-free filesystem helpers shared by the reference
 * diagnostics.  These paths contain generated evidence, not application
 * state, but they still must not turn a report/baseline option into an
 * arbitrary write primitive.
 *
 * A writer never opens an existing destination with truncation.  It first
 * writes and syncs a private file in the destination directory, then checks
 * the directory and destination identities again before publishing.  New
 * destinations use link(2)+unlink(2), which is an atomic no-replace publish
 * on the filesystems supported by Node. Existing destinations are first moved
 * to a unique, identity-checked backup, then the staged inode is published
 * with the same no-replace link primitive. A changed target is never clobbered;
 * uncertain backups and staging files are retained rather than deleting a
 * possibly foreign entry.
 */
import { randomUUID } from "node:crypto";
import { constants } from "node:fs";
import {
  link,
  lstat,
  open,
  rename,
  realpath,
  unlink,
} from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

const O_DIRECTORY = constants.O_DIRECTORY ?? 0;
const O_NOFOLLOW = constants.O_NOFOLLOW ?? 0;
const O_NONBLOCK = constants.O_NONBLOCK ?? 0;

// macOS resolves /var and /tmp through /private; a path under either is the
// same file as its /private twin. Linux has no such link, so treating these as
// aliases there would make every temp-dir fixture "escape its approved root".
const KNOWN_SYSTEM_ALIASES = new Map(
  process.platform === "darwin"
    ? [
        ["/var", "/private/var"],
        ["/tmp", "/private/tmp"],
      ]
    : [],
);

function knownAliasTarget(path) {
  const absolute = resolve(path);
  for (const [alias, target] of KNOWN_SYSTEM_ALIASES) {
    if (absolute === alias || absolute.startsWith(`${alias}${sep}`)) {
      return resolve(target + absolute.slice(alias.length));
    }
  }
  return null;
}

function pathsMatchKnownAlias(path, canonical) {
  const target = knownAliasTarget(path);
  return target !== null && resolve(canonical) === target;
}

function isInside(root, target) {
  const path = relative(resolve(root), resolve(target));
  return (
    path === "" ||
    (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`))
  );
}

function identity(info) {
  return {
    dev: info.dev,
    ino: info.ino,
    mode: info.mode,
  };
}

function sameIdentity(left, right) {
  return (
    left &&
    right &&
    left.dev === right.dev &&
    left.ino === right.ino &&
    left.mode === right.mode
  );
}

async function assertNoSymlinkAncestors(path, label, { allowMissingLeaf = false } = {}) {
  const absolute = resolve(path);
  const components = absolute.split(sep).filter(Boolean);
  let current = absolute.startsWith(sep) ? sep : "";
  for (let index = 0; index < components.length; index += 1) {
    current = current ? join(current, components[index]) : components[index];
    try {
      const info = await lstat(current);
      if (info.isSymbolicLink()) {
        const canonical = await realpath(current).catch(() => null);
        if (canonical && pathsMatchKnownAlias(current, canonical)) continue;
        throw new Error(`${label} contains a symbolic-link ancestor`);
      }
      if (index < components.length - 1 && !info.isDirectory()) {
        throw new Error(`${label} ancestor is not a directory`);
      }
    } catch (error) {
      if (
        error?.code === "ENOENT" &&
        allowMissingLeaf &&
        index === components.length - 1
      ) {
        return absolute;
      }
      throw error;
    }
  }
  return absolute;
}

async function readIdentity(path, label) {
  try {
    return await lstat(path);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw new Error(`${label} cannot be inspected: ${error.message}`, {
      cause: error,
    });
  }
}

async function assertRegularParent(parent, label) {
  await assertNoSymlinkAncestors(parent, `${label} parent`);
  const info = await lstat(parent);
  if (!info.isDirectory() || info.isSymbolicLink()) {
    throw new Error(`${label} parent must be a regular directory`);
  }
  return info;
}

function validateOutputPath(outputPath, label) {
  if (
    typeof outputPath !== "string" ||
    !outputPath ||
    outputPath.includes("\0") ||
    outputPath.includes("\\")
  ) {
    throw new Error(`${label} path is invalid`);
  }
}

/**
 * Safely publish a UTF-8 reference artifact.
 *
 * `overwrite:false` means the destination must remain absent; a creator that
 * appears after the initial check wins and this call fails without replacing
 * it.  `overwrite:true` permits replacing exactly the regular, single-link
 * target observed before staging.  In both modes parent changes are detected
 * immediately before publication.  The parent is checked once more after
 * opening the staging file so a replacement cannot redirect the staging
 * write without making the operation fail closed.
 */
export async function writeReferenceFile(
  outputPath,
  text,
  { root = null, overwrite = false, label = "reference output" } = {},
) {
  validateOutputPath(outputPath, label);
  if (typeof text !== "string") {
    throw new Error(`${label} contents must be a string`);
  }

  const target = resolve(outputPath);
  const parent = dirname(target);
  const approvedRoot = root === null ? null : resolve(root);
  if (approvedRoot && !isInside(approvedRoot, target)) {
    throw new Error(`${label} escapes its approved root`);
  }
  if (approvedRoot) {
    await assertNoSymlinkAncestors(approvedRoot, `${label} root`);
    const rootInfo = await lstat(approvedRoot);
    if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink()) {
      throw new Error(`${label} root must be a regular directory`);
    }
    const canonical = await realpath(approvedRoot);
    if (canonical !== approvedRoot && !pathsMatchKnownAlias(approvedRoot, canonical)) {
      throw new Error(`${label} root contains a symbolic-link ancestor`);
    }
    const targetForContainment = knownAliasTarget(target) ?? target;
    const canonicalRoot = knownAliasTarget(approvedRoot) ?? approvedRoot;
    if (!isInside(canonicalRoot, targetForContainment)) {
      throw new Error(`${label} escapes its approved root`);
    }
  }

  const parentBefore = await assertRegularParent(parent, label);
  const targetBefore = await readIdentity(target, label);
  if (targetBefore?.isSymbolicLink()) {
    throw new Error(`${label} target is a symbolic link`);
  }
  if (targetBefore && !targetBefore.isFile()) {
    throw new Error(`${label} target is a special entry`);
  }
  if (targetBefore && targetBefore.nlink !== 1) {
    throw new Error(`${label} target must not be a hard link`);
  }
  if (targetBefore && !overwrite) {
    throw new Error(`refusing to overwrite existing report: ${outputPath}`);
  }

  const temporary = join(
    parent,
    `.${target.split(sep).at(-1)}.tmp-${process.pid}-${randomUUID()}`,
  );
  const backup = join(
    parent,
    `.${target.split(sep).at(-1)}.old-${process.pid}-${randomUUID()}`,
  );
  let temporaryHandle;
  let temporaryIdentity;
  let temporaryLinked = false;
  let backupPlaceholder;
  let backupPlaceholderIdentity;
  let backupMoved = false;
  let published = false;

  const parentStillSame = async (message) => {
    const current = await readIdentity(parent, `${label} parent`);
    if (!sameIdentity(parentBefore, current)) {
      throw new Error(`${label} parent changed ${message}`);
    }
  };
  const unlinkIfSame = async (path, expected, message) => {
    const current = await readIdentity(path, label);
    if (!current) return false;
    if (!sameIdentity(expected, current)) {
      throw new Error(`${label} ${message} identity changed`);
    }
    await unlink(path);
    return true;
  };
  const restoreBackup = async () => {
    if (!backupMoved || !targetBefore) return false;
    await parentStillSame("while restoring the previous target");
    const currentTarget = await readIdentity(target, label);
    if (currentTarget) return false;
    const currentBackup = await readIdentity(backup, label);
    if (!sameIdentity(targetBefore, currentBackup)) return false;
    // link() is the no-clobber primitive available through Node's path APIs:
    // a concurrent creator wins and the old target remains in `backup`.
    await link(backup, target);
    await unlinkIfSame(backup, targetBefore, "backup");
    backupMoved = false;
    return true;
  };

  try {
    // O_EXCL gives the staging file a unique inode. O_NOFOLLOW and the parent
    // check keep a hostile final entry from becoming a FIFO/device.
    temporaryHandle = await open(
      temporary,
      constants.O_WRONLY |
        constants.O_CREAT |
        constants.O_EXCL |
        O_NOFOLLOW |
        O_NONBLOCK,
      0o600,
    );
    const staged = await temporaryHandle.stat();
    if (!staged.isFile() || staged.nlink !== 1) {
      throw new Error(`${label} staging file is not a private regular file`);
    }
    temporaryIdentity = identity(staged);
    await temporaryHandle.writeFile(text, "utf8");
    await temporaryHandle.sync();
    await temporaryHandle.close();
    temporaryHandle = null;

    await parentStillSame("while staging");
    const stagedPath = await readIdentity(temporary, label);
    if (
      !sameIdentity(temporaryIdentity, stagedPath) ||
      stagedPath.nlink !== 1
    ) {
      throw new Error(`${label} staging file changed before publication`);
    }

    const targetAfterStage = await readIdentity(target, label);
    if (overwrite && targetBefore) {
      if (
        !targetAfterStage ||
        !targetAfterStage.isFile() ||
        targetAfterStage.isSymbolicLink() ||
        targetAfterStage.nlink !== 1 ||
        !sameIdentity(targetBefore, targetAfterStage)
      ) {
        throw new Error(`${label} target changed while staging`);
      }

      // Reserve a unique backup name before moving the old target. Node does
      // not expose renameat2(RENAME_NOREPLACE), so the identity is checked on
      // both sides of this move and any uncertain backup is deliberately
      // retained. No existing target is truncated or written in place.
      backupPlaceholder = await open(
        backup,
        constants.O_WRONLY |
          constants.O_CREAT |
          constants.O_EXCL |
          O_NOFOLLOW |
          O_NONBLOCK,
        0o600,
      );
      const placeholderInfo = await backupPlaceholder.stat();
      backupPlaceholderIdentity = identity(placeholderInfo);
      await backupPlaceholder.close();
      backupPlaceholder = null;
      await parentStillSame("before moving the previous target");
      await rename(target, backup);
      backupMoved = true;
      const moved = await readIdentity(backup, label);
      if (!sameIdentity(targetBefore, moved) || moved.nlink !== 1) {
        throw new Error(`${label} previous target changed during backup`);
      }
      await parentStillSame("after moving the previous target");
    } else if (targetAfterStage) {
      throw new Error(`refusing to overwrite existing report: ${outputPath}`);
    }

    // link() publishes without replacing a target which appeared after the
    // last identity check. This is the no-clobber half of both create and
    // overwrite modes.
    await link(temporary, target);
    temporaryLinked = true;
    published = true;
    await unlinkIfSame(temporary, temporaryIdentity, "staging");
    temporaryLinked = false;
    await parentStillSame("during publication");

    if (backupMoved) {
      const currentBackup = await readIdentity(backup, label);
      if (!sameIdentity(targetBefore, currentBackup)) {
        throw new Error(`${label} backup changed before cleanup`);
      }
      await unlinkIfSame(backup, targetBefore, "backup");
      backupMoved = false;
    }

    // Directory fsync makes the link publication durable. Filesystems that
    // do not support syncing a directory report EINVAL; publication is still
    // complete, and the identity checks above remain authoritative.
    const parentHandle = await open(
      parent,
      constants.O_RDONLY | O_DIRECTORY | O_NOFOLLOW,
    );
    try {
      const parentHandleInfo = await parentHandle.stat();
      if (!sameIdentity(parentBefore, parentHandleInfo)) {
        throw new Error(`${label} parent changed before directory sync`);
      }
      try {
        await parentHandle.sync();
      } catch (error) {
        if (error?.code !== "EINVAL" && error?.code !== "ENOTSUP") {
          throw error;
        }
      }
    } finally {
      await parentHandle.close().catch(() => {});
    }
    return target;
  } catch (error) {
    // A failed overwrite may have moved the old target aside. Restore only
    // with the no-clobber link primitive; if a concurrent creator occupies
    // the target, or either identity is uncertain, retain the backup.
    if (backupMoved && !published) {
      await restoreBackup().catch(() => {});
    }
    throw error;
  } finally {
    await temporaryHandle?.close().catch(() => {});
    await backupPlaceholder?.close().catch(() => {});
    if (!temporaryLinked) {
      const currentParent = await readIdentity(parent, `${label} parent`).catch(
        () => null,
      );
      if (sameIdentity(parentBefore, currentParent)) {
        const currentTemporary = await readIdentity(temporary, label).catch(
          () => null,
        );
        if (sameIdentity(temporaryIdentity, currentTemporary)) {
          await unlink(temporary).catch(() => {});
        }
        if (!backupMoved && backupPlaceholderIdentity) {
          const currentBackup = await readIdentity(backup, label).catch(
            () => null,
          );
          if (sameIdentity(backupPlaceholderIdentity, currentBackup)) {
            await unlink(backup).catch(() => {});
          }
        }
      }
    }
  }
}

export { assertNoSymlinkAncestors, isInside };
