//! The control API (see `protocol::control` and `docs/control-api.md`):
//! cards and effects on / off, the queue, and Custom events, for a Stream
//! Deck, a hotkey tool or a script. Web requests and the WebSocket both go
//! through [`run`]. On the WebSocket, a program can also be a plugin (the
//! external connection API): it registers its node types, fires its event
//! nodes, is sent its effect nodes' runs, and makes messages in its voices
//! when asked. A program can also say which it is (`hello`): Status shows
//! the Stream Deck plugin's version while it's connected.
//!
//! Every request needs the key: browsers let any web page send requests to
//! localhost (they just can't read the answers), and WebSockets aren't
//! limited at all. The key is made on first start and kept in
//! [`KEYS_FILE`] next to the config (secret, gitignored).

use std::collections::HashMap;
use std::path::{Path as FilePath, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use protocol::activity::{QueueAction, QueueGroup};
use protocol::cards::Category;
use protocol::control::{
    CardState, ControlMessage, ControlRequest, ControlState, EffectId, HistorySummary, KEY_HEADER, KEY_PARAM,
    QueueSummary, SoundState, Switch,
};
use protocol::plugin::{Manifest, PluginMessage};
use protocol::{Command, Envelope};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::model::{ChannelRef, Input, InputKind, Origin, PlatformEvent};
use crate::panel::PanelState;
use crate::pipeline::now_ms;
use crate::services::overlay_hub::OverlayHub;
use crate::services::speech::{self, MAX_AUDIO, SpeakRequest, Speaker};
use crate::services::status::StreamDeckGuard;
use crate::web::AppState;

pub const KEYS_FILE: &str = "lsk_control.json";
/// The provider Custom events from the control API come from.
pub const CONTROL_PROVIDER: &str = "control";
const KEY_LENGTH: usize = 32;

/// The keys that may use the control API. One for now; the file holds a
/// list so other programs (and plugins) can get keys of their own later.
pub struct ControlKeys {
    path: PathBuf,
    keys: Mutex<Vec<SavedKey>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeysFile {
    keys: Vec<SavedKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedKey {
    name: String,
    key: String,
}

impl ControlKeys {
    /// Reads the keys in `dir`, or makes one.
    pub fn open(dir: &FilePath) -> anyhow::Result<Arc<ControlKeys>> {
        let path = dir.join(KEYS_FILE);
        let saved = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<KeysFile>(&text) {
                Ok(file) => file.keys.into_iter().filter(|saved| saved.key.len() >= 16).collect(),
                Err(error) => {
                    tracing::warn!(
                        "{} can't be read ({error}); making a new control API key",
                        path.display()
                    );
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        let keys = ControlKeys { path, keys: Mutex::new(saved) };
        if keys.keys.lock().unwrap().is_empty() {
            keys.renew()?;
        }
        Ok(Arc::new(keys))
    }

    /// The key the panel shows.
    pub fn key(&self) -> String {
        self.keys.lock().unwrap().first().map(|saved| saved.key.clone()).unwrap_or_default()
    }

    fn accepts(&self, given: &str) -> bool {
        // The same time for every wrong key, however much of it matches.
        let given = given.as_bytes();
        self.keys.lock().unwrap().iter().any(|saved| {
            let key = saved.key.as_bytes();
            key.len() == given.len() && key.iter().zip(given).fold(0, |diff, (a, b)| diff | (a ^ b)) == 0
        })
    }

    /// Replaces the keys with a new one: whatever used the old one stops
    /// working.
    pub fn renew(&self) -> anyhow::Result<String> {
        const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let key: String =
            (0..KEY_LENGTH).map(|_| LETTERS[rand::random_range(0..LETTERS.len())] as char).collect();
        let keys = vec![SavedKey { name: "default".into(), key: key.clone() }];
        let json = serde_json::to_string_pretty(&KeysFile { keys: keys.clone() })?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, json).with_context(|| format!("couldn't write {}", temp.display()))?;
        std::fs::rename(&temp, &self.path)
            .with_context(|| format!("couldn't write {}", self.path.display()))?;
        *self.keys.lock().unwrap() = keys;
        Ok(key)
    }
}

/// An answer that went wrong: `{"error": …}` with an HTTP status.
#[derive(Debug, PartialEq)]
pub struct ControlError {
    status: StatusCode,
    message: String,
}

impl ControlError {
    fn new(status: StatusCode, message: impl Into<String>) -> ControlError {
        ControlError { status, message: message.into() }
    }

    fn message(self) -> ControlMessage {
        ControlMessage::Error { status: self.status.as_u16(), error: self.message }
    }
}

impl IntoResponse for ControlError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, [(header::CACHE_CONTROL, "no-store")], axum::Json(body)).into_response()
    }
}

type Params = Vec<(String, String)>;

/// The key from `?key=`, `X-Control-Key` or `Authorization: Bearer`.
fn check_key(keys: &ControlKeys, headers: &HeaderMap, params: &Params) -> Result<(), ControlError> {
    let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
    let given = params
        .iter()
        .find(|(name, _)| name == KEY_PARAM)
        .map(|(_, value)| value.as_str())
        .or_else(|| header(KEY_HEADER))
        .or_else(|| header(header::AUTHORIZATION.as_str()).and_then(|value| value.strip_prefix("Bearer ")));
    match given {
        Some(given) if keys.accepts(given.trim()) => Ok(()),
        Some(_) => Err(ControlError::new(
            StatusCode::UNAUTHORIZED,
            "wrong key; the panel's Control API page shows it",
        )),
        None => Err(ControlError::new(
            StatusCode::UNAUTHORIZED,
            "a key is needed (?key=…); the panel's Control API page shows it",
        )),
    }
}

/// Does what a request asks.
pub async fn run(request: ControlRequest, panel: &PanelState) -> Result<ControlMessage, ControlError> {
    match request {
        ControlRequest::State => Ok(ControlMessage::State { state: state(panel) }),
        ControlRequest::Card { id, set } => {
            let card = update(panel, |config| {
                let index = config
                    .cards
                    .iter()
                    .position(|card| card.id == id)
                    .or_else(|| {
                        let mut named = config
                            .cards
                            .iter()
                            .enumerate()
                            .filter(|(_, card)| card.name.trim().eq_ignore_ascii_case(id.trim()));
                        // Only a name that's unique.
                        match (named.next(), named.next()) {
                            (Some((index, _)), None) => Some(index),
                            _ => None,
                        }
                    })
                    .ok_or_else(|| ControlError::new(StatusCode::NOT_FOUND, format!("no card {id:?}")))?;
                let card = &mut config.cards[index];
                card.enabled = set.apply(card.enabled);
                Ok(CardState::of(card))
            })
            .await?;
            tracing::info!(card = card.name, on = card.on, "control API");
            Ok(ControlMessage::Card { card })
        }
        ControlRequest::Effect { id, set } => {
            let effect = EffectId::parse(&id).ok_or_else(|| {
                let known: Vec<&str> = EffectId::ALL.iter().map(|effect| effect.id()).collect();
                ControlError::new(
                    StatusCode::NOT_FOUND,
                    format!("no effect {id:?}; there are {}", known.join(", ")),
                )
            })?;
            let effect = update(panel, |config| {
                let enabled = effect.enabled_mut(config);
                *enabled = set.apply(*enabled);
                Ok(effect.state(config))
            })
            .await?;
            tracing::info!(effect = effect.id, on = effect.on, "control API");
            Ok(ControlMessage::Effect { effect })
        }
        ControlRequest::Queue { action, kind } => {
            match (action, kind) {
                (QueueAction::Clear, only) => panel.activity.clear(only),
                (action, None) => panel.activity.control(action),
                (_, Some(_)) => {
                    return Err(ControlError::new(StatusCode::BAD_REQUEST, "only a clear takes a kind"));
                }
            }
            Ok(ControlMessage::Queue { queue: QueueSummary::of(&panel.activity.queue()) })
        }
        ControlRequest::SkipItem { id } => {
            let was = panel.activity.skip(id);
            Ok(ControlMessage::SkippedItem { id, was })
        }
        ControlRequest::PlayNext { id } => {
            let was = panel.activity.play_next(id);
            Ok(ControlMessage::MovedItem { id, was })
        }
        ControlRequest::HistorySeen => {
            panel.activity.mark_seen(None);
            tracing::info!("control API: history seen");
            Ok(ControlMessage::History { history: history(panel) })
        }
        ControlRequest::Mute { set } => {
            let sound = update(panel, |config| {
                let settings = &mut config.effects.sound;
                settings.muted = set.apply(settings.muted);
                Ok(SoundState::of(config))
            })
            .await?;
            tracing::info!(muted = sound.muted, "control API");
            Ok(ControlMessage::Sound { sound })
        }
        ControlRequest::Event { name, data } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(ControlError::new(StatusCode::BAD_REQUEST, "an event needs a name"));
            }
            send_event(panel, PlatformEvent::Custom { name: name.clone(), data }).await?;
            tracing::info!(event = name, "control API");
            Ok(ControlMessage::Fired { name })
        }
        ControlRequest::Hello { .. } => Err(ControlError::new(
            StatusCode::BAD_REQUEST,
            "a program says hello on the WebSocket (/control/ws)",
        )),
        ControlRequest::Register { .. } | ControlRequest::Emit { .. } | ControlRequest::Spoken { .. } => {
            Err(ControlError::new(
                StatusCode::BAD_REQUEST,
                "a program connects as a plugin on the WebSocket (/control/ws)",
            ))
        }
    }
}

/// An event into the pipeline, as if from the chat's channel (for its
/// emotes).
async fn send_event(panel: &PanelState, event: PlatformEvent) -> Result<(), ControlError> {
    let (platform, id, channel) =
        panel.status.room().unwrap_or_else(|| ("twitch".into(), String::new(), String::new()));
    let input = Input {
        at_ms: now_ms(),
        origin: Origin {
            provider: CONTROL_PROVIDER.into(),
            platform,
            channel: ChannelRef { id, name: channel },
        },
        kind: InputKind::Event(event),
    };
    panel
        .inputs
        .send(input)
        .await
        .map_err(|_| ControlError::new(StatusCode::SERVICE_UNAVAILABLE, "the pipeline has stopped"))
}

/// The program says which it is. A Stream Deck plugin counts as connected,
/// with its version, until the connection closes (or another hello).
fn hello(
    client: &str,
    version: &str,
    stream_deck: &mut Option<StreamDeckGuard>,
    panel: &PanelState,
) -> Result<ControlMessage, ControlError> {
    let (client, version) = (client.trim(), version.trim());
    if client.is_empty() || client.len() > 64 || version.len() > 32 {
        let message = "a hello needs the program's name (up to 64 characters) and its version (up to 32)";
        return Err(ControlError::new(StatusCode::BAD_REQUEST, message));
    }
    tracing::info!(client, version, "control API client said which it is");
    *stream_deck = None;
    if client == protocol::streamdeck::CLIENT {
        *stream_deck = Some(panel.status.stream_deck_connected(version.to_string()));
    }
    Ok(ControlMessage::Hello { version: crate::update::RUNNING.to_string() })
}

/// A connection that registered as a plugin: its id, and the overlay
/// commands, to pass its effect nodes' runs on; with voices, the messages
/// it's asked to read, and the ones it hasn't answered.
struct Registered {
    id: String,
    commands: broadcast::Receiver<Arc<Envelope>>,
    speaking: Option<(Speaker, mpsc::UnboundedReceiver<SpeakRequest>)>,
    waiting: HashMap<u64, oneshot::Sender<Result<speech::Audio, String>>>,
    next_request: u64,
}

/// The program registers as a plugin, once per connection.
async fn register(
    manifest: Manifest,
    plugin: &mut Option<Registered>,
    panel: &PanelState,
    hub: &OverlayHub,
) -> Result<ControlMessage, ControlError> {
    if let Some(plugin) = plugin {
        let message = format!("this connection is already the {} plugin", plugin.id);
        return Err(ControlError::new(StatusCode::CONFLICT, message));
    }
    let id = manifest.id.clone();
    let voices = !manifest.voices.is_empty();
    panel
        .plugins
        .connect(manifest)
        .map_err(|problems| ControlError::new(StatusCode::BAD_REQUEST, problems.join("; ")))?;
    let speaking = voices.then(|| {
        let (speaker, asked) = mpsc::unbounded_channel();
        panel.speech.add_speaker(&id, speaker.clone());
        (speaker, asked)
    });
    *plugin = Some(Registered {
        id: id.clone(),
        commands: hub.subscribe(),
        speaking,
        waiting: HashMap::new(),
        next_request: 1,
    });
    panel.store.set_plugins(panel.plugins.catalog()).await;
    Ok(ControlMessage::Registered { plugin: id })
}

/// Fires one of the registered plugin's event nodes (its `output`, or
/// `event`).
async fn emit(
    node: String,
    output: Option<String>,
    values: Map<String, Value>,
    plugin: Option<&Registered>,
    panel: &PanelState,
) -> Result<ControlMessage, ControlError> {
    let Some(plugin) = plugin else {
        return Err(ControlError::new(StatusCode::CONFLICT, "register as a plugin first"));
    };
    let catalog = panel.plugins.catalog();
    let ty = catalog.node_type(&plugin.id, &node).filter(|ty| ty.category == Category::Events);
    let Some(ty) = ty else {
        let message = format!("the {} plugin has no event node {node:?}", plugin.id);
        return Err(ControlError::new(StatusCode::NOT_FOUND, message));
    };
    if let Some(output) = &output
        && !ty.fires(output)
    {
        let message = format!("its event node {node:?} has no event output {output:?}");
        return Err(ControlError::new(StatusCode::NOT_FOUND, message));
    }
    let output = output.filter(|output| output != "event");
    let event =
        PlatformEvent::Plugin { plugin: plugin.id.clone(), node: node.clone(), values, output, to: None };
    send_event(panel, event).await?;
    tracing::debug!(plugin = plugin.id, node, "plugin event");
    Ok(ControlMessage::Emitted { node })
}

/// The registered plugin's answer to a [`ControlMessage::Speak`].
fn spoken(
    plugin: Option<&mut Registered>,
    request: u64,
    audio: Option<String>,
    format: Option<String>,
    error: Option<String>,
) -> Result<(), ControlError> {
    let Some(plugin) = plugin else {
        return Err(ControlError::new(StatusCode::CONFLICT, "register as a plugin first"));
    };
    let Some(answer) = plugin.waiting.remove(&request) else {
        let message = format!("no message {request} is waiting to be read (too late, or answered already)");
        return Err(ControlError::new(StatusCode::NOT_FOUND, message));
    };
    use base64::Engine as _;
    let decoded = audio.map(|audio| base64::engine::general_purpose::STANDARD.decode(audio.trim()));
    let (result, problem) = match (decoded, error) {
        (Some(Ok(data)), _) if data.len() > MAX_AUDIO => {
            let message = format!("the audio is bigger than {} MB", MAX_AUDIO / 1024 / 1024);
            (Err(message.clone()), Some(message))
        }
        (Some(Ok(data)), _) => {
            (Ok(speech::Audio { data, format: format.unwrap_or_else(|| "wav".into()) }), None)
        }
        (Some(Err(error)), _) => {
            let message = format!("the audio isn't base64: {error}");
            (Err(message.clone()), Some(message))
        }
        (None, Some(error)) => (Err(error), None),
        (None, None) => {
            let message = "an answer needs audio or an error".to_string();
            (Err(message.clone()), Some(message))
        }
    };
    let _ = answer.send(result);
    match problem {
        Some(message) => Err(ControlError::new(StatusCode::BAD_REQUEST, message)),
        None => Ok(()),
    }
}

/// What comes for a registered plugin's program.
enum ForPlugin {
    /// An overlay command (one of its effect nodes' runs, maybe).
    Command(Result<Arc<Envelope>, RecvError>),
    /// A message to read in one of its voices.
    Speak(Box<ControlMessage>),
}

/// The next thing for the registered plugin's program; never, if the
/// connection isn't a plugin.
async fn for_plugin(plugin: &mut Option<Registered>) -> ForPlugin {
    let Some(Registered { commands, speaking, waiting, next_request, .. }) = plugin else {
        return std::future::pending().await;
    };
    let asked = async {
        match speaking {
            Some((_, asked)) => asked.recv().await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        command = commands.recv() => ForPlugin::Command(command),
        Some(SpeakRequest { voice, text, speed, answer }) = asked => {
            let request = *next_request;
            *next_request += 1;
            waiting.insert(request, answer);
            ForPlugin::Speak(Box::new(ControlMessage::Speak { request, voice, text, speed }))
        }
    }
}

/// An overlay command for one of `plugin`'s effect nodes, as a message to
/// its program.
fn plugin_run(envelope: &Envelope, plugin: &str) -> Option<ControlMessage> {
    let Command::Plugin(command) = &envelope.command else { return None };
    if command.plugin != plugin {
        return None;
    }
    match command.message.clone() {
        PluginMessage::Run { node, input, values, changes, settings, .. } => {
            Some(ControlMessage::Run { node, input, values, changes, settings })
        }
        // Hearing the chat is for pages (a program has none).
        PluginMessage::Message { .. }
        | PluginMessage::Settings { .. }
        | PluginMessage::Skip { .. }
        | PluginMessage::Chat { .. }
        | PluginMessage::ChatRemoved { .. }
        | PluginMessage::ChatUserRemoved { .. }
        | PluginMessage::ChatCleared { .. }
        | PluginMessage::Kept { .. }
        | PluginMessage::Samples => None,
    }
}

fn state(panel: &PanelState) -> ControlState {
    ControlState::new(&panel.store.current(), &panel.activity.queue(), history(panel))
}

fn history(panel: &PanelState) -> HistorySummary {
    HistorySummary { new: panel.activity.new_events(&panel.store.current().history) }
}

/// Changes the config, or answers why it can't.
async fn update<T>(
    panel: &PanelState,
    change: impl FnOnce(&mut protocol::config::AppConfig) -> Result<T, ControlError>,
) -> Result<T, ControlError> {
    match panel.store.update(change).await {
        Ok(result) => result,
        Err(problems) => Err(ControlError::new(StatusCode::CONFLICT, problems.join("; "))),
    }
}

/// A web request's answer: what's inside the message.
fn answer(message: ControlMessage) -> Response {
    let body = match message {
        ControlMessage::State { state } => serde_json::to_value(state),
        ControlMessage::Card { card } => serde_json::to_value(card),
        ControlMessage::Effect { effect } => serde_json::to_value(effect),
        ControlMessage::Queue { queue } => serde_json::to_value(queue),
        ControlMessage::SkippedItem { id, was } => Ok(serde_json::json!({ "id": id, "was": was })),
        ControlMessage::MovedItem { id, was } => Ok(serde_json::json!({ "id": id, "was": was })),
        ControlMessage::Sound { sound } => serde_json::to_value(sound),
        ControlMessage::History { history } => serde_json::to_value(history),
        ControlMessage::Fired { name } => Ok(serde_json::json!({ "fired": name })),
        // Only on the WebSocket.
        message @ (ControlMessage::Hello { .. }
        | ControlMessage::Registered { .. }
        | ControlMessage::Emitted { .. }
        | ControlMessage::Run { .. }
        | ControlMessage::Speak { .. }) => serde_json::to_value(message),
        ControlMessage::Error { status, error } => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return ControlError::new(status, error).into_response();
        }
    }
    .expect("control answers serialise");
    ([(header::CACHE_CONTROL, "no-store")], axum::Json(body)).into_response()
}

async fn respond(app: &AppState, headers: &HeaderMap, params: &Params, request: ControlRequest) -> Response {
    if let Err(error) = check_key(&app.panel.control, headers, params) {
        return error.into_response();
    }
    match run(request, &app.panel).await {
        Ok(message) => answer(message),
        Err(error) => error.into_response(),
    }
}

/// `/control/`, `/control/state`.
pub async fn get_state(
    State(app): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    respond(&app, &headers, &params, ControlRequest::State).await
}

/// `/control/cards/{id}/{on|off|toggle}`.
pub async fn card(
    State(app): State<AppState>,
    Path((id, set)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    match switch(&set) {
        Ok(set) => respond(&app, &headers, &params, ControlRequest::Card { id, set }).await,
        Err(error) => error.into_response(),
    }
}

/// `/control/effects/{id}/{on|off|toggle}`.
pub async fn effect(
    State(app): State<AppState>,
    Path((id, set)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    match switch(&set) {
        Ok(set) => respond(&app, &headers, &params, ControlRequest::Effect { id, set }).await,
        Err(error) => error.into_response(),
    }
}

/// `/control/mute/{on|off|toggle}`.
pub async fn mute(
    State(app): State<AppState>,
    Path(set): Path<String>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    match switch(&set) {
        Ok(set) => respond(&app, &headers, &params, ControlRequest::Mute { set }).await,
        Err(error) => error.into_response(),
    }
}

fn switch(name: &str) -> Result<Switch, ControlError> {
    Switch::parse(name)
        .ok_or_else(|| ControlError::new(StatusCode::NOT_FOUND, format!("{name:?} isn't on, off or toggle")))
}

/// `/control/history/seen`.
pub async fn history_seen(
    State(app): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    respond(&app, &headers, &params, ControlRequest::HistorySeen).await
}

/// `/control/queue/{pause|resume|toggle|skip|clear|mute|unmute|toggle_mute}`;
/// a clear can take `?kind=follow`.
pub async fn queue(
    State(app): State<AppState>,
    Path(action): Path<String>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    let Some(action) = QueueAction::parse(&action) else {
        let known: Vec<&str> = QueueAction::ALL.iter().map(|action| action.name()).collect();
        let message = format!("{action:?} isn't one of {}", known.join(", "));
        return ControlError::new(StatusCode::NOT_FOUND, message).into_response();
    };
    let kind = match params.iter().find(|(name, _)| name == "kind") {
        None => None,
        Some((_, kind)) => match QueueGroup::parse(kind) {
            Some(group) => Some(group),
            None => {
                let known: Vec<&str> = QueueGroup::ALL.iter().map(|group| group.name()).collect();
                let message = format!("{kind:?} isn't a kind; there are {}", known.join(", "));
                return ControlError::new(StatusCode::NOT_FOUND, message).into_response();
            }
        },
    };
    respond(&app, &headers, &params, ControlRequest::Queue { action, kind }).await
}

/// `/control/items/{id}/skip`: one item, by the id the state lists it
/// with.
pub async fn skip_item(
    State(app): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    match item_id(&id) {
        Ok(id) => respond(&app, &headers, &params, ControlRequest::SkipItem { id }).await,
        Err(error) => error.into_response(),
    }
}

/// `/control/items/{id}/next`: one waiting item to the front, by the id
/// the state lists it with.
pub async fn play_next(
    State(app): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Response {
    match item_id(&id) {
        Ok(id) => respond(&app, &headers, &params, ControlRequest::PlayNext { id }).await,
        Err(error) => error.into_response(),
    }
}

fn item_id(id: &str) -> Result<u64, ControlError> {
    id.parse().map_err(|_| {
        let message = format!("{id:?} isn't an item id; the state's queue lists them");
        ControlError::new(StatusCode::NOT_FOUND, message)
    })
}

/// `/control/events/{name}`: the query's other parameters and a JSON
/// object body (POST) become the event's data.
pub async fn event(
    State(app): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(params): Query<Params>,
    body: Bytes,
) -> Response {
    match event_data(&params, &body) {
        Ok(data) => respond(&app, &headers, &params, ControlRequest::Event { name, data }).await,
        Err(error) => error.into_response(),
    }
}

fn event_data(params: &Params, body: &[u8]) -> Result<Map<String, Value>, ControlError> {
    let mut data: Map<String, Value> = params
        .iter()
        .filter(|(name, _)| name != KEY_PARAM)
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    if !body.iter().all(u8::is_ascii_whitespace) {
        let Ok(Value::Object(fields)) = serde_json::from_slice(body) else {
            return Err(ControlError::new(StatusCode::BAD_REQUEST, "the body has to be a JSON object"));
        };
        data.extend(fields);
    }
    Ok(data)
}

/// Anything else under `/control/`.
pub async fn unknown() -> Response {
    ControlError::new(StatusCode::NOT_FOUND, "no such control; see docs/control-api.md").into_response()
}

/// `/control/ws`: the state now and on every change, and requests as
/// messages ([`ControlRequest`]), each answered ([`ControlMessage`]); and a
/// program's plugin.
pub async fn websocket(
    State(app): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Err(error) = check_key(&app.panel.control, &headers, &params) {
        return error.into_response();
    }
    upgrade.on_upgrade(move |socket| connection(socket, app.panel, app.hub))
}

async fn connection(socket: WebSocket, panel: PanelState, hub: OverlayHub) {
    let (mut sender, mut receiver) = socket.split();
    let mut config_changes = panel.store.subscribe();
    let mut activity_changes = panel.activity.subscribe();
    tracing::info!("control API client connected");
    let mut last = state(&panel);
    if send(&mut sender, &ControlMessage::State { state: last.clone() }).await.is_err() {
        return;
    }
    let mut plugin: Option<Registered> = None;
    let mut stream_deck: Option<StreamDeckGuard> = None;
    loop {
        tokio::select! {
            message = receiver.next() => match message {
                Some(Ok(WsMessage::Text(text))) => {
                    let answer = match serde_json::from_str::<ControlRequest>(&text) {
                        Ok(ControlRequest::Hello { client, version }) => {
                            hello(&client, &version, &mut stream_deck, &panel)
                        }
                        Ok(ControlRequest::Register { plugin: manifest }) => {
                            register(*manifest, &mut plugin, &panel, &hub).await
                        }
                        Ok(ControlRequest::Emit { node, output, values }) => {
                            emit(node, output, values, plugin.as_ref(), &panel).await
                        }
                        Ok(ControlRequest::Spoken { request, audio, format, error }) => {
                            match spoken(plugin.as_mut(), request, audio, format, error) {
                                Ok(()) => continue,
                                Err(error) => Err(error),
                            }
                        }
                        Ok(request) => run(request, &panel).await,
                        Err(error) => Err(ControlError::new(StatusCode::BAD_REQUEST, format!("not a request: {error}"))),
                    };
                    if send(&mut sender, &answer.unwrap_or_else(ControlError::message)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => continue,
            },
            next = for_plugin(&mut plugin) => match next {
                ForPlugin::Speak(speak) => {
                    if send(&mut sender, &speak).await.is_err() {
                        break;
                    }
                    continue;
                }
                ForPlugin::Command(command) => match command {
                Ok(envelope) => {
                    let run = plugin.as_ref().and_then(|plugin| plugin_run(&envelope, &plugin.id));
                    if let Some(run) = run && send(&mut sender, &run).await.is_err() {
                        break;
                    }
                    continue;
                }
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "a plugin's program fell behind; some of its effects were skipped");
                    continue;
                }
                Err(RecvError::Closed) => break,
                },
            },
            changed = config_changes.changed() => if changed.is_err() { break },
            // The queue, and the history's new events.
            change = activity_changes.recv() => if let Err(RecvError::Closed) = change { break },
        }
        let now = state(&panel);
        if now != last {
            if send(&mut sender, &ControlMessage::State { state: now.clone() }).await.is_err() {
                break;
            }
            last = now;
        }
    }
    if let Some(plugin) = plugin {
        if let Some((speaker, _)) = &plugin.speaking {
            panel.speech.remove_speaker(&plugin.id, speaker);
        }
        panel.plugins.disconnect(&plugin.id);
        panel.store.set_plugins(panel.plugins.catalog()).await;
    }
    tracing::info!("control API client disconnected");
}

async fn send(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    message: &ControlMessage,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(message).expect("control messages serialise");
    sender.send(WsMessage::Text(json.into())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (PathBuf, Arc<ControlKeys>) {
        let dir = std::env::temp_dir().join(format!("lsk_control_{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let keys = ControlKeys::open(&dir).unwrap();
        (dir, keys)
    }

    #[test]
    fn a_key_is_made_once_and_kept() {
        let (dir, keys) = keys();
        let key = keys.key();
        assert_eq!(key.len(), KEY_LENGTH);
        assert_eq!(ControlKeys::open(&dir).unwrap().key(), key);
        let renewed = keys.renew().unwrap();
        assert_ne!(renewed, key);
        assert_eq!(ControlKeys::open(&dir).unwrap().key(), renewed);
        std::fs::write(dir.join(KEYS_FILE), "{ not json").unwrap();
        assert_eq!(ControlKeys::open(&dir).unwrap().key().len(), KEY_LENGTH);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_key_comes_from_the_query_or_a_header() {
        let (dir, keys) = keys();
        let key = keys.key();
        let none = HeaderMap::new();
        assert_eq!(check_key(&keys, &none, &vec![(KEY_PARAM.into(), key.clone())]), Ok(()));
        assert!(check_key(&keys, &none, &vec![(KEY_PARAM.into(), "wrong".into())]).is_err());
        assert!(check_key(&keys, &none, &Vec::new()).is_err());
        let mut headers = HeaderMap::new();
        headers.insert(KEY_HEADER, key.parse().unwrap());
        assert_eq!(check_key(&keys, &headers, &Vec::new()), Ok(()));
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, format!("Bearer {key}").parse().unwrap());
        assert_eq!(check_key(&keys, &headers, &Vec::new()), Ok(()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn item_ids_are_numbers() {
        assert_eq!(item_id("42"), Ok(42));
        for wrong in ["", "next", "-1", "4.5"] {
            assert_eq!(item_id(wrong).map_err(|error| error.status), Err(StatusCode::NOT_FOUND), "{wrong:?}");
        }
    }

    #[test]
    fn event_data_from_the_query_and_body() {
        let params = vec![(KEY_PARAM.into(), "k".into()), ("colour".into(), "red".into())];
        let data = event_data(&params, br#"{ "count": 3 }"#).unwrap();
        assert_eq!(Value::Object(data), serde_json::json!({ "colour": "red", "count": 3 }));
        assert!(event_data(&Vec::new(), b"  ").unwrap().is_empty());
        assert!(event_data(&Vec::new(), b"[1]").is_err());
    }
}
