//! AT-SPI element cache for Linux.
//! Stores element keys (u64 hash) indexed by (pid, xid) → element_index.
//!
//! The locked-HashMap plumbing lives in `cua_driver_core::element_cache` — see
//! `docs/dedup-audit.md` item #3. This module owns the Linux-specific
//! `CacheKey` and `CachedSnapshot` (no Drop needed — `Vec<u64>` frees
//! itself).

use super::AtspiNode;
use cua_driver_core::element_cache::ElementCacheCore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey { pub pid: u32, pub xid: u64 }

pub struct CachedSnapshot {
    /// element_index → element_key (opaque AT-SPI path hash).
    pub elements: Vec<u64>,
}

pub struct ElementCache {
    core: ElementCacheCore<CacheKey, CachedSnapshot>,
    /// Highest indexed-element count ever cached for a pid, across all of
    /// its windows. Lets warning paths distinguish "this app never
    /// registered on the AT-SPI bus" (launch-env problem; relaunch helps)
    /// from "it WAS registered and its whole tree vanished" — wx modal
    /// dialogs drop the entire app off the a11y bus, and relaunching then
    /// destroys in-memory state for nothing (#17).
    peak_elements: std::sync::Mutex<std::collections::HashMap<u32, usize>>,
}

impl ElementCache {
    pub fn new() -> Self {
        Self {
            core: ElementCacheCore::new(),
            peak_elements: std::sync::Mutex::new(Default::default()),
        }
    }

    pub fn update(&self, pid: u32, xid: u64, nodes: &[AtspiNode]) {
        let elements: Vec<u64> = nodes.iter()
            .filter(|n| n.element_index.is_some())
            .map(|n| n.element_key)
            .collect();
        let count = elements.len();
        self.core.insert(CacheKey { pid, xid }, CachedSnapshot { elements });
        let mut peaks = self.peak_elements.lock().unwrap();
        let peak = peaks.entry(pid).or_insert(0);
        if count > *peak { *peak = count; }
    }

    /// Most elements ever observed in one snapshot for any window of `pid`
    /// (0 when the pid has never produced a populated tree).
    pub fn peak_element_count(&self, pid: u32) -> usize {
        self.peak_elements.lock().unwrap().get(&pid).copied().unwrap_or(0)
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

impl Default for ElementCache { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;

    fn node(idx: Option<usize>, key: u64) -> AtspiNode {
        AtspiNode {
            element_index: idx,
            role: "test".into(),
            name: None,
            value: None,
            description: None,
            actions: Vec::new(),
            element_key: key,
        }
    }

    /// #17: the peak survives a tree collapse (wx modal up) and is shared
    /// across the pid's windows, so a never-snapshotted dialog still
    /// inherits the "this app WAS populated" signal.
    #[test]
    fn peak_element_count_survives_collapse_and_spans_windows() {
        let cache = ElementCache::new();
        assert_eq!(cache.peak_element_count(7), 0);

        let rich: Vec<AtspiNode> = (0..5).map(|i| node(Some(i), i as u64)).collect();
        cache.update(7, 100, &rich);
        assert_eq!(cache.peak_element_count(7), 5);
        assert_eq!(cache.element_count(7, 100), 5);

        // Modal up: the main window re-snapshots to a bare window node.
        cache.update(7, 100, &[node(None, 0)]);
        assert_eq!(cache.element_count(7, 100), 0, "live snapshot reflects the collapse");
        assert_eq!(cache.peak_element_count(7), 5, "peak must remember the populated tree");

        // The dialog window was never snapshotted; per-pid peak still applies.
        assert_eq!(cache.element_count(7, 200), 0);
        assert_eq!(cache.peak_element_count(7), 5);

        // Other pids are unaffected.
        assert_eq!(cache.peak_element_count(8), 0);
    }
}
