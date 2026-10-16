//! The Stream Deck plugin built into the exe (the `streamdeck` feature,
//! which `cargo dist` turns on). Install, on Status, writes it to the cache
//! folder and opens it there, and Stream Deck asks whether to install it.
//! Which plugins are connected comes from their `hello` on the control API
//! (`control.rs`), kept in the status.

use std::path::{Path, PathBuf};

use protocol::streamdeck::PLUGIN_FILE;

use crate::desktop::Desktop;

/// The plugin's file, and its version as its manifest has it (`build.rs`).
#[cfg(feature = "streamdeck")]
const PLUGIN: Option<(&[u8], &str)> =
    Some((include_bytes!(env!("STREAMDECK_PLUGIN")), env!("STREAMDECK_VERSION")));
#[cfg(not(feature = "streamdeck"))]
const PLUGIN: Option<(&[u8], &str)> = None;

/// The built-in plugin's version (`0.2.0.0`), `None` without one.
pub fn built_in() -> Option<String> {
    PLUGIN.map(|(_, version)| version.to_string())
}

/// Writes the built-in plugin into `folder` and opens it: where it was
/// written, or why it couldn't be installed.
pub fn install(folder: &Path, desktop: Desktop) -> Result<PathBuf, String> {
    let Some((plugin, _)) = PLUGIN else {
        return Err("this build has no Stream Deck plugin built in".to_string());
    };
    write_and_open(plugin, folder, desktop)
}

/// Writes `plugin` as [`PLUGIN_FILE`] in `folder`, unless it's there
/// already (Stream Deck may still have it open), then opens it.
fn write_and_open(plugin: &[u8], folder: &Path, desktop: Desktop) -> Result<PathBuf, String> {
    let path = folder.join(PLUGIN_FILE);
    let path = std::path::absolute(&path).unwrap_or(path);
    if std::fs::read(&path).ok().as_deref() != Some(plugin) {
        std::fs::create_dir_all(folder)
            .and_then(|()| std::fs::write(&path, plugin))
            .map_err(|error| format!("couldn't write {}: {error}", path.display()))?;
    }
    desktop
        .open(&path)
        .map_err(|error| format!("couldn't open {} ({error}); open it yourself", path.display()))?;
    tracing::info!(path = %path.display(), "opened the Stream Deck plugin, for Stream Deck to install");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_is_written_once_and_opened() {
        let dir = std::env::temp_dir().join(format!("lsk_streamdeck_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("cache");
        let desktop = Desktop::new(true);
        let path = write_and_open(b"plugin", &folder, desktop).unwrap();
        assert!(path.is_absolute());
        assert_eq!(path.file_name().unwrap(), PLUGIN_FILE);
        assert_eq!(std::fs::read(&path).unwrap(), b"plugin");
        // The same again isn't written; another is.
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_and_open(b"plugin", &folder, desktop).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), written);
        write_and_open(b"newer plugin", &folder, desktop).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"newer plugin");
        // A folder that can't be made (a file is in the way) says so.
        std::fs::write(dir.join("taken"), "").unwrap();
        let error = write_and_open(b"plugin", &dir.join("taken"), desktop).unwrap_err();
        assert!(error.starts_with("couldn't write"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_built_in_plugin_has_its_manifests_version() {
        let manifest = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../streamdeck/com.localstreamkit.streamdeck.sdPlugin/manifest.json"
        );
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(manifest).unwrap()).unwrap();
        let expected =
            cfg!(feature = "streamdeck").then(|| manifest["Version"].as_str().unwrap().to_string());
        assert_eq!(built_in(), expected);
    }
}
