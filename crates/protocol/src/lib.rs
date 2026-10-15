//! Everything that crosses the server ↔ overlay boundary: WebSocket
//! messages, settings and URL paths.
//!
//! Both the server and the overlay build from this crate, so a mismatch
//! between them is a compile error rather than something to keep in sync
//! by hand. Neither side should hand-write these JSON shapes or paths.

pub mod activity;
pub mod alert;
pub mod cards;
pub mod chat;
pub mod config;
pub mod control;
pub mod links;
pub mod overrides;
pub mod panel;
pub mod plugin;
pub mod sound;
pub mod speech;
pub mod text_style;
pub mod update;

pub use alert::AlertStyle;
pub use plugin::PluginCommand;
use serde::{Deserialize, Serialize};
pub use sound::SoundSettings;
pub use speech::{SpeechClip, SpeechSettings};
pub use text_style::TextStyle;

/// WebSocket endpoint the overlay connects to.
pub const WS_PATH: &str = "/ws";

/// Image proxy route, in axum path syntax. See [`emote_path`].
pub const EMOTE_ROUTE: &str = "/emote/{source}/{id}";

/// The path the overlay fetches an emote image from.
pub fn emote_path(source: &str, id: &str) -> String {
    format!("/emote/{}/{}", encode_segment(source), encode_segment(id))
}

/// Percent-encodes everything except unreserved URL characters, so an id can
/// never escape its path segment.
pub(crate) fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Overlay → server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Sent on every (re)connect: what its link asked for. `overlay`: the
    /// overlay it is (`?overlay=`; `config::Overlay`), and the app says
    /// which pages it hosts; none and no `plugins`: the default overlay.
    /// `plugins`: the plugins named in `?modules=`, for a link without
    /// `?overlay=`.
    Hello {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overlay: Option<String>,
        #[serde(default)]
        plugins: Option<Vec<String>>,
    },
    /// The queue's item `id` has finished playing here (a plugin page said
    /// it's done). With several overlays, the first `Done` moves the queue
    /// on.
    Done { id: u64 },
    /// A plugin's page kept a value (`PageMessage::Keep`).
    Keep { plugin: String, value: serde_json::Value },
}

/// Server → overlay: one command plus delivery metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Random per server run. When it changes, the overlay forgets the ids
    /// it has seen, because ids start over.
    pub session: u64,
    /// Unique and increasing within a session. The overlay skips ids it has
    /// already handled, so a command delivered twice is harmless.
    pub id: u64,
    /// Unix time in milliseconds.
    pub issued_at_ms: u64,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "module", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // most are a plugin's (a chat line each); boxing gains nothing
pub enum Command {
    /// For a plugin's overlay page; overlays that host it pass it on.
    Plugin(PluginCommand),
    /// Sent to every overlay, whatever pages it hosts.
    Shell(ShellCommand),
}

impl Command {
    /// The plugin this command is for, if it's a plugin's.
    pub fn plugin(&self) -> Option<&str> {
        match self {
            Command::Plugin(command) => Some(&command.plugin),
            _ => None,
        }
    }
}

/// A piece of a text with emotes in it (a chat line, an event's message).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertPart {
    Text {
        text: String,
        /// The text style a node's text marked it with (`[highlight]…[/]`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<TextStyle>,
    },
    /// An emote, with zero-width emotes stacked on it, as in chat.
    Emote {
        code: String,
        sprite: EmoteSprite,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<TextStyle>,
    },
}

impl AlertPart {
    /// Unstyled text.
    pub fn text(text: impl Into<String>) -> AlertPart {
        AlertPart::Text { text: text.into(), style: None }
    }

    pub fn style(&self) -> Option<&TextStyle> {
        match self {
            AlertPart::Text { style, .. } | AlertPart::Emote { style, .. } => style.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellCommand {
    /// The overlay build the server is serving: sent on every (re)connect,
    /// and in dev whenever the overlay is rebuilt. A page running a
    /// different build reloads itself to pick up the new one.
    Build { id: String },
    /// The master volume (0–1) and mute ([`SoundSettings`]): every sound on
    /// the page plays at its own volume times this, live, so a change
    /// reaches what's playing. Sent on connect and on every change.
    Volume { volume: f32, muted: bool },
    /// The installed plugins with an overlay page: sent on every
    /// (re)connect. The page makes an iframe for each one it hosts, at
    /// [`plugin::page_path`].
    Plugins { plugins: Vec<String> },
}

/// One emote image, fetched from [`emote_path`]. Its size comes from the
/// decoded image.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmoteImage {
    pub source: String,
    pub id: String,
}

impl EmoteImage {
    pub fn path(&self) -> String {
        emote_path(&self.source, &self.id)
    }
}

/// An emote: an image plus zero-width emotes drawn on top of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmoteSprite {
    pub image: EmoteImage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<EmoteImage>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(command: Command) -> Envelope {
        Envelope { session: 7, id: 1, issued_at_ms: 1_000, command }
    }

    fn roundtrip(envelope: &Envelope) -> serde_json::Value {
        let text = serde_json::to_string(envelope).unwrap();
        let back: Envelope = serde_json::from_str(&text).unwrap();
        assert_eq!(&back, envelope);
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn shell_commands_roundtrip() {
        let value = roundtrip(&envelope(Command::Shell(ShellCommand::Build { id: "ab12".into() })));
        assert_eq!(value["command"], json!({ "module": "shell", "type": "build", "id": "ab12" }));
    }

    #[test]
    fn hello_shape() {
        let hello = ClientMessage::Hello { overlay: None, plugins: Some(vec!["chat".into()]) };
        let value = serde_json::to_value(&hello).unwrap();
        assert_eq!(value, json!({ "type": "hello", "plugins": ["chat"] }));
        let named = ClientMessage::Hello { overlay: Some("chat".into()), plugins: None };
        assert_eq!(
            serde_json::to_value(&named).unwrap(),
            json!({ "type": "hello", "overlay": "chat", "plugins": null })
        );
        // Neither: the default overlay.
        let all: ClientMessage = serde_json::from_value(json!({ "type": "hello" })).unwrap();
        assert_eq!(all, ClientMessage::Hello { overlay: None, plugins: None });
        let plugin = Command::Plugin(PluginCommand {
            plugin: "confetti".into(),
            message: plugin::PluginMessage::Run {
                node: "burst".into(),
                input: "trigger".into(),
                values: serde_json::Map::new(),
                changes: Default::default(),
                settings: Default::default(),
                queue_id: None,
                run_id: None,
                preview: None,
            },
        });
        assert_eq!(
            serde_json::to_value(&plugin).unwrap(),
            json!({ "module": "plugin", "plugin": "confetti",
                    "message": { "type": "run", "node": "burst", "input": "trigger", "values": {} } })
        );
        let done = serde_json::to_value(ClientMessage::Done { id: 7 }).unwrap();
        assert_eq!(done, json!({ "type": "done", "id": 7 }));
    }

    #[test]
    fn emote_path_encodes_segments() {
        assert_eq!(emote_path("7tv", "01G3WEGZN0000ET2J0MQP5YJ0G"), "/emote/7tv/01G3WEGZN0000ET2J0MQP5YJ0G");
        assert_eq!(emote_path("twitch", "emotesv2_abc"), "/emote/twitch/emotesv2_abc");
        assert_eq!(emote_path("x", "../a b"), "/emote/x/%2E%2E%2Fa%20b");
    }
}
