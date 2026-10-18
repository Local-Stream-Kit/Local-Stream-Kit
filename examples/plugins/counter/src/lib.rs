//! The Counter plugin's server code: a WebAssembly component for Local
//! Stream Kit's plugin interface (`crates/protocol/wit/plugin.wit`). See
//! docs/writing-a-plugin.md → Server code.
//!
//! "Count" adds to a named counter, kept with the plugin's stored values,
//! so counts last between streams. Each change fires "Counted" (for cards
//! that react to a count) and, if wanted, tells the overlay page. "Current
//! count", a value node, gives a counter's count to any event that needs
//! it (two counters in one text: "W:{wins} L:{losses}"). "Every other", a
//! flow node, counts the events coming in and lets every second one
//! through (Passes), the others out of Skipped, each with the count.
//!
//! Its tab in the panel ("Counters", `overlay/tab.html`) lists the counts,
//! with − and + to set one right during a stream, and adds counters: the
//! page sends `{ "counter": "Deaths", "by": -1 }`, `{ "add": "Deaths" }`
//! (or `{}` when it opens), and gets the counts back,
//! `{ "counts": [{ "counter": "Deaths", "count": 12 }] }`.

wit_bindgen::generate!({ path: "../../../crates/protocol/wit", world: "plugin" });

use lsk::plugin::host;
use lsk::plugin::types::{EventValue, Level, NamedEventValue, Route, Value};
use serde_json::json;

struct Counter;

impl Guest for Counter {
    fn start() {
        host::log(Level::Info, "ready to count");
    }

    fn run(node: String, _input: String, values: Vec<NamedValue>, _run: Option<u64>, _queue_id: Option<u64>) {
        if node != "count" {
            return;
        }
        let value = |name: &str| values.iter().find(|value| value.name == name).map(|value| &value.value);
        let counter = counter_name(&values);
        let by = match value("by") {
            Some(Value::Number(by)) => *by,
            _ => 1.0,
        };
        let reset = matches!(value("reset"), Some(Value::Toggle(true)));
        let show = !matches!(value("show"), Some(Value::Toggle(false)));

        let before = if reset { 0.0 } else { count_of(&counter) };
        let shown = set_count(&counter, before + by);
        if show {
            host::send_to_page(&json!({ "counter": counter, "count": shown }).to_string());
        }
    }

    /// Count doesn't wait in the queue.
    fn skip(_queue_id: u64) {}

    /// It has no settings of its own.
    fn settings_changed() {}

    /// Every other: counts the event, and sends every second one on. A
    /// flow node answers at once here; it could also answer `later` and
    /// call `host::route_later` (after a web request, say).
    fn route(
        node: String,
        _input: String,
        values: Vec<NamedValue>,
        _viewer: Option<Viewer>,
        _run: u64,
        _origin: RunOrigin,
    ) -> Routed {
        if node != "every_other" {
            return Routed::Stop;
        }
        let counter = match values.iter().find(|value| value.name == "counter").map(|value| &value.value) {
            Some(Value::Text(text)) if !text.plain.trim().is_empty() => text.plain.trim().to_lowercase(),
            _ => "every other".to_string(),
        };
        let key = format!("every:{counter}");
        let count = host::get(&key).and_then(|text| text.parse::<u64>().ok()).unwrap_or(0) + 1;
        host::set(&key, Some(&count.to_string()));
        let output = if count % 2 == 0 { "event" } else { "miss" };
        let values = vec![NamedValue { name: "count".into(), value: Value::Number(count as f64) }];
        Routed::Go(Route { output: output.into(), values })
    }

    /// Current count: the counter's count, as stored.
    fn compute(node: String, values: Vec<NamedValue>, _run: u64, _origin: RunOrigin) -> Computed {
        if node != "current" {
            return Computed::Now(Vec::new());
        }
        let count = count_of(&counter_name(&values));
        Computed::Now(vec![NamedValue { name: "count".into(), value: Value::Number(count) }])
    }

    /// It doesn't hear the chat.
    fn on_chat(_heard: HeardChat) {}

    /// It doesn't hear events.
    fn on_event(_heard: HeardEvent) {}

    /// Its event nodes have no settings for it.
    fn nodes_changed(_nodes: Vec<PlacedNode>) {}

    /// Its tab: − or + on a counter (`by`), which fires Counted as the
    /// Count node does (not shown on the overlay), a counter added (at 0,
    /// so nothing is counted), or the tab opening (`{}`). Every time the
    /// tab gets the counts.
    fn on_tab(json: String) {
        let message: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
        if let (Some(counter), Some(by)) = (message["counter"].as_str(), message["by"].as_f64()) {
            set_count(counter, count_of(counter) + by);
        } else if let Some(counter) = message["add"].as_str() {
            remember(counter.trim());
        }
        send_counts();
    }

    fn on_timer(_id: u32) {}

    fn on_response(_id: u32, _response: Result<Response, String>) {}

    fn on_socket(_id: u32, _event: SocketEvent) {}

    /// It has no voices, so it's never asked.
    fn speak(_request: u32, _voice: String, _text: String, _speed: f32) {}
}

/// The counter a node's Counter setting names ("Count" when it's empty).
fn counter_name(values: &[NamedValue]) -> String {
    match values.iter().find(|value| value.name == "counter").map(|value| &value.value) {
        Some(Value::Text(text)) if !text.plain.trim().is_empty() => text.plain.trim().to_string(),
        _ => "Count".to_string(),
    }
}

/// A counter's count, as stored (0 for a new one).
fn count_of(counter: &str) -> f64 {
    host::get(&format!("count:{}", counter.to_lowercase())).and_then(|text| text.parse().ok()).unwrap_or(0.0)
}

/// Remembers a counter's name for the tab, the first time (names are one
/// line, kept one per line).
fn remember(counter: &str) {
    let mut names = counters();
    if counter.is_empty() || counter.contains('\n') || names.iter().any(|name| name.eq_ignore_ascii_case(counter)) {
        return;
    }
    names.push(counter.to_string());
    host::set("counters", Some(&names.join("\n")));
}

/// Stores a counter's count, remembers its name for the tab, fires
/// Counted, and shows the counts in the tab. Answers the count as shown.
fn set_count(counter: &str, count: f64) -> String {
    host::set(&format!("count:{}", counter.to_lowercase()), Some(&count.to_string()));
    remember(counter);
    let shown = if count.fract() == 0.0 { format!("{count:.0}") } else { format!("{count:.2}") };
    host::emit(
        "counted",
        &[
            NamedEventValue { name: "counter".into(), value: EventValue::Text(counter.to_string()) },
            NamedEventValue { name: "count".into(), value: EventValue::Number(count) },
            NamedEventValue { name: "summary".into(), value: EventValue::Text(format!("{counter}: {shown}")) },
        ],
    );
    send_counts();
    shown
}

/// The counters it has counted, by name, in the order they started.
fn counters() -> Vec<String> {
    host::get("counters").map(|names| names.lines().map(str::to_string).collect()).unwrap_or_default()
}

/// The counts, for its tab in every open panel.
fn send_counts() {
    let counts: Vec<serde_json::Value> =
        counters().iter().map(|counter| json!({ "counter": counter, "count": count_of(counter) })).collect();
    host::send_to_tab(&json!({ "counts": counts }).to_string());
}

export!(Counter);
