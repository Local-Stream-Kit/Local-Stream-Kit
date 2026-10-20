//! The plugin host: plugins are folders in `plugins/` next to the config,
//! each with a `plugin.json` (see `protocol::plugin` and
//! `docs/writing-a-plugin.md`), plus the ones that ship with the app, built
//! into the exe from `crates/server/plugins/` in the same format. They load
//! at startup and again when the panel asks (after installing one from a
//! zip, removing one, or "Look for new plugins"); a plugin with problems
//! isn't loaded, and the log and the panel say why.
//!
//! A plugin may have server code (`server.wasm`), run by [`server`].
//!
//! Programs on the control API's WebSocket can be plugins too (the external
//! connection API): they register a manifest while connected. Their node
//! types are remembered in a file, so cards keep them while the program
//! isn't running and after a restart.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, RwLock};

use protocol::cards::Category;
use protocol::plugin::{
    Catalog, EmoteValue, MANIFEST_FILE, MAX_ICON_BYTES, Manifest, OVERLAY_DIR, PluginFailure, PluginInfo,
    PluginNote, PluginStatus, SERVER_FILE, TextPart, TextValue,
};
use protocol::{AlertPart, EmoteImage, emote_path};
use serde_json::Value;
use tokio::sync::watch;

use crate::emote_text::EmoteText;
use crate::model::PostedEmote;

pub mod files;
pub mod memory;
pub mod net;
pub mod options;
pub mod secrets;
pub mod server;
pub mod tabs;

/// The plugins that ship with the app. Debug builds read them from disk;
/// release builds embed them (see `build.rs`).
#[derive(rust_embed::Embed)]
#[folder = "plugins/"]
struct Bundled;

/// The largest plugin zip the panel can install, unpacked.
pub const MAX_INSTALL_BYTES: usize = 64 * 1024 * 1024;

/// The installed plugins, and programs' plugins.
#[derive(Debug)]
pub struct Plugins {
    dir: PathBuf,
    /// With the built-in plugins (not in some tests).
    bundled: bool,
    /// Built in, and from the plugins folder.
    installed: RwLock<Installed>,
    /// Programs' plugins by id: connected now, or remembered.
    programs: Mutex<BTreeMap<String, PluginInfo>>,
    /// Where programs' plugins are remembered; `None` in tests.
    remembered: Option<PathBuf>,
    /// Plugins whose server code stopped, and why.
    stopped: Mutex<BTreeMap<String, String>>,
    /// What plugins' server code says about itself.
    statuses: Mutex<BTreeMap<String, PluginStatus>>,
    /// The notes plugins' server code puts under their settings, by plugin
    /// and setting key.
    notes: Mutex<BTreeMap<String, BTreeMap<String, PluginNote>>>,
    /// The numbers plugins' server code puts on their tabs (none at 0).
    tab_counts: Mutex<BTreeMap<String, u32>>,
    /// Bumped whenever a program connects or goes, or server code stops.
    changes: watch::Sender<u64>,
}

/// What a scan of the built-in plugins and the plugins folder found.
#[derive(Debug, Default)]
struct Installed {
    catalog: Catalog,
    failures: Vec<PluginFailure>,
    /// Changes when a plugin's manifest or server code does, so its server
    /// code is started again.
    fingerprints: BTreeMap<String, u64>,
}

/// A file of a plugin's overlay page.
pub enum PluginFile {
    Disk(PathBuf),
    Bundled(Cow<'static, [u8]>),
}

impl Plugins {
    /// Loads the bundled plugins and every plugin in `dir` (none if it
    /// doesn't exist). A plugin in `dir` with a bundled one's id replaces
    /// it. Programs' plugins remembered in `remembered` come back, not
    /// connected.
    pub fn load(dir: &Path, remembered: PathBuf) -> Plugins {
        let mut plugins = Plugins::load_from(dir, true);
        let manifests: Vec<Manifest> = match std::fs::read_to_string(&remembered) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!(
                    "{} can't be read ({error}); programs' plugins are forgotten",
                    remembered.display()
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        let installed = plugins.installed();
        let programs = plugins.programs.get_mut().unwrap();
        for manifest in manifests {
            if manifest.problems().is_empty() && installed.plugin(&manifest.id).is_none() {
                programs.insert(manifest.id.clone(), program(manifest, false));
            }
        }
        plugins.remembered = Some(remembered);
        plugins
    }

    pub(crate) fn load_from(dir: &Path, bundled: bool) -> Plugins {
        Plugins {
            dir: dir.to_path_buf(),
            bundled,
            installed: RwLock::new(scan(dir, bundled)),
            programs: Mutex::default(),
            stopped: Mutex::default(),
            statuses: Mutex::default(),
            notes: Mutex::default(),
            tab_counts: Mutex::default(),
            remembered: None,
            changes: watch::channel(0).0,
        }
    }

    /// The built-in and folder plugins.
    fn installed(&self) -> Catalog {
        self.installed.read().unwrap().catalog.clone()
    }

    /// Reads the plugins folder again. Panels, overlays, the cards and
    /// server code follow.
    pub fn reload(&self) {
        *self.installed.write().unwrap() = scan(&self.dir, self.bundled);
        let installed = self.installed();
        // A plugin installed in the folder wins over a program's remembered one.
        let mut programs = self.programs.lock().unwrap();
        programs.retain(|id, plugin| plugin.connected || installed.plugin(id).is_none());
        self.remember(&programs);
        drop(programs);
        self.changes.send_modify(|count| *count += 1);
    }

    /// Changes whenever a plugin's manifest or server code does.
    pub fn fingerprint(&self, id: &str) -> Option<u64> {
        self.installed.read().unwrap().fingerprints.get(id).copied()
    }

    /// Installs a plugin from a zip: its `plugin.json` at the top, or in
    /// one folder. It replaces an installed plugin with the same id (in the
    /// plugins folder; a built-in one is replaced as by any folder plugin).
    /// Answers the plugin's id.
    pub fn install_zip(&self, bytes: &[u8]) -> Result<String, String> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| format!("that isn't a zip file ({error})"))?;
        let names: Vec<String> =
            zip.file_names().filter_map(|name| name.ok().map(|name| name.into_owned())).collect();
        // Windows PowerShell's Compress-Archive names entries with `\`.
        let separator = |c: char| c == '/' || c == '\\';
        let manifest_name = names
            .iter()
            .filter(|name| {
                name.rsplit(separator).next() == Some(MANIFEST_FILE) && name.matches(separator).count() <= 1
            })
            .min_by_key(|name| name.len())
            .ok_or_else(|| format!("there's no {MANIFEST_FILE} in it (at the top, or in one folder)"))?
            .clone();
        let prefix = manifest_name.strip_suffix(MANIFEST_FILE).unwrap_or_default().replace('\\', "/");
        let mut text = String::new();
        zip.by_name(&manifest_name)
            .map_err(|error| error.to_string())?
            .read_to_string(&mut text)
            .map_err(|error| format!("{MANIFEST_FILE} can't be read: {error}"))?;
        let manifest: Manifest =
            serde_json::from_str(&text).map_err(|error| format!("{MANIFEST_FILE} isn't valid: {error}"))?;
        let id = manifest.id.clone();
        if !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') || id.is_empty() {
            return Err(format!("its id {id:?} isn't usable: lowercase letters, digits and -"));
        }
        // Unpacked beside the plugins, checked, then put in place.
        let temp = self.dir.join(format!(".installing-{id}"));
        let _ = std::fs::remove_dir_all(&temp);
        let unpacked = (|| {
            let mut total = 0usize;
            for index in 0..zip.len() {
                let mut entry = zip.by_index(index).map_err(|error| error.to_string())?;
                let name = entry.name().map_err(|error| error.to_string())?.replace('\\', "/");
                if entry.enclosed_name().is_none() || name.split('/').any(|part| part == "..") {
                    return Err(format!("{:?} points outside the plugin's folder", entry.name()));
                }
                let Some(relative) = name.strip_prefix(&prefix) else { continue };
                let relative: std::path::PathBuf =
                    relative.split('/').filter(|part| !part.is_empty()).collect();
                if entry.is_dir() || relative.as_os_str().is_empty() {
                    continue;
                }
                let too_big = || format!("it's bigger than {} MB unpacked", MAX_INSTALL_BYTES / 1024 / 1024);
                // What the zip says first (a quick no), then what's
                // actually written: an entry can unpack to more than it
                // declares.
                if total.saturating_add(entry.size() as usize) > MAX_INSTALL_BYTES {
                    return Err(too_big());
                }
                let target = temp.join(relative);
                std::fs::create_dir_all(target.parent().unwrap_or(&temp))
                    .map_err(|error| error.to_string())?;
                let mut file = std::fs::File::create(&target).map_err(|error| error.to_string())?;
                // One byte past what's left, to tell "too big" from "just fits".
                let left = (MAX_INSTALL_BYTES - total) as u64;
                let copied = std::io::copy(&mut entry.by_ref().take(left + 1), &mut file)
                    .map_err(|error| error.to_string())?;
                if copied > left {
                    return Err(too_big());
                }
                total += copied as usize;
            }
            let overlay = temp.join(OVERLAY_DIR).join("index.html").is_file();
            let server = temp.join(SERVER_FILE).is_file();
            check(&id, Ok(text.clone()), overlay, server, false, &|path| temp.join(path).is_file())
                .map_err(|problems| problems.join("; "))?;
            let folder = self.dir.join(&id);
            if folder.exists() {
                std::fs::remove_dir_all(&folder)
                    .map_err(|error| format!("the old version can't be removed: {error}"))?;
            }
            std::fs::rename(&temp, &folder).map_err(|error| error.to_string())
        })();
        if let Err(error) = unpacked {
            let _ = std::fs::remove_dir_all(&temp);
            return Err(error);
        }
        tracing::info!(plugin = id, version = manifest.version, "plugin installed");
        self.reload();
        Ok(id)
    }

    /// Deletes a plugin's folder from the plugins folder, then reloads. A
    /// built-in plugin can't be removed (only switched off).
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let from_folder = self.installed().plugin(id).is_some_and(|plugin| !plugin.bundled);
        if !from_folder {
            return Err(format!("{id} isn't a plugin in the plugins folder"));
        }
        std::fs::remove_dir_all(self.dir.join(id))
            .map_err(|error| format!("{id} can't be removed: {error}"))?;
        tracing::info!(plugin = id, "plugin removed");
        self.reload();
        Ok(())
    }

    /// Forgets a program's plugin that isn't connected.
    pub fn forget(&self, id: &str) -> Result<(), String> {
        let mut programs = self.programs.lock().unwrap();
        match programs.get(id) {
            Some(plugin) if plugin.connected => return Err(format!("the {id} program is connected")),
            Some(_) => {
                programs.remove(id);
            }
            None => return Err(format!("no program's plugin {id}")),
        }
        self.remember(&programs);
        drop(programs);
        self.changes.send_modify(|count| *count += 1);
        Ok(())
    }
}

/// Loads the built-in plugins (with `bundled`) and every plugin in `dir`
/// (none if it doesn't exist). A plugin in `dir` with a built-in one's id
/// replaces it.
fn scan(dir: &Path, bundled: bool) -> Installed {
    {
        let mut plugins: BTreeMap<String, PluginInfo> = BTreeMap::new();
        let mut fingerprints = BTreeMap::new();
        let mut failures = Vec::new();
        let mut fail = |name: String, problems: Vec<String>| {
            tracing::warn!(plugin = name, problems = problems.join("; "), "plugin not loaded");
            failures.push(PluginFailure { folder: name, problems });
        };
        let bundled_ids: Vec<String> = if bundled {
            Bundled::iter()
                .filter_map(|file| file.strip_suffix(&format!("/{MANIFEST_FILE}")).map(str::to_string))
                .filter(|id| !id.contains('/'))
                .collect()
        } else {
            Vec::new()
        };
        for id in bundled_ids {
            let text = Bundled::get(&format!("{id}/{MANIFEST_FILE}"))
                .map(|file| String::from_utf8_lossy(&file.data).into_owned())
                .ok_or_else(|| format!("can't read {MANIFEST_FILE}"));
            let overlay = Bundled::get(&format!("{id}/{OVERLAY_DIR}/index.html")).is_some();
            let server = Bundled::get(&format!("{id}/{SERVER_FILE}"));
            fingerprints
                .insert(id.clone(), fingerprint(&text, server.as_ref().map(|file| file.data.len() as u64)));
            let has_file = |path: &str| Bundled::get(&format!("{id}/{path}")).is_some();
            match check(&id, text, overlay, server.is_some(), true, &has_file) {
                Ok(plugin) => {
                    plugins.insert(id, plugin);
                }
                Err(problems) => fail(id, problems),
            }
        }
        let mut folders: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect(),
            Err(_) => Vec::new(),
        };
        folders.sort();
        for folder in folders {
            let name = folder.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            // Half-unpacked installs, and hidden folders.
            if name.starts_with('.') {
                continue;
            }
            let text = std::fs::read_to_string(folder.join(MANIFEST_FILE))
                .map_err(|error| format!("can't read {MANIFEST_FILE}: {error}"));
            let overlay = folder.join(OVERLAY_DIR).join("index.html").is_file();
            let server = std::fs::metadata(folder.join(SERVER_FILE)).ok().filter(|meta| meta.is_file());
            let stamp = server.as_ref().map(|meta| {
                let modified =
                    meta.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
                meta.len() ^ modified.map_or(0, |time| time.as_nanos() as u64)
            });
            fingerprints.insert(name.clone(), fingerprint(&text, stamp));
            match check(&name, text, overlay, server.is_some(), false, &|path| folder.join(path).is_file()) {
                Ok(plugin) => {
                    if plugins.get(&name).is_some_and(|known| known.bundled) {
                        tracing::info!(plugin = name, "the plugins folder's replaces the built-in one");
                    }
                    plugins.insert(name, plugin);
                }
                Err(problems) => fail(name, problems),
            }
        }
        if !plugins.is_empty() {
            let names: Vec<String> = plugins
                .values()
                .map(|plugin| format!("{} {}", plugin.manifest.id, plugin.manifest.version))
                .collect();
            tracing::info!(plugins = names.join(", "), "plugins loaded");
        }
        Installed { catalog: Catalog::new(plugins.into_values().collect()), failures, fingerprints }
    }
}

fn fingerprint(manifest: &Result<String, String>, server: Option<u64>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    manifest.hash(&mut hasher);
    server.hash(&mut hasher);
    hasher.finish()
}

impl Plugins {
    /// Every plugin's node types: the installed ones, then programs'
    /// (connected or remembered).
    pub fn catalog(&self) -> Catalog {
        let programs = self.programs.lock().unwrap();
        let stopped = self.stopped.lock().unwrap();
        let statuses = self.statuses.lock().unwrap();
        let notes = self.notes.lock().unwrap();
        let tab_counts = self.tab_counts.lock().unwrap();
        let installed = self.installed();
        let programs =
            programs.values().filter(|plugin| installed.plugin(&plugin.manifest.id).is_none()).cloned();
        let installed = installed.plugins().iter().map(|plugin| PluginInfo {
            stopped: stopped.get(&plugin.manifest.id).cloned(),
            status: statuses.get(&plugin.manifest.id).cloned(),
            notes: notes.get(&plugin.manifest.id).cloned().unwrap_or_default(),
            tab_count: tab_counts.get(&plugin.manifest.id).copied().unwrap_or(0),
            ..plugin.clone()
        });
        Catalog::new(installed.chain(programs).collect())
    }

    /// The installed plugins with server code.
    pub fn servers(&self) -> Vec<PluginInfo> {
        self.installed().plugins().iter().filter(|plugin| plugin.server).cloned().collect()
    }

    /// Whether an installed plugin has server code, which gets its effect
    /// nodes' runs instead of its page.
    pub fn has_server(&self, id: &str) -> bool {
        self.installed().plugin(id).is_some_and(|plugin| plugin.server)
    }

    /// A plugin's nodes' icon (its manifest's `icon`): a program's SVG, or
    /// the file in a plugin's folder; none past [`MAX_ICON_BYTES`].
    pub fn icon(&self, id: &str) -> Option<Cow<'static, [u8]>> {
        let info = self.catalog().plugin(id).filter(|plugin| !plugin.manifest.icon.is_empty())?.clone();
        let icon = &info.manifest.icon;
        let bytes = if let Some(svg) = info.manifest.icon_markup() {
            Cow::Owned(svg.as_bytes().to_vec())
        } else if info.program || !is_relative_path(icon) {
            return None;
        } else if info.bundled {
            Bundled::get(&format!("{id}/{icon}"))?.data
        } else {
            Cow::Owned(std::fs::read(self.dir.join(id).join(icon)).ok()?)
        };
        (bytes.len() <= MAX_ICON_BYTES).then_some(bytes)
    }

    /// A plugin's server code (the component's bytes).
    pub fn server_file(&self, id: &str) -> Option<Cow<'static, [u8]>> {
        let info = self.installed().plugin(id).filter(|plugin| plugin.server)?.clone();
        if info.bundled {
            return Bundled::get(&format!("{id}/{SERVER_FILE}")).map(|file| file.data);
        }
        std::fs::read(self.dir.join(id).join(SERVER_FILE)).ok().map(Cow::Owned)
    }

    /// A plugin's server code stopped: Status says why.
    pub fn stop(&self, id: &str, reason: String) {
        tracing::error!(plugin = id, "the plugin's server code stopped: {reason}");
        self.stopped.lock().unwrap().insert(id.to_string(), reason);
        self.statuses.lock().unwrap().remove(id);
        self.notes.lock().unwrap().remove(id);
        self.tab_counts.lock().unwrap().remove(id);
        self.changes.send_modify(|count| *count += 1);
    }

    /// Forgets why a plugin's server code stopped, its status, its notes
    /// and its tab's number (it's starting again, or gone).
    pub fn clear_stopped(&self, id: &str) {
        let stopped = self.stopped.lock().unwrap().remove(id).is_some();
        let status = self.statuses.lock().unwrap().remove(id).is_some();
        let notes = self.notes.lock().unwrap().remove(id).is_some();
        let count = self.tab_counts.lock().unwrap().remove(id).is_some();
        if stopped || status || notes || count {
            self.changes.send_modify(|count| *count += 1);
        }
    }

    /// A plugin's server code sets its status (`None` takes it away).
    /// Panels are told only when it changed.
    pub fn set_status(&self, id: &str, status: Option<PluginStatus>) {
        let mut statuses = self.statuses.lock().unwrap();
        let changed = match status {
            Some(status) => statuses.insert(id.to_string(), status.clone()) != Some(status),
            None => statuses.remove(id).is_some(),
        };
        drop(statuses);
        if changed {
            self.changes.send_modify(|count| *count += 1);
        }
    }

    /// A plugin's server code puts a number on its tab (0 takes it away).
    /// Panels are told only when it changed.
    pub fn set_tab_count(&self, id: &str, count: u32) {
        let mut counts = self.tab_counts.lock().unwrap();
        let changed = match count {
            0 => counts.remove(id).is_some(),
            count => counts.insert(id.to_string(), count) != Some(count),
        };
        drop(counts);
        if changed {
            self.changes.send_modify(|count| *count += 1);
        }
    }

    /// A plugin's server code puts a note under its setting `setting`
    /// (`None` takes it away). Panels are told only when it changed.
    pub fn set_note(&self, id: &str, setting: &str, note: Option<PluginNote>) {
        let mut notes = self.notes.lock().unwrap();
        let changed = match note {
            Some(note) => {
                let plugin = notes.entry(id.to_string()).or_default();
                plugin.insert(setting.to_string(), note.clone()) != Some(note)
            }
            None => {
                let plugin = notes.get_mut(id);
                let removed = plugin.is_some_and(|plugin| plugin.remove(setting).is_some());
                if notes.get(id).is_some_and(BTreeMap::is_empty) {
                    notes.remove(id);
                }
                removed
            }
        };
        drop(notes);
        if changed {
            self.changes.send_modify(|count| *count += 1);
        }
    }

    /// Notified whenever a program connects or goes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// A program on the control API registers as a plugin. Refused when
    /// its manifest has problems, or an installed plugin or another
    /// connected program has its id. Its node types are remembered.
    pub fn connect(&self, manifest: Manifest) -> Result<(), Vec<String>> {
        let id = manifest.id.clone();
        let mut problems = manifest.problems();
        if let Some(node) = queued_node(&manifest) {
            problems.push(format!("node {node:?}: a program's effects can't wait in the queue"));
        }
        if let Some(node) =
            manifest.nodes.iter().find(|node| node.category == Category::Effects && !node.outputs.is_empty())
        {
            problems.push(format!("node {:?}: a program's effect nodes can't have outputs yet", node.id));
        }
        if self.installed().plugin(&id).is_some() {
            problems.push(format!("the {id} plugin is installed in the app, so a program can't use its id"));
        }
        if !manifest.icon.is_empty() && manifest.icon_markup().is_none() {
            problems.push("icon: a program gives the SVG itself (<svg …>…</svg>), not a file".into());
        }
        if manifest.tab.is_some() {
            problems.push("tab: a program has no page files for a tab".into());
        }
        let mut programs = self.programs.lock().unwrap();
        if programs.get(&id).is_some_and(|known| known.connected) {
            problems.push(format!("another program is connected as the {id} plugin"));
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        programs.insert(id.clone(), program(manifest, true));
        self.remember(&programs);
        drop(programs);
        tracing::info!(plugin = id, "a program connected as a plugin");
        self.changes.send_modify(|count| *count += 1);
        Ok(())
    }

    /// The program that registered as `id` has gone: its node types stay.
    pub fn disconnect(&self, id: &str) {
        if let Some(plugin) = self.programs.lock().unwrap().get_mut(id) {
            plugin.connected = false;
        }
        tracing::info!(plugin = id, "a plugin's program went");
        self.changes.send_modify(|count| *count += 1);
    }

    fn remember(&self, programs: &BTreeMap<String, PluginInfo>) {
        let Some(path) = &self.remembered else { return };
        let manifests: Vec<&Manifest> = programs.values().map(|plugin| &plugin.manifest).collect();
        let json = serde_json::to_string_pretty(&manifests).expect("manifests serialise");
        let temp = path.with_extension("json.tmp");
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&temp, json))
            .and_then(|()| std::fs::rename(&temp, path));
        if let Err(error) = written {
            tracing::warn!("couldn't remember programs' plugins in {}: {error}", path.display());
        }
    }

    pub fn failures(&self) -> Vec<PluginFailure> {
        self.installed.read().unwrap().failures.clone()
    }

    /// The plugins with an overlay page, by id.
    pub fn pages(&self) -> Vec<String> {
        self.installed()
            .plugins()
            .iter()
            .filter(|plugin| plugin.overlay)
            .map(|plugin| plugin.manifest.id.clone())
            .collect()
    }

    /// A file in a plugin's `overlay/` folder, if the plugin is installed
    /// and `path` stays inside that folder. (A plugin with a tab or help
    /// but no overlay page has page files too.)
    pub fn overlay_file(&self, plugin: &str, path: &str) -> Option<PluginFile> {
        let info = self.installed().plugin(plugin)?.clone();
        if !is_relative_path(path) {
            return None;
        }
        let relative = Path::new(path);
        if info.bundled {
            return Bundled::get(&format!("{plugin}/{OVERLAY_DIR}/{path}"))
                .map(|file| PluginFile::Bundled(file.data));
        }
        let file = self.dir.join(plugin).join(OVERLAY_DIR).join(relative);
        file.is_file().then_some(PluginFile::Disk(file))
    }
}

/// Whether the app comes with a plugin with this id (which a plugin in the
/// plugins folder may replace).
pub fn is_built_in(id: &str) -> bool {
    Bundled::get(&format!("{id}/{MANIFEST_FILE}")).is_some()
}

/// Whether `path` stays inside the folder it's joined to: only names, no
/// `..`, no root, no drive (`C:/…`) and no `\`.
fn is_relative_path(path: &str) -> bool {
    !path.contains('\\') && Path::new(path).components().all(|part| matches!(part, Component::Normal(_)))
}

/// A program's plugin: no overlay page; its effect nodes go to the program.
fn program(manifest: Manifest, connected: bool) -> PluginInfo {
    PluginInfo { manifest, program: true, connected, ..Default::default() }
}

/// Reads a plugin's manifest and checks it; `name` is its folder's, and
/// `has_file` says whether a file is in it (by path).
fn check(
    name: &str,
    text: Result<String, String>,
    overlay: bool,
    server: bool,
    bundled: bool,
    has_file: &dyn Fn(&str) -> bool,
) -> Result<PluginInfo, Vec<String>> {
    let text = text.map_err(|error| vec![error])?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|error| vec![format!("{MANIFEST_FILE} isn't valid: {error}")])?;
    let mut problems = if bundled { manifest.built_in_problems() } else { manifest.problems() };
    if manifest.id != name {
        problems
            .push(format!("its id is {:?}, but its folder is named {name:?}; they must match", manifest.id));
    }
    let has = |category| manifest.nodes.iter().any(|node| node.category == category);
    if has(Category::Effects) && !overlay && !server {
        problems.push(format!(
            "it has effect nodes but no {OVERLAY_DIR}/index.html to show them or {SERVER_FILE} to run them"
        ));
    }
    if let Some(node) =
        manifest.nodes.iter().find(|node| node.category == Category::Effects && !node.outputs.is_empty())
        && !server
    {
        problems.push(format!(
            "node {:?} has outputs, which only {SERVER_FILE} fires (an overlay page can't yet)",
            node.id
        ));
    }
    if has(Category::Flow) && !server {
        problems.push(format!("it has flow nodes but no {SERVER_FILE} to send their events on"));
    }
    if manifest.hears.iter().any(|what| what == protocol::plugin::HEARS_EVENTS) && !server {
        problems.push(format!("it hears events, which only {SERVER_FILE} hears"));
    }
    if manifest.moderates && !server {
        problems.push(format!("it moderates, which only {SERVER_FILE} does"));
    }
    if has(Category::Values) && !server {
        problems.push(format!("it has value nodes but no {SERVER_FILE} to work their values out"));
    }
    if has(Category::Events) && !server {
        problems.push(format!(
            "it has event nodes but no {SERVER_FILE} to fire them (or connect a program to the control API; see docs/writing-a-plugin.md → Programs)"
        ));
    }
    if !manifest.icon.is_empty() && manifest.icon_markup().is_none() && !has_file(&manifest.icon) {
        problems.push(format!("icon {:?}: there's no such file in its folder", manifest.icon));
    }
    if let Some(tab) = &manifest.tab
        && !problems.iter().any(|problem| problem.starts_with("tab:"))
        && !has_file(&format!("{OVERLAY_DIR}/{}", tab.page))
    {
        problems.push(format!("tab: its page {OVERLAY_DIR}/{} isn't there", tab.page));
    }
    if !manifest.voices.is_empty() && !server {
        problems.push(format!(
            "it has voices but no {SERVER_FILE} to make their messages (or connect a program to the control API; see docs/writing-a-plugin.md → Programs)"
        ));
    }
    if problems.is_empty() {
        Ok(PluginInfo { manifest, overlay, bundled, server, ..Default::default() })
    } else {
        Err(problems)
    }
}

/// A text as a plugin page gets it: plain, and as parts with its emotes
/// and text styles.
pub fn text_value(text: &EmoteText) -> Value {
    let parts = text
        .parts()
        .into_iter()
        .map(|part| match part {
            AlertPart::Text { text, style } => TextPart::Text { text, style },
            AlertPart::Emote { code, sprite, style } => {
                TextPart::Emote { emote: emote_value(code, &sprite.image, &sprite.overlays), style }
            }
        })
        .collect();
    serde_json::to_value(TextValue { text: text.plain(), parts, speech: None })
        .expect("text values serialise")
}

/// The spoken message of an input read aloud, added to its text value.
pub fn add_speech(values: &mut serde_json::Map<String, Value>, input: &str, clip: protocol::SpeechClip) {
    if let Some(Value::Object(text)) = values.get_mut(input) {
        text.insert("speech".into(), serde_json::to_value(clip).expect("speech clips serialise"));
    }
}

/// Emotes (from chat, or found in a text) as a plugin page gets them.
pub fn emotes_value(emotes: &[PostedEmote]) -> Value {
    let image =
        |emote: &crate::model::EmoteRef| EmoteImage { source: emote.source.clone(), id: emote.id.clone() };
    let emotes: Vec<EmoteValue> = emotes
        .iter()
        .map(|posted| {
            let overlays: Vec<EmoteImage> = posted.overlays.iter().map(image).collect();
            emote_value(posted.emote.code.clone(), &image(&posted.emote), &overlays)
        })
        .collect();
    serde_json::to_value(emotes).expect("emote values serialise")
}

/// Emotes from the registry (an emotes setting's) as a plugin page gets
/// them.
pub fn emote_infos_value(emotes: &[crate::services::emotes::EmoteInfo]) -> Value {
    let emotes: Vec<EmoteValue> = emotes
        .iter()
        .map(|info| {
            let image = EmoteImage { source: info.emote.source.clone(), id: info.emote.id.clone() };
            emote_value(info.emote.code.clone(), &image, &[])
        })
        .collect();
    serde_json::to_value(emotes).expect("emote values serialise")
}

fn emote_value(code: String, image: &EmoteImage, overlays: &[EmoteImage]) -> EmoteValue {
    EmoteValue {
        code,
        url: emote_path(&image.source, &image.id),
        overlays: overlays.iter().map(EmoteImage::path).collect(),
    }
}

/// The first node that can wait in the queue, which needs an overlay page
/// or server code to say when it's done (a program can't yet).
fn queued_node(manifest: &Manifest) -> Option<&str> {
    manifest
        .nodes
        .iter()
        .find(|node| node.queue != protocol::plugin::QueueMode::Never)
        .map(|node| node.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &Path, file: &str, text: &str) {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const CONFETTI: &str = r#"{
        "id": "confetti", "name": "Confetti", "version": "0.1.0", "api": 1,
        "nodes": [{ "id": "burst", "title": "Confetti", "category": "effects",
                    "inputs": [{ "name": "trigger", "label": "Trigger", "type": "event" }] }]
    }"#;

    #[test]
    fn loads_good_plugins_and_reports_the_rest() {
        let dir = std::env::temp_dir().join(format!("lsk-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "confetti/plugin.json", CONFETTI);
        write(&dir, "confetti/overlay/index.html", "<!doctype html>");
        write(&dir, "confetti/overlay/js/main.js", "");
        // Named differently from its folder, and no page for its effect.
        write(&dir, "other/plugin.json", CONFETTI);
        write(&dir, "broken/plugin.json", "{ not json");
        // An event node needs server code to fire it.
        let events =
            CONFETTI.replace("\"confetti\"", "\"events\"").replace("\"effects\"", "\"events\"").replace(
                r#""inputs": [{ "name": "trigger", "label": "Trigger", "type": "event" }]"#,
                r#""outputs": [{ "name": "event", "label": "Event", "type": "event" }]"#,
            );
        write(&dir, "events/plugin.json", &events);
        write(&dir, "served/plugin.json", &events.replace(r#""id": "events""#, r#""id": "served""#));
        write(&dir, "served/server.wasm", "not really");
        // Only server code moderates.
        let moderates =
            CONFETTI.replace("\"confetti\"", "\"mods\"").replacen('{', "{ \"moderates\": true,", 1);
        write(&dir, "mods/plugin.json", &moderates);
        write(&dir, "mods/overlay/index.html", "<!doctype html>");

        let plugins = Plugins::load_from(&dir, false);
        assert_eq!(plugins.pages(), ["confetti"]);
        assert!(plugins.catalog().node_type("confetti", "burst").is_some());
        let failures = plugins.failures();
        let failed: Vec<&str> = failures.iter().map(|failure| failure.folder.as_str()).collect();
        assert_eq!(failed, ["broken", "events", "mods", "other"]);
        assert!(plugins.failures()[1].problems[0].contains("no server.wasm to fire them"));
        assert_eq!(plugins.failures()[2].problems, ["it moderates, which only server.wasm does"]);
        assert!(plugins.servers().iter().any(|plugin| plugin.manifest.id == "served"));
        assert!(plugins.server_file("served").is_some());
        let other = &plugins.failures()[3].problems;
        assert!(other.iter().any(|problem| problem.contains("folder is named")), "{other:?}");
        assert!(other.iter().any(|problem| problem.contains("no overlay/index.html")), "{other:?}");

        // Only files inside the plugin's overlay folder.
        assert!(plugins.overlay_file("confetti", "index.html").is_some());
        assert!(plugins.overlay_file("confetti", "js/main.js").is_some());
        assert!(plugins.overlay_file("confetti", "../plugin.json").is_none());
        assert!(plugins.overlay_file("confetti", "..\\plugin.json").is_none());
        assert!(plugins.overlay_file("confetti", "nope.js").is_none());
        assert!(plugins.overlay_file("other", "index.html").is_none());
        let _ = std::fs::remove_dir_all(&dir);

        // No plugins folder: no plugins, no problem.
        assert!(Plugins::load_from(&dir, false).catalog().plugins().is_empty());
    }

    #[test]
    fn the_bundled_plugins_load() {
        let plugins = Plugins::load_from(Path::new("no such folder"), true);
        assert_eq!(plugins.failures(), [], "the bundled plugins have problems");
        // The alerts', chat's and rain's pages are built from their crates
        // (trunk) into their folders.
        assert_eq!(
            plugins.pages(),
            ["alert", "chat", "hype-train", "media", "particles", "rain", "sound", "speak"]
        );
        assert!(plugins.catalog().plugins().iter().all(|plugin| plugin.bundled));
        for (plugin, file) in [
            ("media", "media.js"),
            ("particles", "particles.js"),
            ("hype-train", "hype-train.js"),
            ("hype-train", "looks/neon.css"),
            ("hype-train", "help.html"),
            ("rain", "index.html"),
            ("chat", "looks/classic.css"),
            ("chat", "help.html"),
            ("sound", "sound.js"),
        ] {
            assert!(
                matches!(plugins.overlay_file(plugin, file), Some(PluginFile::Bundled(_))),
                "{plugin}/{file}"
            );
        }
        assert!(plugins.overlay_file("media", "../plugin.json").is_none());
        // OBS's and Moderation's server code is built from their crates
        // (`cargo dist`, `cargo e2e`) into their folders.
        let servers: Vec<String> = plugins.servers().into_iter().map(|plugin| plugin.manifest.id).collect();
        assert_eq!(servers, ["moderation", "obs"]);
        for id in ["moderation", "obs"] {
            assert!(plugins.server_file(id).is_some_and(|wasm| wasm.starts_with(b"\0asm")), "{id}");
        }
    }

    #[test]
    fn programs_connect_and_are_remembered() {
        let dir = std::env::temp_dir().join(format!("lsk-programs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("cache").join("connected_plugins.json");
        let plugins = Plugins::load(&dir.join("plugins"), file.clone());
        let manifest: Manifest = serde_json::from_value(json!({
            "id": "billybilly", "name": "BillyBilly", "version": "1.0.0", "api": 1,
            "nodes": [{ "id": "membership", "title": "Membership", "category": "events",
                        "outputs": [{ "name": "event", "label": "Event", "type": "event" }] }]
        }))
        .unwrap();
        let mut changes = plugins.subscribe();
        plugins.connect(manifest.clone()).unwrap();
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        assert!(plugins.catalog().node_type("billybilly", "membership").is_some());
        let refused = plugins.connect(manifest.clone()).unwrap_err();
        assert!(refused[0].contains("another program"), "{refused:?}");
        let mut media = manifest.clone();
        media.id = "media".into();
        assert!(plugins.connect(media).unwrap_err()[0].contains("installed in the app"));
        // Flow and value nodes, and hearing events, as server code has them.
        let answering: Manifest = serde_json::from_value(json!({
            "id": "regulars", "name": "Regulars", "version": "1.0.0", "api": 1,
            "hears": ["chat", "events"],
            "nodes": [
                { "id": "first", "title": "Regulars first", "category": "flow",
                  "inputs": [{ "name": "event", "label": "Event", "type": "event" }],
                  "outputs": [{ "name": "regular", "label": "Regular", "type": "event" }] },
                { "id": "messages", "title": "Messages", "category": "values",
                  "outputs": [{ "name": "count", "label": "Count", "type": "number" }] }
            ]
        }))
        .unwrap();
        assert_eq!(plugins.connect(answering), Ok(()));

        plugins.disconnect("billybilly");
        assert!(changes.has_changed().unwrap());
        let info = plugins.catalog().plugin("billybilly").cloned().unwrap();
        assert!(info.program && !info.connected, "kept, not connected");
        plugins.connect(manifest).unwrap();

        // After a restart: remembered, not connected.
        let again = Plugins::load(&dir.join("plugins"), file);
        let info = again.catalog().plugin("billybilly").cloned().unwrap();
        assert!(info.program && !info.connected);
        assert!(again.pages().iter().all(|page| page != "billybilly"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, text.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn installs_from_a_zip_removes_and_forgets() {
        let dir = std::env::temp_dir().join(format!("lsk-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugins = Plugins::load_from(&dir, false);
        let changes = plugins.subscribe();

        // In a folder inside the zip, as zipping a folder makes it.
        let zip =
            zip_of(&[("confetti/plugin.json", CONFETTI), ("confetti/overlay/index.html", "<!doctype html>")]);
        assert_eq!(plugins.install_zip(&zip), Ok("confetti".to_string()));
        assert!(changes.has_changed().unwrap());
        assert_eq!(plugins.pages(), ["confetti"]);
        assert!(dir.join("confetti/overlay/index.html").is_file());
        // Named with `\`, as Windows PowerShell's Compress-Archive does.
        let zip = zip_of(&[
            ("confetti\\plugin.json", CONFETTI),
            ("confetti\\overlay\\index.html", "<!doctype html>"),
        ]);
        assert_eq!(plugins.install_zip(&zip), Ok("confetti".to_string()));
        assert!(dir.join("confetti/overlay/index.html").is_file());
        // Again, replacing it (files at the top this time).
        let zip = zip_of(&[("plugin.json", CONFETTI), ("overlay/index.html", "<!doctype html>")]);
        assert_eq!(plugins.install_zip(&zip), Ok("confetti".to_string()));

        assert!(plugins.install_zip(b"not a zip").unwrap_err().contains("isn't a zip"));
        assert!(
            plugins.install_zip(&zip_of(&[("readme.txt", "hi")])).unwrap_err().contains("no plugin.json")
        );
        // No page for its effect: refused, and nothing is left behind.
        let broken = CONFETTI.replace("confetti", "broken");
        assert!(
            plugins
                .install_zip(&zip_of(&[("plugin.json", &broken)]))
                .unwrap_err()
                .contains("no overlay/index.html")
        );
        assert!(!dir.join("broken").exists() && !dir.join(".installing-broken").exists());
        // Nothing outside its folder.
        let escape =
            zip_of(&[("plugin.json", CONFETTI), ("overlay/index.html", "x"), ("../escaped.txt", "x")]);
        assert!(plugins.install_zip(&escape).is_err());
        let escape =
            zip_of(&[("plugin.json", CONFETTI), ("overlay/index.html", "x"), ("..\\escaped.txt", "x")]);
        assert!(plugins.install_zip(&escape).is_err());
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
        // A file that unpacks to more than the zip says: what's written
        // counts.
        let big = "\0".repeat(MAX_INSTALL_BYTES + 1);
        let mut lying = zip_of(&[("plugin.json", CONFETTI), ("overlay/index.html", "x"), ("big.bin", &big)]);
        let declared = (big.len() as u32).to_le_bytes();
        let mut patched = 0;
        for at in 0..lying.len() - 28 {
            // Its uncompressed size, in its local header and in the
            // central directory, said to be 1 byte.
            let size_at = match lying[at..at + 4] {
                [0x50, 0x4b, 0x03, 0x04] => at + 22,
                [0x50, 0x4b, 0x01, 0x02] => at + 24,
                _ => continue,
            };
            if lying[size_at..size_at + 4] == declared {
                lying[size_at..size_at + 4].copy_from_slice(&1u32.to_le_bytes());
                patched += 1;
            }
        }
        assert_eq!(patched, 2);
        let refused = plugins.install_zip(&lying).unwrap_err();
        assert!(refused.contains("bigger than"), "{refused}");
        assert!(!dir.join(".installing-confetti").exists());

        assert!(plugins.remove("media").is_err(), "not in the folder");
        plugins.remove("confetti").unwrap();
        assert!(!dir.join("confetti").exists() && plugins.pages().is_empty());

        let manifest: Manifest = serde_json::from_str(&CONFETTI.replace("confetti", "prog")).unwrap();
        let mut manifest = manifest;
        manifest.nodes.clear();
        plugins.connect(manifest).unwrap();
        assert!(plugins.forget("prog").is_err(), "it's connected");
        plugins.disconnect("prog");
        plugins.forget("prog").unwrap();
        assert!(plugins.catalog().plugin("prog").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_paths_inside_the_folder() {
        for good in ["icon.svg", "art/icon.svg", "a/b/c.css"] {
            assert!(is_relative_path(good), "{good}");
        }
        for bad in ["../icon.svg", "art/../../icon.svg", "/icon.svg", "art\\icon.svg"] {
            assert!(!is_relative_path(bad), "{bad}");
        }
        if cfg!(windows) {
            for bad in ["C:/icon.svg", "C:icon.svg", "//server/share/icon.svg"] {
                assert!(!is_relative_path(bad), "{bad}");
            }
        }
    }

    #[test]
    fn values_for_pages() {
        let text = EmoteText::plain_text("hi");
        assert_eq!(text_value(&text), json!({ "text": "hi", "parts": [{ "type": "text", "text": "hi" }] }));
        let emotes = emotes_value(&[crate::effects::tests::posted("PogU", &["RainTime"])]);
        assert_eq!(emotes[0]["code"], "PogU");
        assert_eq!(emotes[0]["overlays"].as_array().unwrap().len(), 1);
    }
}
