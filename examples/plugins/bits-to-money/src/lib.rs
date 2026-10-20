//! The Bits to money plugin's server code: a WebAssembly component for
//! Local Stream Kit's plugin interface (`crates/protocol/wit/plugin.wit`).
//! See docs/writing-a-plugin.md → Server code.
//!
//! "Bits to money" is a value node: wire a cheer's Amount into its Bits,
//! and its Money (a number) and As text ("$5.00") into an alert, a
//! counter or a text. The app asks `compute` once per event that needs
//! it, and the card goes on with the answer.

wit_bindgen::generate!({ path: "../../../crates/protocol/wit", world: "plugin" });

use lsk::plugin::types::{Text, TextPart, Value};

struct BitsToMoney;

impl Guest for BitsToMoney {
    fn start() {}

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

    /// Bits to money: the bits at the rate, at once. (A node that needs a
    /// web request first answers `Computed::Later` and then calls
    /// `host::compute_later` within 10 seconds.)
    fn compute(node: String, values: Vec<NamedValue>, _run: u64, _origin: RunOrigin) -> Computed {
        if node != "money" {
            return Computed::Now(Vec::new());
        }
        let value = |name: &str| values.iter().find(|value| value.name == name).map(|value| &value.value);
        let number = |name: &str, otherwise: f64| match value(name) {
            Some(Value::Number(number)) if number.is_finite() => *number,
            _ => otherwise,
        };
        let currency = match value("currency") {
            Some(Value::Text(text)) => text.plain.trim().to_string(),
            _ => "$".to_string(),
        };
        let money = number("bits", 0.0) / 100.0 * number("rate", 1.0);
        let shown = format!("{currency}{money:.2}");
        Computed::Now(vec![
            NamedValue { name: "money".into(), value: Value::Number(money) },
            NamedValue {
                name: "text".into(),
                value: Value::Text(Text { plain: shown.clone(), parts: vec![TextPart::Text(shown)] }),
            },
        ])
    }

    /// It doesn't hear the chat.
    fn on_chat(_heard: HeardChat) {}

    /// It doesn't hear events.
    fn on_event(_heard: HeardEvent) {}

    /// It has no event nodes.
    fn nodes_changed(_nodes: Vec<PlacedNode>) {}

    /// It has no tab.
    fn on_tab(_json: String) {}

    fn on_timer(_id: u32) {}

    fn on_response(_id: u32, _response: Result<Response, String>) {}

    /// It doesn't moderate.
    fn on_moderated(_id: u32, _done: Result<(), String>) {}

    fn on_socket(_id: u32, _event: SocketEvent) {}

    /// It has no voices, so it's never asked.
    fn speak(_request: u32, _voice: String, _text: String, _speed: f32) {}
}

export!(BitsToMoney);
