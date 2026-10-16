//! The control panel ↔ server WebSocket ([`PANEL_WS_PATH`]): the config,
//! live status, and the panel's requests.

use serde::{Deserialize, Serialize};

use crate::activity::{HistoryEntry, QueueAction, QueueGroup, QueueState};
use crate::config::AppConfig;
use crate::encode_segment;

/// WebSocket endpoint the panel connects to.
pub const PANEL_WS_PATH: &str = "/panel/ws";
/// Where the panel itself is served (trunk's public URL).
pub const PANEL_PATH: &str = "/panel/";

/// Where the panel uploads a picture for alerts (`POST`, the bytes as the
/// body, with [`UPLOAD_HEADER`]), in axum path syntax.
pub const PICTURE_UPLOAD_ROUTE: &str = "/panel/pictures/{name}";

/// A header only the panel sends with uploads: a web page elsewhere can't
/// send it to localhost without the server agreeing (CORS).
pub const UPLOAD_HEADER: &str = "x-lsk-upload";

pub fn picture_upload_path(name: &str) -> String {
    format!("/panel/pictures/{}", encode_segment(name))
}

/// Where the panel uploads a sound, like [`PICTURE_UPLOAD_ROUTE`].
pub const SOUND_UPLOAD_ROUTE: &str = "/panel/sounds/{name}";

pub fn sound_upload_path(name: &str) -> String {
    format!("/panel/sounds/{}", encode_segment(name))
}

/// Where the panel uploads a plugin as a zip to install it (`POST`, with
/// [`UPLOAD_HEADER`]). Answers the plugin's id, or why it can't.
pub const PLUGIN_INSTALL_PATH: &str = "/panel/plugins";

/// Panel → server.
// Each lives for one message; boxing the config would only make building
// and matching them clumsier.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PanelRequest {
    /// Replace the whole config. Answered with `Saved`, `SaveFailed` or
    /// `SaveOutdated`; every panel then gets the new `Config`.
    SaveConfig {
        config: AppConfig,
        /// The `revision` of the `Config` these edits were made on: if the
        /// config has changed since (the control API, another panel, a
        /// hand edit), nothing is saved and the answer is `SaveOutdated`.
        /// `None` saves regardless.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
    },
    /// Reads the plugins folder again: new, changed and removed plugins
    /// take effect without a restart. Every panel gets `Plugins`.
    ReloadPlugins,
    /// Deletes a plugin's folder from the plugins folder (not a built-in
    /// one), then reloads.
    RemovePlugin { id: String },
    /// Forgets a program's plugin that isn't connected (its nodes in cards
    /// then count as missing).
    ForgetProgram { id: String },
    /// Start Twitch's device code login (after a cancel or log out).
    StartLogin,
    /// Stop waiting for the current login code.
    CancelLogin,
    /// Open the current login code's link in the default browser of the
    /// computer Local Stream Kit runs on. A link in the panel would open
    /// inside OBS when the panel is a dock, without the Twitch login.
    OpenLogin,
    /// Forget the Twitch login and disconnect channel points.
    Logout,
    /// Replaces the control API's key: what uses the old one stops working.
    NewControlKey,
    /// Fire a fake event through the cards, as if it happened on Twitch.
    /// With a card's id, that card reacts even while it's off ("Test this
    /// card"); cards that are on react either way.
    Test {
        event: TestEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card: Option<String>,
    },
    /// The pictures alerts can show; answered with `Pictures`.
    Pictures,
    /// The sounds there are; answered with `Sounds`.
    Sounds,
    /// Deletes a file in the sounds folder (not in one of its folders);
    /// answered with `Sounds`, or `SaveFailed`. Its label goes with the
    /// config the panel saves.
    DeleteSound { name: String },
    /// Downloads what text to speech needs (progress in `Status`).
    InstallSpeech,
    /// Reads a text on the overlay with these settings (unsaved edits
    /// included), straight away, not in the queue (the Speak box's Try:
    /// Say it, a voice picked); one before it that's still being made is
    /// dropped, and the Speak page stops one it's still reading. Panels
    /// hear `SpeechPreview` meanwhile.
    PreviewSpeech { text: String, settings: crate::SpeechSettings },
    /// Runs a plugin's effect node on the overlay with these values and
    /// settings, as its Plugins box's preview does (in the queue if the
    /// node waits in it).
    PreviewPlugin {
        plugin: String,
        /// Empty: the page draws its samples (`PluginMessage::Samples`).
        node: String,
        values: serde_json::Map<String, serde_json::Value>,
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        settings: serde_json::Map<String, serde_json::Value>,
    },
    /// The emotes the chat channel can use, for completing emote codes.
    /// Answered with `Emotes`.
    Emotes,
    /// Loads every emote set again now (7TV, BTTV, FFZ and, with a login,
    /// Twitch's own) instead of at the next refresh.
    ResyncEmotes,
    /// Pause, resume, skip, clear or mute the queue. `only` limits a clear
    /// to one group.
    Queue {
        action: QueueAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only: Option<QueueGroup>,
    },
    /// Skips one queue item by its id ([`QueueItem::id`]): ends it if
    /// it's on stage, drops it if it's waiting. One that's gone already
    /// (played, skipped, cleared) is left alone, so a click that lands
    /// just after the queue moved on never skips the item after it.
    ///
    /// [`QueueItem::id`]: crate::activity::QueueItem::id
    SkipItem { id: u64 },
    /// Moves a waiting queue item to the front, by its id, so it plays
    /// once what's on stage ends. One that's gone already, or on stage,
    /// is left alone, as with [`PanelRequest::SkipItem`].
    PlayNext { id: u64 },
    /// Forgets the whole history (the queue isn't touched).
    ClearHistory,
    /// Moves the history's seen marker: under entry `up_to` (it and every
    /// older one are seen), or, with none, under the newest (All seen).
    MarkSeen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        up_to: Option<u64>,
    },
    /// Sends a history entry's event through the cards again, as they are
    /// now (a new entry, marked as a replay).
    Replay { entry: u64 },
    /// Opens a link a viewer wrote, in the Activity tab, in the default
    /// browser of the computer Local Stream Kit runs on, as `OpenLogin`: a
    /// link in an OBS dock would open inside OBS. Only a link that's where
    /// `from` says is opened.
    OpenLink { from: LinkFrom, url: String },
    /// Saves one of the user's looks of a plugin's file setting (a copy,
    /// new or not); `None` deletes it. Every panel then gets `PluginFiles`;
    /// a failure is answered with `SaveFailed`. (Which look is picked is
    /// the setting's value, in the config.)
    SavePluginLook {
        plugin: String,
        key: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// Renames one of the user's looks (its file); as `SavePluginLook`.
    RenamePluginLook { plugin: String, key: String, name: String, to: String },
    /// Saves a plugin's secret setting (in `lsk_secrets.json`, not the
    /// config); `None` (or empty) forgets it. Every panel then gets
    /// `PluginSecrets`; a failure is answered with `SaveFailed`.
    SavePluginSecret {
        plugin: String,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    /// Opens the logs folder in Explorer, on the PC the app runs on.
    OpenLogs,
    /// Empties the Status page's list of recent warnings and errors.
    ClearLogLines,
    /// Downloads the newer version that's out and puts it in the exe's
    /// place, for the next start (progress in `Status`'s `update`).
    DownloadUpdate,
    /// Opens one of the app's pages (the release's, the licence, support)
    /// in the default browser of the PC the app runs on, as `OpenLink`.
    OpenAppLink { link: crate::update::AppLink },
    /// Writes the Stream Deck plugin built into the app to the cache folder
    /// and opens it on the PC the app runs on, so Stream Deck installs it
    /// (or updates the one it has). A download wouldn't work in an OBS
    /// dock. A failure is in `Status`'s `stream_deck`.
    InstallStreamDeck,
    /// The viewers the chat log has seen whose name has `text` in it
    /// (Find a viewer); answered with `ViewersFound`.
    FindViewers { text: String },
    /// Opens a viewer's card: by id (a name in Activity, a viewer found),
    /// else by name (typed). Answered with `Viewer`, then `ViewerInfo` once
    /// Twitch has answered; or with `ViewerMissing`. While it's open, this
    /// panel gets their new log lines (`ViewerLine`).
    OpenViewer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
    },
    /// The open card's log lines before the first `before` (its `older`);
    /// answered with `ViewerOlder`.
    OlderViewerLines { id: String, before: usize },
    /// The card was closed: no more of its lines.
    CloseViewer,
    /// Does something to a viewer (timeout, ban, …); answered with
    /// `Moderated`, then a new `ViewerInfo`.
    Moderate { id: String, action: crate::viewers::ViewerAction },
    /// Opens Twitch's own viewer card for `login`, in the chat's channel, in
    /// the PC's browser (as `OpenLink`): all of their messages there.
    OpenTwitchCard { login: String },
}

/// Where a link the panel opens is (`PanelRequest::OpenLink`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkFrom {
    /// History entry `id`: the viewer's message, or what its items showed
    /// or said ([`HistoryEntry::links`]).
    Entry(u64),
    /// Queue item `id` ([`QueueItem::id`]): what it shows or says, while
    /// it's in the queue, or as its history entry kept it after.
    ///
    /// [`QueueItem::id`]: crate::activity::QueueItem::id
    Item(u64),
}

/// Server → panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PanelEvent {
    /// The panel build the server is serving: sent first on every connect,
    /// and again when it changes (a rebuild during development). A panel
    /// running a different build reloads itself once its edits are saved.
    Build {
        id: String,
    },
    /// On connect, and whenever the config changes (from any panel, or the
    /// file being edited by hand). Plugin nodes come without their types:
    /// the panel attaches them from `Plugins`.
    Config {
        config: AppConfig,
        /// Changes with every change to the config (it starts from the
        /// clock, so it differs after a restart too).
        revision: u64,
    },
    /// On connect: the installed plugins, and the plugin folders that
    /// didn't load, with why.
    Plugins {
        plugins: Vec<crate::plugin::PluginInfo>,
        failures: Vec<crate::plugin::PluginFailure>,
    },
    Status {
        status: Status,
    },
    /// A plugin couldn't be removed or forgotten (for the Plugins page).
    PluginProblem {
        message: String,
    },
    /// The config was saved, as `revision`.
    Saved {
        revision: u64,
    },
    /// The config wasn't saved: it changed since the revision the save was
    /// made on. The panel puts its edits on top of the newer `Config` and
    /// saves again.
    SaveOutdated,
    /// The config wasn't saved; nothing changed.
    SaveFailed {
        problems: Vec<String>,
    },
    /// Every emote code the chat channel can use, once each, sorted
    /// ignoring case.
    Emotes {
        emotes: Vec<KnownEmote>,
    },
    /// The files in the pictures folder, sorted ignoring case. Each is at
    /// [`picture_path`](crate::alert::picture_path). And the folders in
    /// it, which a setting can name to get one of their files at random
    /// ([`media_folder`](crate::plugin::media_folder)).
    Pictures {
        files: Vec<String>,
        #[serde(default)]
        folders: Vec<MediaFolder>,
    },
    /// The files in the sounds folder, sorted ignoring case. Each is at
    /// [`sound_path`](crate::sound::sound_path). And its folders, as for
    /// pictures.
    Sounds {
        files: Vec<String>,
        #[serde(default)]
        folders: Vec<MediaFolder>,
    },
    /// Whether a `PreviewSpeech` is being made or read (until it would
    /// have ended): on connect, and when that changes.
    SpeechPreview {
        reading: bool,
    },
    /// On connect, and whenever the queue changes.
    Queue {
        queue: QueueState,
    },
    /// The cards changed and the waiting events were prepared again: what
    /// that changed in the queue (only when it changed something).
    QueuePrepared {
        prepared: crate::activity::Prepared,
    },
    /// The whole history, newest first: on connect (and after the panel
    /// fell behind, or the history was cleared).
    History {
        entries: Vec<HistoryEntry>,
    },
    /// A new history entry, or one that changed (its cards, its items'
    /// states): it replaces the entry with its id, or goes on top.
    HistoryEntry {
        entry: HistoryEntry,
    },
    /// Where the history's seen marker is: under entry `up_to`, the newest
    /// marked seen (the newer ones are new). On connect, and when it moves.
    HistorySeen {
        up_to: u64,
    },
    /// Every plugin's file settings' looks, by plugin and key (the
    /// plugin's own first, then the user's copies by name): on connect, and
    /// whenever one changes (saved from a panel, or edited by hand).
    PluginFiles {
        files: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, Vec<crate::plugin::PluginLook>>,
        >,
    },
    /// Which plugins' secret settings have a value saved, by plugin (the
    /// keys): on connect, and whenever one is saved or forgotten. Never
    /// the values.
    PluginSecrets {
        saved: std::collections::BTreeMap<String, Vec<String>>,
    },
    /// The lists of choices plugins' server code filled in
    /// (`host.set-options`, `host.set-choices`), by plugin and list name:
    /// on connect, and whenever one changes. Text settings with
    /// `suggest_from` offer them (showing the label, storing the value).
    PluginOptions {
        options: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, Vec<crate::plugin::Choice>>,
        >,
    },
    /// The viewers whose name has `text` in it, the most recently seen
    /// first (at most [`FOUND_LIMIT`](crate::viewers::FOUND_LIMIT)).
    ViewersFound {
        text: String,
        viewers: Vec<crate::viewers::FoundViewer>,
    },
    /// A viewer's card (`OpenViewer`).
    Viewer {
        card: crate::viewers::ViewerCard,
    },
    /// No card for `name`: why, for people.
    ViewerMissing {
        name: String,
        problem: String,
    },
    /// What Twitch says about the viewer with card `id`; or why it can't be
    /// asked.
    ViewerInfo {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        info: Option<crate::viewers::ViewerInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    /// Older lines of card `id`'s log (`OlderViewerLines`), oldest first,
    /// and how many come before them.
    ViewerOlder {
        id: String,
        lines: Vec<crate::viewers::LogLine>,
        older: usize,
    },
    /// A new line in the open card's log.
    ViewerLine {
        id: String,
        line: crate::viewers::LogLine,
    },
    /// One of the open card's messages was taken out of the chat.
    ViewerMessageDeleted {
        id: String,
        message: String,
    },
    /// What came of a `Moderate`: `problem` says why it didn't happen.
    Moderated {
        id: String,
        action: crate::viewers::ViewerAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
}

/// A folder in the pictures (sounds) folder, with its files (sorted
/// ignoring case; each at its path as `"{name}/{file}"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFolder {
    pub name: String,
    pub files: Vec<String>,
}

impl MediaFolder {
    /// How a setting names it: `"alerts/"`.
    pub fn value(&self) -> String {
        format!("{}/", self.name)
    }
}

/// An emote the channel can use. Its image is at
/// [`emote_path`](crate::emote_path)`(source, id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownEmote {
    pub code: String,
    /// `twitch`, `7tv`, `bttv` or `ffz`.
    pub source: String,
    pub id: String,
}

/// Everything the Status page shows, sent whole whenever any of it changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub chat: ChatStatus,
    pub login: LoginStatus,
    pub overlays: Vec<OverlayStatus>,
    /// Emotes known for the chat channel, by source (`7tv`, `bttv`, `ffz`).
    pub emotes: Vec<EmoteCount>,
    /// Global emotes from all sources together.
    pub global_emotes: usize,
    /// The channel's channel point rewards (titles), once logged in.
    pub rewards: Vec<String>,
    /// Saved connection settings that only apply after a restart.
    pub restart_needed: Vec<String>,
    /// Set when `lsk.json` was edited by hand into something that
    /// can't be used; the previous settings stay in use.
    pub config_error: Option<String>,
    pub speech: crate::speech::SpeechStatus,
    /// The control API's key (see `control`); panels are on this PC.
    #[serde(default)]
    pub control_key: String,
    #[serde(default)]
    pub log: LogStatus,
    /// The app's version (`0.1.0`).
    #[serde(default)]
    pub version: String,
    /// The update check and download.
    #[serde(default)]
    pub update: crate::update::UpdateStatus,
    /// The Stream Deck plugin: built in or not, and the ones connected.
    #[serde(default)]
    pub stream_deck: crate::streamdeck::StreamDeckStatus,
    /// A second login waiting for approval while the current one goes on
    /// working (more permissions ticked; 2026-10-16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal: Option<LoginCode>,
}

/// A login's device code: where to approve it, and until when it works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginCode {
    pub url: String,
    pub code: String,
    /// Unix ms when the code stops working (a new one follows).
    pub expires_at_ms: u64,
}

/// The log, for the Status page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogStatus {
    /// The folder its files are in, as a path on this PC.
    pub folder: String,
    /// `RUST_LOG`, when it's set: it decides what's logged, not the
    /// Detailed log setting.
    pub rust_log: Option<String>,
    /// The latest warnings and errors, newest first.
    pub recent: Vec<LogLine>,
}

/// A warning or error from the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    /// When it was last logged (ms since the Unix epoch).
    pub at_ms: u64,
    pub error: bool,
    /// The message, then its fields: "plugin not loaded (plugin: foo)".
    pub text: String,
    /// How many times in a row it was logged.
    pub count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChatStatus {
    /// No channel set.
    #[default]
    Off,
    Connecting {
        channel: String,
    },
    Connected {
        channel: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoginStatus {
    /// No login connection: a recording is being replayed.
    #[default]
    Off,
    /// Not logged in, and not trying (cancelled or logged out).
    LoggedOut,
    /// Checking the saved login with Twitch.
    Checking,
    /// Waiting for the broadcaster to approve `code` at `url`.
    Waiting {
        url: String,
        code: String,
        /// Unix ms when the code stops working (a new one follows).
        expires_at_ms: u64,
        /// Why a login is needed, e.g. the saved one stopped working.
        reason: Option<String>,
    },
    /// Logged in; `listening` once its EventSub connection is up (or not
    /// needed: no event ticked), `points` once the redemption subscription
    /// is, and `follows` and `hype_train` once those are too (Twitch can
    /// refuse either alone). `granted`: what the login's scopes allow.
    LoggedIn {
        login: String,
        listening: bool,
        #[serde(default)]
        points: bool,
        follows: bool,
        #[serde(default)]
        hype_train: bool,
        #[serde(default = "crate::config::TwitchPermissions::all")]
        granted: crate::config::TwitchPermissions,
    },
    /// Using the Twitch CLI's mock server instead of Twitch.
    TwitchCli { address: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayStatus {
    /// The overlay its link names (`config::Overlay`'s id; maybe one
    /// that isn't on the list); `None` for a link naming plugins with
    /// `?modules=`.
    #[serde(default)]
    pub overlay: Option<String>,
    /// The plugins whose pages it hosts.
    pub plugins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmoteCount {
    pub source: String,
    pub count: usize,
}

/// A fake event for testing cards without going live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TestEvent {
    /// `months` 1 = a new sub, more = a resub.
    Sub {
        months: u32,
        /// What the subscriber wrote with it; empty = nothing.
        #[serde(default)]
        message: String,
    },
    GiftedSub,
    GiftBomb {
        count: u32,
    },
    Bits {
        amount: u64,
        /// The cheer's message.
        #[serde(default)]
        message: String,
    },
    Raid {
        viewers: u32,
    },
    Reward {
        reward: String,
        text: String,
    },
    /// A chat message; emote codes in it are recognised as in real chat.
    Chat {
        text: String,
        /// Who sends it (the first viewer, when there are several);
        /// empty: TestViewer.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        viewer: String,
        /// The same message from this many different viewers (for combos
        /// and chants).
        #[serde(default = "one")]
        viewers: u32,
        /// Marked as each viewer's first message in the channel.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        first: bool,
    },
    Custom {
        name: String,
    },
    Follow,
    /// A made-up hype train at `level`.
    HypeTrain {
        change: TestHypeTrain,
        level: u32,
    },
    /// The tested card's Timer nodes tick once ("Test this card"); nothing
    /// without a card.
    Timer,
    /// A plugin's event node, with sample values by output name.
    Plugin {
        plugin: String,
        node: String,
        #[serde(default)]
        values: serde_json::Map<String, serde_json::Value>,
    },
}

/// What happens to a test hype train.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestHypeTrain {
    Start,
    Progress,
    LevelUp,
    End,
    /// A whole short train, a step every couple of seconds: it starts,
    /// someone contributes, it goes up a level and it ends ("Test this
    /// card", so a test doesn't leave a bar up for Twitch's 5 minutes).
    Whole,
}

fn one() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let request = PanelRequest::Test { event: TestEvent::Raid { viewers: 50 }, card: None };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "test", "event": { "event": "raid", "viewers": 50 } }));
        let event = PanelEvent::Status { status: Status::default() };
        let back: PanelEvent = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(back, event);
        let waiting =
            LoginStatus::Waiting { url: "u".into(), code: "C".into(), expires_at_ms: 1, reason: None };
        assert_eq!(serde_json::to_value(&waiting).unwrap()["state"], "waiting");
        let skip = serde_json::to_value(PanelRequest::SkipItem { id: 7 }).unwrap();
        assert_eq!(skip, serde_json::json!({ "type": "skip_item", "id": 7 }));
        let next = serde_json::to_value(PanelRequest::PlayNext { id: 7 }).unwrap();
        assert_eq!(next, serde_json::json!({ "type": "play_next", "id": 7 }));
        let open = serde_json::to_value(PanelRequest::OpenViewer { id: None, name: "Mossy".into() }).unwrap();
        assert_eq!(open, serde_json::json!({ "type": "open_viewer", "name": "Mossy" }));
    }
}
