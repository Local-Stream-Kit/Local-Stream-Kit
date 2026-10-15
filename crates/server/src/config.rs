//! `lsk.json`: the whole configuration (connections, effects, cards), owned
//! by the control panel. Written by the server; hand edits still load (the
//! file is watched), but the panel is the intended editor.
//!
//! First start: the defaults (with the premade cards) are written.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use protocol::config::{AppConfig, CONFIG_VERSION};
use protocol::plugin::SPEAK_PLUGIN;
use serde_json::Value;

pub const FILE_NAME: &str = "lsk.json";

#[derive(Debug, Clone)]
pub struct Config {
    pub app: AppConfig,
    /// The JSON file it was loaded from.
    pub path: PathBuf,
}

impl Config {
    /// Loads the config from `dir`, creating it with the defaults if it
    /// doesn't exist yet.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Config> {
        let path = dir.join(FILE_NAME);
        if path.exists() {
            return Ok(Config { app: Config::read(&path)?, path });
        }
        tracing::warn!("no {} yet: starting with the defaults; set your Twitch channel", path.display());
        let config = Config { app: AppConfig::default(), path };
        config.save()?;
        Ok(config)
    }

    /// Reads and checks a config file.
    pub fn read(path: &Path) -> anyhow::Result<AppConfig> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("couldn't read {}", path.display()))?;
        let mut app: AppConfig =
            serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        keep_older(path, app.version)?;
        normalise(&mut app);
        let problems = app.problems();
        if !problems.is_empty() {
            bail!("{} has problems:\n{}", path.display(), problems.join("\n"));
        }
        Ok(app)
    }

    /// Writes the config (to a temporary file, then renamed, so a crash
    /// can't leave half a file).
    pub fn save(&self) -> anyhow::Result<()> {
        write(&self.path, &self.app)
    }

    /// The folder the config is in; the queue and history are kept there too.
    pub fn dir(&self) -> &Path {
        self.path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."))
    }

    /// Where emote images are cached on disk.
    pub fn image_cache_dir(&self) -> PathBuf {
        self.dir().join("cache").join("emotes")
    }

    /// Pictures for alerts (uploaded from the panel).
    pub fn pictures_dir(&self) -> PathBuf {
        self.dir().join("pictures")
    }

    /// Text to speech: the model, its voices and ONNX Runtime (downloaded
    /// from the panel).
    pub fn speech_dir(&self) -> PathBuf {
        self.dir().join("speech")
    }

    /// Spoken messages.
    pub fn speech_cache_dir(&self) -> PathBuf {
        self.dir().join("cache").join("speech")
    }

    /// Sounds (uploaded from the panel).
    pub fn sounds_dir(&self) -> PathBuf {
        self.dir().join("sounds")
    }

    /// Installed plugins, a folder each.
    pub fn plugins_dir(&self) -> PathBuf {
        self.dir().join("plugins")
    }

    /// What plugins' server code stores, a file each.
    pub fn plugin_data_dir(&self) -> PathBuf {
        self.dir().join("plugin_data")
    }

    /// The node types of programs that connected as plugins, kept for when
    /// they aren't connected.
    pub fn connected_plugins_path(&self) -> PathBuf {
        self.dir().join("cache").join("connected_plugins.json")
    }

    /// The broadcaster's Twitch login (secret; gitignored).
    pub fn twitch_tokens_path(&self) -> PathBuf {
        self.dir().join("twitch_tokens.json")
    }
}

/// Before a config from an older version is brought up to date (and then
/// saved over), keeps a copy as it was: `lsk.v1.json` beside it, so the
/// older exe can be put back with it. One from a newer version (the older
/// exe was put back without its copy) loses what this version doesn't
/// know when it's saved.
fn keep_older(path: &Path, version: u32) -> anyhow::Result<()> {
    if version > CONFIG_VERSION {
        tracing::warn!(
            "{} is from a newer version of the app (format {version}, this one reads {CONFIG_VERSION}):              settings this version doesn't know are lost when it saves",
            path.display()
        );
    }
    if version >= CONFIG_VERSION {
        return Ok(());
    }
    let copy = path.with_file_name(format!("lsk.v{version}.json"));
    std::fs::copy(path, &copy)
        .with_context(|| format!("couldn't keep a copy of {} before updating it", path.display()))?;
    tracing::info!(
        "kept {} as {} before updating it to format {CONFIG_VERSION}",
        path.display(),
        copy.display()
    );
    Ok(())
}

pub fn write(path: &Path, app: &AppConfig) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(app)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, json + "\n")
        .and_then(|()| std::fs::rename(&temp, path))
        .with_context(|| format!("couldn't save {}", path.display()))
}

/// Tidies what people type (a channel name with `#` or capitals), and
/// brings settings saved before a change up to date.
pub fn normalise(app: &mut AppConfig) {
    let channel = &mut app.connections.twitch_channel;
    *channel = channel.trim().trim_start_matches('#').to_ascii_lowercase();
    app.version = CONFIG_VERSION;
    // The chat display's "Leave out commands" became "Show commands", and
    // its bots a list (2026-10-14).
    if let Some(chat) = app.plugins.get_mut("chat") {
        let settings = &mut chat.settings;
        if let Some(hide) = settings.remove("hide_commands") {
            let show = !hide.as_bool().unwrap_or(true);
            settings.entry("show_commands").or_insert(Value::Bool(show));
        }
        if let Some(Value::String(names)) = settings.get("hidden_users") {
            let names =
                names.split(',').map(str::trim).filter(|name| !name.is_empty()).map(Value::from).collect();
            settings.insert("hidden_users".into(), Value::Array(names));
        }
    }
    // Text to speech's switch of its own went: the Speak plugin's is it
    // (2026-10-14), so a saved "off" turns the plugin off.
    if !std::mem::replace(&mut app.effects.speech.old_switch, true) {
        *app.plugin_enabled_mut(SPEAK_PLUGIN) = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lsk_config_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_defaults_then_loads_them() {
        let dir = temp_dir("defaults");
        let created = Config::load_or_create(&dir).unwrap();
        assert_eq!(created.app, AppConfig::default());
        assert!(dir.join(FILE_NAME).exists());
        assert_eq!(Config::load_or_create(&dir).unwrap().app, created.app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_chat_displays_old_settings_are_brought_up_to_date() {
        let mut app = AppConfig::default();
        let chat = app.plugins.entry("chat".into()).or_default();
        chat.settings.insert("hide_commands".into(), Value::Bool(true));
        chat.settings.insert("hidden_users".into(), "Nightbot, StreamElements ,".into());
        normalise(&mut app);
        let settings = &app.plugins["chat"].settings;
        assert_eq!(settings.get("show_commands"), Some(&Value::Bool(false)));
        assert!(!settings.contains_key("hide_commands"));
        assert_eq!(settings["hidden_users"], serde_json::json!(["Nightbot", "StreamElements"]));
        let again = app.clone();
        normalise(&mut app);
        assert_eq!(app, again, "once");
    }

    #[test]
    fn text_to_speechs_old_switch_turns_the_speak_plugin_off() {
        let mut app: AppConfig =
            serde_json::from_str(r#"{"effects": {"speech": {"enabled": false, "voice": "bf_emma"}}}"#)
                .unwrap();
        assert!(app.plugin(SPEAK_PLUGIN).enabled);
        normalise(&mut app);
        assert!(!app.plugin(SPEAK_PLUGIN).enabled);
        assert_eq!(app.effects.speech.settings.voice, "bf_emma");
        let saved = serde_json::to_value(&app).unwrap();
        assert!(saved["effects"]["speech"].get("enabled").is_none(), "never saved");
        // On, it leaves the plugin as it is.
        let mut app: AppConfig =
            serde_json::from_str(r#"{"effects": {"speech": {"enabled": true}}}"#).unwrap();
        normalise(&mut app);
        assert!(app.plugin(SPEAK_PLUGIN).enabled);
    }

    #[test]
    fn an_older_config_is_kept_before_its_brought_up_to_date() {
        let dir = temp_dir("older");
        let path = dir.join(FILE_NAME);
        let old = r#"{"version": 0, "connections": {"twitch_channel": "abc"}}"#;
        std::fs::write(&path, old).unwrap();
        let app = Config::read(&path).unwrap();
        assert_eq!((app.version, app.connections.twitch_channel.as_str()), (CONFIG_VERSION, "abc"));
        assert_eq!(std::fs::read_to_string(dir.join("lsk.v0.json")).unwrap(), old);
        // One already up to date isn't copied.
        let _ = std::fs::remove_file(dir.join("lsk.v0.json"));
        write(&path, &app).unwrap();
        Config::read(&path).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "only lsk.json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_files_are_reported() {
        let dir = temp_dir("bad");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"version": 3, "connections": {"emote_sources": ["nope"]}}"#).unwrap();
        assert!(Config::read(&path).unwrap_err().to_string().contains("unknown emote source"));
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Config::read(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
