use std::num::NonZeroUsize;

use lru::LruCache;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::ChessError;
use crate::fen::normalize_fen;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScoreKind {
    Cp,
    Mate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Score {
    #[serde(rename = "type")]
    pub kind: ScoreKind,
    pub cp: Option<i32>,
    pub mate_in: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalysisLine {
    pub rank: u32,
    pub score: Score,
    pub pv_uci: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalysisResult {
    pub schema_version: String,
    pub fen: String,
    pub depth: u32,
    pub multipv: u32,
    pub lines: Vec<AnalysisLine>,
}

pub const SCHEMA_VERSION: &str = "1.0";

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub struct CacheKey {
    pub normalized_fen: String,
    pub depth: u32,
    pub multipv: u32,
}

impl CacheKey {
    pub fn new(fen: &str, depth: u32, multipv: u32) -> Result<Self, ChessError> {
        Ok(Self {
            normalized_fen: normalize_fen(fen)?,
            depth,
            multipv,
        })
    }
}

pub struct AnalysisCache {
    inner: Mutex<LruCache<CacheKey, AnalysisResult>>,
    capacity: usize,
}

impl AnalysisCache {
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("capacity clamped above");
        Self {
            inner: Mutex::new(LruCache::new(cap)),
            capacity: cap.get(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    pub fn get(&self, key: &CacheKey) -> Option<AnalysisResult> {
        self.inner.lock().get(key).cloned()
    }

    pub fn put(&self, key: CacheKey, value: AnalysisResult) {
        self.inner.lock().put(key, value);
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_result(fen: &str, depth: u32, multipv: u32) -> AnalysisResult {
        AnalysisResult {
            schema_version: SCHEMA_VERSION.to_string(),
            fen: fen.to_string(),
            depth,
            multipv,
            lines: vec![AnalysisLine {
                rank: 1,
                score: Score {
                    kind: ScoreKind::Cp,
                    cp: Some(31),
                    mate_in: None,
                },
                pv_uci: vec!["e2e4".to_string()],
            }],
        }
    }

    #[test]
    fn put_then_get_returns_same_entry() {
        let cache = AnalysisCache::new(4);
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let key = CacheKey::new(fen, 20, 6).unwrap();
        let value = sample_result(fen, 20, 6);
        cache.put(key.clone(), value.clone());
        assert_eq!(cache.get(&key), Some(value));
    }

    #[test]
    fn miss_returns_none() {
        let cache = AnalysisCache::new(4);
        let key = CacheKey::new(
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            20,
            6,
        )
        .unwrap();
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn fen_clocks_dont_affect_key() {
        let a = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let b = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 9 18";
        let ka = CacheKey::new(a, 20, 6).unwrap();
        let kb = CacheKey::new(b, 20, 6).unwrap();
        assert_eq!(ka, kb);
    }

    #[test]
    fn depth_and_multipv_part_of_key() {
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let k1 = CacheKey::new(fen, 20, 6).unwrap();
        let k2 = CacheKey::new(fen, 21, 6).unwrap();
        let k3 = CacheKey::new(fen, 20, 3).unwrap();
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn lru_eviction_drops_least_recently_used() {
        let cache = AnalysisCache::new(2);
        let fens = [
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR b KQkq - 0 1",
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQ - 0 1",
        ];
        let keys: Vec<CacheKey> = fens
            .iter()
            .map(|f| CacheKey::new(f, 20, 6).unwrap())
            .collect();
        cache.put(keys[0].clone(), sample_result(fens[0], 20, 6));
        cache.put(keys[1].clone(), sample_result(fens[1], 20, 6));
        // Touch keys[0] so it becomes MRU
        let _ = cache.get(&keys[0]);
        cache.put(keys[2].clone(), sample_result(fens[2], 20, 6));
        assert!(cache.get(&keys[0]).is_some());
        assert!(cache.get(&keys[1]).is_none()); // evicted
        assert!(cache.get(&keys[2]).is_some());
    }

    #[test]
    fn invalid_fen_rejected_in_key_construction() {
        let err = CacheKey::new("garbage", 20, 6).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::InvalidFen);
    }

    #[test]
    fn zero_capacity_clamped_to_one() {
        let cache = AnalysisCache::new(0);
        assert_eq!(cache.capacity(), 1);
    }

    #[test]
    fn clear_empties_cache() {
        let cache = AnalysisCache::new(4);
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
        let key = CacheKey::new(fen, 20, 6).unwrap();
        cache.put(key, sample_result(fen, 20, 6));
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert!(cache.is_empty());
    }
}
