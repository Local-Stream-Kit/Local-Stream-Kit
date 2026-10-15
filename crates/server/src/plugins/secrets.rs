//! Plugins' secret settings (an obs-websocket password, an API key): kept
//! in `lsk_secrets.json` next to the config rather than in it, so the
//! config can be shown, shared and undone without them. Only server code
//! gets the values (with the plugin's other settings); panels only learn
//! which ones are saved, and set or forget them. A plugin in the plugins
//! folder that replaces a built-in one (`obs`) has its own, so the
//! built-in's saved password isn't handed to it. See
//! `docs/writing-a-plugin.md` → Settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use protocol::plugin::{PluginInfo, secret_settings};
use serde_json::{Map, Value};
use tokio::sync::watch;

use super::Plugins;

pub const SECRETS_FILE: &str = "lsk_secrets.json";

/// Values by plugin ([`slot`]) and key.
type Values = BTreeMap<String, BTreeMap<String, String>>;

/// Which secrets are saved: keys by plugin.
pub type Saved = BTreeMap<String, Vec<String>>;

#[derive(Clone)]
pub struct PluginSecrets {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    plugins: Arc<Plugins>,
    values: Mutex<Values>,
    saved: watch::Sender<Saved>,
}

impl PluginSecrets {
    /// Reads `lsk_secrets.json` in `dir` (none if it isn't there).
    pub fn open(dir: &Path, plugins: Arc<Plugins>) -> PluginSecrets {
        let path = dir.join(SECRETS_FILE);
        let values: Values = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!("{} can't be read ({error}); plugins' secrets are empty", path.display());
                Values::new()
            }),
            Err(_) => Values::new(),
        };
        let saved = watch::Sender::new(Saved::new());
        let secrets =
            PluginSecrets { inner: Arc::new(Inner { path, plugins, values: Mutex::new(values), saved }) };
        secrets.inner.saved.send_replace(secrets.saved());
        secrets
    }

    /// The plugins changed: tells subscribers if which secrets are saved
    /// changed with them.
    pub fn refresh(&self) {
        let saved = self.saved();
        self.inner.saved.send_if_modified(|old| {
            let changed = *old != saved;
            *old = saved;
            changed
        });
    }

    /// Which secrets are saved, for the plugins there are now (a plugin
    /// installed over a built-in one has none saved at first).
    pub fn saved(&self) -> Saved {
        let catalog = self.inner.plugins.catalog();
        let values = self.inner.values.lock().unwrap();
        catalog
            .plugins()
            .iter()
            .filter_map(|info| {
                let keys: Vec<String> = values.get(&slot(info))?.keys().cloned().collect();
                (!keys.is_empty()).then(|| (info.manifest.id.clone(), keys))
            })
            .collect()
    }

    pub fn subscribe(&self) -> watch::Receiver<Saved> {
        self.inner.saved.subscribe()
    }

    /// `plugin`'s secret settings, by key, to go with its settings for its
    /// server code: every one its manifest has, `""` while none is saved.
    pub fn values(&self, plugin: &str) -> Map<String, Value> {
        let catalog = self.inner.plugins.catalog();
        let Some(info) = catalog.plugin(plugin) else { return Map::new() };
        let values = self.inner.values.lock().unwrap();
        let saved = values.get(&slot(info));
        secret_settings(&info.manifest)
            .map(|key| {
                let value = saved.and_then(|saved| saved.get(key)).cloned().unwrap_or_default();
                (key.to_string(), Value::String(value))
            })
            .collect()
    }

    /// Saves one of a plugin's secret settings; `None` or empty forgets it.
    pub fn save(&self, plugin: &str, key: &str, value: Option<String>) -> Result<(), String> {
        let catalog = self.inner.plugins.catalog();
        let info = catalog.plugin(plugin).ok_or_else(|| format!("there's no {plugin} plugin"))?;
        if !secret_settings(&info.manifest).any(|secret| secret == key) {
            return Err(format!("the {plugin} plugin has no secret setting {key:?}"));
        }
        let slot = slot(info);
        let mut values = self.inner.values.lock().unwrap();
        let mut next = values.clone();
        match value.filter(|value| !value.is_empty()) {
            Some(value) => {
                next.entry(slot).or_default().insert(key.to_string(), value);
            }
            None => {
                if let Some(saved) = next.get_mut(&slot) {
                    saved.remove(key);
                    if saved.is_empty() {
                        next.remove(&slot);
                    }
                }
            }
        }
        write(&self.inner.path, &next)
            .map_err(|error| format!("couldn't save {}: {error}", self.inner.path.display()))?;
        *values = next;
        drop(values);
        self.inner.saved.send_replace(self.saved());
        Ok(())
    }
}

/// Where a plugin's secrets are kept: by its id, except a plugin in the
/// plugins folder that replaces a built-in one, whose are kept apart
/// (`"obs (plugins folder)"`; an id can't have a space).
fn slot(info: &PluginInfo) -> String {
    let id = &info.manifest.id;
    if !info.bundled && super::is_built_in(id) { format!("{id} (plugins folder)") } else { id.clone() }
}

fn write(path: &Path, values: &Values) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(values).expect("secrets serialise");
    let temp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&temp, json)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_saved_apart_and_given_to_the_plugin() {
        let dir = std::env::temp_dir().join(format!("lsk-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugins = Arc::new(Plugins::load_from(Path::new("no such folder"), true));
        let secrets = PluginSecrets::open(&dir, Arc::clone(&plugins));
        assert_eq!(secrets.values("obs")["password"], "", "none saved yet");
        assert!(secrets.values("alert").is_empty(), "no secret settings");

        let saved = secrets.subscribe();
        secrets.save("obs", "password", Some("hunter2".into())).unwrap();
        assert_eq!(secrets.values("obs")["password"], "hunter2");
        assert_eq!(*saved.borrow(), Saved::from([("obs".into(), vec!["password".into()])]));
        let file: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap()).unwrap();
        assert_eq!(file["obs"]["password"], "hunter2");

        // Read again after a restart.
        let again = PluginSecrets::open(&dir, Arc::clone(&plugins));
        assert_eq!(again.values("obs")["password"], "hunter2");

        assert!(secrets.save("obs", "address", Some("x".into())).is_err(), "not a secret setting");
        assert!(secrets.save("nope", "password", Some("x".into())).is_err());

        secrets.save("obs", "password", Some(String::new())).unwrap();
        assert_eq!(secrets.values("obs")["password"], "");
        assert!(saved.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_replacing_a_built_in_one_doesnt_get_its_secrets() {
        let dir = std::env::temp_dir().join(format!("lsk-secrets-replaced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugins = Arc::new(Plugins::load_from(&dir.join("plugins"), true));
        let secrets = PluginSecrets::open(&dir, Arc::clone(&plugins));
        secrets.save("obs", "password", Some("hunter2".into())).unwrap();

        // An `obs` in the plugins folder, with a secret setting of the same
        // name.
        let manifest = serde_json::json!({
            "id": "obs", "name": "Not OBS", "version": "0.1.0", "api": 1,
            "settings": [{ "key": "password", "label": "Password", "type": "secret" }]
        });
        std::fs::create_dir_all(dir.join("plugins/obs")).unwrap();
        std::fs::write(dir.join("plugins/obs/plugin.json"), manifest.to_string()).unwrap();
        plugins.reload();
        assert_eq!(plugins.catalog().plugin("obs").map(|info| info.bundled), Some(false));
        let saved = secrets.subscribe();
        secrets.refresh();
        assert!(saved.has_changed().unwrap());
        assert_eq!(secrets.values("obs")["password"], "", "not the built-in's");
        assert!(secrets.saved().is_empty());

        // Its own are kept apart.
        secrets.save("obs", "password", Some("its own".into())).unwrap();
        assert_eq!(secrets.values("obs")["password"], "its own");
        let file: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap()).unwrap();
        assert_eq!(file["obs"]["password"], "hunter2");
        assert_eq!(file["obs (plugins folder)"]["password"], "its own");

        // Removed: the built-in one has its password again.
        plugins.remove("obs").unwrap();
        assert_eq!(secrets.values("obs")["password"], "hunter2");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
