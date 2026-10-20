//! The control panel's side of the server: its files (`/panel/`) and its
//! WebSocket (`/panel/ws`), which carries the config, live status, and the
//! panel's requests (save, log in / out, test events).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use protocol::activity::{ItemOrigin, QueueAction};
use protocol::panel::{
    KnownEmote, LinkFrom, LoginStatus, PANEL_PATH, PanelEvent, PanelRequest, Status, TestEvent, TestHypeTrain,
};
use protocol::plugin::{SPEAK_NODE, SPEAK_PLUGIN};
use protocol::update::AppLink;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use crate::control::ControlKeys;
use crate::media;
use crate::model::{
    Amount, ChannelRef, EmoteSpan, HypeTrain, HypeTrainChange, Input, InputKind, MembershipKind, Message,
    MessageMarks, Origin, PlatformEvent, RewardRef, Roles, User,
};
use crate::pipeline::{CardTest, TEST_PROVIDER, Tested, now_ms};
use crate::plugins::Plugins;
use crate::providers::twitch_eventsub::LoginCommand;
use crate::services::activity::{Activity, Cause, Change, Play};
use crate::services::config_store::{ConfigStore, SaveError};
use crate::services::emotes::EmoteRegistry;
use crate::services::overlay_hub::OverlayHub;
use crate::services::speech::Speech;
use crate::services::status::StatusHub;
use crate::web::AppState;

/// How often emote counts and the like are re-checked for an open panel.
const REFRESH: Duration = Duration::from_secs(2);
/// The time between the steps of a whole test hype train.
const TEST_TRAIN_STEP: Duration = Duration::from_secs(2);

/// The built panel (`trunk build` output in `crates/panel`). Debug builds
/// read it from disk on each request; release builds embed it.
#[derive(rust_embed::Embed)]
#[folder = "../panel/dist/"]
#[allow_missing = true]
struct PanelAssets;

#[derive(Clone)]
pub struct PanelState {
    pub store: Arc<ConfigStore>,
    pub status: StatusHub,
    pub emotes: Arc<EmoteRegistry>,
    /// Test events go in here, like real ones.
    pub inputs: mpsc::Sender<Input>,
    /// Test events for one card, which runs even if it's off.
    pub tests: mpsc::Sender<CardTest>,
    /// `None` when channel points are off.
    pub login: Option<mpsc::Sender<LoginCommand>>,
    /// History entries to replay through the cards.
    pub replays: mpsc::Sender<u64>,
    /// The queue and the history; previews wait in the queue too.
    pub activity: Activity,
    /// The pictures and sounds folders (see `media.rs`).
    pub media: crate::media::MediaFolders,
    pub speech: Speech,
    /// The control API's keys.
    pub control: Arc<ControlKeys>,
    /// The installed plugins (their overlay files are served too), and
    /// programs' plugins.
    pub plugins: Arc<Plugins>,
    /// What plugins' pages keep with the app.
    pub memory: crate::plugins::memory::PageMemory,
    /// Plugins' file settings (a stylesheet).
    pub files: crate::plugins::files::PluginFiles,
    /// Plugins' secret settings (panels only learn which are saved).
    pub secrets: crate::plugins::secrets::PluginSecrets,
    /// The lists of choices plugins' server code filled in.
    pub options: crate::plugins::options::PluginOptions,
    /// What plugins' tabs send their server code, and back.
    pub tabs: crate::plugins::tabs::PluginTabs,
    /// For previews that don't wait in the queue.
    pub hub: OverlayHub,
    pub log: crate::logging::Logging,
    /// The Speak box's Try readings.
    pub speech_previews: SpeechPreviews,
    /// The update check and download.
    pub updates: crate::update::Updater,
    /// Opens links, folders and files on this PC (or logs them, in tests).
    pub desktop: crate::desktop::Desktop,
    /// The cache folder, where Install writes the Stream Deck plugin.
    pub cache: std::path::PathBuf,
    /// Every chat message, for the viewers' cards.
    pub chat_log: crate::services::chat_log::ChatLog,
    /// Moderating viewers with the Twitch login; `None` with the Twitch CLI
    /// or a recording.
    pub moderation: Option<crate::providers::twitch_eventsub::TwitchModeration>,
}

/// After a Try reading's length, before the panels' spinner stops: about
/// when the page has played it.
const TRY_SLACK: Duration = Duration::from_millis(500);

/// The Speak box's Try readings (Say it, a voice picked): read straight
/// away, not in the queue, each taking the place of the one before. The
/// panels show a spinner while one is being made or read.
#[derive(Clone)]
pub struct SpeechPreviews {
    /// The reading being made or read, by a number of its own.
    current: Arc<tokio::sync::watch::Sender<Option<u64>>>,
    next: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for SpeechPreviews {
    fn default() -> Self {
        SpeechPreviews { current: Arc::new(tokio::sync::watch::channel(None).0), next: Arc::default() }
    }
}

impl SpeechPreviews {
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<u64>> {
        self.current.subscribe()
    }

    /// A new reading, the current one from now on.
    fn start(&self) -> u64 {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.current.send_replace(Some(id));
        id
    }

    fn is_current(&self, id: u64) -> bool {
        *self.current.borrow() == Some(id)
    }

    /// Reading `id` is over (read, or it couldn't be made), unless another
    /// has taken its place.
    fn over(&self, id: u64) {
        self.current.send_if_modified(|current| {
            let over = *current == Some(id);
            if over {
                *current = None;
            }
            over
        });
    }
}

pub async fn redirect() -> Redirect {
    Redirect::permanent(PANEL_PATH)
}

pub async fn index() -> Response {
    file("index.html")
}

pub async fn asset(Path(path): Path<String>) -> Response {
    file(&path)
}

/// Identifies the panel build being served: index.html names the hashed
/// `.js` / `.wasm` files, so it changes whenever they do. `None` if the
/// panel hasn't been built. (Debug builds read it from disk, so it changes
/// when `trunk` rebuilds the panel; release builds embed it.)
fn panel_build() -> Option<String> {
    let file = PanelAssets::get("index.html")?;
    Some(file.metadata.sha256_hash()[..8].iter().map(|byte| format!("{byte:02x}")).collect())
}

fn file(path: &str) -> Response {
    let path = if path.is_empty() { "index.html" } else { path };
    match PanelAssets::get(path) {
        Some(file) => {
            // index.html names the hashed files, so only it can't be cached.
            let cache = if path == "index.html" { "no-cache" } else { "public, max-age=31536000, immutable" };
            (
                [(header::CONTENT_TYPE, file.metadata.mimetype().to_string()), (header::CACHE_CONTROL, cache.to_string())],
                file.data.into_owned(),
            )
                .into_response()
        }
        None if path == "index.html" => (
            StatusCode::SERVICE_UNAVAILABLE,
            Html("<p>The control panel hasn't been built yet: run <code>trunk build</code> in <code>crates/panel</code>.</p>"),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !local_origin(&headers) {
        let origin = headers.get(header::ORIGIN).and_then(|origin| origin.to_str().ok()).unwrap_or("?");
        tracing::warn!(origin, "refused a control panel connection from a page on another site");
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade.on_upgrade(move |socket| connection(socket, state.panel))
}

/// Whether a request comes from a page on this PC (the panel in a browser
/// tab or an OBS dock), or from something that isn't a web page at all
/// (no `Origin`). Browsers send `Origin` with every WebSocket, and don't
/// stop a page on another site from connecting to localhost, so without
/// this any site could change the config.
pub(crate) fn local_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else { return true };
    let Some(rest) = origin
        .to_str()
        .ok()
        .and_then(|origin| origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")))
    else {
        return false;
    };
    let host = match rest.strip_prefix('[') {
        Some(ipv6) => ipv6.split(']').next().unwrap_or_default(),
        None => rest.split([':', '/']).next().unwrap_or_default(),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// The plugins, and the plugin folders that didn't load.
fn plugins_event(panel: &PanelState) -> PanelEvent {
    PanelEvent::Plugins {
        plugins: panel.plugins.catalog().plugins().to_vec(),
        failures: panel.plugins.failures().to_vec(),
    }
}

/// One open panel.
async fn connection(socket: WebSocket, panel: PanelState) {
    let (mut sender, mut receiver) = socket.split();
    let mut config_changes = panel.store.subscribe();
    let mut status_changes = panel.status.subscribe();
    let mut activity_changes = panel.activity.subscribe();
    let mut plugin_changes = panel.plugins.subscribe();
    let mut plugin_files = panel.files.subscribe();
    plugin_files.mark_changed();
    let mut plugin_secrets = panel.secrets.subscribe();
    plugin_secrets.mark_changed();
    let mut plugin_options = panel.options.subscribe();
    plugin_options.mark_changed();
    let mut speech_previews = panel.speech_previews.subscribe();
    speech_previews.mark_changed();
    let mut chat_changes = panel.chat_log.subscribe();
    let mut tab_messages = panel.tabs.panel_listens();
    // Answers about viewers, which wait on the disk or on Twitch; and the
    // card this panel has open.
    let (replies, mut answers) = mpsc::channel::<PanelEvent>(32);
    let mut open_viewer: Option<String> = None;
    let mut refresh = tokio::time::interval(REFRESH);
    tracing::info!("control panel connected");

    let mut last_build = panel_build();
    if let Some(id) = &last_build
        && send(&mut sender, &PanelEvent::Build { id: id.clone() }).await.is_err()
    {
        return;
    }
    let (config, revision) = panel.store.current_revision();
    let config = PanelEvent::Config { config, revision };
    if send(&mut sender, &plugins_event(&panel)).await.is_err()
        || send(&mut sender, &config).await.is_err()
        || send_activity(&mut sender, &panel.activity).await.is_err()
    {
        return;
    }
    let mut last_status: Option<Status> = None;
    loop {
        let build = panel_build();
        if build != last_build {
            if let Some(id) = &build {
                tracing::info!("control panel rebuilt; open panels reload");
                if send(&mut sender, &PanelEvent::Build { id: id.clone() }).await.is_err() {
                    break;
                }
            }
            last_build = build;
        }
        let status = panel.status.snapshot(&panel.emotes, panel.store.restart_needed());
        if last_status.as_ref() != Some(&status) {
            if send(&mut sender, &PanelEvent::Status { status: status.clone() }).await.is_err() {
                break;
            }
            last_status = Some(status);
        }
        tokio::select! {
            incoming = receiver.next() => {
                let text = match incoming {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => {
                        tracing::debug!(%error, "panel connection error");
                        break;
                    }
                };
                match serde_json::from_str::<PanelRequest>(&text) {
                    Ok(request) => {
                        let Some(request) = crate::viewers::handle(request, &panel, &mut open_viewer, &replies)
                        else {
                            continue;
                        };
                        if let Some(reply) = handle(request, &panel).await
                            && send(&mut sender, &reply).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => tracing::warn!(%error, "unexpected message from the control panel"),
                }
            }
            changed = config_changes.changed() => {
                if changed.is_err() {
                    break;
                }
                // The watch only says it changed: the store has the config
                // with its revision.
                config_changes.borrow_and_update();
                let (config, revision) = panel.store.current_revision();
                if send(&mut sender, &PanelEvent::Config { config, revision }).await.is_err() {
                    break;
                }
            }
            changed = plugin_changes.changed() => {
                if changed.is_err() {
                    break;
                }
                plugin_changes.borrow_and_update();
                if send(&mut sender, &plugins_event(&panel)).await.is_err() {
                    break;
                }
                // (A plugin installed over a built-in one doesn't have its
                // secrets.)
                panel.secrets.refresh();
            }
            changed = plugin_files.changed() => {
                if changed.is_err() {
                    break;
                }
                let files = plugin_files.borrow_and_update().clone();
                if send(&mut sender, &PanelEvent::PluginFiles { files }).await.is_err() {
                    break;
                }
            }
            changed = plugin_secrets.changed() => {
                if changed.is_err() {
                    break;
                }
                plugin_secrets.borrow_and_update();
                let saved = panel.secrets.saved();
                if send(&mut sender, &PanelEvent::PluginSecrets { saved }).await.is_err() {
                    break;
                }
            }
            changed = plugin_options.changed() => {
                if changed.is_err() {
                    break;
                }
                let options = plugin_options.borrow_and_update().clone();
                if send(&mut sender, &PanelEvent::PluginOptions { options }).await.is_err() {
                    break;
                }
            }
            changed = speech_previews.changed() => {
                if changed.is_err() {
                    break;
                }
                let reading = speech_previews.borrow_and_update().is_some();
                if send(&mut sender, &PanelEvent::SpeechPreview { reading }).await.is_err() {
                    break;
                }
            }
            changed = status_changes.changed() => {
                if changed.is_err() {
                    break;
                }
                status_changes.borrow_and_update();
            }
            change = activity_changes.recv() => {
                let sent = match change {
                    Ok(Change::Queue(queue)) => send(&mut sender, &PanelEvent::Queue { queue }).await,
                    Ok(Change::Entry(entry)) => send(&mut sender, &PanelEvent::HistoryEntry { entry }).await,
                    Ok(Change::History(entries)) => send(&mut sender, &PanelEvent::History { entries }).await,
                    Ok(Change::Seen(up_to)) => send(&mut sender, &PanelEvent::HistorySeen { up_to }).await,
                    Ok(Change::Prepared(prepared)) => send(&mut sender, &PanelEvent::QueuePrepared { prepared }).await,
                    // Fell behind: everything afresh.
                    Err(RecvError::Lagged(_)) => send_activity(&mut sender, &panel.activity).await,
                    Err(RecvError::Closed) => break,
                };
                if sent.is_err() {
                    break;
                }
            }
            Some(answer) = answers.recv() => {
                crate::viewers::opened(&answer, &mut open_viewer);
                if send(&mut sender, &answer).await.is_err() {
                    break;
                }
            }
            change = chat_changes.recv() => {
                let event = match change {
                    Ok(change) => crate::viewers::change(change, open_viewer.as_deref()),
                    // Lines missed: the card shows them when opened again.
                    Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => break,
                };
                if let Some(event) = event
                    && send(&mut sender, &event).await.is_err()
                {
                    break;
                }
            }
            message = tab_messages.recv() => {
                let event = match message {
                    Ok(message) => PanelEvent::TabMessage { plugin: message.plugin.clone(), data: message.data.clone() },
                    // Messages missed: the tab asks again when it's opened.
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                if send(&mut sender, &event).await.is_err() {
                    break;
                }
            }
            _ = refresh.tick() => {}
        }
    }
    tracing::info!("control panel disconnected");
}

/// Reads the plugins folder again; the cards get the new node types.
pub async fn reload_plugins(panel: &PanelState) {
    panel.plugins.reload();
    panel.store.set_plugins(panel.plugins.catalog()).await;
}

async fn handle(request: PanelRequest, panel: &PanelState) -> Option<PanelEvent> {
    match request {
        PanelRequest::SaveConfig { config, revision } => {
            Some(match panel.store.save(config, revision).await {
                Ok(revision) => PanelEvent::Saved { revision },
                Err(SaveError::Outdated) => PanelEvent::SaveOutdated,
                Err(SaveError::Problems(problems)) => PanelEvent::SaveFailed { problems },
            })
        }
        PanelRequest::ReloadPlugins => {
            reload_plugins(panel).await;
            None
        }
        PanelRequest::RemovePlugin { id } => match panel.plugins.remove(&id) {
            Ok(()) => {
                panel.store.set_plugins(panel.plugins.catalog()).await;
                None
            }
            Err(message) => Some(PanelEvent::PluginProblem { message }),
        },
        PanelRequest::ForgetProgram { id } => match panel.plugins.forget(&id) {
            Ok(()) => {
                panel.store.set_plugins(panel.plugins.catalog()).await;
                None
            }
            Err(message) => Some(PanelEvent::PluginProblem { message }),
        },
        PanelRequest::StartLogin => login(panel, LoginCommand::Start).await,
        PanelRequest::CancelLogin => login(panel, LoginCommand::Cancel).await,
        PanelRequest::OpenLogin => {
            // Only the link Twitch gave, never one from the panel.
            match panel.status.login() {
                LoginStatus::Waiting { url, .. } => {
                    if let Err(error) = panel.desktop.open(&url) {
                        tracing::warn!("couldn't open the browser ({error}); open {url} yourself");
                    }
                }
                _ => tracing::info!("no login is waiting; nothing to open"),
            }
            None
        }
        PanelRequest::Logout => login(panel, LoginCommand::Logout).await,
        PanelRequest::NewControlKey => {
            match panel.control.renew() {
                Ok(key) => {
                    tracing::info!("made a new control API key; the old one stops working");
                    panel.status.set_control_key(key);
                }
                Err(error) => tracing::error!("couldn't make a new control API key: {error:#}"),
            }
            None
        }
        // A card's timers tick: no event to send.
        PanelRequest::Test { event: TestEvent::Timer, card } => {
            let Some(card) = card else {
                tracing::info!("a timer test is for a card; nothing was tested");
                return None;
            };
            let test = Tested::Timers(test_origin(panel.status.room()));
            if panel.tests.send(CardTest { test, card }).await.is_err() {
                tracing::warn!("the pipeline has stopped; the test went nowhere");
            }
            None
        }
        PanelRequest::Test { event, card } => {
            // A whole hype train plays out over a few seconds, as a real one
            // would: a bar fills up, goes up a level, ends and leaves.
            let step = match &event {
                TestEvent::HypeTrain { change: TestHypeTrain::Whole, .. } => TEST_TRAIN_STEP,
                _ => Duration::ZERO,
            };
            let inputs = test_inputs(event, panel.status.room(), &panel.emotes);
            let (pipeline, tests) = (panel.inputs.clone(), panel.tests.clone());
            let send = async move {
                for (n, mut input) in inputs.into_iter().enumerate() {
                    if n > 0 && !step.is_zero() {
                        tokio::time::sleep(step).await;
                        input.at_ms = now_ms();
                    }
                    let sent = match &card {
                        Some(card) => tests
                            .send(CardTest { test: Tested::Input(input), card: card.clone() })
                            .await
                            .is_ok(),
                        None => pipeline.send(input).await.is_ok(),
                    };
                    if !sent {
                        tracing::warn!("the pipeline has stopped; the test event went nowhere");
                        break;
                    }
                }
            };
            if step.is_zero() {
                send.await;
            } else {
                tokio::spawn(send);
            }
            None
        }
        PanelRequest::SavePluginLook { plugin, key, name, text } => {
            match panel.files.save(&plugin, &key, &name, text) {
                Ok(()) => None,
                Err(error) => Some(PanelEvent::SaveFailed {
                    problems: vec![format!("couldn't save {plugin}'s look {name:?}: {error}")],
                }),
            }
        }
        PanelRequest::RenamePluginLook { plugin, key, name, to } => {
            match panel.files.rename(&plugin, &key, &name, &to) {
                Ok(()) => None,
                Err(error) => Some(PanelEvent::SaveFailed {
                    problems: vec![format!("couldn't rename {plugin}'s look {name:?}: {error}")],
                }),
            }
        }
        PanelRequest::SavePluginSecret { plugin, key, value } => {
            match panel.secrets.save(&plugin, &key, value) {
                Ok(()) => None,
                Err(error) => Some(PanelEvent::SaveFailed { problems: vec![error] }),
            }
        }
        PanelRequest::Pictures => {
            let (files, folders) = media::PICTURES.list_in(panel).await;
            Some(PanelEvent::Pictures { files, folders })
        }
        PanelRequest::Sounds => {
            let (files, folders) = media::SOUNDS.list_in(panel).await;
            Some(PanelEvent::Sounds { files, folders })
        }
        PanelRequest::DeleteSound { name } => match media::SOUNDS.delete(&panel.media, &name).await {
            Ok(()) => {
                let (files, folders) = media::SOUNDS.list_in(panel).await;
                Some(PanelEvent::Sounds { files, folders })
            }
            Err(problem) => Some(PanelEvent::SaveFailed { problems: vec![problem] }),
        },
        PanelRequest::InstallSpeech => {
            let speech = panel.speech.clone();
            tokio::spawn(async move { speech.install().await });
            None
        }
        PanelRequest::PreviewSpeech { text, settings } => {
            // Read by the Speak plugin's page straight away, not in the
            // queue (asked, 2026-10-14). Picking voice after voice: one
            // still being made when the next is asked for isn't sent, and
            // the page stops a Try it's still reading (`try`).
            let job = crate::effects::speech_job(&text, &settings);
            if !panel.speech.can_speak(&job.voice) {
                tracing::info!("text to speech isn't installed yet; nothing to preview");
            } else if !job.text.trim().is_empty() {
                let previews = panel.speech_previews.clone();
                let id = previews.start();
                let (hub, runtime, volume) =
                    (panel.hub.clone(), tokio::runtime::Handle::current(), job.volume);
                let on_made = {
                    let previews = previews.clone();
                    move |made: Option<crate::services::speech::Made>| {
                        let Some(made) = made.filter(|_| previews.is_current(id)) else {
                            previews.over(id);
                            return;
                        };
                        let length = Duration::from_millis(u64::from(made.duration_ms.unwrap_or(3000)));
                        let mut values = serde_json::Map::new();
                        values.insert("text".into(), protocol::plugin::plain_text_value(&text));
                        values.insert("try".into(), true.into());
                        crate::plugins::add_speech(&mut values, "text", made.clip(volume));
                        let message = protocol::plugin::PluginMessage::Run {
                            node: SPEAK_NODE.into(),
                            input: "trigger".into(),
                            values,
                            changes: Default::default(),
                            settings: Default::default(),
                            queue_id: None,
                            run_id: None,
                            preview: None,
                            origin: ItemOrigin::Test,
                        };
                        hub.send(protocol::Command::Plugin(protocol::PluginCommand {
                            plugin: SPEAK_PLUGIN.into(),
                            message,
                        }));
                        // The page doesn't say when it has read a run that
                        // isn't the queue's: over when it would have ended.
                        runtime.spawn(async move {
                            tokio::time::sleep(length + TRY_SLACK).await;
                            previews.over(id);
                        });
                    }
                };
                if !panel.speech.make(job, on_made) {
                    previews.over(id);
                }
            }
            None
        }
        PanelRequest::PreviewPlugin { plugin, node, .. } if node.is_empty() => {
            let message = protocol::plugin::PluginMessage::Samples;
            panel.hub.send(protocol::Command::Plugin(protocol::PluginCommand { plugin, message }));
            None
        }
        PanelRequest::PreviewPlugin { plugin, node, values, settings } => {
            let ty = panel.plugins.catalog().node_type(&plugin, &node)?;
            let input = ty
                .inputs
                .iter()
                .find(|input| input.ty == protocol::cards::SocketType::Event)
                .map_or_else(|| "trigger".to_string(), |input| input.name.clone());
            if ty.queued(&values) {
                let (title, text, parts) =
                    (ty.title.clone(), ty.description(&values), ty.description_parts(&values));
                let changes = serde_json::Map::new();
                let server = panel.plugins.has_server(&plugin);
                let play = Play {
                    plugin,
                    node,
                    title,
                    text,
                    parts,
                    input,
                    values,
                    changes,
                    settings,
                    speech: None,
                    then: None,
                    server,
                };
                panel.activity.push(play, &preview());
            } else {
                let message = protocol::plugin::PluginMessage::Run {
                    node,
                    input,
                    values,
                    changes: Default::default(),
                    settings,
                    queue_id: None,
                    run_id: None,
                    preview: None,
                    origin: ItemOrigin::Test,
                };
                panel.hub.send(protocol::Command::Plugin(protocol::PluginCommand { plugin, message }));
            }
            None
        }
        PanelRequest::Queue { action: QueueAction::Clear, only } => {
            panel.activity.clear(only);
            None
        }
        PanelRequest::Queue { action, .. } => {
            panel.activity.control(action);
            None
        }
        PanelRequest::SkipItem { id } => {
            panel.activity.skip(id);
            None
        }
        PanelRequest::PlayNext { id } => {
            panel.activity.play_next(id);
            None
        }
        PanelRequest::ClearHistory => {
            panel.activity.clear_history();
            None
        }
        PanelRequest::MarkSeen { up_to } => {
            panel.activity.mark_seen(up_to);
            None
        }

        PanelRequest::Replay { entry } => {
            if panel.replays.send(entry).await.is_err() {
                tracing::warn!("the pipeline has stopped; nothing was replayed");
            }
            None
        }
        PanelRequest::OpenLink { from, url } => {
            // Only a link a viewer wrote, never any the panel names: what
            // it opens could be a program.
            let found = match from {
                LinkFrom::Entry(id) => {
                    panel.activity.history_entry(id).is_some_and(|entry| entry.links().contains(&url))
                }
                LinkFrom::Item(id) => panel
                    .activity
                    .item_text(id)
                    .is_some_and(|text| protocol::links::find(&text).iter().any(|link| link.url == url)),
            };
            if !found {
                tracing::warn!(url, ?from, "that link isn't where the panel said; nothing was opened");
            } else if let Err(error) = panel.desktop.open(&url) {
                tracing::warn!("couldn't open the browser ({error}); open {url} yourself");
            }
            None
        }
        PanelRequest::DownloadUpdate => {
            let updates = panel.updates.clone();
            tokio::spawn(async move { updates.download().await });
            None
        }
        PanelRequest::OpenAppLink { link } => {
            // Only addresses the app builds itself, as for `OpenLink`.
            let url = match link {
                AppLink::WhatsNew => panel.updates.release_page(),
                AppLink::Licence => Some(protocol::update::licence_url()),
                AppLink::Support => Some(protocol::update::SUPPORT_URL.to_string()),
                AppLink::StreamDeckPlugin => Some(protocol::streamdeck::release_file_url(
                    &protocol::update::releases(),
                    crate::update::RUNNING,
                )),
            };
            match url {
                Some(url) => {
                    if let Err(error) = panel.desktop.open(&url) {
                        tracing::warn!("couldn't open the browser ({error}); open {url} yourself");
                    }
                }
                None => tracing::info!("no new version is out; no release page to open"),
            }
            None
        }
        PanelRequest::InstallStreamDeck => {
            let error = crate::streamdeck::install(&panel.cache, panel.desktop).err();
            if let Some(error) = &error {
                tracing::warn!("couldn't install the Stream Deck plugin: {error}");
            }
            panel.status.set_stream_deck_install_error(error);
            None
        }
        PanelRequest::OpenLogs => {
            let folder = panel.log.folder();
            if let Err(error) = std::fs::create_dir_all(folder).and_then(|()| panel.desktop.open(folder)) {
                tracing::warn!("couldn't open the logs folder ({error}); it's {}", folder.display());
            }
            None
        }
        PanelRequest::ExportCard { card } => {
            let name = format!("{}.json", crate::desktop::file_name(&card.name, "Card"));
            let file = protocol::cards::CardFile::new(card).to_json();
            match panel.desktop.download(&name, file.as_bytes()) {
                Ok(path) => {
                    tracing::info!(path = %path.display(), "exported a card");
                    None
                }
                Err(error) => {
                    tracing::warn!("couldn't export a card: {error}");
                    Some(PanelEvent::ExportFailed { message: error })
                }
            }
        }
        PanelRequest::ClearLogLines => {
            panel.log.clear_recent();
            None
        }
        PanelRequest::ResyncEmotes => {
            tracing::info!("refreshing the emotes");
            panel.emotes.resync();
            None
        }
        // The connection handles these (`crate::viewers`).
        PanelRequest::FindViewers { .. }
        | PanelRequest::OpenViewer { .. }
        | PanelRequest::OlderViewerLines { .. }
        | PanelRequest::CloseViewer
        | PanelRequest::Moderate { .. }
        | PanelRequest::OpenTwitchCard { .. } => None,
        PanelRequest::TabMessage { plugin, data } => {
            let size = data.to_string().len();
            if size > protocol::plugin::TAB_MESSAGE_LIMIT {
                tracing::warn!(plugin, size, "a tab's message was too big for its server code; dropped");
            } else {
                panel.tabs.to_code(&plugin, data);
            }
            None
        }
        PanelRequest::Emotes => {
            // Without a chat channel: the globals of Twitch, the only
            // platform so far.
            let (platform, channel_id) = panel
                .status
                .room()
                .map_or(("twitch".to_string(), String::new()), |(platform, id, _)| (platform, id));
            let emotes = panel
                .emotes
                .known(&platform, &channel_id)
                .into_iter()
                .map(|info| KnownEmote {
                    code: info.emote.code,
                    source: info.emote.source,
                    id: info.emote.id,
                })
                .collect();
            Some(PanelEvent::Emotes { emotes })
        }
    }
}

async fn login(panel: &PanelState, command: LoginCommand) -> Option<PanelEvent> {
    match &panel.login {
        Some(login) => {
            let _ = login.send(command).await;
        }
        None => tracing::info!("channel points are off; turn them on (and restart) to log in"),
    }
    None
}

/// A preview from the panel: it waits its turn like a test event.
fn preview() -> Cause {
    Cause { origin: ItemOrigin::Test, ..Cause::default() }
}

async fn send(sender: &mut SplitSink<WebSocket, WsMessage>, event: &PanelEvent) -> Result<(), axum::Error> {
    let json = serde_json::to_string(event).expect("panel events serialise");
    sender.send(WsMessage::Text(json.into())).await
}

/// The queue, the whole history and its seen marker.
async fn send_activity(
    sender: &mut SplitSink<WebSocket, WsMessage>,
    activity: &Activity,
) -> Result<(), axum::Error> {
    send(sender, &PanelEvent::Queue { queue: activity.queue() }).await?;
    send(sender, &PanelEvent::History { entries: activity.history() }).await?;
    send(sender, &PanelEvent::HistorySeen { up_to: activity.seen() }).await
}

/// Fake inputs, as if they came from the chat channel (so random emote rain
/// uses that channel's emotes): one, or a chat message from several viewers.
/// Their messages get the platform's own emotes marked, as real chat has
/// them (see [`platform_emotes`]).
fn test_inputs(
    event: TestEvent,
    room: Option<(String, String, String)>,
    emotes: &EmoteRegistry,
) -> Vec<Input> {
    let mut inputs = unmarked_test_inputs(event, room);
    for input in &mut inputs {
        let (platform, channel) = (&input.origin.platform, &input.origin.channel.id);
        let message = match &mut input.kind {
            InputKind::Message(message) => Some(message),
            InputKind::Event(
                PlatformEvent::Support { message, .. } | PlatformEvent::Membership { message, .. },
            ) => message.as_mut(),
            _ => None,
        };
        if let Some(message) = message {
            message.emotes = platform_emotes(&message.text, emotes, platform, channel);
        }
    }
    inputs
}

/// Where the platform's own emotes (Twitch's: Kappa, the channel's sub
/// emotes) are in `text`, as the platform marks them in real chat: the
/// ones the app knows (from the login, or seen in chat). 7TV, BTTV and FFZ
/// emotes are found later, as in real chat.
fn platform_emotes(text: &str, emotes: &EmoteRegistry, platform: &str, channel: &str) -> Vec<EmoteSpan> {
    let mut spans = Vec::new();
    let mut start = None;
    let chars: Vec<char> = text.chars().collect();
    for index in 0..=chars.len() {
        let space = chars.get(index).is_none_or(|c| c.is_whitespace());
        match (start, space) {
            (None, false) => start = Some(index),
            (Some(from), true) => {
                start = None;
                let code: String = chars[from..index].iter().collect();
                if emotes.lookup_code(platform, channel, &code).is_some() {
                    continue;
                }
                if let Some(info) = emotes.lookup_any_code(platform, channel, &code) {
                    spans.push(EmoteSpan { start: from, end: index, emote: info.emote });
                }
            }
            _ => {}
        }
    }
    spans
}

/// Where test events come from: the chat channel, when there is one.
fn test_origin(room: Option<(String, String, String)>) -> Origin {
    let (platform, id, name) = room.unwrap_or_else(|| ("twitch".into(), "0".into(), "test".into()));
    Origin { provider: TEST_PROVIDER.into(), platform, channel: ChannelRef { id, name } }
}

pub(crate) fn unmarked_test_inputs(event: TestEvent, room: Option<(String, String, String)>) -> Vec<Input> {
    let origin = test_origin(room);
    let viewer = User {
        id: "test-viewer".into(),
        name: "testviewer".into(),
        display_name: "TestViewer".into(),
        // A subscribed mod and VIP, so Viewer roles nodes let tests through
        // (unless they ask for the broadcaster only).
        roles: Roles { broadcaster: false, moderator: true, vip: true, member: true },
        ..Default::default()
    };
    let gifter = User {
        id: "test-gifter".into(),
        name: "testgifter".into(),
        display_name: "TestGifter".into(),
        roles: Roles::default(),
        ..Default::default()
    };
    let kind = match event {
        TestEvent::Chat { text, viewer: named, viewers, first } => {
            // Named on the Test page: that viewer (a mod and VIP, as
            // TestViewer), so filters by name can be tried.
            let named = named.trim();
            let first_viewer = if named.is_empty() || named == "TestViewer" {
                viewer.clone()
            } else {
                User {
                    id: format!("test-viewer:{}", named.to_lowercase()),
                    name: named.to_lowercase(),
                    display_name: named.to_string(),
                    ..viewer.clone()
                }
            };
            return (1..=viewers.clamp(1, 100))
                .map(|n| {
                    let user = match n {
                        1 => first_viewer.clone(),
                        n => User {
                            id: format!("test-viewer-{n}"),
                            name: format!("testviewer{n}"),
                            display_name: format!("TestViewer{n}"),
                            roles: Roles::default(),
                            ..Default::default()
                        },
                    };
                    let marks = MessageMarks { first, ..Default::default() };
                    let message =
                        Message { user, text: text.clone(), emotes: Vec::new(), marks, ..Default::default() };
                    Input { at_ms: now_ms(), origin: origin.clone(), kind: InputKind::Message(message) }
                })
                .collect();
        }
        TestEvent::Sub { months, message } => {
            let kind = if months <= 1 { MembershipKind::New } else { MembershipKind::Renewal { months } };
            let message = Some(message).filter(|text| !text.trim().is_empty()).map(|text| Message {
                user: viewer.clone(),
                text,
                ..Default::default()
            });
            InputKind::Event(PlatformEvent::Membership {
                user: viewer,
                kind,
                tier: Some("1000".into()),
                message,
            })
        }
        TestEvent::GiftedSub => InputKind::Event(PlatformEvent::Membership {
            user: viewer,
            kind: MembershipKind::Gifted { gifter: Some(gifter), batch_id: None },
            tier: Some("1000".into()),
            message: None,
        }),
        TestEvent::GiftBomb { count } => InputKind::Event(PlatformEvent::GiftBatch {
            gifter: Some(gifter),
            count,
            tier: Some("1000".into()),
            batch_id: format!("test-{}", now_ms()),
        }),
        TestEvent::Bits { amount, message } => {
            let message = Some(message).filter(|text| !text.trim().is_empty()).map(|text| Message {
                user: viewer.clone(),
                text,
                ..Default::default()
            });
            InputKind::Event(PlatformEvent::Support {
                user: viewer,
                amount: Amount::Units { value: amount, unit: "bits".into() },
                message,
            })
        }
        TestEvent::Raid { viewers } => InputKind::Event(PlatformEvent::Raid { from: viewer, viewers }),
        TestEvent::Reward { reward, text } => InputKind::Event(PlatformEvent::Redemption {
            user: viewer,
            reward: RewardRef { id: format!("test-{reward}"), title: Some(reward) },
            input: Some(text).filter(|text| !text.is_empty()),
        }),
        TestEvent::Custom { name } => {
            InputKind::Event(PlatformEvent::Custom { name, data: Default::default() })
        }
        TestEvent::Follow => InputKind::Event(PlatformEvent::Follow { user: viewer }),
        TestEvent::HypeTrain { change: TestHypeTrain::Whole, level } => {
            let level = level.max(1);
            return [
                (TestHypeTrain::Start, level),
                (TestHypeTrain::Progress, level),
                (TestHypeTrain::LevelUp, level + 1),
                (TestHypeTrain::End, level + 1),
            ]
            .into_iter()
            .map(|(change, level)| Input {
                at_ms: now_ms(),
                origin: origin.clone(),
                kind: test_hype_train(change, level),
            })
            .collect();
        }
        TestEvent::HypeTrain { change, level } => test_hype_train(change, level),
        TestEvent::Plugin { plugin, node, values } => {
            InputKind::Event(PlatformEvent::Plugin { plugin, node, values, output: None, to: None })
        }
        TestEvent::Timer => return Vec::new(),
    };

    vec![Input { at_ms: now_ms(), origin, kind }]
}

/// A made-up hype train at `level` (each level needs 500 points more than
/// the one before), as Twitch would send it. (A whole train is these
/// steps in turn, above.)
fn test_hype_train(change: TestHypeTrain, level: u32) -> InputKind {
    let level = level.max(1);
    let goal = 500 * u64::from(level);
    let before: u64 = (1..level).map(|each| 500 * u64::from(each)).sum();
    let (change, progress, seconds_left) = match change {
        TestHypeTrain::Start | TestHypeTrain::Whole => (HypeTrainChange::Started, goal / 4, 300),
        TestHypeTrain::Progress => (HypeTrainChange::Progressed { level_up: false }, goal * 3 / 5, 240),
        TestHypeTrain::LevelUp => (HypeTrainChange::Progressed { level_up: true }, goal / 10, 300),
        TestHypeTrain::End => (HypeTrainChange::Ended, 0, 0),
    };
    let (goal, total) = match change {
        HypeTrainChange::Ended => (0, before + goal / 2),
        _ => (goal, before + progress),
    };
    InputKind::Event(PlatformEvent::HypeTrain {
        change,
        train: HypeTrain {
            id: "test-train".into(),
            level,
            progress,
            goal,
            total,
            kind: "regular".into(),
            shared: false,
            seconds_left,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_test_hype_train_starts_goes_up_a_level_and_ends() {
        let inputs =
            unmarked_test_inputs(TestEvent::HypeTrain { change: TestHypeTrain::Whole, level: 1 }, None);
        let steps: Vec<(HypeTrainChange, u32, u64, u64, u32)> = inputs
            .into_iter()
            .map(|input| match input.kind {
                InputKind::Event(PlatformEvent::HypeTrain { change, train }) => {
                    (change, train.level, train.progress, train.goal, train.seconds_left)
                }
                other => panic!("not a hype train: {other:?}"),
            })
            .collect();
        assert_eq!(
            steps,
            [
                (HypeTrainChange::Started, 1, 125, 500, 300),
                (HypeTrainChange::Progressed { level_up: false }, 1, 300, 500, 240),
                (HypeTrainChange::Progressed { level_up: true }, 2, 100, 1000, 300),
                (HypeTrainChange::Ended, 2, 0, 0, 0),
            ]
        );
    }

    #[test]
    fn test_messages_have_the_platforms_own_emotes_marked() {
        use crate::model::EmoteRef;
        use crate::services::emotes::EmoteInfo;
        let registry = EmoteRegistry::for_tests();
        let info = |source: &str, code: &str| EmoteInfo {
            emote: EmoteRef { source: source.into(), id: code.to_lowercase(), code: code.into() },
            image_url: String::new(),
            zero_width: false,
        };
        registry.set_platform_channel(
            "twitch",
            "1",
            vec![info("twitch", "marrtiHi"), info("twitch", "Kappa")],
        );
        let room = Some(("twitch".to_string(), "1".to_string(), "marrtin".to_string()));
        let chat = TestEvent::Chat {
            viewer: String::new(),
            text: "Kappa  hi marrtiHi Kappa2 Kappa".into(),
            viewers: 2,
            first: false,
        };
        let inputs = test_inputs(chat, room.clone(), &registry);
        assert_eq!(inputs.len(), 2);
        for input in &inputs {
            let InputKind::Message(message) = &input.kind else { panic!("a message") };
            let marked: Vec<(usize, usize, &str)> =
                message.emotes.iter().map(|span| (span.start, span.end, span.emote.code.as_str())).collect();
            assert_eq!(marked, [(0, 5, "Kappa"), (10, 18, "marrtiHi"), (26, 31, "Kappa")]);
        }
        // A sub's message too.
        let sub = TestEvent::Sub { months: 3, message: "Kappa".into() };
        let [
            Input {
                kind: InputKind::Event(PlatformEvent::Membership { message: Some(message), .. }), ..
            },
        ] = &test_inputs(sub, room, &registry)[..]
        else {
            panic!("a sub with a message")
        };
        assert_eq!(message.emotes.len(), 1);
    }

    #[test]
    fn test_chat_comes_from_the_viewer_named() {
        let chat = |viewer: &str| TestEvent::Chat {
            viewer: viewer.into(),
            text: "hi".into(),
            viewers: 2,
            first: false,
        };
        let names = |inputs: Vec<Input>| -> Vec<(String, String, bool)> {
            inputs
                .into_iter()
                .map(|input| match input.kind {
                    InputKind::Message(message) => {
                        (message.user.display_name, message.user.name, message.user.roles.moderator)
                    }
                    _ => panic!("a message"),
                })
                .collect()
        };
        // The first is the one named, a mod as TestViewer is; the crowd stays.
        assert_eq!(
            names(unmarked_test_inputs(chat("Ann Bee"), None)),
            [("Ann Bee".into(), "ann bee".into(), true), ("TestViewer2".into(), "testviewer2".into(), false)]
        );
        assert_eq!(names(unmarked_test_inputs(chat(" "), None))[0].0, "TestViewer");
    }

    #[test]
    fn only_local_pages_may_use_the_panel() {
        let from = |origin: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(origin) = origin {
                headers.insert(header::ORIGIN, origin.parse().unwrap());
            }
            local_origin(&headers)
        };
        assert!(from(None), "not a web page");
        assert!(from(Some("http://localhost:3000")));
        assert!(from(Some("http://127.0.0.1:3100")));
        assert!(from(Some("http://[::1]:3000")));
        assert!(!from(Some("https://evil.example")));
        assert!(!from(Some("http://localhost.evil.example")));
        assert!(!from(Some("null")));
    }
}
