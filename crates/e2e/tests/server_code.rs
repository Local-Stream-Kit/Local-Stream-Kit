//! Plugins' server code in cards, end to end: flow and value nodes, hearing
//! the chat and settings on event nodes, and not reaching the app itself.
//! Run with `cargo e2e`. (Effect and event nodes, timers, web requests and sockets
//! are in `panel.rs`, `server_code_runs_in_the_app`.)

use e2e::{App, Panel, alert_node, event_node, install_plugin, plugin_node, test_config, wire};
use protocol::cards::{Card, NodeKind};
use protocol::control::{ControlRequest, Switch};

fn card(id: &str, nodes: Vec<protocol::cards::Node>, wires: Vec<protocol::cards::Wire>) -> Card {
    Card { id: id.into(), name: id.into(), enabled: true, nodes, wires }
}

fn custom(name: &str) -> protocol::cards::Node {
    event_node(NodeKind::Custom { name: name.into() })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn plugin_flow_nodes_send_events_on() {
    let mut config = test_config();
    // The Counter example's Every other: Passes shows its Count, Skipped
    // says so.
    config.cards.push(card(
        "every",
        vec![
            custom("pass"),
            plugin_node(2, "counter", "every_other", serde_json::json!({ "counter": "Passes" })),
            alert_node(3, "unused"),
            alert_node(4, "skipped"),
        ],
        vec![
            wire((1, "event"), (2, "event")),
            wire((2, "event"), (3, "trigger")),
            wire((2, "count"), (3, "message")),
            wire((2, "miss"), (4, "trigger")),
        ],
    ));
    // Host check's Later answers later, with who set the event off.
    for (id, event) in
        [("later", custom("later")), ("later-raid", event_node(NodeKind::Raid { min_viewers: 0.0 }))]
    {
        config.cards.push(card(
            id,
            vec![
                event,
                plugin_node(2, "host-check", "later", serde_json::json!({})),
                alert_node(3, "unused"),
            ],
            vec![
                wire((1, "event"), (2, "event")),
                wire((2, "event"), (3, "trigger")),
                wire((2, "who"), (3, "message")),
            ],
        ));
    }
    config.plugins.insert(
        "host-check".into(),
        protocol::config::PluginConfig { hosts: vec!["127.0.0.1".into()], ..Default::default() },
    );
    let mut app = App::start("plugin_flow_nodes_send_events_on", config).await;
    install_plugin(&app, "examples/plugins/counter");
    install_plugin(&app, "crates/e2e/plugins/host-check");
    app.restart().await;
    app.wait_running(&["counter", "host-check"]).await;

    let mut overlay = app.overlay(vec![]).await;
    let mut control = app.control().await;
    for expected in ["skipped", "2", "skipped", "4"] {
        control.send(&ControlRequest::Event { name: "pass".into(), data: Default::default() }).await;
        assert_eq!(overlay.next_alert().await, expected);
    }
    control.send(&ControlRequest::Event { name: "later".into(), data: Default::default() }).await;
    assert_eq!(overlay.next_alert().await, "nobody (Live)");

    // Tested from the panel: a test, with the test raid's viewer. The
    // editor shows the flow node with its outputs.
    let panel = Panel::open(&app, "card/later-raid").await;
    panel.wait("the card's nodes", "document.querySelectorAll('.node').length === 3").await;
    panel
        .wait(
            "Later's Who",
            "document.querySelector('.node')?.closest('.surface')?.innerText.includes('Who')",
        )
        .await;
    panel.click("#card-test").await;
    // (The premade raid alert shows too.)
    overlay
        .wait_for("TestViewer (Test)", |command| {
            e2e::alert_shown(command).filter(|(_, text)| text == "TestViewer (Test)")
        })
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn plugin_value_nodes_give_their_values() {
    let mut config = test_config();
    // The Bits to money example: Money's As text into an alert's message,
    // from its own Bits (250 at 2 per 100), or a cheer's Amount.
    let money = |settings: serde_json::Value| plugin_node(2, "bits-to-money", "money", settings);
    config.cards.push(card(
        "money",
        vec![custom("money"), money(serde_json::json!({ "bits": 250, "rate": 2 })), alert_node(3, "unused")],
        vec![wire((1, "event"), (3, "trigger")), wire((2, "text"), (3, "message"))],
    ));
    config.cards.push(card(
        "cheer-money",
        vec![
            event_node(NodeKind::Bits { min: 0.0 }),
            money(serde_json::json!({ "currency": "€" })),
            alert_node(3, "unused"),
        ],
        vec![
            wire((1, "event"), (3, "trigger")),
            wire((1, "amount"), (2, "bits")),
            wire((2, "text"), (3, "message")),
        ],
    ));
    let mut app = App::start("plugin_value_nodes_give_their_values", config).await;
    install_plugin(&app, "examples/plugins/bits-to-money");
    app.restart().await;
    app.wait_running(&["bits-to-money"]).await;

    let mut overlay = app.overlay(vec![]).await;
    let mut control = app.control().await;
    control.send(&ControlRequest::Event { name: "money".into(), data: Default::default() }).await;
    assert_eq!(overlay.next_alert().await, "$5.00");

    // Tested from the panel, with the test cheer's bits; the editor shows
    // the value node with its outputs.
    let panel = Panel::open(&app, "card/cheer-money").await;
    panel.wait("the card's nodes", "document.querySelectorAll('.node').length === 3").await;
    panel.wait("its As text", "document.querySelector('.surface')?.innerText.includes('As text')").await;
    panel.click("#card-test").await;
    // (100 bits at the default 1 per 100.)
    overlay.wait_for("€1.00", |command| e2e::alert_shown(command).filter(|(_, text)| text == "€1.00")).await;
    panel.close().await;
}

/// A chat message, as `/debug/input` takes it.
fn chat(id: &str, name: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn server_code_hears_the_chat_and_fires_one_placed_node() {
    let mut config = test_config();
    // The Copypasta example on two cards: two viewers, and three.
    for (id, viewers, message) in [("two", 2, "{viewers} copied {message}"), ("three", 3, "three: {message}")]
    {
        let pasta = plugin_node(
            1,
            "copypasta",
            "copypasta",
            serde_json::json!({ "viewers": viewers, "shortest": 5 }),
        );
        config.cards.push(card(
            id,
            vec![pasta, alert_node(2, message)],
            vec![wire((1, "event"), (2, "trigger"))],
        ));
    }
    config.plugins.insert(
        "copypasta".into(),
        protocol::config::PluginConfig { hears: vec!["chat".into()], ..Default::default() },
    );
    let mut app = App::start_debug("server_code_hears_the_chat_and_fires_one_placed_node", config).await;
    install_plugin(&app, "examples/plugins/copypasta");
    app.restart().await;
    app.wait_running(&["copypasta"]).await;

    let mut overlay = app.overlay(vec![]).await;
    let page = Panel::open(&app, "plugins").await;
    page.wait("its box", "document.getElementById('plugin-copypasta')").await;
    // Too short for either; then each card with its own count.
    for (id, name) in [("m1", "Ann"), ("m2", "Bo"), ("m3", "Cy")] {
        page.input(chat(id, name, "hi")).await;
    }
    page.input(chat("m4", "Ann", "hello there")).await;
    page.input(chat("m5", "Bo", "Hello there")).await;
    assert_eq!(overlay.next_alert().await, "2 copied Hello there", "as the last one wrote it");
    page.input(chat("m6", "Cy", "hello there")).await;
    assert_eq!(overlay.next_alert().await, "three: hello there");

    // Its card switched off: the plugin is told, and only the other fires.
    // What the other had seen before stays with it.
    page.input(chat("m7", "Ann", "third time")).await;
    let mut control = app.control().await;
    control.send(&ControlRequest::Card { id: "three".into(), set: Switch::Off }).await;
    app.wait_for_config("three off", |config| {
        config.cards.iter().any(|card| card.id == "three" && !card.enabled)
    })
    .await;
    for (id, name) in [("m8", "Bo"), ("m9", "Cy"), ("m10", "Dee"), ("m11", "Eve")] {
        let text = if id == "m10" || id == "m11" { "final words" } else { "third time" };
        page.input(chat(id, name, text)).await;
    }
    assert_eq!(overlay.next_alert().await, "2 copied third time", "Ann's, from before, and Bo's");
    assert_eq!(overlay.next_alert().await, "2 copied final words", "not three: third time");
    page.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_plugins_page_asks_to_hear_the_chat_and_events() {
    let mut app = App::start("the_plugins_page_asks_to_hear_the_chat_and_events", test_config()).await;
    // The Bits to money example, wanting to hear both.
    install_plugin(&app, "examples/plugins/bits-to-money");
    let manifest = app.dir.join("plugins").join("bits-to-money").join("plugin.json");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("\"api\": 1,", "\"api\": 1, \"hears\": [\"chat\", \"events\"],"))
        .unwrap();
    app.restart().await;

    let panel = Panel::open(&app, "plugins/bits-to-money").await;
    panel.wait("asked", "document.getElementById('plugin-bits-to-money-allow-hears')").await;
    let asked: String = panel.eval("document.getElementById('plugin-bits-to-money-hears').innerText").await;
    assert!(
        asked.contains(
            "It wants to hear the chat: every message, with who sent it; and every event (cheers, subs, raids…), with who set it off."
        ),
        "{asked}"
    );
    panel.click("#plugin-bits-to-money-allow-hears").await;
    app.wait_for_config("allowed", |config| config.plugin("bits-to-money").hears == ["chat", "events"]).await;
    panel
        .wait(
            "It hears the chat and events",
            "document.getElementById('plugin-bits-to-money-disallow-hears')?.parentElement.innerText.includes('It hears the chat and events')",
        )
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn server_code_cant_reach_the_app_itself() {
    // Host check is allowed 127.0.0.1, where the app listens too: it tries
    // the app's control API, its overlay WebSocket, and a redirect from a
    // tool on this PC to the app. Each is refused; the tool itself isn't.
    let mut config = test_config();
    let check = plugin_node(2, "host-check", "check", serde_json::json!({}));
    config.cards.push(card("check", vec![custom("check"), check], vec![wire((1, "event"), (2, "trigger"))]));
    let result = plugin_node(1, "host-check", "result", serde_json::json!({}));
    config.cards.push(card(
        "report",
        vec![result, alert_node(2, "{what}: {result}")],
        vec![wire((1, "event"), (2, "trigger"))],
    ));
    config.plugins.insert(
        "host-check".into(),
        protocol::config::PluginConfig { hosts: vec!["127.0.0.1".into()], ..Default::default() },
    );
    let mut app = App::start("server_code_cant_reach_the_app_itself", config).await;
    install_plugin(&app, "crates/e2e/plugins/host-check");
    app.restart().await;
    app.wait_running(&["host-check"]).await;

    let tool = e2e::local_tool(app.port).await;
    let mut overlay = app.overlay(vec![]).await;
    let mut control = app.control().await;
    let mut check = async |what: &str, url: String| {
        let data = serde_json::json!({ "what": what, "url": url }).as_object().unwrap().clone();
        control.send(&ControlRequest::Event { name: "check".into(), data }).await;
    };
    let refused = "can't reach the app itself (127.0.0.1 is this PC, on the app's port)";

    check("fetch", format!("http://127.0.0.1:{}/control/state", app.port)).await;
    assert_eq!(overlay.next_alert().await, format!("fetch: refused: server code {refused}"));
    check("socket", format!("ws://127.0.0.1:{}{}", app.port, protocol::WS_PATH)).await;
    assert_eq!(overlay.next_alert().await, format!("socket: refused: server code {refused}"));
    // Redirected to the app: the request fails.
    check("fetch", format!("http://127.0.0.1:{tool}/to-app")).await;
    let failed = overlay.next_alert().await;
    assert!(failed.starts_with("fetch: failed: ") && failed.ends_with(refused), "{failed}");
    // The tool, on another port of this PC, answers.
    check("fetch", format!("http://127.0.0.1:{tool}/")).await;
    assert_eq!(overlay.next_alert().await, "fetch: status 200");
}
