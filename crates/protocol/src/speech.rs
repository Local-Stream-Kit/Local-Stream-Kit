//! Text to speech: Kokoro (Apache-2.0), run on this PC by the server. The
//! speech is a WAV file the overlay plays like a sound, one message at a
//! time. The model, its voices and ONNX Runtime are downloaded once from
//! the panel (Plugins → Speak) into a `speech` folder next to the
//! config.

use serde::{Deserialize, Serialize};

/// Spoken messages are served here, in axum path syntax. See
/// [`speech_path`].
pub const SPEECH_ROUTE: &str = "/speech/{name}";

/// The path the overlay loads a spoken message from.
pub fn speech_path(name: &str) -> String {
    format!("/speech/{}", crate::encode_segment(name))
}

/// A spoken message, ready to play.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeechClip {
    /// See [`speech_path`].
    pub url: String,
    /// Known for WAV files (the app's voices); a plugin's MP3 or Ogg
    /// message has none, so it plays until it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    /// 0–1, the speech volume (with the node's voice change); the page
    /// multiplies it by the master volume, as for every sound.
    pub volume: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    American,
    British,
}

/// One of Kokoro's English voices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Voice {
    /// The voice file's name, e.g. `bm_george`.
    pub id: &'static str,
    pub name: &'static str,
    pub accent: Accent,
    pub female: bool,
    /// Kokoro's own grade of the voice's quality (A best, F worst).
    pub grade: &'static str,
}

const fn voice(id: &'static str, name: &'static str, grade: &'static str) -> Voice {
    let bytes = id.as_bytes();
    let accent = if bytes[0] == b'b' { Accent::British } else { Accent::American };
    Voice { id, name, accent, female: bytes[1] == b'f', grade }
}

/// Kokoro v1.0's English voices, British first; grades from its
/// `VOICES.md`.
pub const VOICES: &[Voice] = &[
    voice("bf_emma", "Emma", "B-"),
    voice("bf_isabella", "Isabella", "C"),
    voice("bf_alice", "Alice", "D"),
    voice("bf_lily", "Lily", "D"),
    voice("bm_george", "George", "C"),
    voice("bm_fable", "Fable", "C"),
    voice("bm_lewis", "Lewis", "D+"),
    voice("bm_daniel", "Daniel", "D"),
    voice("af_heart", "Heart", "A"),
    voice("af_bella", "Bella", "A-"),
    voice("af_nicole", "Nicole", "B-"),
    voice("af_aoede", "Aoede", "C+"),
    voice("af_kore", "Kore", "C+"),
    voice("af_sarah", "Sarah", "C+"),
    voice("af_alloy", "Alloy", "C"),
    voice("af_nova", "Nova", "C"),
    voice("af_sky", "Sky", "C-"),
    voice("af_jessica", "Jessica", "D"),
    voice("af_river", "River", "D"),
    voice("am_fenrir", "Fenrir", "C+"),
    voice("am_michael", "Michael", "C+"),
    voice("am_puck", "Puck", "C+"),
    voice("am_echo", "Echo", "D"),
    voice("am_eric", "Eric", "D"),
    voice("am_liam", "Liam", "D"),
    voice("am_onyx", "Onyx", "D"),
    voice("am_santa", "Santa", "D-"),
    voice("am_adam", "Adam", "F+"),
];

impl Voice {
    pub fn find(id: &str) -> Option<&'static Voice> {
        VOICES.iter().find(|voice| voice.id == id)
    }

    /// "George (British man, C)".
    pub fn label(&self) -> String {
        let accent = match self.accent {
            Accent::American => "American",
            Accent::British => "British",
        };
        let who = if self.female { "woman" } else { "man" };
        format!("{} ({accent} {who}, {})", self.name, self.grade)
    }
}

/// A word said differently: names the dictionary doesn't know are spelled
/// out letter by letter otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pronunciation {
    /// Matched as a whole word, ignoring case.
    pub word: String,
    /// What's said instead, e.g. "Martin" for "Marrtin".
    pub say: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeechSettings {
    /// A [`VOICES`] id.
    pub voice: String,
    /// 0.5–2, 1 = normal.
    pub speed: f32,
    /// 0–1 (times the sound master volume).
    pub volume: f32,
    /// Longer texts are cut at a word; 0 = no limit.
    pub max_chars: u32,
    /// "KEKW KEKW KEKW" is read as one "KEKW".
    pub collapse_repeats: bool,
    /// Links ("https://…", "www.…", "twitch.tv/…") are left out; the rest
    /// of the message is read.
    pub skip_links: bool,
    pub pronunciations: Vec<Pronunciation>,
}

impl Default for SpeechSettings {
    fn default() -> Self {
        SpeechSettings {
            voice: "bm_george".into(),
            speed: 1.0,
            volume: 0.8,
            max_chars: 300,
            collapse_repeats: true,
            skip_links: true,
            pronunciations: Vec::new(),
        }
    }
}

/// What the Speak box's Try reads until something else is typed there.
pub const TRY_TEXT: &str = "Thanks for the follow!";

/// Text to speech in the config: how texts are read, and the Speak box's
/// Try text. Its switch is the Speak plugin's (2026-10-14: the app had one
/// of its own until then, besides the plugin's).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeechConfig {
    #[serde(flatten)]
    pub settings: SpeechSettings,
    /// The Speak box's Try text as typed (see [`try_text`](Self::try_text)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub try_text: Option<String>,
    /// The old switch of its own: read so that a saved "off" turns the
    /// Speak plugin off (the server's `config::normalise`), never saved.
    #[serde(default = "on", rename = "enabled", skip_serializing)]
    pub old_switch: bool,
}

fn on() -> bool {
    true
}

impl Default for SpeechConfig {
    fn default() -> Self {
        SpeechConfig { settings: SpeechSettings::default(), try_text: None, old_switch: true }
    }
}

impl SpeechConfig {
    /// Try's field: what was typed (emptied, empty: 2026-10-15, asked), or
    /// [`TRY_TEXT`] until then. Empty, Try reads [`TRY_TEXT`].
    pub fn try_text(&self) -> &str {
        self.try_text.as_deref().unwrap_or(TRY_TEXT)
    }
}

/// Whether text to speech can be used yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SpeechStatus {
    /// Not downloaded yet.
    #[default]
    NotInstalled,
    Installing {
        /// What's being downloaded, for people.
        step: String,
        /// Of the whole download.
        percent: u8,
    },
    Ready,
    Failed {
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voices() {
        assert_eq!(VOICES.len(), 28);
        let george = Voice::find("bm_george").unwrap();
        assert_eq!((george.accent, george.female), (Accent::British, false));
        assert_eq!(george.label(), "George (British man, C)");
        assert_eq!(Voice::find("af_heart").unwrap().accent, Accent::American);
        assert!(Voice::find(&SpeechSettings::default().voice).is_some());
        assert_eq!(speech_path("ab12.wav"), "/speech/ab12%2Ewav");
    }
}
