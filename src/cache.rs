use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct Entry<V> {
    value: V,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct TtlCache<K, V>
where
    K: Eq + Hash,
{
    ttl: Duration,
    data: HashMap<K, Entry<V>>,
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + Hash,
    V: Clone,
{
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            data: HashMap::new(),
        }
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self::new(ttl)
    }

    pub fn insert(&mut self, key: K, value: V) {
        self.data.insert(
            key,
            Entry {
                value,
                expires_at: Instant::now() + self.ttl,
            },
        );
    }

    pub fn get(&mut self, key: &K) -> Option<V> {
        self.prune_expired();
        self.data.get(key).map(|entry| entry.value.clone())
    }

    pub fn get_stale(&self, key: &K) -> Option<V> {
        self.data.get(key).map(|entry| entry.value.clone())
    }

    pub fn prune_expired(&mut self) {
        let now = Instant::now();
        self.data.retain(|_, entry| entry.expires_at > now);
    }
}
