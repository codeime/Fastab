//! Generator result caches that used to live on the QuickJS host.
//!
//! Fig `runCachedGenerator` / `getScriptSuggestions` keying is unchanged: a
//! generator without a `cache` block is uncached unless
//! `beta.autocomplete.auto-cache` is on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ir::{ArgSpec, Spec};
use crate::runtime::Suggestion;

/// Ceiling for each per-engine hook cache. Directory-keyed generators mint a
/// new entry per cwd, so a long desktop session would otherwise grow these
/// maps without bound. Wholesale clearing at the cap is fine: entries are
/// cheap to regenerate and the cap is far above one session's working set.
const MAX_CACHE_ENTRIES: usize = 512;

thread_local! {
    static CURRENT: std::cell::RefCell<Option<Arc<HookCache>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone)]
struct CacheEntry<T> {
    value: T,
    fetched_at: Instant,
}

/// Fig `CacheEntry.entry`: both strategies serve the stored value until its
/// TTL passes, then refetch and hand back the *fresh* result. (`swrCache`
/// only returns the stale value while another fetch is in flight, which a
/// synchronous engine never has.) `None` never expires.
#[derive(Clone, Copy)]
struct CachePolicy {
    ttl: Option<Duration>,
}

/// Avoid leaking cache keys: store the optional key as owned on the policy.
struct OwnedCachePolicy {
    by_directory: bool,
    key: Option<String>,
    ttl: Option<Duration>,
}

pub struct HookCache {
    /// `custom` generator results, keyed like Fig's `generatorCache`.
    suggestion_cache: Mutex<HashMap<String, CacheEntry<Vec<Suggestion>>>>,
    /// Script generator stdout. Fig caches the `executeCommand` output and
    /// re-applies `splitOn` / `postProcess` on every hit, so the hook still
    /// sees the current tokens; caching rows here would freeze them.
    script_output_cache: Mutex<HashMap<String, CacheEntry<String>>>,
    spec_cache: Mutex<HashMap<String, Spec>>,
}

impl Default for HookCache {
    fn default() -> Self {
        Self {
            suggestion_cache: Mutex::new(HashMap::new()),
            script_output_cache: Mutex::new(HashMap::new()),
            spec_cache: Mutex::new(HashMap::new()),
        }
    }
}

impl HookCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn bind(self: &Arc<Self>) -> BoundCache {
        CURRENT.with(|cell| {
            let previous = cell.replace(Some(Arc::clone(self)));
            BoundCache { previous }
        })
    }

    pub fn clear(&self) {
        self.suggestion_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
        self.script_output_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
        self.spec_cache.lock().unwrap_or_else(|err| err.into_inner()).clear();
    }

    /// Bytes retained by the three maps. Hash-map nodes are omitted. Each
    /// cached spec is measured on its own, and an option `Arc` shared by two
    /// entries is counted in each. This number does not evict; the maps still
    /// clear only when they pass [`MAX_CACHE_ENTRIES`] or [`HookCache::clear`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn allocated_bytes(&self) -> usize {
        fn string_key(key: &str) -> usize {
            std::mem::size_of::<String>() + key.len()
        }

        fn suggestion_heap(suggestion: &Suggestion) -> usize {
            suggestion.name.len()
                + suggestion.description.len()
                + suggestion.kind.len()
                + suggestion.args_hint.len()
                + suggestion.insert_value.as_ref().map_or(0, String::len)
                + suggestion.display_name.as_ref().map_or(0, String::len)
                + suggestion.primary_name.as_ref().map_or(0, String::len)
                + suggestion.separator_to_add.as_ref().map_or(0, String::len)
                + suggestion.icon.as_ref().map_or(0, String::len)
                + suggestion.original_type.as_ref().map_or(0, String::len)
                + suggestion.query_term.as_ref().map_or(0, String::len)
                + suggestion.alias_names.capacity() * std::mem::size_of::<String>()
                + suggestion.alias_names.iter().map(String::len).sum::<usize>()
        }

        let suggestion_bytes = {
            let suggestions = self.suggestion_cache.lock().unwrap_or_else(|err| err.into_inner());
            suggestions
                .iter()
                .map(|(key, entry)| {
                    string_key(key)
                        + std::mem::size_of::<CacheEntry<Vec<Suggestion>>>()
                        + entry.value.capacity() * std::mem::size_of::<Suggestion>()
                        + entry.value.iter().map(suggestion_heap).sum::<usize>()
                })
                .sum::<usize>()
        };
        let script_bytes = {
            let scripts = self.script_output_cache.lock().unwrap_or_else(|err| err.into_inner());
            scripts
                .iter()
                .map(|(key, entry)| string_key(key) + std::mem::size_of::<CacheEntry<String>>() + entry.value.len())
                .sum::<usize>()
        };
        let spec_bytes = {
            let specs = self.spec_cache.lock().unwrap_or_else(|err| err.into_inner());
            specs
                .iter()
                .map(|(key, spec)| string_key(key) + spec.allocated_bytes())
                .sum::<usize>()
        };
        suggestion_bytes + script_bytes + spec_bytes
    }
}

pub struct BoundCache {
    previous: Option<Arc<HookCache>>,
}

impl Drop for BoundCache {
    fn drop(&mut self) {
        CURRENT.with(|cell| {
            *cell.borrow_mut() = self.previous.take();
        });
    }
}

fn current() -> Option<Arc<HookCache>> {
    CURRENT.with(|cell| cell.borrow().clone())
}

fn evict_at_cap<T>(cache: &mut HashMap<String, T>, key: &str) {
    if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(key) {
        cache.clear();
    }
}

/// Fig `runCachedGenerator`: `[cacheByDirectory ? cwd : undefined, cacheKey ||
/// fallback].toString()`. `fallback` is the joined token array for `custom`
/// generators and the serialized `executeCommand` input for scripts.
pub fn cache_key(cache_by_directory: bool, cwd: &str, cache_key: Option<&str>, fallback: &str) -> String {
    let second = cache_key.filter(|key| !key.is_empty()).unwrap_or(fallback);
    if cache_by_directory {
        format!("{cwd},{second}")
    } else {
        format!(",{second}")
    }
}

/// Fig `getScriptSuggestions` caches on `JSON.stringify(executeCommandInput)`
/// — the resolved command, its args and the cwd — never on the typed tokens,
/// so two generators that compute different commands cannot share an entry
/// and the same command in another directory does not either.
pub fn script_cache_fallback(command: &str, args: &[String], cwd: &str) -> String {
    serde_json::json!({ "command": command, "args": args, "cwd": cwd }).to_string()
}

/// Fig `runCachedGenerator` falls back to `tokenArray.join(" ")` for
/// generators that do not run a script.
pub fn custom_cache_fallback(tokens: &[String]) -> String {
    tokens.join(" ")
}

/// Fig `runCachedGenerator` + `CacheEntry.entry`. A generator without a
/// `cache` block is uncached unless `beta.autocomplete.auto-cache` is on,
/// which gives it `{ strategy: "stale-while-revalidate", ttl: 1000 }`. With
/// a block, the TTL rules follow the JS arithmetic exactly: `max-age` with
/// no `ttl` compares against `NaN` and never expires, while
/// `stale-while-revalidate` (the default) defaults `maxAge` to 0 and
/// refetches on every turn — `cache: { cacheByDirectory: true }` alone is
/// therefore not a cache at all.
fn owned_cache_policy(arg: &ArgSpec) -> Option<OwnedCachePolicy> {
    let has_explicit = arg.cache_key.is_some()
        || arg.cache_by_directory.is_some()
        || arg.cache_ttl_ms.is_some()
        || arg.cache_strategy.is_some();
    if !has_explicit {
        let auto = fastab_settings::settings::get_bool_or("beta.autocomplete.auto-cache", false);
        return auto.then(|| OwnedCachePolicy {
            by_directory: false,
            key: None,
            ttl: Some(Duration::from_millis(1_000)),
        });
    }
    let max_age = arg.cache_strategy.as_deref() == Some("max-age");
    let ttl = match arg.cache_ttl_ms {
        Some(ms) => Some(Duration::from_millis(u64::try_from(ms).unwrap_or(0))),
        None if max_age => None,
        None => Some(Duration::ZERO),
    };
    Some(OwnedCachePolicy {
        by_directory: arg.cache_by_directory.unwrap_or(false),
        key: arg.cache_key.clone(),
        ttl,
    })
}

fn cache_get<T: Clone>(cache: &Mutex<HashMap<String, CacheEntry<T>>>, key: &str, policy: CachePolicy) -> Option<T> {
    let mut cache = cache.lock().unwrap_or_else(|err| err.into_inner());
    let expired = cache
        .get(key)
        .is_some_and(|entry| policy.ttl.is_some_and(|ttl| entry.fetched_at.elapsed() > ttl));
    if expired {
        cache.remove(key);
        return None;
    }
    cache.get(key).map(|entry| entry.value.clone())
}

fn cache_put<T>(cache: &Mutex<HashMap<String, CacheEntry<T>>>, key: String, value: T) {
    let mut cache = cache.lock().unwrap_or_else(|err| err.into_inner());
    evict_at_cap(&mut cache, &key);
    cache.insert(
        key,
        CacheEntry {
            value,
            fetched_at: Instant::now(),
        },
    );
}

fn run_cached<T: Clone>(
    cache: &Mutex<HashMap<String, CacheEntry<T>>>,
    arg: &ArgSpec,
    cwd: &str,
    kind: &str,
    fallback_key: &str,
    run: impl FnOnce() -> T,
) -> T {
    let Some(policy) = owned_cache_policy(arg) else {
        return run();
    };
    let key = format!(
        "{kind}:{}",
        cache_key(policy.by_directory, cwd, policy.key.as_deref(), fallback_key)
    );
    let lookup = CachePolicy { ttl: policy.ttl };
    if let Some(hit) = cache_get(cache, &key, lookup) {
        return hit;
    }
    let value = run();
    cache_put(cache, key, value.clone());
    value
}

pub fn cached_suggestions(
    arg: &ArgSpec,
    cwd: &str,
    kind: &str,
    fallback_key: &str,
    run: impl FnOnce() -> Vec<Suggestion>,
) -> Vec<Suggestion> {
    match current() {
        Some(cache) => run_cached(&cache.suggestion_cache, arg, cwd, kind, fallback_key, run),
        None => run(),
    }
}

pub fn cached_script_output(arg: &ArgSpec, cwd: &str, fallback_key: &str, run: impl FnOnce() -> String) -> String {
    match current() {
        Some(cache) => run_cached(&cache.script_output_cache, arg, cwd, "script", fallback_key, run),
        None => run(),
    }
}

pub fn cached_spec(cache_key: &str, run: impl FnOnce() -> Option<Spec>) -> Option<Spec> {
    let Some(cache) = current() else {
        return run();
    };
    {
        let map = cache.spec_cache.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(spec) = map.get(cache_key) {
            return Some(spec.clone());
        }
    }
    let value = run()?;
    let mut map = cache.spec_cache.lock().unwrap_or_else(|err| err.into_inner());
    evict_at_cap(&mut map, cache_key);
    map.insert(cache_key.to_string(), value.clone());
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hook_cache_reports_no_retained_bytes() {
        assert_eq!(HookCache::default().allocated_bytes(), 0);
    }

    #[test]
    fn spec_entry_counts_the_key_and_tree_and_clear_drops_it() {
        let cache = HookCache::default();
        let spec = Spec {
            names: vec!["tool".into()],
            description: "demo".into(),
            ..Spec::default()
        };
        let expected = std::mem::size_of::<String>() + "tool-spec".len() + spec.allocated_bytes();
        cache
            .spec_cache
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert("tool-spec".into(), spec);
        assert_eq!(cache.allocated_bytes(), expected);
        cache.clear();
        assert_eq!(cache.allocated_bytes(), 0);
    }

    #[test]
    fn script_and_suggestion_payloads_are_counted_without_map_nodes() {
        let cache = HookCache::default();
        let aliases = vec!["g".to_owned()];
        assert_eq!(aliases.capacity(), 1);
        let suggestion = Suggestion::new("git", "checkout", "subcommand").with_alias_names(aliases);
        let rows = vec![suggestion];
        assert_eq!(rows.capacity(), 1);
        let suggestion_expected = std::mem::size_of::<String>()
            + "rows".len()
            + std::mem::size_of::<CacheEntry<Vec<Suggestion>>>()
            + std::mem::size_of::<Suggestion>()
            + "git".len()
            + "checkout".len()
            + "subcommand".len()
            + std::mem::size_of::<String>()
            + "g".len();
        cache_put(&cache.suggestion_cache, "rows".into(), rows);
        let script_expected =
            std::mem::size_of::<String>() + "out".len() + std::mem::size_of::<CacheEntry<String>>() + "stdout".len();
        cache_put(&cache.script_output_cache, "out".into(), "stdout".into());
        assert_eq!(cache.allocated_bytes(), suggestion_expected + script_expected);
    }

    #[test]
    fn hook_cache_entry_cap_stays_at_512() {
        assert_eq!(MAX_CACHE_ENTRIES, 512);
    }
}
