//! Page memory: a small value each plugin's overlay page keeps with the app
//! (`{ "type": "keep", "value": … }`), given back when the page loads
//! (`{ "type": "kept", "value": … }`), so it carries on after its Browser
//! Source reloads or the app restarts. Shared by every overlay showing
//! the page: the last one kept wins. Saved as `<id>.page.json` in the
//! plugins' data folder, a moment after it changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use protocol::plugin::KEEP_LIMIT;
use serde_json::Value;

/// How long a change waits before it's saved, so a page keeping something
/// on every chat message doesn't write a file each time.
const SAVE_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct PageMemory {
    inner: Arc<Inner>,
}

struct Inner {
    /// `None` in tests: nothing is read or written.
    dir: Option<PathBuf>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// What's kept, by plugin; `Null` = read, and nothing there.
    values: BTreeMap<String, Value>,
    /// Changed and not saved yet.
    unsaved: BTreeSet<String>,
    saving: bool,
}

impl PageMemory {
    /// Kept in `dir` (the plugins' data folder).
    pub fn open(dir: PathBuf) -> PageMemory {
        PageMemory::new(Some(dir))
    }

    #[cfg(test)]
    pub fn for_tests() -> PageMemory {
        PageMemory::new(None)
    }

    fn new(dir: Option<PathBuf>) -> PageMemory {
        PageMemory { inner: Arc::new(Inner { dir, state: Mutex::default() }) }
    }

    /// What `plugin`'s page kept, if anything.
    pub fn get(&self, plugin: &str) -> Option<Value> {
        let mut state = self.inner.state.lock().unwrap();
        let value = state.values.entry(plugin.to_string()).or_insert_with(|| self.read(plugin));
        (!value.is_null()).then(|| value.clone())
    }

    /// Keeps `value` for `plugin`'s page (`null` forgets it). One bigger
    /// than [`KEEP_LIMIT`] is refused.
    pub fn keep(&self, plugin: &str, value: Value) -> bool {
        let size = value.to_string().len();
        if size > KEEP_LIMIT {
            tracing::warn!(plugin, size, "a plugin page kept more than {KEEP_LIMIT} bytes; it wasn't kept");
            return false;
        }
        let start_saving = {
            let mut state = self.inner.state.lock().unwrap();
            if state.values.get(plugin) == Some(&value) {
                return true;
            }
            state.values.insert(plugin.to_string(), value);
            state.unsaved.insert(plugin.to_string());
            let start = !state.saving && self.inner.dir.is_some();
            state.saving |= start;
            start
        };
        if start_saving && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let memory = self.clone();
            runtime.spawn(async move {
                tokio::time::sleep(SAVE_AFTER).await;
                memory.save();
            });
        }
        true
    }

    fn path(&self, plugin: &str) -> Option<PathBuf> {
        Some(self.inner.dir.as_ref()?.join(format!("{plugin}.page.json")))
    }

    fn read(&self, plugin: &str) -> Value {
        let Some(path) = self.path(plugin) else { return Value::Null };
        let Ok(text) = std::fs::read_to_string(&path) else { return Value::Null };
        serde_json::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!("{} can't be read ({error}); the page starts afresh", path.display());
            Value::Null
        })
    }

    /// Writes what changed since the last save.
    fn save(&self) {
        let unsaved: Vec<(String, Value)> = {
            let mut state = self.inner.state.lock().unwrap();
            state.saving = false;
            let unsaved = std::mem::take(&mut state.unsaved);
            unsaved.into_iter().map(|plugin| (plugin.clone(), state.values[&plugin].clone())).collect()
        };
        for (plugin, value) in unsaved {
            let Some(path) = self.path(&plugin) else { continue };
            let written = if value.is_null() {
                match std::fs::remove_file(&path) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                    _ => Ok(()),
                }
            } else {
                path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
                    let temporary = path.with_extension("json.tmp");
                    std::fs::write(&temporary, value.to_string())?;
                    std::fs::rename(&temporary, &path)
                })
            };
            if let Err(error) = written {
                tracing::warn!("couldn't save {}: {error}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_gets_back_what_it_kept() {
        let memory = PageMemory::for_tests();
        assert_eq!(memory.get("chat"), None);
        assert!(memory.keep("chat", serde_json::json!({ "lines": [1, 2] })));
        assert_eq!(memory.get("chat"), Some(serde_json::json!({ "lines": [1, 2] })));
        assert_eq!(memory.get("other"), None, "a page's own");
        assert!(memory.keep("chat", Value::Null));
        assert_eq!(memory.get("chat"), None, "forgotten");
        assert!(!memory.keep("chat", Value::String("x".repeat(KEEP_LIMIT))), "too big");
        assert_eq!(memory.get("chat"), None);
    }

    #[tokio::test]
    async fn it_is_saved_and_read_back() {
        let dir = std::env::temp_dir().join(format!("lsk-page-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let memory = PageMemory::open(dir.clone());
        memory.keep("chat", serde_json::json!([1]));
        memory.save();
        assert_eq!(PageMemory::open(dir.clone()).get("chat"), Some(serde_json::json!([1])));
        memory.keep("chat", Value::Null);
        memory.save();
        assert!(!dir.join("chat.page.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
