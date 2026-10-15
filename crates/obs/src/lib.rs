//! The built-in OBS plugin's server code: a WebAssembly component for the
//! plugin interface (`crates/protocol/wit/plugin.wit`), built into
//! `crates/server/plugins/obs/server.wasm`. It talks to obs-websocket 5,
//! which is built into OBS 28 and later.
//!
//! It keeps a connection open and logged in: it connects when the app
//! starts, and again a while after OBS closes or isn't running. Once logged
//! in it fills in the panel's choices (the scenes, the inputs, the text
//! sources, each scene's sources and each source's filters) and fills them
//! in again when they change in OBS; it keeps which sources are shown and
//! which filters are on, for Toggle. Its effect nodes switch scenes, show
//! and hide sources, mute, set texts and switch filters; its event nodes
//! fire when the scene changes and when the stream or the recording starts
//! or ends. When OBS was already running as the app started, it refreshes
//! the Browser Sources showing the app (they may have loaded before the
//! app was there). It watches those Browser Sources for being scaled in
//! their scenes (blurry) and unscales them, or warns (`scaling`).
//! Everything OBS sends is logged at debug level.

// wit-bindgen's glue for `run` takes more arguments than clippy likes.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({ path: "../protocol/wit", world: "plugin" });

mod scaling;
mod websocket;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use scaling::{BoundsAndCrop, Shown, Transform, size};
use serde_json::json;
use websocket::{Filter, Incoming, ObsEvent, Output, SceneItem};

use lsk::plugin::host;
use lsk::plugin::types::{EventValue, Level, NamedEventValue, StatusLevel, Value};

/// Where obs-websocket listens unless the address setting says otherwise.
const DEFAULT_ADDRESS: &str = "localhost:4455";

/// The first wait before connecting again, doubled each time it fails, up
/// to [`RETRY_MAX_MS`].
const RETRY_FIRST_MS: u32 = 2_000;
const RETRY_MAX_MS: u32 = 30_000;

/// How long after a change in OBS's scenes or sources the choices are
/// asked for again (changes come in bursts: a scene and its sources).
const REFRESH_AFTER_MS: u32 = 300;

/// The longest a change is held before it's put back ("For").
const LONGEST_SECONDS: f64 = 3600.0;

/// OBS's events that change the choices: scenes, inputs, scenes' sources
/// and sources' filters added, removed, renamed or reordered; and another
/// profile, which may have another canvas.
const REFRESHED_BY: [&str; 15] = [
    "CurrentProfileChanged",
    "SceneCreated",
    "SceneRemoved",
    "SceneNameChanged",
    "SceneListChanged",
    "InputCreated",
    "InputRemoved",
    "InputNameChanged",
    "SceneItemCreated",
    "SceneItemRemoved",
    "SceneItemListReindexed",
    "SourceFilterCreated",
    "SourceFilterRemoved",
    "SourceFilterNameChanged",
    "SourceFilterListReindexed",
];

/// The requests whose answers fill in the choices: their ids, the names
/// of their lists too (a scene's sources' is `sources/<scene>`, a source's
/// filters' `filters/<source>`).
const SCENES: &str = "scenes";
const INPUTS: &str = "inputs";
const SOURCES: &str = "sources/";
const FILTERS: &str = "filters/";
/// The other lists: the text sources, and the sources with filters.
const TEXTS: &str = "texts";
const FILTERED: &str = "filtered";
/// Requests for a Browser Source's settings, to see if it shows the app.
const BROWSER: &str = "browser/";
/// OBS's canvas: the request, and the list the panel reads it from
/// ("2560x1440", the size it gives overlays showing every effect; none
/// while OBS isn't connected).
const CANVAS: &str = "canvas";

/// The Scaled overlays setting (Warn, the default, or Resize), and the key its note
/// goes under.
const SCALED: &str = "scaled";

/// How long after the last change to a source's size (OBS tells each step
/// of a drag) it's checked for being scaled.
const SETTLE_MS: u32 = 1_000;

#[derive(Default)]
struct State {
    /// The open (or opening) connection.
    socket: Option<u32>,
    /// Logged in on it.
    ready: bool,
    /// The timer that connects again.
    retry: Option<u32>,
    /// The next wait before connecting again.
    retry_ms: u32,
    /// The timer that asks for the choices again.
    refresh: Option<u32>,
    /// The scenes, top first.
    scenes: Vec<String>,
    /// The inputs, as OBS lists them.
    inputs: Vec<websocket::Input>,
    /// Each scene's sources, top first.
    items: HashMap<String, Vec<SceneItem>>,
    /// Each source's filters, top first (only sources with some).
    filters: HashMap<String, Vec<Filter>>,
    /// For request ids.
    requests: u64,
    /// The scene on air, as far as it knows.
    on_air: Option<String>,
    /// Switch scene runs whose Started or Ended is wired, oldest first.
    switches: Vec<Switch>,
    /// Changes to put back after a while ("For").
    held: Vec<Held>,
    /// OBS was running when the app started: refresh the Browser Sources
    /// showing the app once logged in.
    refresh_pages: bool,
    /// The address and password it last connected with.
    login: (String, String),
    /// The Browser Sources to refresh, once their settings are in.
    to_refresh: HashSet<String>,
    /// The Browser Sources showing the app.
    app_pages: HashSet<String>,
    /// The lists asked for and not answered yet (scenes' sources, Browser
    /// Sources' settings): the scaled ones are looked for once they're in.
    awaiting: HashSet<String>,
    /// Sources showing the app resized by hand (scene, item id), checked
    /// once the resizing stops.
    resized: Vec<(String, i64)>,
    /// The timer that checks them.
    settle: Option<u32>,
    /// Scaled sources warned about since connecting, warned about again
    /// only when resized by hand.
    warned: HashSet<(String, i64)>,
}

/// A Switch scene run whose card carries on (`host.carry-on`): Started
/// once OBS takes the request, Ended once the scene is on air.
struct Switch {
    request: String,
    run: u64,
    scene: String,
    started: bool,
}

/// The most switches it waits for (older ones are forgotten: OBS went
/// elsewhere before they came on air).
const SWITCHES_KEPT: usize = 20;
const STARTED: &str = "started";
const ENDED: &str = "ended";

/// What can be switched on and off and put back: a source in a scene, or
/// a filter on a source.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Source { scene: String, source: String, id: i64 },
    Filter { source: String, filter: String },
}

/// A change put back when its timer fires, or when its queued run is
/// skipped.
struct Held {
    timer: u32,
    target: Target,
    /// What it was before.
    back: bool,
    /// Its queued run, said done once it's put back.
    queue_id: Option<u64>,
}

/// What a node does to its target: show, hide or toggle (mute, unmute or
/// toggle; on, off or toggle).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    On,
    Off,
    Toggle,
}

impl Action {
    fn of(value: &str, on: &str, off: &str) -> Action {
        match value {
            value if value == on => Action::On,
            value if value == off => Action::Off,
            _ => Action::Toggle,
        }
    }

    /// What it sets, from what was.
    fn apply(self, was: bool) -> bool {
        match self {
            Action::On => true,
            Action::Off => false,
            Action::Toggle => !was,
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { retry_ms: RETRY_FIRST_MS, ..Default::default() });
}

struct Obs;

impl Guest for Obs {
    fn start() {
        host::set_status(StatusLevel::Waiting, "Connecting");
        STATE.with_borrow_mut(|state| state.refresh_pages = true);
        connect();
    }

    fn run(node: String, _input: String, values: Vec<NamedValue>, run: Option<u64>, queue_id: Option<u64>) {
        let text = |name: &str| setting(&values, name).trim().to_string();
        match node.as_str() {
            "switch_scene" => switch_scene(&text("scene"), run),
            "show_source" => {
                let action = Action::of(&text("action"), "show", "hide");
                let target = scene_item(&text("scene"), &text("source"));
                change(target, action, seconds(&values), queue_id);
            }
            "filter" => {
                let action = Action::of(&text("action"), "on", "off");
                change(filter(&text("source"), &text("filter")), action, seconds(&values), queue_id);
            }
            "mute" => mute(&text("input"), Action::of(&text("action"), "mute", "unmute")),
            "set_text" => set_text(&text("source"), &setting(&values, "text")),
            other => host::log(Level::Warn, &format!("no such node: {other}")),
        }
    }

    /// A new address or password: it connects again with them, at once.
    /// Otherwise Scaled overlays may have been switched (Warn, Resize):
    /// what's scaled is fixed now, or warned about.
    fn settings_changed() {
        let login = login(&host::settings());
        if STATE.with_borrow(|state| state.login != login) {
            host::log(Level::Info, "the address or password changed: connecting to OBS again");
            if let Some(socket) = STATE.with_borrow(|state| state.socket) {
                host::close(socket);
            }
            disconnected(true);
            let retry = STATE.with_borrow_mut(|state| {
                state.retry_ms = RETRY_FIRST_MS;
                state.retry.take()
            });
            if let Some(timer) = retry {
                host::cancel_timer(timer);
            }
            host::set_status(StatusLevel::Waiting, "Connecting");
            connect();
        } else if ready_socket().is_some() {
            look_for_scaled();
        }
    }

    /// A queued Show / hide or Filter run was skipped: put back what it
    /// changed now.
    fn skip(queue_id: u64) {
        let held = STATE.with_borrow_mut(|state| {
            let at = state.held.iter().position(|held| held.queue_id == Some(queue_id))?;
            Some(state.held.remove(at))
        });
        if let Some(held) = held {
            host::cancel_timer(held.timer);
            set(&held.target, held.back);
        }
    }

    /// It has no flow nodes, so it's never asked.
    fn route(
        _node: String,
        _input: String,
        _values: Vec<NamedValue>,
        _viewer: Option<Viewer>,
        _run: u64,
        _origin: RunOrigin,
    ) -> Routed {
        Routed::Stop
    }

    /// It has no value nodes, so it's never asked.
    fn compute(_node: String, _values: Vec<NamedValue>, _run: u64, _origin: RunOrigin) -> Computed {
        Computed::Now(Vec::new())
    }

    /// It doesn't hear the chat.
    fn on_chat(_heard: HeardChat) {}

    /// It doesn't hear events.
    fn on_event(_heard: HeardEvent) {}

    /// Its event nodes have no settings for it.
    fn nodes_changed(_nodes: Vec<PlacedNode>) {}

    fn on_timer(id: u32) {
        let (retry, refresh, held, resized) = STATE.with_borrow_mut(|state| {
            let retry = state.retry == Some(id);
            let refresh = state.refresh == Some(id);
            if retry {
                state.retry = None;
            }
            if refresh {
                state.refresh = None;
            }
            let held = state.held.iter().position(|held| held.timer == id).map(|at| state.held.remove(at));
            let resized = if state.settle == Some(id) {
                state.settle = None;
                std::mem::take(&mut state.resized)
            } else {
                Vec::new()
            };
            (retry, refresh, held, resized)
        });
        for (scene, id) in resized {
            resized_by_hand(scene, id);
        }
        if retry {
            connect();
        }
        if refresh && let Some(socket) = ready_socket() {
            ask_for_choices(socket);
        }
        if let Some(held) = held {
            set(&held.target, held.back);
            if let Some(queue_id) = held.queue_id {
                host::done(queue_id);
            }
        }
    }

    fn on_response(_id: u32, _response: Result<Response, String>) {}

    fn on_socket(id: u32, event: SocketEvent) {
        if STATE.with_borrow(|state| state.socket) != Some(id) {
            return;
        }
        match event {
            SocketEvent::Opened => host::log(Level::Debug, "connected to obs-websocket"),
            SocketEvent::Message(text) => {
                host::log(Level::Debug, &format!("obs-websocket: {text}"));
                match websocket::parse(&text) {
                    Ok(message) => heard(id, message),
                    Err(why) => {
                        host::log(Level::Warn, &format!("an obs-websocket message it can't read: {why}"))
                    }
                }
            }
            // obs-websocket speaks JSON, in text messages.
            SocketEvent::Binary(_) => {}
            SocketEvent::Closed(why) => {
                // obs-websocket closes with "Authentication failed." for a
                // wrong password (or none).
                let refused = why.contains("Authentication");
                if refused {
                    host::log(Level::Warn, &format!("not connected to obs-websocket: {why}"));
                    let none = setting(&host::settings(), "password").is_empty();
                    host::set_status(
                        StatusLevel::Problem,
                        if none { "Needs a password" } else { "Wrong password" },
                    );
                } else {
                    host::log(Level::Debug, &format!("not connected to obs-websocket: {why}"));
                    host::set_status(StatusLevel::Waiting, "OBS not running");
                }
                disconnected(refused);
                retry_later();
            }
        }
    }

    /// It has no voices, so it's never asked.
    fn speak(_request: u32, _voice: String, _text: String, _speed: f32) {}
}

/// Switch scene: asks OBS to put `scene` on air; with `run`, its card
/// carries on from Started and Ended.
fn switch_scene(scene: &str, run: Option<u64>) {
    if scene.is_empty() {
        host::log(Level::Warn, "Switch scene: no scene picked");
        return;
    }
    let request = send_request("SetCurrentProgramScene", json!({ "sceneName": scene }), "switch to", scene);
    if let (Some(request), Some(run)) = (request, run) {
        STATE.with_borrow_mut(|state| {
            if state.switches.len() >= SWITCHES_KEPT {
                state.switches.remove(0);
            }
            let scene = scene.to_string();
            state.switches.push(Switch { request, run, scene, started: false });
        });
    }
}

/// A source in a scene, as OBS last listed it: `Err` says why not.
fn scene_item(scene: &str, source: &str) -> Result<Target, String> {
    if scene.is_empty() || source.is_empty() {
        return Err("Show / hide a source: no scene or source picked".into());
    }
    STATE.with_borrow(|state| {
        let items = state.items.get(scene).ok_or_else(|| format!("OBS has no scene {scene:?}"))?;
        let item = items
            .iter()
            .find(|item| item.source == source)
            .ok_or_else(|| format!("OBS's scene {scene:?} has no source {source:?}"))?;
        Ok(Target::Source { scene: scene.into(), source: source.into(), id: item.id })
    })
}

/// A filter on a source, as OBS last listed it: `Err` says why not.
fn filter(source: &str, filter: &str) -> Result<Target, String> {
    if source.is_empty() || filter.is_empty() {
        return Err("Filter on / off: no source or filter picked".into());
    }
    STATE.with_borrow(|state| {
        let filters = state.filters.get(source).ok_or_else(|| format!("OBS's {source:?} has no filters"))?;
        filters
            .iter()
            .find(|known| known.name == filter)
            .ok_or_else(|| format!("OBS's {source:?} has no filter {filter:?}"))?;
        Ok(Target::Filter { source: source.into(), filter: filter.into() })
    })
}

/// Whether a target is on (a source shown, a filter on), as OBS last said.
fn is_on(target: &Target) -> bool {
    STATE.with_borrow(|state| match target {
        Target::Source { scene, id, .. } => state
            .items
            .get(scene)
            .and_then(|items| items.iter().find(|item| item.id == *id))
            .is_some_and(|item| item.enabled),
        Target::Filter { source, filter } => state
            .filters
            .get(source)
            .and_then(|filters| filters.iter().find(|known| known.name == *filter))
            .is_some_and(|known| known.enabled),
    })
}

/// Keeps what a target is now (before OBS says so too).
fn remember(target: &Target, on: bool) {
    STATE.with_borrow_mut(|state| match target {
        Target::Source { scene, id, .. } => {
            if let Some(item) =
                state.items.get_mut(scene).and_then(|items| items.iter_mut().find(|item| item.id == *id))
            {
                item.enabled = on;
            }
        }
        Target::Filter { source, filter } => {
            if let Some(known) = state
                .filters
                .get_mut(source)
                .and_then(|filters| filters.iter_mut().find(|known| known.name == *filter))
            {
                known.enabled = on;
            }
        }
    });
}

/// Switches a target on or off in OBS.
fn set(target: &Target, on: bool) {
    match target {
        Target::Source { scene, source, id } => {
            let data = json!({ "sceneName": scene, "sceneItemId": id, "sceneItemEnabled": on });
            send_request("SetSceneItemEnabled", data, if on { "show" } else { "hide" }, source);
        }
        Target::Filter { source, filter } => {
            let data = json!({ "sourceName": source, "filterName": filter, "filterEnabled": on });
            let doing = if on { "switch on" } else { "switch off" };
            send_request("SetSourceFilterEnabled", data, doing, &format!("{source}'s {filter}"));
        }
    }
    remember(target, on);
}

/// Show / hide and Filter: applies `action` to `target`, and puts it back
/// as it was after `seconds` (0: it stays). A queued run (`queue_id`) is
/// done once it's put back, or at once if it stays or can't be done. A
/// change held already on the same target is taken over: it's put back as
/// it was before that one.
fn change(target: Result<Target, String>, action: Action, seconds: f64, queue_id: Option<u64>) {
    let done = || {
        if let Some(queue_id) = queue_id {
            host::done(queue_id);
        }
    };
    let target = match target {
        Ok(target) if ready_socket().is_some() => target,
        Ok(_) => {
            host::log(Level::Warn, "not connected to OBS, so it can't show, hide or switch anything");
            return done();
        }
        Err(why) => {
            host::log(Level::Warn, &why);
            return done();
        }
    };
    let earlier = STATE.with_borrow_mut(|state| {
        let at = state.held.iter().position(|held| held.target == target)?;
        Some(state.held.remove(at))
    });
    let was = is_on(&target);
    let back = match earlier {
        Some(earlier) => {
            host::cancel_timer(earlier.timer);
            if let Some(queue_id) = earlier.queue_id {
                host::done(queue_id);
            }
            earlier.back
        }
        None => was,
    };
    set(&target, action.apply(was));
    if seconds > 0.0 {
        let timer = host::set_timer((seconds * 1000.0) as u32, false);
        STATE.with_borrow_mut(|state| state.held.push(Held { timer, target, back, queue_id }));
    } else {
        done();
    }
}

/// Mute: mutes, unmutes or toggles an input's sound.
fn mute(input: &str, action: Action) {
    if input.is_empty() {
        return host::log(Level::Warn, "Mute: no input picked");
    }
    match action {
        Action::Toggle => {
            send_request("ToggleInputMute", json!({ "inputName": input }), "mute or unmute", input);
        }
        Action::On | Action::Off => {
            let muted = action == Action::On;
            let data = json!({ "inputName": input, "inputMuted": muted });
            send_request("SetInputMute", data, if muted { "mute" } else { "unmute" }, input);
        }
    }
}

/// Set a text: a text source's text.
fn set_text(source: &str, text: &str) {
    if source.is_empty() {
        return host::log(Level::Warn, "Set a text: no text source picked");
    }
    let data = json!({ "inputName": source, "inputSettings": { "text": text }, "overlay": true });
    send_request("SetInputSettings", data, "set the text of", source);
}

/// The "For" setting, in seconds (0 = it stays).
fn seconds(values: &[NamedValue]) -> f64 {
    let seconds = values.iter().find(|value| value.name == "seconds").and_then(|value| match value.value {
        Value::Number(seconds) => Some(seconds),
        _ => None,
    });
    seconds.filter(|seconds| seconds.is_finite()).unwrap_or(0.0).clamp(0.0, LONGEST_SECONDS)
}

/// A message from OBS on the connection `socket`.
fn heard(socket: u32, message: Incoming) {
    match message {
        Incoming::Hello(hello) => {
            host::log(
                Level::Info,
                &format!("OBS {}, obs-websocket {}", hello.obs_studio_version, hello.obs_web_socket_version),
            );
            host::send(socket, &websocket::identify(&hello, &setting(&host::settings(), "password")));
        }
        Incoming::Identified => {
            host::log(Level::Info, "logged in to obs-websocket");
            host::set_status(StatusLevel::Ok, "Connected");
            STATE.with_borrow_mut(|state| {
                state.ready = true;
                state.retry_ms = RETRY_FIRST_MS;
            });
            ask_for_choices(socket);
        }
        Incoming::Response(response) => answered(socket, response),
        Incoming::Event { kind, data } => {
            if REFRESHED_BY.contains(&kind.as_str()) {
                refresh_soon();
            }
            match websocket::obs_event(&kind, &data) {
                Some(ObsEvent::SceneChanged { scene }) => scene_changed(scene),
                Some(ObsEvent::Output { output, started, file }) => {
                    let (what, node) = match (output, started) {
                        (Output::Record, true) => ("recording started", "recording_started"),
                        (Output::Record, false) => ("recording ended", "recording_ended"),
                        (Output::Stream, true) => ("stream started", "stream_started"),
                        (Output::Stream, false) => ("stream ended", "stream_ended"),
                    };
                    host::log(Level::Info, &format!("OBS: the {what}"));
                    let values: Vec<NamedEventValue> =
                        file.map(|file| text_value("file", &file)).into_iter().collect();
                    host::emit(node, &values);
                }
                Some(ObsEvent::SceneItemShown { scene, id, shown }) => {
                    remember(&Target::Source { scene, source: String::new(), id }, shown);
                }
                Some(ObsEvent::FilterSwitched { source, filter, on }) => {
                    remember(&Target::Filter { source, filter }, on);
                }
                Some(ObsEvent::Transformed { scene, id, transform }) => transformed(scene, id, transform),
                Some(ObsEvent::InputSettings { input, url }) => {
                    let browser = STATE.with_borrow(|state| {
                        state.inputs.iter().any(|known| known.name == input && known.kind == "browser_source")
                    });
                    if browser {
                        if page_is(&input, url.as_deref()) {
                            look_for_scaled();
                        } else {
                            show_scaled();
                        }
                    }
                }
                None => {}
            }
        }
        Incoming::Other(_) => {}
    }
}

/// Another scene is on air: Switch scene runs waiting for it end, and the
/// Scene changed node fires.
fn scene_changed(scene: String) {
    host::log(Level::Info, &format!("OBS: {scene} is on air"));
    let ended: Vec<u64> = STATE.with_borrow_mut(|state| {
        state.on_air = Some(scene.clone());
        let (ended, waiting) = std::mem::take(&mut state.switches)
            .into_iter()
            .partition(|switch| switch.started && switch.scene == scene);
        state.switches = waiting;
        ended.into_iter().map(|switch: Switch| switch.run).collect()
    });
    for run in ended {
        host::carry_on(run, ENDED);
    }
    host::emit("scene_changed", &[text_value("scene", &scene)]);
}

fn text_value(name: &str, text: &str) -> NamedEventValue {
    NamedEventValue { name: name.into(), value: EventValue::Text(text.into()) }
}

/// An answer from OBS; once the last list awaited is in, the sources
/// showing the app are checked for being scaled.
fn answered(socket: u32, response: websocket::Response) {
    let awaited = STATE.with_borrow_mut(|state| state.awaiting.remove(&response.request_id));
    read_answer(socket, response);
    if awaited && STATE.with_borrow(|state| state.awaiting.is_empty()) {
        look_for_scaled();
    }
}

/// An answer from OBS: the choices it asked for, a switch taken, a Browser
/// Source's settings, or a refusal.
fn read_answer(socket: u32, response: websocket::Response) {
    switch_answered(&response.request_id, response.request_status.result);
    if !response.request_status.result {
        host::log(
            Level::Warn,
            &format!(
                "OBS refused {} ({}): {}",
                response.request_type,
                response.request_status.code,
                response.request_status.comment.unwrap_or_default()
            ),
        );
        return;
    }
    let Some(data) = response.response_data.as_ref() else { return };
    let id = response.request_id.as_str();
    let read = if id == SCENES {
        websocket::scene_list(data).map(|list| scenes_listed(socket, list))
    } else if id == INPUTS {
        websocket::input_list(data).map(|inputs| inputs_listed(socket, inputs))
    } else if let Some(scene) = id.strip_prefix(SOURCES) {
        websocket::scene_items(data).map(|items| {
            let mut names: Vec<String> = Vec::new();
            for item in &items {
                if !names.contains(&item.source) {
                    names.push(item.source.clone());
                }
            }
            host::set_options(id, &names);
            STATE.with_borrow_mut(|state| state.items.insert(scene.to_string(), items));
        })
    } else if let Some(source) = id.strip_prefix(FILTERS) {
        websocket::filter_list(data).map(|filters| {
            let names: Vec<String> = filters.iter().map(|filter| filter.name.clone()).collect();
            host::set_options(id, &names);
            STATE.with_borrow_mut(|state| {
                if filters.is_empty() {
                    state.filters.remove(source);
                } else {
                    state.filters.insert(source.to_string(), filters);
                }
            });
            list_filtered();
        })
    } else if id == CANVAS {
        websocket::canvas(data)
            .map(|(width, height)| host::set_options(CANVAS, &[format!("{width}x{height}")]))
    } else if let Some(input) = id.strip_prefix(BROWSER) {
        let url = websocket::browser_url(data).unwrap_or_default();
        page_is(input, Some(&url));
        let refresh =
            STATE.with_borrow_mut(|state| state.to_refresh.remove(input) && state.app_pages.contains(input));
        if refresh {
            host::log(Level::Info, &format!("refreshing OBS's Browser Source {input:?} ({url})"));
            let data = json!({ "inputName": input, "propertyName": "refreshnocache" });
            host::send(socket, &websocket::request("PressInputPropertiesButton", "refresh", Some(data)));
        }
        Ok(())
    } else {
        return;
    };
    if let Err(why) = read {
        host::log(Level::Warn, &format!("an answer from OBS it can't read: {why}"));
    }
}

/// The scenes: their list, and each one's sources and filters asked for;
/// the lists of scenes that went go.
fn scenes_listed(socket: u32, list: websocket::SceneList) {
    host::set_options(SCENES, &list.scenes);
    let gone = STATE.with_borrow_mut(|state| {
        state.on_air = list.on_air.clone();
        let before = std::mem::replace(&mut state.scenes, list.scenes.clone());
        let gone: Vec<String> = before.into_iter().filter(|scene| !list.scenes.contains(scene)).collect();
        for scene in &gone {
            state.items.remove(scene);
        }
        gone
    });
    for scene in gone {
        host::set_options(&format!("{SOURCES}{scene}"), &[]);
    }
    forget_filters();
    for scene in &list.scenes {
        let data = json!({ "sceneName": scene });
        let id = format!("{SOURCES}{scene}");
        host::send(socket, &websocket::request("GetSceneItemList", &id, Some(data)));
        STATE.with_borrow_mut(|state| state.awaiting.insert(id));
        ask_for_filters(socket, scene);
    }
}

/// The inputs: their list, the text sources, each one's filters asked
/// for, and the Browser Sources' settings, to know those showing the app
/// (and refresh them, once, if OBS was running as the app started).
fn inputs_listed(socket: u32, inputs: Vec<websocket::Input>) {
    let names: Vec<String> = inputs.iter().map(|input| input.name.clone()).collect();
    host::set_options(INPUTS, &names);
    let texts: Vec<String> = inputs
        .iter()
        .filter(|input| input.kind.starts_with("text_"))
        .map(|input| input.name.clone())
        .collect();
    host::set_options(TEXTS, &texts);
    let browsers: Vec<String> = inputs
        .iter()
        .filter(|input| input.kind == "browser_source")
        .map(|input| input.name.clone())
        .collect();
    STATE.with_borrow_mut(|state| {
        state.inputs = inputs;
        state.app_pages.retain(|page| browsers.contains(page));
        if std::mem::take(&mut state.refresh_pages) {
            state.to_refresh = browsers.iter().cloned().collect();
        }
    });
    forget_filters();
    for name in &names {
        ask_for_filters(socket, name);
    }
    for name in browsers {
        let data = json!({ "inputName": name });
        let id = format!("{BROWSER}{name}");
        host::send(socket, &websocket::request("GetInputSettings", &id, Some(data)));
        STATE.with_borrow_mut(|state| state.awaiting.insert(id));
    }
}

/// A Browser Source shows `url`: whether that makes it newly known to show
/// the app.
fn page_is(input: &str, url: Option<&str>) -> bool {
    let app = url.is_some_and(|url| websocket::is_app_url(url, host::app_port()));
    STATE.with_borrow_mut(|state| {
        if app {
            state.app_pages.insert(input.to_string())
        } else {
            state.app_pages.remove(input);
            false
        }
    })
}

/// OBS changed a source's transform (moved, resized…): a source showing
/// the app resized by hand is checked once the resizing stops.
fn transformed(scene: String, id: i64, transform: serde_json::Value) {
    let resized = STATE.with_borrow_mut(|state| {
        let app_pages = &state.app_pages;
        let item = state.items.get_mut(&scene)?.iter_mut().find(|item| item.id == id)?;
        let resized = app_pages.contains(&item.source)
            && match (Transform::read(&item.transform), Transform::read(&transform)) {
                (Ok(was), Ok(now)) => !was.sized_as(&now),
                _ => false,
            };
        item.transform = transform;
        Some(resized)
    });
    if resized != Some(true) {
        return;
    }
    let earlier = STATE.with_borrow_mut(|state| {
        if !state.resized.iter().any(|(at, item)| *at == scene && *item == id) {
            state.resized.push((scene, id));
        }
        state.settle.take()
    });
    if let Some(timer) = earlier {
        host::cancel_timer(timer);
    }
    let timer = host::set_timer(SETTLE_MS, false);
    STATE.with_borrow_mut(|state| state.settle = Some(timer));
}

/// The sources showing the app, each scaled one fixed or warned about
/// (once since connecting).
fn look_for_scaled() {
    let pages: Vec<String> = STATE.with_borrow(|state| state.app_pages.iter().cloned().collect());
    for page in pages {
        let shown = showing(&page);
        if let Some((key, _)) = shown.iter().find(|(_, transform)| transform.is_scaled()) {
            unscale(&page, key.clone(), false);
        } else {
            STATE.with_borrow_mut(|state| {
                for (key, _) in &shown {
                    state.warned.remove(key);
                }
            });
        }
    }
    show_scaled();
}

/// The plugin's status, once logged in: Connected, or how many sources
/// showing the app OBS still scales (orange, so the panel's header shows
/// its Plugins dot), with a note under Scaled overlays saying where and
/// what to do.
fn show_scaled() {
    if ready_socket().is_none() {
        return;
    }
    let warn_only = setting(&host::settings(), SCALED) != "fix";
    let (lines, sources) = STATE.with_borrow(|state| {
        let mut items = Vec::new();
        for scene in &state.scenes {
            for item in state.items.get(scene).into_iter().flatten() {
                if state.app_pages.contains(&item.source)
                    && let Ok(transform) = Transform::read(&item.transform)
                {
                    items.push((scene.as_str(), item.source.as_str(), transform));
                }
            }
        }
        let shown: Vec<Shown> =
            items.iter().map(|(scene, source, transform)| Shown { scene, source, transform }).collect();
        let mut sources: Vec<String> = Vec::new();
        for at in shown.iter().filter(|at| at.transform.is_scaled()) {
            if !sources.iter().any(|source| source == at.source) {
                sources.push(at.source.to_string());
            }
        }
        (scaling::blurry(&shown, warn_only), sources)
    });
    match sources.as_slice() {
        [] => host::set_status(StatusLevel::Ok, "Connected"),
        [source] if source.chars().count() <= 20 => {
            host::set_status(StatusLevel::Problem, &format!("{source} is blurry"))
        }
        [_] => host::set_status(StatusLevel::Problem, "An overlay is blurry"),
        sources => host::set_status(StatusLevel::Problem, &format!("{} overlays are blurry", sources.len())),
    }
    host::set_note(SCALED, StatusLevel::Problem, &lines);
}

/// A source resized by hand, once the resizing stopped.
fn resized_by_hand(scene: String, id: i64) {
    let source = STATE.with_borrow(|state| {
        let item = state.items.get(&scene)?.iter().find(|item| item.id == id)?;
        Some(item.source.clone()).filter(|source| state.app_pages.contains(source))
    });
    if let Some(source) = source {
        unscale(&source, (scene, id), true);
        show_scaled();
    }
}

/// The items showing `source` in every scene (scene, item id), top first,
/// with their transforms.
fn showing(source: &str) -> Vec<((String, i64), Transform)> {
    STATE.with_borrow(|state| {
        let mut shown = Vec::new();
        for scene in &state.scenes {
            for item in state.items.get(scene).into_iter().flatten() {
                if item.source == source
                    && let Ok(transform) = Transform::read(&item.transform)
                {
                    shown.push(((scene.clone(), item.id), transform));
                }
            }
        }
        shown
    })
}

/// Scaled overlays: OBS scales `key`, an item showing the Browser Source
/// `source`. With Resize (Warn is the default) the page becomes the size
/// it's shown at and the item unscaled; the source's other items are kept
/// looking as they were. When that leaves those scaled (they're another
/// size), it's done only for a source resized `by_hand`, otherwise warned
/// about: two items of one source at two sizes would swap which is sharp
/// each time the app starts.
fn unscale(source: &str, key: (String, i64), by_hand: bool) {
    let shown = showing(source);
    let Some((_, transform)) = shown.iter().find(|(at, _)| *at == key) else { return };
    let scene = &key.0;
    if !transform.is_scaled() {
        STATE.with_borrow_mut(|state| state.warned.remove(&key));
        return;
    }
    let was = transform.page();
    let page = match transform.unscaled_page() {
        Ok(page) => page,
        Err(BoundsAndCrop) => {
            let why = format!(
                "OBS scales the Browser Source {source:?} in {scene:?}, so it's blurry; it has a bounding box and \
                 a crop, so the app can't work out its size: reset its transform (right-click → Transform → Reset \
                 Transform) and size it again"
            );
            return warn_once(&key, by_hand, &why);
        }
    };
    if setting(&host::settings(), SCALED) != "fix" {
        let why = format!(
            "OBS scales the Browser Source {source:?} in {scene:?} from {} to {}, so it's blurry: set its Width \
             and Height to {} and reset its transform (right-click → Transform → Reset Transform)",
            size(was),
            size(page),
            size(page)
        );
        return warn_once(&key, by_hand, &why);
    }
    let others: Vec<&((String, i64), Transform)> = shown.iter().filter(|(at, _)| *at != key).collect();
    let elsewhere: Vec<String> = others
        .iter()
        .filter(|(_, other)| other.unscaled_page() != Ok(page))
        .map(|((at, _), other)| match other.unscaled_page() {
            Ok(shown) => format!("{} in {at:?}", size(shown)),
            Err(BoundsAndCrop) => format!("another size in {at:?}"),
        })
        .collect();
    if !by_hand && !elsewhere.is_empty() {
        let why = format!(
            "OBS shows the Browser Source {source:?} at {} in {scene:?} and at {}: one page can't be both sizes, \
             so it's scaled and blurry in some; make another Browser Source for one of the scenes",
            size(page),
            elsewhere.join(", ")
        );
        return warn_once(&key, false, &why);
    }

    let mut changes = vec![(key.clone(), transform.unscaled())];
    for (at, other) in others {
        if other.unscaled_page() == Ok(page) {
            if other.is_scaled() {
                changes.push((at.clone(), other.unscaled()));
            }
        } else if let Some(change) = other.kept(page) {
            changes.push((at.clone(), change));
        }
    }
    for ((scene, id), change) in &changes {
        let data = json!({ "sceneName": scene, "sceneItemId": id, "sceneItemTransform": change });
        send_request("SetSceneItemTransform", data, "unscale", source);
    }
    if page != was {
        let settings = json!({ "width": page.0, "height": page.1 });
        let data = json!({ "inputName": source, "inputSettings": settings, "overlay": true });
        send_request("SetInputSettings", data, "resize", source);
    }
    // As OBS will have them, before it says so: what it then says isn't a
    // resize by hand.
    STATE.with_borrow_mut(|state| {
        for (scene, items) in state.items.iter_mut() {
            for item in items.iter_mut().filter(|item| item.source == source) {
                let at = (scene.clone(), item.id);
                if let Some((_, change)) = changes.iter().find(|(key, _)| *key == at) {
                    scaling::merge(&mut item.transform, change);
                }
                scaling::merge(
                    &mut item.transform,
                    &json!({ "sourceWidth": page.0, "sourceHeight": page.1 }),
                );
            }
        }
        state.warned.remove(&key);
    });
    host::log(
        Level::Info,
        &format!(
            "OBS scaled the Browser Source {source:?} in {scene:?} from {} to {}: its page is now {}, unscaled",
            size(was),
            size(page),
            size(page)
        ),
    );
    if !elsewhere.is_empty() {
        let why = format!(
            "the Browser Source {source:?} is also in other scenes, at {}: one page can't be every size, so it's \
             scaled and blurry there; make another Browser Source for those scenes",
            elsewhere.join(", ")
        );
        warn_once(&key, true, &why);
    }
}

/// Logs a warning about the item `key`, once since connecting unless
/// `again`.
fn warn_once(key: &(String, i64), again: bool, why: &str) {
    if STATE.with_borrow_mut(|state| state.warned.insert(key.clone())) || again {
        host::log(Level::Warn, why);
    }
}

fn ask_for_filters(socket: u32, source: &str) {
    let data = json!({ "sourceName": source });
    host::send(socket, &websocket::request("GetSourceFilterList", &format!("{FILTERS}{source}"), Some(data)));
}

/// Forgets the filters of sources that went (no longer a scene or an
/// input), and their lists.
fn forget_filters() {
    let gone = STATE.with_borrow_mut(|state| {
        let known = |source: &String| {
            state.scenes.contains(source) || state.inputs.iter().any(|input| input.name == *source)
        };
        let gone: Vec<String> = state.filters.keys().filter(|source| !known(source)).cloned().collect();
        for source in &gone {
            state.filters.remove(source);
        }
        gone
    });
    for source in &gone {
        host::set_options(&format!("{FILTERS}{source}"), &[]);
    }
    if !gone.is_empty() {
        list_filtered();
    }
}

/// The sources with filters: the scenes (top first), then the inputs.
fn list_filtered() {
    let filtered = STATE.with_borrow(|state| {
        let inputs = state.inputs.iter().map(|input| &input.name);
        state
            .scenes
            .iter()
            .chain(inputs)
            .filter(|source| state.filters.contains_key(*source))
            .cloned()
            .collect::<Vec<_>>()
    });
    host::set_options(FILTERED, &filtered);
}

/// OBS answered request `request`: if it's a switch waiting to carry on,
/// taken (Started; Ended too if the scene is on air already, as OBS then
/// has no change to tell) or refused (neither).
fn switch_answered(request: &str, taken: bool) {
    let (started, ended) = STATE.with_borrow_mut(|state| {
        let Some(at) = state.switches.iter().position(|switch| switch.request == request) else {
            return (None, None);
        };
        if !taken {
            state.switches.remove(at);
            return (None, None);
        }
        let switch = &mut state.switches[at];
        switch.started = true;
        let run = switch.run;
        if state.on_air.as_deref() == Some(switch.scene.as_str()) {
            state.switches.remove(at);
            (Some(run), Some(run))
        } else {
            (Some(run), None)
        }
    });
    if let Some(run) = started {
        host::carry_on(run, STARTED);
    }
    if let Some(run) = ended {
        host::carry_on(run, ENDED);
    }
}

/// Asks for what the choices are made of: the scenes (then each one's
/// sources) and the inputs (then each source's filters); and the canvas.
fn ask_for_choices(socket: u32) {
    host::send(socket, &websocket::request("GetSceneList", SCENES, None));
    host::send(socket, &websocket::request("GetInputList", INPUTS, None));
    host::send(socket, &websocket::request("GetVideoSettings", CANVAS, None));
}

/// Asks for the choices again shortly, once.
fn refresh_soon() {
    if STATE.with_borrow(|state| state.refresh.is_some()) {
        return;
    }
    let timer = host::set_timer(REFRESH_AFTER_MS, false);
    STATE.with_borrow_mut(|state| state.refresh = Some(timer));
}

/// The connection, once logged in.
fn ready_socket() -> Option<u32> {
    STATE.with_borrow(|state| state.socket.filter(|_| state.ready))
}

/// Sends a request an effect node makes, and gives its id; `doing` and
/// `what` say what it is in the log when OBS isn't there ("can't switch to
/// WoW").
fn send_request(kind: &str, data: serde_json::Value, doing: &str, what: &str) -> Option<String> {
    let Some(socket) = ready_socket() else {
        host::log(Level::Warn, &format!("not connected to OBS, so it can't {doing} {what}"));
        return None;
    };
    let id = STATE.with_borrow_mut(|state| {
        state.requests += 1;
        format!("run-{}", state.requests)
    });
    host::send(socket, &websocket::request(kind, &id, Some(data)));
    Some(id)
}

/// Opens the connection to obs-websocket, at the address setting.
fn connect() {
    let login = login(&host::settings());
    let url = format!("ws://{}", login.0);
    STATE.with_borrow_mut(|state| state.login = login);
    match host::connect(&url) {
        Ok(socket) => STATE.with_borrow_mut(|state| state.socket = Some(socket)),
        Err(why) => {
            host::log(Level::Warn, &format!("can't connect to {url}: {why}"));
            host::set_status(StatusLevel::Problem, "Check the address");
            retry_later();
        }
    }
}

/// Connects again after a wait that grows each time.
fn retry_later() {
    let ms = STATE.with_borrow_mut(|state| {
        let ms = state.retry_ms;
        state.retry_ms = (ms * 2).min(RETRY_MAX_MS);
        ms
    });
    let timer = host::set_timer(ms, false);
    STATE.with_borrow_mut(|state| state.retry = Some(timer));
}

/// A text setting's (or input's) value; empty when it has none.
fn setting(settings: &[NamedValue], name: &str) -> String {
    settings
        .iter()
        .find(|setting| setting.name == name)
        .and_then(|setting| match &setting.value {
            Value::Plain(text) => Some(text.clone()),
            Value::Text(text) => Some(text.plain.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The address and the password, as it connects with them.
fn login(settings: &[NamedValue]) -> (String, String) {
    (address(settings), setting(settings, "password"))
}

/// Forgets the connection and what came with it (the Scaled overlays note
/// included: what OBS scales is known again once it's back).
/// `still_there`: OBS may still be running (it refused the login, or a new
/// address or password closed it), so its Browser Sources may still need
/// the refresh; otherwise they load once it starts, with the app there.
fn disconnected(still_there: bool) {
    STATE.with_borrow_mut(|state| {
        state.socket = None;
        state.ready = false;
        state.switches.clear();
        state.awaiting.clear();
        state.resized.clear();
        state.warned.clear();
        state.app_pages.clear();
        if !still_there {
            state.refresh_pages = false;
        }
    });
    host::set_note(SCALED, StatusLevel::Problem, &[]);
    host::set_options(CANVAS, &[]);
}

/// The address setting, `host:port`, without a `ws://` in front.
fn address(settings: &[NamedValue]) -> String {
    let typed = setting(settings, "address");
    let typed = typed.trim();
    let typed = typed.strip_prefix("ws://").unwrap_or(typed).trim_end_matches('/');
    if typed.is_empty() { DEFAULT_ADDRESS.to_string() } else { typed.to_string() }
}

export!(Obs);

#[cfg(test)]
mod tests {
    use super::*;

    fn address_setting(typed: &str) -> Vec<NamedValue> {
        vec![NamedValue { name: "address".into(), value: Value::Plain(typed.into()) }]
    }

    #[test]
    fn the_address_is_host_and_port() {
        assert_eq!(address(&[]), "localhost:4455");
        assert_eq!(address(&address_setting(" ")), "localhost:4455");
        assert_eq!(address(&address_setting("127.0.0.1:4456")), "127.0.0.1:4456");
        assert_eq!(address(&address_setting("ws://localhost:4455/")), "localhost:4455");
    }

    #[test]
    fn scenes_and_filters_added_renamed_or_removed_in_obs_refresh_the_choices() {
        for fixture in [
            include_str!("../fixtures/scene_created.json"),
            include_str!("../fixtures/scene_name_changed.json"),
            include_str!("../fixtures/scene_removed.json"),
            include_str!("../fixtures/scene_list_changed.json"),
            include_str!("../fixtures/source_filter_created.json"),
            include_str!("../fixtures/source_filter_name_changed.json"),
            include_str!("../fixtures/source_filter_removed.json"),
        ] {
            let Ok(Incoming::Event { kind, .. }) = websocket::parse(fixture) else { panic!("an event") };
            assert!(REFRESHED_BY.contains(&kind.as_str()), "{kind}");
        }
    }

    #[test]
    fn actions() {
        assert_eq!(Action::of("show", "show", "hide"), Action::On);
        assert_eq!(Action::of("hide", "show", "hide"), Action::Off);
        assert_eq!(Action::of("", "show", "hide"), Action::Toggle);
        assert!(Action::Toggle.apply(false));
        assert!(!Action::Toggle.apply(true));
        assert!(Action::On.apply(true));
    }

    #[test]
    fn seconds_are_kept_in_bounds() {
        let with = |seconds: f64| vec![NamedValue { name: "seconds".into(), value: Value::Number(seconds) }];
        assert_eq!(seconds(&[]), 0.0);
        assert_eq!(seconds(&with(10.0)), 10.0);
        assert_eq!(seconds(&with(-1.0)), 0.0);
        assert_eq!(seconds(&with(f64::INFINITY)), 0.0);
        assert_eq!(seconds(&with(1e9)), LONGEST_SECONDS);
    }
}
