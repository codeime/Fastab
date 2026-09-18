//! Runtime verification for the generated source/IR publication pair.
//!
//! The compiler writes `bundle/specs` and `bundle/specs-ir` independently.  A
//! pair marker in the IR directory commits both trees, so a reader can reject
//! the intermediate state exposed while the two directories are being
//! published.  Handwritten fixtures without a marker intentionally remain
//! supported; once a marker exists, the schema and every committed digest are
//! strict.

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::io;
use std::path::Path;

use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::snapshot::DirectorySnapshot;

const PAIR_MARKER_NAME: &str = ".spec-pair.json";
#[cfg(test)]
const SOURCE_MANIFEST_NAME: &str = ".source-manifest.json";
const PAIR_FORMAT: u64 = 1;
const PAIR_KIND: &str = "easy-complete-spec-pair";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PairMarker {
    format: u64,
    kind: String,
    source: SourceMarker,
    ir: IrMarker,
    #[serde(rename = "pairSha256")]
    pair_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SourceMarker {
    #[serde(rename = "treeSha256")]
    tree_sha256: String,
    #[serde(rename = "fileCount")]
    file_count: u64,
    // This is deliberately a non-Option wrapper.  `Option<T>` fields are
    // optional in serde, while the JS schema requires the key to be present
    // even when its value is null.
    #[serde(rename = "manifestSha256")]
    manifest_sha256: NullableDigest,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct IrMarker {
    #[serde(rename = "treeSha256")]
    tree_sha256: String,
    #[serde(rename = "fileCount")]
    file_count: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct NullableDigest(Option<String>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeDigest {
    digest: String,
    file_count: u64,
}

#[derive(Debug, Clone)]
struct FileDigest {
    path: Vec<u8>,
    digest: String,
}

/// Verify the marker and committed IR tree when a marker is present.
///
/// A missing marker is the compatibility path for handwritten/legacy IR
/// fixtures.  A malformed marker is not treated as absent: once a marker path
/// exists, it must be a regular file with the current strict schema.
#[cfg(test)]
pub(crate) fn verify_if_present(ir_root: &Path) -> anyhow::Result<()> {
    let marker_path = ir_root.join(PAIR_MARKER_NAME);
    match fs::symlink_metadata(&marker_path) {
        Ok(_) => verify_pair(None, ir_root),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", marker_path.display())),
    }
}

/// Snapshot-backed equivalent of [`verify_if_present`]. The marker and every
/// tree file are checked against the same captured generation that `Registry`
/// and `NativeHooks` share for lazy loading. A later canonical replacement can
/// therefore only be consumed when its bytes are identical.
pub(crate) fn verify_if_present_snapshot(snapshot: &DirectorySnapshot) -> anyhow::Result<()> {
    let marker = Path::new(PAIR_MARKER_NAME);
    let before = match snapshot.read_optional_file(marker)? {
        Some(bytes) => read_marker_bytes(&bytes, &snapshot.display_path().join(marker))?,
        None => return Ok(()),
    };
    let ir = digest_snapshot_tree(snapshot, PAIR_MARKER_NAME)?;
    let after = read_marker_snapshot(snapshot)?;
    if before != after {
        bail!(
            "pair marker changed while reading {}",
            snapshot.display_path().display()
        );
    }
    if ir.digest != before.ir.tree_sha256 || ir.file_count != before.ir.file_count {
        bail!(
            "IR tree does not match {} in {}",
            PAIR_MARKER_NAME,
            snapshot.display_path().display()
        );
    }
    Ok(())
}

/// Verify a generated source/IR pair.  `source_root` is optional because the
/// shipped app contains only `specs-ir`; in that case the marker still commits
/// and verifies the IR tree, while the source digest remains schema-validated
/// but cannot be checked against a tree that was not shipped.
#[cfg(test)]
pub(crate) fn verify_pair(source_root: Option<&Path>, ir_root: &Path) -> anyhow::Result<()> {
    let before = read_marker(ir_root)?;
    let source = source_root
        .map(|root| digest_tree(root, SOURCE_MANIFEST_NAME))
        .transpose()?;
    let source_manifest = source_root
        .map(|root| file_digest_if_present(&root.join(SOURCE_MANIFEST_NAME)))
        .transpose()?;
    let ir = digest_tree(ir_root, PAIR_MARKER_NAME)?;
    let after = read_marker(ir_root)?;

    if before != after {
        bail!("pair marker changed while reading {}", ir_root.display());
    }
    if let (Some(source), Some(source_manifest)) = (&source, &source_manifest) {
        if source.digest != before.source.tree_sha256
            || source.file_count != before.source.file_count
            || source_manifest.as_deref() != before.source.manifest_sha256.0.as_deref()
        {
            bail!(
                "source tree does not match {} in {}",
                PAIR_MARKER_NAME,
                ir_root.display()
            );
        }
    }
    if ir.digest != before.ir.tree_sha256 || ir.file_count != before.ir.file_count {
        bail!("IR tree does not match {} in {}", PAIR_MARKER_NAME, ir_root.display());
    }
    Ok(())
}

#[cfg(test)]
fn read_marker(ir_root: &Path) -> anyhow::Result<PairMarker> {
    let path = ir_root.join(PAIR_MARKER_NAME);
    let metadata = fs::symlink_metadata(&path).with_context(|| format!("read {} metadata", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("pair marker is a symlink or special entry in {}", ir_root.display());
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    read_marker_bytes(&bytes, &path)
}

fn read_marker_bytes(bytes: &[u8], path: &Path) -> anyhow::Result<PairMarker> {
    let marker: PairMarker = serde_json::from_slice(bytes).with_context(|| format!("parse {}", path.display()))?;
    validate_marker(&marker)?;
    Ok(marker)
}

fn read_marker_snapshot(snapshot: &DirectorySnapshot) -> anyhow::Result<PairMarker> {
    let relative = Path::new(PAIR_MARKER_NAME);
    let bytes = snapshot
        .read_file(relative)
        .with_context(|| format!("read {}", snapshot.display_path().join(relative).display()))?;
    read_marker_bytes(&bytes, &snapshot.display_path().join(relative))
}

fn validate_marker(marker: &PairMarker) -> anyhow::Result<()> {
    if marker.format != PAIR_FORMAT {
        bail!("pair marker format {} is unsupported", marker.format);
    }
    if marker.kind != PAIR_KIND {
        bail!("pair marker kind {} is unsupported", marker.kind);
    }
    validate_digest("pair marker source.treeSha256", &marker.source.tree_sha256)?;
    validate_count("pair marker source.fileCount", marker.source.file_count)?;
    if let Some(digest) = marker.source.manifest_sha256.0.as_deref() {
        validate_digest("pair marker source.manifestSha256", digest)?;
    }
    validate_digest("pair marker ir.treeSha256", &marker.ir.tree_sha256)?;
    validate_count("pair marker ir.fileCount", marker.ir.file_count)?;
    validate_digest("pair marker pairSha256", &marker.pair_sha256)?;

    let expected = sha256_hex(canonical_pair_digest(marker).as_bytes());
    if expected != marker.pair_sha256 {
        bail!("pair marker pairSha256 does not match its contents");
    }
    Ok(())
}

fn validate_digest(label: &str, value: &str) -> anyhow::Result<()> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        bail!("{label} must be a lowercase SHA-256 digest");
    }
    Ok(())
}

fn validate_count(label: &str, value: u64) -> anyhow::Result<()> {
    if value > MAX_SAFE_INTEGER {
        bail!("{label} must be a non-negative safe integer");
    }
    Ok(())
}

fn canonical_pair_digest(marker: &PairMarker) -> String {
    format!(
        "format={PAIR_FORMAT}\nkind={PAIR_KIND}\nsource.treeSha256={}\nsource.fileCount={}\nsource.manifestSha256={}\nir.treeSha256={}\nir.fileCount={}\n",
        marker.source.tree_sha256,
        marker.source.file_count,
        marker.source.manifest_sha256.0.as_deref().unwrap_or_default(),
        marker.ir.tree_sha256,
        marker.ir.file_count,
    )
}

#[cfg(test)]
fn digest_tree(root: &Path, excluded_root_name: &str) -> anyhow::Result<TreeDigest> {
    let metadata = fs::symlink_metadata(root).with_context(|| format!("inspect pair tree {}", root.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("pair tree cannot be a symbolic link: {}", root.display());
    }
    if !metadata.file_type().is_dir() {
        bail!("pair tree is not a directory: {}", root.display());
    }

    let mut files = Vec::new();
    digest_tree_walk(root, root, excluded_root_name, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut canonical = Vec::new();
    for file in &files {
        canonical.extend_from_slice(&file.path);
        canonical.push(0);
        canonical.extend_from_slice(file.digest.as_bytes());
        canonical.push(b'\n');
    }
    Ok(TreeDigest {
        digest: sha256_hex(&canonical),
        file_count: files
            .len()
            .try_into()
            .map_err(|error| anyhow!("pair tree has too many files: {error}"))?,
    })
}

fn digest_snapshot_tree(snapshot: &DirectorySnapshot, excluded_root_name: &str) -> anyhow::Result<TreeDigest> {
    let mut files = snapshot
        .file_digests()
        .filter_map(|(path, digest)| {
            if path.parent().unwrap_or_else(|| Path::new("")) == Path::new("")
                && path.file_name().is_some_and(|name| name == excluded_root_name)
            {
                return None;
            }
            Some((path, digest))
        })
        .map(|(path, digest)| {
            let path = path
                .to_str()
                .ok_or_else(|| {
                    anyhow!(
                        "pair tree contains a non-UTF-8 path under {}: {}",
                        snapshot.display_path().display(),
                        path.display()
                    )
                })?
                .replace('\\', "/")
                .into_bytes();
            Ok(FileDigest {
                path,
                digest: digest.to_owned(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut canonical = Vec::new();
    for file in &files {
        canonical.extend_from_slice(&file.path);
        canonical.push(0);
        canonical.extend_from_slice(file.digest.as_bytes());
        canonical.push(b'\n');
    }
    Ok(TreeDigest {
        digest: sha256_hex(&canonical),
        file_count: files
            .len()
            .try_into()
            .map_err(|error| anyhow!("pair tree has too many files: {error}"))?,
    })
}

#[cfg(test)]
fn digest_tree_walk(
    root: &Path,
    current: &Path,
    excluded_root_name: &str,
    files: &mut Vec<FileDigest>,
) -> anyhow::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(current).with_context(|| format!("read {}", current.display()))? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|error| {
            anyhow!(
                "pair tree contains a non-UTF-8 path under {}: {:?}",
                current.display(),
                error
            )
        })?;
        entries.push((name, entry.path()));
    }
    entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

    for (name, full) in entries {
        if current == root && name == excluded_root_name {
            continue;
        }
        let metadata = fs::symlink_metadata(&full).with_context(|| format!("inspect {}", full.display()))?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            bail!("pair tree contains a symlink or special entry: {}", full.display());
        }
        if file_type.is_dir() {
            digest_tree_walk(root, &full, excluded_root_name, files)?;
            continue;
        }
        if !file_type.is_file() {
            bail!("pair tree contains a symlink or special entry: {}", full.display());
        }
        let relative_path = full
            .strip_prefix(root)
            .with_context(|| format!("derive path for {}", full.display()))?
            .to_str()
            .ok_or_else(|| anyhow!("pair tree contains a non-UTF-8 path: {}", full.display()))?
            .replace('\\', "/")
            .into_bytes();
        files.push(FileDigest {
            path: relative_path,
            digest: sha256_hex(&fs::read(&full).with_context(|| format!("read {}", full.display()))?),
        });
    }
    Ok(())
}

#[cfg(test)]
fn file_digest_if_present(path: &Path) -> anyhow::Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("pair tree contains a symlink or special entry: {}", path.display());
    }
    Ok(Some(sha256_hex(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        output.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_pair() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source");
        let ir = root.path().join("ir");
        fs::create_dir_all(source.join("nested")).expect("source dirs");
        fs::create_dir_all(ir.join("hooks")).expect("IR dirs");
        fs::write(source.join("z.js"), b"z\n").expect("source z");
        fs::write(source.join("nested/a.js"), b"a\n").expect("source a");
        fs::write(source.join(SOURCE_MANIFEST_NAME), b"{\"format\":1}\n").expect("source manifest");
        fs::write(ir.join("z.json"), b"{\"names\":[\"z\"]}\n").expect("IR z");
        fs::write(ir.join("hooks/h.js"), b"export default {};\n").expect("IR hook");

        let source_tree = digest_tree(&source, SOURCE_MANIFEST_NAME).expect("source digest");
        let ir_tree = digest_tree(&ir, PAIR_MARKER_NAME).expect("IR digest");
        let mut marker = PairMarker {
            format: PAIR_FORMAT,
            kind: PAIR_KIND.to_string(),
            source: SourceMarker {
                tree_sha256: source_tree.digest,
                file_count: source_tree.file_count,
                manifest_sha256: NullableDigest(
                    file_digest_if_present(&source.join(SOURCE_MANIFEST_NAME)).expect("manifest digest"),
                ),
            },
            ir: IrMarker {
                tree_sha256: ir_tree.digest,
                file_count: ir_tree.file_count,
            },
            pair_sha256: String::new(),
        };
        marker.pair_sha256 = sha256_hex(canonical_pair_digest(&marker).as_bytes());
        fs::write(
            ir.join(PAIR_MARKER_NAME),
            serde_json::to_vec(&marker).expect("marker JSON"),
        )
        .expect("pair marker");
        (root, source, ir)
    }

    #[test]
    fn valid_pair_is_accepted() {
        let (_root, source, ir) = make_pair();
        verify_pair(Some(&source), &ir).expect("valid pair");
    }

    #[test]
    fn source_or_ir_drift_is_rejected() {
        let (_root, source, ir) = make_pair();
        fs::write(source.join("z.js"), b"changed\n").expect("drift source");
        let error = verify_pair(Some(&source), &ir).expect_err("source drift");
        assert!(error.to_string().contains("source tree does not match"), "{error}");

        let (_root, source, ir) = make_pair();
        fs::write(ir.join("z.json"), b"changed\n").expect("drift IR");
        let error = verify_pair(Some(&source), &ir).expect_err("IR drift");
        assert!(error.to_string().contains("IR tree does not match"), "{error}");
    }

    #[test]
    fn invalid_pair_and_unknown_fields_are_rejected() {
        let (_root, source, ir) = make_pair();
        let marker_path = ir.join(PAIR_MARKER_NAME);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker_path).expect("marker")).expect("JSON");
        value["unexpected"] = serde_json::json!(true);
        fs::write(&marker_path, serde_json::to_vec(&value).expect("JSON")).expect("unknown field");
        let error = verify_pair(Some(&source), &ir).expect_err("unknown field");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("unknown field"), "{rendered}");

        let (_root, source, ir) = make_pair();
        let marker_path = ir.join(PAIR_MARKER_NAME);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker_path).expect("marker")).expect("JSON");
        value["pairSha256"] = serde_json::Value::String("0".repeat(64));
        fs::write(&marker_path, serde_json::to_vec(&value).expect("JSON")).expect("invalid pair");
        let error = verify_pair(Some(&source), &ir).expect_err("invalid pair digest");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("pairSha256 does not match"), "{rendered}");
    }

    #[test]
    fn tree_digest_orders_paths_by_utf8_bytes() {
        let root = tempfile::tempdir().expect("tempdir");
        let bmp_private_use = "\u{e000}.json";
        let supplementary = "\u{10000}.json";
        fs::write(root.path().join(bmp_private_use), b"bmp\n").expect("BMP file");
        fs::write(root.path().join(supplementary), b"supplementary\n").expect("supplementary file");

        let bmp_digest = sha256_hex(b"bmp\n");
        let supplementary_digest = sha256_hex(b"supplementary\n");
        let mut canonical = Vec::new();
        for (path, digest) in [
            (bmp_private_use, bmp_digest.as_str()),
            (supplementary, supplementary_digest.as_str()),
        ] {
            canonical.extend_from_slice(path.as_bytes());
            canonical.push(0);
            canonical.extend_from_slice(digest.as_bytes());
            canonical.push(b'\n');
        }

        let tree = digest_tree(root.path(), PAIR_MARKER_NAME).expect("tree digest");
        assert_eq!(tree.file_count, 2);
        assert_eq!(tree.digest, sha256_hex(&canonical));
        assert!(bmp_private_use.as_bytes() < supplementary.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_pair_digest_reuses_the_captured_file_hashes() {
        let (root, _source, ir) = make_pair();
        let expected = digest_tree(&ir, PAIR_MARKER_NAME).expect("disk tree digest");
        let snapshot = DirectorySnapshot::open(&ir).expect("snapshot");
        let moved = root.path().join("moved-ir");
        fs::rename(&ir, &moved).expect("move canonical after capture");

        // Pair-tree calculation must remain a pure operation over the captured
        // generation. Marker reads and later lazy assets separately validate
        // canonical bytes against these hashes.
        let captured = digest_snapshot_tree(&snapshot, PAIR_MARKER_NAME).expect("captured tree digest");
        assert_eq!(captured, expected);
    }

    #[test]
    fn runtime_verification_does_not_infer_an_unrelated_source_sibling() {
        let (root, source, ir) = make_pair();
        let unrelated = root.path().join("specs");
        fs::rename(source, &unrelated).expect("rename source sibling");
        fs::write(unrelated.join("unrelated.js"), b"not this pair\n").expect("unrelated file");

        verify_if_present(&ir).expect("runtime checks IR only");
    }

    #[test]
    fn compiler_generated_pair_matches_node_contract() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/compiler-hook-chain");
        verify_pair(Some(&root.join("source")), &root.join("specs-ir")).expect("compiler-generated pair");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_in_a_marked_tree_is_rejected() {
        let (_root, source, ir) = make_pair();
        std::os::unix::fs::symlink(source.join("z.js"), ir.join("linked.json")).expect("symlink");
        let error = verify_pair(Some(&source), &ir).expect_err("symlink");
        assert!(error.to_string().contains("symlink or special entry"), "{error}");
    }

    #[test]
    fn absent_marker_keeps_legacy_fixture_allowed() {
        let root = tempfile::tempdir().expect("tempdir");
        let ir = root.path().join("ir");
        fs::create_dir(&ir).expect("IR dir");
        fs::write(ir.join("index.json"), b"{}\n").expect("legacy index");
        verify_if_present(&ir).expect("missing marker is legacy-compatible");
    }
}
