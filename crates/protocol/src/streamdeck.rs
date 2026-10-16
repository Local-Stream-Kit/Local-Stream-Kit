//! The Stream Deck plugin (`streamdeck/` in the repo): its file, its
//! version, and what Status shows of it.
//!
//! The plugin has a version of its own, in its manifest (Stream Deck's
//! versions have four numbers: `0.2.0.0`), raised only when the plugin
//! changes. A release has the file beside `lsk.exe`, and a build with it
//! built in installs it from Status → Control API. The plugin says its
//! version when it connects
//! ([`ControlRequest::Hello`](crate::control::ControlRequest::Hello)), so
//! Status can offer an update when it's older.

use serde::{Deserialize, Serialize};

/// The plugin's file name: opening it installs the plugin in Stream Deck.
pub const PLUGIN_FILE: &str = "com.localstreamkit.streamdeck.streamDeckPlugin";

/// What the plugin calls itself in its `hello` on the control API.
pub const CLIENT: &str = "streamdeck";

/// The plugin file of the release `version`, under `releases` (see
/// [`crate::update::releases`]).
pub fn release_file_url(releases: &str, version: &str) -> String {
    format!("{releases}/releases/download/v{version}/{PLUGIN_FILE}")
}

/// A version as Status shows it: without the fourth number when it's 0
/// (`0.1.1.0` → `0.1.1`).
pub fn shown(version: &str) -> &str {
    match version.strip_suffix(".0") {
        Some(three) if three.split('.').count() == 3 => three,
        _ => version,
    }
}

/// Whether plugin version `version` is older than `than`, comparing their
/// numbers in order (missing ones count as 0). One that isn't a version
/// isn't older.
pub fn is_older(version: &str, than: &str) -> bool {
    let (mut version, mut than) = (numbers(version), numbers(than));
    if version.is_empty() {
        return false;
    }
    let length = version.len().max(than.len());
    version.resize(length, 0);
    than.resize(length, 0);
    version < than
}

/// The leading numbers of a dotted version (`0.2.0-beta.1` → 0, 2, 0).
fn numbers(version: &str) -> Vec<u64> {
    let version = version.trim().split(['-', '+']).next().unwrap_or_default();
    version.split('.').map_while(|part| part.parse().ok()).collect()
}

/// The Stream Deck plugin, for Status → Control API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamDeckStatus {
    /// The version of the plugin built into the app (`0.2.0.0`): what
    /// Install puts in Stream Deck. `None` in a build without it.
    pub built_in: Option<String>,
    /// The versions the Stream Deck plugins connected now gave, in the
    /// order they connected (one, usually).
    pub connected: Vec<String>,
    /// Why the last Install failed (until one works).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_error: Option<String>,
}

impl StreamDeckStatus {
    /// A connected plugin is older than the built-in one: Install updates it.
    pub fn update_offered(&self) -> bool {
        self.built_in
            .as_deref()
            .is_some_and(|built_in| self.connected.iter().any(|version| is_older(version, built_in)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(shown("0.1.1.0"), "0.1.1");
        assert_eq!(shown("0.1.1.4"), "0.1.1.4");
        assert_eq!(shown("1.0"), "1.0");
        assert!(is_older("0.1.0.0", "0.1.1.0"));
        assert!(is_older("0.1.1", "0.1.1.1"));
        assert!(is_older("0.9.0.0", "0.10.0.0"), "as numbers, not text");
        assert!(!is_older("0.1.1.0", "0.1.1"));
        assert!(!is_older("0.2.0.0", "0.1.1.0"));
        assert!(!is_older("", "0.1.1.0"));
        assert!(!is_older("dev", "0.1.1.0"));
    }

    #[test]
    fn an_update_is_offered_for_an_older_plugin() {
        let mut status = StreamDeckStatus {
            built_in: Some("0.1.1.0".into()),
            connected: vec!["0.1.1.0".into()],
            install_error: None,
        };
        assert!(!status.update_offered());
        status.connected.push("0.1.0.0".into());
        assert!(status.update_offered());
        status.built_in = None;
        assert!(!status.update_offered(), "nothing to update it with");
        assert_eq!(
            release_file_url("https://github.com/o/r", "0.1.1"),
            "https://github.com/o/r/releases/download/v0.1.1/com.localstreamkit.streamdeck.streamDeckPlugin"
        );
    }
}
