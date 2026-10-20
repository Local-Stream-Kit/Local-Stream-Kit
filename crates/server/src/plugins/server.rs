//! Plugins' server code: a WebAssembly component (`server.wasm` in the
//! plugin's folder) built for the `plugin` world in
//! `crates/protocol/wit/plugin.wit`, run by wasmtime. See
//! docs/writing-a-plugin.md → Server code.
//!
//! Each plugin runs on a thread of its own and is called with one thing at
//! a time ([`Call`]): `start`, its effect nodes' runs, what cards ask its
//! flow and value nodes, the chat and events it hears, its event nodes on
//! the cards, what its tab in the panel sends, timers, web answers, socket
//! messages, messages to read aloud in its voices, and the answers to its
//! moderating. The app does every bit of waiting (timers, web requests,
//! sockets, Twitch) on its own tasks and hands the results back as calls,
//! so a plugin never blocks and needs no async.
//!
//! The sandbox: WASI without files or network (stdout and stderr go to the
//! app's console), web requests and WebSockets only to the hosts in the
//! plugin's manifest (redirects too) and never to the app itself
//! ([`super::net`]), moderating only with the user's Allow, a time limit
//! per call, a memory limit, and limits on timers, sockets and moderations
//! waiting for an answer. A plugin that breaks one of
//! them, or crashes, is stopped (see [`Plugins::stop`]); everything else
//! keeps running.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use protocol::activity::ItemOrigin;
use protocol::cards::{Category, SocketType};
use protocol::config::AppConfig;
use protocol::plugin::{EmoteValue, PluginCommand, PluginInfo, PluginMessage, TextPart, TextValue};
use protocol::plugin::{Manifest, NodeType, Setting, shared_settings};
use protocol::{Command, Envelope};
use serde_json::{Map, Value as Json};
use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::AbortHandle;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::Plugins;
use super::net::{MAX_BODY, Net};
use crate::emote_text::EmoteText;
use crate::model::{ChannelRef, EmoteRef, Input, InputKind, Origin, PlatformEvent, PostedEmote};
use crate::pipeline::now_ms;
use crate::services::activity::{Activity, Asked, Asks, Heard, Pending};
use crate::services::overlay_hub::OverlayHub;
use crate::services::speech::{self, MAX_AUDIO, Speech};
use crate::services::status::StatusHub;

mod bindings {
    // The glue for `route` takes more arguments than clippy likes.
    #![allow(clippy::too_many_arguments)]
    wasmtime::component::bindgen!({ path: "../protocol/wit", world: "plugin" });
}
use bindings::*;

use lsk::plugin::types::{
    Audio, ChatLine, ChatMarks, Computed, Emote, EventValue, Header, HeardChat, HeardEvent, Level,
    Moderation, NamedEventValue, PlacedNode, RemovedViewer, Request, Roles, Route, StatusLevel, Text,
    TextPart as WitPart, Timeout, Value,
};

/// The provider a server plugin's events come from.
pub const PLUGIN_PROVIDER: &str = "plugin";
/// How often the time limit is checked.
const TICK: Duration = Duration::from_millis(10);
/// A call (start included) may take this many ticks: one second.
const CALL_TICKS: u64 = 100;
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// Its tables, together, and the elements in one.
const TABLE_LIMIT: usize = 64;
const TABLE_ELEMENTS_LIMIT: usize = 100_000;
const MAX_TIMERS: usize = 100;
const MIN_TIMER_MS: u32 = 10;
const MAX_SOCKETS: usize = 8;
/// Moderations waiting for Twitch's answer, at most.
const MAX_MODERATING: usize = 20;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Everything a plugin stores, together.
const MAX_STORED: usize = 1024 * 1024;

/// What a plugin's server code needs from the app.
#[derive(Clone)]
pub struct Deps {
    pub hub: OverlayHub,
    pub inputs: mpsc::Sender<Input>,
    pub status: StatusHub,
    /// Where plugins' stored values are kept, a file each.
    pub data_dir: PathBuf,
    pub runtime: Handle,
    /// For plugins' switches, allowed hosts and settings.
    pub config: watch::Receiver<Arc<AppConfig>>,
    /// The port the app listens on, which server code can't reach.
    pub app_port: u16,
    /// Where a plugin with voices takes messages to read.
    pub speech: Speech,
    /// The texts of plugins' file settings, given with their settings.
    pub files: super::files::PluginFiles,
    /// Plugins' secret settings, given with their settings.
    pub secrets: super::secrets::PluginSecrets,
    /// The lists of choices server code fills in.
    pub options: super::options::PluginOptions,
    /// For its effect nodes' own outputs (`carry-on`), and its queued runs
    /// (`done`; the queue plays them while it runs).
    pub activity: crate::services::activity::Activity,
    /// For settings that name a folder of pictures or sounds.
    pub media: crate::media::MediaFolders,
    /// What plugins' tabs in the panel send their code, and back.
    pub tabs: super::tabs::PluginTabs,
    /// For plugins that moderate the chat; none without a Twitch login to
    /// wait for (replaying a recording, the Twitch CLI).
    pub moderation: Option<crate::providers::twitch_eventsub::TwitchModeration>,
}

/// Runs the server code of every plugin that's on and whose hosts are
/// allowed, each on its own thread, and follows changes: a plugin switched
/// off or on, hosts allowed, a plugin installed, updated or removed.
/// Server code that stopped by itself (a crash, too slow) starts again
/// when its plugin is switched off and on, or updated.
pub fn start(plugins: &Arc<Plugins>, deps: Deps) {
    let mut config = wasmtime::Config::new();
    config.epoch_interruption(true);
    let engine = match Engine::new(&config) {
        Ok(engine) => engine,
        Err(error) => {
            for plugin in plugins.servers() {
                plugins.stop(&plugin.manifest.id, format!("the WebAssembly engine didn't start: {error:#}"));
            }
            return;
        }
    };
    {
        let engine = engine.clone();
        std::thread::Builder::new()
            .name("plugin-clock".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(TICK);
                    engine.increment_epoch();
                }
            })
            .expect("start the plugins' clock");
    }
    let plugins = Arc::clone(plugins);
    let runtime = deps.runtime.clone();
    runtime.spawn(async move {
        let mut config = deps.config.clone();
        let mut plugin_changes = plugins.subscribe();
        // Secrets and file settings are settings too (`settings-changed`).
        let (mut secrets, mut files) = (deps.secrets.subscribe(), deps.files.subscribe());
        let mut running: HashMap<String, Started> = HashMap::new();
        loop {
            let current = config.borrow_and_update().clone();
            plugin_changes.borrow_and_update();
            secrets.borrow_and_update();
            files.borrow_and_update();
            follow(&engine, &plugins, &deps, &current, &mut running);
            tokio::select! {
                changed = config.changed() => if changed.is_err() { break },
                changed = plugin_changes.changed() => if changed.is_err() { break },
                changed = secrets.changed() => if changed.is_err() { break },
                changed = files.changed() => if changed.is_err() { break },
            }
        }
    });
}

/// A plugin's server code that was started (it may have stopped since).
struct Started {
    calls: mpsc::UnboundedSender<Call>,
    /// Of the plugin when it was started.
    fingerprint: Option<u64>,
    /// Its settings as it was last told them ([`own_settings`]).
    settings: Json,
    /// Its event nodes on cards as it was last told them.
    placed: Vec<Placed>,
}

/// One of a plugin's event nodes placed on a card (`placed-node`, a
/// program's `nodes`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placed {
    /// `"<card id>/<node id>"`.
    pub id: String,
    /// Its node type's id.
    pub node: String,
    /// The card's name.
    pub card: String,
    pub on: bool,
    /// Its settings that aren't filters, as stored (every one with a
    /// value).
    settings: Map<String, Json>,
}

/// `manifest`'s event nodes on the cards in `config`.
pub(crate) fn placed_nodes(manifest: &Manifest, config: &AppConfig) -> Vec<Placed> {
    let mut placed = Vec::new();
    for card in &config.cards {
        for node in &card.nodes {
            let protocol::cards::NodeKind::Plugin { plugin, node: ty, settings, .. } = &node.kind else {
                continue;
            };
            let found =
                manifest.nodes.iter().find(|each| each.id == *ty && each.category == Category::Events);
            let Some(ty) = found.filter(|_| *plugin == manifest.id) else { continue };
            placed.push(Placed {
                id: crate::cards::placed_id(&card.id, node.id),
                node: ty.id.clone(),
                card: card.name.clone(),
                on: card.enabled,
                settings: ty
                    .settings
                    .iter()
                    .filter(|setting| !ty.is_filter(setting))
                    .map(|setting| (setting.key.clone(), setting.read(settings.get(&setting.key))))
                    .collect(),
            });
        }
    }
    placed
}

/// A placed event node's settings as its plugin gets them (`ty` its node
/// type): as an effect node's run has them (a text as a text, a
/// picture's or sound's path, one from a folder picked now), a list
/// (`many`) as a text with a line each.
pub(crate) fn placed_settings(
    ty: Option<&NodeType>,
    placed: &Placed,
    media: &crate::media::MediaFolders,
) -> Map<String, Json> {
    placed
        .settings
        .iter()
        .filter_map(|(key, stored)| {
            let setting = ty?.setting(key)?;
            let json = match (&setting.kind, stored) {
                (protocol::plugin::SettingKind::Text { .. }, Json::Array(list)) => {
                    let lines: Vec<&str> = list.iter().filter_map(Json::as_str).collect();
                    super::text_value(&EmoteText::plain_text(lines.join("\n")))
                }
                (protocol::plugin::SettingKind::Text { .. }, Json::String(text)) => {
                    super::text_value(&EmoteText::plain_text(text.clone()))
                }
                _ => protocol::plugin::shared_value(setting, Some(stored), &mut |kind, folder, every| {
                    media.pick_files_any(kind, folder, every)
                }),
            };
            Some((key.clone(), json))
        })
        .collect()
}

/// What `host.settings` is made from for plugin `id`, as stored: its
/// plugin-wide settings in the config, its secrets and its file settings'
/// texts. A change is a `settings-changed` call. (Not `host.settings`'
/// own answer: a setting naming a folder picks a file from it each time.)
fn own_settings(deps: &Deps, config: &AppConfig, id: &str) -> Json {
    let stored = config.plugin(id);
    serde_json::json!({
        "stored": stored.shared_settings(),
        "secrets": deps.secrets.values(id),
        "files": deps.files.values(id, stored.shared_settings()),
    })
}

/// Starts and stops server code to match the config and the plugins.
fn follow(
    engine: &Engine,
    plugins: &Arc<Plugins>,
    deps: &Deps,
    config: &AppConfig,
    running: &mut HashMap<String, Started>,
) {
    let servers = plugins.servers();
    running.retain(|id, started| {
        let installed = servers.iter().any(|plugin| plugin.manifest.id == *id);
        if !installed {
            let _ = started.calls.send(Call::Stop);
            plugins.clear_stopped(id);
        }
        installed
    });
    for info in servers {
        let id = info.manifest.id.clone();
        let settings = config.plugin(&id);
        let allowed = protocol::plugin::server_hosts_allowed(&info, &settings.hosts);
        let wanted = settings.enabled && allowed;
        let fingerprint = plugins.fingerprint(&id);
        let settings = own_settings(deps, config, &id);
        let placed = placed_nodes(&info.manifest, config);
        if wanted
            && let Some(started) = running.get_mut(&id).filter(|started| started.fingerprint == fingerprint)
        {
            if started.settings != settings {
                started.settings = settings;
                let _ = started.calls.send(Call::SettingsChanged);
            }
            if started.placed != placed {
                started.placed = placed.clone();
                let _ = started.calls.send(Call::NodesChanged(placed));
            }
            continue;
        }
        if let Some(started) = running.remove(&id) {
            let _ = started.calls.send(Call::Stop);
            tracing::info!(plugin = id, "the plugin's server code was stopped");
        }
        plugins.clear_stopped(&id);
        if !wanted {
            continue;
        }
        let (calls, inbox) = mpsc::unbounded_channel();
        // Listening from now: what comes while it compiles waits for it.
        // Here, not on its thread, so copies take over what cards ask in
        // the order they start (one that's stopping never takes it back).
        let (asks, asked) = deps.activity.asks(&id);
        let listening =
            Listening { commands: deps.hub.subscribe(), tab: deps.tabs.code_listens(), asks, asked };
        // Told its placed nodes right after it starts.
        let _ = calls.send(Call::NodesChanged(placed.clone()));
        let (engine, plugins, deps, own) = (engine.clone(), Arc::clone(plugins), deps.clone(), calls.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("plugin-{id}"))
            .spawn(move || run_plugin(&engine, info, &plugins, deps, listening, own, inbox));
        match spawned {
            Ok(_) => {
                running.insert(id, Started { calls, fingerprint, settings, placed });
            }
            Err(error) => tracing::error!(plugin = id, "couldn't start the plugin's thread: {error}"),
        }
    }
}

/// Something for the plugin to handle.
#[derive(Debug)]
enum Call {
    /// Its plugin was switched off, updated or removed.
    Stop,
    Run {
        node: String,
        input: String,
        values: Map<String, Json>,
        run_id: Option<u64>,
        queue_id: Option<u64>,
        origin: ItemOrigin,
    },
    /// The queued run on stage was skipped.
    Skip(u64),
    /// Its plugin-wide settings changed.
    SettingsChanged,
    Timer(u32),
    Response(u32, Result<Response, String>),
    Socket(u32, SocketEvent),
    /// A message to read in one of its voices.
    Speak(speech::SpeakRequest),
    /// Something a card asks it (a flow or value node's answer), or a wake
    /// for what it hears.
    Asked(Asked),
    /// Its event nodes placed on cards, all of them, after a change (and
    /// once at the start).
    NodesChanged(Vec<Placed>),
    /// A message from its tab in the panel, as JSON.
    Tab(String),
    /// Twitch's answer to one of its `moderate` calls.
    Moderated(u32, Result<(), String>),
}

/// What a plugin's server code listens to from the moment [`follow`]
/// starts it, before it compiles.
struct Listening {
    /// The overlay hub: its effect nodes' runs.
    commands: broadcast::Receiver<Arc<Envelope>>,
    /// What tabs in the panel send (its own and other plugins').
    tab: broadcast::Receiver<Arc<super::tabs::TabMessage>>,
    /// What cards ask it.
    asks: Asks,
    asked: mpsc::UnboundedReceiver<Asked>,
}

fn run_plugin(
    engine: &Engine,
    info: PluginInfo,
    plugins: &Arc<Plugins>,
    deps: Deps,
    listening: Listening,
    calls: mpsc::UnboundedSender<Call>,
    mut inbox: mpsc::UnboundedReceiver<Call>,
) {
    let id = info.manifest.id.clone();
    let Listening { mut commands, tab, asks, asked } = listening;
    let asking = forward_asks(&deps.runtime, asked, calls.clone());
    let mut running = match Running::new(engine, &info, plugins, &deps, calls.clone()) {
        Ok(running) => running,
        Err(reason) => {
            let_go(&deps.activity, &deps.runtime, &id, asks, asking, &mut inbox);
            return plugins.stop(&id, reason);
        }
    };
    tracing::info!(plugin = id, "the plugin's server code is running");
    // Messages to read in its voices, while it runs.
    let speaker = (!info.manifest.voices.is_empty()).then(|| {
        let (speaker, mut asked) = mpsc::unbounded_channel();
        deps.speech.add_speaker(&id, speaker.clone());
        let calls = calls.clone();
        let task = deps.runtime.spawn(async move {
            while let Some(request) = asked.recv().await {
                if calls.send(Call::Speak(request)).is_err() {
                    break;
                }
            }
        });
        (speaker, task)
    });
    let from_tab = forward_tab(&deps.runtime, tab, &id, calls.clone());
    // Its effect nodes' runs, from the overlay hub (the overlays leave them
    // to the server code).
    let forward = {
        let id = id.clone();
        deps.runtime.spawn(async move {
            loop {
                match commands.recv().await {
                    Ok(envelope) => {
                        if let Some(call) = run_call(&envelope, &id)
                            && calls.send(call).is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            plugin = id,
                            skipped,
                            "the plugin fell behind; some runs were skipped"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };
    // Its queued runs play while it runs (again from the start, if it
    // stopped mid-run).
    let serving = deps.activity.serving(&id);
    let result = running.call(|plugin, store| plugin.call_start(store));
    let result = result.and_then(|()| {
        while let Some(call) = inbox.blocking_recv() {
            if matches!(call, Call::Stop) {
                break;
            }
            running.handle(call, &asks)?;
        }
        Ok(())
    });
    forward.abort();
    from_tab.abort();
    let_go(&deps.activity, &deps.runtime, &id, asks, asking, &mut inbox);
    drop(serving);
    if let Some((speaker, task)) = speaker {
        deps.speech.remove_speaker(&id, &speaker);
        task.abort();
    }
    let state = running.store.data_mut();
    state.stop_io();
    state.forget_later();
    if let Err(reason) = result {
        plugins.stop(&id, reason);
    }
}

/// Passes what plugin `id`'s tab sends on to its calls, as JSON text.
fn forward_tab(
    runtime: &Handle,
    mut tab: broadcast::Receiver<Arc<super::tabs::TabMessage>>,
    id: &str,
    calls: mpsc::UnboundedSender<Call>,
) -> tokio::task::JoinHandle<()> {
    let id = id.to_string();
    runtime.spawn(async move {
        loop {
            match tab.recv().await {
                Ok(message) if message.plugin == id => {
                    if calls.send(Call::Tab(message.data.to_string())).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        plugin = id,
                        skipped,
                        "the plugin fell behind; some of its tab's messages were skipped"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Passes what cards ask a plugin's server code on to its calls, until
/// nothing more can be asked of this copy ([`Asks`] dropped, or a newer
/// copy's taken over).
fn forward_asks(
    runtime: &Handle,
    mut asked: mpsc::UnboundedReceiver<Asked>,
    calls: mpsc::UnboundedSender<Call>,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        while let Some(asked) = asked.recv().await {
            if calls.send(Call::Asked(asked)).is_err() {
                break;
            }
        }
    })
}

/// A copy of plugin `id`'s server code stopped (or never started): nothing
/// more that cards ask comes to it, and what came that it won't answer
/// (on its way, or waiting in `inbox`) is let go at once
/// ([`Activity::unanswered`]), rather than waiting for an answer that
/// never comes. Called on its own thread, outside the runtime.
fn let_go(
    activity: &Activity,
    runtime: &Handle,
    id: &str,
    asks: Asks,
    asking: tokio::task::JoinHandle<()>,
    inbox: &mut mpsc::UnboundedReceiver<Call>,
) {
    drop(asks);
    // Its inbox is gone (or a newer copy's took over), so the forwarding
    // ends once it has passed on what was on its way.
    let _ = runtime.block_on(asking);
    inbox.close();
    while let Ok(call) = inbox.try_recv() {
        if let Call::Asked(asked) = call {
            activity.unanswered(id, asked);
        }
    }
}

/// An overlay command that is a run of one of `plugin`'s effect nodes, or
/// a skip of its queued run.
fn run_call(envelope: &Envelope, plugin: &str) -> Option<Call> {
    let Command::Plugin(command) = &envelope.command else { return None };
    if command.plugin != plugin {
        return None;
    }
    match &command.message {
        PluginMessage::Run { node, input, values, run_id, queue_id, origin, .. } => Some(Call::Run {
            node: node.clone(),
            input: input.clone(),
            values: values.clone(),
            run_id: *run_id,
            queue_id: *queue_id,
            origin: *origin,
        }),
        PluginMessage::Skip { queue_id } => Some(Call::Skip(*queue_id)),
        _ => None,
    }
}

/// A plugin's instance, ready for calls.
struct Running {
    store: Store<State>,
    bindings: Plugin,
}

impl Running {
    fn new(
        engine: &Engine,
        info: &PluginInfo,
        plugins: &Arc<Plugins>,
        deps: &Deps,
        calls: mpsc::UnboundedSender<Call>,
    ) -> Result<Running, String> {
        let id = &info.manifest.id;
        let bytes = plugins.server_file(id).ok_or_else(|| "its server.wasm can't be read".to_string())?;
        let compiling = std::time::Instant::now();
        let component = Component::new(engine, &bytes)
            .map_err(|error| format!("its server.wasm isn't a WebAssembly component: {error:#}"))?;
        let ms = compiling.elapsed().as_millis();
        tracing::info!(plugin = id, kb = bytes.len() / 1024, ms, "compiled its server code in {ms} ms");
        let mut linker = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|error| format!("{error:#}"))?;
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(|error| format!("{error:#}"))?;
        let state = State::new(info, Arc::clone(plugins), deps, calls)?;
        let mut store = Store::new(engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(CALL_TICKS);
        let bindings = Plugin::instantiate(&mut store, &component, &linker).map_err(|error| {
            format!("its server.wasm doesn't fit this app's plugin interface (wit/plugin.wit): {error:#}")
        })?;
        Ok(Running { store, bindings })
    }

    /// Calls the plugin within its time limit.
    fn call(
        &mut self,
        call: impl FnOnce(&Plugin, &mut Store<State>) -> wasmtime::Result<()>,
    ) -> Result<(), String> {
        self.store.set_epoch_deadline(CALL_TICKS);
        call(&self.bindings, &mut self.store).map_err(|error| {
            if error.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::Interrupt) {
                "it took longer than a second to handle something".to_string()
            } else {
                format!("it crashed: {error:#}")
            }
        })
    }

    /// A chat message or an event it hears.
    fn hear(&mut self, heard: Heard) -> Result<(), String> {
        match heard {
            Heard::Chat(message) => match heard_chat(message) {
                Some(chat) => self.call(|plugin, store| plugin.call_on_chat(store, &chat)),
                None => Ok(()),
            },
            Heard::Event { kind, summary, viewer, values, origin } => {
                let event = HeardEvent {
                    kind,
                    summary,
                    viewer: viewer.map(viewer_of),
                    values: named_values(&values),
                    origin: origin_of(origin),
                };
                self.call(|plugin, store| plugin.call_on_event(store, &event))
            }
        }
    }

    /// One call; `asks` has what it hears.
    fn handle(&mut self, call: Call, asks: &Asks) -> Result<(), String> {
        match call {
            Call::Stop => Ok(()),
            Call::Run { node, input, mut values, run_id, queue_id, origin } => {
                // Its `viewer`, for a node that asks for it, as a viewer.
                let viewer = self
                    .store
                    .data()
                    .viewers
                    .contains(&node)
                    .then(|| values.remove(protocol::plugin::VIEWER_VALUE))
                    .flatten()
                    .and_then(|viewer| serde_json::from_value(viewer).ok())
                    .map(viewer_of);
                let values = named_values(&values);
                self.call(|plugin, store| {
                    plugin.call_run(
                        store,
                        &node,
                        &input,
                        &values,
                        viewer.as_ref(),
                        run_id,
                        queue_id,
                        origin_of(origin),
                    )
                })
            }
            Call::Asked(Asked::Route { node, input, values, viewer, run, origin }) => {
                let values = named_values(&values);
                let viewer = viewer.map(viewer_of);
                let mut routed = None;
                let result = self.call(|plugin, store| {
                    let answer = plugin.call_route(
                        store,
                        &node,
                        &input,
                        &values,
                        viewer.as_ref(),
                        run,
                        origin_of(origin),
                    )?;
                    routed = Some(answer);
                    Ok(())
                });
                let state = self.store.data_mut();
                match routed {
                    Some(routed) => state.routed(&node, run, routed),
                    // It failed, and stops: as if it weren't running.
                    None => state.deps.activity.forget_run(&state.id, run),
                }
                result
            }
            Call::Asked(Asked::Compute { node, values, run, origin }) => {
                let values = named_values(&values);
                let mut computed = None;
                let result = self.call(|plugin, store| {
                    computed = Some(plugin.call_compute(store, &node, &values, run, origin_of(origin))?);
                    Ok(())
                });
                let state = self.store.data_mut();
                match computed {
                    Some(Computed::Now(values)) => state.computed(&node, run, values),
                    Some(Computed::Later) => state.later_values.park(run, &node),
                    // It failed, and stops: the node gives nothing.
                    None => {
                        let _ = state.deps.activity.computed(&state.id, run, Vec::new());
                    }
                }
                result
            }
            // One heard at a time: whatever else came meanwhile goes first,
            // then it's woken again for the next.
            Call::Asked(Asked::Heard) => {
                let (heard, more) = asks.next_heard();
                if more {
                    let _ = self.store.data().calls.send(Call::Asked(Asked::Heard));
                }
                match heard {
                    Some(heard) => self.hear(heard),
                    None => Ok(()),
                }
            }
            Call::NodesChanged(placed) => {
                let state = self.store.data_mut();
                let nodes: Vec<PlacedNode> = placed.iter().map(|each| state.placed_node(each)).collect();
                state.placed = placed.into_iter().map(|each| (each.id, each.node)).collect();
                self.call(|plugin, store| plugin.call_nodes_changed(store, &nodes))
            }
            Call::Skip(queue_id) => self.call(|plugin, store| plugin.call_skip(store, queue_id)),
            Call::Tab(json) => self.call(|plugin, store| plugin.call_on_tab(store, &json)),
            Call::Moderated(id, done) => {
                let state = self.store.data_mut();
                state.moderating = state.moderating.saturating_sub(1);
                let done = done.as_ref().map(|_| ()).map_err(String::as_str);
                self.call(|plugin, store| plugin.call_on_moderated(store, id, done))
            }
            Call::SettingsChanged => self.call(|plugin, store| plugin.call_settings_changed(store)),
            Call::Timer(id) => {
                // A one-off timer is done once it fires.
                let state = self.store.data_mut();
                if state.timers.get(&id).is_some_and(|timer| !timer.repeat) {
                    state.timers.remove(&id);
                }
                self.call(|plugin, store| plugin.call_on_timer(store, id))
            }
            Call::Response(id, response) => self.call(|plugin, store| {
                plugin.call_on_response(store, id, response.as_ref().map_err(String::as_str))
            }),
            Call::Socket(id, event) => {
                if matches!(event, SocketEvent::Closed(_)) {
                    self.store.data_mut().sockets.remove(&id);
                }
                self.call(|plugin, store| plugin.call_on_socket(store, id, &event))
            }
            Call::Speak(speech::SpeakRequest { voice, text, speed, answer }) => {
                let state = self.store.data_mut();
                let request = state.next_id();
                state.speaking.insert(request, answer);
                self.call(|plugin, store| plugin.call_speak(store, request, &voice, &text, speed))
            }
        }
    }
}

/// A chat message or a moderator taking some off, as a plugin hears it;
/// `None` for a line without a viewer.
fn heard_chat(message: PluginMessage) -> Option<HeardChat> {
    Some(match message {
        PluginMessage::Chat { line } => {
            let viewer = line.user.map(|user| {
                viewer_of(protocol::plugin::RunViewer { platform: line.platform.clone(), user })
            })?;
            let marks = line.marks;
            HeardChat::Line(ChatLine {
                id: line.id,
                platform: line.platform,
                at_ms: line.at_ms,
                viewer,
                text: text_of(&line.parts),
                marks: ChatMarks {
                    action: marks.action,
                    highlighted: marks.highlighted,
                    first: marks.first,
                    reward: marks.reward,
                    cheer: marks.cheer,
                    reply: marks.reply,
                },
            })
        }
        PluginMessage::ChatRemoved { id } => HeardChat::Removed(id),
        PluginMessage::ChatUserRemoved { platform, user_id } => {
            HeardChat::ViewerRemoved(RemovedViewer { platform, id: user_id })
        }
        PluginMessage::ChatCleared { platform } => HeardChat::Cleared(platform),
        _ => return None,
    })
}

/// A chat line's parts as a WIT `text`.
fn text_of(parts: &[protocol::AlertPart]) -> Text {
    let mut plain = String::new();
    let parts = parts
        .iter()
        .map(|part| match part {
            protocol::AlertPart::Text { text, .. } => {
                plain.push_str(text);
                WitPart::Text(text.clone())
            }
            protocol::AlertPart::Emote { code, sprite, .. } => {
                plain.push_str(code);
                WitPart::Emote(Emote {
                    code: code.clone(),
                    url: sprite.image.path(),
                    overlays: sprite.overlays.iter().map(protocol::EmoteImage::path).collect(),
                })
            }
        })
        .collect();
    Text { plain, parts }
}

/// What a plugin gave a flow or value node's outputs, as they carry it:
/// only its value outputs, each as its type.
fn outputs_of(ty: &NodeType, values: Vec<NamedValue>) -> Vec<(String, crate::cards::Value)> {
    values
        .into_iter()
        .filter_map(|NamedValue { name, value }| {
            let output = ty.outputs.iter().find(|own| own.name == name && own.ty != SocketType::Event)?;
            Some((name, card_value(value)?.into_type(output.ty)?))
        })
        .collect()
}

/// What a program gave a flow or value node's outputs (as a run's values
/// are: a number, `true` or `false`, a plain string, a text, emotes), as
/// they carry it ([`outputs_of`]).
pub(crate) fn outputs_of_json(
    ty: &NodeType,
    values: &Map<String, Json>,
) -> Vec<(String, crate::cards::Value)> {
    outputs_of(ty, named_values(values))
}

/// An effect or flow node's values, as the plugin gets them.
fn named_values(values: &Map<String, Json>) -> Vec<NamedValue> {
    values.iter().map(|(name, value)| NamedValue { name: name.clone(), value: value_of(value) }).collect()
}

fn viewer_of(viewer: protocol::plugin::RunViewer) -> lsk::plugin::types::Viewer {
    let roles = viewer.user.roles;
    lsk::plugin::types::Viewer {
        platform: viewer.platform,
        id: viewer.user.id,
        name: viewer.user.name,
        display_name: viewer.user.display_name,
        color: viewer.user.color,
        roles: Roles {
            broadcaster: roles.broadcaster,
            moderator: roles.moderator,
            vip: roles.vip,
            member: roles.member,
        },
    }
}

fn origin_of(origin: protocol::activity::ItemOrigin) -> RunOrigin {
    match origin {
        protocol::activity::ItemOrigin::Live => RunOrigin::Live,
        protocol::activity::ItemOrigin::Test => RunOrigin::Test,
        protocol::activity::ItemOrigin::Replay => RunOrigin::Replay,
    }
}

/// A value the plugin gives (a flow node's output), as a card's wire
/// carries it: a tick box as 1 or 0, a plain string as text; nothing for
/// `empty`.
fn card_value(value: Value) -> Option<crate::cards::Value> {
    use crate::cards::Value as Card;
    let posted = |emote: Emote| {
        let image = protocol::chat::emote_image(&emote.url)?;
        let overlays = emote
            .overlays
            .iter()
            .filter_map(|path| protocol::chat::emote_image(path))
            .map(|image| EmoteRef { source: image.source, id: image.id, code: String::new() })
            .collect();
        Some(PostedEmote {
            emote: EmoteRef { source: image.source, id: image.id, code: emote.code },
            overlays,
        })
    };
    Some(match value {
        Value::Number(number) => Card::Number(number),
        Value::Toggle(on) => Card::Number(if on { 1.0 } else { 0.0 }),
        Value::Plain(text) => Card::Text(EmoteText::plain_text(text)),
        Value::Text(text) => {
            let mut out = EmoteText::default();
            for part in text.parts {
                match part {
                    WitPart::Text(text) => out.push_text(&text),
                    WitPart::Emote(emote) => {
                        let code = emote.code.clone();
                        match posted(emote) {
                            Some(posted) => out.push_posted(posted),
                            None => out.push_text(&code),
                        }
                    }
                }
            }
            Card::Text(out)
        }
        Value::Emotes(emotes) => Card::Emotes(emotes.into_iter().filter_map(posted).collect()),
        Value::Empty => return None,
    })
}

/// A value an effect node sends, as the plugin gets it (see
/// `cards::plugin_values` for how they're made).
fn value_of(json: &Json) -> Value {
    match json {
        Json::Number(number) => Value::Number(number.as_f64().unwrap_or_default()),
        Json::Bool(yes) => Value::Toggle(*yes),
        Json::String(text) => Value::Plain(text.clone()),
        Json::Object(_) => match serde_json::from_value::<TextValue>(json.clone()) {
            Ok(text) => Value::Text(Text {
                plain: text.text,
                parts: text
                    .parts
                    .into_iter()
                    .map(|part| match part {
                        TextPart::Text { text, .. } => WitPart::Text(text),
                        TextPart::Emote { emote, .. } => WitPart::Emote(emote_of(emote)),
                    })
                    .collect(),
            }),
            Err(_) => Value::Empty,
        },
        Json::Array(_) => match serde_json::from_value::<Vec<EmoteValue>>(json.clone()) {
            Ok(emotes) => Value::Emotes(emotes.into_iter().map(emote_of).collect()),
            Err(_) => Value::Empty,
        },
        Json::Null => Value::Empty,
    }
}

fn emote_of(emote: EmoteValue) -> Emote {
    Emote { code: emote.code, url: emote.url, overlays: emote.overlays }
}

struct Timer {
    task: AbortHandle,
    repeat: bool,
}

/// The plugin's side of the app: what its imports reach.
struct State {
    id: String,
    /// Its event nodes, which `emit` and `emit-output` may fire.
    events: Vec<NodeType>,
    /// Its effect nodes' own outputs, which `carry-on` may fire.
    outputs: Vec<String>,
    /// Its flow nodes.
    flows: Vec<NodeType>,
    /// Its value nodes.
    values: Vec<NodeType>,
    /// Its effect nodes whose runs bring the viewer (`"viewer": true`).
    viewers: Vec<String>,
    /// For whether it may moderate ([`protocol::plugin::moderates_now`]).
    info: PluginInfo,
    /// Its moderations waiting for Twitch's answer.
    moderating: usize,
    /// Flow nodes' runs it answers later (`route-later`).
    later: Pending,
    /// Value nodes' runs it answers later (`compute-later`).
    later_values: Pending,
    /// Its event nodes placed on cards, which `emit-to` may fire: their
    /// node type, by placed id.
    placed: HashMap<String, String>,
    /// For its status (`set-status`).
    plugins: Arc<Plugins>,
    /// Its plugin-wide settings, as the manifest declares them.
    settings: Vec<Setting>,
    /// Its tab has a number on it (its manifest's `tab.count`).
    tab_counts: bool,
    deps: Deps,
    calls: mpsc::UnboundedSender<Call>,
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    net: Net,
    next_id: u32,
    timers: HashMap<u32, Timer>,
    /// Open sockets: what to send on each. Dropping one closes it.
    sockets: HashMap<u32, mpsc::UnboundedSender<Outgoing>>,
    stored: BTreeMap<String, String>,
    stored_path: PathBuf,
    /// Messages it was asked to read and hasn't answered (`spoken`).
    speaking: HashMap<u32, oneshot::Sender<Result<speech::Audio, String>>>,
}

impl State {
    fn new(
        info: &PluginInfo,
        plugins: Arc<Plugins>,
        deps: &Deps,
        calls: mpsc::UnboundedSender<Call>,
    ) -> Result<State, String> {
        let id = info.manifest.id.clone();
        let (listening, config) = (deps.app_port, deps.config.clone());
        let app_ports = move || [listening, config.borrow().connections.overlay_port];
        let net = Net::new(info.manifest.hosts.clone(), app_ports, FETCH_TIMEOUT)?;
        let stored_path = deps.data_dir.join(format!("{id}.json"));
        let stored = std::fs::read_to_string(&stored_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let wasi = WasiCtx::builder()
            .inherit_stdout()
            .inherit_stderr()
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .build();
        Ok(State {
            events: info
                .manifest
                .nodes
                .iter()
                .filter(|node| node.category == Category::Events)
                .cloned()
                .collect(),
            outputs: info
                .manifest
                .nodes
                .iter()
                .filter(|node| node.category == Category::Effects)
                .flat_map(|node| node.outputs.iter().map(|output| output.name.clone()))
                .collect(),
            flows: info
                .manifest
                .nodes
                .iter()
                .filter(|node| node.category == Category::Flow)
                .cloned()
                .collect(),
            values: info
                .manifest
                .nodes
                .iter()
                .filter(|node| node.category == Category::Values)
                .cloned()
                .collect(),
            viewers: info
                .manifest
                .nodes
                .iter()
                .filter(|node| node.category == Category::Effects && node.viewer)
                .map(|node| node.id.clone())
                .collect(),
            info: info.clone(),
            moderating: 0,
            later: Pending::default(),
            later_values: Pending::default(),
            placed: HashMap::new(),
            settings: info.manifest.settings.clone(),
            tab_counts: info.manifest.tab.as_ref().is_some_and(|tab| tab.count),
            plugins,
            id,
            deps: deps.clone(),
            calls,
            wasi,
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY_LIMIT)
                .instances(16)
                .tables(TABLE_LIMIT)
                .table_elements(TABLE_ELEMENTS_LIMIT)
                .build(),
            net,
            next_id: 1,
            timers: HashMap::new(),
            sockets: HashMap::new(),
            stored,
            stored_path,
            speaking: HashMap::new(),
        })
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Stops the plugin's timers and closes its sockets.
    fn stop_io(&mut self) {
        for timer in self.timers.values() {
            timer.task.abort();
        }
        self.timers.clear();
        self.sockets.clear();
    }

    /// Sends `message` on socket `socket` (`call`: `send`, `send-binary`);
    /// one bigger than [`MAX_BODY`] isn't sent (the log says so).
    fn send_on(&self, socket: u32, call: &str, message: Outgoing) {
        if message.len() > MAX_BODY {
            tracing::warn!(
                plugin = self.id,
                socket,
                "{call}: a message can be {} MB at most; not sent",
                MAX_BODY / 1024 / 1024
            );
            return;
        }
        match self.sockets.get(&socket) {
            Some(outgoing) => {
                let _ = outgoing.send(message);
            }
            None => tracing::debug!(plugin = self.id, socket, "{call}: no such socket"),
        }
    }

    /// It stopped: the runs it said it answers later won't be answered
    /// (`route-later` and `compute-later` don't reach across a restart), so
    /// they're let go at once, as if it weren't running.
    fn forget_later(&mut self) {
        for run in self.later.drain() {
            self.deps.activity.forget_run(&self.id, run);
        }
        for run in self.later_values.drain() {
            let _ = self.deps.activity.computed(&self.id, run, Vec::new());
        }
    }

    fn save_stored(&self) {
        let json = serde_json::to_string_pretty(&self.stored).expect("stored values serialise");
        let temp = self.stored_path.with_extension("json.tmp");
        let written = self
            .stored_path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&temp, json))
            .and_then(|()| std::fs::rename(&temp, &self.stored_path));
        if let Err(error) = written {
            tracing::warn!(plugin = self.id, "couldn't save the plugin's values: {error}");
        }
    }

    /// Its answer to a flow node's `route`.
    fn routed(&mut self, node: &str, run: u64, routed: Routed) {
        match routed {
            Routed::Go(Route { output, values }) => {
                if let Err(why) = self.route_on(node, run, &output, values) {
                    tracing::warn!(plugin = self.id, node, "route: {why}");
                }
                self.deps.activity.forget_run(&self.id, run);
            }
            Routed::Stop => self.deps.activity.forget_run(&self.id, run),
            Routed::Later => self.later.park(run, node),
        }
    }

    /// Flow node `node`'s event, parked as `run`, goes on from `output`,
    /// with `values` on its value outputs.
    fn route_on(&self, node: &str, run: u64, output: &str, values: Vec<NamedValue>) -> Result<(), String> {
        let Some(ty) = self.flows.iter().find(|ty| ty.id == node) else {
            return Err(format!("it has no flow node {node:?}"));
        };
        if !ty.fires(output) {
            return Err(format!("the node has no event output {output:?}"));
        }
        self.deps.activity.carry_on_with(&self.id, run, output, outputs_of(ty, values))
    }

    /// Value node `node`'s answer for run `run`.
    fn computed(&self, node: &str, run: u64, values: Vec<NamedValue>) {
        let result = match self.values.iter().find(|ty| ty.id == node) {
            Some(ty) => self.deps.activity.computed(&self.id, run, outputs_of(ty, values)),
            None => Err(format!("it has no value node {node:?}")),
        };
        if let Err(why) = result {
            tracing::warn!(plugin = self.id, node, "compute: {why}");
        }
    }

    /// A placed event node as the plugin gets it ([`placed_settings`]).
    fn placed_node(&self, placed: &Placed) -> PlacedNode {
        let ty = self.events.iter().find(|ty| ty.id == placed.node);
        PlacedNode {
            id: placed.id.clone(),
            node: placed.node.clone(),
            card: placed.card.clone(),
            on: placed.on,
            settings: named_values(&placed_settings(ty, placed, &self.deps.media)),
        }
    }

    /// Sends one of its event nodes' events into the pipeline: `output`
    /// when it isn't `event`, to one placed copy of it (`to`) or all.
    fn fire(
        &mut self,
        node: String,
        output: Option<String>,
        values: Vec<NamedEventValue>,
        to: Option<String>,
    ) {
        let values: Map<String, Json> = values
            .into_iter()
            .map(|NamedEventValue { name, value }| {
                let value = match value {
                    EventValue::Number(number) => Json::from(number),
                    EventValue::Text(text) => Json::String(text),
                    EventValue::Emotes(codes) => Json::from(codes),
                };
                (name, value)
            })
            .collect();
        let (platform, channel_id, channel) =
            self.deps.status.room().unwrap_or_else(|| ("twitch".into(), String::new(), String::new()));
        let input = Input {
            at_ms: now_ms(),
            origin: Origin {
                provider: PLUGIN_PROVIDER.into(),
                platform,
                channel: ChannelRef { id: channel_id, name: channel },
            },
            kind: InputKind::Event(PlatformEvent::Plugin {
                plugin: self.id.clone(),
                node,
                values,
                output,
                to,
            }),
        };
        if self.deps.inputs.blocking_send(input).is_err() {
            tracing::warn!(plugin = self.id, "emit: the pipeline has stopped");
        }
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl lsk::plugin::types::Host for State {}

impl lsk::plugin::host::Host for State {
    fn log(&mut self, level: Level, message: String) {
        let plugin = self.id.as_str();
        match level {
            Level::Error => tracing::error!(plugin, "{message}"),
            Level::Warn => tracing::warn!(plugin, "{message}"),
            Level::Info => tracing::info!(plugin, "{message}"),
            Level::Debug => tracing::debug!(plugin, "{message}"),
        }
    }

    fn emit(&mut self, node: String, values: Vec<NamedEventValue>) {
        if !self.events.iter().any(|ty| ty.id == node) {
            tracing::warn!(plugin = self.id, node, "emit: the plugin has no event node by that name");
            return;
        }
        self.fire(node, None, values, None);
    }

    fn emit_output(&mut self, node: String, output: String, values: Vec<NamedEventValue>) {
        let Some(ty) = self.events.iter().find(|ty| ty.id == node) else {
            tracing::warn!(plugin = self.id, node, "emit-output: the plugin has no event node by that name");
            return;
        };
        if !ty.fires(&output) {
            tracing::warn!(
                plugin = self.id,
                node,
                output,
                "emit-output: the node has no event output by that name"
            );
            return;
        }
        self.fire(node, (output != "event").then_some(output), values, None);
    }

    fn send_to_page(&mut self, json: String) {
        match serde_json::from_str::<Json>(&json) {
            Ok(data) => self.deps.hub.send(Command::Plugin(PluginCommand {
                plugin: self.id.clone(),
                message: PluginMessage::Message { data },
            })),
            Err(error) => tracing::warn!(plugin = self.id, "send-to-page: not JSON ({error})"),
        }
    }

    fn set_timer(&mut self, ms: u32, repeat: bool) -> u32 {
        let id = self.next_id();
        if self.timers.len() >= MAX_TIMERS {
            tracing::warn!(
                plugin = self.id,
                "set-timer: it has {MAX_TIMERS} timers already; this one won't fire"
            );
            return id;
        }
        let every = Duration::from_millis(u64::from(ms.max(MIN_TIMER_MS)));
        let calls = self.calls.clone();
        let task = self.deps.runtime.spawn(async move {
            let mut next = tokio::time::Instant::now() + every;
            loop {
                tokio::time::sleep_until(next).await;
                if calls.send(Call::Timer(id)).is_err() || !repeat {
                    break;
                }
                next += every;
            }
        });
        self.timers.insert(id, Timer { task: task.abort_handle(), repeat });
        id
    }

    fn cancel_timer(&mut self, id: u32) {
        if let Some(timer) = self.timers.remove(&id) {
            timer.task.abort();
        }
    }

    fn fetch(&mut self, request: Request) -> Result<u32, String> {
        let url = self.net.check(&request.url, ["http", "https"])?;
        let method = reqwest::Method::from_bytes(request.method.to_ascii_uppercase().as_bytes())
            .map_err(|_| format!("{:?} isn't a request method", request.method))?;
        let headers = request.headers.into_iter().map(|Header { name, value }| (name, value)).collect();
        let id = self.next_id();
        let calls = self.calls.clone();
        let net = self.net.clone();
        self.deps.runtime.spawn(async move {
            let response = net.fetch(method, url, headers, request.body).await.map(|answer| Response {
                status: answer.status,
                headers: answer.headers.into_iter().map(|(name, value)| Header { name, value }).collect(),
                body: answer.body,
            });
            let _ = calls.send(Call::Response(id, response));
        });
        Ok(id)
    }

    fn connect(&mut self, url: String) -> Result<u32, String> {
        if self.sockets.len() >= MAX_SOCKETS {
            return Err(format!("it has {MAX_SOCKETS} sockets open already"));
        }
        let url = self.net.check(&url, ["ws", "wss"])?;
        let id = self.next_id();
        let (outgoing, to_send) = mpsc::unbounded_channel();
        self.sockets.insert(id, outgoing);
        let calls = self.calls.clone();
        let net = self.net.clone();
        self.deps
            .runtime
            .spawn(socket(net, url, to_send, move |event| calls.send(Call::Socket(id, event)).is_ok()));
        Ok(id)
    }

    fn send(&mut self, socket: u32, text: String) {
        self.send_on(socket, "send", Outgoing::Text(text));
    }

    fn send_binary(&mut self, socket: u32, data: Vec<u8>) {
        self.send_on(socket, "send-binary", Outgoing::Binary(data));
    }

    fn close(&mut self, socket: u32) {
        self.sockets.remove(&socket);
    }

    fn route_later(&mut self, run: u64, output: String, values: Vec<NamedValue>) {
        let result = match self.later.node(run) {
            Some(node) => self.route_on(node, run, &output, values),
            None => Err(format!(
                "no run {run} waits for an answer (an hour old, from before a restart, or not its own)"
            )),
        };
        if let Err(why) = result {
            tracing::warn!(plugin = self.id, "route-later: {why}");
        }
    }

    fn compute_later(&mut self, run: u64, values: Vec<NamedValue>) {
        match self.later_values.take(run) {
            Some(node) => self.computed(&node, run, values),
            None => tracing::warn!(plugin = self.id, run, "compute-later: no value node waits for that run"),
        }
    }

    fn emit_to(&mut self, placed: String, output: String, values: Vec<NamedEventValue>) {
        let Some(node) = self.placed.get(&placed).cloned() else {
            tracing::warn!(
                plugin = self.id,
                placed,
                "emit-to: none of its event nodes on a card has that id"
            );
            return;
        };
        if !self.events.iter().any(|ty| ty.id == node && ty.fires(&output)) {
            tracing::warn!(
                plugin = self.id,
                node,
                output,
                "emit-to: the node has no event output by that name"
            );
            return;
        }
        self.fire(node, (output != "event").then_some(output), values, Some(placed));
    }

    fn settings(&mut self) -> Vec<NamedValue> {
        let config = self.deps.config.borrow().clone();
        let media = &self.deps.media;
        let stored = config.plugin(&self.id);
        let mut settings =
            shared_settings(&self.settings, stored.shared_settings(), &mut |kind, folder, every| {
                media.pick_files_any(kind, folder, every)
            });
        settings.extend(self.deps.files.values(&self.id, stored.shared_settings()));
        settings.extend(self.deps.secrets.values(&self.id));
        settings
            .iter()
            .map(|(name, value)| NamedValue { name: name.clone(), value: value_of(value) })
            .collect()
    }

    fn set_status(&mut self, level: StatusLevel, text: String) {
        self.plugins.set_status(&self.id, protocol::plugin::PluginStatus::new(status_level(level), &text));
    }

    fn send_to_tab(&mut self, json: String) {
        match serde_json::from_str::<Json>(&json) {
            Ok(data) => self.deps.tabs.to_panels(&self.id, data),
            Err(error) => tracing::warn!(plugin = self.id, "send-to-tab: not JSON ({error})"),
        }
    }

    fn set_tab_count(&mut self, count: u32) {
        if self.tab_counts {
            self.plugins.set_tab_count(&self.id, count);
        } else {
            tracing::warn!(plugin = self.id, "set-tab-count: its manifest's tab has no \"count\": true");
        }
    }

    fn set_note(&mut self, setting: String, level: StatusLevel, lines: Vec<String>) {
        self.plugins.set_note(
            &self.id,
            &setting,
            protocol::plugin::PluginNote::new(status_level(level), &lines),
        );
    }

    fn carry_on(&mut self, run: u64, output: String) {
        let result = if self.outputs.contains(&output) {
            self.deps.activity.carry_on(&self.id, run, &output)
        } else {
            Err(format!("none of its effect nodes has an output {output:?}"))
        };
        if let Err(why) = result {
            tracing::warn!(plugin = self.id, "carry-on: {why}");
        }
    }

    fn done(&mut self, queue_id: u64) {
        if let Err(why) = self.deps.activity.done_by_server(&self.id, queue_id) {
            tracing::warn!(plugin = self.id, "done: {why}");
        }
    }

    fn app_port(&mut self) -> u16 {
        self.deps.config.borrow().connections.overlay_port
    }

    fn set_options(&mut self, name: String, options: Vec<String>) {
        self.deps.options.set(
            &self.id,
            &name,
            options.into_iter().map(protocol::plugin::Choice::plain).collect(),
        );
    }

    fn set_choices(&mut self, name: String, choices: Vec<lsk::plugin::types::Choice>) {
        let choices = choices
            .into_iter()
            .map(|choice| protocol::plugin::Choice { value: choice.value, label: choice.label })
            .collect();
        self.deps.options.set(&self.id, &name, choices);
    }

    fn spoken(&mut self, request: u32, audio: Result<Audio, String>) {
        let Some(answer) = self.speaking.remove(&request) else {
            tracing::warn!(
                plugin = self.id,
                request,
                "spoken: it wasn't asked for that message, or answered already"
            );
            return;
        };
        let audio = audio.and_then(|Audio { data, format }| {
            if data.len() > MAX_AUDIO {
                return Err(format!("the audio is bigger than {} MB", MAX_AUDIO / 1024 / 1024));
            }
            Ok(speech::Audio { data, format })
        });
        let _ = answer.send(audio);
    }

    fn get(&mut self, key: String) -> Option<String> {
        self.stored.get(&key).cloned()
    }

    fn moderate(&mut self, platform: String, what: Moderation) -> Result<u32, String> {
        let allowed = self.deps.config.borrow().plugin(&self.id).moderates;
        if !protocol::plugin::moderates_now(&self.info, allowed) {
            return Err(if self.info.manifest.moderates {
                "It may not moderate yet: press Allow on its Plugins box.".into()
            } else {
                "Its plugin.json doesn't say it moderates (\"moderates\": true).".into()
            });
        }
        let (user, action) = viewer_action(&platform, what)?;
        let moderation = self.deps.moderation.clone().ok_or(LOGGED_OUT)?;
        moderation.ready()?;
        if self.moderating >= MAX_MODERATING {
            return Err(format!("{MAX_MODERATING} moderations are waiting for Twitch already."));
        }
        self.moderating += 1;
        let (id, calls, plugin) = (self.next_id(), self.calls.clone(), self.id.clone());
        self.deps.runtime.spawn(async move {
            let done = moderation.act(&user, &action).await.map(drop);
            match &done {
                Ok(()) => tracing::info!(plugin, viewer = user, ?action, "moderated a viewer"),
                Err(why) => {
                    tracing::info!(plugin, viewer = user, ?action, "couldn't moderate a viewer: {why}")
                }
            }
            let _ = calls.send(Call::Moderated(id, done));
        });
        Ok(id)
    }

    fn set(&mut self, key: String, value: Option<String>) {
        let changed = match value {
            Some(value) => {
                let size: usize = self
                    .stored
                    .iter()
                    .filter(|(known, _)| **known != key)
                    .map(|(k, v)| k.len() + v.len())
                    .sum();
                if size + key.len() + value.len() > MAX_STORED {
                    tracing::warn!(
                        plugin = self.id,
                        key,
                        "set: a plugin can store {} KB at most",
                        MAX_STORED / 1024
                    );
                    return;
                }
                self.stored.insert(key, value.clone()).as_ref() != Some(&value)
            }
            None => self.stored.remove(&key).is_some(),
        };
        if changed {
            self.save_stored();
        }
    }
}

/// A message a plugin sends on a WebSocket.
#[derive(Debug)]
enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
}

impl Outgoing {
    /// Its size in bytes.
    fn len(&self) -> usize {
        match self {
            Outgoing::Text(text) => text.len(),
            Outgoing::Binary(data) => data.len(),
        }
    }
}

/// A plugin's WebSocket to `url`, for as long as it's open: sends what
/// comes from `to_send` (its end dropped: the plugin closed it), and tells
/// `heard` what happens, until it says to stop. A message coming in bigger
/// than [`MAX_BODY`] closes it, saying so (one going out that big is never
/// sent: [`State::send_on`]).
async fn socket(
    net: Net,
    url: reqwest::Url,
    mut to_send: mpsc::UnboundedReceiver<Outgoing>,
    heard: impl Fn(SocketEvent) -> bool,
) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
    let config = WebSocketConfig::default().max_message_size(Some(MAX_BODY)).max_frame_size(Some(MAX_BODY));
    let mut socket = match net.connect(&url, config).await {
        Ok(socket) => socket,
        Err(error) => {
            heard(SocketEvent::Closed(error));
            return;
        }
    };
    if !heard(SocketEvent::Opened) {
        return;
    }
    let reason = loop {
        tokio::select! {
            outgoing = to_send.recv() => {
                let message = match outgoing {
                    Some(Outgoing::Text(text)) => Message::text(text),
                    Some(Outgoing::Binary(data)) => Message::binary(data),
                    // The plugin closed it.
                    None => {
                        let _ = socket.close(None).await;
                        return;
                    }
                };
                if let Err(error) = socket.send(message).await {
                    break error.to_string();
                }
            }
            message = socket.next() => {
                let event = match message {
                    Some(Ok(Message::Text(text))) => SocketEvent::Message(text.to_string()),
                    Some(Ok(Message::Binary(data))) => SocketEvent::Binary(data.to_vec()),
                    Some(Ok(Message::Close(frame))) => {
                        break frame.map(|frame| frame.reason.to_string()).unwrap_or_else(|| "closed".into());
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => break error.to_string(),
                    None => break "closed".into(),
                };
                if !heard(event) {
                    return;
                }
            }
        }
    };
    // (A message too big leaves it open on the other end.)
    let _ = socket.close(None).await;
    heard(SocketEvent::Closed(reason));
}

const LOGGED_OUT: &str = "Log in with Twitch on Status to moderate.";

/// What `host.moderate` asks, as the viewers' cards ask it: the viewer's id
/// (none to delete a message) and what to do. Only Twitch's chat, so far.
fn viewer_action(
    platform: &str,
    what: Moderation,
) -> Result<(String, protocol::viewers::ViewerAction), String> {
    use protocol::viewers::{MAX_TIMEOUT_SECS, ViewerAction};
    if platform != "twitch" {
        return Err(format!("Only Twitch's chat can be moderated, not {platform:?}'s."));
    }
    let someone = |viewer: String| {
        if viewer.trim().is_empty() { Err("No viewer: their id is empty.".to_string()) } else { Ok(viewer) }
    };
    Ok(match what {
        Moderation::Timeout(Timeout { viewer, seconds }) => {
            (someone(viewer)?, ViewerAction::Timeout { secs: seconds.clamp(1, MAX_TIMEOUT_SECS) })
        }
        Moderation::Ban(viewer) => (someone(viewer)?, ViewerAction::Ban),
        Moderation::Delete(line) => {
            let message = line
                .strip_prefix("twitch:")
                .filter(|id| !id.is_empty())
                .ok_or("Only a message from Twitch's chat can be deleted (a chat line's id: twitch:…).")?;
            (String::new(), ViewerAction::Delete { message: message.to_string() })
        }
    })
}

/// A status's (or note's) colour, as the panel has it.
fn status_level(level: StatusLevel) -> protocol::plugin::StatusLevel {
    match level {
        StatusLevel::Ok => protocol::plugin::StatusLevel::Ok,
        StatusLevel::Waiting => protocol::plugin::StatusLevel::Waiting,
        StatusLevel::Problem => protocol::plugin::StatusLevel::Problem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_as_the_plugin_gets_them() {
        assert!(matches!(value_of(&serde_json::json!(3)), Value::Number(n) if n == 3.0));
        assert!(matches!(value_of(&serde_json::json!(true)), Value::Toggle(true)));
        assert!(matches!(value_of(&serde_json::json!("#ff0000")), Value::Plain(text) if text == "#ff0000"));
        assert!(matches!(value_of(&Json::Null), Value::Empty));
        let text = serde_json::json!({ "text": "hi Kappa", "parts": [
            { "type": "text", "text": "hi " },
            { "type": "emote", "code": "Kappa", "url": "/emote/twitch/25", "overlays": [] }
        ] });
        let Value::Text(text) = value_of(&text) else { panic!("a text") };
        assert_eq!(text.plain, "hi Kappa");
        assert!(matches!(&text.parts[1], WitPart::Emote(emote) if emote.code == "Kappa"));
        let emotes =
            serde_json::json!([{ "code": "PogU", "url": "/emote/7tv/1", "overlays": ["/emote/7tv/2"] }]);
        let Value::Emotes(emotes) = value_of(&emotes) else { panic!("emotes") };
        assert_eq!(emotes[0].overlays.len(), 1);
    }

    #[test]
    fn placed_event_nodes_with_their_settings() {
        use protocol::cards::{Card, Node, NodeKind};

        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "copy", "name": "Copy", "version": "0.1.0", "api": 1,
            "nodes": [{ "id": "copied", "title": "Copied", "category": "events",
                "outputs": [
                    { "name": "event", "label": "Event", "type": "event" },
                    { "name": "message", "label": "Message", "type": "text" }
                ],
                "settings": [
                    { "key": "message", "label": "Message", "type": "text" },
                    { "key": "viewers", "label": "Viewers", "type": "number", "default": 3 }
                ] }]
        }))
        .unwrap();
        let info = PluginInfo { manifest, server: true, ..Default::default() };
        let node = |id: u32, plugin: &str, settings: serde_json::Value| Node {
            id,
            x: 0.0,
            y: 0.0,
            kind: NodeKind::Plugin {
                plugin: plugin.into(),
                node: "copied".into(),
                settings: settings.as_object().unwrap().clone(),
                changes: Default::default(),
                voice: Default::default(),
                ty: Default::default(),
            },
        };
        let card = |id: &str, enabled: bool, nodes: Vec<Node>| Card {
            id: id.into(),
            name: id.to_uppercase(),
            enabled,
            nodes,
            wires: Vec::new(),
        };
        let mut config = AppConfig {
            cards: vec![
                card("a", true, vec![node(1, "copy", serde_json::json!({ "viewers": 5, "message": "hi" }))]),
                card("b", false, vec![node(2, "copy", serde_json::json!({}))]),
                card("other", true, vec![node(1, "someone-else", serde_json::json!({}))]),
            ],
            ..Default::default()
        };
        let placed = placed_nodes(&info.manifest, &config);
        assert_eq!(placed.len(), 2, "not another plugin's node");
        assert_eq!((placed[0].id.as_str(), placed[0].card.as_str(), placed[0].on), ("a/1", "A", true));
        assert_eq!(
            placed[0].settings,
            *serde_json::json!({ "viewers": 5.0 }).as_object().unwrap(),
            "no filter"
        );
        assert_eq!((placed[1].id.as_str(), placed[1].on), ("b/2", false));
        assert_eq!(placed[1].settings["viewers"], 3.0, "every one with a value");

        // Editing another plugin's card changes nothing for this one.
        config.cards[2].nodes.push(node(2, "someone-else", serde_json::json!({})));
        assert_eq!(placed_nodes(&info.manifest, &config), placed);
        config.cards[1].enabled = true;
        assert_ne!(placed_nodes(&info.manifest, &config), placed, "its card switched on");
    }

    /// A WebSocket server on a free local port that sends `first`, then
    /// echoes what it gets; its address.
    async fn echo_server(first: Option<Vec<u8>>) -> reqwest::Url {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(None)
                .max_frame_size(None);
            let mut socket = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await.unwrap();
            if let Some(first) = first {
                let _ = socket.send(Message::binary(first)).await;
            }
            while let Some(Ok(message)) = socket.next().await {
                if message.is_binary() || message.is_text() {
                    let _ = socket.send(message).await;
                }
            }
        });
        reqwest::Url::parse(&format!("ws://{address}")).unwrap()
    }

    /// What a plugin's socket to `url` hears, as it happens.
    fn open(url: reqwest::Url) -> (mpsc::UnboundedSender<Outgoing>, mpsc::UnboundedReceiver<SocketEvent>) {
        let (outgoing, to_send) = mpsc::unbounded_channel();
        let (events, heard) = mpsc::unbounded_channel();
        // (Port 0 for the app's: none is refused.)
        let net = Net::new(vec!["127.0.0.1".into()], || [0, 0], FETCH_TIMEOUT).unwrap();
        tokio::spawn(socket(net, url, to_send, move |event| events.send(event).is_ok()));
        (outgoing, heard)
    }

    #[tokio::test]
    async fn binary_messages_go_both_ways() {
        let (outgoing, mut heard) = open(echo_server(None).await);
        assert!(matches!(heard.recv().await, Some(SocketEvent::Opened)));
        outgoing.send(Outgoing::Binary(vec![1, 2, 3])).unwrap();
        outgoing.send(Outgoing::Text("hi".into())).unwrap();
        assert!(matches!(heard.recv().await, Some(SocketEvent::Binary(data)) if data == [1, 2, 3]));
        assert!(matches!(heard.recv().await, Some(SocketEvent::Message(text)) if text == "hi"));
    }

    #[tokio::test]
    async fn a_message_too_big_closes_the_socket_saying_so() {
        let (_outgoing, mut heard) = open(echo_server(Some(vec![0; MAX_BODY + 1])).await);
        assert!(matches!(heard.recv().await, Some(SocketEvent::Opened)));
        let Some(SocketEvent::Closed(why)) = heard.recv().await else { panic!("closed") };
        assert!(why.contains("too long"), "{why}");
    }

    #[test]
    fn moderating_asks_as_the_viewers_cards_do() {
        use protocol::viewers::ViewerAction;
        let timeout = |seconds| Moderation::Timeout(Timeout { viewer: "1001".into(), seconds });
        assert_eq!(
            viewer_action("twitch", timeout(10)),
            Ok(("1001".to_string(), ViewerAction::Timeout { secs: 10 }))
        );
        assert_eq!(viewer_action("twitch", timeout(0)).unwrap().1, ViewerAction::Timeout { secs: 1 });
        assert_eq!(
            viewer_action("twitch", timeout(u32::MAX)).unwrap().1,
            ViewerAction::Timeout { secs: protocol::viewers::MAX_TIMEOUT_SECS }
        );
        assert_eq!(viewer_action("twitch", Moderation::Ban("1001".into())).unwrap().1, ViewerAction::Ban);
        assert!(viewer_action("twitch", Moderation::Ban(" ".into())).is_err(), "nobody");
        assert_eq!(
            viewer_action("twitch", Moderation::Delete("twitch:abc-123".into())),
            Ok((String::new(), ViewerAction::Delete { message: "abc-123".into() }))
        );
        assert!(viewer_action("twitch", Moderation::Delete("local:4".into())).is_err(), "not Twitch's line");
        assert!(viewer_action("youtube", Moderation::Ban("1001".into())).is_err(), "only Twitch so far");
    }

    #[test]
    fn a_flow_or_value_nodes_numbers_are_finite() {
        let ty: NodeType = serde_json::from_value(serde_json::json!({
            "id": "money", "title": "Money", "category": "values",
            "outputs": [
                { "name": "money", "label": "Money", "type": "number" },
                { "name": "text", "label": "As text", "type": "text" }
            ]
        }))
        .unwrap();
        let gave = |name: &str, value| outputs_of(&ty, vec![NamedValue { name: name.into(), value }]);
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(gave("money", Value::Number(number)).is_empty(), "{number}: nothing");
        }
        assert!(gave("money", Value::Plain("inf".into())).is_empty(), "nor a text that says so");
        assert_eq!(
            gave("money", Value::Number(2.5)),
            [("money".to_string(), crate::cards::Value::Number(2.5))]
        );
        assert_eq!(gave("text", Value::Number(f64::NAN)).len(), 1, "as text it's a text");
    }

    #[test]
    fn a_stopped_copy_lets_go_of_what_it_was_asked() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let activity = Activity::for_tests(OverlayHub::new(), crate::services::speech::Speech::off());
        let mut resumes = activity.resumes();
        let (asks, asked) = activity.asks("money");
        let (calls, mut inbox) = mpsc::unbounded_channel();
        let asking = forward_asks(runtime.handle(), asked, calls);
        let raid = crate::model::Signal::new(
            crate::pipeline::testing::origin(),
            crate::model::SignalKind::Raid { from: crate::pipeline::testing::user("r"), viewers: 3 },
        );
        let mut then = crate::cards::Then::for_tests("card", 2, raid);
        then.into = vec![(3, "trigger".into())];
        let ask = crate::services::activity::ValueAsk {
            plugin: "money".into(),
            node: 4,
            ty: "money".into(),
            values: Map::new(),
        };
        let cause = crate::services::activity::Cause::default();
        assert!(activity.compute(then, vec![ask], &cause));

        let_go(&activity, runtime.handle(), "money", asks, asking, &mut inbox);
        let resume = resumes.try_recv().expect("the value node gave nothing, at once");
        assert_eq!(resume.then.into, [(3, "trigger".to_string())]);
        assert!(inbox.try_recv().is_err(), "nothing more comes to it");
    }
}
