//! The app's whole configuration, as the server stores it
//! (`lsk.json`) and the control panel edits it.
//!
//! Everything has a default, so a missing key never fails to load; the
//! server checks values before saving or running them.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::activity::HistoryFilter;
use crate::cards::Card;
use crate::plugin::Catalog;
use crate::speech::SpeechConfig;
use crate::{SoundSettings, TextStyle};

/// The format's version, saved with it. Before the first release a change
/// needs no converting; after it, bump this and convert older files when
/// they're read.
pub const CONFIG_VERSION: u32 = 1;

/// Emote sources the app knows how to load, in their default order.
pub const EMOTE_SOURCES: [&str; 3] = ["7tv", "bttv", "ffz"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub connections: Connections,
    pub effects: Effects,
    pub cards: Vec<Card>,
    /// What the Activity page's history shows.
    pub history: HistoryFilter,
    /// Each plugin's switch, settings and allowed hosts, by plugin id; a
    /// plugin without an entry is on, with its default settings.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, PluginConfig>,
    /// Looks for parts of a text, by name (`[highlight]…[/]`), shared by
    /// every node and plugin.
    pub text_styles: Vec<TextStyle>,
    /// Their switch (on the Plugins page, like a plugin's): off, a mark
    /// is taken out of the text and gives no look.
    pub text_styles_enabled: bool,
    /// The Test page's texts, as last typed there.
    #[serde(skip_serializing_if = "TestTexts::is_empty")]
    pub test: TestTexts,
    /// The overlays (OBS Browser Sources), each with the plugins' pages
    /// it shows; a Browser Source names one in its link.
    pub overlays: Vec<Overlay>,
    /// What the log keeps.
    pub log: LogSettings,
    /// Whether the app looks for a new version.
    pub updates: UpdateSettings,
    /// What the chat log keeps (each viewer's card in Activity).
    pub chat_log: ChatLogSettings,
    /// Notes closed with their ×, so every panel leaves them out.
    pub closed_notes: ClosedNotes,
}

/// Notes closed with their × (2026-10-16): they stay closed in every
/// panel. One about the login and the chat being different channels is
/// closed for that pair (`"marrtin/vedal987"`, [`ClosedNotes::pair`]), and
/// shows again for another.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClosedNotes {
    /// A viewer card's "Only chat seen while Local Stream Kit was running".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub chat_log: bool,
    /// A viewer card's "The Twitch login is for …, not …".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub card_other_channel: String,
    /// Status → Twitch's "Chat is set to …, but channel points come from …".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub login_other_channel: String,
}

impl ClosedNotes {
    /// How a login and a chat channel are kept: `"marrtin/vedal987"`.
    pub fn pair(login: &str, channel: &str) -> String {
        format!("{}/{}", login.to_lowercase(), channel.to_lowercase())
    }
}

/// What the log keeps (in the console and `logs/`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    /// Debug lines too: every signal, Twitch and OBS message and plugin
    /// debug line. Applied while running; `RUST_LOG`, when set, decides
    /// instead.
    pub detailed: bool,
}

/// What the chat log keeps: every chat message the app sees, in `chat/`,
/// for the viewers' cards in Activity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatLogSettings {
    /// Days of chat kept; older days are deleted. No setting in the panel
    /// yet: only in `lsk.json`.
    pub keep_days: u32,
}

impl Default for ChatLogSettings {
    fn default() -> Self {
        ChatLogSettings { keep_days: 90 }
    }
}

/// Whether the app looks for a new version (Status → About).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Reads the newest release's `latest.json` from GitHub at start and
    /// every 12 hours (`crate::update`). Off, the app contacts no one
    /// about updates.
    pub check: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        UpdateSettings { check: true }
    }
}

/// The overlay a link without `?overlay=` opens.
pub const DEFAULT_OVERLAY: &str = "default";

/// One overlay: an OBS Browser Source's link names it by id
/// (`/?overlay=chat`; the default one's is the plain link), and the app
/// sends it the pages it shows, so changing them needs nothing in OBS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    /// Made from the name when it's added and kept when it's renamed, so
    /// links stay as they are. Lowercase letters, digits and `_`.
    pub id: String,
    pub name: String,
    /// The plugins whose pages it shows; `None` = every one but those
    /// with a Browser Source of their own (the chat display), plugins
    /// added later too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<String>>,
}

impl Overlay {
    /// What a fresh config has: the default overlay with every effect, and
    /// one for the chat display.
    pub fn starters() -> Vec<Overlay> {
        vec![
            Overlay { id: DEFAULT_OVERLAY.into(), name: "Default".into(), plugins: None },
            Overlay { id: "chat".into(), name: "Chat".into(), plugins: Some(vec!["chat".into()]) },
        ]
    }

    /// An id for a new overlay called `name` that none of `overlays` has:
    /// the name in lowercase letters, digits and `_` (`overlay` if that
    /// leaves nothing), then `_2`, `_3`, … if it's taken.
    pub fn free_id(name: &str, overlays: &[Overlay]) -> String {
        let mut base = String::new();
        for c in name.trim().chars().flat_map(char::to_lowercase) {
            if c.is_ascii_alphanumeric() {
                base.push(c);
            } else if !base.ends_with('_') && !base.is_empty() {
                base.push('_');
            }
        }
        let base = match base.trim_end_matches('_') {
            "" => "overlay".to_string(),
            trimmed => trimmed.to_string(),
        };
        let taken = |id: &str| overlays.iter().any(|overlay| overlay.id == id);
        std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}_{n}")))
            .find(|id| !taken(id))
            .expect("some number is free")
    }

    /// Its link for OBS on this port. The default overlay's is the plain
    /// one.
    pub fn link(&self, port: u16) -> String {
        if self.id == DEFAULT_OVERLAY {
            format!("http://localhost:{port}/")
        } else {
            format!("http://localhost:{port}/?{OVERLAY_PARAM}={}", crate::encode_segment(&self.id))
        }
    }

    /// Its link to drag onto OBS: OBS makes a Browser Source of a link
    /// dropped on it, named and sized by these parameters (which it takes
    /// out of the link).
    pub fn obs_drop_link(&self, port: u16, (width, height): (u32, u32)) -> String {
        let link = self.link(port);
        let join = if link.contains('?') { '&' } else { '?' };
        format!(
            "{link}{join}layer-name={}&layer-width={width}&layer-height={height}",
            crate::encode_segment(&self.name)
        )
    }
}

/// The overlay page's parameter naming its overlay.
pub const OVERLAY_PARAM: &str = "overlay";

/// What a Browser Source asked for, in its link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayLink {
    /// `?overlay=`.
    pub overlay: Option<String>,
    /// `?modules=`: plugins named by hand.
    pub modules: Option<Vec<String>>,
}

/// What an overlay shows, worked out from its link and the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPages {
    /// The overlay it is (one that may not be on the list); `None` for a
    /// link naming plugins with `?modules=`.
    pub overlay: Option<String>,
    /// The plugins it shows: `None` = every one but those with a Browser
    /// Source of their own.
    pub plugins: Option<Vec<String>>,
}

/// What the Plugins page sets for one plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    /// Off: its nodes do nothing (their cards still run), its page leaves
    /// the overlay and its server code stops.
    pub enabled: bool,
    /// Its plugin-wide settings (the manifest's `settings`), by key.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub settings: Map<String, Value>,
    /// The hosts the user allowed its server code to reach. It only runs
    /// once every host it lists is allowed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// The hosts the user allowed its overlay page to load from (the
    /// manifest's `page_hosts`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub page_hosts: Vec<String>,
    /// What the user allowed its page to hear (the manifest's `hears`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hears: Vec<String>,
    /// The user allowed its server code to moderate the chat (the
    /// manifest's `moderates`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub moderates: bool,
    /// For a plugin with presets (the manifest's `presets`): its settings
    /// as named sets, instead of `settings`; none yet = one, "Default".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        PluginConfig {
            enabled: true,
            settings: Map::new(),
            hosts: Vec::new(),
            page_hosts: Vec::new(),
            hears: Vec::new(),
            moderates: false,
            presets: Vec::new(),
        }
    }
}

/// One named set of a plugin's own settings (an alert style). The first is
/// the default: nodes that pick none, or one that's gone, get it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// Stable, unique among the plugin's presets.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub settings: Map<String, Value>,
}

/// The preset a plugin with presets starts with.
pub const DEFAULT_PRESET: &str = "default";

impl Preset {
    pub fn first() -> Preset {
        Preset { id: DEFAULT_PRESET.into(), name: "Default".into(), settings: Map::new() }
    }
}

impl PluginConfig {
    /// Its presets: the stored ones, or the one it starts with.
    pub fn presets(&self) -> Vec<Preset> {
        if self.presets.is_empty() { vec![Preset::first()] } else { self.presets.clone() }
    }

    /// The preset with this id; the first for an empty or unknown one.
    pub fn preset(&self, id: &str) -> Preset {
        let presets = self.presets();
        presets.iter().find(|preset| preset.id == id).cloned().unwrap_or_else(|| presets[0].clone())
    }

    /// The presets to change: the one it starts with, stored first.
    pub fn presets_mut(&mut self) -> &mut Vec<Preset> {
        if self.presets.is_empty() {
            self.presets.push(Preset::first());
        }
        &mut self.presets
    }

    /// The settings its overlay page and server code get as the plugin's:
    /// with presets, the first one's.
    pub fn shared_settings(&self) -> &Map<String, Value> {
        self.presets.first().map_or(&self.settings, |preset| &preset.settings)
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            version: CONFIG_VERSION,
            connections: Connections::default(),
            effects: Effects::default(),
            // None: New card's presets are where a streamer starts from.
            cards: Vec::new(),
            history: HistoryFilter::default(),
            plugins: BTreeMap::new(),
            text_styles: TextStyle::starters(),
            text_styles_enabled: true,
            test: TestTexts::default(),
            overlays: Overlay::starters(),
            log: LogSettings::default(),
            updates: UpdateSettings::default(),
            chat_log: ChatLogSettings::default(),
            closed_notes: ClosedNotes::default(),
        }
    }
}

/// Who the Test page's chat message comes from, until another name is typed.
pub const TEST_VIEWER: &str = "TestViewer";
/// The Test page's chat message (7TV's global emotes, which most channels
/// have), until another is typed.
pub const TEST_MESSAGE: &str = "peepoHappy PETPET AYAYA RainTime";
/// What a test channel point redemption's viewer writes, until another
/// text is typed.
pub const TEST_REWARD_TEXT: &str = "Stay hydrated!";
/// What a test sub or cheer comes with (the Events' Message), until
/// another is typed.
pub const TEST_EVENT_MESSAGE: &str = "Love the streams! peepoHappy";

/// The Test page's texts, kept as typed there (2026-10-14, asked): none
/// until its field is changed, showing its starting text
/// ([`TEST_VIEWER`], [`TEST_MESSAGE`], [`TEST_REWARD_TEXT`],
/// [`TEST_EVENT_MESSAGE`]); emptied, it stays empty (2026-10-15, asked).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestTexts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reward_text: Option<String>,
    /// The Events' Message, sent with Sub and Bits (2026-10-15, asked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_message: Option<String>,
}

impl TestTexts {
    /// Whether none was changed yet.
    pub fn is_empty(&self) -> bool {
        self.viewer.is_none()
            && self.message.is_none()
            && self.reward_text.is_none()
            && self.event_message.is_none()
    }

    pub fn viewer(&self) -> &str {
        self.viewer.as_deref().unwrap_or(TEST_VIEWER)
    }

    pub fn message(&self) -> &str {
        self.message.as_deref().unwrap_or(TEST_MESSAGE)
    }

    pub fn reward_text(&self) -> &str {
        self.reward_text.as_deref().unwrap_or(TEST_REWARD_TEXT)
    }

    pub fn event_message(&self) -> &str {
        self.event_message.as_deref().unwrap_or(TEST_EVENT_MESSAGE)
    }
}

/// Connection settings. All but the Twitch channel only take effect after
/// a restart (the chat follows the channel while running).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connections {
    /// The overlay and panel's port on localhost.
    pub overlay_port: u16,
    /// Third-party emote sources, in precedence order.
    pub emote_sources: Vec<String>,
    pub image_cache_max_mb: u64,
    /// The Twitch channel whose chat is read; empty = none. (Channel
    /// points, follows and hype trains come with the broadcaster's login,
    /// on the Status page; `channel_points`, their switch until 2026-10-14,
    /// is left out of an old config when it's read.)
    pub twitch_channel: String,
    /// What the broadcaster's Twitch login may do (Status → Twitch's
    /// ticks, 2026-10-16): Twitch is asked for these when logging in.
    pub twitch_permissions: TwitchPermissions,
}

/// What the broadcaster's Twitch login may do: the ticks in Status →
/// Twitch. Logging in asks Twitch for these; one ticked that the login
/// wasn't given needs a new login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TwitchPermissions {
    pub channel_points: bool,
    pub follows: bool,
    pub hype_train: bool,
    /// Timeouts, bans, warnings, deleting messages and shoutouts from the
    /// viewers' cards, and who's subbed, banned, VIP or a mod on them.
    pub moderating: bool,
    /// Making viewers VIPs and mods, and taking it back.
    pub roles: bool,
}

impl Default for TwitchPermissions {
    /// The events on; moderating ticked by those who want it.
    fn default() -> Self {
        TwitchPermissions {
            channel_points: true,
            follows: true,
            hype_train: true,
            moderating: false,
            roles: false,
        }
    }
}

impl TwitchPermissions {
    /// What both have.
    pub fn and(self, other: TwitchPermissions) -> TwitchPermissions {
        TwitchPermissions {
            channel_points: self.channel_points && other.channel_points,
            follows: self.follows && other.follows,
            hype_train: self.hype_train && other.hype_train,
            moderating: self.moderating && other.moderating,
            roles: self.roles && other.roles,
        }
    }

    /// Whether any of these is in `self` but not in `granted`: a new login
    /// would give it.
    pub fn beyond(self, granted: TwitchPermissions) -> bool {
        self.and(granted) != self
    }

    /// Whether any of the events (channel points, follows, hype trains) is
    /// on: they're what EventSub is for.
    pub fn any_events(self) -> bool {
        self.channel_points || self.follows || self.hype_train
    }

    /// All of them, as a login with every permission has.
    pub fn all() -> TwitchPermissions {
        TwitchPermissions {
            channel_points: true,
            follows: true,
            hype_train: true,
            moderating: true,
            roles: true,
        }
    }
}

impl Default for Connections {
    fn default() -> Self {
        Connections {
            overlay_port: 5750,
            emote_sources: EMOTE_SOURCES.iter().map(|source| source.to_string()).collect(),
            image_cache_max_mb: 512,
            twitch_channel: String::new(),
            twitch_permissions: TwitchPermissions::default(),
        }
    }
}

/// Global settings for each effect; every card's effect nodes use them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    /// The master volume and mute. (Sound nodes are the `sound` plugin's,
    /// switched on the Plugins page.)
    pub sound: SoundSettings,
    /// How texts are read aloud. (Switched with the Speak plugin, on the
    /// Plugins page.)
    pub speech: SpeechConfig,
}

impl AppConfig {
    /// Attaches the cards' plugin nodes' types from the installed plugins
    /// (they aren't stored): needed after loading, before the cards run or
    /// are drawn.
    /// A plugin's settings from the Plugins page (the defaults if none).
    pub fn plugin(&self, id: &str) -> PluginConfig {
        self.plugins.get(id).cloned().unwrap_or_default()
    }

    /// The plugins switched off.
    pub fn plugins_off(&self) -> BTreeSet<String> {
        self.plugins.iter().filter(|(_, plugin)| !plugin.enabled).map(|(id, _)| id.clone()).collect()
    }

    /// A plugin's switch, to change it.
    pub fn plugin_enabled_mut(&mut self, id: &str) -> &mut bool {
        &mut self.plugins.entry(id.to_string()).or_default().enabled
    }

    /// What a Browser Source with this link shows. `?overlay=` (or the
    /// plain link: the default overlay) shows that overlay's pages, and
    /// nothing if it isn't on the list; `?modules=` alone, the plugins it
    /// names.
    pub fn overlay_pages(&self, link: &OverlayLink) -> OverlayPages {
        if let (None, Some(modules)) = (&link.overlay, &link.modules) {
            return OverlayPages { overlay: None, plugins: Some(modules.clone()) };
        }
        let id = link.overlay.clone().unwrap_or_else(|| DEFAULT_OVERLAY.to_string());
        let plugins = match self.overlays.iter().find(|overlay| overlay.id == id) {
            Some(overlay) => overlay.plugins.clone(),
            None => Some(Vec::new()),
        };
        OverlayPages { overlay: Some(id), plugins }
    }

    pub fn attach_plugins(&mut self, catalog: &Catalog) {
        for card in &mut self.cards {
            card.attach_plugins(catalog);
        }
    }

    /// Settings that can't work, as messages for a person. Card problems are
    /// separate (`Card::problems`): a card with problems just doesn't run.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for source in &self.connections.emote_sources {
            if !EMOTE_SOURCES.contains(&source.as_str()) {
                problems
                    .push(format!("unknown emote source {source:?} (known: {})", EMOTE_SOURCES.join(", ")));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        for card in &self.cards {
            if !ids.insert(card.id.as_str()) {
                problems.push(format!("two cards have the id {:?}", card.id));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        for overlay in &self.overlays {
            if !ids.insert(overlay.id.as_str()) {
                problems.push(format!("two overlays have the id {:?}", overlay.id));
            }
        }
        problems
    }

    /// Connection settings in `new` that differ from these and need a
    /// restart, by name.
    pub fn needs_restart_for(&self, new: &AppConfig) -> Vec<&'static str> {
        let (old, new) = (&self.connections, &new.connections);
        let mut changed = Vec::new();
        if old.overlay_port != new.overlay_port {
            changed.push("overlay port");
        }
        if old.emote_sources != new.emote_sources {
            changed.push("emote sources");
        }
        if old.image_cache_max_mb != new.image_cache_max_mb {
            changed.push("image cache size");
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let config = AppConfig::default();
        assert_eq!(config.problems(), Vec::<String>::new());
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert_eq!(serde_json::from_str::<AppConfig>(&json).unwrap(), config);
        // Speech is flat: the settings, and no switch of its own (the
        // Speak plugin's is it).
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["effects"]["speech"].get("enabled").is_none());
        assert!(value["effects"]["speech"]["voice"].is_string());
    }

    #[test]
    fn the_old_channel_points_switch_is_left_out() {
        // Logging in is the switch since 2026-10-14.
        let config: AppConfig =
            serde_json::from_str(r#"{"connections": {"twitch_channel": "abc", "channel_points": true}}"#)
                .unwrap();
        assert_eq!(config.connections.twitch_channel, "abc");
        let saved = serde_json::to_value(&config).unwrap();
        assert!(saved["connections"].get("channel_points").is_none());
    }

    #[test]
    fn missing_keys_use_defaults() {
        let config: AppConfig =
            serde_json::from_str(r#"{"connections": {"twitch_channel": "abc"}}"#).unwrap();
        assert_eq!(config.connections.twitch_channel, "abc");
        assert_eq!(config.connections.overlay_port, 5750);
        assert!(config.cards.is_empty(), "a fresh config has no cards");
        assert!(config.updates.check, "updates are looked for unless switched off");
    }

    #[test]
    fn restart_and_problems() {
        let old = AppConfig::default();
        let mut new = AppConfig::default();
        new.connections.overlay_port = 3001;
        new.effects.speech.settings.speed = 1.5;
        new.connections.twitch_channel = "someone".into();
        assert_eq!(old.needs_restart_for(&new), ["overlay port"], "the channel switches while running");
        new.connections.emote_sources.push("nope".into());
        assert_eq!(new.problems().len(), 1);
    }

    #[test]
    fn overlays() {
        let config = AppConfig::default();
        let link = |overlay: Option<&str>, modules: Option<&[&str]>| OverlayLink {
            overlay: overlay.map(String::from),
            modules: modules.map(|modules| modules.iter().map(|m| m.to_string()).collect()),
        };
        let pages = |overlay: Option<&str>, plugins: Option<&[&str]>| OverlayPages {
            overlay: overlay.map(String::from),
            plugins: plugins.map(|plugins| plugins.iter().map(|p| p.to_string()).collect()),
        };
        // The plain link is the default overlay: every effect.
        assert_eq!(config.overlay_pages(&link(None, None)), pages(Some("default"), None));
        assert_eq!(config.overlay_pages(&link(Some("chat"), None)), pages(Some("chat"), Some(&["chat"])));
        // One that isn't on the list shows nothing; ?modules= alone, what it names.
        assert_eq!(
            config.overlay_pages(&link(Some("gone"), Some(&["rain"]))),
            pages(Some("gone"), Some(&[]))
        );
        assert_eq!(config.overlay_pages(&link(None, Some(&["rain"]))), pages(None, Some(&["rain"])));

        let overlays = Overlay::starters();
        assert_eq!(Overlay::free_id("Chat", &overlays), "chat_2");
        assert_eq!(Overlay::free_id(" Big alerts! ", &overlays), "big_alerts");
        assert_eq!(Overlay::free_id("??", &overlays), "overlay");
        assert_eq!(overlays[0].link(3000), "http://localhost:3000/");
        assert_eq!(overlays[1].link(3000), "http://localhost:3000/?overlay=chat");
        assert_eq!(
            overlays[1].obs_drop_link(3000, (500, 700)),
            "http://localhost:3000/?overlay=chat&layer-name=Chat&layer-width=500&layer-height=700"
        );

        let mut twice = AppConfig::default();
        twice.overlays.push(Overlay { id: "chat".into(), name: "Again".into(), plugins: None });
        assert_eq!(twice.problems(), ["two overlays have the id \"chat\""]);
        // A config from before overlays gets the starters.
        let old: AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(old.overlays, Overlay::starters());
    }
}
