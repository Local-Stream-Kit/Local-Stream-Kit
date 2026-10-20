//! The Copypasta plugin's server code: a WebAssembly component for Local
//! Stream Kit's plugin interface (`crates/protocol/wit/plugin.wit`). See
//! docs/writing-a-plugin.md → Server code.
//!
//! It hears the chat (`hears: ["chat"]`, allowed on the Plugins page) and
//! fires its Copypasta event node when the same message (at least
//! Shortest message characters, ignoring case and spaces at the ends)
//! comes from Viewers different viewers within Within seconds. Each
//! Copypasta node on a card has its own settings (`nodes-changed`) and is
//! fired on its own (`host::emit_to`), so one card can react to three
//! viewers and another to ten.

wit_bindgen::generate!({ path: "../../../crates/protocol/wit", world: "plugin" });

use std::cell::RefCell;
use std::collections::HashMap;

use lsk::plugin::host;
use lsk::plugin::types::{EventValue, NamedEventValue, Value};

/// By message: who posted it, and when (ms).
type Seen = HashMap<String, Vec<(String, u64)>>;

/// A Copypasta node on a card that's on, and what it has seen.
struct Watching {
    id: String,
    viewers: usize,
    within_ms: u64,
    shortest: usize,
    seen: Seen,
}

thread_local! {
    static WATCHING: RefCell<Vec<Watching>> = const { RefCell::new(Vec::new()) };
}

struct Copypasta;

impl Guest for Copypasta {
    fn start() {}

    /// Its nodes and their settings, every time they change (one added,
    /// removed, edited, or its card switched on or off). What a node has
    /// seen stays with it, by its id, whatever changed.
    fn nodes_changed(nodes: Vec<PlacedNode>) {
        WATCHING.with_borrow_mut(|watching| {
            let mut seen: HashMap<String, Seen> =
                watching.drain(..).map(|node| (node.id, node.seen)).collect();
            *watching = nodes
                .into_iter()
                .filter(|node| node.on && node.node == "copypasta")
                .map(|node| {
                    let value = |name: &str| node.settings.iter().find(|setting| setting.name == name);
                    let number = |name: &str, otherwise: f64| match value(name).map(|setting| &setting.value) {
                        Some(Value::Number(number)) if number.is_finite() => *number,
                        _ => otherwise,
                    };
                    Watching {
                        viewers: number("viewers", 3.0).max(2.0) as usize,
                        within_ms: (number("within", 30.0).max(1.0) * 1000.0) as u64,
                        shortest: number("shortest", 10.0).max(1.0) as usize,
                        seen: seen.remove(&node.id).unwrap_or_default(),
                        id: node.id,
                    }
                })
                .collect();
        });
    }

    /// It has no tab.
    fn on_tab(_json: String) {}

    fn on_chat(heard: HeardChat) {
        let HeardChat::Line(line) = heard else { return };
        let message = line.text.plain.trim().to_string();
        let key = message.to_lowercase();
        let viewer = line.viewer.id;
        WATCHING.with_borrow_mut(|watching| {
            for node in watching.iter_mut() {
                if message.chars().count() < node.shortest {
                    continue;
                }
                let within_ms = node.within_ms;
                for posts in node.seen.values_mut() {
                    posts.retain(|(_, at)| line.at_ms.saturating_sub(*at) < within_ms);
                }
                node.seen.retain(|_, posts| !posts.is_empty());
                let posts = node.seen.entry(key.clone()).or_default();
                posts.push((viewer.clone(), line.at_ms));
                let mut viewers: Vec<&str> = posts.iter().map(|(who, _)| who.as_str()).collect();
                viewers.sort_unstable();
                viewers.dedup();
                if viewers.len() < node.viewers {
                    continue;
                }
                let (viewers, count) = (viewers.len(), posts.len());
                node.seen.remove(&key);
                host::emit_to(
                    &node.id,
                    "event",
                    &[
                        NamedEventValue { name: "message".into(), value: EventValue::Text(message.clone()) },
                        NamedEventValue { name: "viewers".into(), value: EventValue::Number(viewers as f64) },
                        NamedEventValue { name: "count".into(), value: EventValue::Number(count as f64) },
                        NamedEventValue {
                            name: "summary".into(),
                            value: EventValue::Text(format!("{viewers} viewers copied “{message}”")),
                        },
                    ],
                );
            }
        });
    }

    /// It has no effect nodes, so it's never asked.
    fn run(
        _node: String,
        _input: String,
        _values: Vec<NamedValue>,
        _viewer: Option<Viewer>,
        _run: Option<u64>,
        _queue_id: Option<u64>,
        _origin: RunOrigin,
    ) {
    }

    fn skip(_queue_id: u64) {}

    /// It has no settings of its own.
    fn settings_changed() {}

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

    /// It doesn't hear events.
    fn on_event(_heard: HeardEvent) {}

    fn on_timer(_id: u32) {}

    fn on_response(_id: u32, _response: Result<Response, String>) {}

    /// It doesn't moderate.
    fn on_moderated(_id: u32, _done: Result<(), String>) {}

    fn on_socket(_id: u32, _event: SocketEvent) {}

    /// It has no voices, so it's never asked.
    fn speak(_request: u32, _voice: String, _text: String, _speed: f32) {}
}

export!(Copypasta);
