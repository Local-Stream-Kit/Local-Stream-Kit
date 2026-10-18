//! Plugins: a manifest (`plugin.json` in the plugin's folder) that adds node
//! types to the cards, and what a plugin's overlay page is sent. See
//! `docs/writing-a-plugin.md`.
//!
//! A card stores a plugin node as [`NodeKind::Plugin`](crate::cards::NodeKind::Plugin)
//! with its settings only; its type (sockets, title, fields) is attached
//! after loading from the installed plugins ([`Catalog`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cards::{Category, Socket, SocketType};
use crate::encode_segment;
use crate::overrides::Unit;

/// The plugin API version this app speaks: a manifest's `api`.
pub const API_VERSION: u32 = 1;

/// The built-in Rain plugin (the emote rain, once an overlay module) and
/// its node.
pub const RAIN_PLUGIN: &str = "rain";
pub const RAIN_NODE: &str = "rain";

/// The built-in Alerts plugin (once the app's Alert node and the overlay's
/// alert module) and its node.
/// The built-in Chat display plugin, and its Show in chat node.
pub const CHAT_PLUGIN: &str = "chat";
pub const CHAT_NODE: &str = "show";

pub const ALERT_PLUGIN: &str = "alert";
pub const ALERT_NODE: &str = "alert";

/// The built-in Speak plugin (once the app's Speak node) and its node.
pub const SPEAK_PLUGIN: &str = "speak";
pub const SPEAK_NODE: &str = "speak";

/// The built-in Hype train bar plugin and its node.
pub const HYPE_TRAIN_PLUGIN: &str = "hype-train";
pub const HYPE_TRAIN_BAR_NODE: &str = "bar";

/// The manifest, in a plugin's folder.
pub const MANIFEST_FILE: &str = "plugin.json";

/// The folder in a plugin's folder that is served at [`files_path`]; a
/// plugin with `overlay/index.html` has an overlay page.
pub const OVERLAY_DIR: &str = "overlay";
/// The built-in OBS plugin's id: its nodes are marked with OBS's logo, as
/// Twitch's events are with Twitch's.
pub const OBS_PLUGIN: &str = "obs";

/// A plugin's server code, in its folder: a WebAssembly component for the
/// `plugin` world in `wit/plugin.wit`.
pub const SERVER_FILE: &str = "server.wasm";

/// A plugin's nodes' icon ([`Manifest::icon`]), in axum path syntax. See
/// [`icon_path`].
pub const ICON_ROUTE: &str = "/plugin-icons/{plugin}";

/// Where a plugin's icon is served, for the panel to draw as a mask.
pub fn icon_path(plugin: &str) -> String {
    format!("/plugin-icons/{}", encode_segment(plugin))
}

/// The largest icon, in bytes.
pub const MAX_ICON_BYTES: usize = 16 * 1024;

/// A plugin's overlay files, in axum path syntax. See [`files_path`].
pub const FILES_ROUTE: &str = "/plugins/{plugin}/{*path}";

/// Where a plugin's overlay files are served: `/plugins/{plugin}/`.
pub fn files_path(plugin: &str) -> String {
    format!("/plugins/{}/", encode_segment(plugin))
}

/// A plugin's overlay page, loaded into its iframe.
pub fn page_path(plugin: &str) -> String {
    format!("{}index.html", files_path(plugin))
}

/// Names a plugin's id can't take: the shell's, and the overlay modules'
/// that built-in plugins took over (so `?modules=` keeps naming them).
const RESERVED_IDS: [&str; 6] = ["rain", "alert", "sound", "chat", "shell", "plugin"];

/// `plugin.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Lowercase letters, digits and `-`; the plugin folder's name.
    pub id: String,
    pub name: String,
    pub version: String,
    /// The plugin API version it was written for ([`API_VERSION`]).
    pub api: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub nodes: Vec<NodeType>,
    /// The hosts its server code may send web requests to and open
    /// WebSockets with: `discord.com`, or `*.example.com` for any
    /// subdomain. Nothing else is reachable, and the code only runs once
    /// the user allowed them ([`server_hosts_allowed`]; a built-in
    /// plugin's come with the app).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// The hosts its overlay page may load from besides the app (fonts,
    /// say), over https, as [`Manifest::hosts`] are written. Only once the
    /// user allows them ([`allowed_page_hosts`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_hosts: Vec<String>,
    /// What it hears without a card: [`HEARS_CHAT`] (every chat message,
    /// and moderators taking some off; its page and its server code or
    /// program) and [`HEARS_EVENTS`] (every event; its server code or
    /// program). Only once the user allows it ([`allowed_hears`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hears: Vec<String>,
    /// Settings for the whole plugin, set on the panel's Plugins page (an
    /// API key, a channel…): its server code reads them (`host.settings`),
    /// its overlay page gets them (`{ "type": "settings", "values": … }`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Setting>,
    /// Text to speech voices it offers: the app asks its server code (the
    /// `speak` export) or its program (a `speak` message) for messages in
    /// them. In the voice pickers as [`plugin_voice`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<VoiceSpec>,
    /// Its own `settings` as named sets the user makes on its Plugins box
    /// (alert styles): each node picks one with a `preset` setting, and
    /// its runs bring that set's settings with the node's changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<PresetsSpec>,
    /// A preview on its Plugins box: its overlay page at 16:9, run as one
    /// of its effect nodes with sample values, with its settings (the
    /// picked preset's) as they're edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewSpec>,
    /// Its page goes on a Browser Source of its own (the chat display): an
    /// overlay without `?modules=` doesn't host it, only one that names it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_source: bool,
    /// A page of help (one of its page files: a path under `overlay/`),
    /// shown on its Plugins box under "Help", without scripts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub help: String,
    /// Its nodes' icon, drawn small in the text's colour beside their
    /// titles, as Twitch's and OBS's are: an SVG of one colour, 24 × 24. A
    /// folder plugin names a file in its folder (`icon.svg`); a program
    /// gives the SVG itself (`<svg …>…</svg>`). Served at [`icon_path`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// A tab of its own in the panel, after Activity: one of its page
    /// files, talking to its server code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<TabSpec>,
}

/// A plugin's tab in the panel ([`Manifest::tab`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabSpec {
    /// The tab's name, at most [`MAX_TAB_LABEL_CHARS`].
    pub label: String,
    /// Its page: one of its page files (a path under `overlay/`), served
    /// at [`tab_path`] and shown in a sandboxed iframe.
    pub page: String,
    /// Its server code puts a number on the tab (`host.set-tab-count`), so
    /// the tab row keeps room for one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub count: bool,
}

/// The longest tab label, in characters: the panel's tab row is narrow.
pub const MAX_TAB_LABEL_CHARS: usize = 16;

/// Where a plugin's tab page is served.
pub fn tab_path(plugin: &str, tab: &TabSpec) -> String {
    format!("{}{}", files_path(plugin), tab.page)
}

/// A stylesheet with the panel's look (its colours as CSS variables, plain
/// buttons and fields) for plugins' tab pages, served by the app.
pub const TAB_STYLE_PATH: &str = "/plugin-tab.css";

/// The most a tab page sends its server code at once
/// ([`TabPageMessage::Send`]), in bytes of JSON.
pub const TAB_MESSAGE_LIMIT: usize = 256 * 1024;

/// What a plugin's tab page tells the panel (with `postMessage` to
/// `window.parent`). It gets [`PluginMessage::Settings`] when it loads and
/// when they change, and [`PluginMessage::Message`] from its server code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TabPageMessage {
    /// For its server code (`on-tab`), any JSON.
    Send { data: Value },
}

impl Manifest {
    /// The icon's SVG when the manifest holds it (a program's), not a
    /// file's name.
    pub fn icon_markup(&self) -> Option<&str> {
        let icon = self.icon.trim();
        icon.starts_with("<svg").then_some(icon)
    }
}

/// A plugin's preview ([`Manifest::preview`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewSpec {
    /// The effect node it runs; none with `samples`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node: String,
    /// Sample values by input or setting name; the rest are the node's
    /// defaults. A text is a plain string here.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub values: Map<String, Value>,
    /// The page's size in OBS, which the preview shows scaled down: 1920
    /// × 1080 unless given (500 × 700 for the chat display).
    #[serde(default = "stage_width")]
    pub width: u32,
    #[serde(default = "stage_height")]
    pub height: u32,
    /// Only the bottom of the page this tall shows in the preview, for a
    /// page that fills from the bottom (the chat display: 400 of its 700);
    /// none = all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown_height: Option<u32>,
    /// The page draws samples of its own ([`PluginMessage::Samples`]) when
    /// it loads and for Play, instead of a run of `node`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub samples: bool,
}

fn stage_width() -> u32 {
    1920
}

fn stage_height() -> u32 {
    1080
}

/// A plain text as a text value (no emotes).
pub fn plain_text_value(text: &str) -> Value {
    serde_json::to_value(TextValue {
        text: text.to_string(),
        parts: vec![TextPart::Text { text: text.to_string(), style: None }],
        speech: None,
    })
    .expect("text values serialise")
}

impl NodeType {
    /// The values a preview's run brings: each input but events and each
    /// of the node's settings, from the sample or the default, as a run's
    /// values are (texts as text values, pictures and sounds as paths).
    pub fn preview_values(&self, samples: &Map<String, Value>) -> Map<String, Value> {
        let mut values = Map::new();
        for input in self.inputs.iter().filter(|input| input.ty != SocketType::Event) {
            let setting = self.setting(&input.name);
            let value = samples
                .get(&input.name)
                .cloned()
                .or_else(|| setting.map(Setting::default_value))
                .unwrap_or(Value::Null);
            let value = match (input.ty, &value) {
                (SocketType::Text, Value::String(text)) => plain_text_value(text),
                (SocketType::Emotes, Value::Array(emotes)) if emotes.iter().all(Value::is_object) => value,
                (SocketType::Emotes, _) => Value::Array(Vec::new()),
                _ => value,
            };
            values.insert(input.name.clone(), value);
        }
        for setting in self.own_settings() {
            let value = match (&setting.kind, samples.get(&setting.key)) {
                (SettingKind::Text { .. }, sample) => {
                    let text = setting.read(sample);
                    plain_text_value(text.as_str().unwrap_or_default())
                }
                (_, sample) => shared_value(setting, sample, &mut pick_nothing),
            };
            values.insert(setting.key.clone(), value);
        }
        values
    }
}

/// A plugin's named sets of settings ([`Manifest::presets`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetsSpec {
    /// What one is called: "Style".
    pub label: String,
}

/// One of a plugin's voices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceSpec {
    /// Lowercase letters, digits, `-` and `_`; unique in the plugin.
    pub id: String,
    pub name: String,
}

/// A plugin voice's id where voices are chosen (the speech settings, a
/// node's voice changes): `<plugin>:<voice>`. The app's own voices have
/// no `:`.
pub fn plugin_voice(plugin: &str, voice: &str) -> String {
    format!("{plugin}:{voice}")
}

/// A plugin voice's plugin and voice ids; `None` for one of the app's.
pub fn split_voice(voice: &str) -> Option<(&str, &str)> {
    voice.split_once(':')
}

/// [`Manifest::hears`]: every chat message, as [`PluginMessage::Chat`],
/// and moderators taking some off ([`PluginMessage::ChatRemoved`],
/// [`PluginMessage::ChatUserRemoved`], [`PluginMessage::ChatCleared`]);
/// its page and its server code (`on-chat`) or program all hear it.
pub const HEARS_CHAT: &str = "chat";

/// [`Manifest::hears`]: every event (cheers, subs, raids…, tests and
/// replays too, but plugins' own), for its server code (`on-event`) or
/// program (`heard_event`).
pub const HEARS_EVENTS: &str = "events";

/// What a plugin can hear ([`Manifest::hears`]).
const HEARABLE: [&str; 2] = [HEARS_CHAT, HEARS_EVENTS];

/// What a plugin hears now: a built-in plugin's `hears` (it comes with
/// the app), else what the user allowed.
pub fn allowed_hears(info: &PluginInfo, allowed: &[String]) -> Vec<String> {
    info.manifest.hears.iter().filter(|what| info.bundled || allowed.contains(what)).cloned().collect()
}

/// The hosts a plugin's page may load from now: a built-in plugin's
/// `page_hosts` (they come with the app), else the ones the user allowed.
pub fn allowed_page_hosts(info: &PluginInfo, allowed: &[String]) -> Vec<String> {
    info.manifest.page_hosts.iter().filter(|host| info.bundled || allowed.contains(host)).cloned().collect()
}

/// Whether a plugin's server code may reach its `hosts` now: a built-in
/// plugin's always (they come with the app), else once the user allowed
/// every one.
pub fn server_hosts_allowed(info: &PluginInfo, allowed: &[String]) -> bool {
    info.bundled || info.manifest.hosts.iter().all(|host| allowed.contains(host))
}

/// Whether `host` is one of `allowed` (see [`Manifest::hosts`]).
pub fn host_allowed(allowed: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    allowed.iter().any(|pattern| match pattern.strip_prefix("*.") {
        Some(domain) => host.strip_suffix(domain).is_some_and(|sub| sub.ends_with('.') && sub.len() > 1),
        None => host == *pattern,
    })
}

fn is_host_pattern(pattern: &str) -> bool {
    let host = pattern.strip_prefix("*.").unwrap_or(pattern);
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// One node type a plugin adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeType {
    /// Unique within the plugin.
    pub id: String,
    pub title: String,
    pub category: Category,
    #[serde(default)]
    pub inputs: Vec<SocketSpec>,
    #[serde(default)]
    pub outputs: Vec<SocketSpec>,
    /// A setting with the same key as a number or text input is that
    /// input's value while nothing is wired into it.
    #[serde(default)]
    pub settings: Vec<Setting>,
    /// Whether an effect node's runs wait their turn in the queue.
    #[serde(default, skip_serializing_if = "QueueMode::is_never")]
    pub queue: QueueMode,
    /// The plugin's own settings (by key) a node of this type can change
    /// for itself ("Change a setting"); the run gets them as `changes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
    /// Those settings, filled in from the plugin's when the catalog is
    /// made ([`Catalog::new`]).
    #[serde(skip)]
    pub changeable: Vec<Setting>,
    /// For a plugin with presets: all its own settings (filled in like
    /// `changeable`), which its runs bring.
    #[serde(skip)]
    pub preset_settings: Vec<Setting>,
    /// The input or setting whose value says what a run is, for people:
    /// what Activity shows for it (an alert's message). None: its title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub describe: Option<String>,
    /// An effect node whose runs bring the viewer who set it off, as
    /// [`VIEWER_VALUE`] ([`RunViewer`], `null` when the event has none).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub viewer: bool,
}

/// The value a node with [`NodeType::viewer`] gets the event's viewer in.
pub const VIEWER_VALUE: &str = "viewer";

/// The viewer who set a run off ([`NodeType::viewer`]): as on the chat
/// display, with the platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunViewer {
    pub platform: String,
    #[serde(flatten)]
    pub user: crate::chat::ChatUser,
}

/// The tick box an effect node with [`QueueMode::Optional`] needs: a bool
/// setting with this key.
pub const QUEUE_SETTING: &str = "queue";

/// The event output an effect node has while it waits in the queue: fired
/// when its run goes on stage (again if it plays again from the start).
pub const STARTED_OUTPUT: &str = "started";
/// The other: fired when its run has ended (not when it's skipped).
pub const ENDED_OUTPUT: &str = "ended";

/// How long a queued plugin effect may take: the queue moves on after it
/// even if the page never says it's done.
pub const QUEUE_LIMIT_MS: u64 = 120_000;

/// Whether an effect node's runs wait in the queue. A queued run goes to
/// the page when its turn comes, with a `queue_id`, and the page answers
/// `{ "type": "done", "queue_id" }` when it has finished.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMode {
    /// Right away, always.
    #[default]
    Never,
    /// Always in the queue.
    Always,
    /// The node's bool setting [`QUEUE_SETTING`] decides.
    Optional,
}

impl QueueMode {
    fn is_never(&self) -> bool {
        *self == QueueMode::Never
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SocketSpec {
    /// Lowercase letters, digits and `_`.
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub ty: SocketType,
    /// An effect node's text input the app reads aloud (text to speech):
    /// its value comes with the spoken message (`TextValue::speech`).
    /// One per node; the node gets "+ Change a voice setting".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub speak: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    /// Lowercase letters, digits and `_`.
    pub key: String,
    pub label: String,
    /// A picture, media or sound setting that must be picked: a node with
    /// none has a problem ("Pick a sound."), so its card doesn't run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// A node's setting shown (and sent with its runs) only while one of
    /// its inputs is wired, or isn't; always without.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<When>,
    /// A plugin's own setting's section on the Plugins page: "Look",
    /// "Physics"…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A plugin's own setting's label on the Plugins page, shorter, where
    /// its group's heading says the rest ("Size" under "Text"); nodes show
    /// `label` ("Text size").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub short: String,
    /// A plugin's own tick box's or number's words after its field on the
    /// Plugins page ("As pictures", its label being "Emotes"; "messages at
    /// most").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caption: String,
    /// A plugin's own tick box drawn on the Plugins page as a small button,
    /// pressed while it's on, beside another of its settings' field, by
    /// key (Bold beside Font).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub beside: String,
    /// That button's face: one of [`TOGGLE_ICONS`]; none = its label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(flatten)]
    pub kind: SettingKind,
}

/// The faces a tick box [`Setting::beside`] another can have: **B** and
/// *I*.
pub const TOGGLE_ICONS: [&str; 2] = ["bold", "italic"];

/// When a setting is shown: a node's, `{ "wired": "emotes" }` or
/// `{ "unwired": "emotes" }`, by input name, or `{ "is": { "shape":
/// "picture" } }`, while its other choice settings have those values; a
/// plugin's own on the Plugins page, `{ "set": "picture" }`, while that
/// other setting is set ([`is_set`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    Wired(String),
    Unwired(String),
    Is(BTreeMap<String, String>),
    Set(String),
}

impl When {
    /// The input it names, if it's about one.
    pub fn input(&self) -> Option<&str> {
        match self {
            When::Wired(name) | When::Unwired(name) => Some(name),
            When::Is(_) | When::Set(_) => None,
        }
    }
}

/// Whether a setting's value (as [`Setting::read`] gives it) counts as set
/// for [`When::Set`]: a text, picture or sound that isn't empty, a tick box
/// that's on, a list with something in it; a number, colour or choice
/// always.
pub fn is_set(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(on) => *on,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Number(_) | Value::Object(_) => true,
    }
}

/// An emotes setting's value for random emotes from the channel; else it's
/// a list of emote codes.
pub const RANDOM_EMOTES: &str = "random";

/// The most emotes an emotes setting gives (its count is clamped to it).
pub const MAX_SETTING_EMOTES: u32 = 500;

/// Whether a stored value fits an emotes setting: `"random"`, or a list of
/// codes.
/// What an event node's filter setting lets through (`stored`, as
/// [`Setting::read`] gives it): its values, trimmed, without empty ones;
/// none = everything.
pub fn filter_values(stored: &Value) -> Vec<String> {
    let values = match stored {
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        Value::String(text) => vec![text.as_str()],
        _ => Vec::new(),
    };
    values.into_iter().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string).collect()
}

/// Whether an event's `value` passes a filter's values (`filter_values`):
/// any of them, ignoring case; none lets everything through.
pub fn passes_filter(wanted: &[String], value: &str) -> bool {
    let value = value.trim().to_lowercase();
    wanted.is_empty() || wanted.iter().any(|wanted| wanted.to_lowercase() == value)
}

pub fn is_emotes_value(value: &Value) -> bool {
    value.as_str() == Some(RANDOM_EMOTES)
        || value.as_array().is_some_and(|codes| codes.iter().all(Value::is_string))
}

/// A setting's type, with its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingKind {
    Number {
        #[serde(default)]
        default: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// How it's shown and typed: `px`, `percent` (stored 0–1, shown
        /// 0–100 %), `ms` (stored in ms, shown in s), `metres_per_second_squared`.
        #[serde(default, skip_serializing_if = "Unit::is_plain")]
        unit: Unit,
        /// A slider's step, as stored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        /// Shown as a slider (needs `min` and `max`) where there's room: a
        /// plugin's own settings on the Plugins page.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        slider: bool,
    },
    /// A template: `{viewer}` and the like are filled from the event,
    /// `{e:Kappa}` is that emote.
    Text {
        #[serde(default)]
        default: String,
        /// Offered while typing (font names, say); anything else goes too.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggestions: Vec<String>,
        /// Shown while it's empty ("The system's").
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
        /// Offered too: a list of choices its server code fills in
        /// (`host.set-options`), by name ("scenes"). `{key}` in the name is
        /// another of the node's (or the plugin's) settings' value, so a
        /// list can follow it: `"sources/{scene}"`. See [`options_list`].
        #[serde(default, skip_serializing_if = "String::is_empty")]
        suggest_from: String,
        /// Several values (a list of texts), not one: an event node's
        /// filters, picked one by one (see [`filter_values`]), or a plugin's
        /// own setting, a line each on its box. Its `default` is the items
        /// separated by commas.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        many: bool,
    },
    /// Where on the screen: a [`Position`] (one of [`POSITIONS`]' names, or
    /// x and y in %), picked on a small screen (2026-10-15). The default is
    /// a name.
    Position {
        #[serde(default = "top")]
        default: String,
        /// "Anywhere (random)" is a choice too: the value
        /// [`RANDOM_POSITION`] (Show media's Where).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        random: bool,
    },
    /// A tick box.
    Bool {
        #[serde(default)]
        default: bool,
    },
    /// `#rrggbb`.
    Colour {
        #[serde(default = "white")]
        default: String,
    },
    Choice {
        /// One of the options' values; none = the first option.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        options: Vec<Choice>,
    },
    /// A picture in the pictures folder; empty = none.
    Picture {
        #[serde(default)]
        default: String,
        /// Sent as a list of paths, not one: a file is a list of one, a
        /// folder every one of its files (see [`shared_value`]).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        many: bool,
    },
    /// A picture or a video (`.webm`, `.mp4`) in the pictures folder;
    /// empty = none.
    Media {
        #[serde(default)]
        default: String,
        /// As a picture's.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        many: bool,
    },
    /// A sound in the sounds folder; empty = none.
    Sound {
        #[serde(default)]
        default: String,
        /// As a picture's.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        many: bool,
    },
    /// Stands in for the emotes input with the same key while nothing is
    /// wired into it: `"random"` (random emotes from the channel) or a list
    /// of codes (`["Kappa", "PogU"]`), which the app turns into emotes.
    /// With `count` (a number input or setting of the node), that many are
    /// picked at random from them; else one random emote, or every code
    /// once.
    Emotes {
        #[serde(default = "random_emotes")]
        default: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<String>,
    },
    /// One of the plugin's presets (an alert's style), by id; empty = the
    /// first. Only nodes of a plugin with `presets`, one each.
    Preset {},
    /// A text file (a stylesheet), picked from looks: the plugin's own
    /// (`starts`, which never change) and the user's copies of them, each a
    /// file, `plugin_data/<plugin>/<key>/<name>.<ext>`, edited on the
    /// Plugins page or in any editor (watched, so a save shows at once).
    /// Its value in the config is the picked look's name; empty or a look
    /// that's gone = `default`'s. The page and server code get the picked
    /// look's text (see [`PluginLook`]). A plugin's own settings only.
    File {
        /// The extension the user's copies get ("chat.css": `.css`). Until
        /// 2026-10-14 the one file there was, `plugin_data/<plugin>/<file>`,
        /// which the app now moves to the copies as "Custom".
        file: String,
        /// The page file (a path under `overlay/`) of the look picked while
        /// none is; one of `starts`, or else a look of its own, "Default".
        #[serde(default, skip_serializing_if = "String::is_empty")]
        default: String,
        /// The plugin's own looks: page files with their names ("Bubbles").
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        starts: Vec<Choice>,
    },
    /// A password or key: kept in `lsk_secrets.json` next to the config,
    /// not in the config, and given to server code only (pages and the
    /// panel never get it; the panel only learns whether one is saved).
    /// A plugin's own settings only. Server code gets `""` while none is
    /// saved.
    Secret {
        /// Shown while none is saved.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        placeholder: String,
    },
}

fn random_emotes() -> Value {
    Value::String(RANDOM_EMOTES.into())
}

/// What a picture, media or sound setting picks from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// A picture in the pictures folder.
    Picture,
    /// A picture or a video in the pictures folder.
    Media,
    /// A sound in the sounds folder.
    Sound,
}

impl MediaKind {
    /// Whether a file in its folder (already one of the folder's types) is
    /// one it takes: a picture setting skips videos.
    pub fn takes(self, file: &str) -> bool {
        self != MediaKind::Picture || !crate::alert::is_video(file)
    }
}

impl SettingKind {
    pub fn media(&self) -> Option<MediaKind> {
        match self {
            SettingKind::Picture { .. } => Some(MediaKind::Picture),
            SettingKind::Media { .. } => Some(MediaKind::Media),
            SettingKind::Sound { .. } => Some(MediaKind::Sound),
            _ => None,
        }
    }

    /// A picture, media or sound setting sent as a list of paths.
    pub fn many_files(&self) -> bool {
        matches!(
            self,
            SettingKind::Picture { many: true, .. }
                | SettingKind::Media { many: true, .. }
                | SettingKind::Sound { many: true, .. }
        )
    }
}

/// A picture, media or sound setting's value naming a folder in the
/// pictures (sounds) folder, `"alerts/"`: each time it's sent, one of the
/// folder's files picked at random. `Some("alerts")`.
pub fn media_folder(value: &str) -> Option<&str> {
    value.trim().strip_suffix('/').filter(|folder| !folder.is_empty() && !folder.contains('/'))
}

/// Picks files for a setting naming a folder ([`media_folder`]), as paths
/// in the pictures (sounds) folder, `"alerts/hype.gif"`: one of them, or
/// with `every` (a [`many`](SettingKind::many_files) setting) all of them,
/// in name order. None for an empty or missing folder.
pub type PickFile<'a> = dyn FnMut(MediaKind, &str, bool) -> Vec<String> + 'a;

/// For where nothing is picked (a manifest's samples): a folder is none.
pub fn pick_nothing(_: MediaKind, _: &str, _: bool) -> Vec<String> {
    Vec::new()
}

fn white() -> String {
    "#ffffff".into()
}

fn top() -> String {
    "top".into()
}

/// A position setting's values and their names, in reading order (a 3 × 3
/// grid).
pub const POSITIONS: [(&str, &str); 9] = [
    ("top_left", "Top left"),
    ("top", "Top"),
    ("top_right", "Top right"),
    ("left", "Left"),
    ("center", "Middle"),
    ("right", "Right"),
    ("bottom_left", "Bottom left"),
    ("bottom", "Bottom"),
    ("bottom_right", "Bottom right"),
];

/// A position setting's value for "Anywhere (random)", where it allows it.
pub const RANDOM_POSITION: &str = "random";

/// Where on the screen a thing goes: how far across and down the room it
/// can move in, in % (0 keeps it against the left or top edge, 100 against
/// the right or bottom, 50 centres it), so it never goes off screen, and a
/// bigger one grows away from the edges it's against (2026-10-15). Stored
/// as one of [`POSITIONS`]' names on their nine points (as before), else
/// as `{ "x": 12, "y": 85 }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

impl Position {
    pub const TOP: Position = Position { x: 50.0, y: 0.0 };

    /// A name's point: the nine in reading order, 0, 50 and 100 each way.
    pub fn named(name: &str) -> Option<Position> {
        let index = POSITIONS.iter().position(|(each, _)| *each == name)?;
        Some(Position { x: (index % 3) as f64 * 50.0, y: (index / 3) as f64 * 50.0 })
    }

    /// A stored value: a name, or x and y, each 0–100.
    pub fn from_value(value: &Value) -> Option<Position> {
        match value {
            Value::String(name) => Position::named(name),
            Value::Object(map) => {
                let part = |key: &str| map.get(key)?.as_f64().filter(|number| (0.0..=100.0).contains(number));
                Some(Position { x: part("x")?, y: part("y")? })
            }
            _ => None,
        }
    }

    /// Its name, on one of the nine points.
    pub fn name(self) -> Option<&'static str> {
        POSITIONS.iter().map(|(name, _)| *name).find(|name| Position::named(name) == Some(self))
    }

    /// As it's stored: its name, or x and y (whole numbers as such).
    pub fn to_value(self) -> Value {
        match self.name() {
            Some(name) => Value::String(name.to_string()),
            None => serde_json::json!({ "x": whole(self.x), "y": whole(self.y) }),
        }
    }

    /// What it's called: "Top", or "12 %, 85 %".
    pub fn label(self) -> String {
        match self.name() {
            Some(name) => {
                POSITIONS.iter().find(|(each, _)| *each == name).map_or("", |(_, label)| label).into()
            }
            None => format!("{} %, {} %", percent(self.x), percent(self.y)),
        }
    }

    /// Across and down, each 0–1: where to place it.
    pub fn fractions(self) -> (f64, f64) {
        (self.x.clamp(0.0, 100.0) / 100.0, self.y.clamp(0.0, 100.0) / 100.0)
    }
}

impl Default for Position {
    fn default() -> Self {
        Position::TOP
    }
}

impl Serialize for Position {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Position::from_value(&value).ok_or_else(|| {
            serde::de::Error::custom("a position is a name (top_left … bottom_right) or x and y, 0–100")
        })
    }
}

/// A whole number as an integer in JSON (12, not 12.0).
fn whole(number: f64) -> Value {
    if number.fract() == 0.0 { Value::from(number as i64) } else { Value::from(number) }
}

/// A percentage as shown: whole, or to one place.
fn percent(number: f64) -> String {
    let rounded = (number * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 { format!("{}", rounded as i64) } else { format!("{rounded:.1}") }
}

fn is_position(value: &str) -> bool {
    POSITIONS.iter().any(|(each, _)| *each == value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

impl Choice {
    /// A choice that shows what it stores.
    pub fn plain(value: impl Into<String>) -> Choice {
        let value = value.into();
        Choice { label: value.clone(), value }
    }
}

impl SocketSpec {
    pub fn socket(&self) -> Socket {
        Socket { name: self.name.clone(), label: self.label.clone(), ty: self.ty }
    }
}

impl Setting {
    pub fn default_value(&self) -> Value {
        match &self.kind {
            SettingKind::Number { default, .. } => Value::from(*default),
            SettingKind::Text { default, many: true, .. } => Value::Array(
                default
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(|item| Value::String(item.to_string()))
                    .collect(),
            ),
            SettingKind::Text { default, .. }
            | SettingKind::Position { default, .. }
            | SettingKind::Colour { default }
            | SettingKind::Picture { default, .. }
            | SettingKind::Media { default, .. }
            | SettingKind::Sound { default, .. } => Value::String(default.clone()),
            SettingKind::Bool { default } => Value::Bool(*default),
            SettingKind::Emotes { default, .. } => default.clone(),
            SettingKind::Preset {} | SettingKind::File { .. } | SettingKind::Secret { .. } => {
                Value::String(String::new())
            }
            SettingKind::Choice { default, options } => Value::String(
                default
                    .clone()
                    .or_else(|| options.first().map(|choice| choice.value.clone()))
                    .unwrap_or_default(),
            ),
        }
    }

    /// The stored value if it fits this setting (numbers within its bounds),
    /// else the default.
    pub fn read(&self, stored: Option<&Value>) -> Value {
        let Some(stored) = stored else { return self.default_value() };
        let fits = match &self.kind {
            SettingKind::Number { min, max, .. } => {
                return match stored.as_f64().filter(|number| number.is_finite()) {
                    Some(number) => Value::from(clamp(number, *min, *max)),
                    None => self.default_value(),
                };
            }
            SettingKind::Text { many: true, .. } => {
                stored.as_array().is_some_and(|items| items.iter().all(Value::is_string))
            }
            SettingKind::Text { .. }
            | SettingKind::Picture { .. }
            | SettingKind::Media { .. }
            | SettingKind::Sound { .. }
            | SettingKind::Preset {}
            | SettingKind::File { .. } => stored.is_string(),
            // Never in the config.
            SettingKind::Secret { .. } => false,
            SettingKind::Bool { .. } => stored.is_boolean(),
            SettingKind::Emotes { .. } => is_emotes_value(stored),
            SettingKind::Colour { .. } => stored.as_str().is_some_and(is_colour),
            SettingKind::Position { random, .. } => {
                Position::from_value(stored).is_some()
                    || (*random && stored.as_str() == Some(RANDOM_POSITION))
            }
            SettingKind::Choice { options, .. } => {
                stored.as_str().is_some_and(|value| options.iter().any(|choice| choice.value == value))
            }
        };
        if fits { stored.clone() } else { self.default_value() }
    }

    /// A number within this setting's bounds (numbers only).
    pub fn clamp(&self, number: f64) -> f64 {
        match &self.kind {
            SettingKind::Number { min, max, .. } => clamp(number, *min, *max),
            _ => number,
        }
    }
}

fn clamp(number: f64, min: Option<f64>, max: Option<f64>) -> f64 {
    let number = min.map_or(number, |min| number.max(min));
    max.map_or(number, |max| number.min(max))
}

fn is_colour(text: &str) -> bool {
    text.len() == 7 && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn is_id(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A file setting's file name: letters, digits, `-`, `_` and `.`, not
/// starting with `.`.
fn is_file_name(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('.')
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A path under a plugin's folder: `/`-separated names, none of them `..`,
/// and none with a `:` (on Windows `C:/…` starts at a drive's root, and
/// `name:stream` is a file's hidden stream).
fn is_page_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.contains('\\') && !part.contains(':')
        })
}

/// The most lists of choices a plugin's server code can fill in
/// (`host.set-options`), the most choices in one, and the longest choice
/// (and list name), in bytes. More are left out.
pub const MAX_OPTION_LISTS: usize = 1000;
pub const MAX_OPTIONS: usize = 1000;
pub const MAX_OPTION_LENGTH: usize = 200;

/// The `{key}` names in a text setting's `suggest_from`.
fn placeholders(template: &str) -> impl Iterator<Item = &str> {
    template.split('{').skip(1).filter_map(|part| part.split_once('}').map(|(name, _)| name))
}

/// The name of the list a text setting suggests from (its
/// `suggest_from`), with each `{key}` replaced by that setting's value
/// (`value_of`); `None` while one of them is empty, or for no list.
pub fn options_list(suggest_from: &str, value_of: impl Fn(&str) -> Option<String>) -> Option<String> {
    if suggest_from.is_empty() {
        return None;
    }
    let mut name = suggest_from.to_string();
    for key in placeholders(suggest_from) {
        let value = value_of(key).filter(|value| !value.is_empty())?;
        name = name.replace(&format!("{{{key}}}"), &value);
    }
    Some(name)
}

/// A plugin's secret settings' keys.
pub fn secret_settings(manifest: &Manifest) -> impl Iterator<Item = &str> {
    manifest
        .settings
        .iter()
        .filter(|setting| matches!(setting.kind, SettingKind::Secret { .. }))
        .map(|setting| setting.key.as_str())
}

/// A plugin's file settings: key, file name and default page file.
pub fn file_settings(manifest: &Manifest) -> impl Iterator<Item = (&str, &str, &str)> {
    manifest.settings.iter().filter_map(|setting| match &setting.kind {
        SettingKind::File { file, default, .. } => {
            Some((setting.key.as_str(), file.as_str(), default.as_str()))
        }
        _ => None,
    })
}

/// One of a file setting's looks, as the panel gets them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLook {
    pub name: String,
    pub text: String,
    /// The user's copy (a file of theirs), not one of the plugin's own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own: bool,
    /// The one picked while none is (the setting's `default`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

/// A file setting's own looks, by name and page file: its `starts`, with
/// its `default` first as "Default" when it isn't one of them.
pub fn built_in_looks(starts: &[Choice], default: &str) -> Vec<Choice> {
    let mut looks = Vec::new();
    if !default.is_empty() && !starts.iter().any(|start| start.value == default) {
        looks.push(Choice { value: default.to_string(), label: DEFAULT_LOOK.into() });
    }
    looks.extend(starts.iter().cloned());
    looks
}

/// The name of a file setting's default page file when none of its
/// `starts` is it.
pub const DEFAULT_LOOK: &str = "Default";

/// What an old file setting's file became among the user's copies.
pub const MOVED_LOOK: &str = "Custom";

/// The look a file setting's value picks: the one with that name, else
/// the default one.
pub fn picked_look<'a>(looks: &'a [PluginLook], value: &str) -> Option<&'a PluginLook> {
    looks.iter().find(|look| look.name == value).or_else(|| looks.iter().find(|look| look.default))
}

/// Characters a look's name (its file's) can't have.
const NOT_IN_LOOK_NAMES: &str = "/\\:*?\"<>|";
const MAX_LOOK_NAME: usize = 60;

/// Why `name` can't be a copy's name among `looks` (`renaming`: the copy
/// being renamed, which may keep its name), if it can't. Names are compared
/// without case, as Windows compares files'.
pub fn look_name_problem(name: &str, looks: &[PluginLook], renaming: Option<&str>) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("It needs a name.");
    }
    if name != name.trim() || name.ends_with('.') || name.starts_with('.') {
        return Some("No spaces or dots at the start or end of the name.");
    }
    if name.chars().count() > MAX_LOOK_NAME
        || name.chars().any(|c| c.is_control() || NOT_IN_LOOK_NAMES.contains(c))
    {
        return Some("At most 60 characters, without / \\ : * ? \" < > |.");
    }
    let taken = looks.iter().any(|look| {
        look.name.eq_ignore_ascii_case(name)
            && renaming.is_none_or(|current| !look.name.eq_ignore_ascii_case(current))
    });
    taken.then_some("Another look has this name.")
}

/// A name for a copy of `name`: "Classic copy", then "Classic copy 2", …
pub fn copy_name(name: &str, looks: &[PluginLook]) -> String {
    let base = format!("{name} copy");
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base} {n}")))
        .find(|candidate| look_name_problem(candidate, looks, None).is_none())
        .expect("some number is free")
}

fn is_name(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl NodeType {
    pub fn inputs(&self) -> Vec<Socket> {
        self.inputs.iter().map(SocketSpec::socket).collect()
    }

    pub fn outputs(&self) -> Vec<Socket> {
        self.outputs.iter().map(SocketSpec::socket).collect()
    }

    pub fn setting(&self, key: &str) -> Option<&Setting> {
        self.settings.iter().find(|setting| setting.key == key)
    }

    /// Every setting at its default: a new node's settings.
    pub fn default_settings(&self) -> Map<String, Value> {
        self.settings.iter().map(|setting| (setting.key.clone(), setting.default_value())).collect()
    }

    /// Settings with a row of their own: the ones that don't stand in for
    /// an input.
    pub fn own_settings(&self) -> impl Iterator<Item = &Setting> {
        self.settings.iter().filter(|setting| !self.inputs.iter().any(|input| input.name == setting.key))
    }

    /// Whether a setting is shown (and sent) with these inputs wired and
    /// these settings (the node's, as stored). A hidden one keeps its value.
    pub fn shown(
        &self,
        setting: &Setting,
        wired: impl Fn(&str) -> bool,
        settings: &Map<String, Value>,
    ) -> bool {
        match &setting.when {
            None => true,
            Some(When::Wired(input)) => wired(input),
            Some(When::Unwired(input)) => !wired(input),
            Some(When::Is(values)) => values.iter().all(|(key, value)| {
                self.setting(key).is_some_and(|other| other.read(settings.get(key)).as_str() == Some(value))
            }),
            // Only the plugin's own settings have it (checked).
            Some(When::Set(_)) => true,
        }
    }

    /// Whether a run of this node with these settings waits in the queue.
    pub fn queued(&self, settings: &Map<String, Value>) -> bool {
        match self.queue {
            QueueMode::Never => false,
            QueueMode::Always => true,
            QueueMode::Optional => self
                .setting(QUEUE_SETTING)
                .is_some_and(|setting| setting.read(settings.get(QUEUE_SETTING)) == Value::Bool(true)),
        }
    }

    /// What a run with these values is, for people: the `describe` value
    /// as text (a text's plain text), or the node's title.
    pub fn description(&self, values: &Map<String, Value>) -> String {
        let value = self.describe.as_ref().and_then(|name| values.get(name));
        let text = match value {
            Some(Value::Object(text)) => text.get("text").and_then(Value::as_str).map(str::to_string),
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        };
        text.filter(|text| !text.trim().is_empty()).unwrap_or_else(|| self.title.clone())
    }

    /// The `describe` value's parts, when it's a text with emotes or text
    /// styles in it (so it can be shown as it looked); else none, the
    /// [`description`](Self::description) says it all.
    pub fn description_parts(&self, values: &Map<String, Value>) -> Vec<TextPart> {
        let value = self.describe.as_ref().and_then(|name| values.get(name));
        let Some(parts) = value.and_then(|value| value.get("parts")) else { return Vec::new() };
        let parts: Vec<TextPart> = serde_json::from_value(parts.clone()).unwrap_or_default();
        let plain = parts.iter().all(|part| matches!(part, TextPart::Text { style: None, .. }));
        if plain { Vec::new() } else { parts }
    }

    /// The setting that picks one of its plugin's presets, if any.
    pub fn preset_setting(&self) -> Option<&Setting> {
        self.settings.iter().find(|setting| matches!(setting.kind, SettingKind::Preset {}))
    }

    /// Whether this is an event node with an event output `output` (what
    /// `emit` (`event`) and `emit-output` may fire), or a flow node with
    /// one (where its server code may send an event on).
    pub fn fires(&self, output: &str) -> bool {
        matches!(self.category, Category::Events | Category::Flow)
            && self.outputs.iter().any(|socket| socket.name == output && socket.ty == SocketType::Event)
    }

    /// Whether one of an event node's settings is a filter, which the app
    /// applies: a text setting named after one of its text outputs lets
    /// through only events whose value there is one of the setting's. Its
    /// other settings are for its server code (`nodes-changed`).
    pub fn is_filter(&self, setting: &Setting) -> bool {
        self.category == Category::Events
            && matches!(setting.kind, SettingKind::Text { .. })
            && self.outputs.iter().any(|output| output.name == setting.key && output.ty == SocketType::Text)
    }

    /// The text input the app reads aloud, if the node has one.
    pub fn spoken_input(&self) -> Option<&SocketSpec> {
        self.inputs.iter().find(|input| input.speak)
    }

    /// The setting that stands in for a number or text input of this type.
    pub fn input_setting(&self, input: &str, ty: SocketType) -> Option<&Setting> {
        let setting = self.setting(input)?;
        let matches = match ty {
            SocketType::Number => matches!(setting.kind, SettingKind::Number { .. }),
            SocketType::Text => matches!(setting.kind, SettingKind::Text { .. }),
            SocketType::Emotes => matches!(setting.kind, SettingKind::Emotes { .. }),
            _ => false,
        };
        (matches && self.inputs.iter().any(|socket| socket.name == input && socket.ty == ty))
            .then_some(setting)
    }

    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let node = &self.id;
        if !is_name(&self.id.replace('-', "_")) {
            problems.push(format!("node id {node:?}: use lowercase letters, digits, - and _"));
        }
        if self.category == Category::Values {
            if self.inputs.iter().chain(&self.outputs).any(|socket| socket.ty == SocketType::Event) {
                problems.push(format!("node {node:?}: a value node has no event inputs or outputs"));
            }
            if self.outputs.is_empty() {
                problems.push(format!("node {node:?}: a value node needs an output"));
            }
        }
        if self.category == Category::Flow {
            if !self.inputs.iter().any(|input| input.ty == SocketType::Event) {
                problems.push(format!("node {node:?}: a flow node needs an event input"));
            }
            if !self.outputs.iter().any(|output| output.ty == SocketType::Event) {
                problems.push(format!("node {node:?}: a flow node needs an event output"));
            }
        }
        if !self.changes.is_empty() && self.category != Category::Effects {
            problems.push(format!("node {node:?}: only an effect node changes the plugin's settings"));
        }
        for (side, sockets) in [("input", &self.inputs), ("output", &self.outputs)] {
            let mut names = BTreeSet::new();
            for socket in sockets {
                if !is_name(&socket.name) {
                    problems.push(format!(
                        "node {node:?}, {side} {:?}: use lowercase letters, digits and _",
                        socket.name
                    ));
                }
                if !names.insert(&socket.name) {
                    problems.push(format!("node {node:?} has two {side}s named {:?}", socket.name));
                }
                if socket.speak && (side != "input" || socket.ty != SocketType::Text) {
                    problems.push(format!(
                        "node {node:?}, {side} {:?}: only a text input can be read aloud",
                        socket.name
                    ));
                }
            }
        }
        if self.category == Category::Events {
            if !self.inputs.is_empty() {
                problems.push(format!("node {node:?}: event nodes have no inputs"));
            }
            if !self.outputs.iter().any(|output| output.name == "event" && output.ty == SocketType::Event) {
                problems.push(format!(
                    "node {node:?}: an event node needs an output named \"event\" of type event"
                ));
            }
        } else if let Some(setting) =
            self.settings.iter().find(|setting| matches!(setting.kind, SettingKind::Text { many: true, .. }))
        {
            problems.push(format!(
                "node {node:?}, setting {:?}: \"many\" is only for an event node's text settings so far",
                setting.key
            ));
        }
        if self.queue != QueueMode::Never && self.category != Category::Effects {
            problems.push(format!("node {node:?}: only effect nodes can wait in the queue"));
        }
        if self.queue == QueueMode::Optional
            && !self
                .setting(QUEUE_SETTING)
                .is_some_and(|setting| matches!(setting.kind, SettingKind::Bool { .. }))
        {
            problems.push(format!(
                "node {node:?}: \"queue\": \"optional\" needs a tick box setting (type bool) with the key \"queue\""
            ));
        }
        if self.category == Category::Effects {
            if self.outputs.iter().any(|output| output.ty != SocketType::Event) {
                problems.push(format!(
                    "node {node:?}: an effect node's outputs are events (its server code fires them)"
                ));
            }
            if self.queue != QueueMode::Never
                && self
                    .outputs
                    .iter()
                    .any(|output| [STARTED_OUTPUT, ENDED_OUTPUT].contains(&output.name.as_str()))
            {
                problems.push(format!(
                    "node {node:?}: \"started\" and \"ended\" are the queue's outputs on a node that can wait in it"
                ));
            }
            if !self.inputs.iter().any(|input| input.ty == SocketType::Event) {
                problems.push(format!("node {node:?}: an effect needs an event input (a trigger)"));
            }
        }
        if let Some(name) = &self.describe
            && !self.inputs.iter().any(|input| input.name == *name)
            && self.setting(name).is_none()
        {
            problems.push(format!(
                "node {node:?}: \"describe\" names {name:?}, which isn't one of its inputs or settings"
            ));
        }
        if self.settings.iter().filter(|setting| matches!(setting.kind, SettingKind::Preset {})).count() > 1 {
            problems.push(format!("node {node:?}: only one setting can pick a preset"));
        }
        if self.viewer && self.category != Category::Effects {
            problems.push(format!("node {node:?}: only an effect node gets the viewer"));
        }
        if self.viewer
            && (self.inputs.iter().any(|input| input.name == VIEWER_VALUE)
                || self.setting(VIEWER_VALUE).is_some())
        {
            problems.push(format!(
                "node {node:?}: it gets the viewer as {VIEWER_VALUE:?}, so no input or setting can have that name"
            ));
        }
        if self.inputs.iter().filter(|input| input.speak).count() > 1 {
            problems.push(format!("node {node:?}: only one input can be read aloud"));
        }
        if self.category != Category::Effects && self.inputs.iter().any(|input| input.speak) {
            problems.push(format!("node {node:?}: only an effect node reads aloud"));
        }
        problems.extend(settings_problems(&format!("node {node:?}"), &self.settings));
        for setting in &self.settings {
            let key = &setting.key;
            if matches!(setting.kind, SettingKind::File { .. }) {
                problems.push(format!("node {node:?}, setting {key:?}: a file setting is the plugin's own"));
            }
            if matches!(setting.kind, SettingKind::Secret { .. }) {
                problems
                    .push(format!("node {node:?}, setting {key:?}: a secret setting is the plugin's own"));
            }
            match &setting.when {
                Some(When::Set(_)) => problems.push(format!(
                    "node {node:?}, setting {key:?}: \"when\": {{ \"set\": … }} is for the plugin's own settings"
                )),
                Some(When::Is(values)) => {
                    if values.is_empty() {
                        problems.push(format!("node {node:?}, setting {key:?}: \"is\" names no setting"));
                    }
                    for (other, value) in values {
                        let options = match self.own_settings().find(|each| each.key == *other && each.key != *key) {
                            Some(Setting { kind: SettingKind::Choice { options, .. }, .. }) => options,
                            _ => {
                                problems.push(format!(
                                    "node {node:?}, setting {key:?}: \"is\" names {other:?}, which isn't another of its choice settings"
                                ));
                                continue;
                            }
                        };
                        if !options.iter().any(|choice| choice.value == *value) {
                            problems.push(format!(
                                "node {node:?}, setting {key:?}: \"is\": {value:?} isn't one of {other:?}'s options"
                            ));
                        }
                    }
                }
                Some(when) => {
                    let input = when.input().unwrap_or_default();
                    if !self.inputs.iter().any(|each| each.name == input) {
                        problems.push(format!(
                            "node {node:?}, setting {key:?}: \"when\" names {input:?}, which isn't one of its inputs"
                        ));
                    }
                }
                None => {}
            }
            if let SettingKind::Emotes { count: Some(count), .. } = &setting.kind {
                let numbers =
                    self.inputs.iter().any(|input| input.name == *count && input.ty == SocketType::Number)
                        || self
                            .setting(count)
                            .is_some_and(|setting| matches!(setting.kind, SettingKind::Number { .. }));
                if !numbers {
                    problems.push(format!(
                        "node {node:?}, setting {key:?}: its count {count:?} isn't a number input or setting of the node"
                    ));
                }
            }
            if matches!(setting.kind, SettingKind::Emotes { .. })
                && self.input_setting(key, SocketType::Emotes).is_none()
            {
                problems.push(format!(
                    "node {node:?}, setting {key:?}: an emotes setting stands in for an emotes input, so it needs one named {key:?}"
                ));
            }
            if let Some(input) = self.inputs.iter().find(|input| input.name == *key)
                && self.input_setting(key, input.ty).is_none()
            {
                problems.push(format!(
                    "node {node:?}, setting {key:?}: it's named like an input, so it must be of the input's type (number or text)"
                ));
            }
        }
        problems
    }
}

/// Problems with a list of settings (a node's, or the plugin's own);
/// `whose` starts each message.
fn settings_problems(whose: &str, settings: &[Setting]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut keys = BTreeSet::new();
    for setting in settings {
        let key = &setting.key;
        if let SettingKind::Text { suggest_from, .. } = &setting.kind
            && !suggest_from.is_empty()
        {
            if suggest_from.len() > MAX_OPTION_LENGTH {
                problems.push(format!("{whose}, setting {key:?}: suggest_from is too long"));
            }
            for named in placeholders(suggest_from) {
                if !settings.iter().any(|other| other.key == named && other.key != *key) {
                    problems.push(format!(
                        "{whose}, setting {key:?}: suggest_from names {{{named}}}, which isn't another of its settings"
                    ));
                }
            }
        }
        if !is_name(key) {
            problems.push(format!("{whose}, setting {key:?}: use lowercase letters, digits and _"));
        }
        if !keys.insert(key) {
            problems.push(format!("{whose} has two settings named {key:?}"));
        }
        if setting.required
            && !matches!(
                setting.kind,
                SettingKind::Picture { .. } | SettingKind::Media { .. } | SettingKind::Sound { .. }
            )
        {
            problems
                .push(format!("{whose}, setting {key:?}: only pictures, media and sounds can be required"));
        }
        if setting.required && setting.when.is_some() {
            problems.push(format!(
                "{whose}, setting {key:?}: a required setting is always shown, so no \"when\""
            ));
        }
        if let SettingKind::Emotes { default, .. } = &setting.kind
            && !is_emotes_value(default)
        {
            problems.push(format!(
                "{whose}, setting {key:?}: the default is \"random\" or a list of emote codes"
            ));
        }
        match &setting.kind {
            SettingKind::Number { default, min, max, slider, .. } => {
                if *slider && (min.is_none() || max.is_none()) {
                    problems.push(format!("{whose}, setting {key:?}: a slider needs a min and a max"));
                }
                if min.zip(*max).is_some_and(|(min, max)| min > max) {
                    problems.push(format!("{whose}, setting {key:?}: min is more than max"));
                }
                if !default.is_finite() || setting.clamp(*default) != *default {
                    problems.push(format!("{whose}, setting {key:?}: the default is out of bounds"));
                }
            }
            SettingKind::Colour { default } if !is_colour(default) => {
                problems.push(format!("{whose}, setting {key:?}: the default isn't #rrggbb"));
            }
            SettingKind::Position { default, random }
                if !is_position(default) && !(*random && default == RANDOM_POSITION) =>
            {
                problems.push(format!(
                    "{whose}, setting {key:?}: the default isn't a position (top_left … bottom_right)"
                ));
            }
            SettingKind::Choice { options, .. } if options.is_empty() => {
                problems.push(format!("{whose}, setting {key:?}: a choice needs options"));
            }
            SettingKind::Choice { default: Some(default), options }
                if !options.iter().any(|choice| choice.value == *default) =>
            {
                problems.push(format!("{whose}, setting {key:?}: the default isn't one of the options"));
            }
            _ => {}
        }
    }
    problems
}

/// Settings as stored (`PluginConfig::settings`, a node's settings), each
/// read leniently ([`Setting::read`]): every setting has a value.
pub fn read_settings(settings: &[Setting], stored: &Map<String, Value>) -> Map<String, Value> {
    settings.iter().map(|setting| (setting.key.clone(), setting.read(stored.get(&setting.key)))).collect()
}

/// A plugin's own setting as its page and server code get it (`settings`
/// messages, runs' `settings` and `changes`, `host.settings`): read
/// leniently, with pictures, media and sounds as their paths on the app
/// (an http(s) address as it is; a folder, a file `pick`ed from it; `null`
/// for none; with `many`, a list of them: a folder all its files, none
/// `[]`), the rest as stored.
pub fn shared_value(setting: &Setting, stored: Option<&Value>, pick: &mut PickFile<'_>) -> Value {
    let value = setting.read(stored);
    let Some(kind) = setting.kind.media() else { return value };
    let many = setting.kind.many_files();
    let name = value.as_str().unwrap_or_default().trim();
    let to_path = match kind {
        MediaKind::Picture | MediaKind::Media => crate::alert::picture_path,
        MediaKind::Sound => crate::sound::sound_path,
    };
    let paths: Vec<String> = if name.is_empty() {
        Vec::new()
    } else if name.starts_with("http://") || name.starts_with("https://") {
        vec![name.to_string()]
    } else if let Some(folder) = media_folder(name) {
        let files = pick(kind, folder, many);
        let wanted = if many { files.len() } else { 1 };
        files.iter().take(wanted).map(|file| to_path(file)).collect()
    } else {
        vec![to_path(name)]
    };
    if many {
        Value::Array(paths.into_iter().map(Value::String).collect())
    } else {
        paths.into_iter().next().map_or(Value::Null, Value::String)
    }
}

/// Every one of `settings` as [`shared_value`]s, but secret ones, which
/// only server code gets (from the app's secrets).
pub fn shared_settings(
    settings: &[Setting],
    stored: &Map<String, Value>,
    pick: &mut PickFile<'_>,
) -> Map<String, Value> {
    settings
        .iter()
        .filter(|setting| !matches!(setting.kind, SettingKind::Secret { .. }))
        .map(|setting| (setting.key.clone(), shared_value(setting, stored.get(&setting.key), pick)))
        .collect()
}

impl Manifest {
    /// Everything that keeps this plugin from loading.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = self.built_in_problems();
        if RESERVED_IDS.contains(&self.id.as_str()) {
            problems.push(format!("id {:?} is taken by the app", self.id));
        }
        problems
    }

    /// The same, for a plugin built into the app, which may take the name
    /// of the overlay module it replaces (`sound`), so `?modules=` keeps
    /// working.
    pub fn built_in_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if !is_id(&self.id) {
            problems.push(format!("id {:?}: use lowercase letters, digits and -", self.id));
        }
        if self.name.trim().is_empty() {
            problems.push("it needs a name".into());
        }
        if self.api != API_VERSION {
            problems.push(format!(
                "it's for plugin API version {}; this app has version {API_VERSION}",
                self.api
            ));
        }
        problems.extend(settings_problems("the plugin", &self.settings));
        for setting in &self.settings {
            if setting.when.as_ref().is_some_and(|when| !matches!(when, When::Set(_)))
                || matches!(setting.kind, SettingKind::Emotes { .. } | SettingKind::Preset {})
            {
                problems.push(format!(
                    "the plugin's setting {:?}: \"when\" wired, unwired or is, emotes and preset settings are for nodes",
                    setting.key
                ));
            }
        }
        // How it shows on the Plugins box: only the plugin's own.
        for setting in &self.settings {
            let key = &setting.key;
            let other =
                |name: &str| self.settings.iter().find(|other| other.key == name && other.key != *key);
            let is_bool = matches!(setting.kind, SettingKind::Bool { .. });
            if !setting.caption.is_empty() && !is_bool && !matches!(setting.kind, SettingKind::Number { .. })
            {
                problems.push(format!(
                    "the plugin's setting {key:?}: a caption is for a tick box (bool) or a number"
                ));
            }
            if !setting.beside.is_empty() {
                match other(&setting.beside) {
                    _ if !is_bool => problems.push(format!(
                        "the plugin's setting {key:?}: only a tick box (bool) goes beside another"
                    )),
                    None => problems.push(format!(
                        "the plugin's setting {key:?}: beside {:?}, which isn't another of its settings",
                        setting.beside
                    )),
                    Some(target)
                        if !target.beside.is_empty()
                            || matches!(
                                target.kind,
                                SettingKind::File { .. } | SettingKind::Secret { .. }
                            ) =>
                    {
                        problems.push(format!(
                            "the plugin's setting {key:?}: it can't go beside {:?} (a file, a secret or one beside another)",
                            setting.beside
                        ));
                    }
                    Some(_) => {}
                }
            }
            if !setting.icon.is_empty()
                && (setting.beside.is_empty() || !TOGGLE_ICONS.contains(&setting.icon.as_str()))
            {
                problems.push(format!(
                    "the plugin's setting {key:?}: an icon is for one beside another, and one of {}",
                    TOGGLE_ICONS.join(", ")
                ));
            }
            if let Some(When::Set(name)) = &setting.when
                && other(name).is_none()
            {
                problems.push(format!(
                    "the plugin's setting {key:?}: when set {name:?}, which isn't another of its settings"
                ));
            }
        }
        for setting in &self.settings {
            if let SettingKind::File { file, default, starts } = &setting.kind {
                let key = &setting.key;
                if !is_file_name(file) {
                    problems.push(format!(
                        "the plugin's setting {key:?}: its file {file:?} needs a name of letters, digits, -, _ and ."
                    ));
                }
                for path in std::iter::once(default)
                    .filter(|path| !path.is_empty())
                    .chain(starts.iter().map(|start| &start.value))
                {
                    if !is_page_path(path) {
                        problems.push(format!(
                            "the plugin's setting {key:?}: {path:?} should be a path of one of its page files (looks/plain.css)"
                        ));
                    }
                }
            }
        }
        if !self.help.is_empty() && !is_page_path(&self.help) {
            problems.push(format!("help {:?}: a path of one of its page files (help.html)", self.help));
        }
        if let Some(tab) = &self.tab {
            let label = tab.label.trim();
            if label.is_empty() || label.chars().count() > MAX_TAB_LABEL_CHARS || label != tab.label {
                problems.push(format!(
                    "tab: its label {:?} needs 1 to {MAX_TAB_LABEL_CHARS} characters, without spaces around them",
                    tab.label
                ));
            }
            if !is_page_path(&tab.page) {
                problems.push(format!(
                    "tab: its page {:?} should be a path of one of its page files (tab.html)",
                    tab.page
                ));
            }
        }
        if !self.icon.is_empty() {
            match self.icon_markup() {
                Some(svg) if svg.len() > MAX_ICON_BYTES => {
                    problems.push(format!("icon: at most {} KB of SVG", MAX_ICON_BYTES / 1024));
                }
                Some(_) => {}
                None if !is_page_path(&self.icon) || !self.icon.to_ascii_lowercase().ends_with(".svg") => {
                    problems.push(format!(
                        "icon {:?}: an SVG file in its folder (icon.svg), or for a program the SVG itself",
                        self.icon
                    ));
                }
                None => {}
            }
        }
        for what in &self.hears {
            if !HEARABLE.contains(&what.as_str()) {
                problems.push(format!("hears {what:?}: a plugin can hear \"chat\" and \"events\""));
            }
        }
        for host in self.hosts.iter().chain(&self.page_hosts) {
            if !is_host_pattern(host) {
                problems.push(format!(
                    "host {host:?}: a host name in lowercase (example.com), or *.example.com for its subdomains"
                ));
            }
        }
        let mut voices = BTreeSet::new();
        for voice in &self.voices {
            let id = &voice.id;
            if id.is_empty()
                || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            {
                problems.push(format!("voice {id:?}: use lowercase letters, digits, - and _"));
            }
            if voice.name.trim().is_empty() {
                problems.push(format!("voice {id:?} needs a name"));
            }
            if !voices.insert(id) {
                problems.push(format!("two voices have the id {id:?}"));
            }
        }
        if let Some(preview) = &self.preview
            && !(preview.samples && preview.node.is_empty())
            && !self.nodes.iter().any(|node| node.id == preview.node && node.category == Category::Effects)
        {
            problems
                .push(format!("the preview runs {:?}, which isn't one of its effect nodes", preview.node));
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            for key in &node.changes {
                match self.settings.iter().find(|setting| setting.key == *key) {
                    None => problems.push(format!(
                        "node {:?} changes {key:?}, which isn't one of the plugin's own settings",
                        node.id
                    )),
                    Some(setting)
                        if matches!(
                            setting.kind,
                            SettingKind::File { .. }
                                | SettingKind::Secret { .. }
                                | SettingKind::Text { many: true, .. }
                        ) =>
                    {
                        problems.push(format!(
                            "node {:?} changes {key:?}: a node can't change a file, a secret or a list",
                            node.id
                        ));
                    }
                    Some(_) => {}
                }
            }
            if node.preset_setting().is_some() && self.presets.is_none() {
                problems
                    .push(format!("node {:?} picks a preset, but the plugin has no \"presets\"", node.id));
            }
            if !node.changes.is_empty() && node.category != Category::Effects {
                problems.push(format!("node {:?}: only effect nodes change settings", node.id));
            }
            if !ids.insert(&node.id) {
                problems.push(format!("two nodes have the id {:?}", node.id));
            }
            problems.extend(node.problems());
        }
        problems
    }
}

/// An installed plugin, as the server loaded it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub manifest: Manifest,
    /// It has an overlay page (`overlay/index.html`).
    pub overlay: bool,
    /// It ships with the app (built into the exe), rather than coming from
    /// the plugins folder.
    #[serde(default)]
    pub bundled: bool,
    /// It comes from a program connected to the control API, not from a
    /// folder (see `docs/writing-a-plugin.md` → Programs).
    #[serde(default)]
    pub program: bool,
    /// A program's plugin whose program is connected now. Its node types
    /// are remembered while it isn't, so cards keep them.
    #[serde(default)]
    pub connected: bool,
    /// It has server code ([`SERVER_FILE`]): its effect nodes' runs go
    /// there instead of to its overlay page.
    #[serde(default)]
    pub server: bool,
    /// Its server code was stopped (it failed to start, ran too long or
    /// crashed), and why. Its nodes stay in the cards but do nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// What its server code says about itself (`host.set-status`): shown on
    /// its Plugins box's head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PluginStatus>,
    /// Notes its server code puts under its settings (`host.set-note`), by
    /// setting key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, PluginNote>,
    /// The number its server code puts on its tab (`host.set-tab-count`;
    /// only with the tab's `count`): 0 shows none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tab_count: u32,
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

/// A note under one of a plugin's settings, as its server code sets it:
/// lines of what its status has no room for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginNote {
    pub level: StatusLevel,
    pub lines: Vec<String>,
}

/// The most lines in a note, and the longest line, in characters. More is
/// cut.
pub const MAX_NOTE_LINES: usize = 20;
pub const MAX_NOTE_LINE_CHARS: usize = 300;

impl PluginNote {
    /// A note from server code: its lines trimmed, empty ones left out,
    /// cut to [`MAX_NOTE_LINES`] of [`MAX_NOTE_LINE_CHARS`]; `None` without
    /// a line (no note).
    pub fn new(level: StatusLevel, lines: &[String]) -> Option<PluginNote> {
        let lines: Vec<String> = lines
            .iter()
            .map(|line| cut(line.trim(), MAX_NOTE_LINE_CHARS))
            .filter(|line| !line.is_empty())
            .take(MAX_NOTE_LINES)
            .collect();
        (!lines.is_empty()).then_some(PluginNote { level, lines })
    }
}

/// `text`, cut to `max` characters with … when longer.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_string(),
    }
}

/// A plugin's status, as its server code sets it: a few words
/// ("Connected"), at most [`MAX_STATUS_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginStatus {
    pub level: StatusLevel,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusLevel {
    /// Working (green).
    Ok,
    /// Nothing wrong, waiting for something (grey): OBS isn't running.
    Waiting,
    /// Needs the user (orange, and the header's Plugins dot): a wrong
    /// password.
    Problem,
}

/// The longest status, in characters: it shares the Plugins box's head
/// with the plugin's name, so a few words. Longer ones are cut.
pub const MAX_STATUS_CHARS: usize = 32;

impl PluginStatus {
    /// A status from server code: trimmed and cut to
    /// [`MAX_STATUS_CHARS`]; `None` for an empty one (no status).
    pub fn new(level: StatusLevel, text: &str) -> Option<PluginStatus> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(PluginStatus { level, text: cut(text, MAX_STATUS_CHARS) })
    }
}

/// A plugin folder that wasn't loaded, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginFailure {
    /// The folder's name in `plugins/`.
    pub folder: String,
    pub problems: Vec<String>,
}

/// The installed plugins' node types, to attach to the cards' plugin nodes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    plugins: Vec<PluginInfo>,
    types: BTreeMap<(String, String), Arc<NodeType>>,
}

impl Catalog {
    pub fn new(plugins: Vec<PluginInfo>) -> Catalog {
        let types = plugins
            .iter()
            .flat_map(|plugin| {
                let id = &plugin.manifest.id;
                let settings = &plugin.manifest.settings;
                let presets = plugin.manifest.presets.is_some();
                plugin.manifest.nodes.iter().map(move |node| {
                    let mut node = node.clone();
                    node.changeable = node
                        .changes
                        .iter()
                        .filter_map(|key| settings.iter().find(|setting| setting.key == *key).cloned())
                        .collect();
                    if presets {
                        node.preset_settings = settings.clone();
                    }
                    ((id.clone(), node.id.clone()), Arc::new(node))
                })
            })
            .collect();
        Catalog { plugins, types }
    }

    pub fn plugins(&self) -> &[PluginInfo] {
        &self.plugins
    }

    /// The same node types, and every plugin hears the same (whatever
    /// else changed, such as a program connecting).
    pub fn same_types(&self, other: &Catalog) -> bool {
        let hears = |catalog: &Catalog| -> Vec<(String, Vec<String>)> {
            catalog
                .plugins
                .iter()
                .map(|plugin| (plugin.manifest.id.clone(), plugin.manifest.hears.clone()))
                .collect()
        };
        self.types == other.types && hears(self) == hears(other)
    }

    pub fn plugin(&self, id: &str) -> Option<&PluginInfo> {
        self.plugins.iter().find(|plugin| plugin.manifest.id == id)
    }

    pub fn node_type(&self, plugin: &str, node: &str) -> Option<Arc<NodeType>> {
        self.types.get(&(plugin.to_string(), node.to_string())).cloned()
    }

    /// Every plugin node type as a new node: the add-node menu's part,
    /// plugin by plugin, each one's nodes in its manifest's order.
    pub fn node_kinds(&self) -> Vec<crate::cards::NodeKind> {
        self.plugins
            .iter()
            .flat_map(|plugin| plugin.manifest.nodes.iter().map(|node| (&plugin.manifest.id, &node.id)))
            .filter_map(|(plugin, node)| {
                let ty = self.types.get(&(plugin.clone(), node.clone()))?;
                Some(crate::cards::NodeKind::Plugin {
                    plugin: plugin.clone(),
                    node: node.clone(),
                    settings: ty.default_settings(),
                    changes: Map::new(),
                    voice: Map::new(),
                    ty: Attached(Some(Arc::clone(ty))),
                })
            })
            .collect()
    }
}

/// A plugin node's type, attached after loading
/// ([`AppConfig::attach_plugins`](crate::config::AppConfig::attach_plugins));
/// `None` while the plugin isn't installed. Not stored, and not compared: two
/// nodes with the same settings are equal whether or not a type is attached.
#[derive(Debug, Clone, Default)]
pub struct Attached(pub Option<Arc<NodeType>>);

impl PartialEq for Attached {
    fn eq(&self, _: &Attached) -> bool {
        true
    }
}

/// Server → a plugin's overlay page, through the shell (`Command::Plugin`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginCommand {
    pub plugin: String,
    pub message: PluginMessage,
}

/// What a plugin's overlay page gets (with `issued_at_ms` added by the
/// shell), as JSON through `postMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // a message per chat line; boxing it gains nothing
pub enum PluginMessage {
    /// One of the plugin's effect nodes fired: `input` is the event input
    /// the event came into; `values` has every other input and every
    /// setting by name (see [`TextValue`], [`EmoteValue`]). A queued run
    /// has its item's `queue_id`: the page answers [`PageMessage::Done`]
    /// with it once it has finished.
    Run {
        node: String,
        input: String,
        values: Map<String, Value>,
        /// The plugin's own settings this node changes for itself (its
        /// type's `changes`), by key; the rest are as on the Plugins page.
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        changes: Map<String, Value>,
        /// For a plugin with presets: every one of its own settings for
        /// this run, from the node's preset with its changes (as when the
        /// node fired, for a queued run).
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        settings: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_id: Option<u64>,
        /// For server code, when one of the node's own outputs is wired:
        /// what `host.carry-on` names to fire one for this run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<u64>,
        /// A run in the panel's preview of the plugin (its Plugins box), not
        /// on an overlay: the page may keep it up and follow `settings`
        /// messages while it shows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<PreviewRun>,
    },
    /// The queue skipped a queued run (the panel's Skip): stop showing it.
    Skip { queue_id: u64 },
    /// From the plugin's server code (`send-to-page`), any JSON.
    Message { data: Value },
    /// The plugin-wide settings, every one with a value: when the page
    /// loads and whenever they change.
    Settings { values: Map<String, Value> },
    /// A chat message, for a page that hears the chat ([`HEARS_CHAT`]):
    /// every one, commands and bots too (the page leaves out what it
    /// wants), with the message it answers if it's a reply.
    Chat { line: crate::chat::ChatLine },
    /// A moderator deleted a message (its line's id).
    ChatRemoved { id: String },
    /// A viewer was banned or timed out: their messages go.
    ChatUserRemoved { platform: String, user_id: String },
    /// The chat was cleared (empty `platform`: every platform's).
    ChatCleared { platform: String },
    /// What the page kept ([`PageMessage::Keep`]), when it loads (after
    /// its settings); not sent when it kept nothing.
    Kept { value: Value },
    /// Its preview's Play, for a page that draws samples of its own
    /// ([`PreviewSpec::samples`]): show them (a sample chat, say).
    Samples,
}

/// Which run of the panel's preview a [`PluginMessage::Run`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewRun {
    /// Drawn as the preview opens: quietly (no sound, nothing read aloud).
    Show,
    /// Its ▶ Play: as on the overlay, sound too.
    Play,
}

/// What the overlay shell itself tells every plugin page (not from the
/// server): the master volume (0–1) and mute, when the page has loaded and
/// whenever they change. A page multiplies its own volumes by it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellMessage {
    Volume { volume: f32, muted: bool },
}

/// What a plugin's overlay page tells the overlay (with `postMessage` to
/// `window.parent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PageMessage {
    /// A queued run has finished: the queue moves on.
    Done { queue_id: u64 },
    /// Keep this with the app (`null` forgets it): the page gets it back
    /// when it loads again ([`PluginMessage::Kept`]), on any overlay. At
    /// most [`KEEP_LIMIT`] bytes of JSON; the last one kept wins.
    Keep { value: Value },
}

/// The most a page keeps ([`PageMessage::Keep`]), in bytes of JSON.
pub const KEEP_LIMIT: usize = 256 * 1024;

/// A text value as a plugin page gets it: the plain text, and the same as
/// parts with the emotes in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextValue {
    pub text: String,
    pub parts: Vec<TextPart>,
    /// An input read aloud ([`SocketSpec::speak`]): the spoken message.
    /// Missing when nothing is read (text to speech is off or isn't
    /// installed, the text is empty, or the message couldn't be made).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<crate::SpeechClip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextPart {
    Text {
        text: String,
        /// The text style a node's text marked it with (`[highlight]…[/]`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<crate::TextStyle>,
    },
    Emote {
        #[serde(flatten)]
        emote: EmoteValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<crate::TextStyle>,
    },
}

impl TextPart {
    pub fn style(&self) -> Option<&crate::TextStyle> {
        match self {
            TextPart::Text { style, .. } | TextPart::Emote { style, .. } => style.as_ref(),
        }
    }
}

/// An emote as a plugin page gets it: its image's path on the app, and the
/// zero-width emotes stacked on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmoteValue {
    pub code: String,
    pub url: String,
    #[serde(default)]
    pub overlays: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn confetti() -> Manifest {
        serde_json::from_value(json!({
            "id": "confetti",
            "name": "Confetti",
            "version": "0.1.0",
            "api": 1,
            "nodes": [{
                "id": "burst",
                "title": "Confetti",
                "category": "effects",
                "inputs": [
                    { "name": "trigger", "label": "Trigger", "type": "event" },
                    { "name": "pieces", "label": "Pieces", "type": "number" }
                ],
                "settings": [
                    { "key": "pieces", "label": "Pieces", "type": "number", "default": 150, "min": 1, "max": 1000 },
                    { "key": "from", "label": "From", "type": "choice",
                      "options": [{ "value": "bottom", "label": "The bottom corners" },
                                  { "value": "top", "label": "The top" }] },
                    { "key": "colour", "label": "Colour", "type": "colour", "default": "#ffcc00" }
                ]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn a_description_keeps_its_parts_when_they_show_more() {
        let mut node = confetti().nodes[0].clone();
        node.describe = Some("message".into());
        let mut values = Map::new();
        values.insert("message".into(), plain_text_value("Thanks!"));
        assert_eq!(node.description(&values), "Thanks!");
        assert!(node.description_parts(&values).is_empty(), "plain text says it all");
        values.insert(
            "message".into(),
            json!({ "text": "hi Kappa", "parts": [
                { "type": "text", "text": "hi " },
                { "type": "emote", "code": "Kappa", "url": "/emote/k" }
            ] }),
        );
        assert_eq!(node.description_parts(&values).len(), 2, "an emote");
        let styled = json!({ "text": "hi", "parts": [{ "type": "text", "text": "hi", "style": { "name": "big",
            "font": "", "size_percent": 150.0, "bold": null, "italic": null, "colour": "" } }] });
        values.insert("message".into(), styled);
        assert!(node.description_parts(&values)[0].style().is_some(), "a text style");
    }

    #[test]
    fn the_example_manifest_loads() {
        let manifest = confetti();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        let node = &manifest.nodes[0];
        assert_eq!(node.inputs().len(), 2);
        assert_eq!(
            Value::Object(node.default_settings()),
            json!({ "pieces": 150.0, "from": "bottom", "colour": "#ffcc00" })
        );
        // Pieces stands in for its input; the others get rows of their own.
        assert!(node.input_setting("pieces", SocketType::Number).is_some());
        let own: Vec<_> = node.own_settings().map(|setting| setting.key.as_str()).collect();
        assert_eq!(own, ["from", "colour"]);
    }

    #[test]
    fn settings_read_leniently() {
        let node = &confetti().nodes[0];
        let pieces = node.setting("pieces").unwrap();
        assert_eq!(pieces.read(Some(&json!(5000))), json!(1000.0), "clamped");
        assert_eq!(pieces.read(Some(&json!("lots"))), json!(150.0), "the wrong type is the default");
        assert_eq!(pieces.read(None), json!(150.0));
        let from = node.setting("from").unwrap();
        assert_eq!(from.read(Some(&json!("sideways"))), json!("bottom"), "not an option");
        assert_eq!(from.read(Some(&json!("top"))), json!("top"));
        let colour = node.setting("colour").unwrap();
        assert_eq!(colour.read(Some(&json!("red"))), json!("#ffcc00"));
    }

    #[test]
    fn manifest_problems() {
        let mut manifest = confetti();
        manifest.id = "rain".into();
        manifest.api = 2;
        manifest.nodes[0].settings[0].kind = SettingKind::Text {
            default: String::new(),
            suggestions: Vec::new(),
            placeholder: String::new(),
            suggest_from: String::new(),
            many: false,
        };
        let problems = manifest.problems();
        assert!(problems.iter().any(|problem| problem.contains("taken")), "{problems:?}");
        assert!(problems.iter().any(|problem| problem.contains("API version 2")), "{problems:?}");
        assert!(problems.iter().any(|problem| problem.contains("named like an input")), "{problems:?}");

        let mut manifest = confetti();
        manifest.nodes[0].inputs.remove(0);
        assert!(manifest.problems().iter().any(|problem| problem.contains("needs an event input")));

        // Event nodes: outputs only, one of them `event`.
        let mut membership = membership();
        assert_eq!(membership.problems(), Vec::<String>::new());
        membership.nodes[0].outputs.remove(0);
        membership.nodes[0].inputs = confetti().nodes[0].inputs.clone();
        let problems = membership.problems();
        assert!(problems.iter().any(|problem| problem.contains("no inputs")), "{problems:?}");
        assert!(problems.iter().any(|problem| problem.contains("named \"event\"")), "{problems:?}");

        // Flow nodes: an event in and an event out, values out too; no
        // viewer to ask for (they always get it), no changes.
        let flow = |inputs: Value, outputs: Value, more: Value| -> Vec<String> {
            let mut node = json!({ "id": "pass", "title": "Pass", "category": "flow",
                "inputs": inputs, "outputs": outputs });
            node.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
            let manifest: Manifest = serde_json::from_value(json!({
                "id": "flows", "name": "Flows", "version": "0.1.0", "api": 1, "nodes": [node]
            }))
            .unwrap();
            manifest.problems()
        };
        let event_in = json!([{ "name": "event", "label": "Event", "type": "event" }]);
        let outs = json!([
            { "name": "event", "label": "Event", "type": "event" },
            { "name": "miss", "label": "Miss", "type": "event" },
            { "name": "count", "label": "Count", "type": "number" }
        ]);
        assert_eq!(flow(event_in.clone(), outs.clone(), json!({})), Vec::<String>::new());
        let problems =
            flow(json!([]), json!([{ "name": "count", "label": "Count", "type": "number" }]), json!({}));
        assert!(problems.iter().any(|problem| problem.contains("needs an event input")), "{problems:?}");
        assert!(problems.iter().any(|problem| problem.contains("needs an event output")), "{problems:?}");
        let problems = flow(event_in, outs, json!({ "viewer": true, "changes": ["x"] }));
        assert!(problems.iter().any(|problem| problem.contains("only an effect node gets the viewer")));
        assert!(
            problems.iter().any(|problem| problem.contains("only an effect node changes")),
            "{problems:?}"
        );

        // Value nodes: values in and out, no events.
        let value = |inputs: Value, outputs: Value| -> Vec<String> {
            let manifest: Manifest = serde_json::from_value(json!({
                "id": "values", "name": "Values", "version": "0.1.0", "api": 1,
                "nodes": [{ "id": "money", "title": "Money", "category": "values",
                    "inputs": inputs, "outputs": outputs,
                    "settings": [{ "key": "bits", "label": "Bits", "type": "number", "default": 100 }] }]
            }))
            .unwrap();
            manifest.problems()
        };
        let bits_in = json!([{ "name": "bits", "label": "Bits", "type": "number" }]);
        let money_out = json!([{ "name": "money", "label": "Money", "type": "text" }]);
        assert_eq!(value(bits_in.clone(), money_out), Vec::<String>::new());
        let problems = value(bits_in, json!([]));
        assert!(problems.iter().any(|problem| problem.contains("needs an output")), "{problems:?}");
        let problems = value(json!([]), json!([{ "name": "event", "label": "Event", "type": "event" }]));
        assert!(
            problems.iter().any(|problem| problem.contains("no event inputs or outputs")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_file_settings_looks_and_a_lists_default() {
        let starts = [
            Choice { value: "looks/bubbles.css".into(), label: "Bubbles".into() },
            Choice { value: "looks/classic.css".into(), label: "Classic".into() },
        ];
        assert_eq!(built_in_looks(&starts, "looks/classic.css"), starts);
        let with_default = built_in_looks(&starts, "looks/plain.css");
        assert_eq!(with_default[0], Choice { value: "looks/plain.css".into(), label: DEFAULT_LOOK.into() });
        let look = |name: &str, own: bool, default: bool| PluginLook {
            name: name.into(),
            text: format!(".{name} {{}}"),
            own,
            default,
        };
        let looks =
            [look("Bubbles", false, true), look("Classic", false, false), look("Classic copy", true, false)];
        assert_eq!(picked_look(&looks, "Classic").unwrap().name, "Classic");
        assert_eq!(picked_look(&looks, "").unwrap().name, "Bubbles", "none: the default");
        assert_eq!(picked_look(&looks, "Gone").unwrap().name, "Bubbles");
        assert_eq!(copy_name("Classic", &looks), "Classic copy 2");
        assert_eq!(copy_name("Bubbles", &looks), "Bubbles copy");
        assert!(look_name_problem("classic", &looks, None).is_some(), "taken, whatever the case");
        assert!(look_name_problem("Classic copy", &looks, Some("Classic copy")).is_none(), "keeps its own");
        for bad in ["", " Mine", "Mine.", "a/b", "a:b", "x".repeat(61).as_str()] {
            assert!(look_name_problem(bad, &looks, None).is_some(), "{bad:?}");
        }
        assert!(look_name_problem("Stream look 2", &looks, None).is_none());

        let list: Setting = serde_json::from_value(json!({
            "key": "bots", "label": "Bots", "type": "text", "many": true, "default": "Nightbot, , Moobot "
        }))
        .unwrap();
        assert_eq!(list.default_value(), json!(["Nightbot", "Moobot"]));
        assert_eq!(
            list.read(Some(&json!("Nightbot"))),
            json!(["Nightbot", "Moobot"]),
            "not a list: the default"
        );
        assert_eq!(list.read(Some(&json!(["Fossabot"]))), json!(["Fossabot"]));

        // A node can't change a list (nor a file, nor a secret).
        let manifest: Manifest = serde_json::from_value(json!({
            "id": "chat", "name": "Chat", "version": "0.1.0", "api": 1,
            "settings": [{ "key": "bots", "label": "Bots", "type": "text", "many": true }],
            "nodes": [{ "id": "show", "title": "Show", "category": "effects", "changes": ["bots"],
                "inputs": [{ "name": "trigger", "label": "Trigger", "type": "event" }] }]
        }))
        .unwrap();
        let problems = manifest.problems();
        assert!(
            problems.iter().any(|problem| problem.contains("can't change a file, a secret or a list")),
            "{problems:?}"
        );
    }

    #[test]
    fn how_a_plugins_own_settings_show_on_its_box() {
        let problems = |settings: Value| -> Vec<String> {
            let manifest: Manifest = serde_json::from_value(json!({
                "id": "look", "name": "Look", "version": "0.1.0", "api": 1, "settings": settings
            }))
            .unwrap();
            manifest.problems()
        };
        let font = json!({ "key": "font", "label": "Font", "type": "text", "default": "Segoe UI" });
        let fine = problems(json!([
            font,
            { "key": "bold", "label": "Bold", "type": "bool", "beside": "font", "icon": "bold" },
            { "key": "caps", "label": "Caps", "type": "bool", "beside": "font" },
            { "key": "emotes", "label": "Show emotes as pictures", "short": "Emotes", "caption": "As pictures",
              "type": "bool" },
            { "key": "picture", "label": "Picture", "type": "picture" },
            { "key": "size", "label": "Picture size", "short": "Size", "type": "number",
              "when": { "set": "picture" } },
            { "key": "most", "label": "Most messages", "short": "On screen", "type": "number",
              "caption": "messages at most" },
            { "key": "bots", "label": "Bots", "type": "text", "many": true, "default": "Nightbot, Moobot" }
        ]));
        assert_eq!(fine, Vec::<String>::new());
        let has = |problems: &[String], text: &str| problems.iter().any(|problem| problem.contains(text));
        let wrong = problems(json!([
            font,
            { "key": "size", "label": "Size", "type": "text", "caption": "px" },
            { "key": "big", "label": "Big", "type": "number", "beside": "font" },
            { "key": "bold", "label": "Bold", "type": "bool", "beside": "nope" },
            { "key": "under", "label": "Under", "type": "bool", "beside": "bold" },
            { "key": "own", "label": "Own", "type": "bool", "beside": "own" },
            { "key": "loud", "label": "Loud", "type": "bool", "beside": "font", "icon": "volume" },
            { "key": "lone", "label": "Lone", "type": "bool", "icon": "bold" },
            { "key": "shown", "label": "Shown", "type": "text", "when": { "set": "nothing" } },
            { "key": "wired", "label": "Wired", "type": "text", "when": { "wired": "emotes" } }
        ]));
        assert!(has(&wrong, "\"size\": a caption is for a tick box (bool) or a number"), "{wrong:?}");
        assert!(has(&wrong, "\"big\": only a tick box (bool) goes beside another"), "{wrong:?}");
        assert!(has(&wrong, "\"bold\": beside \"nope\", which isn't another"), "{wrong:?}");
        assert!(has(&wrong, "\"under\": it can't go beside \"bold\""), "{wrong:?}");
        assert!(has(&wrong, "\"own\": beside \"own\", which isn't another"), "{wrong:?}");
        assert!(
            has(&wrong, "\"loud\": an icon is for one beside another, and one of bold, italic"),
            "{wrong:?}"
        );
        assert!(has(&wrong, "\"lone\": an icon is for one beside another"), "{wrong:?}");
        assert!(has(&wrong, "\"shown\": when set \"nothing\", which isn't another"), "{wrong:?}");
        assert!(has(&wrong, "\"wired\": \"when\" wired, unwired or is"), "{wrong:?}");
        // Nodes' settings are shown by their inputs, not by being set.
        let node: Manifest = serde_json::from_value(json!({
            "id": "look", "name": "Look", "version": "0.1.0", "api": 1,
            "nodes": [{ "id": "show", "title": "Show", "category": "effects",
                "inputs": [{ "name": "trigger", "label": "Trigger", "type": "event" }],
                "settings": [{ "key": "text", "label": "Text", "type": "text", "when": { "set": "trigger" } }] }]
        }))
        .unwrap();
        assert!(has(&node.problems(), "is for the plugin's own settings"), "{:?}", node.problems());

        assert!(
            !is_set(&Value::Null) && !is_set(&json!("")) && !is_set(&json!("  ")) && !is_set(&json!(false))
        );
        assert!(
            is_set(&json!("hype.gif")) && is_set(&json!(true)) && is_set(&json!(0)) && is_set(&json!(["a"]))
        );
    }

    #[test]
    fn when_and_emotes_settings() {
        let node = |settings: Value| -> Manifest {
            serde_json::from_value(json!({
                "id": "shower", "name": "Shower", "version": "0.1.0", "api": 1,
                "nodes": [{ "id": "shower", "title": "Shower", "category": "effects",
                    "inputs": [
                        { "name": "trigger", "label": "Trigger", "type": "event" },
                        { "name": "emotes", "label": "Emotes", "type": "emotes" }
                    ],
                    "settings": settings }]
            }))
            .unwrap()
        };
        let fine = node(json!([
            { "key": "emotes", "label": "Emotes", "type": "emotes", "count": "count" },
            { "key": "count", "label": "Count", "type": "number", "when": { "unwired": "emotes" } }
        ]));
        assert_eq!(fine.problems(), Vec::<String>::new());
        let setting = &fine.nodes[0].settings[0];
        assert_eq!(setting.default_value(), json!("random"));
        assert_eq!(setting.read(Some(&json!(["Kappa"]))), json!(["Kappa"]));
        assert_eq!(setting.read(Some(&json!(5))), json!("random"), "not emotes: the default");
        let count = &fine.nodes[0].settings[1];
        let none = Map::new();
        assert!(fine.nodes[0].shown(count, |_| false, &none));
        assert!(!fine.nodes[0].shown(count, |input| input == "emotes", &none));

        // Shown by another setting's value: a hidden one keeps its value.
        let by_shape = node(json!([
            { "key": "shape", "label": "Pieces", "type": "choice", "default": "confetti",
              "options": [{ "value": "confetti", "label": "Confetti" }, { "value": "picture", "label": "A picture" }] },
            { "key": "picture", "label": "Picture", "type": "picture", "when": { "is": { "shape": "picture" } } }
        ]));
        assert_eq!(by_shape.problems(), Vec::<String>::new());
        let (ty, picture) = (&by_shape.nodes[0], &by_shape.nodes[0].settings[1]);
        assert!(!ty.shown(picture, |_| false, &none), "the default shape");
        let picked = json!({ "shape": "picture", "picture": "a.png" });
        assert!(ty.shown(picture, |_| false, picked.as_object().unwrap()));
        let other = json!({ "shape": "confetti", "picture": "a.png" });
        assert!(!ty.shown(picture, |_| false, other.as_object().unwrap()));

        let broken = node(json!([
            { "key": "other", "label": "Other", "type": "emotes", "default": 3, "count": "nope" },
            { "key": "size", "label": "Size", "type": "number", "when": { "wired": "nope" } },
            { "key": "shape", "label": "Pieces", "type": "choice", "options": [{ "value": "a", "label": "A" }] },
            { "key": "x", "label": "X", "type": "number", "when": { "is": { "size": "3" } } },
            { "key": "y", "label": "Y", "type": "number", "when": { "is": { "shape": "b" } } }
        ]));
        let problems = broken.problems();
        for expected in [
            "stands in for an emotes input",
            "\"random\" or a list",
            "isn't a number input",
            "isn't one of its inputs",
            "\"size\", which isn't another of its choice settings",
            "\"b\" isn't one of \"shape\"'s options",
        ] {
            assert!(problems.iter().any(|problem| problem.contains(expected)), "{expected}: {problems:?}");
        }
    }

    #[test]
    fn nodes_change_the_plugins_own_settings() {
        let mut manifest = confetti();
        manifest.settings = serde_json::from_value(json!([
            { "key": "lifetime_ms", "label": "Lifetime", "type": "number", "default": 8000,
              "min": 1000, "max": 60000, "unit": "ms", "step": 500, "slider": true, "group": "Timing" },
            { "key": "secret", "label": "Secret", "type": "text" }
        ]))
        .unwrap();
        manifest.nodes[0].changes = vec!["lifetime_ms".into()];
        assert_eq!(manifest.problems(), Vec::<String>::new());
        let SettingKind::Number { unit, slider, .. } = &manifest.settings[0].kind else { panic!() };
        assert_eq!((*unit, *slider), (Unit::Seconds, true));
        let catalog = Catalog::new(vec![PluginInfo {
            manifest: manifest.clone(),
            overlay: true,
            ..Default::default()
        }]);
        let ty = catalog.node_type("confetti", "burst").unwrap();
        assert_eq!(ty.changeable.len(), 1);
        assert_eq!(ty.changeable[0].key, "lifetime_ms");

        manifest.nodes[0].changes = vec!["nope".into()];
        manifest.settings[1].kind = SettingKind::Number {
            default: 0.0,
            min: None,
            max: None,
            unit: Unit::Plain,
            step: None,
            slider: true,
        };
        let problems = manifest.problems();
        assert!(
            problems.iter().any(|problem| problem.contains("isn't one of the plugin's own settings")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|problem| problem.contains("a slider needs a min and a max")),
            "{problems:?}"
        );
    }

    #[test]
    fn queued_effects() {
        let mut manifest = confetti();
        let node = &mut manifest.nodes[0];
        assert!(!node.queued(&Map::new()));
        node.queue = QueueMode::Always;
        assert!(node.queued(&Map::new()));
        // Optional needs its tick box, which decides.
        node.queue = QueueMode::Optional;
        let problems = manifest.problems();
        assert!(problems.iter().any(|problem| problem.contains("\"queue\"")), "{problems:?}");
        let node = &mut manifest.nodes[0];
        node.settings.push(Setting {
            key: QUEUE_SETTING.into(),
            label: "Wait in the queue".into(),
            required: false,
            when: None,
            group: None,
            short: String::new(),
            caption: String::new(),
            beside: String::new(),
            icon: String::new(),
            kind: SettingKind::Bool { default: true },
        });
        assert_eq!(manifest.problems(), Vec::<String>::new());
        let node = &manifest.nodes[0];
        assert!(node.queued(&Map::new()));
        assert!(!node.queued(json!({ "queue": false }).as_object().unwrap()));
        // Only effects wait.
        let mut membership = membership();
        membership.nodes[0].queue = QueueMode::Always;
        assert!(membership.problems().iter().any(|problem| problem.contains("only effect nodes")));
    }

    #[test]
    fn page_hosts() {
        let mut manifest = confetti();
        manifest.page_hosts = vec!["fonts.googleapis.com".into(), "Not A Host".into()];
        assert!(manifest.problems().iter().any(|problem| problem.contains("Not A Host")));
        manifest.page_hosts.pop();
        let info = PluginInfo { manifest, ..Default::default() };
        assert!(allowed_page_hosts(&info, &[]).is_empty(), "until allowed");
        assert_eq!(allowed_page_hosts(&info, &["fonts.googleapis.com".into()]), ["fonts.googleapis.com"]);
        let built_in = PluginInfo { bundled: true, ..info };
        assert_eq!(
            allowed_page_hosts(&built_in, &[]),
            ["fonts.googleapis.com"],
            "a built-in's come with the app"
        );
    }

    #[test]
    fn plugin_settings_as_pages_get_them() {
        let settings: Vec<Setting> = serde_json::from_value(json!([
            { "key": "picture", "label": "Picture", "type": "picture" },
            { "key": "ding", "label": "Sound", "type": "sound", "default": "ding.mp3" },
            { "key": "size", "label": "Size", "type": "number", "default": 3 }
        ]))
        .unwrap();
        let shared = shared_settings(&settings, &Map::new(), &mut pick_nothing);
        assert_eq!(
            shared,
            *json!({ "picture": null, "ding": "/sounds/ding%2Emp3", "size": 3.0 }).as_object().unwrap()
        );
        let stored = json!({ "picture": "https://example.com/a.png" });
        assert_eq!(
            shared_settings(&settings, stored.as_object().unwrap(), &mut pick_nothing)["picture"],
            "https://example.com/a.png"
        );
        // A folder: a file picked from it, each time.
        let stored = json!({ "picture": "alerts/", "ding": "dings/" });
        let asked = std::cell::RefCell::new(Vec::new());
        let mut pick = |kind, folder: &str, every| {
            asked.borrow_mut().push((kind, folder.to_string(), every));
            if kind == MediaKind::Sound {
                return Vec::new();
            }
            let files = [format!("{folder}/hype.gif"), format!("{folder}/wow.png")];
            files[..if every { 2 } else { 1 }].to_vec()
        };
        let shared = shared_settings(&settings, stored.as_object().unwrap(), &mut pick);
        assert_eq!(shared["picture"], "/pictures/alerts%2Fhype%2Egif");
        assert_eq!(shared["ding"], Value::Null, "an empty folder is none");
        assert_eq!(
            *asked.borrow(),
            [(MediaKind::Picture, "alerts".into(), false), (MediaKind::Sound, "dings".into(), false)]
        );
        // With many: a list, of every file in a folder.
        let many: Vec<Setting> = serde_json::from_value(json!([
            { "key": "pictures", "label": "Picture", "type": "picture", "many": true },
            { "key": "file", "label": "Picture", "type": "picture", "many": true },
            { "key": "none", "label": "Picture", "type": "picture", "many": true },
            { "key": "dings", "label": "Sound", "type": "sound", "many": true }
        ]))
        .unwrap();
        let stored = json!({ "pictures": "alerts/", "file": "a.png", "dings": "dings/" });
        asked.borrow_mut().clear();
        let shared = shared_settings(&many, stored.as_object().unwrap(), &mut pick);
        assert_eq!(
            Value::Object(shared),
            json!({
                "pictures": ["/pictures/alerts%2Fhype%2Egif", "/pictures/alerts%2Fwow%2Epng"],
                "file": ["/pictures/a%2Epng"],
                "none": [],
                "dings": []
            })
        );
        assert!(asked.borrow().iter().all(|(_, _, every)| *every), "{asked:?}");
        assert_eq!(media_folder(" alerts/ "), Some("alerts"));
        assert_eq!(media_folder("alerts/hype.gif"), None);
        assert_eq!(media_folder("/"), None);
        assert!(MediaKind::Media.takes("clip.webm") && !MediaKind::Picture.takes("clip.webm"));
    }

    #[test]
    fn positions_by_name_or_x_and_y() {
        assert_eq!(Position::named("bottom_left"), Some(Position { x: 0.0, y: 100.0 }));
        assert_eq!(Position::named("center"), Some(Position { x: 50.0, y: 50.0 }));
        let custom = Position::from_value(&json!({ "x": 12, "y": 85 })).unwrap();
        assert_eq!((custom.name(), custom.label()), (None, "12 %, 85 %".to_string()));
        assert_eq!(custom.to_value(), json!({ "x": 12, "y": 85 }));
        // On one of the nine points it's saved by its name, as before.
        let top = Position::from_value(&json!({ "x": 50, "y": 0 })).unwrap();
        assert_eq!((top.to_value(), top.label()), (json!("top"), "Top".to_string()));
        assert_eq!(Position { x: 12.5, y: 0.0 }.label(), "12.5 %, 0 %");
        assert_eq!(Position::from_value(&json!({ "x": 12 })), None);
        assert_eq!(serde_json::from_value::<Position>(json!("right")).unwrap().fractions(), (1.0, 0.5));
    }

    #[test]
    fn positions_and_suggestions() {
        let position: Setting =
            serde_json::from_value(json!({ "key": "at", "label": "At", "type": "position" })).unwrap();
        assert_eq!(position.default_value(), json!("top"));
        assert_eq!(position.read(Some(&json!("bottom_right"))), json!("bottom_right"));
        assert_eq!(position.read(Some(&json!("upstairs"))), json!("top"));
        let wrong = Setting {
            kind: SettingKind::Position { default: "upstairs".into(), random: false },
            ..position.clone()
        };
        assert!(settings_problems("x", &[wrong])[0].contains("isn't a position"));
        // Anywhere between the nine points, as x and y in %.
        assert_eq!(position.read(Some(&json!({ "x": 12, "y": 85 }))), json!({ "x": 12, "y": 85 }));
        assert_eq!(position.read(Some(&json!({ "x": 120, "y": 85 }))), json!("top"));
        assert_eq!(position.read(Some(&json!("random"))), json!("top"), "only where it's allowed");
        let anywhere: Setting = serde_json::from_value(
            json!({ "key": "at", "label": "At", "type": "position", "default": "random", "random": true }),
        )
        .unwrap();
        assert_eq!(anywhere.read(Some(&json!("random"))), json!("random"));
        assert!(settings_problems("x", std::slice::from_ref(&anywhere)).is_empty());
        let font: Setting = serde_json::from_value(json!({ "key": "font", "label": "Font", "type": "text",
            "suggestions": ["Arial"], "placeholder": "The system's" }))
        .unwrap();
        assert!(matches!(&font.kind, SettingKind::Text { suggestions, .. } if suggestions == &["Arial"]));
    }

    #[test]
    fn presets() {
        let preset = Setting {
            key: "style".into(),
            label: "Style".into(),
            required: false,
            when: None,
            group: None,
            short: String::new(),
            caption: String::new(),
            beside: String::new(),
            icon: String::new(),
            kind: SettingKind::Preset {},
        };
        let mut manifest = confetti();
        manifest.nodes[0].settings.push(preset.clone());
        let problems = manifest.problems();
        assert!(problems.iter().any(|problem| problem.contains("has no \"presets\"")), "{problems:?}");
        manifest.presets = Some(PresetsSpec { label: "Style".into() });
        assert_eq!(manifest.problems(), Vec::<String>::new());
        assert_eq!(manifest.nodes[0].preset_setting().map(|setting| setting.key.as_str()), Some("style"));
        // Only nodes pick one, once.
        manifest.nodes[0].settings.push(Setting { key: "other".into(), ..preset.clone() });
        manifest.settings.push(Setting { key: "wide".into(), ..preset });
        let problems = manifest.problems();
        assert!(problems.iter().any(|problem| problem.contains("only one setting")), "{problems:?}");
        assert!(problems.iter().any(|problem| problem.contains("are for nodes")), "{problems:?}");
        // Its runs get every plugin setting.
        let mut manifest = confetti();
        manifest.presets = Some(PresetsSpec { label: "Style".into() });
        manifest.settings = manifest.nodes[0].settings.clone();
        let catalog = Catalog::new(vec![PluginInfo { manifest, ..Default::default() }]);
        assert_eq!(catalog.node_type("confetti", "burst").unwrap().preset_settings.len(), 3);

        // Stored: none yet is one, "Default"; an unknown id is the first.
        let stored = crate::config::PluginConfig::default();
        assert_eq!(stored.presets()[0].name, "Default");
        let mut stored = stored;
        stored.presets_mut().push(crate::config::Preset {
            id: "loud".into(),
            name: "Loud".into(),
            settings: Map::new(),
        });
        assert_eq!(stored.preset("loud").name, "Loud");
        assert_eq!(stored.preset("gone").id, crate::config::DEFAULT_PRESET);
    }

    #[test]
    fn text_inputs_read_aloud() {
        let text = |name: &str, ty: SocketType, speak: bool| SocketSpec {
            name: name.into(),
            label: name.into(),
            ty,
            speak,
        };
        let mut manifest = confetti();
        manifest.nodes[0].inputs.push(text("read", SocketType::Text, true));
        assert_eq!(manifest.problems(), Vec::<String>::new());
        assert_eq!(manifest.nodes[0].spoken_input().map(|input| input.name.as_str()), Some("read"));
        // One per node, and only text.
        manifest.nodes[0].inputs.push(text("also", SocketType::Text, true));
        manifest.nodes[0].inputs.push(text("count", SocketType::Number, true));
        let problems = manifest.problems();
        assert!(problems.iter().any(|problem| problem.contains("only one input")), "{problems:?}");
        assert!(problems.iter().any(|problem| problem.contains("only a text input")), "{problems:?}");
        let spoken: SocketSpec =
            serde_json::from_value(json!({ "name": "read", "label": "Read", "type": "text", "speak": true }))
                .unwrap();
        assert!(spoken.speak);
    }

    #[test]
    fn an_icon_is_a_file_or_the_svg() {
        let with = |icon: &str| Manifest { icon: icon.into(), ..confetti() }.problems();
        assert_eq!(with("icon.svg"), Vec::<String>::new());
        assert_eq!(with("art/Icon.SVG"), Vec::<String>::new());
        assert_eq!(with(" <svg viewBox=\"0 0 24 24\"><circle r=\"9\"/></svg>"), Vec::<String>::new());
        assert_eq!(with(" <svg viewBox=\"0 0 24 24\"><circle r=\"9\"/></svg>").len(), 0);
        for bad in ["icon.png", "../icon.svg", "/icon.svg", "C:/icon.svg", "C:icon.svg", "art\\icon.svg"] {
            assert!(with(bad)[0].contains("an SVG file in its folder"), "{bad}");
        }
        assert!(with(&format!("<svg>{}</svg>", " ".repeat(MAX_ICON_BYTES)))[0].contains("at most 16 KB"));
        assert_eq!(icon_path("vtube"), "/plugin-icons/vtube");
    }

    #[test]
    fn a_tab_has_a_short_label_and_one_of_its_page_files() {
        let manifest: Manifest = serde_json::from_value(json!({
            "id": "requests", "name": "Media requests", "version": "0.1.0", "api": 1,
            "tab": { "label": "Requests", "page": "tab.html", "count": true }
        }))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        let tab = manifest.tab.clone().unwrap();
        assert!(tab.count);
        assert_eq!(tab_path("requests", &tab), "/plugins/requests/tab.html");
        let with = |label: &str, page: &str| {
            let tab = TabSpec { label: label.into(), page: page.into(), count: false };
            Manifest { tab: Some(tab), ..manifest.clone() }.problems()
        };
        assert_eq!(with("Sixteen letters!", "pages/tab.html"), Vec::<String>::new());
        for label in ["", "  ", " Requests", "Seventeen letters"] {
            assert!(with(label, "tab.html")[0].contains("1 to 16 characters"), "{label:?}");
        }
        for page in ["", "../tab.html", "/tab.html", "C:tab.html", "pages\\tab.html"] {
            assert!(with("Requests", page)[0].contains("one of its page files"), "{page:?}");
        }
        // A count leaves the manifest as it was when there's none.
        let plain = serde_json::to_value(TabSpec { label: "A".into(), page: "a.html".into(), count: false });
        assert_eq!(plain.unwrap(), json!({ "label": "A", "page": "a.html" }));
        let sent: TabPageMessage =
            serde_json::from_value(json!({ "type": "send", "data": { "add": 1 } })).unwrap();
        assert_eq!(sent, TabPageMessage::Send { data: json!({ "add": 1 }) });
    }

    fn membership() -> Manifest {
        serde_json::from_value(json!({
            "id": "billybilly",
            "name": "BillyBilly",
            "version": "0.1.0",
            "api": 1,
            "nodes": [{
                "id": "membership",
                "title": "BillyBilly membership",
                "category": "events",
                "outputs": [
                    { "name": "event", "label": "Event", "type": "event" },
                    { "name": "viewer", "label": "Viewer", "type": "text" },
                    { "name": "months", "label": "Months", "type": "number" }
                ]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn the_catalog_makes_nodes_with_defaults() {
        let catalog =
            Catalog::new(vec![PluginInfo { manifest: confetti(), overlay: true, ..Default::default() }]);
        assert!(catalog.node_type("confetti", "burst").is_some());
        assert!(catalog.node_type("confetti", "nope").is_none());
        let kinds = catalog.node_kinds();
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].title(), "Confetti");
        assert_eq!(kinds[0].number_setting("pieces"), Some(150.0));

        // In the manifest's order, not by id.
        let mut manifest = confetti();
        let mut started = manifest.nodes[0].clone();
        started.id = "started".into();
        let mut ended = manifest.nodes[0].clone();
        ended.id = "ended".into();
        manifest.nodes = vec![started, ended];
        let catalog = Catalog::new(vec![PluginInfo { manifest, overlay: true, ..Default::default() }]);
        let ids: Vec<String> = catalog
            .node_kinds()
            .iter()
            .filter_map(|kind| match kind {
                crate::cards::NodeKind::Plugin { node, .. } => Some(node.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["started", "ended"]);
    }

    #[test]
    fn run_messages_and_text_values() {
        let message = PluginMessage::Run {
            node: "burst".into(),
            input: "trigger".into(),
            values: json!({ "pieces": 3 }).as_object().unwrap().clone(),
            changes: Default::default(),
            settings: Default::default(),
            queue_id: None,
            run_id: None,
            preview: None,
        };
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({ "type": "run", "node": "burst", "input": "trigger", "values": { "pieces": 3 } })
        );
        let queued = PluginMessage::Run {
            node: "burst".into(),
            input: "trigger".into(),
            values: Map::new(),
            changes: Map::new(),
            settings: Default::default(),
            queue_id: Some(7),
            run_id: None,
            preview: None,
        };
        assert_eq!(serde_json::to_value(&queued).unwrap()["queue_id"], json!(7));
        let done: PageMessage = serde_json::from_value(json!({ "type": "done", "queue_id": 7 })).unwrap();
        assert_eq!(done, PageMessage::Done { queue_id: 7 });
        let text = TextValue {
            text: "hi Kappa".into(),
            parts: vec![
                TextPart::Text { text: "hi ".into(), style: None },
                TextPart::Emote {
                    emote: EmoteValue {
                        code: "Kappa".into(),
                        url: "/emote/twitch/25".into(),
                        overlays: vec![],
                    },
                    style: None,
                },
            ],
            speech: None,
        };
        assert_eq!(
            serde_json::to_value(&text).unwrap(),
            json!({ "text": "hi Kappa", "parts": [
                { "type": "text", "text": "hi " },
                { "type": "emote", "code": "Kappa", "url": "/emote/twitch/25", "overlays": [] }
            ] })
        );
        assert_eq!(page_path("confetti"), "/plugins/confetti/index.html");
        let message = PluginMessage::Message { data: json!({ "count": 3 }) };
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({ "type": "message", "data": { "count": 3 } })
        );
    }

    #[test]
    fn hosts() {
        let allowed = vec!["discord.com".to_string(), "*.example.com".to_string()];
        assert!(host_allowed(&allowed, "discord.com"));
        assert!(host_allowed(&allowed, "Discord.com."));
        assert!(!host_allowed(&allowed, "evil-discord.com"));
        assert!(!host_allowed(&allowed, "discord.com.evil.net"));
        assert!(host_allowed(&allowed, "api.example.com"));
        assert!(host_allowed(&allowed, "a.b.example.com"));
        assert!(!host_allowed(&allowed, "example.com"), "subdomains only");
        assert!(!host_allowed(&allowed, "badexample.com"));
        let mut manifest = confetti();
        manifest.hosts = vec!["Discord.com".into(), "https://x.com".into(), "*.ok.net".into()];
        let problems = manifest.problems();
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn secret_settings_stay_out_of_the_config_and_pages() {
        let mut manifest = confetti();
        manifest.settings = serde_json::from_value(json!([
            { "key": "greeting", "label": "Greeting", "type": "text", "default": "hi" },
            { "key": "password", "label": "Password", "type": "secret" }
        ]))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        assert_eq!(secret_settings(&manifest).collect::<Vec<_>>(), ["password"]);
        // A value in the config is never read.
        assert_eq!(manifest.settings[1].read(Some(&json!("leaked"))), json!(""));
        let stored = json!({ "password": "leaked" });
        let shared = shared_settings(&manifest.settings, stored.as_object().unwrap(), &mut pick_nothing);
        assert_eq!(Value::Object(shared), json!({ "greeting": "hi" }));

        let mut on_a_node = confetti();
        on_a_node.nodes[0].settings.push(manifest.settings[1].clone());
        let problems = on_a_node.problems();
        assert!(
            problems.iter().any(|problem| problem.contains("a secret setting is the plugin's own")),
            "{problems:?}"
        );
    }

    #[test]
    fn event_nodes_filter_by_their_text_outputs() {
        let node = |settings: Value| -> NodeType {
            serde_json::from_value(serde_json::json!({
                "id": "changed", "title": "Changed", "category": "events",
                "outputs": [
                    { "name": "event", "label": "Event", "type": "event" },
                    { "name": "scene", "label": "Scene", "type": "text" },
                    { "name": "count", "label": "Count", "type": "number" }
                ],
                "settings": settings
            }))
            .unwrap()
        };
        let scenes =
            node(serde_json::json!([{ "key": "scene", "label": "Scenes", "type": "text", "many": true }]));
        assert!(scenes.problems().is_empty(), "{:?}", scenes.problems());
        let setting = &scenes.settings[0];
        assert_eq!(setting.read(None), serde_json::json!([]));
        assert_eq!(setting.read(Some(&serde_json::json!(["A", "B"]))), serde_json::json!(["A", "B"]));
        assert_eq!(setting.read(Some(&serde_json::json!("A"))), serde_json::json!([]), "a list or nothing");
        assert!(scenes.is_filter(setting));
        // Its other settings are for its server code, of any kind, a list
        // or not.
        for other in [
            serde_json::json!([{ "key": "other", "label": "Other", "type": "text", "many": true }]),
            serde_json::json!([{ "key": "count", "label": "Count", "type": "text" }]),
            serde_json::json!([{ "key": "scene", "label": "Scene", "type": "bool" }]),
        ] {
            let node = node(other);
            assert_eq!(node.problems(), Vec::<String>::new());
            assert!(!node.is_filter(&node.settings[0]), "{:?}", node.settings[0].key);
        }

        let wanted = filter_values(&serde_json::json!([" Gaming ", "", "BRB"]));
        assert_eq!(wanted, ["Gaming", "BRB"]);
        assert!(passes_filter(&wanted, "gaming"));
        assert!(!passes_filter(&wanted, "Just chatting"));
        assert!(passes_filter(&[], "anything"));
        assert_eq!(filter_values(&serde_json::json!("One")), ["One"]);
    }

    #[test]
    fn statuses_are_a_few_words() {
        assert_eq!(PluginStatus::new(StatusLevel::Ok, "  "), None, "empty: no status");
        let ok = PluginStatus::new(StatusLevel::Ok, " Connected ").unwrap();
        assert_eq!(ok.text, "Connected");
        let long = PluginStatus::new(StatusLevel::Problem, &"x".repeat(50)).unwrap();
        assert_eq!(long.text.chars().count(), MAX_STATUS_CHARS + 1, "cut, with …");
        assert!(long.text.ends_with('…'));
    }

    #[test]
    fn notes_are_lines_within_bounds() {
        assert_eq!(PluginNote::new(StatusLevel::Ok, &[]), None, "no lines: no note");
        assert_eq!(PluginNote::new(StatusLevel::Ok, &[" ".into()]), None);
        let note = PluginNote::new(StatusLevel::Problem, &[" Chat ".into(), String::new(), "x".repeat(400)])
            .unwrap();
        assert_eq!(note.lines.len(), 2, "empty lines left out");
        assert_eq!(note.lines[0], "Chat");
        assert_eq!(note.lines[1].chars().count(), MAX_NOTE_LINE_CHARS + 1, "cut, with …");
        let many: Vec<String> = (0..30).map(|line| line.to_string()).collect();
        assert_eq!(PluginNote::new(StatusLevel::Ok, &many).unwrap().lines.len(), MAX_NOTE_LINES);
    }

    #[test]
    fn effect_nodes_outputs_are_events() {
        let mut manifest = confetti();
        manifest.nodes[0].outputs =
            serde_json::from_value(json!([{ "name": "done", "label": "Done", "type": "event" }])).unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        manifest.nodes[0].outputs.push(
            serde_json::from_value(json!({ "name": "count", "label": "Count", "type": "number" })).unwrap(),
        );
        let problems = manifest.problems();
        assert!(problems.iter().any(|problem| problem.contains("outputs are events")), "{problems:?}");

        // Started and Ended are the queue's on a node that can wait in it.
        let mut manifest = confetti();
        manifest.nodes[0].outputs =
            serde_json::from_value(json!([{ "name": "ended", "label": "Ended", "type": "event" }])).unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        manifest.nodes[0].queue = QueueMode::Always;
        let problems = manifest.problems();
        assert!(problems.iter().any(|problem| problem.contains("the queue's outputs")), "{problems:?}");
    }

    #[test]
    fn suggestions_from_server_code() {
        let mut manifest = confetti();
        manifest.settings = serde_json::from_value(json!([
            { "key": "scene", "label": "Scene", "type": "text", "suggest_from": "scenes" },
            { "key": "source", "label": "Source", "type": "text", "suggest_from": "sources/{scene}" },
            { "key": "filter", "label": "Filter", "type": "text", "suggest_from": "filters/{nope}" }
        ]))
        .unwrap();
        let problems = manifest.problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("{nope}"), "{problems:?}");

        let scene = |key: &str| (key == "scene").then(|| "Gaming".to_string());
        assert_eq!(options_list("scenes", scene).as_deref(), Some("scenes"));
        assert_eq!(options_list("sources/{scene}", scene).as_deref(), Some("sources/Gaming"));
        assert_eq!(options_list("sources/{scene}", |_| Some(String::new())), None, "no scene picked");
        assert_eq!(options_list("", scene), None);
    }

    #[test]
    fn built_in_plugins_hosts_come_allowed() {
        let mut manifest = confetti();
        manifest.hosts = vec!["localhost".into(), "127.0.0.1".into()];
        let mut info = PluginInfo { manifest, server: true, ..Default::default() };
        assert!(!server_hosts_allowed(&info, &[]));
        assert!(!server_hosts_allowed(&info, &["localhost".into()]));
        assert!(server_hosts_allowed(&info, &["localhost".into(), "127.0.0.1".into()]));
        info.bundled = true;
        assert!(server_hosts_allowed(&info, &[]));
    }
}
