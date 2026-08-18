//! AT-SPI element cache for Linux.
//! Stores element keys (u64 hash) indexed by (pid, xid) → element_index.
//!
//! The locked-HashMap plumbing lives in `cua_driver_core::element_cache` — see
//! `docs/dedup-audit.md` item #3. This module owns the Linux-specific
//! `CacheKey` and `CachedSnapshot` (no Drop needed — `Vec<u64>` frees
//! itself).

use super::AtspiNode;
use cua_driver_core::element_cache::ElementCacheCore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const MAX_PEAK_IDENTITIES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProcessIdentity {
    pid: u32,
    start_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub pid: u32,
    pub xid: u64,
}

pub struct CachedSnapshot {
    /// element_index → element_key (opaque AT-SPI path hash).
    pub elements: Vec<u64>,
}

/// One process instance's peak observation plus the insertion order that
/// makes overflow eviction deterministic. `HashMap` iteration order is
/// unspecified (and randomized per process), so evicting `keys().next()`
/// dropped an arbitrary identity — including the one the caller was about
/// to read back. Oldest-insertion eviction is reproducible and drops the
/// least recently *registered* identity instead.
#[derive(Debug, Clone, Copy)]
struct PeakRecord {
    peak: usize,
    inserted_seq: u64,
}

pub struct ElementCache {
    core: ElementCacheCore<CacheKey, CachedSnapshot>,
    /// Highest actionable-element count ever cached for a process instance,
    /// across all of its windows. A modal can temporarily collapse every
    /// window's AT-SPI tree, so this history must outlive individual snapshots.
    peak_elements: Mutex<HashMap<ProcessIdentity, PeakRecord>>,
    /// Monotonic insertion counter for [`PeakRecord::inserted_seq`].
    peak_seq: Mutex<u64>,
    process_instance_id: Arc<dyn Fn(u32) -> Option<u64> + Send + Sync>,
}

impl ElementCache {
    pub fn new() -> Self {
        Self::with_process_instance_id(crate::proc_fs::process_instance_id)
    }

    fn with_process_instance_id(
        process_instance_id: impl Fn(u32) -> Option<u64> + Send + Sync + 'static,
    ) -> Self {
        Self {
            core: ElementCacheCore::new(),
            peak_elements: Mutex::new(Default::default()),
            peak_seq: Mutex::new(0),
            process_instance_id: Arc::new(process_instance_id),
        }
    }

    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        (self.process_instance_id)(pid).map(|start_time| ProcessIdentity { pid, start_time })
    }

    pub fn update(&self, pid: u32, xid: u64, nodes: &[AtspiNode]) {
        let elements: Vec<u64> = nodes
            .iter()
            .filter(|n| n.element_index.is_some())
            .map(|n| n.element_key)
            .collect();
        let count = elements.len();
        self.core
            .insert(CacheKey { pid, xid }, CachedSnapshot { elements });
        if let Some(identity) = self.process_identity(pid) {
            let mut peaks = self.peak_elements.lock().unwrap();
            peaks.retain(|key, _| (self.process_instance_id)(key.pid) == Some(key.start_time));
            if !peaks.contains_key(&identity) && peaks.len() >= MAX_PEAK_IDENTITIES {
                // Deterministic overflow policy: drop the least recently
                // observed identity. Every `update` refreshes the winner's
                // `inserted_seq` below, so an actively driven app is never
                // evicted ahead of a stale one.
                if let Some(evicted) = peaks
                    .iter()
                    .min_by_key(|(_, record)| record.inserted_seq)
                    .map(|(key, _)| *key)
                {
                    peaks.remove(&evicted);
                }
            }
            let seq = {
                let mut counter = self.peak_seq.lock().unwrap();
                *counter += 1;
                *counter
            };
            let record = peaks.entry(identity).or_insert(PeakRecord {
                peak: 0,
                inserted_seq: seq,
            });
            record.peak = record.peak.max(count);
            record.inserted_seq = seq;
        }
    }

    /// Most actionable elements ever observed in one snapshot for `pid`.
    pub fn peak_element_count(&self, pid: u32) -> usize {
        let Some(identity) = self.process_identity(pid) else {
            return 0;
        };
        self.peak_elements
            .lock()
            .unwrap()
            .get(&identity)
            .map(|record| record.peak)
            .unwrap_or(0)
    }

    pub fn get_element_key(&self, pid: u32, xid: u64, idx: usize) -> Option<u64> {
        self.core
            .with_snapshot(&CacheKey { pid, xid }, |s| s.elements.get(idx).copied())
            .flatten()
    }

    pub fn element_count(&self, pid: u32, xid: u64) -> usize {
        self.core
            .with_snapshot(&CacheKey { pid, xid }, |s| s.elements.len())
            .unwrap_or(0)
    }
}

impl Default for ElementCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn node(index: Option<usize>, key: u64) -> AtspiNode {
        AtspiNode {
            element_index: index,
            role: "test".into(),
            name: None,
            value: None,
            checked: None,
            enabled: None,
            selected: None,
            description: None,
            actions: Vec::new(),
            element_key: key,
            depth: 0,
            parent_element_index: None,
            in_web_content: false,
        }
    }

    #[test]
    fn peak_element_count_survives_collapse_and_spans_windows() {
        let cache = ElementCache::with_process_instance_id(|_| Some(1));
        let populated: Vec<_> = (0..5).map(|i| node(Some(i), i as u64)).collect();

        cache.update(7, 100, &populated);
        cache.update(7, 100, &[node(None, 0)]);

        assert_eq!(cache.element_count(7, 100), 0);
        assert_eq!(cache.element_count(7, 200), 0);
        assert_eq!(cache.peak_element_count(7), 5);
        assert_eq!(cache.peak_element_count(8), 0);
    }

    #[test]
    fn peak_history_does_not_survive_pid_reuse() {
        let start_time = Arc::new(AtomicU64::new(10));
        let resolver = start_time.clone();
        let cache =
            ElementCache::with_process_instance_id(move |_| Some(resolver.load(Ordering::Relaxed)));
        let populated: Vec<_> = (0..5).map(|i| node(Some(i), i as u64)).collect();

        cache.update(7, 100, &populated);
        assert_eq!(cache.peak_element_count(7), 5);

        start_time.store(20, Ordering::Relaxed);
        assert_eq!(cache.peak_element_count(7), 0);
        cache.update(7, 200, &[node(Some(0), 1)]);

        assert_eq!(cache.peak_element_count(7), 1);
        assert_eq!(cache.peak_elements.lock().unwrap().len(), 1);
    }

    #[test]
    fn peak_history_is_bounded() {
        let cache = ElementCache::with_process_instance_id(|pid| Some(pid as u64));

        for pid in 1..=(MAX_PEAK_IDENTITIES as u32 + 1) {
            cache.update(pid, 100, &[node(Some(0), pid as u64)]);
        }

        assert_eq!(
            cache.peak_elements.lock().unwrap().len(),
            MAX_PEAK_IDENTITIES
        );
    }

    #[test]
    fn peak_history_evicts_the_least_recently_observed_identity() {
        let cache = ElementCache::with_process_instance_id(|pid| Some(pid as u64));

        for pid in 1..=(MAX_PEAK_IDENTITIES as u32) {
            cache.update(pid, 100, &[node(Some(0), pid as u64)]);
        }
        // Re-observe pid 1 so it is no longer the oldest entry; pid 2 is.
        cache.update(1, 100, &[node(Some(0), 1)]);
        cache.update(MAX_PEAK_IDENTITIES as u32 + 1, 100, &[node(Some(0), 1)]);

        let peaks = cache.peak_elements.lock().unwrap();
        assert_eq!(peaks.len(), MAX_PEAK_IDENTITIES);
        assert!(peaks.contains_key(&ProcessIdentity {
            pid: 1,
            start_time: 1
        }));
        assert!(
            !peaks.contains_key(&ProcessIdentity {
                pid: 2,
                start_time: 2
            }),
            "the least recently observed identity must be the evicted one"
        );
    }
}
