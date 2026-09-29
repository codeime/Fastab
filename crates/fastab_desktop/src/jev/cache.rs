//! A small memory-only cache of validated candidate IDs, never insertion text.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const CAPACITY: usize = 64;
const TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheKey(pub [u8; 32]);

struct Entry {
    key: CacheKey,
    choice: String,
    created: Instant,
}

#[derive(Default)]
pub struct RecommendationCache {
    entries: VecDeque<Entry>,
}

impl RecommendationCache {
    pub fn get(&mut self, key: CacheKey, now: Instant) -> Option<String> {
        self.entries
            .retain(|entry| now.saturating_duration_since(entry.created) < TTL);
        let index = self.entries.iter().position(|entry| entry.key == key)?;
        let entry = self.entries.remove(index)?;
        let choice = entry.choice.clone();
        self.entries.push_back(entry);
        Some(choice)
    }

    pub fn insert(&mut self, key: CacheKey, choice: String, now: Instant) {
        // Response decoding guarantees this too; keep the storage boundary
        // closed so future callers cannot retain arbitrary response strings.
        if choice.is_empty()
            || choice.len() > 32
            || !choice
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return;
        }
        self.entries
            .retain(|entry| entry.key != key && now.saturating_duration_since(entry.created) < TTL);
        if self.entries.len() >= CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry {
            key,
            choice,
            created: now,
        });
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_bounded_and_hits_never_extend_the_ttl() {
        let now = Instant::now();
        let mut cache = RecommendationCache::default();
        for id in 0..=64 {
            cache.insert(CacheKey([id; 32]), "c0".into(), now);
        }
        assert_eq!(cache.entries.len(), CAPACITY);
        assert!(cache.get(CacheKey([0; 32]), now).is_none());
        assert_eq!(
            cache
                .get(CacheKey([64; 32]), now + TTL - Duration::from_millis(1))
                .as_deref(),
            Some("c0")
        );
        assert!(cache.get(CacheKey([64; 32]), now + TTL).is_none());
    }

    #[test]
    fn changed_identity_misses_and_only_ids_are_stored() {
        let now = Instant::now();
        let mut cache = RecommendationCache::default();
        cache.insert(CacheKey([1; 32]), "keep_local".into(), now);
        assert!(cache.get(CacheKey([2; 32]), now).is_none());
        assert_eq!(cache.get(CacheKey([1; 32]), now).as_deref(), Some("keep_local"));
        cache.insert(CacheKey([3; 32]), "echo unwanted payload".into(), now);
        assert_eq!(cache.entries.len(), 1);
        cache.clear();
        assert!(cache.get(CacheKey([1; 32]), now).is_none());
    }
}
