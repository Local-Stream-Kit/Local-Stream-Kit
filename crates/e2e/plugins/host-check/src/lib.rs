//! For the e2e tests: its Check node tries one thing the app offers server
//! code (named by `what`), and its Result event says how it went.

wit_bindgen::generate!({ path: "../../../protocol/wit", world: "plugin" });

use std::cell::RefCell;

use lsk::plugin::host;
use lsk::plugin::types::{
    Audio, Choice, EventValue, Level, NamedEventValue, Request, StatusLevel, Text, TextPart, Value,
};

thread_local! {
    /// What each timer, request or socket was started for, by id.
    static WAITING: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
    /// Hold's runs, by the timer that ends them.
    static HOLDING: RefCell<Vec<(u32, u64)>> = const { RefCell::new(Vec::new()) };
    /// How often the repeating timer has fired.
    static REPEATS: RefCell<u32> = const { RefCell::new(0) };
    /// Later's runs, by the timer that sends them on, with who set them
    /// off.
    static LATER: RefCell<Vec<(u32, u64, String)>> = const { RefCell::new(Vec::new()) };
}

fn wait(id: u32, what: &str) {
    WAITING.with(|waiting| waiting.borrow_mut().push((id, what.to_string())));
}

fn waited(id: u32) -> String {
    WAITING.with(|waiting| {
        let mut waiting = waiting.borrow_mut();
        let found = waiting.iter().position(|(known, _)| *known == id);
        found.map(|index| waiting.remove(index).1).unwrap_or_default()
    })
}

fn report(what: &str, result: &str) {
    host::emit(
        "result",
        &[
            NamedEventValue { name: "what".into(), value: EventValue::Text(what.into()) },
            NamedEventValue { name: "result".into(), value: EventValue::Text(result.into()) },
        ],
    );
}

struct HostCheck;

impl Guest for HostCheck {
    fn start() {}

    fn run(node: String, _input: String, values: Vec<NamedValue>, run: Option<u64>, queue_id: Option<u64>) {
        // Hold: waits in the queue; done after 50 ms, or held until it's
        // skipped (`until` "skip").
        if node == "hold" {
            let Some(queue_id) = queue_id else {
                return host::log(Level::Warn, "hold: no queue id");
            };
            let until = values.iter().find(|value| value.name == "until").map(|value| &value.value);
            if matches!(until, Some(Value::Text(text)) if text.plain == "skip") {
                // Its page hears it (an alert would wait behind it).
                host::send_to_page(r#"{"holding":true}"#);
            } else {
                report("hold", "on stage");
                HOLDING.with(|holding| holding.borrow_mut().push((host::set_timer(50, false), queue_id)));
            }
            return;
        }
        // Relay: fires its own output Done for the run, at once.
        if node == "relay" {
            match run {
                Some(run) => host::carry_on(run, "done"),
                None => host::log(Level::Warn, "relay: no run id (Done isn't wired)"),
            }
            return;
        }
        let text = |name: &str| match values.iter().find(|value| value.name == name).map(|value| &value.value)
        {
            Some(Value::Text(text)) => text.plain.clone(),
            _ => String::new(),
        };
        let (what, url) = (text("what"), text("url"));
        match what.as_str() {
            "store" => {
                let n = host::get("n").and_then(|n| n.parse::<u32>().ok()).unwrap_or(0) + 1;
                host::set("n", Some(&n.to_string()));
                report("store", &format!("stored {n}"));
            }
            "timer" => wait(host::set_timer(30, false), "timer"),
            "repeat" => wait(host::set_timer(20, true), "repeat"),
            "fetch" | "blocked" => {
                let request = Request { method: "GET".into(), url, headers: Vec::new(), body: Vec::new() };
                match host::fetch(&request) {
                    Ok(id) => wait(id, &what),
                    Err(error) => report(&what, &format!("refused: {error}")),
                }
            }
            "socket" => match host::connect(&url) {
                Ok(id) => wait(id, "socket"),
                Err(error) => report("socket", &format!("refused: {error}")),
            },
            "settings" => {
                let greeting = host::settings().into_iter().find(|setting| setting.name == "greeting");
                match greeting.map(|setting| setting.value) {
                    Some(Value::Text(text)) => report("settings", &text.plain),
                    Some(Value::Plain(text)) => report("settings", &text),
                    _ => report("settings", "none"),
                }
            }
            "options" => {
                host::set_options("picks", &["one".to_string(), "two".to_string()]);
                report("options", "set");
            }
            "choices" => {
                let choice = |value: &str, label: &str| Choice { value: value.into(), label: label.into() };
                host::set_choices("picks", &[choice("one", "One"), choice("two-v", "Two")]);
                report("choices", "set");
            }
            // A note on its Plugins box itself (no setting).
            "note" => {
                host::set_note("", StatusLevel::Problem, &["The avatar didn't load".into(), "Try another".into()]);
                report("note", "set");
            }
            // Its Result's other output, Again (an output it hasn't is
            // ignored).
            "output" => {
                let values = [
                    NamedEventValue { name: "what".into(), value: EventValue::Text("output".into()) },
                    NamedEventValue { name: "result".into(), value: EventValue::Text("again".into()) },
                ];
                host::emit_output("result", "nope", &values);
                host::emit_output("result", "again", &values);
            }
            "secret" => {
                let token = host::settings().into_iter().find(|setting| setting.name == "token");
                match token.map(|setting| setting.value) {
                    Some(Value::Plain(text)) if text.is_empty() => report("secret", "empty"),
                    Some(Value::Plain(text)) => report("secret", &text),
                    _ => report("secret", "none"),
                }
            }
            "page" => {
                host::send_to_page(r#"{"hello":"page"}"#);
                report("page", "sent");
            }
            "loop" => loop {
                std::hint::black_box(());
            },
            other => report(other, "unknown"),
        }
    }

    fn skip(_queue_id: u64) {
        report("hold", "skipped");
    }

    /// Says its greeting and its token, as they are now.
    fn settings_changed() {
        let plain = |name: &str| {
            host::settings().into_iter().find(|setting| setting.name == name).and_then(|setting| {
                match setting.value {
                    Value::Text(text) => Some(text.plain),
                    Value::Plain(text) => Some(text),
                    _ => None,
                }
            })
        };
        let token = plain("token").filter(|token| !token.is_empty()).unwrap_or_else(|| "no token".into());
        report("settings changed", &format!("{}, {token}", plain("greeting").unwrap_or_default()));
    }

    /// Later: sends the event on 30 ms later (`route-later`), with Who
    /// saying who set it off and where it came from.
    fn route(
        _node: String,
        _input: String,
        _values: Vec<NamedValue>,
        viewer: Option<Viewer>,
        run: u64,
        origin: RunOrigin,
    ) -> Routed {
        let who = viewer.map_or_else(|| "nobody".to_string(), |viewer| viewer.display_name);
        let origin = match origin {
            RunOrigin::Live => "Live",
            RunOrigin::Test => "Test",
            RunOrigin::Replay => "Replay",
        };
        let who = format!("{who} ({origin})");
        LATER.with(|later| later.borrow_mut().push((host::set_timer(30, false), run, who)));
        Routed::Later
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
        let later = LATER.with(|later| {
            let mut later = later.borrow_mut();
            let found = later.iter().position(|(timer, _, _)| *timer == id);
            found.map(|index| later.remove(index))
        });
        if let Some((_, run, who)) = later {
            let text = Text { plain: who.clone(), parts: vec![TextPart::Text(who)] };
            return host::route_later(run, "event", &[NamedValue { name: "who".into(), value: Value::Text(text) }]);
        }
        let holding = HOLDING.with(|holding| {
            let mut holding = holding.borrow_mut();
            let found = holding.iter().position(|(timer, _)| *timer == id);
            found.map(|index| holding.remove(index).1)
        });
        if let Some(queue_id) = holding {
            return host::done(queue_id);
        }
        let what = WAITING.with(|waiting| {
            waiting
                .borrow()
                .iter()
                .find(|(known, _)| *known == id)
                .map(|(_, what)| what.clone())
                .unwrap_or_default()
        });
        if what == "repeat" {
            let fired = REPEATS.with(|repeats| {
                *repeats.borrow_mut() += 1;
                *repeats.borrow()
            });
            if fired == 3 {
                host::cancel_timer(id);
                waited(id);
                report("repeat", "fired 3 times");
            }
        } else {
            waited(id);
            report("timer", "fired");
        }
    }

    fn on_response(id: u32, response: Result<Response, String>) {
        let what = waited(id);
        match response {
            Ok(response) => report(&what, &format!("status {}", response.status)),
            Err(error) => report(&what, &format!("failed: {error}")),
        }
    }

    fn on_socket(id: u32, event: SocketEvent) {
        if let SocketEvent::Opened = event {
            waited(id);
            host::close(id);
            report("socket", "opened");
        }
    }

    /// Its voice "beep" says anything as half a second of silence (a WAV);
    /// "broken" can't say anything.
    fn speak(request: u32, voice: String, _text: String, _speed: f32) {
        match voice.as_str() {
            "beep" => host::spoken(request, Ok(&Audio { data: silence(500), format: "wav".into() })),
            _ => host::spoken(request, Err("this voice is broken")),
        }
    }
}

/// `ms` of silence as a WAV file: 8 kHz, mono, 16 bits.
fn silence(ms: u32) -> Vec<u8> {
    let data = ms * 8 * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes()); // bytes a second
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    wav.resize(wav.len() + data as usize, 0);
    wav
}

export!(HostCheck);
