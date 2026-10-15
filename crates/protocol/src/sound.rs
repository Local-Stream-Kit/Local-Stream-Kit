//! Sounds: files in the `sounds` folder (uploaded from the panel), played
//! by plugin pages on the overlay, so they reach OBS through the browser
//! source: the Sounds plugin's node, and an alert style's sound.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The built-in plugin with the "Play sound" node (once the app's own
/// Sound node).
pub const SOUND_PLUGIN: &str = "sound";
/// Its node.
pub const SOUND_NODE: &str = "play";

/// Sounds are served here, in axum path syntax. See [`sound_path`].
pub const SOUNDS_ROUTE: &str = "/sounds/{name}";

/// The path the overlay loads a sound from.
pub fn sound_path(name: &str) -> String {
    format!("/sounds/{}", crate::encode_segment(name))
}

/// Where a sound loads from: a file in the sounds folder, a spoken message
/// (`/speech/…`), or an http(s) address. `None` for no sound.
pub fn sound_url(sound: &str) -> Option<String> {
    let sound = sound.trim();
    if sound.is_empty() {
        None
    } else if sound.starts_with("/speech/") || sound.starts_with("https://") || sound.starts_with("http://") {
        Some(sound.to_string())
    } else {
        Some(sound_path(sound))
    }
}

/// The sound effect's settings. The master volume and mute reach every
/// overlay as [`ShellCommand::Volume`](crate::ShellCommand::Volume), so
/// they apply live, to what's playing too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundSettings {
    /// 0–1: every sound's volume is multiplied by this, alerts' sounds and
    /// spoken messages too.
    pub volume: f32,
    /// Everything plays on, silently: unmuted mid-message, the rest of it
    /// is heard.
    pub muted: bool,
    /// Names given to files in the sounds folder, by file name: the panel
    /// shows them instead of the file name (see [`label_of`](Self::label_of)).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl Default for SoundSettings {
    fn default() -> Self {
        SoundSettings { volume: 1.0, muted: false, labels: BTreeMap::new() }
    }
}

impl SoundSettings {
    /// What the panel calls a sound: its label, or else its file name.
    pub fn label_of<'a>(&'a self, file: &'a str) -> &'a str {
        self.labels.get(file).map(|label| label.trim()).filter(|label| !label.is_empty()).unwrap_or(file)
    }

    /// Gives a sound a label; one that's empty or its file name takes it
    /// away.
    pub fn set_label(&mut self, file: &str, label: &str) {
        let label = label.trim();
        if label.is_empty() || label == file {
            self.labels.remove(file);
        } else {
            self.labels.insert(file.to_string(), label.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(sound_url(""), None);
        assert_eq!(sound_url("air horn.mp3").as_deref(), Some("/sounds/air%20horn%2Emp3"));
        assert_eq!(sound_url("https://x.test/a.ogg").as_deref(), Some("https://x.test/a.ogg"));
    }

    #[test]
    fn labels() {
        let mut settings = SoundSettings::default();
        assert_eq!(settings.label_of("wow.mp3"), "wow.mp3", "no label: the file name");
        settings.set_label("wow.mp3", "  WOW ");
        assert_eq!(settings.label_of("wow.mp3"), "WOW");
        settings.set_label("wow.mp3", "wow.mp3");
        assert!(settings.labels.is_empty(), "its own file name is no label");
        settings.set_label("wow.mp3", "WOW");
        settings.set_label("wow.mp3", " ");
        assert!(settings.labels.is_empty(), "an empty one takes it away");
        // Saved only when there are any.
        assert!(!serde_json::to_string(&settings).unwrap().contains("labels"));
    }
}
