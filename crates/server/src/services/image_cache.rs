//! Disk + memory cache for emote images.
//!
//! Emote images never change for a given id, so entries don't expire; when
//! the disk cache is over its size limit, the least recently used files are
//! deleted. Last-use times live in an index file, because Windows doesn't
//! reliably update file access times.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use axum::body::Bytes;
use serde::{Deserialize, Serialize};

const INDEX_FILE: &str = "index.json";

#[derive(Clone)]
pub struct CachedImage {
    pub content_type: String,
    pub bytes: Bytes,
}

#[derive(Clone, Serialize, Deserialize)]
struct IndexEntry {
    content_type: String,
    size: u64,
    last_used_ms: u64,
}

pub struct ImageCache {
    dir: PathBuf,
    max_disk_bytes: u64,
    max_memory_bytes: u64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    index: HashMap<String, IndexEntry>,
    disk_bytes: u64,
    index_dirty: bool,
    memory: HashMap<String, (CachedImage, u64)>,
    memory_bytes: u64,
}

impl ImageCache {
    /// Opens (or creates) the cache directory and loads its index. Index
    /// entries whose files are gone are dropped; files not in the index are
    /// deleted.
    pub fn open(dir: PathBuf, max_disk_bytes: u64, max_memory_bytes: u64) -> anyhow::Result<ImageCache> {
        std::fs::create_dir_all(&dir)?;
        let mut index: HashMap<String, IndexEntry> = std::fs::read(dir.join(INDEX_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        index.retain(|key, _| dir.join(file_name(key)).is_file());
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let known = name.strip_suffix(".bin").is_some_and(|key| index.contains_key(key));
            if name != INDEX_FILE && !known {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let disk_bytes = index.values().map(|entry| entry.size).sum();
        let cache = ImageCache {
            dir,
            max_disk_bytes,
            max_memory_bytes,
            state: Mutex::new(State { index, disk_bytes, index_dirty: true, ..State::default() }),
        };
        cache.evict_disk(&mut cache.state.lock().unwrap(), None);
        cache.save_index();
        Ok(cache)
    }

    pub fn get(&self, key: &str, now_ms: u64) -> Option<CachedImage> {
        let mut state = self.state.lock().unwrap();
        if let Some((image, last_used)) = state.memory.get_mut(key) {
            *last_used = now_ms;
            let image = image.clone();
            if let Some(entry) = state.index.get_mut(key) {
                entry.last_used_ms = now_ms;
                state.index_dirty = true;
            }
            return Some(image);
        }
        let entry = state.index.get_mut(key)?;
        entry.last_used_ms = now_ms;
        let content_type = entry.content_type.clone();
        state.index_dirty = true;
        match std::fs::read(self.dir.join(file_name(key))) {
            Ok(bytes) => {
                let image = CachedImage { content_type, bytes: Bytes::from(bytes) };
                self.remember(&mut state, key, image.clone(), now_ms);
                Some(image)
            }
            Err(error) => {
                tracing::warn!(key, %error, "cached emote image is unreadable; dropping it");
                if let Some(entry) = state.index.remove(key) {
                    state.disk_bytes -= entry.size;
                }
                None
            }
        }
    }

    pub fn put(&self, key: &str, image: CachedImage, now_ms: u64) {
        let path = self.dir.join(file_name(key));
        let temp = self.dir.join(format!("{}.tmp", file_name(key)));
        let written = std::fs::write(&temp, &image.bytes).and_then(|()| std::fs::rename(&temp, &path));
        let mut state = self.state.lock().unwrap();
        match written {
            Ok(()) => {
                let size = image.bytes.len() as u64;
                if let Some(old) = state.index.insert(
                    key.to_string(),
                    IndexEntry { content_type: image.content_type.clone(), size, last_used_ms: now_ms },
                ) {
                    state.disk_bytes -= old.size;
                }
                state.disk_bytes += size;
                state.index_dirty = true;
                self.evict_disk(&mut state, Some(key));
            }
            Err(error) => {
                tracing::warn!(key, %error, "couldn't write emote image to the disk cache");
                let _ = std::fs::remove_file(&temp);
            }
        }
        self.remember(&mut state, key, image, now_ms);
        drop(state);
        self.save_index();
    }

    fn remember(&self, state: &mut State, key: &str, image: CachedImage, now_ms: u64) {
        let size = image.bytes.len() as u64;
        if size > self.max_memory_bytes {
            return;
        }
        if let Some((old, _)) = state.memory.insert(key.to_string(), (image, now_ms)) {
            state.memory_bytes -= old.bytes.len() as u64;
        }
        state.memory_bytes += size;
        while state.memory_bytes > self.max_memory_bytes {
            let oldest = state
                .memory
                .iter()
                .filter(|(other, _)| other.as_str() != key)
                .min_by_key(|(_, (_, last_used))| *last_used)
                .map(|(other, _)| other.clone());
            let Some(oldest) = oldest else { break };
            if let Some((removed, _)) = state.memory.remove(&oldest) {
                state.memory_bytes -= removed.bytes.len() as u64;
            }
        }
    }

    /// Deletes the least recently used files until the cache fits, never
    /// the one just written.
    fn evict_disk(&self, state: &mut State, keep: Option<&str>) {
        while state.disk_bytes > self.max_disk_bytes {
            let oldest = state
                .index
                .iter()
                .filter(|(key, _)| Some(key.as_str()) != keep)
                .min_by_key(|(_, entry)| entry.last_used_ms)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            if let Some(entry) = state.index.remove(&oldest) {
                state.disk_bytes -= entry.size;
                let _ = std::fs::remove_file(self.dir.join(file_name(&oldest)));
                state.index_dirty = true;
            }
            // Keep memory a subset of disk, so the size limit means what it says.
            if let Some((removed, _)) = state.memory.remove(&oldest) {
                state.memory_bytes -= removed.bytes.len() as u64;
            }
        }
    }

    /// Writes the index if it changed (atomically, via a temp file).
    pub fn save_index(&self) {
        let json = {
            let mut state = self.state.lock().unwrap();
            if !state.index_dirty {
                return;
            }
            state.index_dirty = false;
            serde_json::to_vec(&state.index).expect("index serialises")
        };
        let temp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        if let Err(error) =
            std::fs::write(&temp, json).and_then(|()| std::fs::rename(&temp, self.dir.join(INDEX_FILE)))
        {
            tracing::warn!(%error, "couldn't save the emote image cache index");
        }
    }

    #[cfg(test)]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

/// Cache key for an emote image (source + id + variant), safe as a file name.
pub fn cache_key(source: &str, id: &str) -> String {
    format!("{}_{}_2x", escape(source), escape(id))
}

fn escape(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for byte in part.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("~{byte:02x}"));
        }
    }
    out
}

fn file_name(key: &str) -> String {
    format!("{key}.bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lsk_cache_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn image(size: usize) -> CachedImage {
        CachedImage { content_type: "image/webp".into(), bytes: Bytes::from(vec![7u8; size]) }
    }

    #[test]
    fn keys_are_safe_file_names() {
        assert_eq!(cache_key("7tv", "01ABC"), "7tv_01ABC_2x");
        assert_eq!(cache_key("x", "a/b_c"), "x_a~2fb~5fc_2x");
    }

    #[test]
    fn evicts_least_recently_used_and_survives_reopen() {
        let dir = temp_dir("evict");
        {
            let cache = ImageCache::open(dir.clone(), 250, 1_000).unwrap();
            cache.put("a", image(100), 1);
            cache.put("b", image(100), 2);
            assert!(cache.get("a", 3).is_some()); // a is now more recent than b
            cache.put("c", image(100), 4); // over 250 → evict b
            assert!(cache.get("b", 5).is_none());
        }
        // A fresh cache (empty memory) still has a and c on disk.
        let cache = ImageCache::open(dir.clone(), 250, 1_000).unwrap();
        let a = cache.get("a", 6).unwrap();
        assert_eq!(a.bytes.len(), 100);
        assert_eq!(a.content_type, "image/webp");
        assert!(cache.get("c", 7).is_some());
        assert!(cache.get("b", 8).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn removes_files_missing_from_the_index() {
        let dir = temp_dir("orphans");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stray_2x.bin"), b"x").unwrap();
        let cache = ImageCache::open(dir.clone(), 1_000, 1_000).unwrap();
        assert!(!cache.dir().join("stray_2x.bin").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
