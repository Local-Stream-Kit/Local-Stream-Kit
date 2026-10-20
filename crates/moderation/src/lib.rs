//! The built-in Moderation plugin's server code: a WebAssembly component for
//! the plugin interface (`crates/protocol/wit/plugin.wit`), built into
//! `crates/server/plugins/moderation/server.wasm`. Its effect nodes time out
//! or ban the viewer who set their card off (a `!vanish` command), with the
//! streamer's login (`host.moderate`).
//!
//! Only events from the stream do: a test or a replay only logs what it
//! would have done, so "Test this card" never times out a real viewer. The
//! broadcaster is left alone (Twitch refuses anyway). Why a moderation
//! didn't work goes in a note on its Plugins box until the next one works.

// wit-bindgen's glue for `run` takes more arguments than clippy likes.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({ path: "../protocol/wit", world: "plugin" });

use std::cell::RefCell;
use std::collections::HashMap;

use lsk::plugin::host;
use lsk::plugin::types::{Level, Moderation, StatusLevel, Timeout, Value};

/// The longest timeout Twitch allows: two weeks.
const LONGEST_SECONDS: u32 = 14 * 24 * 60 * 60;

thread_local! {
    /// The moderations waiting for an answer, by id: what each was, for
    /// people ("time out Ann for 10 s").
    static ASKED: RefCell<HashMap<u32, String>> = RefCell::new(HashMap::new());
}

/// What one of its nodes does.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Act {
    /// For this many seconds.
    Timeout(u32),
    Ban,
}

impl Act {
    /// Node `node`'s, with its values.
    fn of(node: &str, values: &[NamedValue]) -> Option<Act> {
        match node {
            "timeout" => Some(Act::Timeout(seconds(values))),
            "ban" => Some(Act::Ban),
            _ => None,
        }
    }

    /// For people: "time out Ann for 10 s", "ban Ann".
    fn describe(self, name: &str) -> String {
        match self {
            Act::Timeout(seconds) => format!("time out {name} for {}", duration(seconds)),
            Act::Ban => format!("ban {name}"),
        }
    }

    fn moderation(self, viewer: String) -> Moderation {
        match self {
            Act::Timeout(seconds) => Moderation::Timeout(Timeout { viewer, seconds }),
            Act::Ban => Moderation::Ban(viewer),
        }
    }
}

/// Seconds, wired in or typed: whole, from 1 to two weeks.
fn seconds(values: &[NamedValue]) -> u32 {
    let seconds = values.iter().find(|value| value.name == "seconds").and_then(|value| match value.value {
        Value::Number(seconds) if seconds.is_finite() => Some(seconds),
        _ => None,
    });
    seconds.unwrap_or(1.0).round().clamp(1.0, f64::from(LONGEST_SECONDS)) as u32
}

/// "10 s", "5 min", "2 h", "90 s".
fn duration(seconds: u32) -> String {
    match seconds {
        seconds if seconds >= 3600 && seconds.is_multiple_of(3600) => format!("{} h", seconds / 3600),
        seconds if seconds >= 60 && seconds.is_multiple_of(60) => format!("{} min", seconds / 60),
        seconds => format!("{seconds} s"),
    }
}

/// It didn't work: in the log, and in a note on its Plugins box.
fn failed(what: &str, why: &str) {
    let line = format!("Couldn't {what}: {why}");
    host::log(Level::Warn, &line);
    host::set_note("", StatusLevel::Problem, &[line]);
}

struct Plugin;

impl Guest for Plugin {
    fn start() {}

    fn run(
        node: String,
        _input: String,
        values: Vec<NamedValue>,
        viewer: Option<Viewer>,
        _run: Option<u64>,
        _queue_id: Option<u64>,
        origin: RunOrigin,
    ) {
        let Some(act) = Act::of(&node, &values) else {
            host::log(Level::Warn, &format!("no such node: {node}"));
            return;
        };
        let Some(viewer) = viewer else {
            host::log(Level::Info, &format!("{}: the event has no viewer", act.describe("nobody")));
            return;
        };
        let name = if viewer.display_name.is_empty() { &viewer.name } else { &viewer.display_name };
        let what = act.describe(name);
        if viewer.roles.broadcaster {
            host::log(Level::Info, &format!("Didn't {what}: they're the broadcaster."));
            return;
        }
        match origin {
            RunOrigin::Live => {}
            RunOrigin::Test => return host::log(Level::Info, &format!("A test: would {what}.")),
            RunOrigin::Replay => return host::log(Level::Info, &format!("A replay: would {what}.")),
        }
        match host::moderate(&viewer.platform, &act.moderation(viewer.id)) {
            Ok(id) => ASKED.with_borrow_mut(|asked| {
                asked.insert(id, what);
            }),
            Err(why) => failed(&what, &why),
        }
    }

    fn on_moderated(id: u32, done: Result<(), String>) {
        let Some(what) = ASKED.with_borrow_mut(|asked| asked.remove(&id)) else { return };
        match done {
            Ok(()) => {
                host::log(Level::Info, &format!("Did {what}."));
                host::set_note("", StatusLevel::Ok, &[]);
            }
            Err(why) => failed(&what, &why),
        }
    }

    /// Nothing waits in the queue.
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

    fn on_socket(_id: u32, _event: SocketEvent) {}

    /// It has no voices.
    fn speak(_request: u32, _voice: String, _text: String, _speed: f32) {}
}

export!(Plugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn with_seconds(seconds: f64) -> Vec<NamedValue> {
        vec![NamedValue { name: "seconds".into(), value: Value::Number(seconds) }]
    }

    #[test]
    fn each_node_does_its_own() {
        assert_eq!(Act::of("timeout", &with_seconds(10.0)), Some(Act::Timeout(10)));
        assert_eq!(Act::of("ban", &[]), Some(Act::Ban));
        assert_eq!(Act::of("warn", &[]), None);
    }

    #[test]
    fn seconds_are_whole_from_one_to_two_weeks() {
        assert_eq!(seconds(&with_seconds(9.6)), 10);
        assert_eq!(seconds(&with_seconds(0.0)), 1);
        assert_eq!(seconds(&with_seconds(-5.0)), 1);
        assert_eq!(seconds(&with_seconds(f64::NAN)), 1);
        assert_eq!(seconds(&with_seconds(1e12)), LONGEST_SECONDS);
        assert_eq!(seconds(&[]), 1, "nothing at all: the shortest");
    }

    #[test]
    fn what_it_does_for_people() {
        assert_eq!(Act::Timeout(10).describe("Ann"), "time out Ann for 10 s");
        assert_eq!(Act::Timeout(90).describe("Ann"), "time out Ann for 90 s");
        assert_eq!(Act::Timeout(600).describe("Ann"), "time out Ann for 10 min");
        assert_eq!(Act::Timeout(7200).describe("Ann"), "time out Ann for 2 h");
        assert_eq!(Act::Ban.describe("Ann"), "ban Ann");
    }
}
