//! A small keyed cache for values that have to be fetched.
//!
//! Waku had grown three hand-rolled versions of the same thing — the git branch
//! per project, the diff file list, session transcripts — each with its own
//! generation counter to stop a slow result from overwriting a newer one. This
//! is that pattern once, borrowing the shape of TanStack Query: read from
//! `render`, get `Pending` on a miss, start the fetch, and let the value arrive
//! on a later frame.
//!
//! What it deliberately does not do: no refetch intervals, no retry policy, no
//! background revalidation. Fetching is the caller's job, so the cache never
//! needs to own an executor, be generic over an entity, or hold a callback.
//!
//! ```ignore
//! match self.branches.read(&path) {
//!     Query::Ready(branch) => branch.clone(),
//!     Query::Pending => None,        // draw the last known value
//!     Query::Missing(token) => {     // first asker starts the work
//!         cx.spawn(async move |waku, cx| {
//!             let branch = cx.background_executor().spawn(fetch).await;
//!             waku.update(cx, |waku, cx| {
//!                 if waku.branches.fulfill(token, branch) {
//!                     cx.notify();
//!                 }
//!             })
//!         })
//!         .detach();
//!         None
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::sync::Arc;

/// Proof that a caller is the one who should fetch a key, carrying the
/// generation the request was made at.
///
/// [`QueryCache::fulfill`] refuses a token whose generation has been superseded
/// by an invalidation, which is what stops a slow fetch from overwriting a
/// newer value. Holding a token means the fetch is in flight, so a second
/// reader gets [`Query::Pending`] rather than starting duplicate work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchToken<K> {
    key: K,
    generation: u64,
}

/// What a read found.
#[derive(Debug)]
pub enum Query<K, V> {
    /// Cached and current.
    Ready(Arc<V>),
    /// Someone is already fetching this; draw a placeholder.
    Pending,
    /// Nobody is fetching yet, and the caller now owns doing so.
    Missing(FetchToken<K>),
}

#[derive(Debug)]
enum Slot<V> {
    Loading,
    Ready(Arc<V>),
}

#[derive(Debug)]
struct Cached<V> {
    slot: Slot<V>,
    /// Bumped on invalidation, so an in-flight fetch can be recognised as stale.
    generation: u64,
    /// Read counter, for eviction. Cheaper than maintaining an LRU list and
    /// good enough to keep a bounded cache holding what is actually used.
    last_used: u64,
}

/// A bounded key/value cache with in-flight tracking.
#[derive(Debug)]
pub struct QueryCache<K, V> {
    entries: HashMap<K, Cached<V>>,
    capacity: usize,
    clock: u64,
    generation: u64,
}

impl<K: Clone + Eq + Hash, V> QueryCache<K, V> {
    /// `capacity` counts resolved values; entries being fetched are never
    /// evicted, since dropping one would strand its token.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
            clock: 0,
            generation: 0,
        }
    }

    /// Reads a key, claiming the fetch if nobody else has.
    ///
    /// Safe to call from `render`: it only touches memory, and the miss path
    /// hands back a token rather than doing any work itself.
    pub fn read(&mut self, key: &K) -> Query<K, V> {
        self.clock += 1;
        let clock = self.clock;
        if let Some(cached) = self.entries.get_mut(key) {
            cached.last_used = clock;
            return match &cached.slot {
                Slot::Ready(value) => Query::Ready(Arc::clone(value)),
                Slot::Loading => Query::Pending,
            };
        }
        let generation = self.generation;
        self.entries.insert(
            key.clone(),
            Cached {
                slot: Slot::Loading,
                generation,
                last_used: clock,
            },
        );
        Query::Missing(FetchToken {
            key: key.clone(),
            generation,
        })
    }

    /// Reads without claiming a fetch, for callers that only want a hit.
    #[cfg(test)]
    pub fn peek(&self, key: &K) -> Option<Arc<V>> {
        match self.entries.get(key) {
            Some(Cached {
                slot: Slot::Ready(value),
                ..
            }) => Some(Arc::clone(value)),
            _ => None,
        }
    }

    /// Stores a fetched value. Returns whether it was accepted — a token from
    /// before an invalidation is dropped, so stale results cannot win.
    pub fn fulfill(&mut self, token: FetchToken<K>, value: V) -> bool {
        let Entry::Occupied(mut entry) = self.entries.entry(token.key) else {
            // Invalidated and evicted while the fetch was running.
            return false;
        };
        if entry.get().generation != token.generation {
            return false;
        }
        self.clock += 1;
        entry.get_mut().slot = Slot::Ready(Arc::new(value));
        entry.get_mut().last_used = self.clock;
        self.evict_over_capacity();
        true
    }

    /// Abandons a fetch that failed, so a later read retries rather than
    /// waiting on a result that will never come.
    ///
    /// Also used by debounced queries that become irrelevant before their
    /// fetch starts. Abandoning removes only the matching generation, so a
    /// newer request for the same key cannot be cancelled by an older token.
    pub fn abandon(&mut self, token: FetchToken<K>) {
        if self
            .entries
            .get(&token.key)
            .is_some_and(|cached| cached.generation == token.generation)
        {
            self.entries.remove(&token.key);
        }
    }

    /// Drops a key's value and invalidates any fetch in flight for it.
    pub fn invalidate(&mut self, key: &K) {
        self.generation += 1;
        self.entries.remove(key);
    }

    /// Drops every cached value and invalidates all fetches in flight.
    pub fn clear(&mut self) {
        self.generation += 1;
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn evict_over_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            let victim = self
                .entries
                .iter()
                .filter(|(_, cached)| matches!(cached.slot, Slot::Ready(_)))
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key.clone());
            // Only in-flight entries are left; evicting one would strand its
            // token, so let the cache run over until they resolve.
            let Some(victim) = victim else { break };
            self.entries.remove(&victim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> QueryCache<&'static str, String> {
        QueryCache::new(4)
    }

    #[test]
    fn a_miss_hands_out_one_token_and_later_readers_wait() {
        let mut cache = cache();

        let Query::Missing(token) = cache.read(&"a") else {
            panic!("first read owns the fetch");
        };
        assert!(
            matches!(cache.read(&"a"), Query::Pending),
            "a second reader must not start duplicate work"
        );

        assert!(cache.fulfill(token, "value".into()));
        let Query::Ready(value) = cache.read(&"a") else {
            panic!("resolved");
        };
        assert_eq!(*value, "value");
    }

    #[test]
    fn a_result_invalidated_mid_flight_is_refused() {
        let mut cache = cache();
        let Query::Missing(token) = cache.read(&"a") else {
            panic!()
        };

        // The underlying data changed while the fetch was running.
        cache.invalidate(&"a");

        assert!(
            !cache.fulfill(token, "stale".into()),
            "a fetch started before the change must not win"
        );
        assert!(
            matches!(cache.read(&"a"), Query::Missing(_)),
            "and the key is fetched again"
        );
    }

    #[test]
    fn a_newer_fetch_is_not_refused_by_an_older_invalidation() {
        let mut cache = cache();
        let Query::Missing(first) = cache.read(&"a") else {
            panic!()
        };
        cache.invalidate(&"a");
        let Query::Missing(second) = cache.read(&"a") else {
            panic!()
        };

        assert!(!cache.fulfill(first, "stale".into()));
        assert!(cache.fulfill(second, "fresh".into()));
        assert_eq!(*cache.peek(&"a").unwrap(), "fresh");
    }

    #[test]
    fn abandoning_lets_the_next_read_retry() {
        let mut cache = cache();
        let Query::Missing(token) = cache.read(&"a") else {
            panic!()
        };
        cache.abandon(token);

        assert!(
            matches!(cache.read(&"a"), Query::Missing(_)),
            "a failed fetch must not leave the key pending forever"
        );
    }

    #[test]
    fn capacity_evicts_the_least_recently_read() {
        let mut cache: QueryCache<&str, String> = QueryCache::new(2);
        for key in ["a", "b"] {
            let Query::Missing(token) = cache.read(&key) else {
                panic!()
            };
            cache.fulfill(token, key.to_string());
        }
        // Touch "a" so "b" becomes the coldest.
        assert!(cache.peek(&"a").is_some());
        cache.read(&"a");

        let Query::Missing(token) = cache.read(&"c") else {
            panic!()
        };
        cache.fulfill(token, "c".into());

        assert_eq!(cache.len(), 2);
        assert!(cache.peek(&"a").is_some(), "recently read survives");
        assert!(cache.peek(&"c").is_some());
        assert!(cache.peek(&"b").is_none(), "coldest was evicted");
    }

    #[test]
    fn in_flight_entries_are_never_evicted() {
        let mut cache: QueryCache<&str, String> = QueryCache::new(1);
        let Query::Missing(pending) = cache.read(&"slow") else {
            panic!()
        };
        let Query::Missing(quick) = cache.read(&"quick") else {
            panic!()
        };
        cache.fulfill(quick, "done".into());

        // Over capacity, but evicting the in-flight key would strand its token.
        assert!(matches!(cache.read(&"slow"), Query::Pending));
        assert!(cache.fulfill(pending, "arrived".into()));
        assert_eq!(*cache.peek(&"slow").unwrap(), "arrived");
    }
}
