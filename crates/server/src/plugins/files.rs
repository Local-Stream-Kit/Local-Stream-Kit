//! Plugins' file settings (a stylesheet): each picks one of its looks, the
//! plugin's own (page files, which never change) or the user's copies of
//! them, a file each in the plugins' data folder,
//! `<plugin>/<key>/<name>.<ext>`, edited on the Plugins page or in any
//! editor. Checked once a second, so a save from elsewhere shows at once.
//! Which look is picked is the setting's value in the config; pages and
//! server code get its text with the plugin's settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use protocol::plugin::{
    Catalog, MOVED_LOOK, PluginLook, SettingKind, built_in_looks, look_name_problem, picked_look,
};
use serde_json::{Map, Value};
use tokio::sync::watch;

use super::{PluginFile, Plugins};

const CHECK_EVERY: Duration = Duration::from_secs(1);

/// Every plugin's file settings' looks, by plugin and key: the plugin's
/// own first, then the user's copies by name.
pub type Texts = BTreeMap<String, BTreeMap<String, Vec<PluginLook>>>;

#[derive(Clone)]
pub struct PluginFiles {
    inner: Arc<Inner>,
}

struct Inner {
    /// The plugins' data folder.
    dir: PathBuf,
    plugins: Arc<Plugins>,
    texts: watch::Sender<Texts>,
}

impl PluginFiles {
    /// Reads the looks in `dir` (the plugins' data folder) now.
    pub fn open(dir: PathBuf, plugins: Arc<Plugins>) -> PluginFiles {
        let files =
            PluginFiles { inner: Arc::new(Inner { dir, plugins, texts: watch::Sender::new(Texts::new()) }) };
        files.refresh();
        files
    }

    /// Checks the files once a second (and the plugins: one installed or
    /// gone brings or takes its settings).
    pub fn watch(&self) {
        let files = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(CHECK_EVERY);
            loop {
                interval.tick().await;
                files.refresh();
            }
        });
    }

    pub fn subscribe(&self) -> watch::Receiver<Texts> {
        self.inner.texts.subscribe()
    }

    /// `plugin`'s file settings' texts, by key: the looks its settings
    /// (`stored`) pick, or their defaults.
    pub fn values(&self, plugin: &str, stored: &Map<String, Value>) -> Map<String, Value> {
        let texts = self.inner.texts.borrow();
        texts
            .get(plugin)
            .map(|files| {
                files
                    .iter()
                    .map(|(key, looks)| {
                        let picked = stored.get(key).and_then(Value::as_str).unwrap_or_default();
                        let text =
                            picked_look(looks, picked).map(|look| look.text.clone()).unwrap_or_default();
                        (key.clone(), Value::String(text))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Saves one of the user's looks (a new copy, or over one of theirs);
    /// `None` deletes it.
    pub fn save(&self, plugin: &str, key: &str, name: &str, text: Option<String>) -> std::io::Result<()> {
        let (folder, extension) = self.folder(plugin, key)?;
        let looks = self.looks(plugin, key);
        let theirs = looks.iter().any(|look| look.own && look.name == name);
        let path = folder.join(format!("{name}{extension}"));
        match text {
            Some(text) => {
                let renaming = theirs.then_some(name);
                if let Some(problem) = look_name_problem(name, &looks, renaming) {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, problem));
                }
                std::fs::create_dir_all(&folder)?;
                let temporary = folder.join(format!("{name}{extension}.tmp"));
                std::fs::write(&temporary, text)?;
                std::fs::rename(&temporary, &path)?;
            }
            None if !theirs => return Err(not_found("no such look of yours")),
            None => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            },
        }
        self.refresh();
        Ok(())
    }

    /// Renames one of the user's looks (its file).
    pub fn rename(&self, plugin: &str, key: &str, name: &str, to: &str) -> std::io::Result<()> {
        let (folder, extension) = self.folder(plugin, key)?;
        let looks = self.looks(plugin, key);
        if !looks.iter().any(|look| look.own && look.name == name) {
            return Err(not_found("no such look of yours"));
        }
        if let Some(problem) = look_name_problem(to, &looks, Some(name)) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, problem));
        }
        std::fs::rename(folder.join(format!("{name}{extension}")), folder.join(format!("{to}{extension}")))?;
        self.refresh();
        Ok(())
    }

    fn looks(&self, plugin: &str, key: &str) -> Vec<PluginLook> {
        self.inner.texts.borrow().get(plugin).and_then(|files| files.get(key)).cloned().unwrap_or_default()
    }

    /// Where a file setting's copies are, and the extension they have.
    fn folder(&self, plugin: &str, key: &str) -> std::io::Result<(PathBuf, String)> {
        let catalog = self.inner.plugins.catalog();
        let manifest = &catalog.plugin(plugin).ok_or_else(|| not_found("no such plugin"))?.manifest;
        let file = manifest
            .settings
            .iter()
            .find_map(|setting| match &setting.kind {
                SettingKind::File { file, .. } if setting.key == key => Some(file.as_str()),
                _ => None,
            })
            .ok_or_else(|| not_found("no such file setting"))?;
        Ok((self.inner.dir.join(plugin).join(key), extension(file)))
    }

    /// Reads every file setting's looks again; subscribers hear of a
    /// change.
    fn refresh(&self) {
        let catalog = self.inner.plugins.catalog();
        let mut texts = Texts::new();
        for info in catalog.plugins() {
            let id = &info.manifest.id;
            for setting in &info.manifest.settings {
                let SettingKind::File { file, default, starts } = &setting.kind else { continue };
                let mut looks: Vec<PluginLook> = built_in_looks(starts, default)
                    .into_iter()
                    .map(|look| PluginLook {
                        text: self.page_file(id, &look.value),
                        default: look.value == *default,
                        name: look.label,
                        own: false,
                    })
                    .collect();
                let mut own =
                    own_looks(&self.inner.dir.join(id).join(&setting.key), &extension(file), &looks);
                own.sort_by_key(|look| look.name.to_lowercase());
                looks.extend(own);
                texts.entry(id.clone()).or_default().insert(setting.key.clone(), looks);
            }
        }
        self.inner.texts.send_if_modified(|current| {
            let changed = *current != texts;
            if changed {
                *current = texts;
            }
            changed
        });
    }

    /// One of a plugin's page files as text (empty if there's none).
    fn page_file(&self, plugin: &str, path: &str) -> String {
        if path.is_empty() {
            return String::new();
        }
        let bytes = match self.inner.plugins.overlay_file(plugin, path) {
            Some(PluginFile::Bundled(bytes)) => bytes.into_owned(),
            Some(PluginFile::Disk(file)) => std::fs::read(file).unwrap_or_default(),
            None => Vec::new(),
        };
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// The user's looks in `folder`: its files with `extension` whose names
/// can be looks' (not one of the plugin's own, `built_in`).
fn own_looks(folder: &Path, extension: &str, built_in: &[PluginLook]) -> Vec<PluginLook> {
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let file_name = path.file_name()?.to_str()?;
            let name = if extension.is_empty() {
                file_name
            } else {
                let cut = file_name.len().checked_sub(extension.len())?;
                file_name.get(cut..)?.eq_ignore_ascii_case(extension).then(|| &file_name[..cut])?
            };
            if !path.is_file() || look_name_problem(name, built_in, None).is_some() {
                return None;
            }
            let text = std::fs::read_to_string(&path).ok()?;
            Some(PluginLook { name: name.to_string(), text, own: true, default: false })
        })
        .collect()
}

/// `.css` for `chat.css`; none for a name without one.
fn extension(file: &str) -> String {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or_else(String::new, |extension| format!(".{extension}"))
}

fn not_found(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, what.to_string())
}

/// Moves each file setting's file from before 2026-10-14, when there was
/// one, `<plugin>/<file>` in the plugins' data folder `dir`, to its looks
/// as "Custom". Gives the settings it moved, by plugin and key, for the
/// config to pick it.
pub fn move_old_files(dir: &Path, catalog: &Catalog) -> Vec<(String, String)> {
    let mut moved = Vec::new();
    for info in catalog.plugins() {
        let id = &info.manifest.id;
        for setting in &info.manifest.settings {
            let SettingKind::File { file, .. } = &setting.kind else { continue };
            let old = dir.join(id).join(file);
            let folder = dir.join(id).join(&setting.key);
            let new = folder.join(format!("{MOVED_LOOK}{}", extension(file)));
            if !old.is_file() || new.exists() {
                continue;
            }
            match std::fs::create_dir_all(&folder).and_then(|()| std::fs::rename(&old, &new)) {
                Ok(()) => {
                    tracing::info!("moved {} to {}", old.display(), new.display());
                    moved.push((id.clone(), setting.key.clone()));
                }
                Err(error) => tracing::warn!("couldn't move {} to {}: {error}", old.display(), new.display()),
            }
        }
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_setting_picks_one_of_its_looks() {
        let dir = std::env::temp_dir().join(format!("lsk-plugin-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("plugins").join("styled");
        std::fs::create_dir_all(folder.join("overlay").join("looks")).unwrap();
        std::fs::write(
            folder.join("plugin.json"),
            serde_json::json!({
                "id": "styled", "name": "Styled", "version": "1.0.0", "api": 1,
                "settings": [{ "key": "look", "label": "Look", "type": "file", "file": "look.css",
                               "default": "looks/plain.css",
                               "starts": [{ "value": "looks/plain.css", "label": "Plain" },
                                          { "value": "looks/bold.css", "label": "Bold" }] }]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(folder.join("overlay").join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(folder.join("overlay").join("looks").join("plain.css"), ".plain {}").unwrap();
        std::fs::write(folder.join("overlay").join("looks").join("bold.css"), ".bold {}").unwrap();
        // The one file from before: moved to the looks as "Custom".
        let data = dir.join("plugin_data");
        std::fs::create_dir_all(data.join("styled")).unwrap();
        std::fs::write(data.join("styled").join("look.css"), ".old {}").unwrap();
        let plugins = Arc::new(Plugins::load_from(&dir.join("plugins"), false));
        assert_eq!(move_old_files(&data, &plugins.catalog()), [("styled".to_string(), "look".to_string())]);
        assert!(!data.join("styled").join("look.css").exists());
        assert!(move_old_files(&data, &plugins.catalog()).is_empty(), "once");

        let files = PluginFiles::open(data.clone(), plugins);
        let changes = files.subscribe();
        let picked =
            |name: &str| files.values("styled", serde_json::json!({ "look": name }).as_object().unwrap());
        assert_eq!(picked("")["look"], ".plain {}", "the default");
        assert_eq!(picked("Bold")["look"], ".bold {}");
        assert_eq!(picked("Custom")["look"], ".old {}");
        assert_eq!(picked("Gone")["look"], ".plain {}", "one that's gone: the default");

        files.save("styled", "look", "Bold copy", Some(".mine {}".into())).unwrap();
        assert_eq!(
            std::fs::read_to_string(data.join("styled").join("look").join("Bold copy.css")).unwrap(),
            ".mine {}"
        );
        assert!(changes.has_changed().unwrap());
        let names: Vec<(String, bool)> =
            changes.borrow()["styled"]["look"].iter().map(|look| (look.name.clone(), look.own)).collect();
        assert_eq!(
            names,
            [
                ("Plain".into(), false),
                ("Bold".into(), false),
                ("Bold copy".into(), true),
                ("Custom".into(), true)
            ]
        );
        assert!(files.save("styled", "look", "plain", Some("x".into())).is_err(), "the plugin's own name");
        assert!(files.save("styled", "look", "a/b", Some("x".into())).is_err());

        // Edited elsewhere: the next check finds it.
        std::fs::write(data.join("styled").join("look").join("Bold copy.css"), ".edited {}").unwrap();
        files.refresh();
        assert_eq!(picked("Bold copy")["look"], ".edited {}");

        files.rename("styled", "look", "Bold copy", "Mine").unwrap();
        assert_eq!(picked("Mine")["look"], ".edited {}");
        assert!(files.rename("styled", "look", "Mine", "Custom").is_err(), "taken");
        assert!(files.rename("styled", "look", "Plain", "Mine 2").is_err(), "not theirs");

        files.save("styled", "look", "Mine", None).unwrap();
        assert_eq!(picked("Mine")["look"], ".plain {}", "deleted: the default");
        assert!(files.save("styled", "look", "Plain", None).is_err(), "the plugin's own stay");
        assert!(files.save("styled", "other", "x", None).is_err(), "no such setting");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
