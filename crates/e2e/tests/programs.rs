//! Programs on the control API as plugins, end to end: their flow and value
//! nodes answered on the WebSocket, hearing the chat and events, and one
//! placed event node fired at a time. Run with `cargo e2e`. (Registering,
//! event and effect nodes and voices are in `panel.rs`,
//! `a_program_connects_as_a_plugin`.)

use std::time::Duration;

use e2e::{App, Panel, alert_node, event_node, plugin_node, test_config, wire};
use protocol::activity::ItemOrigin;
use protocol::cards::{Card, NodeKind};
use protocol::control::{ControlMessage, ControlRequest, PlacedNode, Switch};
use serde_json::{Map, Value, json};

const PLUGIN: &str = "regulars";

fn card(id: &str, nodes: Vec<protocol::cards::Node>, wires: Vec<protocol::cards::Wire>) -> Card {
    Card { id: id.into(), name: id.into(), enabled: true, nodes, wires }
}

fn custom(name: &str) -> protocol::cards::Node {
    event_node(NodeKind::Custom { name: name.into() })
}

fn event(name: &str) -> ControlRequest {
    ControlRequest::Event { name: name.into(), data: Map::new() }
}

fn values(json: Value) -> Map<String, Value> {
    json.as_object().unwrap().clone()
}

/// A chat message, as `/debug/input` takes it.
fn chat(id: &str, name: &str, text: &str) -> Value {
    json!({
        "at_ms": 0,
        "origin": { "provider": "twitch_chat", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
        "kind": {
            "input": "message",
            "id": id,
            "user": { "id": name, "name": name.to_lowercase(), "display_name": name },
            "text": text,
            "emotes": [],
        },
    })
}

fn error(message: &ControlMessage) -> Option<(u16, String)> {
    match message {
        ControlMessage::Error { status, error } => Some((*status, error.clone())),
        _ => None,
    }
}

fn nodes(message: &ControlMessage) -> Option<Vec<PlacedNode>> {
    match message {
        ControlMessage::Nodes { nodes } => Some(nodes.clone()),
        _ => None,
    }
}

/// A flow node (Split: Yes with what it said, or No), a value node
/// (Double), and an event node with a setting for the program (Word
/// said); hearing the chat and events.
fn manifest() -> protocol::plugin::Manifest {
    serde_json::from_value(json!({
        "id": PLUGIN, "name": "Regulars", "version": "1.0.0", "api": 1,
        "hears": ["chat", "events"],
        "nodes": [
            { "id": "split", "title": "Split", "category": "flow",
              "inputs": [{ "name": "event", "label": "Event", "type": "event" }],
              "outputs": [
                { "name": "yes", "label": "Yes", "type": "event" },
                { "name": "no", "label": "No", "type": "event" },
                { "name": "said", "label": "Said", "type": "text" }
              ] },
            { "id": "double", "title": "Double", "category": "values",
              "inputs": [{ "name": "n", "label": "N", "type": "number" }],
              "outputs": [{ "name": "twice", "label": "Twice", "type": "number" }],
              "settings": [{ "key": "n", "label": "N", "type": "number", "default": 1 }] },
            { "id": "said", "title": "Word said", "category": "events",
              "outputs": [
                { "name": "event", "label": "Event", "type": "event" },
                { "name": "viewer", "label": "Viewer", "type": "text" }
              ],
              "settings": [{ "key": "word", "label": "Word", "type": "text", "default": "gg" }] }
        ]
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_program_answers_its_flow_and_value_nodes_and_hears() {
    let mut config = test_config();
    config.cards.push(card(
        "split",
        vec![
            custom("split"),
            plugin_node(2, PLUGIN, "split", json!({})),
            alert_node(3, "unused"),
            alert_node(4, "no"),
        ],
        vec![
            wire((1, "event"), (2, "event")),
            wire((2, "yes"), (3, "trigger")),
            wire((2, "said"), (3, "message")),
            wire((2, "no"), (4, "trigger")),
        ],
    ));
    config.cards.push(card(
        "double",
        vec![custom("double"), plugin_node(2, PLUGIN, "double", json!({ "n": 21 })), alert_node(3, "unused")],
        vec![wire((1, "event"), (3, "trigger")), wire((2, "twice"), (3, "message"))],
    ));
    for word in ["hi", "bye"] {
        config.cards.push(card(
            word,
            vec![
                plugin_node(1, PLUGIN, "said", json!({ "word": word })),
                alert_node(2, &format!("{word}: {{viewer}}")),
            ],
            vec![wire((1, "event"), (2, "trigger"))],
        ));
    }
    config.plugins.insert(
        PLUGIN.into(),
        protocol::config::PluginConfig { hears: vec!["chat".into(), "events".into()], ..Default::default() },
    );
    let app = App::start_debug("a_program_answers_its_flow_and_value_nodes_and_hears", config).await;
    let mut overlay = app.overlay(vec![]).await;
    let mut program = app.control().await;
    program.send(&ControlRequest::Register { plugin: Box::new(manifest()) }).await;
    program
        .wait_for("registered", |message| matches!(message, ControlMessage::Registered { .. }).then_some(()))
        .await;

    // Told its event nodes on the cards, each with its own setting.
    let placed = program.wait_for("its nodes", nodes).await;
    let words: Vec<(&str, &str, bool)> = placed
        .iter()
        .map(|node| (node.id.as_str(), node.settings["word"]["text"].as_str().unwrap_or_default(), node.on))
        .collect();
    assert_eq!(words, [("hi/1", "hi", true), ("bye/1", "bye", true)]);

    // Split: an event waits until the program says where it goes.
    let route = |message: &ControlMessage| match message {
        ControlMessage::Route { run, node, origin, .. } if node == "split" => Some((*run, *origin)),
        _ => None,
    };
    let routed = |run, output: Option<&str>, values| ControlRequest::Routed {
        run,
        output: output.map(Into::into),
        values,
    };
    program.send(&event("split")).await;
    let (run, origin) = program.wait_for("a route", route).await;
    assert_eq!(origin, ItemOrigin::Live);
    program.send(&routed(run, Some("yes"), values(json!({ "said": "went on" })))).await;
    assert_eq!(overlay.next_alert().await, "went on");
    // Nowhere, then from No; answered twice, refused.
    program.send(&event("split")).await;
    let (run, _) = program.wait_for("a route", route).await;
    program.send(&routed(run, None, Map::new())).await;
    program.send(&event("split")).await;
    let (run, _) = program.wait_for("a route", route).await;
    program.send(&routed(run, Some("no"), Map::new())).await;
    assert_eq!(overlay.next_alert().await, "no", "the one before went nowhere");
    program.send(&routed(run, None, Map::new())).await;
    let refused = program.wait_for("a refusal", error).await;
    assert_eq!(refused.0, 404, "{refused:?}");

    // Double: its value, worked out for the event.
    let compute = |message: &ControlMessage| match message {
        ControlMessage::Compute { run, node, values, .. } if node == "double" => {
            Some((*run, values["n"].as_f64()))
        }
        _ => None,
    };
    program.send(&event("double")).await;
    let (run, n) = program.wait_for("a compute", compute).await;
    assert_eq!(n, Some(21.0));
    program.send(&ControlRequest::Computed { run, values: values(json!({ "twice": 42 })) }).await;
    assert_eq!(overlay.next_alert().await, "42");

    // It hears the chat, and events.
    let panel = Panel::open(&app, "plugins").await;
    panel.input(chat("m1", "Ann", "hi everyone")).await;
    let said = program
        .wait_for("the chat", |message| match message {
            ControlMessage::Chat { line } => Some(
                line.parts
                    .iter()
                    .map(|part| match part {
                        protocol::AlertPart::Text { text, .. } => text.as_str(),
                        protocol::AlertPart::Emote { code, .. } => code.as_str(),
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .await;
    assert_eq!(said, "hi everyone");
    program.send(&event("party")).await;
    let heard = program
        .wait_for("an event", |message| match message {
            ControlMessage::HeardEvent { kind, origin, .. } => Some((kind.clone(), *origin)),
            _ => None,
        })
        .await;
    assert_eq!(heard, ("custom".to_string(), ItemOrigin::Live));

    // Word said: one card at a time, by its id.
    let emit = |node: &str, viewer: &str, to: &str| ControlRequest::Emit {
        node: node.into(),
        output: None,
        values: values(json!({ "viewer": viewer })),
        to: Some(to.into()),
    };
    program.send(&emit("", "Ann", "bye/1")).await;
    assert_eq!(overlay.next_alert().await, "bye: Ann");
    program.send(&emit("said", "Bo", "hi/1")).await;
    assert_eq!(overlay.next_alert().await, "hi: Bo", "not hi: Ann before it");
    program.send(&emit("", "Cy", "gone/1")).await;
    let refused = program.wait_for("a refusal", error).await;
    assert!(refused.0 == 404 && refused.1.contains("\"gone/1\""), "{refused:?}");
    // Told again when one of their cards goes off.
    program.send(&ControlRequest::Card { id: "bye".into(), set: Switch::Off }).await;
    let placed = program.wait_for("its nodes again", nodes).await;
    let on: Vec<(&str, bool)> = placed.iter().map(|node| (node.id.as_str(), node.on)).collect();
    assert_eq!(on, [("hi/1", true), ("bye/1", false)]);

    // Gone while asked: the event waiting for its value goes on at once,
    // without it (the alert's own message).
    program.send(&event("double")).await;
    program.wait_for("a compute", compute).await;
    drop(program);
    let alert = overlay.wait_at_most(Duration::from_secs(5), e2e::alert_shown).await;
    assert_eq!(alert.map(|(_, text)| text).as_deref(), Some("unused"), "well before 10 seconds");
    panel.close().await;
}
