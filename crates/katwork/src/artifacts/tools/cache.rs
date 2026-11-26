//! `read_file`'s extraction cache: extracted blocks, and the failures a rerun must not repeat.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::time::Instant;

use crate::{
    docs::{DocError, Extracted, MediaKind},
    ids::WorkspaceId,
};

/// How long a failure is remembered.
pub const FAILED_TTL: Duration = Duration::from_secs(600);
/// At most this many failures are remembered, oldest dropped first: they weigh nothing in
/// bytes, so a stream of distinct bad uploads is bounded by count.
pub const MAX_FAILED: usize = 4096;
/// The bytes a block costs beyond its strings.
const BLOCK_OVERHEAD: u64 = 96;

/// `(workspace, sha256, kind)`. Within one process a kind has exactly one extractor, so this
/// is the plan's `(workspace, sha256, extractor name, extractor version)`. The workspace is in
/// the key, so a fast hit never tells one tenant that another holds the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub workspace_id: WorkspaceId,
    pub sha256: String,
    pub kind: MediaKind,
}

#[derive(Debug, Clone)]
pub enum Cached {
    Blocks(Arc<Extracted>),
    Failed(DocError),
}

enum Entry {
    Blocks {
        value: Arc<Extracted>,
        weight: u64,
        text: u64,
    },
    Failed {
        error: DocError,
        at: Instant,
    },
}

#[derive(Default)]
struct Inner {
    map: HashMap<CacheKey, (Entry, u64)>,
    tick: u64,
    weight: u64,
    text: u64,
    failed: usize,
}

/// An LRU of extractions bounded at `cap` bytes (text, line boxes and cell facts, plus a fixed
/// cost per block), and of up to `MAX_FAILED` failures, each kept `FAILED_TTL`.
pub struct ExtractCache {
    cap: u64,
    inner: Mutex<Inner>,
}

/// The bytes an extraction holds in the cache.
pub fn weight(extracted: &Extracted) -> u64 {
    let s = |s: &Option<String>| s.as_ref().map_or(0, |s| s.len() as u64);
    extracted
        .blocks
        .iter()
        .map(|b| {
            let cell = b.cell.as_ref().map_or(0, |c| {
                s(&c.cached)
                    + s(&c.displayed)
                    + s(&c.formula)
                    + s(&c.number_format)
                    + s(&c.row_header)
                    + s(&c.col_header)
                    + s(&c.header_unit)
            });
            BLOCK_OVERHEAD
                + b.text.len() as u64
                + b.lines.len() as u64 * 32
                + cell
                + s(&b.unit_context)
        })
        .sum::<u64>()
        + extracted.warnings.len() as u64 * BLOCK_OVERHEAD
        + extracted.pages.len() as u64 * 48
}

fn text_bytes(extracted: &Extracted) -> u64 {
    extracted.blocks.iter().map(|b| b.text.len() as u64).sum()
}

impl ExtractCache {
    pub fn new(cap_bytes: u64) -> Self {
        Self {
            cap: cap_bytes,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self, key: &CacheKey, now: Instant) -> Option<Cached> {
        let mut inner = self.lock();
        inner.tick += 1;
        let tick = inner.tick;
        let expired = match inner.map.get(key) {
            None => return None,
            Some((Entry::Failed { at, .. }, _)) => now.saturating_duration_since(*at) >= FAILED_TTL,
            Some(_) => false,
        };
        if expired {
            inner.remove(key);
            return None;
        }
        let (entry, used) = inner.map.get_mut(key)?;
        *used = tick;
        Some(match entry {
            Entry::Blocks { value, .. } => Cached::Blocks(value.clone()),
            Entry::Failed { error, .. } => Cached::Failed(*error),
        })
    }

    /// Stores an extraction; false when it alone weighs more than the cache holds.
    pub fn put_blocks(&self, key: CacheKey, value: Arc<Extracted>) -> bool {
        let weight = weight(&value);
        if weight > self.cap {
            return false;
        }
        let text = text_bytes(&value);
        let mut inner = self.lock();
        inner.remove(&key);
        while inner.weight + weight > self.cap {
            let Some(oldest) = inner.oldest(|e| matches!(e, Entry::Blocks { .. })) else {
                break;
            };
            inner.remove(&oldest);
        }
        inner.tick += 1;
        let tick = inner.tick;
        inner.weight += weight;
        inner.text += text;
        inner.map.insert(
            key,
            (
                Entry::Blocks {
                    value,
                    weight,
                    text,
                },
                tick,
            ),
        );
        true
    }

    /// Remembers a failure a rerun must not repeat: only `TooComplex` and the pool's own
    /// `CallTimeout`. Any other error is refused (false), so a caller's `Timeout` is never
    /// cached.
    pub fn put_failed(&self, key: CacheKey, error: DocError, now: Instant) -> bool {
        if !matches!(error, DocError::TooComplex(_) | DocError::CallTimeout) {
            return false;
        }
        let mut inner = self.lock();
        inner.remove(&key);
        while inner.failed >= MAX_FAILED {
            let Some(oldest) = inner.oldest(|e| matches!(e, Entry::Failed { .. })) else {
                break;
            };
            inner.remove(&oldest);
        }
        inner.tick += 1;
        let tick = inner.tick;
        inner.failed += 1;
        inner
            .map
            .insert(key, (Entry::Failed { error, at: now }, tick));
        true
    }

    /// Forgets every entry of a blob (Task 30's purge).
    pub fn evict_blob(&self, workspace_id: WorkspaceId, sha256: &str) {
        let mut inner = self.lock();
        let keys: Vec<CacheKey> = inner
            .map
            .keys()
            .filter(|k| k.workspace_id == workspace_id && k.sha256 == sha256)
            .cloned()
            .collect();
        for key in keys {
            inner.remove(&key);
        }
    }

    /// The block text held, in bytes.
    pub fn text_bytes(&self) -> u64 {
        self.lock().text
    }

    /// The bytes held, as `put_blocks` counts them; never above the cap.
    pub fn weight(&self) -> u64 {
        self.lock().weight
    }

    pub fn failed_entries(&self) -> usize {
        self.lock().failed
    }
}

impl Inner {
    fn remove(&mut self, key: &CacheKey) {
        match self.map.remove(key) {
            Some((Entry::Blocks { weight, text, .. }, _)) => {
                self.weight -= weight;
                self.text -= text;
            }
            Some((Entry::Failed { .. }, _)) => self.failed -= 1,
            None => {}
        }
    }

    fn oldest(&self, kind: impl Fn(&Entry) -> bool) -> Option<CacheKey> {
        self.map
            .iter()
            .filter(|(_, (entry, _))| kind(entry))
            .min_by_key(|(_, (_, used))| *used)
            .map(|(key, _)| key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::{Block, ExtractorId, Locator};

    fn key(n: u32) -> CacheKey {
        CacheKey {
            workspace_id: WorkspaceId(uuid::Uuid::from_u128(1)),
            sha256: format!("{n:064x}"),
            kind: MediaKind::Text,
        }
    }

    fn extracted(text_len: usize) -> Arc<Extracted> {
        Arc::new(Extracted {
            extractor: ExtractorId::TEXT,
            blocks: vec![Block {
                ordinal: 1,
                piece: 0,
                text: "x".repeat(text_len),
                locator: Locator::Text { start: 0, end: 0 },
                lines: vec![],
                cell: None,
                unit_context: None,
                hidden: None,
                suspect: None,
            }],
            pages: vec![],
            warnings: vec![],
            workbook: None,
        })
    }

    #[test]
    fn negative_entries_expire_after_ten_minutes_and_are_bounded() {
        let cache = ExtractCache::new(1 << 20);
        let t0 = Instant::now();
        assert!(cache.put_failed(key(1), DocError::TooComplex("zip"), t0));
        assert!(matches!(
            cache.get(&key(1), t0 + FAILED_TTL - Duration::from_secs(1)),
            Some(Cached::Failed(DocError::TooComplex("zip")))
        ));
        assert!(cache.get(&key(1), t0 + FAILED_TTL).is_none());
        for n in 0..(MAX_FAILED as u32 + 10) {
            assert!(cache.put_failed(key(n), DocError::CallTimeout, t0));
        }
        assert_eq!(cache.failed_entries(), MAX_FAILED);
        assert!(cache.get(&key(0), t0).is_none(), "the oldest went first");
        assert!(cache.get(&key(MAX_FAILED as u32 + 9), t0).is_some());
    }

    #[test]
    fn put_failed_refuses_timeout_malformed_and_toolarge() {
        let cache = ExtractCache::new(1 << 20);
        let now = Instant::now();
        for error in [
            DocError::Timeout,
            DocError::Malformed("x"),
            DocError::TooLarge("x"),
            DocError::Cancelled,
            DocError::Internal,
            DocError::Encrypted,
            DocError::Unsupported("x"),
        ] {
            assert!(!cache.put_failed(key(1), error, now), "{error:?}");
        }
        assert!(cache.get(&key(1), now).is_none());
    }

    #[test]
    fn blocks_are_evicted_oldest_first_within_the_cap() {
        let one = weight(&extracted(1000));
        let cache = ExtractCache::new(one * 2);
        assert!(cache.put_blocks(key(1), extracted(1000)));
        assert!(cache.put_blocks(key(2), extracted(1000)));
        let now = Instant::now();
        assert!(cache.get(&key(1), now).is_some());
        assert!(cache.put_blocks(key(3), extracted(1000)));
        assert!(cache.get(&key(2), now).is_none(), "least recently used");
        assert!(cache.get(&key(1), now).is_some());
        assert!(cache.weight() <= one * 2);
        assert!(
            !cache.put_blocks(key(4), extracted(10_000)),
            "too big alone"
        );
        cache.evict_blob(key(1).workspace_id, &key(1).sha256);
        assert!(cache.get(&key(1), now).is_none());
    }
}
