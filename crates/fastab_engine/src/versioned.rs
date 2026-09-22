//! Compile-time versioned spec selection.
//!
//! Mirrors `@fig/autocomplete-helpers` `getBestVersionIndex` /
//! `getVersionFromVersionedSpec` / `createVersionedSpec`: pick the highest
//! version file at or below the detected CLI version (the last file when none
//! is ≤ the target or detection fails), then apply the same rule to that
//! file's `versions` diffs.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

use crate::process::{self, CommandError};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VersionedCommand {
    pub command: Vec<String>,
    pub parse: String,
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default, rename = "regexGroup")]
    pub regex_group: Option<usize>,
    #[serde(default, rename = "parseFallback")]
    pub parse_fallback: Option<String>,
    pub fallback: String,
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub applied: BTreeMap<String, BTreeMap<String, String>>,
}

pub fn compare_semver(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_semver(left), parse_semver(right)) {
        (None, None) => left.cmp(right),
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => {
            for index in 0..3 {
                match left.numbers[index].cmp(&right.numbers[index]) {
                    std::cmp::Ordering::Equal => {},
                    other => return other,
                }
            }
            match (left.prerelease.is_empty(), right.prerelease.is_empty()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (true, true) => std::cmp::Ordering::Equal,
                (false, false) => left.prerelease.cmp(&right.prerelease),
            }
        },
    }
}

struct ParsedSemver {
    numbers: [u64; 3],
    prerelease: String,
}

fn parse_semver(value: &str) -> Option<ParsedSemver> {
    let trimmed = value.trim().trim_start_matches('v');
    let (core, prerelease) = match trimmed.split_once('-') {
        Some((core, rest)) => (core, rest.split('+').next().unwrap_or(rest).to_string()),
        None => (trimmed.split('+').next().unwrap_or(trimmed), String::new()),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(ParsedSemver {
        numbers: [major, minor, patch],
        prerelease,
    })
}

pub fn get_best_version_index(versions: &[String], target: Option<&str>) -> usize {
    if versions.is_empty() {
        return 0;
    }
    let Some(target) = target.filter(|value| !value.is_empty()) else {
        return versions.len() - 1;
    };
    for index in (0..versions.len()).rev() {
        if compare_semver(&versions[index], target) != std::cmp::Ordering::Greater {
            return index;
        }
    }
    versions.len() - 1
}

fn semver_keys(map: &BTreeMap<String, impl Sized>) -> Vec<String> {
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort_by(|left, right| compare_semver(left, right));
    keys
}

pub fn resolve_versioned_path(entry: &VersionedCommand, detected: Option<&str>) -> Option<String> {
    let file_versions = semver_keys(&entry.files);
    if file_versions.is_empty() {
        return None;
    }
    let file_index = get_best_version_index(&file_versions, detected);
    let file_version = &file_versions[file_index];
    let file_path = entry.files.get(file_version)?.clone();
    let Some(applied) = entry.applied.get(file_version) else {
        return Some(file_path);
    };
    if applied.is_empty() {
        return Some(file_path);
    }
    let diff_versions = semver_keys(applied);
    let diff_index = get_best_version_index(&diff_versions, detected);
    Some(applied.get(&diff_versions[diff_index]).cloned().unwrap_or(file_path))
}

pub fn semver_clean(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_start_matches(['v', '=']);
    let bytes = trimmed.as_bytes();
    let mut start = None;
    for (index, byte) in bytes.iter().enumerate() {
        if byte.is_ascii_digit() {
            start = Some(index);
            break;
        }
    }
    let start = start?;
    let rest = &trimmed[start..];
    let mut end = 0;
    let mut dots = 0;
    for (index, ch) in rest.char_indices() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' || ch.is_ascii_alphabetic() {
            if ch == '.' {
                dots += 1;
            }
            end = index + ch.len_utf8();
            continue;
        }
        break;
    }
    if dots < 2 {
        return None;
    }
    Some(rest[..end].trim_end_matches(['.', '-', '+']).to_string())
}

pub fn parse_version_stdout(stdout: &str, entry: &VersionedCommand) -> Option<String> {
    match entry.parse.as_str() {
        "stdout" => Some(stdout.to_string()),
        "after-first-space" => match stdout.find(' ') {
            Some(index) => Some(stdout[index + 1..].to_string()),
            None => Some(stdout.to_string()),
        },
        "semver-clean" => semver_clean(stdout),
        "semver-clean-after-space" => {
            let sliced = match stdout.find(' ') {
                Some(index) => &stdout[index + 1..],
                None => stdout,
            };
            semver_clean(sliced)
        },
        "regex" => {
            let pattern = entry.regex.as_deref()?;
            let Ok(regex) = fancy_regex::Regex::new(pattern) else {
                return entry.parse_fallback.clone();
            };
            match regex.captures(stdout) {
                Ok(Some(captures)) => {
                    let group = entry.regex_group.unwrap_or(0);
                    captures
                        .get(group)
                        .map(|matched| matched.as_str().to_string())
                        .or_else(|| entry.parse_fallback.clone())
                },
                _ => entry.parse_fallback.clone(),
            }
        },
        _ => None,
    }
}

pub fn detect_cli_version(entry: &VersionedCommand, cwd: &str, timeout: Duration) -> Option<String> {
    let (command, args) = entry.command.split_first()?;
    match process::execute_full(command, args, cwd, &[], timeout) {
        Ok(output) => {
            let parsed = parse_version_stdout(&output.stdout, entry)?;
            let trimmed = parsed.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        },
        Err(CommandError::Failed | CommandError::TimedOut) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heroku_entry() -> VersionedCommand {
        VersionedCommand {
            command: vec!["heroku".into(), "--version".into()],
            parse: "regex".into(),
            regex: Some("heroku\\/([0-9]+\\.[0-9]+\\.[0.9]+)".into()),
            regex_group: Some(1),
            parse_fallback: Some("8.0.0".into()),
            fallback: "8.6.0".into(),
            files: BTreeMap::from([
                ("8.0.0".into(), "heroku/8.0.0.json".into()),
                ("8.6.0".into(), "heroku/8.6.0.json".into()),
            ]),
            applied: BTreeMap::from([(
                "8.0.0".into(),
                BTreeMap::from([("8.11.1".into(), "heroku/8.0.0+8.11.1.json".into())]),
            )]),
        }
    }

    #[test]
    fn heroku_8_3_0_uses_8_0_0_file_and_8_11_1_diff() {
        let entry = heroku_entry();
        assert_eq!(
            resolve_versioned_path(&entry, Some("8.3.0")).as_deref(),
            Some("heroku/8.0.0+8.11.1.json")
        );
        assert_eq!(
            resolve_versioned_path(&entry, None).as_deref(),
            Some("heroku/8.6.0.json")
        );
    }

    #[test]
    fn heroku_regex_extracts_8_3_0_and_falls_back_when_unmatched() {
        let entry = heroku_entry();
        assert_eq!(
            parse_version_stdout("heroku/8.3.0 darwin-arm64", &entry).as_deref(),
            Some("8.3.0")
        );
        assert_eq!(parse_version_stdout("not-heroku", &entry).as_deref(), Some("8.0.0"));
    }

    #[test]
    fn older_than_every_file_still_picks_the_last_file() {
        let entry = heroku_entry();
        assert_eq!(
            resolve_versioned_path(&entry, Some("7.0.0")).as_deref(),
            Some("heroku/8.6.0.json")
        );
    }

    #[test]
    fn stdout_parsers_cover_the_five_bundled_selectors() {
        let fig = VersionedCommand {
            command: vec!["fig".into(), "--version".into()],
            parse: "after-first-space".into(),
            regex: None,
            regex_group: None,
            parse_fallback: None,
            fallback: "2.0.0".into(),
            files: BTreeMap::from([("2.0.0".into(), "fig/2.0.0.json".into())]),
            applied: BTreeMap::new(),
        };
        assert_eq!(parse_version_stdout("fig 2.16.0", &fig).as_deref(), Some("2.16.0"));

        let shopify = VersionedCommand {
            command: vec!["shopify".into(), "version".into()],
            parse: "regex".into(),
            regex: Some("\\d+\\.\\d+\\.\\d+".into()),
            regex_group: Some(0),
            parse_fallback: Some(String::new()),
            fallback: "3.0.0".into(),
            files: BTreeMap::new(),
            applied: BTreeMap::new(),
        };
        assert_eq!(
            parse_version_stdout("Current Shopify CLI version: 3.50.0", &shopify).as_deref(),
            Some("3.50.0")
        );

        let infracost = VersionedCommand {
            command: vec!["infracost".into(), "--version".into()],
            parse: "semver-clean-after-space".into(),
            regex: None,
            regex_group: None,
            parse_fallback: None,
            fallback: "0.10.0".into(),
            files: BTreeMap::new(),
            applied: BTreeMap::new(),
        };
        assert_eq!(
            parse_version_stdout("Infracost v0.10.30", &infracost).as_deref(),
            Some("0.10.30")
        );

        let sdc = VersionedCommand {
            command: vec!["npx".into(), "@usermn/sdc".into(), "--version".into()],
            parse: "stdout".into(),
            regex: None,
            regex_group: None,
            parse_fallback: None,
            fallback: "0.0.0".into(),
            files: BTreeMap::new(),
            applied: BTreeMap::new(),
        };
        assert_eq!(parse_version_stdout("0.0.7\n", &sdc).as_deref(), Some("0.0.7\n"));
        assert_eq!(semver_clean("v0.10.30 extra").as_deref(), Some("0.10.30"));
    }
}
