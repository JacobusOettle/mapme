//! The store's bookkeeping: decoded tiles under a byte budget (least recently used dropped),
//! missing marks (oldest dropped past `MAX_MISSING_MARKS`), behind one mutex held only for
//! lookups and inserts. Queries copy `Arc`s of the tiles they use under the lock and compute
//! without it, so a tile evicted meanwhile stays alive until the query is done.

use super::{MAX_MISSING_MARKS, TILE_BYTES};
use crate::dem::TileId;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

/// A tile as a query finds it.
pub enum TileState {
    Present(Arc<Vec<f32>>),
    Missing,
    Unknown,
}

struct Held {
    data: Arc<Vec<f32>>,
    stamp: u64,
}

#[derive(Default)]
struct Inner {
    held: HashMap<TileId, Held>,
    /// Use stamp → tile, oldest first.
    lru: BTreeMap<u64, TileId>,
    /// Missing tile → the stamp it was marked at.
    missing: HashMap<TileId, u64>,
    missing_order: BTreeMap<u64, TileId>,
    clock: u64,
    bytes: u64,
    evicted: u64,
}

impl Inner {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn unmark(&mut self, t: TileId) {
        if let Some(s) = self.missing.remove(&t) {
            self.missing_order.remove(&s);
        }
    }
}

/// Counters for `mm_terrain_stats`.
pub struct Counts {
    pub tiles: usize,
    pub missing: usize,
    pub bytes: u64,
    pub evicted: u64,
}

pub struct Tiles {
    max_bytes: u64,
    inner: Mutex<Inner>,
}

impl Tiles {
    pub fn new(max_bytes: u64) -> Tiles {
        Tiles { max_bytes, inner: Mutex::new(Inner::default()) }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves consistent maps (every update is a few
        // insert/remove pairs on plain collections): keep serving.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hold a tile's elevations (`TILE_SIZE²` values), dropping the least recently used past the
    /// budget. Clears its missing mark.
    pub fn insert(&self, t: TileId, data: Vec<f32>) {
        let data = Arc::new(data);
        let mut g = self.lock();
        g.unmark(t);
        let stamp = g.tick();
        if let Some(old) = g.held.insert(t, Held { data, stamp }) {
            g.lru.remove(&old.stamp);
            g.bytes -= TILE_BYTES;
        }
        g.lru.insert(stamp, t);
        g.bytes += TILE_BYTES;
        while g.bytes > self.max_bytes {
            let Some((_, oldest)) = g.lru.pop_first() else { break };
            if g.held.remove(&oldest).is_some() {
                g.bytes -= TILE_BYTES;
                g.evicted += 1;
            }
        }
    }

    /// Mark a tile missing (a tile held stays held: it is better than none).
    pub fn mark_missing(&self, t: TileId) {
        let mut g = self.lock();
        if g.held.contains_key(&t) || g.missing.contains_key(&t) {
            return;
        }
        let stamp = g.tick();
        g.missing.insert(t, stamp);
        g.missing_order.insert(stamp, t);
        while g.missing.len() > MAX_MISSING_MARKS {
            let Some((_, oldest)) = g.missing_order.pop_first() else { break };
            g.missing.remove(&oldest);
        }
    }

    pub fn clear_missing(&self) {
        let mut g = self.lock();
        g.missing.clear();
        g.missing_order.clear();
    }

    pub fn counts(&self) -> Counts {
        let g = self.lock();
        Counts { tiles: g.held.len(), missing: g.missing.len(), bytes: g.bytes, evicted: g.evicted }
    }

    /// Look tiles up under one lock (`Snapshot::lookup`).
    pub fn snapshot(&self) -> Snapshot<'_> {
        Snapshot { inner: self.lock() }
    }
}

/// The store locked for a query's lookups. Drop it before computing.
pub struct Snapshot<'a> {
    inner: MutexGuard<'a, Inner>,
}

impl Snapshot<'_> {
    /// A tile's state; a held tile counts as just used.
    pub fn lookup(&mut self, t: TileId) -> TileState {
        let g = &mut *self.inner;
        if g.held.contains_key(&t) {
            g.clock += 1;
            let stamp = g.clock;
            let h = g.held.get_mut(&t).expect("held");
            let old = std::mem::replace(&mut h.stamp, stamp);
            let data = h.data.clone();
            g.lru.remove(&old);
            g.lru.insert(stamp, t);
            return TileState::Present(data);
        }
        if g.missing.contains_key(&t) {
            TileState::Missing
        } else {
            TileState::Unknown
        }
    }
}
