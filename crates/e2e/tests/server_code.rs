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
    // The Counter example's Current count: a wins and losses tracker, two
    // counters in one text whenever either is counted.
    for name in ["Wins", "Losses"] {
        let count = plugin_node(2, "counter", "count", serde_json::json!({ "counter": name, "show": false }));
        config.cards.push(card(name, vec![custom(name), count], vec![wire((1, "event"), (2, "trigger"))]));
    }
    let current =
        |id, name: &str| plugin_node(id, "counter", "current", serde_json::json!({ "counter": name }));
    let score = NodeKind::Text { template: "W:{wins} L:{losses}".into(), size: None };
    config.cards.push(card(
        "score",
        vec![
            plugin_node(1, "counter", "counted", serde_json::json!({})),
            current(2, "Wins"),
            current(3, "Losses"),
            protocol::cards::Node { id: 4, x: 0.0, y: 0.0, kind: score },
            alert_node(5, "unused"),
        ],
        vec![
            wire((1, "event"), (5, "trigger")),
            wire((2, "count"), (4, "wins")),
            wire((3, "count"), (4, "losses")),
            wire((4, "text"), (5, "message")),
        ],
    ));
    let mut app = App::start("plugin_value_nodes_give_their_values", config).await;
    install_plugin(&app, "examples/plugins/bits-to-money");
    install_plugin(&app, "examples/plugins/counter");
    app.restart().await;
    app.wait_running(&["bits-to-money", "counter"]).await;

    let mut overlay = app.overlay(vec![]).await;
    let mut control = app.control().await;
    control.send(&ControlRequest::Event { name: "money".into(), data: Default::default() }).await;
    assert_eq!(overlay.next_alert().await, "$5.00");
    for (name, expected) in [("Wins", "W:1 L:0"), ("Losses", "W:1 L:1"), ("Wins", "W:2 L:1")] {
        control.send(&ControlRequest::Event { name: name.into(), data: Default::default() }).await;
        assert_eq!(overlay.next_alert().await, expected);
    }

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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn plugins_tabs_talk_to_their_server_code_and_fold_under_more() {
    let mut config = test_config();
    // The Counter example's Count, from a Custom event.
    config.cards.push(card(
        "hugs",
        vec![
            custom("hug"),
            plugin_node(2, "counter", "count", serde_json::json!({ "counter": "Hugs", "show": false })),
        ],
        vec![wire((1, "event"), (2, "trigger"))],
    ));
    // Its Counted, from the card or the tab, as an alert.
    config.cards.push(card(
        "counted",
        vec![plugin_node(1, "counter", "counted", serde_json::json!({})), alert_node(2, "unused")],
        vec![wire((1, "event"), (2, "trigger")), wire((1, "summary"), (2, "message"))],
    ));
    config.plugins.insert(
        "host-check".into(),
        protocol::config::PluginConfig { hosts: vec!["127.0.0.1".into()], ..Default::default() },
    );
    let mut app = App::start("plugins_tabs_talk_to_their_server_code_and_fold_under_more", config).await;
    install_plugin(&app, "examples/plugins/counter");
    install_plugin(&app, "crates/e2e/plugins/host-check");
    app.restart().await;
    app.wait_running(&["counter", "host-check"]).await;
    let mut overlay = app.overlay(vec![]).await;
    let mut control = app.control().await;

    // At full width every tab fits: the plugins' after Activity, by the
    // plugins' names (Counter, Host check).
    let panel = Panel::open(&app, "cards").await;
    let row =
        "[...document.querySelectorAll('.dock-header nav.tabs > a.tab')].map(tab => tab.textContent).join()";
    panel
        .wait("the plugins' tabs", &format!("{row} === 'Cards,Activity,Counters,Checks,Plugins,Test,Status'"))
        .await;
    assert_eq!(panel.count("#tabs-more").await, 0, "no ⋯ while everything fits");

    // The Counter's tab: its page asks for the counts when it opens, and
    // shows a card's count as it happens.
    let counter = "/plugins/counter/tab.html";
    panel.click(".dock-header a.tab[href='#/tab/counter']").await;
    panel
        .wait(
            "the Counters tab",
            "location.hash === '#/tab/counter' && !!document.querySelector('#tab-page')",
        )
        .await;
    panel
        .wait_in_frame(counter, "its empty list", "document.body.innerText.includes('Nothing counted yet.')")
        .await;
    control.send(&ControlRequest::Event { name: "hug".into(), data: Default::default() }).await;
    let count = "document.querySelector('.count')?.textContent";
    panel.wait_in_frame(counter, "the card's count", &format!("{count} === '1'")).await;
    assert_eq!(overlay.next_alert().await, "Hugs: 1");
    // Its + goes to the server code, which counts, fires Counted as a
    // card's count does, and answers.
    panel
        .eval_in_frame::<serde_json::Value>(
            counter,
            "document.querySelector('[aria-label=\"One more Hugs\"]').click()",
        )
        .await;
    panel.wait_in_frame(counter, "one more", &format!("{count} === '2'")).await;
    assert_eq!(overlay.next_alert().await, "Hugs: 2");
    control.send(&ControlRequest::Event { name: "hug".into(), data: Default::default() }).await;
    panel.wait_in_frame(counter, "the card counting on from there", &format!("{count} === '3'")).await;
    assert_eq!(overlay.next_alert().await, "Hugs: 3");
    // Add counter: a name and Enter, and the new counter shows at 0, after
    // the others, without firing Counted (the next alert is its first
    // count).
    panel.eval_in_frame::<serde_json::Value>(counter, "document.getElementById('add-counter').click()").await;
    panel
        .eval_in_frame::<serde_json::Value>(
            counter,
            "const name = document.getElementById('add-name'); name.value = 'Deaths'; \
             name.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))",
        )
        .await;
    let rows = "[...document.querySelectorAll('#counters .row')]\
        .map(row => `${row.querySelector('.name').textContent} ${row.querySelector('.count').textContent}`).join()";
    panel.wait_in_frame(counter, "the new counter", &format!("{rows} === 'Hugs 3,Deaths 0'")).await;
    panel
        .wait_in_frame(
            counter,
            "the name field closed",
            "document.getElementById('add-form').hidden && !document.getElementById('add-counter').hidden",
        )
        .await;
    panel
        .eval_in_frame::<serde_json::Value>(
            counter,
            "document.querySelector('[aria-label=\"One more Deaths\"]').click()",
        )
        .await;
    assert_eq!(overlay.next_alert().await, "Deaths: 1");

    // Host check's tab counts: its page sends 3, its code puts 3 on the
    // tab and answers.
    let checks = "/plugins/host-check/tab.html";
    panel.click(".dock-header a.tab[href='#/tab/host-check']").await;
    panel.wait_in_frame(checks, "its page", "typeof send === 'function'").await;
    panel.eval_in_frame::<serde_json::Value>(checks, "send(3)").await;
    panel
        .wait_in_frame(
            checks,
            "the answer",
            "document.getElementById('heard').textContent === '{\"count\":3}'",
        )
        .await;
    let checks_tab = "document.querySelector(\".dock-header a.tab[href='#/tab/host-check']\")?.textContent";
    panel.wait("the count on its tab", &format!("{checks_tab} === 'Checks (3)'")).await;

    // In the dock, what doesn't fit goes under ⋯ from the row's end; Cards
    // and Activity stay, and nothing runs off the edge. (Counters fits;
    // Checks, with room kept for a two-digit count, doesn't.)
    panel.resize(380, 800).await;
    panel.wait("⋯", "!!document.querySelector('#tabs-more')").await;
    panel.wait("the row in the dock", &format!("{row} === 'Cards,Activity,Counters'")).await;
    let fits: bool = panel
        .eval(
            "(() => { const nav = document.querySelector('.dock-header nav.tabs');              return nav.scrollWidth <= nav.clientWidth && document.documentElement.scrollWidth <= innerWidth; })()",
        )
        .await;
    assert!(fits, "the row runs off the edge");
    assert_eq!(panel.count("#tabs-more.current").await, 1, "the tab shown is under ⋯");
    assert_eq!(panel.count("#tabs-more .more-dot").await, 1, "a count is under ⋯");
    panel.click("#tabs-more").await;
    let menu = "[...document.querySelectorAll('.tabs-menu .menu-item')].map(item => item.textContent).join()";
    panel.wait("⋯'s menu", &format!("{menu} === 'Checks (3),Plugins,Test,Status'")).await;
    assert_eq!(
        panel.count(".tabs-menu .menu-sep").await,
        1,
        "a line between the plugins' tabs and the app's"
    );
    assert_eq!(panel.count(".tabs-menu .menu-item[aria-current='page']").await, 1, "the tab shown, marked");
    panel.click(".tabs-menu .menu-item[href='#/status']").await;
    panel
        .wait("Status, from ⋯", "location.hash === '#/status' && !document.querySelector('.tabs-menu')")
        .await;
    assert_eq!(panel.count("#tabs-more.current").await, 1, "Status is under ⋯ too");

    // Its hosts taken back, Host check's code stops: its tab says so, and
    // its count goes.
    panel.resize(1400, 900).await;
    panel.go("plugins/host-check").await;
    panel.click("#plugin-host-check-disallow").await;
    panel.go("tab/host-check").await;
    panel
        .wait(
            "the notice",
            "document.querySelector('#tab-not-running')?.innerText.includes(\"Host check isn't running: see Plugins.\")",
        )
        .await;
    panel.wait("its count gone", &format!("{checks_tab} === 'Checks'")).await;

    // Switched off, the Counter's tab goes, and a link to it opens its box.
    control.send(&ControlRequest::Plugin { id: "counter".into(), set: Switch::Off }).await;
    panel
        .wait(
            "the Counters tab gone",
            "!document.querySelector(\".dock-header a.tab[href='#/tab/counter']\")",
        )
        .await;
    panel.go("tab/counter").await;
    panel.wait("its box", "location.hash === '#/plugins/counter'").await;
    panel.close().await;
}
