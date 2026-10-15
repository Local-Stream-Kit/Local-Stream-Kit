//! The Counter plugin's server code: a WebAssembly component for Local
//! Stream Kit's plugin interface (`crates/protocol/wit/plugin.wit`). See
//! docs/writing-a-plugin.md → Server code.
//!
//! "Count" adds to a named counter, kept with the plugin's stored values,
//! so counts last between streams. Each change fires "Counted" (for cards
//! that react to a count) and, if wanted, tells the overlay page. "Every
//! other", a flow node, counts the events coming in and lets every second
//! one through (Passes), the others out of Skipped, each with the count.

wit_bindgen::generate!({ path: "../../../crates/protocol/wit", world: "plugin" });

use lsk::plugin::host;
use lsk::plugin::types::{EventValue, Level, NamedEventValue, Route, Value};

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
        let counter = match value("counter") {
            Some(Value::Text(text)) if !text.plain.trim().is_empty() => text.plain.trim().to_string(),
            _ => "Count".to_string(),
        };
        let by = match value("by") {
            Some(Value::Number(by)) => *by,
            _ => 1.0,
        };
        let reset = matches!(value("reset"), Some(Value::Toggle(true)));
        let show = !matches!(value("show"), Some(Value::Toggle(false)));

        let key = format!("count:{}", counter.to_lowercase());
        let before =
            if reset { 0.0 } else { host::get(&key).and_then(|text| text.parse().ok()).unwrap_or(0.0) };
        let count = before + by;
        host::set(&key, Some(&count.to_string()));

        let shown = if count.fract() == 0.0 { format!("{count:.0}") } else { format!("{count:.2}") };
        host::emit(
            "counted",
            &[
                NamedEventValue { name: "counter".into(), value: EventValue::Text(counter.clone()) },
                NamedEventValue { name: "count".into(), value: EventValue::Number(count) },
                NamedEventValue {
                    name: "summary".into(),
                    value: EventValue::Text(format!("{counter}: {shown}")),
                },
            ],
        );
        if show {
            host::send_to_page(&format!(r#"{{"counter":{},"count":"{shown}"}}"#, json_string(&counter)));
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

    fn on_timer(_id: u32) {}

    fn on_response(_id: u32, _response: Result<Response, String>) {}

    fn on_socket(_id: u32, _event: SocketEvent) {}

    /// It has no voices, so it's never asked.
    fn speak(_request: u32, _voice: String, _text: String, _speed: f32) {}
}

/// A text as a JSON string (no JSON crate needed for this much).
fn json_string(text: &str) -> String {
    let mut json = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            c if (c as u32) < 0x20 => json.push_str(&format!("\\u{:04x}", c as u32)),
            c => json.push(c),
        }
    }
    json.push('"');
    json
}

export!(Counter);
