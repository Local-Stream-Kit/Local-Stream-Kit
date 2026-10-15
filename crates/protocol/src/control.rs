//! The control API: what a Stream Deck, a hotkey tool or a script can do
//! with the app, under `/control/` on the app's port, with the app's key.
//! One-off web requests (GET or POST) and a WebSocket that stays connected
//! take the same requests ([`ControlRequest`]). See `docs/control-api.md`.
//!
//! Made to grow with plugins: effects are named by string ids that clients
//! read from [`ControlState`] instead of knowing them, so a plugin's effect
//! shows up beside the built-ins; clients ignore fields they don't know.
//!
//! The WebSocket is also the external connection API: a program registers
//! as a plugin ([`ControlRequest::Register`]), fires its event nodes
//! ([`ControlRequest::Emit`]) and is told when its effect nodes fire
//! ([`ControlMessage::Run`]). See `docs/writing-a-plugin.md` → Programs.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use std::collections::BTreeMap;

use crate::activity::{QueueAction, QueueGroup, QueueItem, QueueState, Skipped};
use crate::cards::{Card, NodeKind};
use crate::config::AppConfig;
use crate::encode_segment;
use crate::plugin::Manifest;
use crate::plugin::{ALERT_PLUGIN, RAIN_PLUGIN, SPEAK_PLUGIN};
use crate::sound::SOUND_PLUGIN;

/// Bumped when a change would break existing clients.
pub const CONTROL_VERSION: u32 = 1;
/// `GET` answers the state; the Stream Deck plugin is given this with the
/// key (see [`connection_url`]).
pub const CONTROL_PATH: &str = "/control/";
pub const CONTROL_WS_PATH: &str = "/control/ws";
/// The key as a query parameter (`?key=…`), or in this header, or as
/// `Authorization: Bearer …`.
pub const KEY_PARAM: &str = "key";
pub const KEY_HEADER: &str = "x-control-key";

/// What the Stream Deck plugin is pasted: the app's address with the key.
pub fn connection_url(origin: &str, key: &str) -> String {
    format!("{origin}{CONTROL_PATH}?{KEY_PARAM}={}", encode_segment(key))
}

pub fn card_path(id: &str, set: Switch) -> String {
    format!("/control/cards/{}/{}", encode_segment(id), set.name())
}

pub fn effect_path(effect: EffectId, set: Switch) -> String {
    format!("/control/effects/{}/{}", effect.id(), set.name())
}

/// With `only`, a clear takes one group (`?kind=follow`).
pub fn queue_path(action: QueueAction, only: Option<QueueGroup>) -> String {
    match only {
        Some(group) => format!("/control/queue/{}?kind={}", action.name(), group.name()),
        None => format!("/control/queue/{}", action.name()),
    }
}

/// Skips one queue item by its id (in the state's `queue.on_stage` and
/// `queue.up_next`).
pub fn skip_item_path(id: u64) -> String {
    format!("/control/items/{id}/skip")
}

/// `path` with the key added, ready to copy.
pub fn with_key(path: &str, key: &str) -> String {
    let joint = if path.contains('?') { '&' } else { '?' };
    format!("{path}{joint}{KEY_PARAM}={}", encode_segment(key))
}

/// Marks every event in the history seen: the seen marker goes above them
/// all (2026-10-14).
pub const HISTORY_SEEN_PATH: &str = "/control/history/seen";

/// `on` mutes, `off` unmutes.
pub fn mute_path(set: Switch) -> String {
    format!("/control/mute/{}", set.name())
}

pub fn event_path(name: &str) -> String {
    format!("/control/events/{}", encode_segment(name))
}

/// Turns something on, off, or the other way round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Switch {
    On,
    Off,
    Toggle,
}

impl Switch {
    pub const ALL: [Switch; 3] = [Switch::On, Switch::Off, Switch::Toggle];

    pub fn name(self) -> &'static str {
        match self {
            Switch::On => "on",
            Switch::Off => "off",
            Switch::Toggle => "toggle",
        }
    }

    pub fn parse(name: &str) -> Option<Switch> {
        Switch::ALL.into_iter().find(|set| set.name() == name)
    }

    /// The new state of something that is `on` now.
    pub fn apply(self, on: bool) -> bool {
        match self {
            Switch::On => true,
            Switch::Off => false,
            Switch::Toggle => !on,
        }
    }
}

/// The built-in effects, by their ids on the wire. Plugin effects will
/// have ids of their own, so clients take the list from the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectId {
    Rain,
    Alert,
    Sound,
    Speech,
}

impl EffectId {
    pub const ALL: [EffectId; 4] = [EffectId::Rain, EffectId::Alert, EffectId::Sound, EffectId::Speech];

    pub fn id(self) -> &'static str {
        match self {
            EffectId::Rain => "rain",
            EffectId::Alert => "alert",
            EffectId::Sound => "sound",
            EffectId::Speech => "speech",
        }
    }

    pub fn parse(id: &str) -> Option<EffectId> {
        EffectId::ALL.into_iter().find(|effect| effect.id() == id)
    }

    /// As the panel called it on its Effects page (until 2026-10-12).
    pub fn label(self) -> &'static str {
        match self {
            EffectId::Rain => "Emote rain",
            EffectId::Alert => "Alerts",
            EffectId::Sound => "Sounds",
            EffectId::Speech => "Text to speech",
        }
    }

    /// The built-in `rain`, `alert`, `sound` and `speak` plugins' switches.
    /// Text to speech (the Speak plugin's) is every voice read aloud, alerts'
    /// too (2026-10-14; it was a switch of the app's own until then).
    pub fn enabled(self, config: &AppConfig) -> bool {
        match self {
            EffectId::Rain => config.plugin(RAIN_PLUGIN).enabled,
            EffectId::Alert => config.plugin(ALERT_PLUGIN).enabled,
            EffectId::Sound => config.plugin(SOUND_PLUGIN).enabled,
            EffectId::Speech => config.plugin(SPEAK_PLUGIN).enabled,
        }
    }

    pub fn enabled_mut(self, config: &mut AppConfig) -> &mut bool {
        match self {
            EffectId::Rain => config.plugin_enabled_mut(RAIN_PLUGIN),
            EffectId::Alert => config.plugin_enabled_mut(ALERT_PLUGIN),
            EffectId::Sound => config.plugin_enabled_mut(SOUND_PLUGIN),
            EffectId::Speech => config.plugin_enabled_mut(SPEAK_PLUGIN),
        }
    }

    pub fn state(self, config: &AppConfig) -> EffectState {
        EffectState { id: self.id().into(), name: self.label().into(), on: self.enabled(config) }
    }
}

/// Everything a client shows: answered by `GET /control/state`, and sent
/// on the WebSocket when it connects and whenever it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlState {
    /// [`CONTROL_VERSION`].
    pub version: u32,
    /// In the Cards tab's order.
    pub cards: Vec<CardState>,
    pub effects: Vec<EffectState>,
    /// The names Custom event nodes listen for, sorted.
    pub events: Vec<String>,
    pub queue: QueueSummary,
    /// What a queue's clear can be limited to (`?kind=`), in order.
    pub kinds: Vec<KindInfo>,
    pub sound: SoundState,
    /// The history's new events (2026-10-14).
    #[serde(default)]
    pub history: HistorySummary,
}

/// The history, as far as the seen marker goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySummary {
    /// The events above the seen marker that the History's filter shows,
    /// replays left out.
    pub new: usize,
}

/// The master volume and mute (Plugins → Sounds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundState {
    /// 0–1.
    pub volume: f32,
    pub muted: bool,
}

impl SoundState {
    pub fn of(config: &AppConfig) -> SoundState {
        let settings = &config.effects.sound;
        SoundState { volume: settings.volume, muted: settings.muted }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindInfo {
    /// `follow`
    pub id: String,
    /// `Follows`
    pub name: String,
}

impl ControlState {
    pub fn new(config: &AppConfig, queue: &QueueState, history: HistorySummary) -> ControlState {
        ControlState {
            version: CONTROL_VERSION,
            cards: config.cards.iter().map(CardState::of).collect(),
            effects: EffectId::ALL.into_iter().map(|effect| effect.state(config)).collect(),
            events: custom_events(&config.cards),
            queue: QueueSummary::of(queue),
            kinds: QueueGroup::ALL
                .into_iter()
                .map(|group| KindInfo { id: group.name().into(), name: group.plural().into() })
                .collect(),
            sound: SoundState::of(config),
            history,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardState {
    /// Stays the same when the card is renamed.
    pub id: String,
    pub name: String,
    pub on: bool,
}

impl CardState {
    pub fn of(card: &Card) -> CardState {
        CardState { id: card.id.clone(), name: card.name.clone(), on: card.enabled }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectState {
    pub id: String,
    pub name: String,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueSummary {
    pub paused: bool,
    /// Something is playing on the overlay.
    pub playing: bool,
    pub waiting: usize,
    /// What's waiting, by kind (`{"follow": 37}`); kinds with none left out.
    pub by_kind: BTreeMap<QueueGroup, usize>,
    /// Overlays showing plugin pages. With none, the queue waits.
    pub players: usize,
    /// What's playing (2026-10-13).
    #[serde(default)]
    pub on_stage: Option<ItemSummary>,
    /// What's waiting, in order: the first [`QUEUE_LIST_LIMIT`], as the
    /// panel lists them (2026-10-13).
    ///
    /// [`QUEUE_LIST_LIMIT`]: crate::activity::QUEUE_LIST_LIMIT
    #[serde(default)]
    pub up_next: Vec<ItemSummary>,
}

impl QueueSummary {
    pub fn of(queue: &QueueState) -> QueueSummary {
        QueueSummary {
            paused: queue.paused,
            playing: queue.on_stage.is_some(),
            waiting: queue.waiting_count,
            by_kind: queue.by_group.clone(),
            players: queue.players,
            on_stage: queue.on_stage.as_ref().map(ItemSummary::of),
            up_next: queue.waiting.iter().map(ItemSummary::of).collect(),
        }
    }
}

/// A queue item, enough to show it and skip it by its id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemSummary {
    /// What [`ControlRequest::SkipItem`] takes; unique, kept across
    /// restarts.
    pub id: u64,
    /// Its node's title: `Alert`, `Speak`.
    pub label: String,
    /// What it shows or says.
    pub text: String,
    /// Its kind, as `by_kind` counts it (`follow`).
    pub kind: QueueGroup,
}

impl ItemSummary {
    pub fn of(item: &QueueItem) -> ItemSummary {
        ItemSummary { id: item.id, label: item.label.clone(), text: item.text.clone(), kind: item.group }
    }
}

/// The names Custom event nodes listen for (on cards that are off too),
/// sorted, each once. A node without a name hears every Custom event.
pub fn custom_events(cards: &[Card]) -> Vec<String> {
    let mut names: Vec<String> = cards
        .iter()
        .flat_map(|card| &card.nodes)
        .filter_map(|node| match &node.kind {
            NodeKind::Custom { name } if !name.trim().is_empty() => Some(name.trim().to_string()),
            _ => None,
        })
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names
}

/// A request, as a WebSocket message. The web requests are the same ones:
/// `/control/state`, `/control/cards/{id}/{on|off|toggle}`,
/// `/control/effects/{id}/{on|off|toggle}`,
/// `/control/queue/{pause|resume|toggle|skip|clear}` (a clear with
/// `?kind=follow` takes only follows), `/control/items/{id}/skip`,
/// `/control/mute/{on|off|toggle}`, `/control/history/seen` and
/// `/control/events/{name}` (its data from the query and a JSON body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    State,
    /// A card by id (or, failing that, by name, ignoring case).
    Card {
        id: String,
        set: Switch,
    },
    Effect {
        id: String,
        set: Switch,
    },
    /// `kind` limits a clear to one kind (see [`QueueGroup`]).
    Queue {
        action: QueueAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<QueueGroup>,
    },
    /// Skips one queue item by its id (2026-10-13): ends it if it's on
    /// stage, drops it if it's waiting. One that's gone already (played,
    /// skipped, cleared, its time ran out) is left alone, and the item
    /// now in its place plays on: answered [`Skipped::Gone`], not an
    /// error.
    SkipItem {
        id: u64,
    },
    /// Mutes every sound (on), or unmutes (off), live.
    Mute {
        set: Switch,
    },
    /// Marks every event in the history seen (2026-10-14): the seen
    /// marker goes above them all, as the panel's All seen.
    HistorySeen,
    /// Fires a Custom event, as if a platform sent it: cards starting with
    /// a Custom event node of that name run. `data`'s fields fill their
    /// texts' placeholders: `{"colour": "red"}` is `{colour}`.
    Event {
        name: String,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        data: Map<String, Value>,
    },
    /// Makes this connection a plugin: its node types join the cards'
    /// while it's connected, and are remembered after it goes. WebSocket
    /// only; once per connection.
    Register {
        plugin: Box<Manifest>,
    },
    /// Fires one of this connection's plugin's event nodes. `values` by
    /// output name: a number, a text (emote codes in it become emotes), or
    /// emote codes as a list. WebSocket only. `output` (2026-10-13) fires
    /// another of the node's event outputs than `event` ("ended", say).
    Emit {
        node: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        values: Map<String, Value>,
    },
    /// The answer to a [`ControlMessage::Speak`]: the message's audio
    /// (base64; `format` `wav`, `mp3` or `ogg`), or why there's none.
    /// WebSocket only, from a plugin with voices.
    Spoken {
        request: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// The answer to a request, as a WebSocket message (a web request gets
/// what's inside, or `{"error": …}` with an HTTP status), and the state
/// whenever it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    State {
        state: ControlState,
    },
    Card {
        card: CardState,
    },
    Effect {
        effect: EffectState,
    },
    Queue {
        queue: QueueSummary,
    },
    /// The answer to a [`ControlRequest::SkipItem`]: what became of it.
    SkippedItem {
        id: u64,
        was: Skipped,
    },
    Sound {
        sound: SoundState,
    },
    /// The answer to a [`ControlRequest::HistorySeen`].
    History {
        history: HistorySummary,
    },
    Fired {
        name: String,
    },
    /// This connection is now the plugin `plugin`.
    Registered {
        plugin: String,
    },
    Emitted {
        node: String,
    },
    /// Not an answer: one of this connection's plugin's effect nodes fired.
    /// The same as an overlay page's `run`: `input` is the event input the
    /// event came into, `values` every other input and every setting.
    Run {
        node: String,
        input: String,
        values: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        changes: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        settings: Map<String, Value>,
    },
    /// Not an answer: the app wants a message read aloud in one of this
    /// connection's plugin's voices (its id, without the plugin's).
    /// Answer with [`ControlRequest::Spoken`] and the same `request`
    /// within 30 seconds. `speed`: 1 is normal, 0.5 to 2.
    Speak {
        request: u64,
        voice: String,
        text: String,
        speed: f32,
    },
    /// `status` is what a web request would have answered (401, 404…).
    Error {
        status: u16,
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(card_path("hydrate", Switch::Toggle), "/control/cards/hydrate/toggle");
        assert_eq!(card_path("a b", Switch::On), "/control/cards/a%20b/on");
        assert_eq!(effect_path(EffectId::Speech, Switch::Off), "/control/effects/speech/off");
        assert_eq!(queue_path(QueueAction::Toggle, None), "/control/queue/toggle");
        assert_eq!(
            queue_path(QueueAction::Clear, Some(QueueGroup::Follow)),
            "/control/queue/clear?kind=follow"
        );
        assert_eq!(
            with_key("/control/queue/clear?kind=follow", "k"),
            "/control/queue/clear?kind=follow&key=k"
        );
        assert_eq!(with_key("/control/queue/skip", "k"), "/control/queue/skip?key=k");
        assert_eq!(skip_item_path(42), "/control/items/42/skip");
        assert_eq!(event_path("party mode"), "/control/events/party%20mode");
        assert_eq!(connection_url("http://localhost:3000", "abc"), "http://localhost:3000/control/?key=abc");
    }

    #[test]
    fn names_match_serde() {
        for set in Switch::ALL {
            assert_eq!(serde_json::to_value(set).unwrap(), set.name());
            assert_eq!(Switch::parse(set.name()), Some(set));
        }
        for action in QueueAction::ALL {
            assert_eq!(serde_json::to_value(action).unwrap(), action.name());
            assert_eq!(QueueAction::parse(action.name()), Some(action));
        }
        for effect in EffectId::ALL {
            assert_eq!(EffectId::parse(effect.id()), Some(effect));
        }
    }

    #[test]
    fn switches() {
        assert!(Switch::On.apply(false) && Switch::On.apply(true));
        assert!(!Switch::Off.apply(true));
        assert!(Switch::Toggle.apply(false) && !Switch::Toggle.apply(true));
    }

    #[test]
    fn state_lists_cards_in_order_and_custom_events_once() {
        let mut config = AppConfig { cards: crate::cards::premade(), ..AppConfig::default() };
        let mut extra = config.cards[0].clone();
        extra.id = "extra".into();
        for node in &mut extra.nodes {
            node.kind = NodeKind::Custom { name: " party ".into() };
        }
        config.cards.push(extra);
        *config.plugin_enabled_mut(SPEAK_PLUGIN) = false;
        let state = ControlState::new(&config, &QueueState::default(), HistorySummary { new: 2 });
        assert_eq!(state.version, CONTROL_VERSION);
        let ids: Vec<&str> = state.cards.iter().map(|card| card.id.as_str()).collect();
        let expected: Vec<&str> = config.cards.iter().map(|card| card.id.as_str()).collect();
        assert_eq!(ids, expected);
        assert!(state.events.contains(&"party".to_string()));
        assert_eq!(state.events.iter().filter(|name| *name == "party").count(), 1);
        let speech = state.effects.iter().find(|effect| effect.id == "speech").unwrap();
        assert!(!speech.on);
        assert_eq!(state.effects.len(), 4);
        assert_eq!(serde_json::to_value(&state).unwrap()["history"], serde_json::json!({ "new": 2 }));
    }

    #[test]
    fn requests_as_messages() {
        let request: ControlRequest =
            serde_json::from_str(r#"{"type":"card","id":"hydrate","set":"toggle"}"#).unwrap();
        assert_eq!(request, ControlRequest::Card { id: "hydrate".into(), set: Switch::Toggle });
        let request: ControlRequest = serde_json::from_str(r#"{"type":"queue","action":"skip"}"#).unwrap();
        assert_eq!(request, ControlRequest::Queue { action: QueueAction::Skip, kind: None });
        let request: ControlRequest =
            serde_json::from_str(r#"{"type":"queue","action":"clear","kind":"gift_bomb"}"#).unwrap();
        assert_eq!(
            request,
            ControlRequest::Queue { action: QueueAction::Clear, kind: Some(QueueGroup::GiftBomb) }
        );
        for group in QueueGroup::ALL {
            assert_eq!(serde_json::to_value(group).unwrap(), group.name());
            assert_eq!(QueueGroup::parse(group.name()), Some(group));
        }
        let mut queue = QueueState::default();
        queue.by_group.insert(QueueGroup::Follow, 3);
        let summary = serde_json::to_value(QueueSummary::of(&queue)).unwrap();
        assert_eq!(summary["by_kind"], serde_json::json!({ "follow": 3 }));
        let request: ControlRequest = serde_json::from_str(r#"{"type":"event","name":"party"}"#).unwrap();
        assert_eq!(request, ControlRequest::Event { name: "party".into(), data: Map::new() });
        let request: ControlRequest = serde_json::from_str(r#"{"type":"skip_item","id":7}"#).unwrap();
        assert_eq!(request, ControlRequest::SkipItem { id: 7 });
        let request: ControlRequest = serde_json::from_str(r#"{"type":"history_seen"}"#).unwrap();
        assert_eq!(request, ControlRequest::HistorySeen);
        let skipped = ControlMessage::SkippedItem { id: 7, was: Skipped::OnStage };
        assert_eq!(
            serde_json::to_value(&skipped).unwrap(),
            serde_json::json!({ "type": "skipped_item", "id": 7, "was": "on_stage" })
        );
        assert_eq!(serde_json::to_value(Skipped::Gone).unwrap(), "gone");
        let error = ControlMessage::Error { status: 404, error: "no card".into() };
        assert_eq!(serde_json::to_value(&error).unwrap()["type"], "error");
    }

    #[test]
    fn the_queue_lists_its_items_by_id() {
        let item = |id, text: &str, group| QueueItem {
            id,
            label: "Alert".into(),
            text: text.into(),
            parts: Vec::new(),
            card: "card".into(),
            event: None,
            origin: crate::activity::ItemOrigin::Live,
            group,
            names: Vec::new(),
            queued_at_ms: 0,
        };
        let queue = QueueState {
            on_stage: Some(item(3, "Friend is raiding", QueueGroup::Raid)),
            waiting: vec![
                item(4, "Bot1 followed", QueueGroup::Follow),
                item(5, "Bot2 followed", QueueGroup::Follow),
            ],
            waiting_count: 2,
            ..QueueState::default()
        };
        let summary = serde_json::to_value(QueueSummary::of(&queue)).unwrap();
        assert_eq!(
            summary["on_stage"],
            serde_json::json!({ "id": 3, "label": "Alert", "text": "Friend is raiding", "kind": "raid" })
        );
        let ids: Vec<u64> = QueueSummary::of(&queue).up_next.iter().map(|item| item.id).collect();
        assert_eq!(ids, [4, 5]);
        let idle = serde_json::to_value(QueueSummary::of(&QueueState::default())).unwrap();
        assert_eq!((&idle["on_stage"], &idle["up_next"]), (&Value::Null, &serde_json::json!([])));
        // From before the items were listed.
        let old: QueueSummary = serde_json::from_value(serde_json::json!({
            "paused": false, "playing": false, "waiting": 0, "by_kind": {}, "players": 1
        }))
        .unwrap();
        assert!(old.on_stage.is_none() && old.up_next.is_empty());
    }

    #[test]
    fn plugin_messages() {
        let request: ControlRequest = serde_json::from_value(serde_json::json!({
            "type": "register",
            "plugin": { "id": "billybilly", "name": "BillyBilly", "version": "1.0.0", "api": 1, "nodes": [] }
        }))
        .unwrap();
        assert!(matches!(request, ControlRequest::Register { plugin } if plugin.id == "billybilly"));
        let request: ControlRequest =
            serde_json::from_str(r#"{"type":"emit","node":"membership","values":{"months":3}}"#).unwrap();
        let ControlRequest::Emit { node, values, output } = request else { panic!() };
        assert_eq!((node.as_str(), &values["months"]), ("membership", &serde_json::json!(3)));
        assert_eq!(output, None);
        let request: ControlRequest =
            serde_json::from_str(r#"{"type":"emit","node":"played","output":"ended"}"#).unwrap();
        assert!(matches!(request, ControlRequest::Emit { output: Some(output), .. } if output == "ended"));
        let run = ControlMessage::Run {
            node: "burst".into(),
            input: "trigger".into(),
            values: Map::new(),
            changes: Map::new(),
            settings: Map::new(),
        };
        assert_eq!(
            serde_json::to_value(&run).unwrap(),
            serde_json::json!({ "type": "run", "node": "burst", "input": "trigger", "values": {} })
        );
    }
}
