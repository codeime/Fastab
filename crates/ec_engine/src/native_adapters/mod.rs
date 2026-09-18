//! Named native adapters for hook bodies the typed compiler cannot emit.
//!
//! Bound by `bodySha256` + field. The catalog dump is compiled into the
//! library so `dump-adapters` can publish `adapters.json`. Evaluation stays
//! test-only until T3.1 wires adapters into the completion path.

use serde::Serialize;

const ADAPTERS_KIND: &str = "native-hook-adapters";
const ADAPTERS_VERSION: u64 = 1;

struct AdapterMeta {
    body_sha256: &'static str,
    field: &'static str,
    representative_hook_id: &'static str,
    reason: &'static str,
}

macro_rules! meta {
    ($sha:literal, $field:literal, $id:literal, $reason:literal) => {
        AdapterMeta {
            body_sha256: $sha,
            field: $field,
            representative_hook_id: $id,
            reason: $reason,
        }
    };
}

const ADAPTERS: &[AdapterMeta] = &[
    meta!(
        "16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad",
        "filterTemplateSuggestions",
        "direnv#filterTemplateSuggestions#0",
        "factory helper e disagrees across call sites"
    ),
    meta!(
        "06cf60a4db4009e789aa2efbfd2e826a8fc7be284a20bc56a257ff0ba2524a33",
        "postProcess",
        "bunx#postProcess#0",
        "free variable n is the closed-over command list"
    ),
    meta!(
        "08c4a4a0e91a87111822336c4aba36b82aa89bef9faac02831e7c1bafb0eeb1c",
        "postProcess",
        "just#postProcess#1",
        "helper c references console"
    ),
    meta!(
        "1db94727eae43e429172bdb647fcdce51562bc830797579fdd211681742b949b",
        "postProcess",
        "rush#postProcess#10",
        "helper P uses a non-simple parameter shape"
    ),
    meta!(
        "20afa25251f32cd950d69142214fb6b725cca32e75676c486610ddabb6200d58",
        "postProcess",
        "dcli#postProcess#0",
        "Array.sort plus Date formatting are outside typed IR"
    ),
    meta!(
        "40446df6c189303738556d8b980c04a816d639d9ed1576c2f1eee6df70b1ff3d",
        "postProcess",
        "limactl#postProcess#0",
        "factory helper e disagrees across call sites"
    ),
    meta!(
        "40f323a2aaed65f89013fd38fc15cd73181b00b577418036d25981f47b98ebb8",
        "postProcess",
        "git#postProcess#16",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "46cfb8ef98d11e187a3fb9a8756cf911956bd5f5bc48d22ce938faee7860f35b",
        "postProcess",
        "react-native#postProcess#5",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "47dd9ebffc5ba15a140bafd0c09167f71b38de57bb8782036e4edd0ba8049e89",
        "postProcess",
        "asdf#postProcess#1",
        "factory helper n disagrees across call sites"
    ),
    meta!(
        "566065d403b4e22dba74ec84e8e64ef2c541ce984fef4051e41baaf5a8941120",
        "postProcess",
        "cargo#postProcess#261",
        "helper le references console"
    ),
    meta!(
        "57813e77ada7aad9de83a53034b622f02d180c989cc9b6688aec5f271b614906",
        "postProcess",
        "fnm#postProcess#0",
        "free variables g/u plus Map are outside typed IR"
    ),
    meta!(
        "581af18c6c4ccb098615187809a15e6892771178da47cb9c7016272dc18fb572",
        "postProcess",
        "dcli#postProcess#1",
        "Array.sort plus Date formatting are outside typed IR"
    ),
    meta!(
        "599dd22a2fdc7873a899448dd4c11ffce9c0742c9a64e930819f57990b0c126f",
        "postProcess",
        "rustup#postProcess#8",
        "Date is not representable in typed IR"
    ),
    meta!(
        "5b988a428bdc5060c8f035d9d480ea81be6e411de28351871118d9e8bc7d4978",
        "postProcess",
        "tldr#postProcess#0",
        "dynamic RegExp is outside typed IR"
    ),
    meta!(
        "5cde47b72c7c8040bea8146f4c5bd7267bb7f07bb53b84964901d097f94b2d37",
        "postProcess",
        "rustup#postProcess#1",
        "factory helper o disagrees across call sites"
    ),
    meta!(
        "61d0b086797b88cb187f1bcc5b3a82c66b081a9756fde0e10cfde9fb90f5eabf",
        "postProcess",
        "rustup#postProcess#15",
        "factory helper o disagrees across call sites"
    ),
    meta!(
        "66349787d8ae1c1f6b8b895a43758316cff3ef10d8f8c70a9eaace0b079c937c",
        "postProcess",
        "pre-commit#postProcess#11",
        "YAML helper _s is not inlinable"
    ),
    meta!(
        "7684a7c68524c7c00f9309417061576eb590152382aea725b4687ea6824cfe6c",
        "postProcess",
        "brew#postProcess#15",
        "factory helper i disagrees across call sites"
    ),
    meta!(
        "775c0c96e9204671a2a03e3ec4839bfd73f1e9fdec79ce11f7d2b1abf9120c67",
        "postProcess",
        "deno#postProcess#0",
        "array of matcher arrows is not representable"
    ),
    meta!(
        "83c37762b8e6a3fb3dfef347ac4d34f35174c2ae0483908a2101a1c521a2ef1f",
        "postProcess",
        "yarn#postProcess#11",
        "Object.assign is not representable"
    ),
    meta!(
        "86847ea037183bc60ece6f12b5dc2764beb807a76a4930c2a28412a63d96e174",
        "postProcess",
        "yo#postProcess#0",
        "nested arrows in the title-case helper"
    ),
    meta!(
        "86e2f49405e79a81d3c4fd402009b27d5e352d1e80d69ee496dda22abc8c65b2",
        "postProcess",
        "task/taskwarrior#postProcess#0",
        "module helpers use Array.reduce"
    ),
    meta!(
        "97aa92d325bb5a2273a65c8204a97bbe4be8598b9d41cf129d8fa554d510fbf3",
        "postProcess",
        "asdf#postProcess#11",
        "factory helper n disagrees across call sites"
    ),
    meta!(
        "9e08e5a9bac41e7def677da3f7fc812fc90377d437d8121a05c1f661fecfb32a",
        "postProcess",
        "open#postProcess#1",
        "new Map is outside the typed Set-only constructor"
    ),
    meta!(
        "a148afe726cd7409a53bcfe722708bc9e35a5e9f7a409817a22783fe77527df3",
        "postProcess",
        "snaplet#postProcess#0",
        "typed JSON objects do not expose .src"
    ),
    meta!(
        "a2343033ad7ec234dc9cd89f566c5c9024a725655738aa583562d0c4557f0de5",
        "postProcess",
        "sake#postProcess#1",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "a8bce6f4bcf79ed952f5325ec0b5c7f1041eb0e10320fbd9beb629efb4071bbe",
        "postProcess",
        "task/taskwarrior#postProcess#33",
        "module helpers use Array.reduce"
    ),
    meta!(
        "b0575d96c049ffec2de9ce491f5319e3ad61c3c908d0dec5554aeda00ed971c7",
        "postProcess",
        "tailscale#postProcess#0",
        "factory helper s disagrees across call sites"
    ),
    meta!(
        "b3739a280026f1e961efaf2ded7bf918e22c909af433ec86f2eec02c6aadeca1",
        "postProcess",
        "snaplet#postProcess#2",
        "typed JSON objects do not expose .name on helper rows"
    ),
    meta!(
        "b3b72135b863b89e29cf44274d5e381daf51d9c53c3313cc68096fc1f2af8177",
        "postProcess",
        "pnpm#postProcess#10",
        "Object.assign is not representable"
    ),
    meta!(
        "baa2ab8ac2cd27aaf62bd6b24ddd1734a395dcbab0dfed6ad6ddf108d19ac709",
        "postProcess",
        "tccutil#postProcess#0",
        "new Map is outside the typed Set-only constructor"
    ),
    meta!(
        "bcb6a45f08256f9fd34e48ecbfb13cd9b5d825e96e308131538d16cb2f5400bc",
        "postProcess",
        "turbo#postProcess#1",
        "the in operator is unsupported"
    ),
    meta!(
        "c3fac3c2f48178fe13af582ab93d7b9bfc579951c78a805946b6563c415e8848",
        "postProcess",
        "just#postProcess#3",
        "helper c references console"
    ),
    meta!(
        "ca1383d43cf172af5d0f743e6933ef8e7154147c5ff549ad1b6fc4a6e88e90be",
        "postProcess",
        "task/taskwarrior#postProcess#51",
        "module helpers use Array.reduce"
    ),
    meta!(
        "d060a61ead077da1a1b2a779f8670f87f55010a22a34bdd3f8b8545378d0833e",
        "postProcess",
        "pre-commit#postProcess#10",
        "Array.reduce is not in the typed allowlist"
    ),
    meta!(
        "d3e93ba8a10aa6f82f74dd3df6cc51e81d815eb8e36072ed084d9a2dddcd6f61",
        "postProcess",
        "just#postProcess#5",
        "helper c references console"
    ),
    meta!(
        "de5329d88e6a3667531eace7c10a57034bb8ec9e643f910095a0baa80235de20",
        "postProcess",
        "git#postProcess#8",
        "inlined helper AST exceeds the 2048-node cap"
    ),
    meta!(
        "deba5217b1bd370de0396ae66042361a21ebd372bc0e308858dedca94ec79986",
        "postProcess",
        "vr#postProcess#0",
        "conditional object spread is not a suggestion object"
    ),
    meta!(
        "e0ff02cf4190764a1e6043c0ace62aaf37a63bffd693744f1072c87df04c710d",
        "postProcess",
        "kubecolor#postProcess#40",
        "free variable a is a module helper"
    ),
    meta!(
        "e66f8b0736bf23a063bda8970e1de8b9ac8f72328a53ac15671982af49b7f468",
        "postProcess",
        "snaplet#postProcess#4",
        "typed JSON objects do not expose .status"
    ),
    meta!(
        "e8289375fc0901b1f363b7525760614e99748125598c35e172710d7446292842",
        "postProcess",
        "deno#postProcess#3",
        "helper ie is recursive"
    ),
    meta!(
        "e8a02695049c0e386fb7345f0e54d5bd2d2ce1c04481e228e5e9f2bf4f583c14",
        "postProcess",
        "gource#postProcess#0",
        "spread on a partial suggestion is a type mismatch"
    ),
    meta!(
        "f04211ce9cc3b53755c0429dfc0fcaac7b07fe7d4a7d9021fc2c3c11cf5f6adb",
        "postProcess",
        "cf#postProcess#0",
        "factory helper s disagrees across call sites"
    ),
];

#[derive(Serialize)]
struct AdapterCatalogFile {
    version: u64,
    kind: &'static str,
    adapters: Vec<AdapterCatalogRow>,
}

#[derive(Serialize)]
struct AdapterCatalogRow {
    #[serde(rename = "bodySha256")]
    body_sha256: &'static str,
    field: &'static str,
    #[serde(rename = "representativeHookId")]
    representative_hook_id: &'static str,
    reason: &'static str,
}

/// JSON catalog written by `dump-adapters` and read by classify/compile.
pub fn dump_native_adapter_catalog() -> String {
    let mut adapters: Vec<AdapterCatalogRow> = ADAPTERS
        .iter()
        .map(|entry| AdapterCatalogRow {
            body_sha256: entry.body_sha256,
            field: entry.field,
            representative_hook_id: entry.representative_hook_id,
            reason: entry.reason,
        })
        .collect();
    adapters.sort_by(|left, right| {
        left.field
            .cmp(right.field)
            .then_with(|| left.body_sha256.cmp(right.body_sha256))
    });
    let catalog = AdapterCatalogFile {
        version: ADAPTERS_VERSION,
        kind: ADAPTERS_KIND,
        adapters,
    };
    format!("{}\n", serde_json::to_string_pretty(&catalog).expect("catalog"))
}

pub fn native_adapter_catalog_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/native-hooks/adapters.json")
}

#[cfg(test)]
mod bunx_names;
#[cfg(test)]
mod eval;
#[cfg(test)]
mod filter;
#[cfg(test)]
mod git_config_keys;
#[cfg(test)]
mod post_process;
#[cfg(test)]
mod turbo_icon;

#[cfg(test)]
mod tests {
    use serde_json::Value as JsonValue;

    use super::*;
    use crate::hook_baseline::{Expected, load_all};
    use crate::native_adapters::eval::{AdapterResult, expected_from_adapter};

    enum AdapterFn {
        PostProcess(fn(&str, &[String]) -> AdapterResult),
        Filter(fn(&[JsonValue]) -> AdapterResult),
    }

    fn adapter_eval(field: &str, body_sha256: &str) -> Option<AdapterFn> {
        use crate::native_adapters::{filter, post_process};
        Some(match (field, body_sha256) {
            ("filterTemplateSuggestions", "16eb363f9957097622cbe2ed626cfc4859c24b797e5e2acff58cf40f4f9fbbad") => {
                AdapterFn::Filter(filter::direnv_envrc)
            },
            ("postProcess", "06cf60a4db4009e789aa2efbfd2e826a8fc7be284a20bc56a257ff0ba2524a33") => {
                AdapterFn::PostProcess(|stdout, _| post_process::bunx_npx(stdout))
            },
            ("postProcess", "08c4a4a0e91a87111822336c4aba36b82aa89bef9faac02831e7c1bafb0eeb1c") => {
                AdapterFn::PostProcess(|stdout, _| post_process::just_assignments(stdout))
            },
            ("postProcess", "1db94727eae43e429172bdb647fcdce51562bc830797579fdd211681742b949b") => {
                AdapterFn::PostProcess(|stdout, _| post_process::rush_projects(stdout))
            },
            ("postProcess", "20afa25251f32cd950d69142214fb6b725cca32e75676c486610ddabb6200d58") => {
                AdapterFn::PostProcess(|stdout, _| post_process::dcli_devices(stdout))
            },
            ("postProcess", "40446df6c189303738556d8b980c04a816d639d9ed1576c2f1eee6df70b1ff3d") => {
                AdapterFn::PostProcess(|stdout, _| post_process::limactl_instances(stdout))
            },
            ("postProcess", "40f323a2aaed65f89013fd38fc15cd73181b00b577418036d25981f47b98ebb8") => {
                AdapterFn::PostProcess(|stdout, _| post_process::git_remotes(stdout))
            },
            ("postProcess", "46cfb8ef98d11e187a3fb9a8756cf911956bd5f5bc48d22ce938faee7860f35b") => {
                AdapterFn::PostProcess(|stdout, _| post_process::react_native_devices(stdout))
            },
            ("postProcess", "47dd9ebffc5ba15a140bafd0c09167f71b38de57bb8782036e4edd0ba8049e89") => {
                AdapterFn::PostProcess(|stdout, _| post_process::asdf_plugins(stdout))
            },
            ("postProcess", "566065d403b4e22dba74ec84e8e64ef2c541ce984fef4051e41baaf5a8941120") => {
                AdapterFn::PostProcess(|stdout, _| post_process::cargo_deps(stdout))
            },
            ("postProcess", "57813e77ada7aad9de83a53034b622f02d180c989cc9b6688aec5f271b614906") => {
                AdapterFn::PostProcess(|stdout, _| post_process::fnm_list(stdout))
            },
            ("postProcess", "581af18c6c4ccb098615187809a15e6892771178da47cb9c7016272dc18fb572") => {
                AdapterFn::PostProcess(|stdout, _| post_process::dcli_access_keys(stdout))
            },
            ("postProcess", "599dd22a2fdc7873a899448dd4c11ffce9c0742c9a64e930819f57990b0c126f") => {
                AdapterFn::PostProcess(|stdout, _| post_process::rustup_channels(stdout))
            },
            ("postProcess", "5b988a428bdc5060c8f035d9d480ea81be6e411de28351871118d9e8bc7d4978") => {
                AdapterFn::PostProcess(|stdout, _| post_process::tldr_pages(stdout))
            },
            ("postProcess", "5cde47b72c7c8040bea8146f4c5bd7267bb7f07bb53b84964901d097f94b2d37") => {
                AdapterFn::PostProcess(|stdout, _| post_process::rustup_targets(stdout))
            },
            ("postProcess", "61d0b086797b88cb187f1bcc5b3a82c66b081a9756fde0e10cfde9fb90f5eabf") => {
                AdapterFn::PostProcess(|stdout, _| post_process::rustup_installed(stdout))
            },
            ("postProcess", "66349787d8ae1c1f6b8b895a43758316cff3ef10d8f8c70a9eaace0b079c937c") => {
                AdapterFn::PostProcess(|stdout, _| post_process::precommit_hooks(stdout))
            },
            ("postProcess", "7684a7c68524c7c00f9309417061576eb590152382aea725b4687ea6824cfe6c") => {
                AdapterFn::PostProcess(|stdout, _| post_process::brew_packages(stdout))
            },
            ("postProcess", "775c0c96e9204671a2a03e3ec4839bfd73f1e9fdec79ce11f7d2b1abf9120c67") => {
                AdapterFn::PostProcess(|stdout, _| post_process::deno_url(stdout))
            },
            ("postProcess", "83c37762b8e6a3fb3dfef347ac4d34f35174c2ae0483908a2101a1c521a2ef1f") => {
                AdapterFn::PostProcess(post_process::yarn_deps)
            },
            ("postProcess", "86847ea037183bc60ece6f12b5dc2764beb807a76a4930c2a28412a63d96e174") => {
                AdapterFn::PostProcess(|stdout, _| post_process::yo_generators(stdout))
            },
            ("postProcess", "86e2f49405e79a81d3c4fd402009b27d5e352d1e80d69ee496dda22abc8c65b2") => {
                AdapterFn::PostProcess(|stdout, _| post_process::taskwarrior_a(stdout))
            },
            ("postProcess", "97aa92d325bb5a2273a65c8204a97bbe4be8598b9d41cf129d8fa554d510fbf3") => {
                AdapterFn::PostProcess(|stdout, _| post_process::asdf_versions(stdout))
            },
            ("postProcess", "9e08e5a9bac41e7def677da3f7fc812fc90377d437d8121a05c1f661fecfb32a") => {
                AdapterFn::PostProcess(|stdout, _| post_process::open_apps(stdout))
            },
            ("postProcess", "a148afe726cd7409a53bcfe722708bc9e35a5e9f7a409817a22783fe77527df3") => {
                AdapterFn::PostProcess(|stdout, _| post_process::snaplet_cloud(stdout))
            },
            ("postProcess", "a2343033ad7ec234dc9cd89f566c5c9024a725655738aa583562d0c4557f0de5") => {
                AdapterFn::PostProcess(|stdout, _| post_process::sake_groups(stdout))
            },
            ("postProcess", "a8bce6f4bcf79ed952f5325ec0b5c7f1041eb0e10320fbd9beb629efb4071bbe") => {
                AdapterFn::PostProcess(|stdout, _| post_process::taskwarrior_b(stdout))
            },
            ("postProcess", "b0575d96c049ffec2de9ce491f5319e3ad61c3c908d0dec5554aeda00ed971c7") => {
                AdapterFn::PostProcess(|stdout, _| post_process::tailscale_peers(stdout))
            },
            ("postProcess", "b3739a280026f1e961efaf2ded7bf918e22c909af433ec86f2eec02c6aadeca1") => {
                AdapterFn::PostProcess(|stdout, _| post_process::snaplet_backups(stdout))
            },
            ("postProcess", "b3b72135b863b89e29cf44274d5e381daf51d9c53c3313cc68096fc1f2af8177") => {
                AdapterFn::PostProcess(post_process::pnpm_deps)
            },
            ("postProcess", "baa2ab8ac2cd27aaf62bd6b24ddd1734a395dcbab0dfed6ad6ddf108d19ac709") => {
                AdapterFn::PostProcess(|stdout, _| post_process::tccutil_services(stdout))
            },
            ("postProcess", "bcb6a45f08256f9fd34e48ecbfb13cd9b5d825e96e308131538d16cb2f5400bc") => {
                AdapterFn::PostProcess(|stdout, _| post_process::turbo_pipeline(stdout))
            },
            ("postProcess", "c3fac3c2f48178fe13af582ab93d7b9bfc579951c78a805946b6563c415e8848") => {
                AdapterFn::PostProcess(|stdout, _| post_process::just_recipe_list(stdout))
            },
            ("postProcess", "ca1383d43cf172af5d0f743e6933ef8e7154147c5ff549ad1b6fc4a6e88e90be") => {
                AdapterFn::PostProcess(|stdout, _| post_process::taskwarrior_c(stdout))
            },
            ("postProcess", "d060a61ead077da1a1b2a779f8670f87f55010a22a34bdd3f8b8545378d0833e") => {
                AdapterFn::PostProcess(|stdout, _| post_process::git_remotes(stdout))
            },
            ("postProcess", "d3e93ba8a10aa6f82f74dd3df6cc51e81d815eb8e36072ed084d9a2dddcd6f61") => {
                AdapterFn::PostProcess(post_process::just_recipes_arity)
            },
            ("postProcess", "de5329d88e6a3667531eace7c10a57034bb8ec9e643f910095a0baa80235de20") => {
                AdapterFn::PostProcess(|stdout, _| post_process::git_config(stdout))
            },
            ("postProcess", "deba5217b1bd370de0396ae66042361a21ebd372bc0e308858dedca94ec79986") => {
                AdapterFn::PostProcess(|stdout, _| post_process::vr_scripts(stdout))
            },
            ("postProcess", "e0ff02cf4190764a1e6043c0ace62aaf37a63bffd693744f1072c87df04c710d") => {
                AdapterFn::PostProcess(|stdout, _| post_process::kubectl_cronjob(stdout))
            },
            ("postProcess", "e66f8b0736bf23a063bda8970e1de8b9ac8f72328a53ac15671982af49b7f468") => {
                AdapterFn::PostProcess(|stdout, _| post_process::snaplet_status(stdout))
            },
            ("postProcess", "e8289375fc0901b1f363b7525760614e99748125598c35e172710d7446292842") => {
                AdapterFn::PostProcess(post_process::deno_docs)
            },
            ("postProcess", "e8a02695049c0e386fb7345f0e54d5bd2d2ce1c04481e228e5e9f2bf4f583c14") => {
                AdapterFn::PostProcess(|stdout, _| post_process::gource_displays(stdout))
            },
            ("postProcess", "f04211ce9cc3b53755c0429dfc0fcaac7b07fe7d4a7d9021fc2c3c11cf5f6adb") => {
                AdapterFn::PostProcess(|stdout, _| post_process::cf_lines(stdout))
            },
            _ => return None,
        })
    }

    fn evaluate_adapter(field: &str, body_sha256: &str, args: &[JsonValue]) -> Option<Expected> {
        let eval = adapter_eval(field, body_sha256)?;
        let result = match eval {
            AdapterFn::PostProcess(function) => {
                let stdout = args.first().and_then(JsonValue::as_str).unwrap_or("");
                let tokens = args
                    .get(1)
                    .and_then(JsonValue::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| item.as_str().unwrap_or("").to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                function(stdout, &tokens)
            },
            AdapterFn::Filter(function) => {
                let suggestions = args.first().and_then(JsonValue::as_array).cloned().unwrap_or_default();
                function(&suggestions)
            },
        };
        Some(expected_from_adapter(result))
    }

    #[test]
    fn adapters_json_matches_registry() {
        let path = native_adapter_catalog_path();
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed,
            dump_native_adapter_catalog(),
            "adapters.json is stale; run `cargo run -p ec_engine --example dump-adapters`"
        );
    }

    #[test]
    fn native_adapters_baseline_parity() {
        let baselines = load_all().expect("T1.2 baselines");
        let mut compared = 0usize;
        let mut failures = Vec::new();
        let mut seen = 0usize;
        for baseline in &baselines {
            if adapter_eval(&baseline.field, &baseline.body_sha256).is_none() {
                continue;
            }
            seen += 1;
            for case in &baseline.cases {
                if matches!(case.expected, Expected::Timeout { .. }) {
                    continue;
                }
                let Some(actual) = evaluate_adapter(&baseline.field, &baseline.body_sha256, &case.args) else {
                    failures.push(format!("{}:{} missing adapter", baseline.field, baseline.body_sha256));
                    continue;
                };
                if actual == case.expected {
                    compared += 1;
                } else {
                    failures.push(format!(
                        "{} {} {} actual={actual:?} expected={:?}",
                        baseline.field, baseline.body_sha256, case.id, case.expected
                    ));
                }
            }
        }
        assert_eq!(
            seen,
            ADAPTERS.len(),
            "every registered adapter must have a T1.2 baseline"
        );
        assert!(
            failures.is_empty(),
            "native adapter baseline mismatches ({}): {}",
            failures.len(),
            failures.iter().take(20).cloned().collect::<Vec<_>>().join(" | ")
        );
        assert!(
            compared >= ADAPTERS.len(),
            "native adapter baseline parity compared {compared} cases"
        );
    }
}
