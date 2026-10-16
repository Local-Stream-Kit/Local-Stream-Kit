//! The control panel, end to end. Run with `cargo e2e`.
//!
//! Premade cards used here (see `protocol::cards::premade`):
//! - `hydrate` (off): 1 Channel point reward → 2 Cooldown → 3 Alert
//! - `raid-alert` (on): 1 Raid → 2 Alert
//! - `rain-on-bits` (off): 1 Bits, 2 Math, 3 Clamp, 4 Emote rain

use e2e::{App, Key, Panel, js_string, modifiers, select, test_config};
use protocol::cards::{Card, Node, NodeKind, SocketRef, Wire};
use protocol::config::AppConfig;
use protocol::control::{ControlMessage, ControlRequest, Switch};
use protocol::plugin::PluginMessage;
use protocol::{Command, ShellCommand};
use std::sync::{Arc, Mutex};

/// A card's problems, with the built-in plugins' node types attached (as
/// the app has them).
fn problems(card: &Card) -> Vec<protocol::cards::Problem> {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/plugins");
    let plugins = ["alert", "hype-train", "media", "particles", "rain", "sound", "speak"]
        .into_iter()
        .map(|id| {
            let manifest = std::fs::read_to_string(folder.join(id).join("plugin.json")).unwrap();
            protocol::plugin::PluginInfo {
                manifest: serde_json::from_str(&manifest).unwrap(),
                overlay: true,
                bundled: true,
                ..Default::default()
            }
        })
        .collect();
    let mut card = card.clone();
    card.attach_plugins(&protocol::plugin::Catalog::new(plugins));
    card.problems()
}

fn card<'a>(config: &'a AppConfig, id: &str) -> &'a Card {
    config.cards.iter().find(|card| card.id == id).unwrap_or_else(|| panic!("no card {id}"))
}

fn wire(from: (u32, &str), to: (u32, &str)) -> Wire {
    Wire { from: SocketRef::new(from.0, from.1), to: SocketRef::new(to.0, to.1) }
}

/// A point on the canvas with nothing on it.
async fn empty_spot(panel: &Panel) -> (f64, f64) {
    let at: (f64, f64) = panel
        .eval(
            "(() => { const r = document.querySelector('.surface').getBoundingClientRect(); \
             return [r.x + r.width * 0.5, r.bottom - 120]; })()",
        )
        .await;
    let empty: bool = panel
        .eval(&format!("document.elementFromPoint({}, {}).classList.contains('surface')", at.0, at.1))
        .await;
    assert!(empty, "expected empty canvas at {at:?}");
    at
}

/// Opens a card's editor and waits for its nodes.
async fn open_card(app: &App, id: &str, nodes: usize) -> Panel {
    let panel = Panel::open(app, &format!("card/{id}")).await;
    panel.wait("the card's nodes", &format!("document.querySelectorAll('.node').length === {nodes}")).await;
    panel
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_card_list_opens_a_card_by_its_name() {
    // A card without its event: its problem shows under its summary.
    let mut config = test_config();
    let mut broken = card(&config, "raid-alert").clone();
    (broken.id, broken.name) = ("no-event".into(), "No event".into());
    broken.nodes.retain(|node| !node.kind.is_source());
    broken.wires.clear();
    config.cards.push(broken);
    let app = App::start("the_card_list_opens_a_card_by_its_name", config).await;
    let panel = Panel::open(&app, "cards").await;
    panel.wait("the list", "document.querySelector(\"li[data-card='no-event'] .card-problem\")").await;
    let problem: String =
        panel.eval("document.querySelector(\"li[data-card='no-event'] .card-problem\").textContent").await;
    assert_eq!(problem, "Connect an event to Trigger.");

    // Its name opens it, and leaves it as it was (only the switch turns it
    // on or off).
    let on = card(&app.saved_config(), "raid-alert").enabled;
    panel.click("li[data-card='raid-alert'] .card-name").await;
    panel.wait("the raid alert's editor", "location.hash === '#/card/raid-alert'").await;
    assert_eq!(card(&app.saved_config(), "raid-alert").enabled, on, "not switched");

    // + New card, after the cards.
    panel.run("location.hash = '#/cards'").await;
    panel.wait("the list again", "document.getElementById('card-new')").await;
    let last: bool = panel
        .eval("document.querySelector('.card-list').compareDocumentPosition(document.getElementById('card-new')) === Node.DOCUMENT_POSITION_FOLLOWING")
        .await;
    assert!(last, "New card comes after the cards");
    panel.click("#card-new").await;
    panel.wait("the presets", "location.hash === '#/new'").await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn card_switch_saves_and_undoes() {
    let app = App::start("card_switch_saves_and_undoes", test_config()).await;
    let panel = Panel::open(&app, "cards").await;

    panel.click("#card-switch-hydrate").await;
    app.wait_for_config("hydrate on", |config| card(config, "hydrate").enabled).await;

    panel.key(Key::Z, modifiers::CTRL).await;
    app.wait_for_config("hydrate off again", |config| !card(config, "hydrate").enabled).await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_stream_deck_change_while_a_panel_save_is_due_or_sent_is_kept() {
    let app =
        App::start("a_stream_deck_change_while_a_panel_save_is_due_or_sent_is_kept", test_config()).await;
    let panel = Panel::open(&app, "cards").await;
    let key = app.control_key();
    let control = |path: &str| format!("/control/cards/{path}?key={key}");
    let both = |hydrate: bool, raid: bool| {
        move |config: &AppConfig| {
            card(config, "hydrate").enabled == hydrate && card(config, "raid-alert").enabled == raid
        }
    };

    // A switch here, and the raid alert off over the control API while
    // the panel's save is still due.
    panel.click("#card-switch-hydrate").await;
    let (status, _) = panel.request("POST", &control("raid-alert/off")).await;
    assert_eq!(status, 200);
    app.wait_for_config("hydrate on, raid alert off", both(true, false)).await;
    panel
        .wait("the raid alert off here too", "!document.querySelector('#card-switch-raid-alert').checked")
        .await;
    assert!(both(true, false)(&app.saved_config()));

    // The panel's save held on its way until the raid alert is on again:
    // the server refuses it as made on an older config, and the panel
    // saves again on top of the newer one.
    panel
        .run(
            "const socket = window.__panelSockets.at(-1); window.__held = [];              socket.send = data => JSON.parse(data).type === 'save_config'                  ? window.__held.push(data) : WebSocket.prototype.send.call(socket, data);",
        )
        .await;
    panel.click("#card-switch-hydrate").await;
    panel.wait("the save held", "window.__held.length === 1").await;
    let (status, _) = panel.request("POST", &control("raid-alert/on")).await;
    assert_eq!(status, 200);
    app.wait_for_config("the raid alert on", both(true, true)).await;
    panel
        .wait("the raid alert on here too", "document.querySelector('#card-switch-raid-alert').checked")
        .await;
    panel
        .run(
            "const socket = window.__panelSockets.at(-1); delete socket.send;              for (const data of window.__held) socket.send(data);",
        )
        .await;
    app.wait_for_config("hydrate off, raid alert still on", both(false, true)).await;
    assert!(both(false, true)(&app.saved_config()));
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn undo_keeps_a_stream_deck_change() {
    let app = App::start("undo_keeps_a_stream_deck_change", test_config()).await;
    let panel = Panel::open(&app, "cards").await;
    let key = app.control_key();

    panel.click("#card-switch-hydrate").await;
    app.wait_for_config("hydrate on", |config| card(config, "hydrate").enabled).await;

    // The raid alert off elsewhere; undo here takes back only the panel's
    // own switch.
    let (status, _) = panel.request("POST", &format!("/control/cards/raid-alert/off?key={key}")).await;
    assert_eq!(status, 200);
    panel.wait("the raid alert off here", "!document.querySelector('#card-switch-raid-alert').checked").await;
    panel.key(Key::Z, modifiers::CTRL).await;
    let config = app.wait_for_config("hydrate off again", |config| !card(config, "hydrate").enabled).await;
    assert!(!card(&config, "raid-alert").enabled, "the raid alert stays off");

    // Redo, too.
    panel.key(Key::Y, modifiers::CTRL).await;
    let config = app.wait_for_config("hydrate on again", |config| card(config, "hydrate").enabled).await;
    assert!(!card(&config, "raid-alert").enabled, "the raid alert stays off");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn wiring_replaces_the_field_and_undoes() {
    let app = App::start("wiring_replaces_the_field_and_undoes", test_config()).await;
    let panel = open_card(&app, "hydrate", 3).await;
    let message_field = "document.querySelectorAll('#node-3-message').length";
    assert_eq!(panel.eval::<usize>(message_field).await, 1, "the message has a field while unwired");

    let from = panel.center(&select::output(1, "viewer")).await;
    let to = panel.center(&select::input(3, "message")).await;
    panel.drag(from, to, modifiers::NONE).await;
    let viewer_to_message = wire((1, "viewer"), (3, "message"));
    app.wait_for_config("the viewer wired into the message", |config| {
        card(config, "hydrate").wires.contains(&viewer_to_message)
    })
    .await;
    assert_eq!(panel.eval::<usize>(message_field).await, 0, "a wired input has no field");

    panel.key(Key::Z, modifiers::CTRL).await;
    app.wait_for_config("the wire undone", |config| {
        !card(config, "hydrate").wires.contains(&viewer_to_message)
    })
    .await;
    panel.wait("the field back", &format!("{message_field} === 1")).await;

    panel.key(Key::Y, modifiers::CTRL).await;
    app.wait_for_config("the wire redone", |config| {
        card(config, "hydrate").wires.contains(&viewer_to_message)
    })
    .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn wires_only_join_matching_types() {
    let app = App::start("wires_only_join_matching_types", test_config()).await;
    let panel = open_card(&app, "rain-on-bits", 4).await;
    let before = card(&app.saved_config(), "rain-on-bits").wires.clone();

    // Viewer (text) into the math node's By (number): refused.
    let from = panel.center(&select::output(1, "viewer")).await;
    let to = panel.center(&select::input(2, "b")).await;
    panel.drag(from, to, modifiers::NONE).await;
    // Amount (number) into By: fine. Once that's saved, the refused wire
    // would have been saved too.
    let from = panel.center(&select::output(1, "amount")).await;
    panel.drag(from, to, modifiers::NONE).await;
    let config = app
        .wait_for_config("amount wired into By", |config| {
            card(config, "rain-on-bits").wires.contains(&wire((1, "amount"), (2, "b")))
        })
        .await;
    assert_eq!(card(&config, "rain-on-bits").wires.len(), before.len() + 1, "only the matching wire");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_wire_dropped_on_empty_canvas_is_removed() {
    let app = App::start("a_wire_dropped_on_empty_canvas_is_removed", test_config()).await;
    let panel = open_card(&app, "hydrate", 3).await;

    let from = panel.center(&select::input(3, "trigger")).await;
    let to = empty_spot(&panel).await;
    panel.drag(from, to, modifiers::NONE).await;
    app.wait_for_config("nothing into the alert's trigger", |config| {
        card(config, "hydrate").wire_into(3, "trigger").is_none()
    })
    .await;
    panel
        .wait(
            "the problem on the alert",
            "document.querySelector(\".node[data-node='3'] .node-problem\")?.textContent === 'Connect an event to Trigger.'",
        )
        .await;
    assert_eq!(panel.count(".menu").await, 0, "picking a wire up and dropping it doesn't open the menu");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_loose_wire_adds_a_connected_node() {
    let app = App::start("a_loose_wire_adds_a_connected_node", test_config()).await;
    let panel = open_card(&app, "hydrate", 3).await;

    let from = panel.center(&select::output(1, "text")).await;
    let to = empty_spot(&panel).await;
    panel.drag(from, to, modifiers::NONE).await;
    panel.wait("the add-node menu", "document.querySelector('.menu input:focus')").await;
    let offered: Vec<String> =
        panel.eval("[...document.querySelectorAll('.menu-item')].map(item => item.textContent)").await;
    assert_eq!(
        offered,
        [
            "Has text",
            "Compare text",
            "Text",
            "Emotes in text",
            "Alert",
            "Show in chat",
            "Hype train bar",
            "OBSSet a text",
            "Speak"
        ],
        "only nodes that take text"
    );

    panel.type_text("ale").await;
    // As a person would: Enter once the search has narrowed the list.
    panel.wait("only Alert left", "document.querySelectorAll('.menu-item').length === 1").await;
    panel.key(Key::ENTER, modifiers::NONE).await;
    let config =
        app.wait_for_config("a fourth node", |config| card(config, "hydrate").nodes.len() == 4).await;
    let hydrate = card(&config, "hydrate");
    assert!(
        matches!(hydrate.node(4).map(|node| &node.kind), Some(NodeKind::Plugin { plugin, .. }) if plugin == "alert")
    );
    assert!(hydrate.wires.contains(&wire((1, "text"), (4, "message"))), "{:?}", hydrate.wires);
    assert_eq!(panel.count(".menu").await, 0);
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn moving_selected_nodes_is_one_undo_step() {
    let app = App::start("moving_selected_nodes_is_one_undo_step", test_config()).await;
    let panel = open_card(&app, "rain-on-bits", 4).await;
    let position = |config: &AppConfig, id: u32| {
        let node = card(config, "rain-on-bits").node(id).expect("the node");
        (node.x, node.y)
    };
    let before = app.saved_config();

    panel.click(&select::head(2)).await;
    let head3 = panel.center(&select::head(3)).await;
    panel.click_at(head3, modifiers::SHIFT).await;
    assert_eq!(panel.count(".node.selected").await, 2);

    let from = panel.center(&select::head(2)).await;
    panel.drag(from, (from.0 + 100.0, from.1 + 60.0), modifiers::NONE).await;
    let moved =
        app.wait_for_config("the nodes moved", |config| position(config, 2) != position(&before, 2)).await;
    let delta = |id| {
        (position(&moved, id).0 - position(&before, id).0, position(&moved, id).1 - position(&before, id).1)
    };
    assert_eq!(delta(2), delta(3), "both selected nodes moved together");
    assert!(delta(2).0 > 0.0 && delta(2).1 > 0.0);
    assert_eq!(position(&moved, 1), position(&before, 1), "unselected nodes stay");

    panel.key(Key::Z, modifiers::CTRL).await;
    let back = app
        .wait_for_config("one undo puts both back", |config| {
            position(config, 2) == position(&before, 2) && position(config, 3) == position(&before, 3)
        })
        .await;

    // Holding Alt, the grabbed node lands on the grid (22 px), the other
    // selected one moves with it.
    let from = panel.center(&select::head(2)).await;
    panel.drag(from, (from.0 + 107.0, from.1 + 53.0), modifiers::ALT).await;
    let snapped =
        app.wait_for_config("the nodes moved", |config| position(config, 2) != position(&back, 2)).await;
    let (x, y) = position(&snapped, 2);
    assert_eq!((x % 22.0, y % 22.0), (0.0, 0.0), "on the grid: {x}, {y}");
    let moved = |id| {
        (position(&snapped, id).0 - position(&back, id).0, position(&snapped, id).1 - position(&back, id).1)
    };
    assert_eq!(moved(2), moved(3), "both selected nodes moved together");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn box_select_and_delete() {
    let app = App::start("box_select_and_delete", test_config()).await;
    let panel = open_card(&app, "rain-on-bits", 4).await;

    let (start, end): ((f64, f64), (f64, f64)) = panel
        .eval(
            "(() => { const r = document.querySelector('.surface').getBoundingClientRect(); \
             return [[r.x + 5, r.y + 55], [r.right - 5, r.bottom - 60]]; })()",
        )
        .await;
    panel.drag(start, end, modifiers::SHIFT).await;
    panel.wait("all four selected", "document.querySelectorAll('.node.selected').length === 4").await;

    panel.key(Key::ESCAPE, modifiers::NONE).await;
    panel.wait("nothing selected", "document.querySelectorAll('.node.selected').length === 0").await;

    // Select one by clicking, delete it with its wires.
    panel.click(&select::head(3)).await;
    // Only Delete deletes: not Backspace, not X.
    panel.key(Key::BACKSPACE, modifiers::NONE).await;
    panel.key(Key::X, modifiers::NONE).await;
    assert_eq!(panel.count(".node").await, 4, "Backspace and X leave nodes alone");
    assert_eq!(panel.count(".save-state").await, 0, "saved: nothing to say");
    panel.key(Key::DELETE, modifiers::NONE).await;
    let config =
        app.wait_for_config("the clamp gone", |config| card(config, "rain-on-bits").node(3).is_none()).await;
    let left = card(&config, "rain-on-bits");
    assert_eq!(left.nodes.len(), 3);
    assert!(left.wires.iter().all(|wire| wire.from.node != 3 && wire.to.node != 3), "its wires went too");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn number_fields_keep_what_is_typed() {
    let app = App::start("number_fields_keep_what_is_typed", test_config()).await;
    let panel = open_card(&app, "hydrate", 3).await;
    let field = "document.getElementById('node-2-seconds')";

    panel.click("#node-2-seconds").await;
    panel.run(&format!("{field}.select()")).await;
    panel.type_text("12.").await;
    app.wait_for_config("12 seconds", |config| {
        matches!(card(config, "hydrate").node(2).unwrap().kind, NodeKind::Cooldown { seconds, .. } if seconds == 12.0)
    })
    .await;
    assert_eq!(panel.eval::<String>(&format!("{field}.value")).await, "12.", "the dot stays while typing");

    panel.type_text("5").await;
    app.wait_for_config("12.5 seconds", |config| {
        matches!(card(config, "hydrate").node(2).unwrap().kind, NodeKind::Cooldown { seconds, .. } if seconds == 12.5)
    })
    .await;

    // Delete in a field edits the text, not the nodes.
    panel.key(Key::DELETE, modifiers::NONE).await;
    assert_eq!(panel.count(".node").await, 3);
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn copy_and_paste_between_cards() {
    let app = App::start("copy_and_paste_between_cards", test_config()).await;
    let panel = open_card(&app, "hydrate", 3).await;

    panel.key(Key::A, modifiers::CTRL).await;
    panel.wait("all selected", "document.querySelectorAll('.node.selected').length === 3").await;
    let copied = panel.copy().await;
    assert!(copied.contains("\"lsk\""), "{copied}");

    panel.go("card/raid-alert").await;
    panel.wait("the raid card", "document.querySelectorAll('.node').length === 2").await;
    let spot = empty_spot(&panel).await;
    panel.hover(spot).await;
    panel.paste(&copied).await;
    let config =
        app.wait_for_config("pasted nodes", |config| card(config, "raid-alert").nodes.len() == 5).await;
    let raid = card(&config, "raid-alert");
    let ids: Vec<u32> = raid.nodes.iter().map(|node| node.id).collect();
    assert_eq!(ids, [1, 2, 3, 4, 5], "pasted nodes get new ids");
    assert!(raid.wires.contains(&wire((3, "event"), (4, "event"))), "wires between pasted nodes come along");
    assert_eq!(raid.wires.len(), 3);
    assert_eq!(panel.count(".node.selected").await, 3, "the pasted nodes are selected");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn test_this_card_reaches_the_overlay() {
    let app = App::start("test_this_card_reaches_the_overlay", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = open_card(&app, "raid-alert", 2).await;

    panel.click("#card-test").await;
    let text = overlay.next_alert().await;
    assert_eq!(text, "TestViewer is raiding with 25 viewers");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn new_card_from_a_preset() {
    let app = App::start("new_card_from_a_preset", test_config()).await;
    let panel = Panel::open(&app, "new").await;

    // Empty card on its own, then the groups; the alerts most streamers
    // want first.
    let groups: String = panel
        .eval("[...document.querySelectorAll('.preset-group')].map(h => h.textContent).join(' | ')")
        .await;
    assert_eq!(groups, "Alerts | Emote rain | More");
    let alerts: String = panel
        .eval(
            "[...document.querySelectorAll('.preset-box')[1].querySelectorAll('.preset-title')]             .slice(0, 4).map(t => t.textContent).join(' | ')",
        )
        .await;
    assert_eq!(alerts, "Sub alert | Bits alert | Channel point alert | Raid alert");
    // It takes the preset's name (a Raid alert card is there already).
    panel.run("[...document.querySelectorAll('.preset')].find(p => p.textContent.includes('Raid alert')).click()").await;
    panel.wait("the new card's editor", "location.hash === '#/card/raid-alert-2'").await;
    // Its nodes are known in the editor straight away.
    panel
        .wait(
            "its alert node",
            "[...document.querySelectorAll('.node')].some(n => n.innerText.includes('Alert'))",
        )
        .await;
    let text: String = panel.eval("document.body.innerText").await;
    assert!(!text.contains("isn't installed"), "{text}");
    let config = app
        .wait_for_config("the new card", |config| config.cards.iter().any(|card| card.id == "raid-alert-2"))
        .await;
    let new = card(&config, "raid-alert-2");
    assert_eq!(new.name, "Raid alert");
    assert!(new.enabled);
    assert!(problems(new).is_empty(), "{:?}", problems(new));
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_fresh_start_has_no_cards_until_one_is_made() {
    // A fresh config's cards (none), and nothing that needs the network.
    let config = AppConfig { cards: AppConfig::default().cards, ..test_config() };
    let app = App::start("a_fresh_start_has_no_cards_until_one_is_made", config).await;
    let panel = Panel::open(&app, "cards").await;
    panel.wait("the empty list's note", "document.querySelector('.card-empty')").await;
    assert!(app.saved_config().cards.is_empty(), "the app made no cards");

    // Chat's emote rain, which a fresh install used to start with, is a
    // preset.
    panel.click("#card-new").await;
    panel.wait("the presets", "location.hash === '#/new'").await;
    panel
        .run("[...document.querySelectorAll('.preset')].find(p => p.textContent.includes('Emote rain from chat')).click()")
        .await;
    panel.wait("the new card's editor", "location.hash === '#/card/emote-rain'").await;
    let config = app.wait_for_config("the new card", |config| config.cards.len() == 1).await;
    let rain = card(&config, "emote-rain");
    assert_eq!(rain.name, "Emote rain");
    assert!(rain.enabled);
    assert!(problems(rain).is_empty(), "{:?}", problems(rain));
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn an_unchanged_empty_new_card_goes_when_left() {
    let app = App::start("an_unchanged_empty_new_card_goes_when_left", test_config()).await;
    let panel = Panel::open(&app, "new").await;
    let has = |id: &'static str| move |config: &AppConfig| config.cards.iter().any(|card| card.id == id);
    let made = has("new-card");
    let pick = |text: &str| {
        format!(
            "[...document.querySelectorAll('.preset')].find(p => p.textContent.includes('{text}')).click()"
        )
    };

    // Empty, then backed out of with nothing changed: gone.
    panel.run(&pick("Empty card")).await;
    panel.wait("the new card's editor", "location.hash === '#/card/new-card'").await;
    app.wait_for_config("the new card", made).await;
    // The tabs stay above the editor, Cards current; leaving by them counts too.
    let current: String = panel.eval("document.querySelector('.dock-header .tab.current').textContent").await;
    assert_eq!(current, "Cards");
    panel.click(".dock-header .tab[href='#/cards']").await;
    panel.wait("the cards", "location.hash === '#/cards'").await;
    app.wait_for_config("the new card gone", |config| !made(config)).await;
    assert_eq!(panel.count("li[data-card='new-card']").await, 0, "not in the list");

    // Empty and changed (switched off): it stays.
    panel.run("location.hash = '#/new'").await;
    panel.wait("the presets", "document.querySelector('.preset')").await;
    panel.run(&pick("Empty card")).await;
    panel.wait("the new card's editor", "location.hash === '#/card/new-card'").await;
    panel.click(".editor-bar .switch").await;
    app.wait_for_config("the new card, off", |config| made(config) && !card(config, "new-card").enabled)
        .await;
    panel.click(".editor-bar .back").await;
    panel.wait("the cards", "location.hash === '#/cards'").await;
    tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
    assert!(made(&app.saved_config()));

    // From a preset, left unchanged: it stays.
    panel.run("location.hash = '#/new'").await;
    panel.wait("the presets", "document.querySelector('.preset')").await;
    panel.run(&pick("Raid alert")).await;
    panel.wait("the new card's editor", "location.hash === '#/card/raid-alert-2'").await;
    app.wait_for_config("the preset card", has("raid-alert-2")).await;
    panel.click(".editor-bar .back").await;
    panel.wait("the cards", "location.hash === '#/cards'").await;
    tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
    assert!(has("raid-alert-2")(&app.saved_config()));
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn edits_made_offline_are_saved_on_reconnect() {
    let mut app = App::start("edits_made_offline_are_saved_on_reconnect", test_config()).await;
    let panel = Panel::open(&app, "cards").await;

    app.stop().await;
    panel.wait("the reconnecting banner", "document.querySelector('.banner.warn')").await;
    panel.click("#card-switch-hydrate").await;
    // Past the save delay, so the save comes due while offline.
    tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
    assert!(!card(&app.saved_config(), "hydrate").enabled);

    app.restart().await;
    app.wait_for_config("the offline edit saved", |config| card(config, "hydrate").enabled).await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_server_names_its_panel_build_first() {
    use futures_util::StreamExt;
    use protocol::panel::{PANEL_WS_PATH, PanelEvent};

    let app = App::start("the_server_names_its_panel_build_first", test_config()).await;
    let url = format!("ws://127.0.0.1:{}{PANEL_WS_PATH}", app.port);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
    let first = socket.next().await.expect("a message").expect("no error");
    let event: PanelEvent = serde_json::from_str(first.to_text().expect("text")).expect("a panel event");
    let PanelEvent::Build { id } = event else { panic!("expected the build first, got {event:?}") };
    assert_eq!(id.len(), 16, "eight bytes of index.html's hash, in hex: {id}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_panel_reloads_for_a_new_build() {
    use protocol::panel::PanelEvent;

    let app = App::start("the_panel_reloads_for_a_new_build", test_config()).await;
    let panel = Panel::open(&app, "cards").await;
    panel.run("window.__beforeReload = true").await;

    // The page runs the build the server named when it connected; this
    // one differs.
    panel.receive(&PanelEvent::Build { id: "0000000000000000".into() }).await;
    panel
        .wait("the page reloaded", "window.__beforeReload === undefined && document.querySelector('.dock')")
        .await;
    assert_eq!(panel.eval::<String>("location.hash").await, "#/cards", "it comes back on the same page");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn fields_keep_focus_when_rows_and_problems_change() {
    let app = App::start("fields_keep_focus_when_rows_and_problems_change", test_config()).await;
    let panel = open_card(&app, "rain-on-bits", 4).await;
    let focused = "document.activeElement?.id";

    // Picking "These emotes" adds the codes row (and a problem: no codes
    // yet); the dropdown keeps the focus.
    panel.click("#node-4-emotes").await;
    panel.key(Key::ESCAPE, modifiers::NONE).await;
    panel
        .run("const select = document.getElementById('node-4-emotes'); select.value = 'codes'; select.dispatchEvent(new Event('change', { bubbles: true }))")
        .await;
    panel.wait("the codes row", "document.getElementById('node-4-plugin-emotes-codes')").await;
    assert_eq!(panel.eval::<String>(focused).await, "node-4-emotes");

    // The first letter clears the problem; typing goes on in the same field.
    panel.click("#node-4-plugin-emotes-codes").await;
    panel.type_text("K").await;
    panel.wait("the problem gone", "!document.querySelector(\".node[data-node='4'] .node-problem\")").await;
    assert_eq!(
        panel.eval::<String>(focused).await,
        "node-4-plugin-emotes-codes",
        "the field keeps the focus"
    );
    panel.type_text("appa PogU").await;
    app.wait_for_config("the codes", |config| {
        matches!(
            &card(config, "rain-on-bits").node(4).unwrap().kind,
            NodeKind::Plugin { settings, .. } if settings.get("emotes") == Some(&serde_json::json!(["Kappa", "PogU"]))
        )
    })
    .await;
    assert_eq!(panel.eval::<String>(focused).await, "node-4-plugin-emotes-codes");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn emote_codes_are_completed() {
    use protocol::panel::{KnownEmote, PanelEvent};

    let app = App::start("emote_codes_are_completed", test_config()).await;
    let panel = open_card(&app, "rain-on-bits", 4).await;
    panel
        .run("const select = document.getElementById('node-4-emotes'); select.value = 'codes'; select.dispatchEvent(new Event('change', { bubbles: true }))")
        .await;
    panel.wait("the codes row", "document.getElementById('node-4-plugin-emotes-codes')").await;

    // Focusing the field asks the server for the channel's emotes. The test
    // server has none, so once it has answered, hand the panel some.
    panel.click("#node-4-plugin-emotes-codes").await;
    panel.wait("the server's emote list", "window.__panelReceived.includes('emotes')").await;
    let emote = |code: &str| KnownEmote { code: code.into(), source: "7tv".into(), id: code.to_lowercase() };
    panel
        .receive(&PanelEvent::Emotes {
            emotes: vec![emote("KEKW"), emote("Pog"), emote("PogU"), emote("catPog")],
        })
        .await;

    let shown = "[...document.querySelectorAll('.completion .code')].map(code => code.textContent)";
    panel.type_text("po").await;
    panel.wait("completions", "document.querySelectorAll('.completion').length === 3").await;
    assert_eq!(panel.eval::<Vec<String>>(shown).await, ["Pog", "PogU", "catPog"]);

    // Keyboard: down to PogU, Enter.
    panel.key(Key::ARROW_DOWN, modifiers::NONE).await;
    panel.key(Key::ENTER, modifiers::NONE).await;
    let field = "document.getElementById('node-4-plugin-emotes-codes')";
    assert_eq!(panel.eval::<String>(&format!("{field}.value")).await, "PogU, ");
    assert_eq!(panel.count(".completion").await, 0);

    // Mouse: pick KEKW from the list.
    panel.type_text("ke").await;
    panel.wait("KEKW offered", "document.querySelectorAll('.completion').length === 1").await;
    panel.click(".completion").await;
    assert_eq!(panel.eval::<String>(&format!("{field}.value")).await, "PogU, KEKW, ");
    assert_eq!(
        panel.eval::<String>("document.activeElement?.id").await,
        "node-4-plugin-emotes-codes",
        "the field keeps the focus"
    );
    app.wait_for_config("both codes", |config| {
        matches!(
            &card(config, "rain-on-bits").node(4).unwrap().kind,
            NodeKind::Plugin { settings, .. } if settings.get("emotes") == Some(&serde_json::json!(["PogU", "KEKW"]))
        )
    })
    .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_dock_menu_duplicates_and_deletes() {
    let app = App::start("the_dock_menu_duplicates_and_deletes", test_config()).await;
    let panel = open_card(&app, "raid-alert", 2).await;
    // As narrow as an OBS dock: Duplicate and Delete are in the ⋯ menu
    // (at every width since 2026-10-15), and the bar fits.
    panel.resize(420, 800).await;
    panel.wait("the menu button", "document.querySelector('.bar-menu').offsetParent !== null").await;
    assert_eq!(panel.count(".bar-menu-list").await, 0);
    let fits: bool = panel
        .eval(
            "(() => { const bar = document.querySelector('.editor-bar');              return bar.scrollWidth <= bar.clientWidth              && document.getElementById('card-test').getBoundingClientRect().right <= innerWidth; })()",
        )
        .await;
    assert!(fits, "the bar fits, ▶ Test in view");

    panel.click(".bar-menu .icon-button").await;
    panel.wait("the menu", "document.querySelector('.bar-menu-list')").await;
    panel.run("[...document.querySelectorAll('.bar-menu-list .menu-item')].find(b => b.textContent === 'Duplicate').id = 'menu-duplicate'")
        .await;
    panel.click("#menu-duplicate").await;
    panel.wait("the copy's editor", "location.hash === '#/card/raid-alert-copy'").await;
    app.wait_for_config("the copy", |config| config.cards.iter().any(|card| card.id == "raid-alert-copy"))
        .await;
    assert_eq!(panel.count(".bar-menu-list").await, 0, "the menu closes");

    // A click beside the menu closes it.
    panel.click(".bar-menu .icon-button").await;
    panel.wait("the menu", "document.querySelector('.bar-menu-list')").await;
    panel.click_at((200.0, 600.0), 0).await;
    panel.wait("the menu to close", "!document.querySelector('.bar-menu-list')").await;

    panel.run("window.confirm = () => true").await;
    panel.click(".bar-menu .icon-button").await;
    panel.wait("the menu", "document.querySelector('.bar-menu-list')").await;
    panel.click(".bar-menu-list .danger").await;
    panel.wait("the card list", "location.hash === '#/cards'").await;
    app.wait_for_config("the copy deleted", |config| {
        config.cards.iter().all(|card| card.id != "raid-alert-copy")
    })
    .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_chant_card_tests_with_a_crowd() {
    let app = App::start("a_chant_card_tests_with_a_crowd", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "new").await;

    panel
        .run("[...document.querySelectorAll('.preset')].find(p => p.textContent.includes('Chant')).click()")
        .await;
    panel.wait("the chant card", "document.querySelectorAll('.node').length === 2").await;
    app.wait_for_config("the chant card", |config| config.cards.iter().any(|card| card.id == "chant")).await;

    // "Test this card" sends "gg!" from as many viewers as the chant needs.
    panel.click("#card-test").await;
    assert_eq!(overlay.next_alert().await, "5 viewers chant gg");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn rarity_tiers_are_added_edited_and_removed() {
    let app = App::start("rarity_tiers_are_added_edited_and_removed", test_config()).await;
    let panel = Panel::open(&app, "new").await;
    let tiers = |config: &AppConfig| match &card(config, "rarity-alerts").node(2).unwrap().kind {
        NodeKind::Rarity { tiers, .. } => {
            tiers.iter().map(|tier| (tier.name.clone(), tier.percent)).collect()
        }
        _ => Vec::new(),
    };

    // The preset: a reward → rarity (Legendary 1, Epic 10, Rare 20, Common) → 4 alerts.
    panel
        .run("[...document.querySelectorAll('.preset')].find(p => p.textContent.includes('Rarity')).click()")
        .await;
    panel.wait("the rarity card", "document.querySelectorAll('.node').length === 6").await;
    app.wait_for_config("the rarity card", |config| {
        config.cards.iter().any(|card| card.id == "rarity-alerts")
    })
    .await;

    panel.click(".add-tier").await;
    app.wait_for_config("a fourth tier", |config| tiers(config).len() == 4).await;
    assert_eq!(tiers(&app.saved_config())[3], ("Tier 4".to_string(), 5.0));

    panel.click("#node-2-tier-2-percent").await;
    panel.run("document.getElementById('node-2-tier-2-percent').select()").await;
    panel.type_text("50").await;
    app.wait_for_config("epic at 50 %", |config| tiers(config).get(1) == Some(&("Epic".to_string(), 50.0)))
        .await;
    panel.wait("what's left", "document.querySelector('.tier-left').textContent === '24 %'").await;

    // Removing a tier takes its wire with it.
    panel.click(".tier-remove[aria-label='Remove Legendary']").await;
    let config = app.wait_for_config("no legendary", |config| tiers(config).len() == 3).await;
    assert!(card(&config, "rarity-alerts").wires.iter().all(|wire| wire.from.socket != "tier-1"));
    panel.wait("what's left", "document.querySelector('.tier-left').textContent === '25 %'").await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_position_is_placed_on_its_small_screen() {
    let mut config = test_config();
    config.cards.push(Card {
        id: "show".into(),
        name: "Show".into(),
        enabled: true,
        nodes: vec![plugin_node(1, "media", "show", serde_json::json!({ "position": "center" }))],
        wires: Vec::new(),
    });
    let app = App::start("a_position_is_placed_on_its_small_screen", config).await;
    let position = |config: &AppConfig| config.plugin("alert").presets()[0].settings.get("position").cloned();
    let x_of = |config: &AppConfig| position(config).and_then(|value| value["x"].as_f64());

    // Plugins → Alerts: the default style's Position.
    let panel = Panel::open(&app, "plugins/alert").await;
    let screen = "#plugin-alert-setting-position";
    panel.wait("the screen", &format!("document.querySelector('{screen}')")).await;
    // A dot (the 9th: bottom right): its name, as before.
    panel.click(&format!("{screen} .position-dot:nth-child(9)")).await;
    app.wait_for_config("bottom right", |config| position(config) == Some("bottom_right".into())).await;

    // A press between the dots: there, as x and y in % (the room is the
    // screen less 3.5 % of its width each side).
    let spot: (f64, f64) = panel
        .eval(&format!(
            "(() => {{ const r = document.querySelector('{screen}').getBoundingClientRect(); \
               return [r.left + r.width * (3.5 + 70 * 0.93) / 100, \
                       r.top + r.height * (3.5 * 16 / 9 + 20 * (1 - 0.07 * 16 / 9)) / 100]; }})()"
        ))
        .await;
    panel.click_at(spot, modifiers::NONE).await;
    app.wait_for_config("70 %, 20 %", |config| {
        position(config) == Some(serde_json::json!({ "x": 70, "y": 20 }))
    })
    .await;

    // Dragged by its box: it keeps where it was held, 24 px of the 223 px
    // room further right.
    let from = panel.center(&format!("{screen} .position-box")).await;
    panel.drag(from, (from.0 + 24.0, from.1), modifiers::NONE).await;
    let config = app.wait_for_config("further right", |config| x_of(config).is_some_and(|x| x > 75.0)).await;
    let dragged = position(&config).unwrap();
    let x = x_of(&config).unwrap();
    assert!((79.0..=83.0).contains(&x) && dragged["y"] == 20, "{dragged}");

    // Shift+← moves it 10 %; the numbers say it exactly.
    panel.key(Key::ARROW_LEFT, modifiers::SHIFT).await;
    app.wait_for_config("10 % left", |config| x_of(config) == Some(x - 10.0)).await;
    panel.click(&format!("{screen}-x")).await;
    panel.run(&format!("document.querySelector('{screen}-x').select()")).await;
    panel.type_text("33").await;
    app.wait_for_config("33 % from the left", |config| x_of(config) == Some(33.0)).await;
    panel
        .wait(
            "the box there, with its point",
            &format!(
                "document.querySelector('{screen}').getAttribute('aria-valuetext') === '33 %, 20 %' \
                      && document.querySelector('{screen} .position-anchor')"
            ),
        )
        .await;
    panel.close().await;

    // In a node: a field that opens the control. Show media's Where can be
    // anywhere (random).
    let panel = open_card(&app, "show", 1).await;
    let where_ = |config: &AppConfig| match &card(config, "show").nodes[0].kind {
        NodeKind::Plugin { settings, .. } => settings.get("position").cloned(),
        _ => None,
    };
    let field = "#node-1-plugin-position";
    panel.wait("Where", &format!("document.querySelector('{field}').textContent === 'Middle'")).await;
    panel.click(field).await;
    panel.wait("the control", &format!("document.querySelector('{field}-pick')")).await;
    panel.click(&format!("{field}-pick .position-dot:nth-child(1)")).await;
    app.wait_for_config("top left", |config| where_(config) == Some("top_left".into())).await;
    panel.click(&format!("{field}-pick-random")).await;
    app.wait_for_config("anywhere", |config| where_(config) == Some("random".into())).await;
    panel
        .wait("the numbers off", &format!("document.querySelector('{field}-pick-x').matches(':disabled')"))
        .await;
    // A press outside closes it.
    panel.click_at((5.0, 5.0), modifiers::NONE).await;
    panel
        .wait(
            "closed, saying so",
            &format!("!document.querySelector('{field}-pick') && document.querySelector('{field}').textContent === 'Anywhere'"),
        )
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn alert_styles_reach_the_overlay() {
    let app = App::start("alert_styles_reach_the_overlay", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    let styles = |config: &AppConfig| config.plugin("alert").presets();

    // Plugins → Alerts: its styles.
    let panel = Panel::open(&app, "plugins/alert").await;
    panel.wait("the styles", "document.getElementById('plugin-alert-preset')").await;
    panel.wait("its preview", "document.getElementById('plugin-alert-preview')").await;
    panel.click("#plugin-alert-preset-duplicate").await;
    app.wait_for_config("a second style", |config| styles(config).len() == 2).await;
    panel
        .wait(
            "the copy picked",
            "document.getElementById('plugin-alert-preset-name').value === 'Default copy'",
        )
        .await;
    panel.click("#plugin-alert-setting-position .position-dot:nth-child(7)").await;
    app.wait_for_config("bottom left", |config| {
        styles(config)[1].settings.get("position") == Some(&"bottom_left".into())
    })
    .await;
    assert!(styles(&app.saved_config())[0].settings.is_empty(), "the copy changed, not the default");

    // A picture, uploaded as if picked in the file dialog. Only the start of
    // the file has to look like a picture.
    let picture = app.dir.join("Big hype!.png");
    std::fs::write(&picture, b"\x89PNG\r\n\x1a\nnot really a picture").unwrap();
    panel.choose_file("#plugin-alert-setting-picture-upload", &picture).await;
    app.wait_for_config("the picture", |config| {
        styles(config)[1].settings.get("picture") == Some(&"Big hype_.png".into())
    })
    .await;
    assert!(app.dir.join("pictures").join("Big hype_.png").exists());
    let served: u16 = panel.eval("fetch('/pictures/Big%20hype_.png').then(r => r.status)").await;
    assert_eq!(served, 200);
    let without_header: u16 = panel
        .eval("fetch('/panel/pictures/x.gif', { method: 'POST', body: 'GIF89a' }).then(r => r.status)")
        .await;
    assert_eq!(without_header, 403, "uploads need the panel's header");

    // Show on overlay: a sample alert in the picked style.
    panel.click("#plugin-alert-show").await;
    let settings = overlay
        .wait_for("the preview", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { settings, .. } => Some(settings.clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(
        (&settings["position"], &settings["picture"]),
        (&"bottom_left".into(), &protocol::alert::picture_path("Big hype_.png").into())
    );

    // An alert node picks the style; its alerts come in it.
    let copy = styles(&app.saved_config())[1].id.clone();
    panel.go("card/raid-alert").await;
    panel.wait("the alert node", "document.getElementById('node-2-plugin-style')").await;
    panel
        .run(&format!(
            "const s = document.getElementById('node-2-plugin-style'); s.value = '{copy}'; \
              s.dispatchEvent(new Event('change', {{ bubbles: true }}))"
        ))
        .await;
    app.wait_for_config("the node's style", |config| {
        matches!(&card(config, "raid-alert").node(2).unwrap().kind,
            NodeKind::Plugin { settings, .. } if settings.get("style") == Some(&copy.clone().into()))
    })
    .await;
    panel.click("#card-test").await;
    let settings = overlay
        .wait_for("the raid alert", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { values, settings, .. }
                    if values["message"]["text"].as_str().is_some_and(|text| text.contains("raiding")) =>
                {
                    Some(settings.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(settings["position"], "bottom_left");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_alerts_box_lines_up_and_previews_what_is_edited() {
    let app = App::start("the_alerts_box_lines_up_and_previews_what_is_edited", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    let styles = |config: &AppConfig| config.plugin("alert").presets();
    let first = |config: &AppConfig, key: &str| styles(config)[0].settings.get(key).cloned();
    let panel = Panel::open(&app, "plugins/alert").await;
    panel.resize(760, 1400).await;
    panel.wait("the box", "document.getElementById('plugin-alert-setting-font')").await;

    // No description, nodes or hosts: the style row comes first.
    let said: String = panel.eval("document.getElementById('plugin-alert-body').innerText").await;
    assert!(
        !said.contains("A message on the overlay")
            && !said.contains("Nodes:")
            && !said.contains("may load from"),
        "{said}"
    );

    // Every row's label starts on one line, and every field on another.
    let (labels, fields): (Vec<f64>, Vec<f64>) = panel
        .eval(
            "(() => { const rows = [...document.querySelectorAll('#plugin-alert .setting-row')]; \
              return [rows.map(row => row.querySelector('.setting-label').getBoundingClientRect().left), \
                      rows.map(row => row.querySelector('select, input, .position-screen, .choice-buttons') \
                                         .getBoundingClientRect().left)]; })()",
        )
        .await;
    let spread = |lefts: &[f64]| {
        lefts.iter().copied().fold(f64::MIN, f64::max) - lefts.iter().copied().fold(f64::MAX, f64::min)
    };
    assert!(labels.len() >= 10, "{labels:?}");
    assert!(spread(&labels) < 1.0 && spread(&fields) < 1.0, "labels {labels:?}, fields {fields:?}");
    assert!(fields[0] - labels[0] > 100.0, "the fields are beside the labels: {labels:?} {fields:?}");
    // A colour picker's colour goes up to its edge, as the other fields' do.
    let padding: String = panel
        .eval("getComputedStyle(document.getElementById('plugin-alert-setting-text_color')).paddingLeft")
        .await;
    assert_eq!(padding, "0px");
    // Under Text: "Size", not "Text size"; emotes as "Emotes", "As pictures".
    let (size, emotes): (String, String) = panel
        .eval(
            "[document.querySelector('label[for=plugin-alert-setting-font_size_px]').textContent, \
              document.getElementById('plugin-alert-setting-emotes').closest('.setting-row').innerText]",
        )
        .await;
    assert_eq!(size, "Size");
    assert_eq!(emotes.split_whitespace().collect::<Vec<_>>(), ["Emotes", "As", "pictures"]);

    // The font: Segoe UI, without an empty choice; B and I beside it.
    let (fonts, font): (Vec<String>, String) = panel
        .eval(
            "(() => { const pick = document.getElementById('plugin-alert-setting-font'); \
              return [[...pick.options].map(o => o.text), pick.selectedOptions[0].text]; })()",
        )
        .await;
    assert_eq!(font, "Segoe UI");
    assert!(!fonts.iter().any(|font| font == "The system's" || font == "None"), "{fonts:?}");
    let pressed: Vec<String> = panel
        .eval(
            "[...document.querySelectorAll('#plugin-alert .with-toggles .toggle')] \
             .map(button => button.id + ' ' + button.getAttribute('aria-pressed'))",
        )
        .await;
    assert_eq!(pressed, ["plugin-alert-setting-bold true", "plugin-alert-setting-italic false"]);
    panel.click("#plugin-alert-setting-italic").await;
    app.wait_for_config("italic", |config| first(config, "italic") == Some(true.into())).await;

    // The picture's size and placement only once one is picked, and the
    // sound's volume once a sound is.
    let hidden = "#plugin-alert-setting-picture_height_px, #plugin-alert-setting-picture_placement, \
                  #plugin-alert-setting-sound_volume";
    assert_eq!(panel.count(hidden).await, 0);
    let picture = app.dir.join("hype.png");
    std::fs::write(&picture, b"\x89PNG\r\n\x1a\nnot really a picture").unwrap();
    panel.choose_file("#plugin-alert-setting-picture-upload", &picture).await;
    app.wait_for_config("the picture", |config| first(config, "picture") == Some("hype.png".into())).await;
    panel
        .wait(
            "its size and placement",
            "document.getElementById('plugin-alert-setting-picture_height_px') \
             && document.getElementById('plugin-alert-setting-picture_placement-left')",
        )
        .await;
    panel.click("#plugin-alert-setting-picture_placement-left").await;
    app.wait_for_config("left of the text", |config| {
        first(config, "picture_placement") == Some("left".into())
    })
    .await;
    // Four short ones, in one row (2026-10-15).
    let words: Vec<String> = panel
        .eval("[...document.querySelectorAll('#plugin-alert-setting-picture_placement button')].map(b => b.textContent)")
        .await;
    assert_eq!(words, ["Above", "Below", "Left", "Right"]);
    panel.click("#plugin-alert-setting-picture_placement-below").await;
    app.wait_for_config("below the text", |config| {
        first(config, "picture_placement") == Some("below".into())
    })
    .await;
    assert_eq!(panel.count("#plugin-alert-setting-sound_volume").await, 0);

    // The preview shows the sample alert without a Play, in the style as
    // it's edited: red text once the text's colour is red.
    panel
        .run(
            "const colour = document.getElementById('plugin-alert-setting-text_color'); colour.value = '#ff0000'; \
             colour.dispatchEvent(new InputEvent('input', { bubbles: true }));",
        )
        .await;
    app.wait_for_config("red text", |config| first(config, "text_color") == Some("#ff0000".into())).await;
    panel.run("document.getElementById('plugin-alert-preview').scrollIntoView({ block: 'center' })").await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let path = app.dir.join("preview.png");
    let png = panel.screenshot(&path).await;
    let red: u32 = panel
        .eval(&format!(
            "(async () => {{ const box = document.getElementById('plugin-alert-preview').parentElement \
             .getBoundingClientRect(); const ratio = devicePixelRatio; const image = new Image(); \
             image.src = 'data:image/png;base64,{png}'; await image.decode(); \
             const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height; \
             const context = canvas.getContext('2d'); context.drawImage(image, 0, 0); \
             const d = context.getImageData(box.left * ratio, box.top * ratio, box.width * ratio, box.height * ratio).data; \
             let red = 0; for (let i = 0; i < d.length; i += 4) if (d[i] > 180 && d[i + 1] < 90 && d[i + 2] < 90) red++; \
             return red; }})()"
        ))
        .await;
    assert!(red > 100, "only {red} red pixels in the preview; see {}", path.display());

    // Show on overlay: the style, italic, without the preview's `preview`.
    panel.click("#plugin-alert-show").await;
    let (settings, preview) = overlay
        .wait_for("the sample alert", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { settings, preview, .. } => Some((settings.clone(), *preview)),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(
        (&settings["italic"], &settings["text_color"], preview),
        (&true.into(), &"#ff0000".into(), None)
    );

    // The style row: Duplicate opens the copy's name to type over; Enter
    // keeps it. ⋯ → Delete this style takes it out again.
    panel.click("#plugin-alert-preset-duplicate").await;
    panel
        .wait(
            "the name open",
            "document.activeElement === document.getElementById('plugin-alert-preset-name')",
        )
        .await;
    panel.type_text("Big raid").await;
    panel.key(Key::ENTER, modifiers::NONE).await;
    app.wait_for_config("renamed", |config| {
        styles(config).get(1).is_some_and(|style| style.name == "Big raid")
    })
    .await;
    panel
        .wait(
            "the picker back, on the copy",
            "document.getElementById('plugin-alert-preset')?.selectedOptions[0].text === 'Big raid'",
        )
        .await;
    panel.click("#plugin-alert-preset-more").await;
    panel.click("#plugin-alert-preset-delete").await;
    app.wait_for_config("deleted", |config| styles(config).len() == 1).await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn an_alert_node_changes_some_of_its_style() {
    let app = App::start("an_alert_node_changes_some_of_its_style", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = open_card(&app, "raid-alert", 2).await;
    let changes = |config: &AppConfig| match &card(config, "raid-alert").node(2).unwrap().kind {
        NodeKind::Plugin { changes, .. } => changes.clone(),
        _ => Default::default(),
    };
    let pick = |key: &str| {
        format!(
            "const s = document.getElementById('node-2-add-change'); s.value = '{key}'; \
             s.dispatchEvent(new Event('change', {{ bubbles: true }}))"
        )
    };

    // Each starts at the style's value.
    panel.run(&pick("position")).await;
    panel.wait("the position row", "document.getElementById('node-2-change-position')").await;
    panel.run(&pick("font_size_px")).await;
    app.wait_for_config("two changes", |config| changes(config).len() == 2).await;
    assert_eq!(changes(&app.saved_config())["position"], "top");

    panel.click("#node-2-change-position").await;
    panel.click("#node-2-change-position-pick .position-dot:nth-child(9)").await;
    panel.key(Key::ESCAPE, modifiers::NONE).await;
    panel
        .wait(
            "closed, saying where",
            "!document.getElementById('node-2-change-position-pick') \
             && document.getElementById('node-2-change-position').textContent === 'Bottom right'",
        )
        .await;
    panel.click("#node-2-change-font_size_px").await;
    panel.run("document.getElementById('node-2-change-font_size_px').select()").await;
    panel.type_text("90").await;
    app.wait_for_config("bottom right, size 90", |config| {
        let changes = changes(config);
        changes.get("position").and_then(|v| v.as_str()) == Some("bottom_right")
            && changes.get("font_size_px").and_then(|v| v.as_f64()) == Some(90.0)
    })
    .await;

    // The alert: the default style, changed in those two ways.
    panel.click("#card-test").await;
    let settings = overlay
        .wait_for("the raid alert", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { settings, .. } => Some(settings.clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!((&settings["position"], &settings["font_size_px"]), (&"bottom_right".into(), &90.0.into()));
    assert_eq!(settings["text_color"], "#ffffff");

    // × goes back to the style's value.
    panel.click("[data-node='2'] .override button[aria-label='Stop changing Position']").await;
    app.wait_for_config("position back", |config| !changes(config).contains_key("position")).await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn confetti_waits_for_its_alert() {
    // A custom event → an alert; the confetti isn't wired yet.
    let mut config = test_config();
    config.cards.push(Card {
        id: "party".into(),
        name: "Party".into(),
        enabled: true,
        nodes: vec![
            protocol::cards::Node { id: 1, x: 40.0, y: 60.0, kind: NodeKind::Custom { name: String::new() } },
            plugin_node(2, "alert", "alert", serde_json::json!({ "message": "Welcome!" })),
            protocol::cards::Node { x: 600.0, ..plugin_node(3, "particles", "burst", serde_json::json!({})) },
        ],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    });
    let app = App::start("confetti_waits_for_its_alert", config).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = open_card(&app, "party", 3).await;

    // The alert waits in the queue, so it has Started and Ended; wire
    // Started into the confetti.
    panel.wait("its Ended", &format!("document.querySelector(\"{}\")", select::output(2, "ended"))).await;
    let from = panel.center(&select::output(2, "started")).await;
    let to = panel.center(&select::input(3, "trigger")).await;
    panel.drag(from, to, modifiers::NONE).await;
    let started = wire((2, "started"), (3, "trigger"));
    app.wait_for_config("Started wired", |config| card(config, "party").wires.contains(&started)).await;

    // The confetti comes once the alert is on stage, not before.
    panel.click("#card-test").await;
    overlay
        .wait_for("the alert", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { queue_id: Some(_), .. } => Some(()),
                _ => None,
            },
            Command::Plugin(command) if command.plugin == "particles" => panic!("the confetti came first"),
            _ => None,
        })
        .await;
    overlay
        .wait_for("the confetti", |command| match command {
            Command::Plugin(command) if command.plugin == "particles" => match &command.message {
                PluginMessage::Run { .. } => Some(()),
                _ => None,
            },
            _ => None,
        })
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn an_alert_picture_from_a_folder() {
    let app = App::start("an_alert_picture_from_a_folder", test_config()).await;
    let folder = app.dir.join("pictures").join("alerts");
    std::fs::create_dir_all(&folder).unwrap();
    for name in ["a.png", "b.png", "clip.webm"] {
        std::fs::write(folder.join(name), b"\x89PNG\r\n\x1a\nnot really a picture").unwrap();
    }
    let mut overlay = app.overlay(vec![]).await;
    let panel = open_card(&app, "raid-alert", 2).await;
    let changes = |config: &AppConfig| match &card(config, "raid-alert").node(2).unwrap().kind {
        NodeKind::Plugin { changes, .. } => changes.clone(),
        _ => Default::default(),
    };
    let set = |id: &str, value: &str| {
        format!(
            "const s = document.getElementById('{id}'); s.value = '{value}'; \
             s.dispatchEvent(new Event('change', {{ bubbles: true }}))"
        )
    };

    // The picture's list has the folder, with its pictures counted (not
    // the video).
    panel.run(&set("node-2-add-change", "picture")).await;
    panel.wait("the picture row", "document.getElementById('node-2-change-picture')").await;
    panel.run("document.getElementById('node-2-change-picture').focus()").await;
    panel
        .wait(
            "the folder",
            "[...document.querySelectorAll('#node-2-change-picture optgroup option')] \
             .some(o => o.value === 'alerts/' && o.textContent.trim() === 'alerts/ (2 pictures)')",
        )
        .await;
    panel.run(&set("node-2-change-picture", "alerts/")).await;
    app.wait_for_config("the folder picked", |config| {
        changes(config).get("picture") == Some(&"alerts/".into())
    })
    .await;

    // The alert: one of the folder's pictures, which the app serves.
    panel.click("#card-test").await;
    let settings = overlay
        .wait_for("the raid alert", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { settings, .. } => Some(settings.clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    let picture = settings["picture"].as_str().unwrap_or_default().to_string();
    let from_folder = ["alerts/a.png", "alerts/b.png"].map(protocol::alert::picture_path);
    assert!(from_folder.contains(&picture), "{picture}");
    let served: u16 = panel.eval(&format!("fetch('{picture}').then(r => r.status)")).await;
    assert_eq!(served, 200);
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn sounds_are_uploaded_and_played() {
    // A card: a custom event plays a sound at 50 %.
    let mut config = test_config();
    config.cards.push(Card {
        id: "horn".into(),
        name: "Horn".into(),
        enabled: true,
        nodes: vec![
            protocol::cards::Node { id: 1, x: 40.0, y: 60.0, kind: NodeKind::Custom { name: String::new() } },
            plugin_node(
                2,
                "sound",
                "play",
                serde_json::json!({ "sound": "", "volume": 50.0, "queue": false }),
            ),
        ],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    });
    let app = App::start("sounds_are_uploaded_and_played", config).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "plugins/sound").await;

    // Plugins → Sounds: upload, and it's listed and served.
    let file = app.dir.join("Air horn!.mp3");
    std::fs::write(&file, b"ID3\x04\0not really a sound").unwrap();
    panel.wait("the sounds", "document.getElementById('sound-upload')").await;
    panel.choose_file("#sound-upload", &file).await;
    panel
        .wait("the sound in the list", "document.getElementById('sound-file-0')?.value === 'Air horn_.mp3'")
        .await;
    assert!(app.dir.join("sounds").join("Air horn_.mp3").exists());
    let served: u16 = panel.eval("fetch('/sounds/Air%20horn_.mp3').then(r => r.status)").await;
    assert_eq!(served, 200);

    // A label, saved when the field is left; the file name goes under it.
    panel.click("#sound-file-0").await;
    panel.run("document.getElementById('sound-file-0').select()").await;
    panel.type_text("Horn").await;
    panel.run("document.activeElement.blur()").await;
    app.wait_for_config("the label", |config| {
        config.effects.sound.labels.get("Air horn_.mp3").map(String::as_str) == Some("Horn")
    })
    .await;
    panel
        .wait(
            "the file name under it",
            "document.querySelector('#sound-app .sound-file')?.textContent === 'Air horn_.mp3'",
        )
        .await;

    // An alert style with the sound (on the Alerts plugin's box): the
    // picker shows its label.
    panel.go("plugins/alert").await;
    panel.wait("the style's sound", "document.getElementById('plugin-alert-setting-sound')").await;
    panel
        .wait(
            "the label in the picker",
            "document.querySelector('#plugin-alert-setting-sound option[value=\"Air horn_.mp3\"]')?.textContent.trim() === 'Horn'",
        )
        .await;
    panel
        .run(
            "const s = document.getElementById('plugin-alert-setting-sound'); s.value = 'Air horn_.mp3'; \
              s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("the style's sound", |config| {
        config.plugin("alert").presets()[0].settings.get("sound") == Some(&"Air horn_.mp3".into())
    })
    .await;

    // A Sound node (the Sounds plugin's) with none picked: its card has a
    // problem. With it: "Test this card" sends it to the plugin's page.
    panel.go("card/horn").await;
    panel.wait("the sound node", "document.getElementById('node-2-plugin-sound')").await;
    panel.wait("its problem", "document.body.textContent.includes('Pick a sound.')").await;
    panel
        .run(
            "const s = document.getElementById('node-2-plugin-sound'); s.value = 'Air horn_.mp3'; \
              s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("the node's sound", |config| {
        matches!(&card(config, "horn").node(2).unwrap().kind,
            NodeKind::Plugin { settings, .. } if settings["sound"] == "Air horn_.mp3")
    })
    .await;
    panel.click("#card-test").await;
    let played = overlay
        .wait_for("the sound", |command| match command {
            Command::Plugin(command) if command.plugin == "sound" => match &command.message {
                PluginMessage::Run { values, queue_id: None, .. } => Some(values.clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(played["sound"], protocol::sound::sound_path("Air horn_.mp3"));
    assert_eq!(played["volume"], 50.0);

    // Deleting it asks once more; the file and its label go.
    panel.go("plugins/sound").await;
    panel.click("#sound-file-0-delete").await;
    panel
        .wait(
            "asked once more",
            "document.getElementById('sound-file-0-delete')?.textContent === 'Click again to delete'",
        )
        .await;
    assert!(app.dir.join("sounds").join("Air horn_.mp3").exists());
    panel.click("#sound-file-0-delete").await;
    panel
        .wait(
            "no sounds",
            "document.querySelector('#sound-app .sound-list')?.textContent.includes('No sounds yet.')",
        )
        .await;
    assert!(!app.dir.join("sounds").join("Air horn_.mp3").exists());
    app.wait_for_config("its label gone", |config| config.effects.sound.labels.is_empty()).await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn text_to_speech_settings_and_voices() {
    let app = App::start("text_to_speech_settings_and_voices", test_config()).await;
    let panel = Panel::open(&app, "plugins/speak").await;
    let speech = |config: &AppConfig| config.effects.speech.settings.clone();

    // On the Speak plugin's box, whose switch is every voice read aloud.
    // Not installed: the download is offered (not started here: it's big).
    panel.click("#plugin-speak-on").await;
    app.wait_for_config("reading aloud off", |config| !config.plugin("speak").enabled).await;
    panel
        .wait(
            "the download button",
            "document.getElementById('speech-install')?.textContent === 'Download (460 MB)'",
        )
        .await;

    panel
        .run(
            "const s = document.getElementById('speech-voice'); s.value = 'bf_emma'; \
              s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("Emma", |config| speech(config).voice == "bf_emma").await;
    panel.click("#speech-repeats").await;
    app.wait_for_config("repeats read", |config| !speech(config).collapse_repeats).await;
    panel.click("#speech-links").await;
    app.wait_for_config("links read", |config| !speech(config).skip_links).await;
    panel.click("#speech-max-chars").await;
    panel.run("document.getElementById('speech-max-chars').select()").await;
    panel.type_text("0").await;
    app.wait_for_config("no length limit", |config| speech(config).max_chars == 0).await;

    // Try's text is kept, saved when the field is left; emptied, it stays
    // empty, the default its placeholder.
    let try_text: String = panel.eval("document.getElementById('speech-sample').value").await;
    assert_eq!(try_text, protocol::speech::TRY_TEXT);
    panel.click("#speech-sample").await;
    panel.run("document.getElementById('speech-sample').select()").await;
    panel.type_text("Hello chat").await;
    panel.run("document.activeElement.blur()").await;
    app.wait_for_config("the Try text", |config| {
        config.effects.speech.try_text.as_deref() == Some("Hello chat")
    })
    .await;
    panel
        .run(
            "const t = document.getElementById('speech-sample'); t.value = ' '; \
              t.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("the Try text emptied", |config| {
        config.effects.speech.try_text.as_deref() == Some("")
    })
    .await;
    panel
        .wait(
            "the field still empty",
            "document.getElementById('speech-sample').value === '' \
              && document.getElementById('speech-sample').placeholder === 'Thanks for the follow!'",
        )
        .await;

    // "Say instead": a word and how to say it.
    panel.click("#speech-add-word").await;
    panel.wait("a word row", "document.querySelector('.say-row input')").await;
    panel.click(".say-row input[aria-label='Word']").await;
    panel.type_text("Marrtin").await;
    panel.click(".say-row input[aria-label='Say instead']").await;
    panel.type_text("Martin").await;
    app.wait_for_config("the word", |config| {
        speech(config)
            .pronunciations
            .first()
            .is_some_and(|entry| entry.word == "Marrtin" && entry.say == "Martin")
    })
    .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_card_that_is_off_can_be_tested() {
    let app = App::start("a_card_that_is_off_can_be_tested", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    // Hydrate is a premade example that's off.
    let panel = open_card(&app, "hydrate", 3).await;
    assert!(!card(&app.saved_config(), "hydrate").enabled);

    // The Channel points alert card is on and reacts to every reward too.
    panel.click("#card-test").await;
    let alerts = [overlay.next_alert().await, overlay.next_alert().await];
    assert!(alerts.contains(&"TestViewer: drink some water!".to_string()), "{alerts:?}");
    assert!(!card(&app.saved_config(), "hydrate").enabled, "still off");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_timer_card_is_set_and_its_test_ticks() {
    // A Timer into an alert, off; and cheers through Compare number.
    let mut config = test_config();
    let timer = Node { id: 1, x: 40.0, y: 60.0, kind: NodeKind::Timer { minutes: 15.0, from_start: true } };
    config.cards.push(Card {
        id: "reminder".into(),
        name: "Reminder".into(),
        enabled: false,
        nodes: vec![timer, alert_node(2, "Tick!")],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    });
    let compare = NodeKind::CompareNumber {
        test: protocol::cards::NumberTest::AtLeast,
        value: 0.0,
        number: 1000.0,
        and: 50.0,
    };
    config.cards.push(Card {
        id: "cheers".into(),
        name: "Cheers".into(),
        enabled: false,
        nodes: vec![
            Node { id: 1, x: 40.0, y: 60.0, kind: NodeKind::Bits { min: 0.0 } },
            Node { id: 2, x: 340.0, y: 60.0, kind: compare },
            Node { id: 3, x: 600.0, y: 0.0, kind: alert_kind("Matches {amount}", 5.0) },
        ],
        wires: vec![
            wire((1, "event"), (2, "event")),
            wire((1, "amount"), (2, "value")),
            wire((2, "event"), (3, "trigger")),
        ],
    });
    let app = App::start("a_timer_card_is_set_and_its_test_ticks", config).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = open_card(&app, "reminder", 2).await;
    assert!(problems(card(&app.saved_config(), "reminder")).is_empty());

    panel.click("#node-1-minutes").await;
    panel.run("document.getElementById('node-1-minutes').select()").await;
    panel.type_text("5").await;
    panel.click("#node-1-from_start").await;
    app.wait_for_config("every 5 minutes, started by an event", |config| {
        matches!(
            card(config, "reminder").node(1).unwrap().kind,
            NodeKind::Timer { minutes, from_start: false } if minutes == 5.0
        )
    })
    .await;
    // Off, and not started: "Test this card" ticks it anyway.
    panel.click("#card-test").await;
    assert_eq!(overlay.next_alert().await, "Tick!");

    // Between shows And; a test cheer of 100 bits is between 50 and 1000.
    panel.go("card/cheers").await;
    panel.wait("the cheers card", "document.querySelectorAll('.node').length === 3").await;
    assert_eq!(panel.count("#node-2-and").await, 0, "no And for At least");
    panel
        .run(
            "const test = document.querySelector(\".node[data-node='2'] select\"); \
             test.value = 'Between'; test.dispatchEvent(new Event('change', { bubbles: true }));",
        )
        .await;
    panel.wait("the And field", "document.getElementById('node-2-and')").await;
    let config = app
        .wait_for_config("between", |config| {
            matches!(
                card(config, "cheers").node(2).unwrap().kind,
                NodeKind::CompareNumber { test: protocol::cards::NumberTest::Between, .. }
            )
        })
        .await;
    assert!(problems(card(&config, "cheers")).is_empty());
    // The premade Bits alert card is on and reacts to the cheer too.
    panel.click("#card-test").await;
    let alerts = [overlay.next_alert().await, overlay.next_alert().await];
    assert!(alerts.contains(&"Matches 100".to_string()), "{alerts:?}");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_placeholder_taken_out_takes_its_wire() {
    // A card: a raid's raider and viewers wired into "{raider} {viewers}".
    let mut config = test_config();
    let node = |id, kind| protocol::cards::Node { id, x: 40.0 + 300.0 * f64::from(id - 1), y: 60.0, kind };
    config.cards.push(Card {
        id: "texty".into(),
        name: "Texty".into(),
        enabled: true,
        nodes: vec![
            node(1, NodeKind::Raid { min_viewers: 0.0 }),
            node(2, NodeKind::text("{raider} {viewers}")),
            node(3, alert_kind("", 6.0)),
        ],
        wires: vec![
            wire((1, "event"), (3, "trigger")),
            wire((1, "raider"), (2, "raider")),
            wire((1, "viewers"), (2, "viewers")),
            wire((2, "text"), (3, "message")),
        ],
    });
    let app = App::start("a_placeholder_taken_out_takes_its_wire", config).await;
    let panel = open_card(&app, "texty", 3).await;

    // Take " {viewers}" out of the text, as typed.
    panel.click("[data-node='2'] textarea").await;
    panel.key(Key::END, modifiers::CTRL).await;
    for _ in 0.." {viewers}".len() {
        panel.key(Key::BACKSPACE, modifiers::NONE).await;
    }
    let config = app
        .wait_for_config("the text without {viewers}", |config| {
            matches!(&card(config, "texty").node(2).unwrap().kind, NodeKind::Text { template, .. } if template == "{raider}")
        })
        .await;
    let texty = card(&config, "texty");
    assert!(texty.wires.iter().all(|wire| wire.to.socket != "viewers"), "its wire went with it");
    assert!(problems(texty).is_empty(), "{:?}", problems(texty));
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn cards_are_put_in_order_by_dragging_or_keys() {
    let app = App::start("cards_are_put_in_order_by_dragging_or_keys", test_config()).await;
    let panel = Panel::open(&app, "cards").await;
    let order =
        |config: &AppConfig| config.cards.iter().map(|card| card.id.clone()).take(5).collect::<Vec<_>>();
    let start = order(&app.saved_config());
    assert_eq!(start, ["emote-rain", "bits-alert", "sub-alert", "gift-bomb-alert", "raid-alert"]);

    // Bits alert dragged by its handle to just past the middle of Raid alert.
    panel.wait("the handles", "document.getElementById('card-handle-bits-alert')").await;
    let from = panel.center("#card-handle-bits-alert").await;
    let raid = panel.center("li[data-card='raid-alert']").await;
    panel.drag(from, (from.0, raid.1 + 5.0), modifiers::NONE).await;
    let after_drag = ["emote-rain", "sub-alert", "gift-bomb-alert", "raid-alert", "bits-alert"];
    app.wait_for_config("bits alert after raid alert", |config| order(config) == after_drag).await;
    let moved = panel.count("li.card-item[style*='translateY(']").await;
    assert_eq!(moved, 0, "nothing is left shifted after the drop");

    // Two presses of ↑ on its handle: the handle keeps the focus.
    panel.click("#card-handle-bits-alert").await;
    panel.key(Key::ARROW_UP, modifiers::NONE).await;
    app.wait_for_config("one up", |config| order(config)[3] == "bits-alert").await;
    panel.wait("the focus back", "document.activeElement.id === 'card-handle-bits-alert'").await;
    panel.key(Key::ARROW_UP, modifiers::NONE).await;
    app.wait_for_config("two up", |config| order(config)[2] == "bits-alert").await;

    // One undo step per move.
    panel.key(Key::letter("z", "KeyZ", 90), modifiers::CTRL).await;
    app.wait_for_config("undone once", |config| order(config)[3] == "bits-alert").await;
    panel.close().await;
}

/// A raid, as if it came from Twitch.
fn raid(name: &str, viewers: u32) -> serde_json::Value {
    serde_json::json!({
        "at_ms": 0,
        "origin": { "provider": "twitch_chat", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
        "kind": {
            "input": "event",
            "event": "raid",
            "from": { "id": name, "name": name.to_lowercase(), "display_name": name },
            "viewers": viewers,
        },
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_queue_plays_one_at_a_time_and_events_replay() {
    let mut app = App::start_debug("the_queue_plays_one_at_a_time_and_events_replay", test_config()).await;
    let mut overlay = app.overlay_holding(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    panel.wait("the queue", "document.getElementById('queue-state')").await;
    let on_stage = "document.querySelectorAll('.queue-list.now .queue-item').length";
    let up_next = "document.querySelectorAll('ol.queue-list .queue-item').length";
    let outcome = |n: usize| {
        format!(
            "[...document.querySelectorAll('.history-item')][{n}]?.querySelector('.history-outcome')?.textContent"
        )
    };

    // Two raids: one plays, the other waits its turn.
    panel.input(raid("Alpha", 5)).await;
    panel.input(raid("Bravo", 6)).await;
    let (alpha, text) = overlay.next_show().await;
    assert_eq!(text, "Alpha is raiding with 5 viewers");
    panel.wait("one on stage, one next", &format!("{on_stage} === 1 && {up_next} === 1")).await;
    panel.wait("both in the history", "document.querySelectorAll('.history-item').length === 2").await;
    let newest: String =
        panel.eval("document.querySelector('.history-item .history-summary').textContent").await;
    assert_eq!(newest, "Bravo raided with 6 viewers");
    panel.wait("Bravo's alert waits", &format!("{} === 'waiting'", outcome(0))).await;

    // Paused: Alpha finishes, Bravo waits, also across a restart.
    panel.click("#queue-pause").await;
    panel.wait("paused", "document.getElementById('queue-state').textContent === 'Paused'").await;
    overlay.done(alpha).await;
    panel.wait("nothing on stage", &format!("{on_stage} === 0 && {up_next} === 1")).await;
    // Played is no news: the line says nothing, the item's state is kept.
    panel
        .wait(
            "Alpha's alert played",
            &format!(
                "{} === undefined                  && [...document.querySelectorAll('.history-item')][1].querySelector('.history-items li')?.dataset.state === 'played'",
                outcome(1)
            ),
        )
        .await;
    drop(overlay);
    let opened = panel.sockets_opened().await;
    // The queue and history are saved a moment after they change (the
    // restart kills the server, so nothing is written as it stops).
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    app.restart().await;
    let mut overlay = app.overlay_holding(vec![]).await;
    // The panel shows the queue as it was until it's back (and a click on
    // Resume before then goes nowhere): the queue from the new start.
    panel.wait_reconnected(opened, "queue").await;
    panel
        .wait(
            "still paused, Bravo still waiting",
            &format!("document.getElementById('queue-state')?.textContent === 'Paused' && {up_next} === 1"),
        )
        .await;

    // Resume: Bravo plays. Skip: it ends now.
    panel.click("#queue-resume").await;
    let (bravo, text) = overlay.next_show().await;
    assert_eq!(text, "Bravo is raiding with 6 viewers");
    panel.wait("something to skip", "!document.getElementById('queue-skip').disabled").await;
    panel.click("#queue-skip").await;
    let hidden = overlay.wait_for("the skip", e2e::skipped).await;
    assert_eq!(hidden, bravo);
    panel.wait("Bravo's alert skipped", &format!("{} === 'skipped'", outcome(0))).await;

    // Replay Alpha's raid: through the cards again, as a new entry.
    panel
        .run(
            "const alpha = [...document.querySelectorAll('.history-item')][1];              alpha.querySelector('details').open = true; alpha.querySelector('button.replay').click()",
        )
        .await;
    let (replay, text) = overlay.next_show().await;
    assert_eq!(text, "Alpha is raiding with 5 viewers");
    panel
        .wait(
            "the replay on top",
            "document.querySelectorAll('.history-item').length === 3 \
             && document.querySelector('.history-item .tag')?.textContent === 'Replay'",
        )
        .await;
    // The card it set off links to that card's editor.
    let href: String = panel
        .eval(
            "[...document.querySelectorAll('.history-item')][1].querySelector('.history-cards a')?.getAttribute('href')",
        )
        .await;
    assert_eq!(href, "#/card/raid-alert");
    // A queue item says what it is, then where it's from, on its second
    // line.
    panel
        .wait(
            "the replay on stage, what it is first",
            "document.querySelector('.queue-list.now .queue-from')?.textContent              === 'Alert · Replay · Alpha raided with 5 viewers · Raid alert'",
        )
        .await;

    // A waiting one, skipped from its row: dropped; the one on stage plays
    // on. Its row stays, faded, while the pointer is on the list (taking
    // it out would move the rest), and goes when the pointer leaves.
    let away = panel.center(".queue .box-title").await;
    panel.input(raid("Charlie", 7)).await;
    panel.wait("Charlie waits", &format!("{up_next} === 1")).await;
    panel.click("ol.queue-list .queue-skip-one").await;
    panel.wait("Charlie's alert skipped", &format!("{} === 'skipped'", outcome(0))).await;
    panel.wait("its row faded", "document.querySelector('ol.queue-list .queue-item.gone')").await;
    panel.hover(away).await;
    panel.wait("its row gone", &format!("{up_next} === 0 && {on_stage} === 1")).await;

    // Pointed at, the list holds still while the queue moves on: Delta
    // stays under Up next, under the pointer, and its × still skips Delta
    // (on stage by then), not whatever came after it.
    panel.input(raid("Delta", 8)).await;
    panel.wait("Delta waits", &format!("{up_next} === 1")).await;
    panel.hover(panel.center("ol.queue-list .queue-skip-one").await).await;
    overlay.done(replay).await;
    let (delta, text) = overlay.next_show().await;
    assert_eq!(text, "Delta is raiding with 8 viewers");
    panel.wait("held, with a note", "document.getElementById('queue-held')").await;
    panel
        .wait(
            "Delta still under Up next, the replay faded on stage",
            &format!("{up_next} === 1 && document.querySelector('.queue-list.now .queue-item.gone')"),
        )
        .await;
    panel.click("ol.queue-list .queue-skip-one").await;
    assert_eq!(overlay.wait_for("the skip", e2e::skipped).await, delta);
    panel.wait("Delta's row faded", "document.querySelector('ol.queue-list .queue-item.gone')").await;
    panel.hover(away).await;
    panel
        .wait(
            "caught up once the pointer leaves",
            &format!("{on_stage} === 0 && {up_next} === 0 && !document.getElementById('queue-held')"),
        )
        .await;

    // Played next from its row: Golf goes to the front. The first row's
    // ⤒ is hidden (it plays next already). While the pointer is on the
    // list, Golf's row stays where it is with its ⤒ lit, with no note (the
    // queue didn't move on by itself); it moves up when the pointer leaves.
    let texts =
        "[...document.querySelectorAll('ol.queue-list .queue-main')].map(m => m.textContent).join(' | ')";
    panel.click("#queue-pause").await;
    panel.wait("paused", "document.getElementById('queue-state').textContent === 'Paused'").await;
    for (name, viewers) in [("Echo", 9), ("Foxtrot", 10), ("Golf", 11)] {
        panel.input(raid(name, viewers)).await;
    }
    panel.wait("three wait", &format!("{up_next} === 3")).await;
    let next_of =
        |n: usize| format!("document.querySelector('ol.queue-list .queue-item:nth-child({n}) .queue-next')");
    panel.wait("the first's ⤒ hidden", &format!("{}.classList.contains('first')", next_of(1))).await;
    panel.click("ol.queue-list .queue-item:nth-child(3) .queue-next").await;
    panel.wait("Golf's ⤒ lit", &format!("{}.getAttribute('aria-pressed') === 'true'", next_of(3))).await;
    panel.wait("Echo isn't first any more", &format!("!{}.classList.contains('first')", next_of(1))).await;
    let held: String = panel.eval(texts).await;
    assert_eq!(
        held,
        "Echo is raiding with 9 viewers | Foxtrot is raiding with 10 viewers | Golf is raiding with 11 viewers"
    );
    assert_eq!(panel.count("#queue-held").await, 0, "no note for a move made here");
    panel.hover(away).await;
    panel
        .wait(
            "Golf first once the pointer leaves",
            &format!(
                "{texts} === 'Golf is raiding with 11 viewers | Echo is raiding with 9 viewers | Foxtrot is raiding with 10 viewers' \
                 && {}.classList.contains('first')",
                next_of(1)
            ),
        )
        .await;
    panel.click("#queue-resume").await;
    let (_, text) = overlay.next_show().await;
    assert_eq!(text, "Golf is raiding with 11 viewers");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn waiting_alerts_follow_the_cards_as_they_change() {
    let app = App::start_debug("waiting_alerts_follow_the_cards_as_they_change", test_config()).await;
    let key = app.control_key();
    let panel = Panel::open(&app, "activity").await;
    panel.wait("the queue", "document.getElementById('queue-state')").await;
    let up_next =
        "[...document.querySelectorAll('ol.queue-list .queue-main')].map(m => m.textContent).join(' | ')";
    let note = "(document.getElementById('queue-note')?.textContent ?? '')";
    let both = "'Alpha is raiding with 5 viewers | Bravo is raiding with 6 viewers'";

    // No overlay: two raids wait.
    panel.input(raid("Alpha", 5)).await;
    panel.input(raid("Bravo", 6)).await;
    panel.wait("both waiting", &format!("{up_next} === {both}")).await;

    // The raid alert switched off (a Stream Deck button): they're taken
    // out, and the history says so; on again, they're back.
    let (status, _) = panel.request("POST", &format!("/control/cards/raid-alert/off?key={key}")).await;
    assert_eq!(status, 200);
    panel
        .wait("taken out", &format!("{up_next} === '' && {note}.startsWith('Your change took out 2.')"))
        .await;
    panel
        .wait(
            "withdrawn in the history",
            "[...document.querySelectorAll('.history-items li')].every(li => li.dataset.state === 'withdrawn')",
        )
        .await;
    let (status, _) = panel.request("POST", &format!("/control/cards/raid-alert/on?key={key}")).await;
    assert_eq!(status, 200);
    panel
        .wait("back", &format!("{up_next} === {both} && {note}.startsWith('Your change brought back 2.')"))
        .await;

    // The alert's message changed on its card: the alerts waiting show it,
    // and flash on Activity until the note is closed.
    panel.go("card/raid-alert").await;
    panel.wait("the alert's message", "document.getElementById('node-2-message')").await;
    panel
        .run(
            "const f = document.getElementById('node-2-message'); f.value = '{raider} raids!'; \
             f.dispatchEvent(new InputEvent('input', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("the new message", |config| {
        matches!(&card(config, "raid-alert").node(2).unwrap().kind,
            NodeKind::Plugin { settings, .. } if settings.get("message") == Some(&"{raider} raids!".into()))
    })
    .await;
    panel.go("activity").await;
    panel
        .wait(
            "updated",
            &format!(
                "{up_next} === 'Alpha raids! | Bravo raids!' \
                 && {note}.startsWith('Your change updated 2 waiting items.') \
                 && document.querySelectorAll('.queue-item.changed').length === 2"
            ),
        )
        .await;
    panel.click("#queue-note-close").await;
    panel
        .wait(
            "the note closed",
            "!document.getElementById('queue-note') && !document.querySelector('.queue-item.changed')",
        )
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_history_shows_what_its_filter_lets_through() {
    let app = App::start_debug("the_history_shows_what_its_filter_lets_through", test_config()).await;
    let _overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    panel.input(raid("Small", 5)).await;
    panel.input(raid("Big", 60)).await;
    let summaries = "[...document.querySelectorAll('.history-summary')].map(s => s.textContent).join(' | ')";
    panel.wait("both raids", "document.querySelectorAll('.history-item').length === 2").await;

    // Raids of at least 10 viewers: the small one is hidden, not gone.
    panel.click("#history-filter").await;
    panel.wait("the filter", "document.getElementById('history-min-raids')").await;
    panel.click("#history-min-raids").await;
    panel.run("document.getElementById('history-min-raids').select()").await;
    panel.type_text("10").await;
    app.wait_for_config("raids of 10 or more", |config| config.history.min_raid == 10).await;
    panel.wait("the small raid hidden", "document.querySelectorAll('.history-item').length === 1").await;
    let shown: String = panel.eval(summaries).await;
    assert_eq!(shown, "Big raided with 60 viewers");
    let note: String = panel.eval("document.getElementById('history-hidden').textContent").await;
    assert_eq!(note, "1 event hidden by the filter");

    // No raids at all; then everything again.
    panel.click("#history-show-raids").await;
    app.wait_for_config("no raids", |config| !config.history.raids).await;
    panel.wait("no raids shown", "document.querySelectorAll('.history-item').length === 0").await;
    panel
        .run("[...document.querySelectorAll('.history-settings button')].find(b => b.textContent === 'Show everything').click()")
        .await;
    app.wait_for_config("everything", |config| config.history == Default::default()).await;
    panel.wait("both again", "document.querySelectorAll('.history-item').length === 2").await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_history_marks_what_was_seen() {
    let app = App::start_debug("the_history_marks_what_was_seen", test_config()).await;
    let _overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    panel.input(raid("Old", 5)).await;
    panel.wait("the old raid", "document.querySelectorAll('.history-item').length === 1").await;
    let rows = "[...document.querySelectorAll('.history-list > li')]        .map(li => li.classList.contains('seen-line') ? '---' : li.querySelector('.history-summary').textContent.split(' ')[0]).join(' ')";
    let new = "(document.getElementById('history-new')?.textContent ?? '')";

    // Off at first; switched on, what's there is seen.
    panel.click("#history-filter").await;
    panel.wait("the filter", "document.getElementById('history-seen-marker')").await;
    let on: bool = panel.eval("document.getElementById('history-seen-marker').checked").await;
    assert!(!on, "off at first");
    panel.click("#history-seen-marker").await;
    app.wait_for_config("the marker on", |config| config.history.seen_marker).await;
    panel.wait("the stream start option", "document.getElementById('history-seen-at-start')").await;
    panel.click("#history-seen-at-start").await;
    app.wait_for_config("seen at stream start", |config| config.history.seen_at_stream_start).await;
    panel.click("#history-filter").await;

    // New ones go above the line.
    panel.input(raid("Alpha", 6)).await;
    panel.input(raid("Bravo", 7)).await;
    panel.wait("two new", &format!("{new} === '2 new'")).await;
    let shown: String = panel.eval(rows).await;
    assert_eq!(shown, "Bravo Alpha --- Old");

    // A row's ✓: it and the older ones are seen.
    panel.run("document.querySelectorAll('.history-item.seeable .seen-here')[1].click()").await;
    panel.wait("one new", &format!("{new} === '1 new'")).await;
    let shown: String = panel.eval(rows).await;
    assert_eq!(shown, "Bravo --- Alpha Old");
    let open: bool =
        panel.eval("[...document.querySelectorAll('.history-item details')].some(d => d.open)").await;
    assert!(!open, "the ✓ doesn't open the entry");

    // The control API (a Stream Deck key): everything seen, and it says
    // how many are new.
    let key = app.control_key();
    let (_, state) = panel.request("GET", &format!("/control/state?key={key}")).await;
    assert_eq!(state["history"]["new"], 1);
    let (status, answer) =
        panel.request("POST", &format!("{}?key={key}", protocol::control::HISTORY_SEEN_PATH)).await;
    assert_eq!((status, answer), (200, serde_json::json!({ "new": 0 })));
    panel.wait("all seen", &format!("{new} === '' && !document.getElementById('history-seen-line')")).await;

    // All seen on the line.
    panel.input(raid("Charlie", 8)).await;
    panel.wait("the line", "document.getElementById('history-all-seen')").await;
    panel.click("#history-all-seen").await;
    panel.wait("all seen again", "!document.getElementById('history-seen-line')").await;
    panel.close().await;
}

/// Cheered bits with a message, as if they came from Twitch chat (which
/// sends the name colour the viewer picked).
fn cheer(name: &str, bits: u64, text: &str) -> serde_json::Value {
    let user = serde_json::json!({ "id": name, "name": name.to_lowercase(), "display_name": name, "color": "#ff4500" });
    serde_json::json!({
        "at_ms": 0,
        "origin": { "provider": "twitch_chat", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
        "kind": {
            "input": "event",
            "event": "support",
            "user": user,
            "amount": { "type": "units", "value": bits, "unit": "bits" },
            "message": { "user": user, "text": text },
        },
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_history_shows_the_message_without_opening_the_entry() {
    let app =
        App::start_debug("the_history_shows_the_message_without_opening_the_entry", test_config()).await;
    let _overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    let long = "so many words in this message ".repeat(30);
    panel.input(cheer("Talker", 100, long.trim())).await;
    panel.wait("the cheer", "document.querySelectorAll('.history-item').length === 1").await;

    // Closed: the message is in the summary, on the entry's one line.
    let message = "document.querySelector('.history-item summary .history-message-text')";
    let closed: bool =
        panel.eval(&format!("!document.querySelector('.history-item details').open && !!{message}")).await;
    assert!(closed, "the message shows on the closed entry");
    let cut: bool = panel.eval(&format!("{message}.scrollWidth > {message}.clientWidth + 1")).await;
    assert!(cut, "a long message is cut to one line");
    let height: u32 = panel.eval("document.querySelector('.history-item summary').offsetHeight").await;
    assert!(height < 34, "the entry is one line high, not {height} px");
    let text: String = panel.eval(&format!("{message}.textContent")).await;
    assert_eq!(text, long.trim());

    // The name in its chat colour, in the history and in the queue.
    let colors: String = panel
        .eval(
            "[...document.querySelectorAll('.history-summary .viewer-name, .queue-item .viewer-name')]              .map(name => `${name.textContent} ${getComputedStyle(name).color}`).join(' | ')",
        )
        .await;
    assert!(colors.starts_with("Talker rgb(255, 69, 0)"), "{colors}");
    assert!(colors.contains(" | Talker rgb(255, 69, 0)"), "in the queue too: {colors}");

    // Open: all of it.
    panel.run("document.querySelector('.history-item details').open = true").await;
    panel.wait("all of it", &format!("{message}.scrollHeight <= {message}.clientHeight + 1")).await;

    // Turned off: only in the opened entry, and the Filter button isn't
    // lit up for it (it hides no events).
    panel.click("#history-filter").await;
    panel.wait("the setting", "document.getElementById('history-messages')").await;
    panel.click("#history-messages").await;
    app.wait_for_config("no messages", |config| !config.history.messages).await;
    panel
        .wait("not in the summary", "!document.querySelector('.history-item summary .history-message')")
        .await;
    let in_body: bool =
        panel.eval("!!document.querySelector('.history-item .history-body .history-message')").await;
    assert!(in_body, "the opened entry still has it");
    panel.click("#history-filter").await;
    let lit: bool =
        panel.eval("document.getElementById('history-filter').classList.contains('primary')").await;
    assert!(!lit, "the Filter button isn't lit up for it");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn links_in_the_activity_tab_open_in_the_browser() {
    // The Bits alert shows the message, so it has the link too.
    let mut config = test_config();
    let card = config.cards.iter_mut().find(|card| card.id == "bits-alert").unwrap();
    for node in &mut card.nodes {
        if let NodeKind::Plugin { plugin, settings, .. } = &mut node.kind
            && plugin == "alert"
        {
            settings.insert("message".into(), "Thanks {viewer}! {message}".into());
        }
    }
    let app = App::start_debug("links_in_the_activity_tab_open_in_the_browser", config).await;
    // The alert stays on stage, in the queue.
    let _overlay = app.overlay_holding(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    panel.input(cheer("Talker", 100, "come see twitch.tv/Talker!")).await;
    panel.wait("the cheer", "document.querySelectorAll('.history-item').length === 1").await;
    // Held here: the server would open this PC's browser.
    panel
        .run(
            "const socket = window.__panelSockets.at(-1); window.__opened = [];              socket.send = data => JSON.parse(data).type === 'open_link'                  ? window.__opened.push(JSON.parse(data)) : WebSocket.prototype.send.call(socket, data);",
        )
        .await;
    let opened = |n: usize| format!("window.__opened[{n}]");
    let url = "https://twitch.tv/Talker";

    // In the queue: the alert on stage.
    let link = "document.querySelector('.queue-list.now .queue-item .viewer-link')";
    panel.wait("the alert on stage", link).await;
    let shown: (String, String) =
        panel.eval(&format!("[{link}.textContent, {link}.getAttribute('href')]")).await;
    assert_eq!(shown, ("twitch.tv/Talker".to_string(), url.to_string()));
    panel.run(&format!("{link}.click()")).await;
    panel.wait("the server asked to open it", "window.__opened.length === 1").await;
    let item: u64 =
        panel.eval("Number(document.querySelector('.queue-list.now .queue-item').dataset.item)").await;
    let asked: serde_json::Value = panel.eval(&opened(0)).await;
    assert_eq!(asked, serde_json::json!({ "type": "open_link", "from": { "item": item }, "url": url }));
    let hash: String = panel.eval("location.hash").await;
    assert_eq!(hash, "#/activity", "the panel didn't follow the link");

    // In the History: the message on the entry's line.
    let link = "document.querySelector('.history-item summary .viewer-link')";
    let shown: (String, String) =
        panel.eval(&format!("[{link}.textContent, {link}.getAttribute('href')]")).await;
    assert_eq!(shown, ("twitch.tv/Talker".to_string(), url.to_string()));
    panel.run(&format!("{link}.click()")).await;
    panel.wait("asked again", "window.__opened.length === 2").await;
    let entry: u64 = panel.eval("Number(document.querySelector('.history-item').dataset.entry)").await;
    let asked: serde_json::Value = panel.eval(&opened(1)).await;
    assert_eq!(asked, serde_json::json!({ "type": "open_link", "from": { "entry": entry }, "url": url }));
    let (open, hash): (bool, String) =
        panel.eval("[document.querySelector('.history-item details').open, location.hash]").await;
    assert!(!open, "the entry didn't open");
    assert_eq!(hash, "#/activity", "the panel didn't follow the link");

    // Opened: in the alert as it looked too.
    panel.run("document.querySelector('.history-item details').open = true").await;
    panel.wait("the alert's link", "document.querySelector('.history-items .viewer-link')").await;
    panel.run("document.querySelector('.history-items .viewer-link').click()").await;
    panel.wait("asked a third time", "window.__opened.length === 3").await;
    let still: bool = panel.eval("document.querySelector('.history-item details').open").await;
    assert!(still, "the entry stays open");

    // The server opens only a link that's where the panel says.
    for from in [format!("{{ entry: {entry} }}"), format!("{{ item: {item} }}")] {
        panel
            .run(&format!(
                "WebSocket.prototype.send.call(window.__panelSockets.at(-1), JSON.stringify(                 {{ type: 'open_link', from: {from}, url: 'https://example.com/elsewhere' }}))"
            ))
            .await;
    }
    let refused = || {
        std::fs::read_to_string(app.log_path())
            .unwrap_or_default()
            .matches("isn't where the panel said")
            .count()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while refused() < 2 {
        assert!(std::time::Instant::now() < deadline, "the server opened a link that isn't where it said");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn an_opened_entry_shows_the_alert_as_it_looked() {
    // The Bits alert says thanks in the small style, then the message.
    let mut config = test_config();
    let card = config.cards.iter_mut().find(|card| card.id == "bits-alert").unwrap();
    for node in &mut card.nodes {
        if let NodeKind::Plugin { plugin, settings, .. } = &mut node.kind
            && plugin == "alert"
        {
            settings.insert("message".into(), "[small]Thanks {viewer}![/] {message}".into());
        }
    }
    let app = App::start_debug("an_opened_entry_shows_the_alert_as_it_looked", config).await;
    let _overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    panel.input(cheer("Talker", 100, "hello there")).await;
    panel.wait("the cheer", "document.querySelectorAll('.history-item').length === 1").await;
    let quote = "getComputedStyle(document.querySelector('.history-item summary .history-message')).display";
    let shown: String = panel.eval(quote).await;
    assert_ne!(shown, "none", "closed, the message is on the entry's line");

    // Open: the alert as it looked, the message in it, once.
    panel.run("document.querySelector('.history-item details').open = true").await;
    panel.wait("the alert", "document.querySelector('.history-items li')").await;
    let alert: String = panel.eval("document.querySelector('.history-items li').textContent").await;
    assert!(alert.starts_with("Thanks Talker! hello there ·"), "{alert:?}");
    let small: String = panel
        .eval("document.querySelector('.history-items li span[style]')?.getAttribute('style') ?? ''")
        .await;
    assert!(small.contains("font-size: 70%"), "the small style: {small:?}");
    let hidden: String = panel.eval(quote).await;
    assert_eq!(hidden, "none", "the quote steps aside");
    // Under the summary's words.
    let lined_up: bool = panel
        .eval(
            "Math.abs(document.querySelector('.history-item .history-summary').getBoundingClientRect().left              - document.querySelector('.history-body-main').getBoundingClientRect().left) < 1",
        )
        .await;
    assert!(lined_up, "the opened entry lines up under the summary");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn queued_alerts_show_their_emotes() {
    // The Bits alert says thanks, then the message, Kappa as a picture.
    let mut config = test_config();
    let card = config.cards.iter_mut().find(|card| card.id == "bits-alert").unwrap();
    for node in &mut card.nodes {
        if let NodeKind::Plugin { plugin, settings, .. } = &mut node.kind
            && plugin == "alert"
        {
            settings.insert("message".into(), "Thanks {viewer}! {message}".into());
        }
    }
    let app = App::start_debug("queued_alerts_show_their_emotes", config).await;
    // The first alert stays on stage, the second waits.
    let _overlay = app.overlay_holding(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    for name in ["Talker", "Other"] {
        let mut input = cheer(name, 100, "hello Kappa");
        input["kind"]["message"]["emotes"] = serde_json::json!([
            { "start": 6, "end": 11, "emote": { "source": "twitch", "id": "25", "code": "Kappa" } }
        ]);
        panel.input(input).await;
    }
    panel.wait("both alerts", "document.querySelectorAll('.queue-main').length === 2").await;
    let looked = |list: &str| {
        let main = format!("document.querySelector('{list} .queue-main')");
        format!("[{main}.textContent.trim(), [...{main}.querySelectorAll('img')].map(img => img.alt)]")
    };
    let on_stage: (String, Vec<String>) = panel.eval(&looked(".queue-list.now")).await;
    assert_eq!(on_stage, ("Thanks Talker! hello".to_string(), vec!["Kappa".to_string()]));
    let up_next: (String, Vec<String>) = panel.eval(&looked("ol.queue-list")).await;
    assert_eq!(up_next, ("Thanks Other! hello".to_string(), vec!["Kappa".to_string()]));
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_follow_alert_card_can_be_tested() {
    let app = App::start("the_follow_alert_card_can_be_tested", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    // Off at first, like the other examples.
    assert!(!card(&app.saved_config(), "follow-alert").enabled);
    let panel = open_card(&app, "follow-alert", 3).await;
    panel.click("#card-test").await;
    assert_eq!(overlay.next_alert().await, "TestViewer followed!");
    panel.close().await;
}

/// A hype train event from Twitch's EventSub, at `level`.
fn hype_train(change: serde_json::Value, level: u32, seconds_left: u32) -> serde_json::Value {
    serde_json::json!({
        "at_ms": 0,
        "origin": { "provider": "twitch_eventsub", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
        "kind": {
            "input": "event",
            "event": "hype_train",
            "change": change,
            "train": {
                "id": "train", "level": level, "progress": 300, "goal": 1000, "total": 1800,
                "kind": "golden_kappa", "shared": false, "seconds_left": seconds_left,
            },
        },
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn hype_trains_are_tested_and_the_bar_follows_them() {
    let app = App::start_debug("hype_trains_are_tested_and_the_bar_follows_them", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    let bar = |command: &Command| match command {
        Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Run { values, .. } })
            if plugin == "hype-train" =>
        {
            Some(values.clone())
        }
        _ => None,
    };
    let numbers = |values: &serde_json::Map<String, serde_json::Value>| {
        ["level", "progress", "goal", "seconds_left"].map(|name| values[name].as_f64().unwrap())
    };

    // Both cards are off at first. "Test this card" plays a whole short
    // train, so the bar fills up, goes up a level and ends (and leaves):
    // it isn't left up for Twitch's 5 minutes.
    let config = app.saved_config();
    assert!(!card(&config, "hype-train-alert").enabled && !card(&config, "hype-train-bar").enabled);
    let panel = open_card(&app, "hype-train-bar", 2).await;
    panel.click("#card-test").await;
    let mut steps = Vec::new();
    for _ in 0..4 {
        let values = overlay.wait_for("the bar", bar).await;
        assert_eq!(values["title"]["text"], "Hype train");
        steps.push(numbers(&values));
    }
    assert_eq!(
        steps,
        [
            [1.0, 125.0, 500.0, 300.0],
            [1.0, 300.0, 500.0, 240.0],
            [2.0, 100.0, 1000.0, 300.0],
            [2.0, 0.0, 0.0, 0.0]
        ]
    );
    // The alert card: its start, its level-up and its end, not the
    // contribution in between.
    panel.go("card/hype-train-alert").await;
    panel.wait("the alert card", "document.querySelectorAll('.node').length === 2").await;
    panel.click("#card-test").await;
    for text in ["A hype train started", "The hype train reached level 2", "The hype train ended at level 2"]
    {
        assert_eq!(overlay.next_alert().await, text);
    }

    // With the bar card on, the Test page's train at level 3 reaches it,
    // with the train's numbers; then its end, with no time left.
    panel.go("cards").await;
    panel.click("#card-switch-hype-train-bar").await;
    app.wait_for_config("the bar card on", |config| card(config, "hype-train-bar").enabled).await;
    panel.go("test").await;
    panel.click("#test-hype-train-level").await;
    panel.key(Key::A, modifiers::CTRL).await;
    panel.type_text("3").await;
    panel.click("#test-hype-train-level-up").await;
    let values = overlay.wait_for("the bar", bar).await;
    assert_eq!(numbers(&values), [3.0, 150.0, 1500.0, 300.0]);
    panel.click("#test-hype-train-end").await;
    let values = overlay.wait_for("the bar's end", bar).await;
    assert_eq!(values["seconds_left"].as_f64(), Some(0.0));

    // Status lists what the login listens for only with a login (logging
    // in is optional: chat needs none).
    panel.go("status").await;
    open_status_box(&panel, "status-twitch").await;
    panel
        .wait(
            "not logged in",
            "document.getElementById('status-login-state')?.textContent === 'Not logged in' \
             && document.getElementById('login-start')",
        )
        .await;
    assert_eq!(panel.count("#status-hype-train").await, 0);
    panel.close().await;

    // The bar's page draws a train from Twitch and keeps it with the app,
    // so a reloaded overlay carries on with it. (One browser at a time:
    // the event goes in from the overlay's page.)
    let page = Panel::open_overlay(&app, "?modules=hype-train").await;
    page.wait("its page loaded", "document.querySelector('iframe#plugin-hype-train[data-loaded]')").await;
    page.input(hype_train(serde_json::json!({ "type": "progressed", "level_up": true }), 4, 240)).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let mut fresh = app.overlay(vec!["hype-train"]).await;
        let kept = fresh
            .wait_at_most(std::time::Duration::from_millis(300), |command| match command {
                Command::Plugin(protocol::PluginCommand {
                    plugin,
                    message: PluginMessage::Kept { value },
                }) if plugin == "hype-train" => Some(value.clone()),
                _ => None,
            })
            .await;
        if let Some(train) = kept.as_ref().filter(|train| train["level"] == 4) {
            assert_eq!(train["title"]["text"], "Golden Kappa train");
            assert_eq!(
                train["classes"],
                serde_json::json!(["golden_kappa"]),
                "its kind, from the node's Class"
            );
            assert_eq!((train["progress"].as_f64(), train["goal"].as_f64()), (Some(300.0), Some(1000.0)));
            assert_eq!(train["ended_at_ms"], serde_json::Value::Null);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the bar's page never kept the train: {kept:?}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    // The default look (Purple) draws it, gold for a Golden Kappa train.
    page.run("document.body.style.background = 'black'").await;
    let path = app.dir.join("hype-train.png");
    let mut gold = 0;
    for _ in 0..20 {
        let png = page.screenshot(&path).await;
        gold = page
            .eval::<u32>(&format!(
                "(async () => {{ const image = new Image(); image.src = 'data:image/png;base64,{png}'; \
                 await image.decode(); const canvas = document.createElement('canvas'); \
                 canvas.width = image.width; canvas.height = image.height; \
                 const context = canvas.getContext('2d'); context.drawImage(image, 0, 0); \
                 const d = context.getImageData(0, 0, canvas.width, canvas.height).data; let gold = 0; \
                 for (let i = 0; i < d.length; i += 4) if (d[i] > 200 && d[i + 1] > 150 && d[i + 1] < 210 && d[i + 2] < 90) gold++; \
                 return gold; }})()"
            ))
            .await;
        if gold > 1000 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(gold > 1000, "only {gold} gold pixels; see {}", path.display());
    page.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_control_api_switches_cards_and_effects_and_fires_events() {
    // A card for `/control/events/party`: "{colour} party!".
    let mut config = test_config();
    let node = |id, kind| protocol::cards::Node { id, x: 40.0 + 300.0 * f64::from(id - 1), y: 60.0, kind };
    config.cards.push(Card {
        id: "party".into(),
        name: "Party".into(),
        enabled: true,
        nodes: vec![
            node(1, NodeKind::Custom { name: "party".into() }),
            node(2, alert_kind("{colour} party!", 5.0)),
        ],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    });
    let app = App::start("the_control_api_switches_cards_and_effects_and_fires_events", config).await;
    let mut overlay = app.overlay_holding(vec![]).await;
    let key = app.control_key();
    let panel = Panel::open(&app, "control").await;
    let with_key = |path: &str| format!("{path}?key={key}");

    // Nothing without the key.
    let (status, answer) = panel.request("GET", "/control/state").await;
    assert_eq!(status, 401, "{answer}");
    let (status, _) = panel.request("POST", "/control/cards/party/off?key=wrong").await;
    assert_eq!(status, 401);
    assert!(card(&app.saved_config(), "party").enabled);

    // The state, as the Stream Deck plugin sees it on connecting.
    let mut client = app.control().await;
    let state = client.wait_for_state("the first state", |_| true).await;
    assert_eq!(state.version, 1);
    assert!(state.cards.iter().any(|card| card.id == "party" && card.on));
    assert_eq!(state.events, ["party"]);
    assert_eq!(state.effects.len(), 4);
    let (status, answer) = panel.request("GET", &with_key("/control/")).await;
    assert_eq!(status, 200);
    assert_eq!(answer["cards"].as_array().map(Vec::len), Some(state.cards.len()));

    // A card off, by id; on again, by name. The WebSocket hears both.
    let (status, answer) = panel.request("POST", &with_key("/control/cards/party/toggle")).await;
    assert_eq!((status, answer["on"].as_bool()), (200, Some(false)));
    app.wait_for_config("the card off", |config| !card(config, "party").enabled).await;
    client.wait_for_state("the card off", |state| state.cards.iter().any(|c| c.id == "party" && !c.on)).await;
    let (status, answer) = panel.request("GET", &with_key("/control/cards/Party/on")).await;
    assert_eq!((status, answer["on"].as_bool()), (200, Some(true)));
    app.wait_for_config("the card on", |config| card(config, "party").enabled).await;
    let (status, _) = panel.request("GET", &with_key("/control/cards/nope/on")).await;
    assert_eq!(status, 404);

    // An effect, over the WebSocket.
    client.send(&ControlRequest::Effect { id: "rain".into(), set: Switch::Off }).await;
    let effect = client
        .wait_for("the answer", |message| match message {
            ControlMessage::Effect { effect } => Some(effect.clone()),
            _ => None,
        })
        .await;
    assert_eq!((effect.id.as_str(), effect.on), ("rain", false));
    app.wait_for_config("the rain off", |config| !config.plugin("rain").enabled).await;
    let (status, _) = panel.request("POST", &with_key("/control/effects/confetti/on")).await;
    assert_eq!(status, 404);

    // An event with data runs the card.
    let (status, answer) =
        panel.request("GET", &format!("{}&colour=Red", with_key("/control/events/party"))).await;
    assert_eq!((status, answer["fired"].as_str()), (200, Some("party")));
    let (id, text) = overlay.next_show().await;
    assert_eq!(text, "Red party!");

    // The queue: toggle pauses and resumes; skip ends what's playing.
    let (_, answer) = panel.request("POST", &with_key("/control/queue/toggle")).await;
    assert_eq!(answer["paused"].as_bool(), Some(true));
    let (_, answer) = panel.request("POST", &with_key("/control/queue/toggle")).await;
    assert_eq!(answer["paused"].as_bool(), Some(false));
    let (_, answer) = panel.request("POST", &with_key("/control/queue/skip")).await;
    assert_eq!(answer["playing"].as_bool(), Some(false));
    let hidden = overlay.wait_for("the skip", e2e::skipped).await;
    assert_eq!(hidden, id);

    // One item, by the id the state lists it with: a waiting one is
    // dropped; skipped again, it's gone and nothing more happens.
    panel.request("POST", &with_key("/control/queue/pause")).await;
    for colour in ["Blue", "Green"] {
        panel.request("GET", &format!("{}&colour={colour}", with_key("/control/events/party"))).await;
    }
    let state = client.wait_for_state("two waiting", |state| state.queue.up_next.len() == 2).await;
    let texts: Vec<&str> = state.queue.up_next.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, ["Blue party!", "Green party!"]);
    let blue = state.queue.up_next[0].id;
    let skip = |id: u64| with_key(&protocol::control::skip_item_path(id));
    let (status, answer) = panel.request("POST", &skip(blue)).await;
    assert_eq!((status, answer["was"].as_str()), (200, Some("waiting")));
    let (status, answer) = panel.request("POST", &skip(blue)).await;
    assert_eq!((status, answer["was"].as_str()), (200, Some("gone")));
    let (status, _) = panel.request("POST", &with_key("/control/items/next/skip")).await;
    assert_eq!(status, 404);

    // Played next, by id: a waiting one goes to the front; one that's gone
    // is left alone.
    panel.request("GET", &format!("{}&colour=Pink", with_key("/control/events/party"))).await;
    let state = client.wait_for_state("two waiting again", |state| state.queue.up_next.len() == 2).await;
    let pink = state.queue.up_next[1].id;
    let next = |id: u64| with_key(&protocol::control::play_next_path(id));
    let (status, answer) = panel.request("POST", &next(pink)).await;
    assert_eq!((status, answer["was"].as_str()), (200, Some("waiting")));
    let state = client.wait_for_state("Pink first", |state| state.queue.up_next[0].id == pink).await;
    let texts: Vec<&str> = state.queue.up_next.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, ["Pink party!", "Green party!"]);
    let (status, answer) = panel.request("GET", &next(blue)).await;
    assert_eq!((status, answer["was"].as_str()), (200, Some("gone")));
    client.send(&ControlRequest::PlayNext { id: pink }).await;
    let was = client
        .wait_for("the answer", |message| match message {
            ControlMessage::MovedItem { was, .. } => Some(*was),
            _ => None,
        })
        .await;
    assert_eq!(was, protocol::activity::Moved::Waiting, "at the front already");
    // Pink skipped: Green is next again.
    client.send(&ControlRequest::SkipItem { id: pink }).await;
    client
        .wait_for("Pink skipped", |message| match message {
            ControlMessage::SkippedItem { was, .. } => Some(*was),
            _ => None,
        })
        .await;

    // The one on stage, over the WebSocket: it ends.
    panel.request("POST", &with_key("/control/queue/resume")).await;
    let (green, text) = overlay.next_show().await;
    assert_eq!(text, "Green party!");
    client.send(&ControlRequest::SkipItem { id: green }).await;
    let was = client
        .wait_for("the answer", |message| match message {
            ControlMessage::SkippedItem { was, .. } => Some(*was),
            _ => None,
        })
        .await;
    assert_eq!(was, protocol::activity::Skipped::OnStage);
    assert_eq!(overlay.wait_for("the skip", e2e::skipped).await, green);

    // The panel's Control API box (#/control opens Status at it): the key
    // hidden until asked, and a URL to copy under "Make a URL", folded.
    panel.wait("the key", "document.getElementById('control-key')").await;
    let shown: String = panel.eval("document.getElementById('control-key').value").await;
    assert!(!shown.contains(&key));
    panel.click("#control-show").await;
    panel.wait("the key shown", &format!("document.getElementById('control-key').value === '{key}'")).await;
    assert_eq!(panel.count("#control-url").await, 0, "folded");
    panel.click("#control-make-open").await;
    panel.wait("the URL", "document.getElementById('control-url')").await;
    let url: String = panel.eval("document.getElementById('control-url').value").await;
    assert_eq!(url, format!("http://127.0.0.1:{}/control/cards/emote-rain/toggle?key={key}", app.port));

    // A new key: the old one stops working.
    panel.run("window.confirm = () => true").await;
    panel.click("#control-new-key").await;
    panel.wait("a new key", &format!("document.getElementById('control-key').value !== '{key}'")).await;
    let (status, _) = panel.request("GET", &with_key("/control/state")).await;
    assert_eq!(status, 401);
    assert_ne!(app.control_key(), key);
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_stream_deck_plugin_installs_from_status_and_says_its_version() {
    use protocol::panel::{PanelEvent, Status};
    use protocol::streamdeck::{CLIENT, PLUGIN_FILE, release_file_url, shown};
    let app =
        App::start("the_stream_deck_plugin_installs_from_status_and_says_its_version", test_config()).await;
    let panel = Panel::open(&app, "control").await;
    panel.resize(380, 800).await;
    let state = "document.getElementById('control-deck-state')?.textContent";
    let button = "document.getElementById('control-deck-install')?.textContent";
    let head = "document.querySelector('#control > .box-head > .plugin-status')?.textContent";
    // The plugin's own version, from its manifest.
    let manifest = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../streamdeck/com.localstreamkit.streamdeck.sdPlugin/manifest.json"
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest).unwrap()).unwrap();
    let built_in = manifest["Version"].as_str().unwrap().to_string();
    let version = shown(&built_in).to_string();

    // None connected: Install writes the built-in plugin (cargo e2e's
    // stand-in) to the cache folder and opens it (logged, with --no-open).
    panel.wait("no plugin", &format!("{state} === 'Not connected'")).await;
    panel.wait("Install", &format!("{button} === 'Install'")).await;
    panel.click("#control-deck-install").await;
    let opened = |line: &str| line.contains("not opened (--no-open)") && line.contains(PLUGIN_FILE);
    app.wait_logged("the plugin opened", 1, opened).await;
    let stand_in = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/stand-in.streamDeckPlugin")).unwrap();
    let file = app.dir.join("cache").join(PLUGIN_FILE);
    assert!(std::fs::read(&file).unwrap() == stand_in, "the built-in plugin, in the cache folder");
    assert_eq!(panel.count("#control-deck-error").await, 0, "nothing to say when it worked");

    // The plugin says its version when it connects, and Status shows it
    // while it's connected.
    let mut deck = app.control().await;
    deck.send(&ControlRequest::Hello { client: CLIENT.into(), version: built_in.clone() }).await;
    let answer = deck
        .wait_for("the answer", |message| match message {
            ControlMessage::Hello { version } => Some(version.clone()),
            _ => None,
        })
        .await;
    assert_eq!(answer, env!("CARGO_PKG_VERSION"), "the app's version");
    panel.wait("connected", &format!("{state} === '{version} connected'")).await;
    assert_eq!(panel.eval::<String>(button).await, "Install");
    assert_eq!(panel.eval::<String>(head).await, format!("Stream Deck {version}"));
    // A hello needs a name.
    deck.send(&ControlRequest::Hello { client: " ".into(), version: "1".into() }).await;
    let refused = deck
        .wait_for("the refusal", |message| match message {
            ControlMessage::Error { status, .. } => Some(*status),
            _ => None,
        })
        .await;
    assert_eq!(refused, 400);
    drop(deck);
    panel.wait("gone", &format!("{state} === 'Not connected'")).await;

    // An older one: Update installs the built-in one.
    let mut old = app.control().await;
    old.send(&ControlRequest::Hello { client: CLIENT.into(), version: "0.0.1.0".into() }).await;
    panel.wait("the old one", &format!("{state} === '0.0.1 connected'")).await;
    panel.wait("Update", &format!("{button} === 'Update to {version}'")).await;
    assert_eq!(panel.eval::<String>(head).await, "Stream Deck 0.0.1");
    panel.click("#control-deck-install").await;
    app.wait_logged("the plugin opened again", 2, opened).await;
    let wide: f64 = panel.eval("document.documentElement.scrollWidth").await;
    assert!(wide <= 380.0, "fits the dock: {wide}");

    // A build without the plugin says so, and links to the release's file,
    // opened in the browser on the app's PC. (Its status, handed to the
    // panel; the server's own are held back from then on.)
    let status = Status {
        version: env!("CARGO_PKG_VERSION").into(),
        control_key: app.control_key(),
        ..Status::default()
    };
    let json = serde_json::to_string(&PanelEvent::Status { status }).unwrap();
    panel
        .run(&format!(
            "const socket = window.__panelSockets.at(-1); const real = socket.onmessage; \
             real.call(socket, new MessageEvent('message', {{ data: {} }})); \
             socket.onmessage = event => JSON.parse(event.data).type === 'status' || real.call(socket, event);",
            js_string(&json)
        ))
        .await;
    panel.wait("no plugin built in", "document.getElementById('control-deck-none')").await;
    assert_eq!(panel.count("#control-deck-install").await, 0);
    let href: String =
        panel.eval("document.getElementById('control-deck-download').getAttribute('href')").await;
    assert_eq!(href, release_file_url(&protocol::update::releases(), env!("CARGO_PKG_VERSION")));
    panel.click("#control-deck-download").await;
    app.wait_logged("the release's file opened", 1, |line| {
        line.contains("not opened (--no-open)") && line.contains(&href)
    })
    .await;
    drop(old);
    panel.close().await;
}

fn follow(name: &str) -> serde_json::Value {
    serde_json::json!({
        "at_ms": 0,
        "origin": { "provider": "twitch_eventsub", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
        "kind": {
            "input": "event",
            "event": "follow",
            "user": { "id": name, "name": name.to_lowercase(), "display_name": name },
        },
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_queue_clears_one_kind_and_mutes_and_the_history_clears() {
    let mut config = test_config();
    config.cards.iter_mut().find(|card| card.id == "follow-alert").expect("the follow card").enabled = true;
    let app = App::start_debug("the_queue_clears_one_kind_and_mutes_and_the_history_clears", config).await;
    let mut overlay = app.overlay_holding(vec![]).await;
    let panel = Panel::open(&app, "activity").await;
    panel.wait("the queue", "document.getElementById('queue-state')").await;
    let up_next = "document.querySelectorAll('ol.queue-list .queue-item').length";
    let text = |id: &str| format!("document.getElementById('{id}')?.textContent");

    // Paused: a follow bot's burst and a raid wait.
    panel.click("#queue-pause").await;
    panel.wait("paused", "document.getElementById('queue-state').textContent === 'Paused'").await;
    for name in ["Bot1", "Bot2", "Bot3"] {
        panel.input(follow(name)).await;
    }
    panel.input(raid("Friend", 12)).await;
    panel.wait("four waiting", &format!("{up_next} === 4")).await;
    let chips: String = panel
        .eval("[...document.querySelectorAll('.queue .filters button')].map(b => b.textContent).join(' | ')")
        .await;
    assert_eq!(chips, "All | Raids 1 | Follows 3");

    // Follows only: the list shows them, Clear drops them, the raid stays.
    panel.click("#queue-only-follow").await;
    panel.wait("only follows listed", &format!("{up_next} === 3")).await;
    let clear: String = panel.eval(&text("queue-clear")).await;
    assert_eq!(clear, "Clear follows");
    panel.click("#queue-clear").await;
    panel.wait("asks", &format!("{} === 'Drop 3 follows?'", text("queue-clear"))).await;
    panel.click("#queue-clear").await;
    panel.wait("the raid left", &format!("{up_next} === 1")).await;
    let left: String = panel.eval("document.querySelector('ol.queue-list .queue-main').textContent").await;
    assert_eq!(left, "Friend is raiding with 12 viewers");
    assert_eq!(panel.count(".queue .filters").await, 0, "one kind left: no chips");

    // The volume's mute button: saved, and every overlay is told, so what
    // plays goes on silently. The raid's alert plays as usual.
    let level = |volume: f32, muted: bool| {
        move |command: &Command| match command {
            Command::Shell(ShellCommand::Volume { volume: v, muted: m }) if *v == volume && *m == muted => {
                Some(())
            }
            _ => None,
        }
    };
    panel.click("#queue-mute").await;
    panel.wait("muted", "document.getElementById('queue-muted')").await;
    app.wait_for_config("muted", |config| config.effects.sound.muted).await;
    overlay.wait_for("the overlay muted", level(1.0, true)).await;
    let key = app.control_key();
    let (_, state) = panel.request("GET", &format!("/control/state?key={key}")).await;
    assert_eq!(
        (state["sound"]["volume"].as_f64(), state["sound"]["muted"].as_bool()),
        (Some(1.0), Some(true))
    );
    assert_eq!(state["queue"]["by_kind"], serde_json::json!({ "raid": 1 }));
    panel.click("#queue-resume").await;
    let (_, shown) = overlay.wait_for("the raid's alert", e2e::alert_shown).await;
    assert_eq!(shown, "Friend is raiding with 12 viewers");

    // Unmuted from the control API; the slider sets the volume, live too.
    let (status, answer) = panel.request("POST", &format!("/control/mute/toggle?key={key}")).await;
    assert_eq!((status, answer["muted"].as_bool()), (200, Some(false)));
    panel.wait("unmuted", "!document.getElementById('queue-muted')").await;
    overlay.wait_for("the overlay unmuted", level(1.0, false)).await;
    panel
        .run("const s = document.getElementById('queue-volume'); s.value = '40'; s.dispatchEvent(new Event('input', { bubbles: true }))")
        .await;
    app.wait_for_config("at 40%", |config| (config.effects.sound.volume - 0.4).abs() < 1e-6).await;
    overlay.wait_for("the overlay at 40%", level(0.4, false)).await;
    let (status, _) = panel.request("POST", &format!("/control/queue/clear?kind=nope&key={key}")).await;
    assert_eq!(status, 404);

    // Clear history: it asks, then the list is empty, also after a reload.
    panel.wait("the history", "document.querySelectorAll('.history-item').length === 4").await;
    panel.click("#history-clear").await;
    panel.wait("asks", &format!("{} === 'Forget 4 events?'", text("history-clear"))).await;
    panel.click("#history-clear").await;
    panel.wait("forgotten", "document.querySelectorAll('.history-item').length === 0").await;
    panel.run("location.reload()").await;
    panel.wait("the queue again", "document.getElementById('queue-state')").await;
    assert_eq!(panel.count(".history-item").await, 0);
    panel.close().await;
}

fn chat(id: &str, name: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "at_ms": 0,
        "origin": { "provider": "twitch_chat", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
        "kind": {
            "input": "message",
            "id": id,
            "user": { "id": name, "name": name.to_lowercase(), "display_name": name, "color": "#ff0000" },
            "text": text,
            "emotes": [],
        },
    })
}

/// The lines the chat display's page keeps with the app (what it shows),
/// as a new overlay gets them back, once they're `wanted`.
async fn chat_shows(
    app: &App,
    what: &str,
    wanted: impl Fn(&[protocol::chat::ChatLine]) -> bool,
) -> Vec<protocol::chat::ChatLine> {
    let kept = |command: &Command| match command {
        Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Kept { value } })
            if plugin == "chat" =>
        {
            serde_json::from_value::<Vec<protocol::chat::ChatLine>>(value["lines"].clone()).ok()
        }
        _ => None,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let mut overlay = app.overlay(vec!["chat"]).await;
        let lines =
            overlay.wait_at_most(std::time::Duration::from_millis(300), &kept).await.unwrap_or_default();
        if wanted(&lines) {
            return lines;
        }
        assert!(std::time::Instant::now() < deadline, "the chat display never showed {what}: {lines:?}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_chat_display_shows_chat_and_card_lines() {
    let mut config = test_config();
    config.cards.push(Card {
        id: "raided".into(),
        name: "Raided".into(),
        enabled: true,
        nodes: vec![
            Node { id: 1, x: 0.0, y: 0.0, kind: NodeKind::Raid { min_viewers: 0.0 } },
            plugin_node(
                2,
                "chat",
                "show",
                serde_json::json!({ "message": "{raider} is here!", "class": "raid" }),
            ),
        ],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    });
    let app = App::start_debug("the_chat_display_shows_chat_and_card_lines", config).await;
    // Its own source: an overlay without ?modules= doesn't host it.
    let mut everything = app.overlay(vec![]).await;
    let pages = everything
        .wait_for("its pages", |command| match command {
            Command::Shell(ShellCommand::Plugins { plugins }) => Some(plugins.clone()),
            _ => None,
        })
        .await;
    assert!(pages.contains(&"alert".to_string()) && !pages.contains(&"chat".to_string()), "{pages:?}");

    let page = Panel::open_overlay(&app, "?modules=chat").await;
    page.wait("its page loaded", "document.querySelector('iframe#plugin-chat[data-loaded]')").await;
    page.run("document.body.style.background = 'black'").await;
    // Chat: commands and bots are left out (by default); a deleted message
    // goes. (The inputs go from the overlay's page: one browser at a time.)
    page.input(chat("m1", "Nightbot", "a bot")).await;
    page.input(chat("m2", "Viewer", "!so someone")).await;
    page.input(chat("m3", "Viewer", "hello <b>chat</b>")).await;
    let lines = chat_shows(&app, "a viewer's message", |lines| !lines.is_empty()).await;
    assert_eq!(lines.iter().map(|line| line.id.as_str()).collect::<Vec<_>>(), ["twitch:m3"]);
    assert!(lines[0].html().contains("hello &lt;b&gt;chat&lt;/b&gt;"));
    // The built-in look draws it: a white bubble.
    let path = app.dir.join("chat.png");
    let mut white = 0;
    for _ in 0..20 {
        let png = page.screenshot(&path).await;
        white = page
            .eval::<u32>(&format!(
                "(async () => {{ const image = new Image(); image.src = 'data:image/png;base64,{png}'; \
                 await image.decode(); const canvas = document.createElement('canvas'); \
                 canvas.width = image.width; canvas.height = image.height; \
                 const context = canvas.getContext('2d'); context.drawImage(image, 0, 0); \
                 const d = context.getImageData(0, 0, canvas.width, canvas.height).data; let white = 0; \
                 for (let i = 0; i < d.length; i += 4) if (d[i] > 240 && d[i + 1] > 240 && d[i + 2] > 240) white++; \
                 return white; }})()"
            ))
            .await;
        if white > 1000 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(white > 1000, "only {white} white pixels; see {}", path.display());
    page.input(serde_json::json!({
            "at_ms": 0,
            "origin": { "provider": "twitch_chat", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
            "kind": { "input": "moderation", "type": "message_deleted", "id": "m3" },
        }))
        .await;
    chat_shows(&app, "the message deleted", |lines| lines.is_empty()).await;

    // A Show in chat node: the raider as its name, with the node's class.
    page.input(raid("Alpha", 5)).await;
    let lines = chat_shows(&app, "the raid's line", |lines| !lines.is_empty()).await;
    assert_eq!(lines[0].source, protocol::chat::LineSource::Card { class: "raid".into() });
    assert!(lines[0].html().contains("Alpha is here!"), "{}", lines[0].html());
    assert_eq!(lines[0].user.as_ref().map(|user| user.display_name.as_str()), Some("Alpha"));
    page.close().await;

    // Its box on Plugins: no description; the look picked from its own,
    // with its help beside it; the messages' settings in rows.
    let panel = Panel::open(&app, "plugins/chat").await;
    panel.wait("the look", "document.getElementById('plugin-chat-setting-look')?.value === 'Bubbles'").await;
    let said: String = panel.eval("document.getElementById('plugin-chat-body').innerText").await;
    assert!(!said.contains("Browser Source of its own"), "{said}");
    let headings: Vec<String> = panel
        .eval("[...document.querySelectorAll('#plugin-chat .setting-group h3')].map(h => h.textContent)")
        .await;
    assert_eq!(headings, ["Look", "Messages"]);
    panel
        .run(
            "const s = document.getElementById('plugin-chat-setting-look'); s.value = 'Classic'; \
             s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("Classic picked", |config| {
        config.plugin("chat").settings.get("look") == Some(&"Classic".into())
    })
    .await;
    panel.click("#plugin-chat-setting-look-help").await;
    panel.wait("its help", "document.getElementById('plugin-chat-setting-look-help-page')").await;
    // A short number field with its words; tick boxes without; the bots a
    // line each.
    let (width, words): (f64, String) = panel
        .eval(
            "(() => { const field = document.getElementById('plugin-chat-setting-max_messages'); \
              return [field.getBoundingClientRect().width, field.closest('.number-row').innerText]; })()",
        )
        .await;
    assert!(width < 100.0, "{width}");
    assert_eq!(words.trim(), "messages at most");
    let commands: String = panel
        .eval(
            "document.getElementById('plugin-chat-setting-show_commands').closest('.setting-row').innerText",
        )
        .await;
    assert_eq!(commands.trim(), "Commands");
    let bots: Vec<String> = panel
        .eval("[...document.querySelectorAll('#plugin-chat-setting-hidden_users input')].map(input => input.value)")
        .await;
    assert_eq!(bots.len(), 7, "{bots:?}");
    assert_eq!(bots[0], "Nightbot");
    panel.click("#plugin-chat-setting-hidden_users-0-remove").await;
    panel.click("#plugin-chat-setting-hidden_users-add").await;
    app.wait_for_config("a bot out, a line in", |config| {
        let settings = config.plugin("chat").settings;
        let bots = settings.get("hidden_users").and_then(serde_json::Value::as_array);
        bots.is_some_and(|bots| bots.len() == 7 && bots[0] == "StreamElements" && bots[6] == "")
    })
    .await;
    panel.click("#plugin-chat-setting-show_commands").await;
    app.wait_for_config("commands shown", |config| {
        config.plugin("chat").settings.get("show_commands") == Some(&serde_json::Value::Bool(true))
    })
    .await;
    // Its preview: chat-sized, with sample lines in the look (white bubbles).
    panel.wait("its preview", "document.getElementById('plugin-chat-preview')").await;
    panel
        .run(
            "document.getElementById('plugin-chat-preview').parentElement.scrollIntoView({ block: 'start' })",
        )
        .await;
    let (width, height): (f64, f64) = panel
        .eval(
            "(() => { const box = document.getElementById('plugin-chat-preview').parentElement.getBoundingClientRect();              return [box.width, box.height]; })()",
        )
        .await;
    assert!((height / width - 400.0 / 500.0).abs() < 0.03, "500 × 400, scaled: {width} × {height}");
    let path = app.dir.join("preview.png");
    let mut white = 0;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let png = panel.screenshot(&path).await;
        white = panel
            .eval::<u32>(&format!(
                "(async () => {{ const box = document.getElementById('plugin-chat-preview').parentElement.getBoundingClientRect();                  const image = new Image(); image.src = 'data:image/png;base64,{png}'; await image.decode();                  const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height;                  const context = canvas.getContext('2d'); context.drawImage(image, 0, 0);                  const d = context.getImageData(box.left, box.top, box.width, box.height).data; let white = 0;                  for (let i = 0; i < d.length; i += 4) if (d[i] > 240 && d[i + 1] > 240 && d[i + 2] > 240) white++;                  return white; }})()"
            ))
            .await;
        if white > 1000 {
            break;
        }
    }
    assert!(white > 1000, "only {white} white pixels in the preview; see {}", path.display());
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn refresh_emotes_reaches_the_server() {
    let app = App::start("refresh_emotes_reaches_the_server", test_config()).await;
    let panel = Panel::open(&app, "status").await;
    open_status_box(&panel, "status-emotes").await;
    panel.wait("the button", "document.getElementById('status-resync-emotes')").await;
    panel.click("#status-resync-emotes").await;
    panel.wait("loading", "document.getElementById('status-resync-emotes').disabled").await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::fs::read_to_string(app.log_path()).unwrap_or_default().contains("refreshing the emotes") {
        assert!(std::time::Instant::now() < deadline, "the server never got the resync");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panel.wait("ready again", "!document.getElementById('status-resync-emotes').disabled").await;
    panel.close().await;
}

/// Opens a box on Status (they start folded, unless they need a look).
async fn open_status_box(panel: &Panel, id: &str) {
    panel.wait("the box", &format!("document.getElementById('{id}-open')")).await;
    panel
        .run(&format!(
            "const b = document.getElementById('{id}-open'); if (b.getAttribute('aria-expanded') === 'false') b.click()"
        ))
        .await;
    panel.wait("it open", &format!("document.getElementById('{id}-body')")).await;
}

/// What the logs folder's files hold, together.
fn log_files(app: &App) -> String {
    let Ok(entries) = std::fs::read_dir(app.dir.join("logs")) else { return String::new() };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("lsk."))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect()
}

async fn wait_for_log(app: &App, what: &str, check: impl Fn(&str) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !check(&log_files(app)) {
        assert!(std::time::Instant::now() < deadline, "the log file never had {what}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_log_is_kept_and_detailed_switches_live() {
    let mut app = App::start_debug("the_log_is_kept_and_detailed_switches_live", test_config()).await;
    // With RUST_LOG set, it decides and the tick can't be changed.
    let panel = Panel::open(&app, "status").await;
    open_status_box(&panel, "status-log").await;
    panel.wait("the log box", "document.getElementById('log-detailed')?.disabled").await;
    panel.close().await;
    app.rust_log = None;
    app.stop().await;
    // The first start's lines (detailed: RUST_LOG) are in the same file.
    let start = log_files(&app).len();
    app.restart().await;
    wait_for_log(&app, "the start", |log| log[start..].contains("plugins loaded")).await;

    // Not detailed: a signal isn't logged; ticked, from the next one on.
    let panel = Panel::open(&app, "status").await;
    open_status_box(&panel, "status-log").await;
    panel.wait("the tick", "document.getElementById('log-detailed')?.disabled === false").await;
    panel.input(raid("Alpha", 5)).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!log_files(&app)[start..].contains("DEBUG"), "debug lines without the tick");
    panel.click("#log-detailed").await;
    app.wait_for_config("detailed", |config| config.log.detailed).await;
    wait_for_log(&app, "the switch", |log| log.contains("detailed log on")).await;
    panel.input(raid("Bravo", 6)).await;
    wait_for_log(&app, "a debug line", |log| log[start..].contains("DEBUG")).await;

    // Warnings and errors are listed on Status until cleared: no channel,
    // then an lsk.json that can't be used.
    panel
        .wait(
            "the warning",
            "document.querySelector('#log-recent .log-line:not(.error)')?.textContent.includes('no Twitch channel set')",
        )
        .await;
    panel.click("#log-clear").await;
    panel.wait("the list cleared", "document.getElementById('log-recent-none')").await;
    std::fs::write(app.dir.join("lsk.json"), "{ not json").unwrap();
    panel
        .wait(
            "the error",
            "document.querySelector('#log-recent .log-line.error')?.textContent.includes('previous settings stay in use')",
        )
        .await;
    panel.click("#log-clear").await;
    panel.wait("the list cleared", "document.getElementById('log-recent-none')").await;
    panel.close().await;
}

/// A stand-in for GitHub's releases: `latest.json` and its signature (as
/// given), and the exe of any version, whatever `exe` holds when it's
/// asked for. Its address, for `--update-from`.
async fn stand_in_releases(feed: String, signature: String, exe: Arc<Mutex<Vec<u8>>>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let port = listener.local_addr().expect("its address").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let (feed, signature, exe) = (feed.clone(), signature.clone(), Arc::clone(&exe));
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let path =
                    String::from_utf8_lossy(&request).split(' ').nth(1).unwrap_or_default().to_string();
                let body = match path.as_str() {
                    "/releases/latest/download/latest.json" => Some(feed.into_bytes()),
                    "/releases/latest/download/latest.json.sig" => Some(signature.into_bytes()),
                    path if path.starts_with("/releases/download/v") && path.ends_with("/lsk.exe") => {
                        Some(exe.lock().unwrap().clone())
                    }
                    _ => None,
                };
                let head = match &body {
                    Some(body) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                    }
                };
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body.unwrap_or_default()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_new_version_is_downloaded_into_the_exes_place() {
    use base64::Engine;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    let base64 = base64::engine::general_purpose::STANDARD;
    // Version 99.0.0: an "exe", and latest.json for it signed with the
    // test's own key.
    let exe: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let sha256: String = ring::digest::digest(&ring::digest::SHA256, &exe)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let feed = format!(r#"{{"version":"99.0.0","size":{},"sha256":"{sha256}"}}"#, exe.len());
    let signature = base64.encode(pair.sign(feed.as_bytes()));
    // At first the download comes broken: a byte changed.
    let mut broken = exe.clone();
    broken[1000] ^= 1;
    let served = Arc::new(Mutex::new(broken));
    let releases = stand_in_releases(feed, signature, Arc::clone(&served)).await;
    // The exe it replaces is a stand-in in the test's folder, never
    // target/debug/lsk.exe (the one every test runs).
    let key = base64.encode(pair.public_key().as_ref());
    let args = ["--update-from", &releases, "--update-exe", "bin/lsk.exe", "--update-key", &key];
    let mut app =
        App::start_with("a_new_version_is_downloaded_into_the_exes_place", test_config(), &args).await;
    let bin = app.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("lsk.exe"), "the running exe").unwrap();

    // The notice, with What's new; About says it too, with the licence and
    // support.
    let panel = Panel::open(&app, "status").await;
    panel.resize(380, 800).await;
    let notice = "document.getElementById('update-notice')?.textContent";
    panel.wait("the notice", &format!("{notice}?.includes('Version 99.0.0 is out')")).await;
    let whats_new: String =
        panel.eval("document.getElementById('update-whats-new').getAttribute('href')").await;
    assert!(whats_new.ends_with("/releases/tag/v99.0.0"), "{whats_new}");
    open_status_box(&panel, "status-about").await;
    panel
        .wait("About", "document.getElementById('about-version')?.textContent.includes('99.0.0 is out')")
        .await;
    let (notice_line, licence, support): (String, String, String) = panel
        .eval(
            "[document.getElementById('about-notice').textContent, \
              document.getElementById('about-licence').getAttribute('href'), \
              document.getElementById('about-support').getAttribute('href')]",
        )
        .await;
    assert_eq!(notice_line, protocol::update::REQUIRED_NOTICE);
    assert_eq!(licence, protocol::update::licence_url());
    assert_eq!(support, protocol::update::SUPPORT_URL);
    // Everything fits the dock.
    let wide: f64 = panel.eval("document.documentElement.scrollWidth").await;
    assert!(wide <= 380.0, "fits the dock: {wide}");

    // A broken download fails, saying why; the exe stays as it was.
    panel.click("#update-download").await;
    panel
        .wait(
            "the failure",
            &format!("{notice}?.includes('The download of version 99.0.0 failed') && {notice}.includes(\"isn't the released file\")"),
        )
        .await;
    assert_eq!(std::fs::read_to_string(bin.join("lsk.exe")).unwrap(), "the running exe");
    let left: Vec<_> = std::fs::read_dir(&bin).unwrap().flatten().map(|entry| entry.file_name()).collect();
    assert_eq!(left, ["lsk.exe"], "no old exe, no half download");

    // Try again: it's in the exe's place, the exe it replaced beside it.
    *served.lock().unwrap() = exe.clone();
    panel.click("#update-download").await;
    panel.wait("ready", &format!("{notice}?.includes('Version 99.0.0 is ready')")).await;
    assert!(std::fs::read(bin.join("lsk.exe")).unwrap() == exe, "the new exe");
    assert_eq!(std::fs::read_to_string(bin.join("lsk.old.exe")).unwrap(), "the running exe");
    panel.close().await;

    // The next start deletes the old one; the switch turns checking off.
    app.restart().await;
    assert!(!bin.join("lsk.old.exe").exists(), "deleted at start");
    let panel = Panel::open(&app, "status").await;
    panel.wait("the notice again", &format!("{notice}?.includes('Version 99.0.0 is out')")).await;
    open_status_box(&panel, "status-about").await;
    panel.click("#updates-check").await;
    app.wait_for_config("checking off", |config| !config.updates.check).await;
    panel
        .wait("not checked", "document.getElementById('about-version')?.textContent.includes('Not checked')")
        .await;
    panel.wait("no notice", "!document.getElementById('update-notice')").await;
    panel.close().await;
}

/// The pages an overlay is told to host next.
async fn next_pages(overlay: &mut e2e::Overlay, what: &str) -> Vec<String> {
    overlay
        .wait_for(what, |command| match command {
            Command::Shell(ShellCommand::Plugins { plugins }) => Some(plugins.clone()),
            _ => None,
        })
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn overlays_are_managed_on_status_and_follow_live() {
    let app = App::start("overlays_are_managed_on_status_and_follow_live", test_config()).await;
    // The plain link is the default overlay; ?overlay=chat the chat display's;
    // one that isn't on the list shows nothing.
    let mut default = app.overlay(vec![]).await;
    assert!(next_pages(&mut default, "the default's pages").await.contains(&"alert".to_string()));
    let mut chat = app.named_overlay("chat").await;
    assert_eq!(next_pages(&mut chat, "the chat overlay's pages").await, ["chat"]);
    let mut big = app.named_overlay("big").await;
    assert_eq!(next_pages(&mut big, "an unknown overlay's pages").await, Vec::<String>::new());

    let panel = Panel::open(&app, "status").await;
    let head: String =
        panel.eval("document.querySelector('#overlays .box-head .plugin-status').innerText").await;
    assert_eq!(head.trim(), "3 in OBS");
    open_status_box(&panel, "overlays").await;
    panel.wait("the chat overlay", "document.getElementById('overlay-chat')").await;
    let link: String = panel.eval("document.getElementById('overlay-chat-link').value").await;
    assert_eq!(link, format!("http://localhost:{}/?overlay=chat", app.port));
    let drag: String = panel.eval("document.getElementById('overlay-chat-drag').getAttribute('href')").await;
    assert!(drag.ends_with("?overlay=chat&layer-name=Chat&layer-width=500&layer-height=700"), "{drag}");
    let state: String =
        panel.eval("document.querySelector('#overlay-chat .overlay-state').textContent").await;
    assert_eq!(state, "In OBS");

    // The unknown one is listed; added back, it shows every effect at once.
    panel.wait("the unlisted overlay", "document.getElementById('overlay-unlisted-0')").await;
    panel.click("#overlay-unlisted-0 button").await;
    app.wait_for_config("big added back", |config| {
        config.overlays.iter().any(|overlay| overlay.id == "big" && overlay.plugins.is_none())
    })
    .await;
    assert!(next_pages(&mut big, "every effect").await.contains(&"rain".to_string()));

    // Chat also shows alerts, live; renamed, its id stays.
    panel.click("#overlay-chat-open").await;
    panel.wait("its plugins", "document.getElementById('overlay-chat-plugin-alert')").await;
    panel.click("#overlay-chat-plugin-alert").await;
    let pages = next_pages(&mut chat, "chat and alerts").await;
    assert!(pages.contains(&"alert".to_string()) && pages.contains(&"chat".to_string()), "{pages:?}");
    panel.click("#overlay-chat-name").await;
    panel.run("document.getElementById('overlay-chat-name').select()").await;
    panel.type_text("Chat box").await;
    app.wait_for_config("renamed", |config| {
        config.overlays.iter().any(|overlay| overlay.id == "chat" && overlay.name == "Chat box")
    })
    .await;

    // Added (showing nothing until picked) and removed.
    panel.click("#overlays-add").await;
    app.wait_for_config("a new overlay", |config| {
        config.overlays.iter().any(|overlay| overlay.id == "overlay" && overlay.plugins == Some(vec![]))
    })
    .await;
    panel.wait("its row, open", "document.getElementById('overlay-overlay-remove')").await;
    panel.click("#overlay-overlay-remove").await;
    app.wait_for_config("removed", |config| !config.overlays.iter().any(|overlay| overlay.id == "overlay"))
        .await;
    panel.close().await;
}

/// Copies a folder, recursively.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("make the folder");
    for entry in std::fs::read_dir(from).expect("read the folder").map(Result::unwrap) {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy a file");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn plugin_settings_follow_the_wires_and_emotes_settings() {
    // A rain-like plugin: Emotes (or its setting) and Count; "Rain" only
    // with emotes wired in, the count's field only without.
    let mut config = test_config();
    let shower = |id: &str, wired: bool| Card {
        id: id.into(),
        name: id.into(),
        enabled: true,
        nodes: vec![
            Node { id: 1, x: 0.0, y: 0.0, kind: NodeKind::EmotesInChat },
            plugin_node(2, "shower", "shower", serde_json::json!({})),
        ],
        wires: if wired {
            vec![wire((1, "event"), (2, "trigger")), wire((1, "emotes"), (2, "emotes"))]
        } else {
            vec![wire((1, "event"), (2, "trigger"))]
        },
    };
    config.cards.push(shower("loose", false));
    config.cards.push(shower("wired", true));
    let mut app = App::start("plugin_settings_follow_the_wires_and_emotes_settings", config).await;
    let folder = app.dir.join("plugins").join("shower");
    std::fs::create_dir_all(folder.join("overlay")).unwrap();
    std::fs::write(folder.join("overlay").join("index.html"), "<!doctype html>").unwrap();
    let manifest = serde_json::json!({
        "id": "shower", "name": "Shower", "version": "0.1.0", "api": 1,
        "settings": [{ "key": "lifetime_ms", "label": "Lifetime", "type": "number", "default": 8000,
                       "min": 1000, "max": 60000, "unit": "ms", "step": 500, "slider": true, "group": "Timing" }],
        "nodes": [{
            "id": "shower", "title": "Shower", "category": "effects", "changes": ["lifetime_ms"],
            "inputs": [
                { "name": "trigger", "label": "Trigger", "type": "event" },
                { "name": "emotes", "label": "Emotes", "type": "emotes" },
                { "name": "count", "label": "Count", "type": "number" }
            ],
            "settings": [
                { "key": "emotes", "label": "Emotes", "type": "emotes", "count": "count" },
                { "key": "count", "label": "Count", "type": "number", "default": 20, "when": { "unwired": "emotes" } },
                { "key": "per_message", "label": "Rain", "type": "choice", "when": { "wired": "emotes" },
                  "options": [{ "value": "each", "label": "Every emote" }, { "value": "one", "label": "One" }] }
            ]
        }]
    });
    std::fs::write(folder.join("plugin.json"), manifest.to_string()).unwrap();
    app.restart().await;

    // Nothing wired into Emotes: Random or these emotes, and the count.
    let panel = open_card(&app, "loose", 2).await;
    panel.wait("the emotes field", "document.getElementById('node-2-emotes')?.value === 'random'").await;
    assert_eq!(panel.count("#node-2-count").await, 1);
    assert_eq!(panel.count("#node-2-plugin-per_message").await, 0, "only with emotes wired in");
    panel
        .run(
            "const s = document.getElementById('node-2-emotes'); s.value = 'codes'; \
             s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("codes chosen", |config| {
        matches!(&card(config, "loose").node(2).unwrap().kind,
            NodeKind::Plugin { settings, .. } if settings.get("emotes") == Some(&serde_json::json!([])))
    })
    .await;
    panel.wait("the codes field", "document.getElementById('node-2-plugin-emotes-codes')").await;

    // "+ Change a setting": the plugin's lifetime, for this node only,
    // starting at the plugin's, shown in seconds.
    panel
        .run(
            "const s = document.getElementById('node-2-add-change'); s.value = 'lifetime_ms';              s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("the lifetime changed", |config| {
        matches!(&card(config, "loose").node(2).unwrap().kind,
            NodeKind::Plugin { changes, .. } if changes.get("lifetime_ms") == Some(&serde_json::json!(8000.0)))
    })
    .await;
    panel.wait("its field", "document.getElementById('node-2-change-lifetime_ms')?.value === '8'").await;
    assert_eq!(panel.count("#node-2-add-change").await, 0, "nothing else to change");
    panel.close().await;

    // The Plugins page: the plugin's own setting as a slider, in its group
    // under the group's heading, its label in the label column beside it
    // and its value after it.
    let panel = Panel::open(&app, "plugins/shower").await;
    panel
        .wait(
            "the slider",
            "document.querySelector('.setting-group #plugin-shower-setting-lifetime_ms')?.type === 'range'",
        )
        .await;
    let (heading, label, value): (String, String, String) = panel
        .eval(
            "(() => { const slider = document.getElementById('plugin-shower-setting-lifetime_ms'); \
              const row = slider.closest('.setting-row'); \
              return [slider.closest('.setting-group').querySelector('h3').textContent, \
                      row.querySelector('label.setting-label[for=plugin-shower-setting-lifetime_ms]').textContent, \
                      row.querySelector('.slider-value').textContent]; })()",
        )
        .await;
    assert_eq!((heading.as_str(), label.as_str(), value.as_str()), ("Timing", "Lifetime", "8.0 s"));
    let beside: bool = panel
        .eval(
            "(() => { const row = document.getElementById('plugin-shower-setting-lifetime_ms').closest('.setting-row'); \
              const label = row.querySelector('.setting-label').getBoundingClientRect(); \
              const control = row.querySelector('.setting-control').getBoundingClientRect(); \
              return label.right <= control.left && Math.abs(label.top + label.height / 2 - (control.top + control.height / 2)) < 4; })()",
        )
        .await;
    assert!(beside, "the label is beside its slider, in line with it");
    panel.close().await;

    // Emotes wired in: "Rain", and neither the emotes nor the count field.
    let panel = open_card(&app, "wired", 2).await;
    panel.wait("the Rain setting", "document.getElementById('node-2-plugin-per_message')").await;
    assert_eq!(panel.count("#node-2-count").await, 0);
    assert_eq!(panel.count("#node-2-emotes").await, 0);
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_plugin_node_is_added_set_and_fired() {
    let mut app = App::start("a_plugin_node_is_added_set_and_fired", test_config()).await;
    // The example plugin, as a user would install it.
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/confetti");
    copy_dir(&example, &app.dir.join("plugins").join("confetti"));
    // And one that doesn't load.
    std::fs::create_dir_all(app.dir.join("plugins").join("broken")).unwrap();
    std::fs::write(app.dir.join("plugins").join("broken").join("plugin.json"), "{ not json").unwrap();
    app.restart().await;

    // An overlay without ?modules= hosts every plugin's page.
    let mut overlay = app.overlay(vec![]).await;
    let pages = overlay
        .wait_for("the plugin pages", |command| match command {
            Command::Shell(ShellCommand::Plugins { plugins }) => Some(plugins.clone()),
            _ => None,
        })
        .await;
    assert_eq!(
        pages,
        ["alert", "confetti", "hype-train", "media", "particles", "rain", "sound", "speak"],
        "the folder's and the built-in ones"
    );

    let panel = open_card(&app, "raid-alert", 2).await;
    // Its page is served, kept to the app's own address; nothing outside
    // its overlay folder is.
    let (status, policy): (u16, String) = panel
        .eval("fetch('/plugins/confetti/index.html').then(r => [r.status, r.headers.get('content-security-policy')])")
        .await;
    assert_eq!(status, 200);
    assert!(policy.starts_with(&format!("default-src http://127.0.0.1:{}", app.port)), "{policy}");
    let outside: u16 = panel.eval("fetch('/plugins/confetti/%2e%2e/plugin.json').then(r => r.status)").await;
    assert_eq!(outside, 404);

    // Shift+A: the plugin's node is in the add-node menu.
    panel.hover(empty_spot(&panel).await).await;
    panel.key(Key::A, modifiers::SHIFT).await;
    panel.wait("the add-node menu", "document.querySelector('.menu input:focus')").await;
    panel.type_text("confetti").await;
    panel
        .wait(
            "only Confetti",
            "document.querySelectorAll('.menu-item').length === 1 \
             && document.querySelector('.menu-item').textContent === 'Confetti'",
        )
        .await;
    panel.key(Key::ENTER, modifiers::NONE).await;
    let config =
        app.wait_for_config("the confetti node", |config| card(config, "raid-alert").nodes.len() == 3).await;
    let Some(NodeKind::Plugin { plugin, node, settings, .. }) =
        card(&config, "raid-alert").node(3).map(|node| &node.kind)
    else {
        panic!("node 3 isn't a plugin's: {:?}", card(&config, "raid-alert").node(3));
    };
    assert_eq!((plugin.as_str(), node.as_str()), ("confetti", "burst"));
    assert_eq!(settings["from"], "corners", "every setting starts at its default");
    assert_eq!(settings["pieces"], 150.0);

    // Pieces is its input's field; From has a row of its own.
    panel.wait("the Pieces field", "document.getElementById('node-3-pieces')").await;
    panel
        .run(
            "const s = document.getElementById('node-3-plugin-from'); s.value = 'top'; \
             s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("from the top", |config| {
        matches!(&card(config, "raid-alert").node(3).unwrap().kind,
                 NodeKind::Plugin { settings, .. } if settings["from"] == "top")
    })
    .await;

    // Wired to the raid, a test sends its values to the plugin's page.
    // (Home fits every node in view first: the new one is tall.)
    panel.key(Key::HOME, modifiers::NONE).await;
    let from = panel.center(&select::output(1, "event")).await;
    let to = panel.center(&select::input(3, "trigger")).await;
    panel.drag(from, to, modifiers::NONE).await;
    app.wait_for_config("the raid wired into it", |config| {
        card(config, "raid-alert").wires.contains(&wire((1, "event"), (3, "trigger")))
    })
    .await;
    panel.click("#card-test").await;
    let values = overlay
        .wait_for("confetti", |command| match command {
            Command::Plugin(command) => match &command.message {
                PluginMessage::Run { values, .. } => Some(values.clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(values["from"], "top");
    assert_eq!(values["pieces"], 150.0);
    assert_eq!(values["rainbow"], true);

    // Plugins lists them, and why one didn't load; the header's dot says
    // a plugin needs a look.
    panel.go("plugins").await;
    panel.wait("why one didn't load", "document.getElementById('plugins-failures')").await;
    let text = |id: &str| format!("document.getElementById('{id}').innerText");
    let confetti: String = panel.eval(&text("plugin-confetti")).await;
    assert!(confetti.contains("0.1.0"), "{confetti}");
    let particles: String = panel.eval(&text("plugin-particles")).await;
    assert!(particles.contains("built in"), "{particles}");
    let failures: String = panel.eval(&text("plugins-failures")).await;
    assert!(failures.contains("broken") && failures.contains("isn't valid"), "{failures}");
    panel.click("#header-plugins").await;
    panel.wait("Plugins", "location.hash === '#/plugins'").await;
    panel.close().await;
}

/// A 1×1 red GIF.
const RED_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00,
    0x3B,
];

fn plugin_node(id: u32, plugin: &str, node: &str, settings: serde_json::Value) -> Node {
    Node {
        id,
        x: 300.0,
        y: 0.0,
        kind: NodeKind::Plugin {
            plugin: plugin.into(),
            node: node.into(),
            settings: settings.as_object().unwrap().clone(),
            changes: Default::default(),
            voice: Default::default(),
            ty: Default::default(),
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn built_in_plugins_draw_on_the_overlay() {
    // A Custom event "party" → green hearts (Particles) and a red picture
    // in the middle (Show media), the plugins that ship with the app.
    let mut config = test_config();
    config.cards.push(Card {
        id: "party".into(),
        name: "Party".into(),
        enabled: true,
        nodes: vec![
            Node { id: 1, x: 0.0, y: 0.0, kind: NodeKind::Custom { name: "party".into() } },
            plugin_node(
                2,
                "particles",
                "burst",
                serde_json::json!({ "shape": "hearts", "from": "middle", "count": 300, "size": 20,
                                    "colour": "#00ff00", "rainbow": false }),
            ),
            plugin_node(
                3,
                "media",
                "show",
                serde_json::json!({ "media": "red.gif", "position": "center", "height": 300, "animation": "none" }),
            ),
        ],
        wires: vec![wire((1, "event"), (2, "trigger")), wire((1, "event"), (3, "trigger"))],
    });
    let app = App::start("built_in_plugins_draw_on_the_overlay", config).await;
    std::fs::create_dir_all(app.dir.join("pictures")).unwrap();
    std::fs::write(app.dir.join("pictures").join("red.gif"), RED_GIF).unwrap();

    let overlay = Panel::open_overlay(&app, "?modules=particles,media").await;
    overlay
        .wait(
            "both pages loaded",
            "document.querySelectorAll('iframe.plugin[sandbox=allow-scripts][data-loaded]').length === 2",
        )
        .await;
    // Black behind the layers, so what they draw shows. (What's sent before
    // a plugin's page has loaded waits for it.)
    overlay.run("document.body.style.background = 'black'").await;
    let mut control = app.control().await;
    control.send(&ControlRequest::Event { name: "party".into(), data: Default::default() }).await;

    // Until both show (the hearts don't last long; a busy machine is slow,
    // so fire again while nothing shows).
    let path = app.dir.join("overlay.png");
    let (mut green, mut red) = (0, 0);
    for round in 1..=40 {
        if round % 10 == 0 && green == 0 && red == 0 {
            control.send(&ControlRequest::Event { name: "party".into(), data: Default::default() }).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let png = overlay.screenshot(&path).await;
        (green, red) = overlay
            .eval(&format!(
                "(async () => {{ const image = new Image(); image.src = 'data:image/png;base64,{png}'; \
                 await image.decode(); const canvas = document.createElement('canvas'); \
                 canvas.width = image.width; canvas.height = image.height; \
                 const context = canvas.getContext('2d'); context.drawImage(image, 0, 0); \
                 const d = context.getImageData(0, 0, canvas.width, canvas.height).data; let green = 0, red = 0; \
                 for (let i = 0; i < d.length; i += 4) {{ \
                   if (d[i + 1] > 180 && d[i] < 100 && d[i + 2] < 100) green++; \
                   if (d[i] > 100 && d[i + 1] < 60 && d[i + 2] < 60) red++; }} \
                 return [green, red]; }})()"
            ))
            .await;
        if green > 2000 && red > 10_000 {
            break;
        }
    }
    assert!(green > 2000, "only {green} pixels of hearts; see {}", path.display());
    // A 1×1 picture this big is smoothed into a glow, so not all of it is red.
    assert!(red > 10_000, "only {red} pixels of the picture; see {}", path.display());
    overlay.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_alerts_page_shows_alerts_and_says_when_its_done() {
    // A Custom event → an alert in a style with a green box, from the
    // Alerts plugin's page (Rust, in its sandbox).
    let mut config = test_config();
    let alert =
        plugin_node(2, "alert", "alert", serde_json::json!({ "message": "Hello {colour}", "duration": 1 }));
    config.cards.push(custom_into("party", "party", alert));
    config.plugins.insert(
        "alert".into(),
        protocol::config::PluginConfig {
            presets: vec![protocol::config::Preset {
                id: "default".into(),
                name: "Green".into(),
                settings: serde_json::json!({ "background": "#00ff00", "background_opacity": 1.0,
                                              "animation": "none", "position": "center" })
                .as_object()
                .unwrap()
                .clone(),
            }],
            ..Default::default()
        },
    );
    let app = App::start("the_alerts_page_shows_alerts_and_says_when_its_done", config).await;
    let overlay = Panel::open_overlay(&app, "?modules=alert").await;
    overlay.wait("its page loaded", "document.querySelector('iframe#plugin-alert[data-loaded]')").await;
    overlay.run("document.body.style.background = 'black'").await;
    let mut control = app.control().await;
    let data = serde_json::json!({ "colour": "green" }).as_object().unwrap().clone();
    control.send(&ControlRequest::Event { name: "party".into(), data }).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = app.dir.join("alert.png");
    let png = overlay.screenshot(&path).await;
    let green: u32 = overlay
        .eval(&format!(
            "(async () => {{ const image = new Image(); image.src = 'data:image/png;base64,{png}'; \
             await image.decode(); const canvas = document.createElement('canvas'); \
             canvas.width = image.width; canvas.height = image.height; \
             const context = canvas.getContext('2d'); context.drawImage(image, 0, 0); \
             const d = context.getImageData(0, 0, canvas.width, canvas.height).data; let green = 0; \
             for (let i = 0; i < d.length; i += 4) {{ \
               if (d[i + 1] > 200 && d[i] < 60 && d[i + 2] < 60) green++; }} \
             return green; }})()"
        ))
        .await;
    assert!(green > 5_000, "only {green} pixels of the alert's box; see {}", path.display());

    // After its second, the page says it's done: the queue is empty.
    control
        .wait_for_state("the alert played", |state| state.queue.waiting == 0 && !state.queue.playing)
        .await;
    overlay.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn effects_are_on_the_plugins_tab() {
    let mut config = test_config();
    config.plugins.insert(
        "rain".into(),
        protocol::config::PluginConfig {
            settings: [("emote_height_px".to_string(), serde_json::json!(35))].into_iter().collect(),
            ..Default::default()
        },
    );
    let app = App::start("effects_are_on_the_plugins_tab", config).await;
    // The Effects page's link opens the Plugins tab.
    let panel = Panel::open(&app, "effects").await;
    panel.resize(380, 600).await;
    panel.wait("the Plugins tab", "document.querySelector('.tab.current')?.textContent === 'Plugins'").await;
    // The boxes are folded to their head: the switch works, the title
    // opens it.
    panel.wait("the rain's box", "document.getElementById('plugin-rain-open')").await;
    assert_eq!(panel.count("[id^='plugin-rain-setting-']").await, 0);
    panel.click("#plugin-rain-on").await;
    app.wait_for_config("the rain off", |config| !config.plugin("rain").enabled).await;
    panel.click("#plugin-rain-open").await;
    panel.wait("the rain's settings", "document.getElementById('plugin-rain-setting-emote_height_px')").await;
    panel.wait("the rain's preview", "document.getElementById('plugin-rain-preview')").await;
    // The app's sound and speech settings are on the Sounds and Speak boxes.
    panel.click("#plugin-sound-open").await;
    panel.wait("the master volume", "document.querySelector('#plugin-sound #sound-app #sound-volume')").await;
    panel.click("#plugin-speak-open").await;
    panel.wait("text to speech", "document.querySelector('#plugin-speak #speech-app #speech-voice')").await;
    // Nothing needs a look: no Plugins dot. Status has no plugins.
    assert_eq!(panel.count("#header-plugins").await, 0);
    panel.go("status").await;
    panel.wait("Status", "document.getElementById('control')").await;
    assert_eq!(panel.count("#status-plugins").await, 0);

    // A link to a plugin opens Plugins at its box, opened.
    panel.go("plugins/alert").await;
    panel.wait("the alerts' box open", "document.getElementById('plugin-alert-body')").await;
    panel.go("plugins/rain").await;
    panel
        .wait(
            "the rain's box in view",
            "(() => { const box = document.getElementById('plugin-rain'); \
             if (!box) return false; const top = box.getBoundingClientRect().top; \
             const below = document.querySelector('.dock-header').getBoundingClientRect().bottom; \
             return top >= below && top < innerHeight; })()",
        )
        .await;

    // Reset puts its settings back to the defaults.
    panel.click("#plugin-rain-reset").await;
    app.wait_for_config("the rain's settings reset", |config| config.plugin("rain").settings.is_empty())
        .await;
    panel.wait("nothing left to reset", "document.getElementById('plugin-rain-reset').disabled").await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_rain_plugin_draws_in_its_sandbox() {
    // The Rain plugin's page is Rust (WebAssembly) in a sandboxed iframe:
    // its module script, its .wasm and the images it decodes are
    // cross-origin requests from there.
    let app = App::start("the_rain_plugin_draws_in_its_sandbox", test_config()).await;
    std::fs::create_dir_all(app.dir.join("pictures")).unwrap();
    std::fs::write(app.dir.join("pictures").join("red.gif"), RED_GIF).unwrap();
    let overlay = Panel::open_overlay(&app, "?modules=rain").await;
    overlay.wait("its layer", "document.querySelector('iframe#plugin-rain[sandbox=allow-scripts]')").await;
    overlay.run("document.body.style.background = 'black'").await;
    // A run as the shell passes it on, with a picture standing in for an
    // emote (the test app knows no emotes).
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    overlay
        .run(
            "document.getElementById('plugin-rain').contentWindow.postMessage({ type: 'run', node: 'rain',              input: 'trigger', values: { emotes: [{ code: 'Red', url: '/pictures/red.gif', overlays: [] }],              count: 40 }, issued_at_ms: Date.now() }, '*')",
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;

    let path = app.dir.join("rain.png");
    let png = overlay.screenshot(&path).await;
    let red: u32 = overlay
        .eval(&format!(
            "(async () => {{ const image = new Image(); image.src = 'data:image/png;base64,{png}';              await image.decode(); const canvas = document.createElement('canvas');              canvas.width = image.width; canvas.height = image.height;              const context = canvas.getContext('2d'); context.drawImage(image, 0, 0);              const d = context.getImageData(0, 0, canvas.width, canvas.height).data; let red = 0;              for (let i = 0; i < d.length; i += 4) {{ if (d[i] > 100 && d[i + 1] < 60 && d[i + 2] < 60) red++; }}              return red; }})()"
        ))
        .await;
    assert!(red > 20_000, "only {red} pixels of fallen emotes; see {}", path.display());
    overlay.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn queued_plugin_effects_wait_for_their_page() {
    // Show media set to wait in the queue, for a second each.
    let mut config = test_config();
    config.cards.push(Card {
        id: "queued".into(),
        name: "Queued".into(),
        enabled: true,
        nodes: vec![
            Node { id: 1, x: 0.0, y: 0.0, kind: NodeKind::Custom { name: "show".into() } },
            plugin_node(
                2,
                "media",
                "show",
                serde_json::json!({ "media": "red.gif", "animation": "none", "seconds": 1, "queue": true }),
            ),
        ],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    });
    let app = App::start("queued_plugin_effects_wait_for_their_page", config).await;
    std::fs::create_dir_all(app.dir.join("pictures")).unwrap();
    std::fs::write(app.dir.join("pictures").join("red.gif"), RED_GIF).unwrap();
    let mut control = app.control().await;
    for _ in 0..2 {
        control.send(&ControlRequest::Event { name: "show".into(), data: Default::default() }).await;
    }
    // No overlay shows the media page yet: both wait.
    control.wait_for_state("two waiting", |state| state.queue.waiting == 2 && !state.queue.playing).await;

    // An overlay with only that page (no alerts) plays them one at a time,
    // and its page says when each is done (long before the queue would
    // give up on it).
    let overlay = Panel::open_overlay(&app, "?modules=media").await;
    control
        .wait_for_state("the first playing", |state| state.queue.playing && state.queue.waiting == 1)
        .await;
    control.wait_for_state("both played", |state| !state.queue.playing && state.queue.waiting == 0).await;
    overlay.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_program_connects_as_a_plugin() {
    // A card made for a program's plugin: its membership event → its own
    // Shout effect, with a text filled from the event.
    let mut config = test_config();
    config.cards.push(Card {
        id: "billy".into(),
        name: "Billy".into(),
        enabled: true,
        nodes: vec![
            plugin_node(1, "billybilly", "membership", serde_json::json!({})),
            plugin_node(
                2,
                "billybilly",
                "shout",
                serde_json::json!({ "message": "{viewer} for {months} months" }),
            ),
            plugin_node(3, "billybilly", "shout", serde_json::json!({ "message": "{viewer} ended" })),
        ],
        wires: vec![wire((1, "event"), (2, "trigger")), wire((1, "ended"), (3, "trigger"))],
    });
    let mut app = App::start("a_program_connects_as_a_plugin", config).await;
    let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
        "id": "billybilly", "name": "BillyBilly", "version": "1.0.0", "api": 1,
        "icon": "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><circle cx=\"12\" cy=\"12\" r=\"10\"/></svg>",
        "nodes": [
            { "id": "membership", "title": "BillyBilly membership", "category": "events", "outputs": [
                { "name": "event", "label": "Event", "type": "event" },
                { "name": "ended", "label": "Ended", "type": "event" },
                { "name": "viewer", "label": "Viewer", "type": "text" },
                { "name": "months", "label": "Months", "type": "number" }
            ] },
            { "id": "shout", "title": "Shout", "category": "effects",
              "inputs": [
                { "name": "trigger", "label": "Trigger", "type": "event" },
                { "name": "message", "label": "Message", "type": "text" }
              ],
              "settings": [{ "key": "message", "label": "Message", "type": "text", "default": "{viewer}!" }] }
        ]
    }))
    .unwrap();

    // Registering on the control API's WebSocket.
    let mut program = app.control().await;
    let mut bad = manifest.clone();
    bad.id = "Not An Id".into();
    program.send(&ControlRequest::Register { plugin: Box::new(bad) }).await;
    let refused = program
        .wait_for("a refusal", |message| match message {
            ControlMessage::Error { status, error } => Some((*status, error.clone())),
            _ => None,
        })
        .await;
    assert_eq!(refused.0, 400, "{refused:?}");
    program.send(&ControlRequest::Register { plugin: Box::new(manifest.clone()) }).await;
    let id = program
        .wait_for("registered", |message| match message {
            ControlMessage::Registered { plugin } => Some(plugin.clone()),
            _ => None,
        })
        .await;
    assert_eq!(id, "billybilly");

    // Its event runs the card, and its effect comes back to it.
    let values = serde_json::json!({ "viewer": "Ann", "months": 3 }).as_object().unwrap().clone();
    program.send(&ControlRequest::Emit { node: "membership".into(), output: None, values }).await;
    let run = |message: &ControlMessage| match message {
        ControlMessage::Run { node, values, .. } if node == "shout" => Some(values.clone()),
        _ => None,
    };
    let values = program.wait_for("its effect", run).await;
    assert_eq!(values["message"]["text"], "Ann for 3 months");
    // Another of its event outputs; one it hasn't is refused.
    let values = serde_json::json!({ "viewer": "Bo", "months": 1 }).as_object().unwrap().clone();
    let emit = |output: &str| ControlRequest::Emit {
        node: "membership".into(),
        output: Some(output.into()),
        values: values.clone(),
    };
    program.send(&emit("viewer")).await;
    let refused = program
        .wait_for("a refusal", |message| match message {
            ControlMessage::Error { status, error } => Some((*status, error.clone())),
            _ => None,
        })
        .await;
    assert!(refused.0 == 404 && refused.1.contains("no event output \"viewer\""), "{refused:?}");
    program.send(&emit("ended")).await;
    let values = program.wait_for("its Ended", run).await;
    assert_eq!(values["message"]["text"], "Bo ended");

    // The panel shows it on Plugins, and "Test this card" makes one up.
    let panel = Panel::open(&app, "plugins").await;
    panel
        .wait(
            "the program's plugin",
            "document.getElementById('plugin-billybilly')?.innerText.includes('1.0.0, a program, connected')",
        )
        .await;
    panel.go("card/billy").await;
    panel.wait("the card's nodes", "document.querySelectorAll('.node').length === 3").await;
    // Its icon, the SVG it registered, served by the app.
    panel
        .wait(
            "its icons",
            "document.querySelectorAll('.node .plugin-icon[aria-label=BillyBilly]').length === 3",
        )
        .await;
    panel.run("fetch('/plugin-icons/billybilly').then(r => r.text()).then(t => window.icon = t)").await;
    panel.wait("the icon served", "window.icon?.startsWith('<svg')").await;
    panel.click("#card-test").await;
    let values = program.wait_for("the test", run).await;
    assert_eq!(values["message"]["text"], "TestViewer for 5 months");

    // Gone, it's remembered: the card keeps its nodes, even after a restart.
    drop(program);
    panel.go("plugins").await;
    panel
        .wait(
            "not connected",
            "document.getElementById('plugin-billybilly')?.innerText.includes('1.0.0, a program, not connected')",
        )
        .await;
    panel.close().await;
    app.restart().await;
    let panel = open_card(&app, "billy", 3).await;
    assert_eq!(panel.count(".node.problem, .canvas-problems").await, 0, "no plugin missing");
    let title: String = panel.eval("document.querySelector('.node').innerText").await;
    assert!(title.contains("BillyBilly membership"), "{title}");
    panel.close().await;
}

fn alert_node(id: u32, message: &str) -> Node {
    Node { id, x: 300.0, y: 0.0, kind: alert_kind(message, 5.0) }
}

/// An Alerts plugin node: `message` for `duration` seconds.
fn alert_kind(message: &str, duration: f64) -> NodeKind {
    plugin_node(0, "alert", "alert", serde_json::json!({ "message": message, "duration": duration })).kind
}

/// A card: a Custom event `name` into a plugin's effect node.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn plugin_presets_are_made_on_plugins_and_picked_by_nodes() {
    let mut config = test_config();
    let show = plugin_node(2, "banner", "show", serde_json::json!({}));
    config.cards.push(custom_into("banner", "banner", show));
    let app = App::start("plugin_presets_are_made_on_plugins_and_picked_by_nodes", config).await;
    let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
        "id": "banner", "name": "Banner", "version": "1.0.0", "api": 1,
        "presets": { "label": "Style" },
        "settings": [
            { "key": "colour", "label": "Colour", "type": "colour", "default": "#ffffff" },
            { "key": "where", "label": "Position", "type": "position", "default": "top" },
            { "key": "font", "label": "Font", "type": "text", "suggestions": ["Arial", "Impact"],
              "placeholder": "The system's" }
        ],
        "nodes": [{ "id": "show", "title": "Banner", "category": "effects",
            "inputs": [{ "name": "trigger", "label": "Trigger", "type": "event" }],
            "settings": [{ "key": "style", "label": "Style", "type": "preset" }] }]
    }))
    .unwrap();
    let mut program = app.control().await;
    program.send(&ControlRequest::Register { plugin: Box::new(manifest) }).await;
    program
        .wait_for("registered", |message| matches!(message, ControlMessage::Registered { .. }).then_some(()))
        .await;
    let presets = |config: &AppConfig| config.plugin("banner").presets();

    // Plugins: the first is "Default" and can't be deleted (its ⋯ menu
    // says so); Duplicate makes a copy to edit, its name open to type over.
    let panel = Panel::open(&app, "plugins/banner").await;
    panel.wait("the style picker", "document.getElementById('plugin-banner-preset')").await;
    panel.click("#plugin-banner-preset-more").await;
    panel.wait("the menu", "document.getElementById('plugin-banner-preset-delete')").await;
    let first: bool = panel.eval("document.getElementById('plugin-banner-preset-delete').disabled").await;
    assert!(first, "the first can't be deleted");
    panel.key(Key::ESCAPE, modifiers::NONE).await;
    panel.wait("the menu closed", "!document.getElementById('plugin-banner-preset-delete')").await;
    panel.click("#plugin-banner-preset-duplicate").await;
    app.wait_for_config("a copy", |config| presets(config).len() == 2).await;
    panel
        .wait(
            "the copy picked, its name open and picked to type over",
            "(() => { const name = document.getElementById('plugin-banner-preset-name'); \
              return name?.value === 'Default copy' && document.activeElement === name \
                && name.selectionStart === 0 && name.selectionEnd === name.value.length; })()",
        )
        .await;
    panel
        .run(
            "const name = document.getElementById('plugin-banner-preset-name'); name.value = 'Loud'; \
             name.dispatchEvent(new InputEvent('input', { bubbles: true }));\
             const colour = document.getElementById('plugin-banner-setting-colour'); colour.value = '#ff0000'; \
             colour.dispatchEvent(new InputEvent('input', { bubbles: true }));",
        )
        .await;
    let config = app
        .wait_for_config("named and coloured", |config| {
            presets(config)[1].name == "Loud"
                && presets(config)[1].settings.get("colour") == Some(&"#ff0000".into())
        })
        .await;
    assert!(presets(&config)[0].settings.is_empty(), "the default didn't change");
    // A position on its grid; a text with suggestions.
    panel.click("#plugin-banner-setting-where .position-dot:nth-child(7)").await;
    app.wait_for_config("bottom left", |config| {
        presets(config)[1].settings.get("where") == Some(&"bottom_left".into())
    })
    .await;
    // A text with suggestions is a dropdown: empty is the placeholder, and
    // "Other…" shows a box for any other value.
    let fonts: Vec<String> = panel
        .eval("[...document.getElementById('plugin-banner-setting-font').options].map(o => o.text)")
        .await;
    assert_eq!(fonts, ["The system's", "Arial", "Impact", "Other…"]);
    panel
        .run(
            "const pick = document.getElementById('plugin-banner-setting-font'); pick.value = 'Impact';              pick.dispatchEvent(new Event('change', { bubbles: true }));",
        )
        .await;
    app.wait_for_config("Impact picked", |config| {
        presets(config)[1].settings.get("font") == Some(&"Impact".into())
    })
    .await;
    panel
        .run(
            "const pick = document.getElementById('plugin-banner-setting-font');              pick.value = pick.options[pick.options.length - 1].value;              pick.dispatchEvent(new Event('change', { bubbles: true }));",
        )
        .await;
    panel.wait("the box for another", "document.getElementById('plugin-banner-setting-font-other')").await;
    panel
        .run(
            "const other = document.getElementById('plugin-banner-setting-font-other'); other.value = 'Comic Neue';              other.dispatchEvent(new InputEvent('input', { bubbles: true }));",
        )
        .await;
    let config = app
        .wait_for_config("another font", |config| {
            presets(config)[1].settings.get("font") == Some(&"Comic Neue".into())
        })
        .await;
    panel
        .wait(
            "still typing",
            "document.getElementById('plugin-banner-setting-font-other').value === 'Comic Neue'              && document.getElementById('plugin-banner-setting-font').selectedOptions[0].text === 'Other…'",
        )
        .await;
    let loud = presets(&config)[1].id.clone();
    panel.close().await;

    // The node picks it; its run brings its settings.
    let panel = open_card(&app, "banner", 2).await;
    panel.wait("the node's style", "document.getElementById('node-2-plugin-style')").await;
    let options: Vec<String> =
        panel.eval("[...document.getElementById('node-2-plugin-style').options].map(o => o.text)").await;
    assert_eq!(options, ["Default", "Loud"]);
    panel
        .run(&format!(
            "const style = document.getElementById('node-2-plugin-style'); style.value = '{loud}'; \
             style.dispatchEvent(new Event('change', {{ bubbles: true }}));"
        ))
        .await;
    app.wait_for_config("Loud picked", |config| {
        matches!(&card(config, "banner").node(2).unwrap().kind,
            NodeKind::Plugin { settings, .. } if settings.get("style") == Some(&loud.clone().into()))
    })
    .await;
    panel.close().await;
    program.send(&ControlRequest::Event { name: "banner".into(), data: Default::default() }).await;
    let settings = program
        .wait_for("the run", |message| match message {
            ControlMessage::Run { node, settings, .. } if node == "show" => Some(settings.clone()),
            _ => None,
        })
        .await;
    assert_eq!(settings["colour"], "#ff0000");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_plugin_page_loads_from_hosts_once_allowed() {
    let mut app = App::start("a_plugin_page_loads_from_hosts_once_allowed", test_config()).await;
    // The confetti example, wanting fonts from Google.
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/confetti");
    let folder = app.dir.join("plugins").join("fonty");
    copy_dir(&example, &folder);
    let manifest = std::fs::read_to_string(folder.join("plugin.json")).unwrap();
    let manifest = manifest.replace(
        "\"id\": \"confetti\",",
        "\"id\": \"fonty\", \"page_hosts\": [\"fonts.googleapis.com\", \"fonts.gstatic.com\"],",
    );
    std::fs::write(folder.join("plugin.json"), manifest).unwrap();
    app.restart().await;

    let panel = Panel::open(&app, "plugins/fonty").await;
    let policy = || {
        panel.eval::<String>(
            "fetch('/plugins/fonty/index.html').then(r => r.headers.get('content-security-policy'))",
        )
    };
    let before = policy().await;
    assert!(!before.contains("googleapis"), "{before}");
    panel.wait("the hosts to allow", "document.getElementById('plugin-fonty-allow-page')").await;
    panel.click("#plugin-fonty-allow-page").await;
    app.wait_for_config("allowed", |config| config.plugin("fonty").page_hosts.len() == 2).await;
    let after = policy().await;
    assert!(
        after.contains("default-src")
            && after.contains("https://fonts.googleapis.com https://fonts.gstatic.com"),
        "{after}"
    );
    panel.wait("taken back", "document.getElementById('plugin-fonty-disallow-page')").await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_test_page_sends_chat_from_a_named_viewer_and_keeps_its_texts() {
    let app =
        App::start("the_test_page_sends_chat_from_a_named_viewer_and_keeps_its_texts", test_config()).await;
    // The chat overlay hosts the chat display, which hears the chat.
    let mut chat = app.named_overlay("chat").await;
    let panel = Panel::open(&app, "test").await;
    panel.wait("the viewer", "document.getElementById('test-viewer')").await;
    let viewer: String = panel.eval("document.getElementById('test-viewer').value").await;
    assert_eq!(viewer, "TestViewer");

    // A name typed is kept (once the field is left), and the message comes
    // from that viewer.
    panel.click("#test-viewer").await;
    panel.run("document.getElementById('test-viewer').select()").await;
    panel.type_text("Ann").await;
    panel.run("document.activeElement.blur()").await;
    app.wait_for_config("the viewer kept", |config| config.test.viewer.as_deref() == Some("Ann")).await;
    panel.click("#test-chat-send").await;
    let from = chat
        .wait_for("Ann's message", |command| match command {
            Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Chat { line } })
                if plugin == "chat" =>
            {
                line.user.as_ref().map(|user| user.display_name.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(from, "Ann");
    panel
        .wait(
            "listed as sent",
            "document.querySelector('#test-sent .sent-line')?.textContent.endsWith('Chat message from Ann')",
        )
        .await;

    // Emptied, a text stays empty, its starting one the placeholder, and
    // that's who the message comes from; the reward's is kept too.
    let set = |id: &str, text: &str| {
        format!(
            "const f = document.getElementById('{id}'); f.value = {}; \
              f.dispatchEvent(new Event('change', {{ bubbles: true }}))",
            js_string(text)
        )
    };
    panel.run(&set("test-viewer", "")).await;
    app.wait_for_config("the viewer emptied", |config| config.test.viewer.as_deref() == Some("")).await;
    panel
        .wait(
            "the viewer still empty",
            "document.getElementById('test-viewer').value === '' \
              && document.getElementById('test-viewer').placeholder === 'TestViewer'",
        )
        .await;
    panel.click("#test-chat-send").await;
    chat.wait_for("TestViewer's message", |command| match command {
        Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Chat { line } })
            if plugin == "chat" =>
        {
            line.user.as_ref().filter(|user| user.display_name == "TestViewer").map(|_| ())
        }
        _ => None,
    })
    .await;
    let reward: String = panel.eval("document.getElementById('test-reward-text').value").await;
    assert_eq!(reward, "Stay hydrated!");
    panel.run(&set("test-reward-text", "one more")).await;
    app.wait_for_config("the reward's text kept", |config| {
        config.test.reward_text.as_deref() == Some("one more")
    })
    .await;

    // The Events' Message, kept too, goes with a sub and a cheer; emptied,
    // they come without one.
    let message: String = panel.eval("document.getElementById('test-event-message').value").await;
    assert_eq!(message, protocol::config::TEST_EVENT_MESSAGE);
    panel.run(&set("test-event-message", "Hype hype")).await;
    app.wait_for_config("the events' message kept", |config| {
        config.test.event_message.as_deref() == Some("Hype hype")
    })
    .await;
    panel
        .run(
            "const socket = window.__panelSockets.at(-1); window.__tests = []; \
              socket.send = data => { const request = JSON.parse(data); \
                if (request.type === 'test') window.__tests.push([request.event.event, request.event.message]); \
                WebSocket.prototype.send.call(socket, data); };",
        )
        .await;
    let sub_and_bits = async || -> String {
        panel.run("window.__tests = []").await;
        panel.click("#test-sub-send").await;
        panel.click("#test-bits-send").await;
        panel.wait("both sent", "window.__tests.length === 2").await;
        panel.eval("JSON.stringify(window.__tests)").await
    };
    assert_eq!(sub_and_bits().await, r#"[["sub","Hype hype"],["bits","Cheer500 Hype hype"]]"#);
    panel.run(&set("test-event-message", "")).await;
    app.wait_for_config("the events' message emptied", |config| {
        config.test.event_message.as_deref() == Some("")
    })
    .await;
    assert_eq!(sub_and_bits().await, r#"[["sub",""],["bits","Cheer500"]]"#);
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_text_being_typed_survives_a_redraw() {
    let app = App::start_debug("a_text_being_typed_survives_a_redraw", test_config()).await;
    let _overlay = app.overlay_holding(vec![]).await;
    let panel = Panel::open(&app, "test").await;
    panel.wait("the events' message", "document.getElementById('test-event-message')").await;

    // Emptied and typed in, not left yet: the page redrawn meanwhile (the
    // queue's count in the tabs) keeps what's typed, not the saved text
    // (2026-10-15: it came back).
    panel.click("#test-event-message").await;
    panel.run("document.getElementById('test-event-message').select()").await;
    panel.type_text("Hype").await;
    panel.input(raid("Alpha", 5)).await;
    panel.input(raid("Bravo", 6)).await;
    panel
        .wait(
            "a redraw",
            "[...document.querySelectorAll('a.tab')].some(tab => tab.textContent.startsWith('Activity ('))",
        )
        .await;
    let typed: String = panel.eval("document.getElementById('test-event-message').value").await;
    assert_eq!(typed, "Hype");

    // Saved once left.
    panel.run("document.activeElement.blur()").await;
    app.wait_for_config("the message saved", |config| config.test.event_message.as_deref() == Some("Hype"))
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_plugin_page_hears_the_chat_once_allowed() {
    let mut app = App::start("a_plugin_page_hears_the_chat_once_allowed", test_config()).await;
    // The confetti example, wanting to hear the chat.
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/confetti");
    let folder = app.dir.join("plugins").join("listener");
    copy_dir(&example, &folder);
    let manifest = std::fs::read_to_string(folder.join("plugin.json")).unwrap();
    let manifest = manifest.replace("\"id\": \"confetti\",", "\"id\": \"listener\", \"hears\": [\"chat\"],");
    std::fs::write(folder.join("plugin.json"), manifest).unwrap();
    app.restart().await;
    let mut overlay = app.overlay(vec![]).await;

    let panel = Panel::open(&app, "test").await;
    let say = |text: &str| {
        format!(
            "const chat = document.getElementById('test-chat'); chat.value = {};              chat.dispatchEvent(new InputEvent('input', {{ bubbles: true }}));",
            js_string(text)
        )
    };
    panel.wait("the chat box", "document.getElementById('test-chat')").await;
    panel.run(&say("not yet")).await;
    panel.click("#test-chat-send").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    panel.go("plugins/listener").await;
    panel.wait("asked", "document.getElementById('plugin-listener-allow-hears')").await;
    panel.click("#plugin-listener-allow-hears").await;
    app.wait_for_config("allowed", |config| config.plugin("listener").hears == ["chat"]).await;
    panel.wait("allowed", "document.getElementById('plugin-listener-disallow-hears')").await;

    panel.go("test").await;
    panel.wait("the chat box", "document.getElementById('test-chat')").await;
    panel.run(&say("!hello there")).await;
    panel.click("#test-chat-send").await;
    let text = overlay
        .wait_for("a chat line for the page", |command| match command {
            Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Chat { line } })
                if plugin == "listener" =>
            {
                Some(
                    line.parts
                        .iter()
                        .map(|part| match part {
                            protocol::AlertPart::Text { text, .. } => text.as_str(),
                            protocol::AlertPart::Emote { code, .. } => code.as_str(),
                        })
                        .collect::<String>(),
                )
            }
            _ => None,
        })
        .await;
    assert_eq!(text, "!hello there", "the first it hears is after Allow, commands too");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_plugin_page_gets_back_what_it_kept() {
    let mut app = App::start("a_plugin_page_gets_back_what_it_kept", test_config()).await;
    // The confetti example with a page that keeps something as it loads.
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/confetti");
    let folder = app.dir.join("plugins").join("memo");
    copy_dir(&example, &folder);
    let manifest = std::fs::read_to_string(folder.join("plugin.json")).unwrap();
    std::fs::write(
        folder.join("plugin.json"),
        manifest.replace("\"id\": \"confetti\",", "\"id\": \"memo\","),
    )
    .unwrap();
    std::fs::write(
        folder.join("overlay").join("index.html"),
        "<!doctype html><script>parent.postMessage({ type: 'keep', value: { said: 'hello' } }, '*');</script>",
    )
    .unwrap();
    app.restart().await;

    let page = Panel::open_overlay(&app, "?modules=memo").await;
    page.wait("its page loaded", "document.querySelector('iframe#plugin-memo[data-loaded]')").await;
    let kept = |command: &Command| match command {
        Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Kept { value } })
            if plugin == "memo" =>
        {
            Some(value.clone())
        }
        _ => None,
    };
    // An overlay that loads it later gets it back.
    let mut later = None;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut overlay = app.overlay(vec![]).await;
        later = overlay.wait_at_most(std::time::Duration::from_millis(300), &kept).await;
        if later.is_some() {
            break;
        }
    }
    assert_eq!(later, Some(serde_json::json!({ "said": "hello" })));
    page.close().await;

    // And after the app restarts (it's saved a moment after it changes).
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    app.restart().await;
    let mut overlay = app.overlay(vec![]).await;
    assert_eq!(overlay.wait_for("what it kept", kept).await, serde_json::json!({ "said": "hello" }));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_plugin_file_setting_picks_a_look_and_copies_are_files() {
    let mut app = App::start("a_plugin_file_setting_picks_a_look_and_copies_are_files", test_config()).await;
    // The confetti example with a stylesheet setting and looks of its own.
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/confetti");
    let folder = app.dir.join("plugins").join("styled");
    copy_dir(&example, &folder);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("plugin.json")).unwrap()).unwrap();
    manifest["id"] = "styled".into();
    manifest["settings"] = serde_json::json!([{
        "key": "look", "label": "Look", "type": "file", "file": "look.css", "default": "looks/plain.css",
        "starts": [{ "value": "looks/fancy.css", "label": "Fancy" }]
    }]);
    std::fs::write(folder.join("plugin.json"), manifest.to_string()).unwrap();
    std::fs::create_dir_all(folder.join("overlay").join("looks")).unwrap();
    std::fs::write(folder.join("overlay").join("looks").join("plain.css"), ".plain {}").unwrap();
    std::fs::write(folder.join("overlay").join("looks").join("fancy.css"), ".fancy {}").unwrap();
    // The one file it had before 2026-10-14: one of the looks now, picked.
    let data = app.dir.join("plugin_data").join("styled");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("look.css"), ".old {}").unwrap();
    app.restart().await;
    let copies = data.join("look");
    assert_eq!(std::fs::read_to_string(copies.join("Custom.css")).unwrap(), ".old {}");
    assert!(!data.join("look.css").exists());
    let picks =
        |config: &AppConfig, name: &str| config.plugin("styled").settings.get("look") == Some(&name.into());
    app.wait_for_config("the moved file picked", |config| picks(config, "Custom")).await;

    let mut overlay = app.overlay(vec![]).await;
    let look = |command: &Command| match command {
        Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Settings { values } })
            if plugin == "styled" =>
        {
            values["look"].as_str().map(str::to_string)
        }
        _ => None,
    };
    assert_eq!(overlay.wait_for("the moved look", look).await, ".old {}");

    // The picker: the plugin's own looks ("Default", as its default isn't one
    // of them), then the user's.
    let panel = Panel::open(&app, "plugins/styled").await;
    panel
        .wait("the picker", "document.getElementById('plugin-styled-setting-look')?.value === 'Custom'")
        .await;
    let groups: Vec<Vec<String>> = panel
        .eval(
            "[...document.querySelectorAll('#plugin-styled-setting-look optgroup')] \
             .map(group => [group.label, ...[...group.children].map(option => option.text)])",
        )
        .await;
    assert_eq!(groups, [vec!["Built in", "Default", "Fancy"], vec!["Yours", "Custom"]]);
    panel
        .run(
            "const s = document.getElementById('plugin-styled-setting-look'); s.value = 'Fancy'; \
             s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("Fancy picked", |config| picks(config, "Fancy")).await;
    assert_eq!(overlay.wait_for("the picked look", look).await, ".fancy {}");

    // ✎: its text. Typing in one of the plugin's own makes a copy, picked
    // once saved.
    panel.click("#plugin-styled-setting-look-edit").await;
    let text = |text: &str| {
        format!("document.getElementById('plugin-styled-setting-look-text')?.value === {}", js_string(text))
    };
    panel.wait("its text", &text(".fancy {}")).await;
    assert!(
        panel.eval::<bool>("!document.getElementById('plugin-styled-setting-look-name')").await,
        "not a copy"
    );
    panel
        .run(
            "const t = document.getElementById('plugin-styled-setting-look-text'); t.value = '.fancy { color: red }'; \
             t.dispatchEvent(new Event('input', { bubbles: true }))",
        )
        .await;
    panel
        .wait(
            "a copy",
            "document.getElementById('plugin-styled-setting-look').value === 'Fancy copy' \
             && document.getElementById('plugin-styled-setting-look').disabled \
             && document.getElementById('plugin-styled-setting-look-name')?.value === 'Fancy copy'",
        )
        .await;
    assert!(!copies.join("Fancy copy.css").exists(), "not saved yet");
    panel.click("#plugin-styled-setting-look-save").await;
    app.wait_for_config("the copy picked", |config| picks(config, "Fancy copy")).await;
    assert_eq!(overlay.wait_for("the copy", look).await, ".fancy { color: red }");
    assert_eq!(std::fs::read_to_string(copies.join("Fancy copy.css")).unwrap(), ".fancy { color: red }");
    assert_eq!(
        std::fs::read_to_string(folder.join("overlay").join("looks").join("fancy.css")).unwrap(),
        ".fancy {}"
    );
    panel.wait("saved", "!document.getElementById('plugin-styled-setting-look').disabled").await;

    // Changed in another editor: the box and the page follow.
    std::fs::write(copies.join("Fancy copy.css"), ".edited {}").unwrap();
    assert_eq!(overlay.wait_for("the edited look", look).await, ".edited {}");
    panel.wait("the edit in the box", &text(".edited {}")).await;

    // Renamed: its file too, and still picked.
    panel
        .run(
            "const n = document.getElementById('plugin-styled-setting-look-name'); n.value = 'Mine'; \
             n.dispatchEvent(new Event('input', { bubbles: true })); n.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("renamed", |config| picks(config, "Mine")).await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !copies.join("Mine.css").exists() {
        assert!(std::time::Instant::now() < deadline, "the file wasn't renamed");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!copies.join("Fancy copy.css").exists());

    // Deleted (after a second click): the default again.
    panel.click("#plugin-styled-setting-look-delete").await;
    panel.click("#plugin-styled-setting-look-delete").await;
    assert_eq!(overlay.wait_for("the default again", look).await, ".plain {}");
    assert!(!copies.join("Mine.css").exists());
    panel
        .wait(
            "the default picked",
            "document.getElementById('plugin-styled-setting-look').value === 'Default'",
        )
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_plugin_preview_can_have_its_own_size_and_samples() {
    let mut app = App::start("a_plugin_preview_can_have_its_own_size_and_samples", test_config()).await;
    // The confetti example with a page that says when it's asked for samples.
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/confetti");
    let folder = app.dir.join("plugins").join("sampler");
    copy_dir(&example, &folder);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("plugin.json")).unwrap()).unwrap();
    manifest["id"] = "sampler".into();
    manifest["preview"] = serde_json::json!({ "samples": true, "width": 500, "height": 700 });
    manifest["help"] = "help.html".into();
    std::fs::write(folder.join("plugin.json"), manifest.to_string()).unwrap();
    std::fs::write(folder.join("overlay").join("help.html"), "<!doctype html><p>How it works</p>").unwrap();
    std::fs::write(
        folder.join("overlay").join("index.html"),
        "<!doctype html><script>let drawn = 0; addEventListener('message', event => {          if (event.data.type === 'samples') parent.postMessage({ type: 'drew', drawn: ++drawn }, '*'); });</script>",
    )
    .unwrap();
    app.restart().await;
    let mut overlay = app.overlay(vec![]).await;

    // Listening before the Plugins page (and its preview) opens, so the
    // page's first message isn't missed.
    let panel = Panel::open(&app, "status").await;
    panel.resize(380, 800).await;
    panel.run("window.drawn = 0; addEventListener('message', event => { if (event.data.type === 'drew') window.drawn = event.data.drawn; });").await;
    panel.go("plugins/sampler").await;
    panel.wait("its preview", "document.getElementById('plugin-sampler-preview')").await;
    // Scrolled on before the page places its box again (400 ms after it
    // opens): it isn't taken back. (It was, under a click aimed at Show on
    // overlay.)
    let scrolled: f64 = panel.eval("(scrollBy(0, 100), scrollY)").await;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert_eq!(panel.eval::<f64>("scrollY").await, scrolled, "taken back to the box");
    // 500 × 700, scaled to the dock (a frame after it's laid out).
    panel
        .wait(
            "500 × 700, scaled",
            "(() => { const box = document.getElementById('plugin-sampler-preview').parentElement \
             .getBoundingClientRect(); return box.width < 380 && Math.abs(box.height / box.width - 700 / 500) < 0.03; })()",
        )
        .await;
    panel.wait("samples once it loaded", "window.drawn === 1").await;
    panel.click("#plugin-sampler-play").await;
    panel.wait("samples again for Play", "window.drawn === 2").await;
    // Its help, on its box, without scripts.
    let (sandbox, src): (String, String) = panel
        .eval("(() => { const frame = document.querySelector('#plugin-sampler-help iframe');                return [frame.getAttribute('sandbox'), frame.getAttribute('src')]; })()")
        .await;
    assert_eq!((sandbox.as_str(), src.as_str()), ("", "/plugins/sampler/help.html"));
    panel.click("#plugin-sampler-show").await;
    overlay
        .wait_for("samples on the overlay", |command| match command {
            Command::Plugin(protocol::PluginCommand { plugin, message: PluginMessage::Samples })
                if plugin == "sampler" =>
            {
                Some(())
            }
            _ => None,
        })
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_plugin_previews_on_its_plugins_box() {
    let app = App::start("a_plugin_previews_on_its_plugins_box", test_config()).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "plugins/particles").await;
    panel.resize(380, 800).await;
    panel.wait("the preview", "document.getElementById('plugin-particles-preview')").await;
    let (sandbox, src, width): (String, String, f64) = panel
        .eval(
            "(() => { const frame = document.getElementById('plugin-particles-preview'); \
             return [frame.getAttribute('sandbox'), frame.getAttribute('src'), \
                     frame.parentElement.getBoundingClientRect().width]; })()",
        )
        .await;
    assert_eq!((sandbox.as_str(), src.as_str()), ("allow-scripts", "/plugins/particles/index.html"));
    assert!(width < 380.0, "fits the dock: {width}");
    // Its height follows the box's width a frame after the dock narrows.
    panel
        .wait(
            "16:9",
            "(() => { const box = document.getElementById('plugin-particles-preview').parentElement \
             .getBoundingClientRect(); return Math.abs(box.height / box.width - 9 / 16) < 0.02; })()",
        )
        .await;
    // ▶ Play: its page draws in the preview (colours on the grey checks).
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    panel.click("#plugin-particles-play").await;
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let path = app.dir.join("preview.png");
    let png = panel.screenshot(&path).await;
    let coloured: u32 = panel
        .eval(&format!(
            "(async () => {{ const box = document.getElementById('plugin-particles-preview').parentElement \
             .getBoundingClientRect(); const ratio = devicePixelRatio; const image = new Image(); \
             image.src = 'data:image/png;base64,{png}'; await image.decode(); \
             const canvas = document.createElement('canvas'); canvas.width = image.width; canvas.height = image.height; \
             const context = canvas.getContext('2d'); context.drawImage(image, 0, 0); \
             const d = context.getImageData(box.left * ratio, box.top * ratio, box.width * ratio, box.height * ratio).data; \
             let coloured = 0; for (let i = 0; i < d.length; i += 4) {{ \
             const [r, g, b] = [d[i], d[i + 1], d[i + 2]]; \
             if (Math.max(r, g, b) - Math.min(r, g, b) > 60) coloured++; }} return coloured; }})()"
        ))
        .await;
    assert!(coloured > 200, "only {coloured} coloured pixels in the preview; see {}", path.display());

    // Show on overlay: the same run, on the overlays.
    panel.click("#plugin-particles-show").await;
    let values = overlay
        .wait_for("the preview's run", |command| match command {
            Command::Plugin(command) if command.plugin == "particles" => match &command.message {
                PluginMessage::Run { node, values, .. } if node == "burst" => Some(values.clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert!(values.contains_key("shape") && values["emotes"] == serde_json::json!([]), "{values:?}");
    panel.close().await;
}

/// `ms` of silence as a WAV file: 8 kHz, mono, 16 bits.
fn silence_wav(ms: u32) -> Vec<u8> {
    let data = ms * 16;
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    for value in [16u32, 0x0001_0001, 8000, 16000, 0x0010_0002] {
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    wav.resize(wav.len() + data as usize, 0);
    wav
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn plugins_read_aloud_in_their_own_voices() {
    // A program's plugin with a voice and a node that reads its Text
    // aloud; Host check's server code has voices too.
    let mut config = test_config();
    config.effects.speech.settings.voice = "robot:hum".into();
    let tell = plugin_node(2, "robot", "tell", serde_json::json!({ "text": "Hi {colour}" }));
    config.cards.push(custom_into("tell", "tell", tell.clone()));
    let mut beep = tell;
    if let NodeKind::Plugin { voice, .. } = &mut beep.kind {
        voice.insert("voice".into(), "host-check:beep".into());
    }
    config.cards.push(custom_into("beep", "beep", beep));
    config.plugins.insert(
        "host-check".into(),
        protocol::config::PluginConfig { hosts: vec!["127.0.0.1".into()], ..Default::default() },
    );
    let mut app = App::start("plugins_read_aloud_in_their_own_voices", config).await;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_dir(&root.join("plugins/host-check"), &app.dir.join("plugins").join("host-check"));
    app.restart().await;
    app.wait_running(&["host-check"]).await;

    let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
        "id": "robot", "name": "Robot", "version": "1.0.0", "api": 1,
        "voices": [{ "id": "hum", "name": "Hum" }],
        "nodes": [{ "id": "tell", "title": "Tell", "category": "effects",
            "inputs": [
                { "name": "trigger", "label": "Trigger", "type": "event" },
                { "name": "text", "label": "Text", "type": "text", "speak": true }
            ],
            "settings": [{ "key": "text", "label": "Text", "type": "text", "default": "Hi" }] }]
    }))
    .unwrap();
    let mut program = app.control().await;
    program.send(&ControlRequest::Register { plugin: Box::new(manifest) }).await;
    program
        .wait_for("registered", |message| matches!(message, ControlMessage::Registered { .. }).then_some(()))
        .await;
    let fire = |name: &str| {
        let data = serde_json::json!({ "colour": "red" }).as_object().unwrap().clone();
        ControlRequest::Event { name: name.into(), data }
    };
    let run = |message: &ControlMessage| match message {
        ControlMessage::Run { node, values, .. } if node == "tell" => Some(values["text"].clone()),
        _ => None,
    };

    // In its own voice: the app asks the program, which answers with a WAV.
    // (Fired again until the cards have the program's node types.)
    let mut asked = None;
    for _ in 0..20 {
        program.send(&fire("tell")).await;
        asked = program
            .wait_at_most(std::time::Duration::from_millis(250), |message| match message {
                ControlMessage::Speak { request, voice, text, .. } => {
                    Some((*request, voice.clone(), text.clone()))
                }
                _ => None,
            })
            .await;
        if asked.is_some() {
            break;
        }
    }
    let (request, voice, text) = asked.expect("asked to speak");
    assert_eq!((voice.as_str(), text.as_str()), ("hum", "Hi red"));
    use base64::Engine as _;
    let audio = base64::engine::general_purpose::STANDARD.encode(silence_wav(700));
    let spoken =
        ControlRequest::Spoken { request, audio: Some(audio), format: Some("wav".into()), error: None };
    program.send(&spoken).await;
    let text = program.wait_for("the run, with its message", run).await;
    assert_eq!(text["text"], "Hi red");
    assert_eq!(text["speech"]["duration_ms"], 700);
    let url = text["speech"]["url"].as_str().unwrap().to_string();

    // A node's own voice: Host check's server code makes it (once it runs).
    let mut beeped = None;
    for _ in 0..40 {
        program.send(&fire("beep")).await;
        let text = program.wait_for("the beep run", run).await;
        if text.get("speech").is_some() {
            beeped = Some(text);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let beeped = beeped.expect("Host check read it");
    assert_eq!(beeped["speech"]["duration_ms"], 500);

    // The messages are served; the voices are in the pickers, under their
    // plugins; the node has its voice change.
    let panel = Panel::open(&app, "plugins/speak").await;
    let (status, kind): (u16, String) =
        panel.eval(&format!("fetch('{url}').then(r => [r.status, r.headers.get('content-type')])")).await;
    assert_eq!((status, kind.as_str()), (200, "audio/wav"));
    panel.wait("the voice picker", "document.getElementById('speech-voice')").await;
    let voices: Vec<String> = panel
        .eval("[...document.querySelectorAll('#speech-voice optgroup')].map(g => g.label + ': ' + [...g.querySelectorAll('option')].map(o => o.value).join(' '))")
        .await;
    assert!(voices.contains(&"Robot: robot:hum".to_string()), "{voices:?}");
    assert!(voices.contains(&"Host check: host-check:beep host-check:broken".to_string()), "{voices:?}");
    let selected: String = panel.eval("document.getElementById('speech-voice').value").await;
    assert_eq!(selected, "robot:hum");
    let groups: Vec<String> = panel
        .eval("[...document.querySelectorAll('#speech-voice optgroup')].slice(0, 2).map(g => g.label)")
        .await;
    assert_eq!(groups, ["Kokoro · British", "Kokoro · American"]);
    // Picking a voice reads Try with it (the program's voice needs no
    // Kokoro).
    panel
        .run(
            "const s = document.getElementById('speech-voice'); s.value = 'robot:hum'; \
              s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    let (request, voice) = program
        .wait_for("asked to read Try", |message| match message {
            ControlMessage::Speak { request, voice, text, .. } if text == protocol::speech::TRY_TEXT => {
                Some((*request, voice.clone()))
            }
            _ => None,
        })
        .await;
    assert_eq!(voice, "hum");
    // A spinner after "Voice" while it's made and read (straight away, not
    // in the queue), gone once it would have ended.
    panel.wait("the spinner", "document.getElementById('speech-reading')").await;
    let audio = base64::engine::general_purpose::STANDARD.encode(silence_wav(300));
    let spoken =
        ControlRequest::Spoken { request, audio: Some(audio), format: Some("wav".into()), error: None };
    program.send(&spoken).await;
    panel.wait("the spinner gone", "!document.getElementById('speech-reading')").await;
    panel.close().await;

    let panel = open_card(&app, "beep", 2).await;
    panel.wait("the voice change", "document.getElementById('node-2-override-voice')").await;
    let voice: String = panel.eval("document.getElementById('node-2-override-voice').value").await;
    assert_eq!(voice, "host-check:beep");
    let adds: String = panel.eval("document.getElementById('node-2-add-override').options[0].text").await;
    assert_eq!(adds, "+ Change a voice setting");
    panel.close().await;
}

fn custom_into(id: &str, name: &str, effect: Node) -> Card {
    Card {
        id: id.into(),
        name: id.into(),
        enabled: true,
        nodes: vec![Node { id: 1, x: 0.0, y: 0.0, kind: NodeKind::Custom { name: name.into() } }, effect],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    }
}

/// A card: a plugin's event node into an alert.
fn alert_on(id: &str, event: Node, message: &str) -> Card {
    Card {
        id: id.into(),
        name: id.into(),
        enabled: true,
        nodes: vec![event, alert_node(2, message)],
        wires: vec![wire((1, "event"), (2, "trigger"))],
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn server_code_runs_in_the_app() {
    // Host check tries what `what` names and reports back with its Result
    // event; Counter is the example plugin.
    let mut config = test_config();
    let check = plugin_node(2, "host-check", "check", serde_json::json!({}));
    config.cards.push(custom_into("check", "check", check));
    let result = plugin_node(1, "host-check", "result", serde_json::json!({}));
    config.cards.push(alert_on("report", result, "{what}: {result}"));
    let count = plugin_node(2, "counter", "count", serde_json::json!({ "counter": "Hugs" }));
    config.cards.push(custom_into("hug", "hug", count));
    let counted = plugin_node(1, "counter", "counted", serde_json::json!({}));
    config.cards.push(alert_on("counted", counted, "{summary}"));
    // Relay fires its own output, Done, for the run: the card carries on.
    let mut relay =
        custom_into("relay", "relay", plugin_node(2, "host-check", "relay", serde_json::json!({})));
    relay.nodes.push(alert_node(3, "relayed {who}"));
    relay.wires.push(wire((2, "done"), (3, "trigger")));
    config.cards.push(relay);
    // Hold waits in the queue; its server code says when it's done, or
    // hears it was skipped.
    for (name, until) in [("hold", "done"), ("hold_skip", "skip")] {
        let hold = plugin_node(2, "host-check", "hold", serde_json::json!({ "until": until }));
        let mut card = custom_into(name, name, hold);
        card.nodes.push(alert_node(3, &format!("{name} ended")));
        card.wires.push(wire((2, "ended"), (3, "trigger")));
        config.cards.push(card);
    }
    // Its hosts, allowed on the Plugins page.
    config.plugins.insert(
        "host-check".into(),
        protocol::config::PluginConfig { hosts: vec!["127.0.0.1".into()], ..Default::default() },
    );
    let mut app = App::start("server_code_runs_in_the_app", config).await;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_dir(&root.join("plugins/host-check"), &app.dir.join("plugins").join("host-check"));
    copy_dir(&root.join("../../examples/plugins/counter"), &app.dir.join("plugins").join("counter"));
    app.restart().await;
    app.wait_running(&["host-check", "counter"]).await;

    let mut overlay = app.overlay(vec![]).await;
    let mut control = app.control().await;
    // A tool on this PC it may reach (the app itself it can't:
    // server_code.rs, `server_code_cant_reach_the_app_itself`).
    let tool = format!("127.0.0.1:{}", e2e::local_tool(app.port).await);
    let mut check = async |what: &str, url: &str| {
        let data = serde_json::json!({ "what": what, "url": url }).as_object().unwrap().clone();
        control.send(&ControlRequest::Event { name: "check".into(), data }).await;
    };

    check("store", "").await;
    assert_eq!(overlay.next_alert().await, "store: stored 1");
    check("timer", "").await;
    assert_eq!(overlay.next_alert().await, "timer: fired");
    check("repeat", "").await;
    assert_eq!(overlay.next_alert().await, "repeat: fired 3 times");
    // Only the hosts in its plugin.json: 127.0.0.1 here.
    check("fetch", &format!("http://{tool}/")).await;
    assert_eq!(overlay.next_alert().await, "fetch: status 200");
    check("blocked", &format!("http://localhost:{}/control/state", app.port)).await;
    let blocked = overlay.next_alert().await;
    assert!(blocked.starts_with("blocked: refused: localhost isn't one of the hosts"), "{blocked}");
    check("socket", &format!("ws://{tool}/")).await;
    assert_eq!(overlay.next_alert().await, "socket: opened");
    // A message for its page; its effect node's runs never go there.
    check("page", "").await;
    let data = overlay
        .wait_for("the page's message", |command| match command {
            Command::Plugin(command) => match &command.message {
                PluginMessage::Message { data } => Some(data.clone()),
                PluginMessage::Run { .. } => panic!("a run went to the page: {command:?}"),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(data, serde_json::json!({ "hello": "page" }));
    assert_eq!(overlay.next_alert().await, "page: sent");

    // The example: counts, kept between runs of the app.
    let mut hug =
        async || control.send(&ControlRequest::Event { name: "hug".into(), data: Default::default() }).await;
    hug().await;
    assert_eq!(overlay.next_alert().await, "Hugs: 1");
    hug().await;
    assert_eq!(overlay.next_alert().await, "Hugs: 2");
    drop(control);
    drop(overlay);
    app.restart().await;
    app.wait_running(&["counter"]).await;
    let mut overlay = app.overlay(vec![]).await;
    let mut control = app.control().await;
    control.send(&ControlRequest::Event { name: "hug".into(), data: Default::default() }).await;
    // (The queue may play the last alert again first: it's kept across a
    // restart until an overlay says it's done.)
    overlay
        .wait_for("Hugs: 3", |command| e2e::alert_shown(command).filter(|(_, text)| text == "Hugs: 3"))
        .await;
    let data = serde_json::json!({ "what": "store" }).as_object().unwrap().clone();
    control.send(&ControlRequest::Event { name: "check".into(), data }).await;
    assert_eq!(overlay.next_alert().await, "store: stored 2");

    // An effect node's own output, fired by its server code.
    let data = serde_json::json!({ "who": "Ann" }).as_object().unwrap().clone();
    control.send(&ControlRequest::Event { name: "relay".into(), data }).await;
    assert_eq!(overlay.next_alert().await, "relayed Ann", "with the event that set it off");

    // A queued run played by server code: its report waits behind it,
    // then its Ended (it said done; no overlay did).
    control.send(&ControlRequest::Event { name: "hold".into(), data: Default::default() }).await;
    assert_eq!(overlay.next_alert().await, "hold: on stage");
    assert_eq!(overlay.next_alert().await, "hold ended");
    // Skipped: its code hears it, and Ended doesn't fire.
    control.send(&ControlRequest::Event { name: "hold_skip".into(), data: Default::default() }).await;
    overlay
        .wait_for("the held run", |command| match command {
            Command::Plugin(command) => match &command.message {
                PluginMessage::Message { data } if data["holding"] == true => Some(()),
                _ => None,
            },
            _ => None,
        })
        .await;
    control.send(&ControlRequest::Queue { action: protocol::activity::QueueAction::Skip, kind: None }).await;
    assert_eq!(overlay.next_alert().await, "hold: skipped");
    let data = serde_json::json!({ "what": "store" }).as_object().unwrap().clone();
    control.send(&ControlRequest::Event { name: "check".into(), data }).await;
    assert_eq!(overlay.next_alert().await, "store: stored 3", "no hold_skip ended");

    // One that runs too long is stopped; its box on Plugins says why, the
    // header's dot that a plugin needs a look, and the other plugin
    // carries on.
    let data = serde_json::json!({ "what": "loop" }).as_object().unwrap().clone();
    control.send(&ControlRequest::Event { name: "check".into(), data }).await;
    let panel = Panel::open(&app, "plugins").await;
    panel
        .wait(
            "Host check stopped",
            "document.getElementById('plugins-page')?.innerText.includes('took longer than a second')",
        )
        .await;
    panel.wait("the Plugins dot", "document.getElementById('header-plugins')").await;
    control.send(&ControlRequest::Event { name: "hug".into(), data: Default::default() }).await;
    assert_eq!(overlay.next_alert().await, "Hugs: 4");
    // Its queued runs wait while its code doesn't run.
    control.send(&ControlRequest::Event { name: "hold".into(), data: Default::default() }).await;
    panel.go("activity").await;
    panel
        .wait(
            "the queue waiting for Host check",
            "document.getElementById('queue-state')?.innerText.includes('Waiting for the Host check plugin to run')",
        )
        .await;
    panel.close().await;
}

/// Zips a folder, its files at the top of the zip.
fn zip_folder(folder: &std::path::Path, to: &std::path::Path) {
    fn add(zip: &mut zip::ZipWriter<std::fs::File>, folder: &std::path::Path, prefix: &str) {
        for entry in std::fs::read_dir(folder).expect("read the folder").map(Result::unwrap) {
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.path().is_dir() {
                add(zip, &entry.path(), &format!("{name}/"));
            } else {
                zip.start_file(name, zip::write::SimpleFileOptions::default()).expect("start a file");
                std::io::Write::write_all(zip, &std::fs::read(entry.path()).unwrap()).expect("write a file");
            }
        }
    }
    let mut zip = zip::ZipWriter::new(std::fs::File::create(to).expect("make the zip"));
    add(&mut zip, folder, "");
    zip.finish().expect("finish the zip");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_plugins_page_installs_switches_and_allows() {
    let mut config = test_config();
    let check = plugin_node(2, "host-check", "check", serde_json::json!({}));
    config.cards.push(custom_into("check", "check", check));
    let result = plugin_node(1, "host-check", "result", serde_json::json!({}));
    config.cards.push(alert_on("report", result.clone(), "{what}: {result}"));
    // Its Result's other output.
    let mut again = alert_on("again", result, "again: {what}, {result}");
    again.wires = vec![wire((1, "again"), (2, "trigger"))];
    config.cards.push(again);
    let app = App::start("the_plugins_page_installs_switches_and_allows", config).await;
    let mut overlay = app.overlay(vec![]).await;
    let pages = async |overlay: &mut e2e::Overlay, what: &str| {
        overlay
            .wait_for(what, |command| match command {
                Command::Shell(ShellCommand::Plugins { plugins }) => Some(plugins.clone()),
                _ => None,
            })
            .await
    };
    assert_eq!(
        pages(&mut overlay, "the pages").await,
        ["alert", "hype-train", "media", "particles", "rain", "sound", "speak"]
    );
    let panel = Panel::open(&app, "plugins").await;
    panel
        .wait(
            "the built-in plugins",
            "document.getElementById('plugin-media') && document.getElementById('plugin-particles')",
        )
        .await;

    // Installing from a zip: in the plugins folder, and on the overlay at once.
    let zip = app.dir.join("confetti.zip");
    zip_folder(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/confetti"),
        &zip,
    );
    panel.choose_file("#plugins-install", &zip).await;
    panel.wait("confetti installed", "document.getElementById('plugin-confetti')").await;
    // The answer to the upload can come after the plugins' update.
    panel
        .wait(
            "the message",
            "document.getElementById('plugins-installed')?.innerText === 'Installed Confetti 0.1.0.'",
        )
        .await;
    assert!(app.dir.join("plugins/confetti/overlay/confetti.js").is_file());
    assert_eq!(
        pages(&mut overlay, "confetti's page").await,
        ["alert", "confetti", "hype-train", "media", "particles", "rain", "sound", "speak"]
    );

    // Dropped on the page: a file that isn't a zip is refused; a zip
    // installs (again, replacing it). The page is outlined while dragged over.
    let drop = |name: &str, bytes: &[u8]| {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        format!(
            "(() => {{ const bytes = Uint8Array.from(atob('{data}'), c => c.charCodeAt(0)); \
             const transfer = new DataTransfer(); transfer.items.add(new File([bytes], '{name}')); \
             const page = document.getElementById('plugins-page'); \
             page.dispatchEvent(new DragEvent('dragover', {{ bubbles: true, cancelable: true, dataTransfer: transfer }})); \
             setTimeout(() => {{ window.outlined = page.classList.contains('dropping'); \
             page.dispatchEvent(new DragEvent('drop', {{ bubbles: true, cancelable: true, dataTransfer: transfer }})); }}, 100); }})()"
        )
    };
    panel.run(&drop("notes.txt", b"hi")).await;
    panel
        .wait(
            "a text file refused",
            "document.querySelector('#plugins-page [role=alert]')?.innerText.includes(\"notes.txt isn't a .zip file\")",
        )
        .await;
    assert!(
        panel.eval::<bool>("window.outlined").await,
        "the page is outlined while a file is dragged over it"
    );
    assert_eq!(panel.count("#plugins-page.dropping").await, 0);
    panel.run(&drop("confetti.zip", &std::fs::read(&zip).unwrap())).await;
    panel
        .wait(
            "the dropped zip installed",
            "document.getElementById('plugins-installed')?.innerText === 'Installed Confetti 0.1.0.'",
        )
        .await;

    // Switched off: off the overlay.
    panel.click("#plugin-particles-on").await;
    app.wait_for_config("particles off", |config| !config.plugin("particles").enabled).await;
    assert_eq!(
        pages(&mut overlay, "without particles").await,
        ["alert", "confetti", "hype-train", "media", "rain", "sound", "speak"]
    );

    // OBS isn't there (the tests point it at port 1): a grey status, not a
    // problem.
    panel
        .wait(
            "OBS's status",
            "document.getElementById('plugin-obs-status')?.innerText.trim() === 'OBS not running'",
        )
        .await;
    assert_eq!(panel.count("#plugin-obs-status .dot.warn, #plugin-obs-status .dot.ok").await, 0);

    // A built-in plugin's hosts come with it: OBS's server code runs
    // without asking, and they can't be taken back (only the plugin
    // switched off), so its box says nothing about them, nor lists its
    // nodes.
    panel.click("#plugin-obs-open").await;
    panel.wait("OBS's box", "document.getElementById('plugin-obs-setting-address')").await;
    let said: String = panel.eval("document.getElementById('plugin-obs-body').innerText").await;
    assert!(!said.contains("May connect to") && !said.contains("Nodes:"), "{said}");
    assert_eq!(panel.count("#plugin-obs-allow, #plugin-obs-disallow").await, 0);

    // A plugin copied into the folder, found without a restart. Its server
    // code waits until its hosts are allowed.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_dir(&root.join("plugins/host-check"), &app.dir.join("plugins").join("host-check"));
    panel.click("#plugins-reload").await;
    panel.wait("its hosts to allow", "document.getElementById('plugin-host-check-allow')").await;
    // Allow is on the right of the notice's first line.
    let on_its_line: bool = panel
        .eval(
            "(() => { const notice = document.getElementById('plugin-host-check-hosts').getBoundingClientRect(); \
             const words = document.querySelector('#plugin-host-check-hosts .notice-text').getBoundingClientRect(); \
             const allow = document.getElementById('plugin-host-check-allow').getBoundingClientRect(); \
             return allow.left > words.left && allow.top < words.top + 10 && notice.right - allow.right < 20; })()",
        )
        .await;
    assert!(on_its_line);
    let mut control = app.control().await;
    let mut check = async |what: &str| {
        let data = serde_json::json!({ "what": what }).as_object().unwrap().clone();
        control.send(&ControlRequest::Event { name: "check".into(), data }).await;
    };
    check("store").await;
    let early = tokio::time::timeout(std::time::Duration::from_millis(1500), overlay.next_alert()).await;
    assert!(early.is_err(), "it ran before its hosts were allowed: {early:?}");
    panel.click("#plugin-host-check-allow").await;
    app.wait_for_config("hosts allowed", |config| config.plugin("host-check").hosts == ["127.0.0.1"]).await;
    // Started now (it compiles first).
    app.wait_running(&["host-check"]).await;
    check("store").await;
    assert_eq!(overlay.next_alert().await, "store: stored 1");

    // A sound setting: the sounds folder's picker, Listen and "+" (upload).
    panel
        .wait(
            "the sound field",
            "document.querySelector('#plugin-host-check-setting-chime')?.closest('.sound-pick')?.querySelector('button[aria-label=Listen]')              && document.getElementById('plugin-host-check-setting-chime-upload')",
        )
        .await;

    // Its own setting: the server code reads it, the page is sent it.
    panel
        .run(
            "const s = document.getElementById('plugin-host-check-setting-greeting'); s.value = 'hello'; \
             s.dispatchEvent(new Event('input', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("the greeting", |config| {
        config.plugin("host-check").settings.get("greeting") == Some(&serde_json::json!("hello"))
    })
    .await;
    let values = overlay
        .wait_for("the page's settings", |command| match command {
            Command::Plugin(command) if command.plugin == "host-check" => match &command.message {
                PluginMessage::Settings { values } if values["greeting"] == "hello" => Some(values.clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(values["greeting"], "hello");
    // Its server code is told (`settings-changed`), as for each change below.
    assert_eq!(overlay.next_alert().await, "settings changed: hello, no token");
    check("settings").await;
    assert_eq!(overlay.next_alert().await, "settings: hello");

    // A secret setting: saved apart from the config, given to the server
    // code only; the panel only learns that it's saved.
    assert!(values.get("token").is_none(), "the page got the secret: {values:?}");
    check("secret").await;
    assert_eq!(overlay.next_alert().await, "secret: empty");
    assert_eq!(panel.count("#plugin-host-check-setting-token-forget").await, 0);
    panel
        .run(
            "const s = document.getElementById('plugin-host-check-setting-token'); s.value = 'sesame'; \
             s.dispatchEvent(new InputEvent('input', { bubbles: true }))",
        )
        .await;
    panel.click("#plugin-host-check-setting-token-save").await;
    panel.wait("saved", "document.getElementById('plugin-host-check-setting-token-forget')").await;
    assert_eq!(
        panel.eval::<String>("document.getElementById('plugin-host-check-setting-token').value").await,
        "",
        "the box empties once it's saved"
    );
    let secrets: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(app.dir.join("lsk_secrets.json")).unwrap()).unwrap();
    assert_eq!(secrets["host-check"]["token"], "sesame");
    assert!(!std::fs::read_to_string(app.dir.join("lsk.json")).unwrap().contains("sesame"));
    assert_eq!(overlay.next_alert().await, "settings changed: hello, sesame");
    check("secret").await;
    assert_eq!(overlay.next_alert().await, "secret: sesame");
    panel.click("#plugin-host-check-setting-token-forget").await;
    panel.wait("forgotten", "!document.getElementById('plugin-host-check-setting-token-forget')").await;
    assert_eq!(overlay.next_alert().await, "settings changed: hello, no token");
    check("secret").await;
    assert_eq!(overlay.next_alert().await, "secret: empty");

    // Choices its server code fills in: a box until then, a dropdown of
    // them after.
    assert_eq!(
        panel.eval::<String>("document.getElementById('plugin-host-check-setting-pick').tagName").await,
        "INPUT"
    );
    check("options").await;
    assert_eq!(overlay.next_alert().await, "options: set");
    panel
        .wait(
            "the choices",
            "[...(document.querySelector('select#plugin-host-check-setting-pick')?.options ?? [])]              .map(o => o.text).join(',') === 'None,one,two,Other…'",
        )
        .await;
    // With labels (`set-choices`): the label shown, the value kept.
    check("choices").await;
    assert_eq!(overlay.next_alert().await, "choices: set");
    panel
        .wait(
            "the labels",
            "[...(document.querySelector('select#plugin-host-check-setting-pick')?.options ?? [])]              .map(o => o.text.trim() + '=' + o.value).slice(0, 3).join(',') === 'None=,One=one,Two=two-v'",
        )
        .await;
    panel
        .run(
            "const s = document.getElementById('plugin-host-check-setting-pick'); s.value = 'two-v'; \
             s.dispatchEvent(new Event('change', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("the value picked", |config| {
        config.plugin("host-check").settings.get("pick") == Some(&serde_json::json!("two-v"))
    })
    .await;
    assert_eq!(overlay.next_alert().await, "settings changed: hello, no token");
    panel
        .wait(
            "its label shown",
            "(s => s.options[s.selectedIndex].text.trim())(document.getElementById('plugin-host-check-setting-pick')) === 'Two'",
        )
        .await;

    // A note on its box itself (`set-note` with no setting).
    check("note").await;
    assert_eq!(overlay.next_alert().await, "note: set");
    panel
        .wait(
            "the note",
            "document.querySelectorAll('#plugin-host-check-body > #plugin-host-check-note.warn p').length === 2",
        )
        .await;

    // Its event node's other output (`emit-output`).
    check("output").await;
    assert_eq!(overlay.next_alert().await, "again: output, again");

    // Its icon (`icon`, a file in its folder) on its nodes, drawn as a mask
    // of the served SVG.
    panel.go("card/check").await;
    panel
        .wait(
            "its icon",
            "(e => e && (getComputedStyle(e).maskImage || getComputedStyle(e).webkitMaskImage).includes('/plugin-icons/host-check'))(document.querySelector('.node .plugin-icon'))",
        )
        .await;
    panel
        .run("fetch('/plugin-icons/host-check').then(r => window.iconType = r.status + ' ' + r.headers.get('content-type'))")
        .await;
    panel.wait("the icon served", "window.iconType === '200 image/svg+xml'").await;
    panel.go("plugins").await;
    panel.wait("the plugins", "document.getElementById('plugin-confetti-open')").await;

    // Removed: its folder deleted, off the page.
    panel.click("#plugin-confetti-open").await;
    panel.click("#plugin-confetti-remove").await;
    panel
        .wait("asking again", "document.getElementById('plugin-confetti-remove').innerText.includes('again')")
        .await;
    panel.click("#plugin-confetti-remove").await;
    panel.wait("confetti gone", "!document.getElementById('plugin-confetti')").await;
    assert!(!app.dir.join("plugins/confetti").exists());
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn text_styles_are_edited_and_reach_alerts() {
    let mut config = test_config();
    let raid = config.cards.iter_mut().find(|card| card.id == "raid-alert").unwrap();
    let Some(NodeKind::Plugin { settings, .. }) =
        raid.nodes.iter_mut().find(|node| node.id == 2).map(|node| &mut node.kind)
    else {
        panic!("the raid card's alert")
    };
    settings.insert("message".into(), "[highlight]{raider}[/] is here".into());
    let app = App::start("text_styles_are_edited_and_reach_alerts", config).await;
    let mut overlay = app.overlay(vec![]).await;
    let panel = Panel::open(&app, "plugins").await;

    // Plugins → Text styles, a box like a plugin's (a switch, "built in"),
    // among the plugins by name.
    panel.wait("the boxes", "document.getElementById('text-styles-on')").await;
    let names: Vec<String> = panel
        .eval("[...document.querySelectorAll('#plugins-page .box-title')].map(b => b.textContent)")
        .await;
    let mut sorted = names.clone();
    sorted.sort_by_key(|name| name.to_lowercase());
    assert_eq!(names, sorted);
    assert!(names.iter().any(|name| name == "Text styles"), "{names:?}");
    let kind: String =
        panel.eval("document.querySelector('#text-styles .box-head .muted').textContent").await;
    assert_eq!(kind, "built in");

    // Folded until opened: the two it starts with. Each style is a line
    // with its name and a sample until it's opened.
    panel.click("#text-styles-open").await;
    panel.wait("the styles", "document.getElementById('text-style-1-preview')").await;
    assert!(panel.eval::<bool>("!document.getElementById('text-style-0-size')").await, "folded");
    panel.click("#text-style-0-open").await;
    panel.wait("the first open", "document.getElementById('text-style-0-size')").await;

    // Laid out as the plugin boxes: the sample first, then a row each,
    // every label starting on one line and every field on another.
    let (first, labels, fields): (String, Vec<f64>, Vec<f64>) = panel
        .eval(
            "(() => { const body = document.getElementById('text-style-0-body'); \
              const rows = [...body.querySelectorAll('.setting-row')]; \
              return [body.firstElementChild.id, \
                      rows.map(row => row.querySelector('.setting-label').getBoundingClientRect().left), \
                      rows.map(row => row.querySelector('select, input, .choice-buttons') \
                                         .getBoundingClientRect().left)]; })()",
        )
        .await;
    assert_eq!(first, "text-style-0-sample");
    let spread = |lefts: &[f64]| {
        lefts.iter().copied().fold(f64::MIN, f64::max) - lefts.iter().copied().fold(f64::MAX, f64::min)
    };
    assert_eq!(labels.len(), 6, "name, font, bold, italic, size, colour: {labels:?}");
    assert!(spread(&labels) < 1.0 && spread(&fields) < 1.0, "labels {labels:?}, fields {fields:?}");
    assert!(fields[0] - labels[0] > 80.0, "the fields are beside the labels: {labels:?} {fields:?}");

    // Bold and italic: Unchanged, On or Off, as buttons.
    let bold: Vec<String> = panel
        .eval("[...document.querySelectorAll('#text-style-0-bold button.on')].map(b => b.textContent)")
        .await;
    assert_eq!(bold, ["On"]);
    panel.click("#text-style-0-italic-off").await;
    app.wait_for_config("not italic", |config| config.text_styles[0].italic == Some(false)).await;
    panel.click("#text-style-0-italic-unchanged").await;
    app.wait_for_config("italic as the text", |config| config.text_styles[0].italic.is_none()).await;

    // The size on a slider, with its value beside it.
    panel
        .run(
            "const s = document.getElementById('text-style-0-size'); s.value = '150'; \
             s.dispatchEvent(new Event('input', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("a bigger highlight", |config| config.text_styles[0].size_percent == 150.0).await;
    panel
        .wait(
            "the size shown",
            "document.querySelector('#text-style-0 .slider-value').textContent === '150%'",
        )
        .await;

    // Its colour unticked: "Unchanged" where the swatch was, the colour
    // kept (not applied); ticked again, the same colour.
    panel
        .run(
            "const c = document.getElementById('text-style-0-colour'); c.value = '#00ff00'; \
             c.dispatchEvent(new Event('input', { bubbles: true }))",
        )
        .await;
    app.wait_for_config("green", |config| config.text_styles[0].colour == "#00ff00").await;
    panel.click("#text-style-0-own-colour").await;
    app.wait_for_config("the text's colour, green kept", |config| {
        config.text_styles[0].colour.is_empty() && config.text_styles[0].kept_colour == "#00ff00"
    })
    .await;
    panel
        .wait(
            "Unchanged",
            "!document.getElementById('text-style-0-colour') && \
             document.querySelector('label.muted[for=text-style-0-own-colour]')?.textContent === 'Unchanged'",
        )
        .await;
    panel.click("#text-style-0-own-colour").await;
    app.wait_for_config("green again", |config| {
        config.text_styles[0].colour == "#00ff00" && config.text_styles[0].kept_colour.is_empty()
    })
    .await;
    panel.wait("its swatch", "document.getElementById('text-style-0-colour')?.value === '#00ff00'").await;

    // A name that can't be marked says so. Both can be open at once.
    panel.click("#text-style-1-open").await;
    panel.wait("the second open", "document.getElementById('text-style-1-name')").await;
    assert!(
        panel.eval::<bool>("!!document.getElementById('text-style-0-size')").await,
        "the first still open"
    );
    panel.click("#text-style-1-name").await;
    panel.run("document.getElementById('text-style-1-name').select()").await;
    panel.type_text("small text").await;
    app.wait_for_config("renamed", |config| config.text_styles[1].name == "small text").await;
    panel.wait("the warning", "document.querySelector('#text-style-1 .notice.warn')").await;

    // Added with a free name, open, and deleted.
    panel.click("#text-styles-add").await;
    app.wait_for_config("a third", |config| {
        config.text_styles.len() == 3 && config.text_styles[2].name == "style"
    })
    .await;
    panel.wait("the new one open", "document.getElementById('text-style-2-delete')").await;
    panel.click("#text-style-2-delete").await;
    app.wait_for_config("two again", |config| config.text_styles.len() == 2).await;

    // The raid card's alert marks the raider: the part comes with the style.
    panel.go("card/raid-alert").await;
    panel.wait("the alert node", "document.getElementById('node-2-plugin-style')").await;
    panel.click("#card-test").await;
    let first = overlay
        .wait_for("the raid alert", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { values, .. } => Some(values["message"]["parts"][0].clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    assert_eq!(first["style"]["name"], "highlight", "{first}");
    assert_eq!(first["style"]["size_percent"], 150.0, "{first}");

    // Switched off, the marks are taken out and give no look.
    panel.go("plugins").await;
    panel.click("#text-styles-on").await;
    app.wait_for_config("text styles off", |config| !config.text_styles_enabled).await;
    panel.go("card/raid-alert").await;
    panel.wait("the alert node", "document.getElementById('node-2-plugin-style')").await;
    panel.click("#card-test").await;
    let message = overlay
        .wait_for("the raid alert, unstyled", |command| match command {
            Command::Plugin(command) if command.plugin == "alert" => match &command.message {
                PluginMessage::Run { values, .. } => Some(values["message"].clone()),
                _ => None,
            },
            _ => None,
        })
        .await;
    let text = message["text"].as_str().unwrap_or_default();
    assert!(text.ends_with(" is here") && !text.contains('['), "{message}");
    let parts = message["parts"].as_array().cloned().unwrap_or_default();
    assert!(parts.iter().all(|part| part.get("style").is_none()), "{message}");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn right_clicking_a_selection_picks_a_text_style() {
    let mut config = test_config();
    config.cards.push(Card {
        id: "styled".into(),
        name: "Styled".into(),
        enabled: true,
        nodes: vec![Node { id: 1, x: 40.0, y: 60.0, kind: NodeKind::text("so hype") }],
        wires: Vec::new(),
    });
    let app = App::start("right_clicking_a_selection_picks_a_text_style", config).await;
    let panel = open_card(&app, "styled", 1).await;
    let area = "[data-node='1'] textarea";
    // All of it selected (a right click outside the selection moves the
    // caret, as in any text box), and the right click on its first word.
    let select_all =
        format!("(a => {{ a.focus(); a.select(); }})(document.querySelector({}))", js_string(area));
    let template = |config: &AppConfig| match &card(config, "styled").node(1).unwrap().kind {
        NodeKind::Text { template, .. } => template.clone(),
        _ => String::new(),
    };

    // Nothing selected: the canvas's own menu, as before.
    let at: (f64, f64) = panel
        .eval(&format!(
            "(() => {{ const r = document.querySelector({}).getBoundingClientRect(); return [r.x + 10, r.y + 10]; }})()",
            js_string(area)
        ))
        .await;
    panel.run(&format!("document.querySelector({}).setSelectionRange(2, 2)", js_string(area))).await;
    panel.right_click_at(at).await;
    panel.wait("the add-node menu", "document.querySelector('.menu[aria-label=\"Add a node\"]')").await;
    panel.key(Key::ESCAPE, modifiers::NONE).await;
    panel.wait("no menu", "!document.querySelector('.menu')").await;

    // Selected: the styles, each in its look; Escape leaves the text.
    panel.run(&select_all).await;
    panel.right_click_at(at).await;
    panel.wait("the style list", "document.querySelector('.style-menu')").await;
    assert!(panel.eval::<bool>("!document.querySelector('.menu[aria-label=\"Add a node\"]')").await);
    let names: Vec<String> = panel
        .eval("[...document.querySelectorAll('.style-menu .menu-item')].map(item => item.innerText)")
        .await;
    assert_eq!(names, ["highlight", "small"]);
    panel.key(Key::ESCAPE, modifiers::NONE).await;
    panel.wait("closed", "!document.querySelector('.style-menu')").await;
    assert_eq!(template(&app.saved_config()), "so hype");

    // Picking one wraps the selection, which stays selected (with its marks).
    panel.run(&select_all).await;
    panel.right_click_at(at).await;
    panel.wait("the style list", "document.querySelector('.style-menu')").await;
    panel.click(".style-menu .menu-item:nth-child(2)").await;
    app.wait_for_config("wrapped", |config| template(config) == "[small]so hype[/]").await;
    let selected: String = panel
        .eval(&format!(
            "(() => {{ const a = document.querySelector({}); return a.value.slice(a.selectionStart, a.selectionEnd); }})()",
            js_string(area)
        ))
        .await;
    assert_eq!(selected, "[small]so hype[/]");

    // By keyboard, too.
    panel.run(&select_all).await;
    panel.right_click_at(at).await;
    panel.wait("the style list", "document.querySelector('.style-menu')").await;
    panel.key(Key::ENTER, modifiers::NONE).await;
    app.wait_for_config("wrapped again", |config| template(config) == "[highlight][small]so hype[/][/]")
        .await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_wheel_scrolls_a_long_text_instead_of_zooming() {
    let mut config = test_config();
    let long: String = (1..=12).map(|n| format!("line {n}\n")).collect();
    config.cards.push(Card {
        id: "long".into(),
        name: "Long".into(),
        enabled: true,
        nodes: vec![protocol::cards::Node { id: 1, x: 40.0, y: 60.0, kind: NodeKind::text(long) }],
        wires: Vec::new(),
    });
    let app = App::start("the_wheel_scrolls_a_long_text_instead_of_zooming", config).await;
    let panel = open_card(&app, "long", 1).await;
    let zoom = "document.querySelector('.world').style.transform";
    let before: String = panel.eval(zoom).await;

    // Over the text: it scrolls, the canvas stays.
    let text = panel.center("[data-node='1'] textarea").await;
    panel.wheel(text, 100.0).await;
    panel
        .wait("the text scrolled", "document.querySelector(\"[data-node='1'] textarea\").scrollTop > 0")
        .await;
    assert_eq!(panel.eval::<String>(zoom).await, before, "no zoom over the text");

    // Elsewhere on the canvas the wheel still zooms.
    let (x, y) = empty_spot(&panel).await;
    panel.wheel((x, y), 100.0).await;
    panel.wait("zoomed", &format!("{zoom} !== {}", js_string(&before))).await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_text_node_keeps_its_wires_while_marked_and_resizes() {
    // A raid's raider wired into a Text node's "{raider}".
    let mut config = test_config();
    let node = |id, kind| protocol::cards::Node { id, x: 40.0 + 300.0 * f64::from(id - 1), y: 60.0, kind };
    config.cards.push(Card {
        id: "texty".into(),
        name: "Texty".into(),
        enabled: true,
        nodes: vec![node(1, NodeKind::Raid { min_viewers: 0.0 }), node(2, NodeKind::text("{raider}"))],
        wires: vec![wire((1, "raider"), (2, "raider"))],
    });
    let app = App::start("a_text_node_keeps_its_wires_while_marked_and_resizes", config).await;
    let panel = open_card(&app, "texty", 2).await;
    let template = |config: &AppConfig| match &card(config, "texty").node(2).unwrap().kind {
        NodeKind::Text { template, .. } => template.clone(),
        _ => String::new(),
    };
    let wired =
        |config: &AppConfig| card(config, "texty").wires.iter().any(|wire| wire.to.socket == "raider");

    // A style typed in front of the placeholder: the wire stays all along,
    // even while a stray brace is in front of it.
    panel.click("[data-node='2'] textarea").await;
    panel.key(Key::HOME, modifiers::CTRL).await;
    panel.type_text("{").await;
    let config = app.wait_for_config("a brace in front", |config| template(config) == "{{raider}").await;
    assert!(wired(&config), "a brace before a placeholder keeps its wire");
    panel.key(Key::BACKSPACE, modifiers::NONE).await;
    panel.type_text("[highlight]").await;
    panel.key(Key::END, modifiers::CTRL).await;
    panel.type_text("[/]").await;
    let config = app.wait_for_config("marked", |config| template(config) == "[highlight]{raider}[/]").await;
    assert!(wired(&config));

    // Its corner resizes it: wider, and a taller text box.
    let size = |config: &AppConfig| match &card(config, "texty").node(2).unwrap().kind {
        NodeKind::Text { size, .. } => *size,
        _ => None,
    };
    let corner = panel.center("[data-node='2'] .node-resize").await;
    panel.drag(corner, (corner.0 + 120.0, corner.1 + 90.0), modifiers::NONE).await;
    let config = app
        .wait_for_config("resized", |config| size(config).is_some_and(|[w, h]| w > 250.0 && h > 52.0))
        .await;
    assert!(wired(&config), "resizing keeps the wire");
    let [width, height] = size(&config).unwrap();
    let shown: (f64, f64) = panel
        .eval(
            "(() => { const node = document.querySelector(\"[data-node='2']\"); \
             return [node.getBoundingClientRect().width, node.querySelector('textarea').getBoundingClientRect().height]; })()",
        )
        .await;
    let zoom: f64 = panel
        .eval(r"Number(/scale\(([\d.]+)\)/.exec(document.querySelector('.world').style.transform)[1])")
        .await;
    assert!(
        (shown.0 / zoom - width).abs() < 2.0 && (shown.1 / zoom - height).abs() < 2.0,
        "{shown:?} at {zoom}"
    );

    // A double click on it (real clicks: the canvas holds the pointer
    // while the corner is pressed): the usual size again.
    let corner = panel.center("[data-node='2'] .node-resize").await;
    panel.click_at(corner, modifiers::NONE).await;
    panel.click_at(corner, modifiers::NONE).await;
    app.wait_for_config("the usual size", |config| size(config).is_none()).await;
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn a_dropdown_opens_its_own_list_at_the_canvas_zoom() {
    let app = App::start("a_dropdown_opens_its_own_list_at_the_canvas_zoom", test_config()).await;
    let panel = open_card(&app, "raid-alert", 2).await;
    let changes = |config: &AppConfig| match &card(config, "raid-alert").node(2).unwrap().kind {
        NodeKind::Plugin { changes, .. } => changes.clone(),
        _ => Default::default(),
    };
    let pick = "#node-2-add-change";
    let open = "document.querySelector('.dropdown-list')";
    let closed = "!document.querySelector('.dropdown-list')";

    // Zoomed out, a press opens the panel's own list (not the browser's):
    // beside the dropdown, the same width, its text zoomed as the node's,
    // the dropdown's options in it.
    panel.wheel(empty_spot(&panel).await, 300.0).await;
    panel.wait("zoomed out", "!document.querySelector('.world').style.transform.endsWith('scale(1)')").await;
    panel.click(pick).await;
    panel.wait("the list", open).await;
    let (rows, options): (Vec<String>, Vec<String>) = panel
        .eval(&format!(
            "[[...document.querySelectorAll('.dropdown-option')].map(o => o.textContent), \
              [...document.querySelector({}).options].map(o => o.text)]",
            js_string(pick)
        ))
        .await;
    assert_eq!(rows, options);
    let fit: serde_json::Value = panel
        .eval(&format!(
            "(() => {{ const list = document.querySelector('.dropdown-list'), select = document.querySelector({}); \
             const l = list.getBoundingClientRect(), s = select.getBoundingClientRect(); \
             const zoom = s.height / select.offsetHeight; \
             return {{ zoom, font: parseFloat(getComputedStyle(list).fontSize) / parseFloat(getComputedStyle(select).fontSize), \
                       left: l.left - s.left, width: l.width - s.width, below: l.top - s.bottom, above: s.top - l.bottom, \
                       focused: document.activeElement === select }}; }})()",
            js_string(pick)
        ))
        .await;
    let number = |key: &str| fit[key].as_f64().unwrap();
    assert!(number("zoom") < 0.9, "{fit}");
    assert!((number("font") - number("zoom")).abs() < 0.01, "{fit}");
    assert!(number("left").abs() < 1.0 && number("width") > -1.0, "{fit}");
    assert!((number("below") - 2.0).abs() < 1.0 || (number("above") - 2.0).abs() < 1.0, "{fit}");
    assert_eq!(fit["focused"], true, "the dropdown keeps the focus");

    // Escape closes it, a second press too; nothing is picked.
    panel.key(Key::ESCAPE, modifiers::NONE).await;
    panel.wait("closed by Escape", closed).await;
    panel.click(pick).await;
    panel.wait("open again", open).await;
    panel.click(pick).await;
    panel.wait("closed by a second press", closed).await;
    assert!(changes(&app.saved_config()).is_empty());

    // The arrows and Enter pick, as the browser's list would.
    let second: String =
        panel.eval(&format!("document.querySelector({}).options[1].value", js_string(pick))).await;
    panel.click(pick).await;
    panel.wait("open for the keys", open).await;
    panel.key(Key::ARROW_DOWN, modifiers::NONE).await;
    panel.key(Key::ENTER, modifiers::NONE).await;
    app.wait_for_config("the second option picked", |config| changes(config).contains_key(&second)).await;
    panel.wait("closed by Enter", closed).await;

    // So does a click on an option.
    let last: String =
        panel.eval(&format!("[...document.querySelector({}).options].at(-1).value", js_string(pick))).await;
    panel.click(pick).await;
    panel.wait("open for the mouse", open).await;
    panel.click(".dropdown-option:last-child").await;
    app.wait_for_config("the last option picked", |config| changes(config).contains_key(&last)).await;
    panel.wait("closed by the click", closed).await;

    // The wheel (the canvas zooms) and a press elsewhere close it.
    panel.click(pick).await;
    panel.wait("open for the wheel", open).await;
    panel.wheel(empty_spot(&panel).await, -100.0).await;
    panel.wait("closed by the wheel", closed).await;
    panel.click(pick).await;
    panel.wait("open for a press elsewhere", open).await;
    panel.click_at(empty_spot(&panel).await, modifiers::NONE).await;
    panel.wait("closed by a press elsewhere", closed).await;
    assert_eq!(changes(&app.saved_config()).len(), 2);
    panel.close().await;
}

/// A stand-in obs-websocket on a free port, answering with what the user's
/// OBS sent (`crates/obs/fixtures/`): a Hello without a password, then
/// Identified, the scene list, the input list and (for every scene) the
/// scene on air's sources. Webcam has a filter, Blur (off); the Browser
/// Sources Chat and LSK Default show the app (once its port is sent on
/// `app_port`), Event List another site. Every other request (Set…,
/// Toggle…, Press…) is answered as done and passed on; events sent on
/// `events` go to OBS's connections.
struct StandInObs {
    port: u16,
    requests: tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    events: tokio::sync::broadcast::Sender<serde_json::Value>,
    app_port: tokio::sync::watch::Sender<u16>,
    /// How many connections it has taken.
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl StandInObs {
    /// The next request passed on.
    async fn next(&mut self) -> serde_json::Value {
        tokio::time::timeout(std::time::Duration::from_secs(10), self.requests.recv())
            .await
            .expect("OBS asked within 10 s")
            .unwrap()
    }
}

async fn stand_in_obs() -> StandInObs {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let fixture = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
    let mut hello = fixture(include_str!("../../obs/fixtures/hello.json"));
    hello["d"].as_object_mut().unwrap().remove("authentication");
    let identified = fixture(include_str!("../../obs/fixtures/identified.json"));
    let scenes = fixture(include_str!("../../obs/fixtures/get_scene_list.json"));
    let inputs = fixture(include_str!("../../obs/fixtures/get_input_list.json"));
    let items = fixture(include_str!("../../obs/fixtures/get_scene_item_list.json"));
    let changed = fixture(include_str!("../../obs/fixtures/current_program_scene_changed.json"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (requests, received) = tokio::sync::mpsc::unbounded_channel();
    let (events, _) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
    let (app_port, app_port_known) = tokio::sync::watch::channel(0u16);
    let to_connections = events.clone();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&connections);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let Ok(socket) = tokio_tungstenite::accept_async(stream).await else { continue };
            let (mut sink, mut stream) = socket.split();
            // Everything it sends goes through one writer.
            let (out, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
            tokio::spawn(async move {
                while let Some(message) = outgoing.recv().await {
                    if sink.send(Message::text(message.to_string())).await.is_err() {
                        break;
                    }
                }
            });
            let _ = out.send(hello.clone());
            let mut events = to_connections.subscribe();
            {
                let out = out.clone();
                tokio::spawn(async move {
                    while let Ok(event) = events.recv().await {
                        if out.send(event).is_err() {
                            break;
                        }
                    }
                });
            }
            let requests = requests.clone();
            let (identified, scenes, inputs, items, changed) =
                (identified.clone(), scenes.clone(), inputs.clone(), items.clone(), changed.clone());
            let app_port_known = app_port_known.clone();
            tokio::spawn(async move {
                while let Some(Ok(Message::Text(text))) = stream.next().await {
                    let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                    match message["op"].as_u64() {
                        Some(1) => {
                            let _ = out.send(identified.clone());
                        }
                        Some(6) => {
                            let request = message["d"].clone();
                            let kind = request["requestType"].as_str().unwrap_or_default().to_string();
                            let answer = |data: Option<serde_json::Value>| {
                                let mut answer = serde_json::json!({ "op": 7, "d": {
                                    "requestType": kind, "requestId": request["requestId"],
                                    "requestStatus": { "code": 100, "result": true } } });
                                if let Some(data) = data {
                                    answer["d"]["responseData"] = data;
                                }
                                answer
                            };
                            let with_id = |mut answer: serde_json::Value| {
                                answer["d"]["requestId"] = request["requestId"].clone();
                                answer
                            };
                            match kind.as_str() {
                                "GetSceneList" => drop(out.send(with_id(scenes.clone()))),
                                "GetInputList" => drop(out.send(with_id(inputs.clone()))),
                                "GetSceneItemList" => drop(out.send(with_id(items.clone()))),
                                // In obs-websocket's documented shape.
                                "GetVideoSettings" => drop(out.send(answer(Some(serde_json::json!({
                                    "baseWidth": 2560, "baseHeight": 1440, "outputWidth": 1920,
                                    "outputHeight": 1080, "fpsNumerator": 60, "fpsDenominator": 1
                                }))))),
                                // In obs-websocket's documented shape (not
                                // captured yet).
                                "GetSourceFilterList" => {
                                    let filters = if request["requestData"]["sourceName"] == "Webcam" {
                                        serde_json::json!([{ "filterEnabled": false, "filterIndex": 0,
                                            "filterKind": "gaussian_blur", "filterName": "Blur", "filterSettings": {} }])
                                    } else {
                                        serde_json::json!([])
                                    };
                                    let _ = out.send(answer(Some(serde_json::json!({ "filters": filters }))));
                                }
                                "GetInputSettings" => {
                                    let input =
                                        request["requestData"]["inputName"].as_str().unwrap_or_default();
                                    let path = match input {
                                        "Chat" => Some("?overlay=chat"),
                                        "LSK Default" => Some(""),
                                        _ => None,
                                    };
                                    let (out, mut known) = (out.clone(), app_port_known.clone());
                                    let reply = answer(None);
                                    tokio::spawn(async move {
                                        let Ok(port) =
                                            known.wait_for(|port| *port != 0).await.map(|port| *port)
                                        else {
                                            return;
                                        };
                                        let url = match path {
                                            Some(path) => format!("http://localhost:{port}/{path}"),
                                            None => "https://streamelements.com/overlay/x".to_string(),
                                        };
                                        let mut reply = reply;
                                        reply["d"]["responseData"] = serde_json::json!({
                                            "inputKind": "browser_source", "inputSettings": { "url": url } });
                                        let _ = out.send(reply);
                                    });
                                }
                                _ => {
                                    let _ = requests.send(request.clone());
                                    let _ = out.send(answer(None));
                                    // After a switch, the scene's on air (when
                                    // the transition ends), as OBS tells it.
                                    if kind == "SetCurrentProgramScene" {
                                        let mut event = changed.clone();
                                        event["d"]["eventData"]["sceneName"] =
                                            request["requestData"]["sceneName"].clone();
                                        let _ = out.send(event);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    StandInObs { port, requests: received, events, app_port, connections }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn obs_scenes_are_picked_and_switched_to() {
    let mut obs = stand_in_obs().await;
    let mut config = test_config();
    e2e::obs_at(&mut config, &format!("127.0.0.1:{}", obs.port));
    let switch = plugin_node(2, "obs", "switch_scene", serde_json::json!({ "scene": "Be right back" }));
    let mut brb = custom_into("brb", "brb", switch);
    brb.nodes.push(alert_node(3, "Started"));
    brb.nodes.push(alert_node(4, "Ended"));
    brb.wires.push(wire((2, "started"), (3, "trigger")));
    brb.wires.push(wire((2, "ended"), (4, "trigger")));
    config.cards.push(brb);
    let app = App::start("obs_scenes_are_picked_and_switched_to", config).await;
    app.wait_running(&["obs"]).await;
    let mut overlay = app.overlay(vec![]).await;

    // The Scene field: OBS's scenes, as its Scenes list shows them.
    let panel = open_card(&app, "brb", 4).await;
    panel
        .wait(
            "OBS's scenes",
            "[...(document.querySelector('select#node-2-plugin-scene')?.options ?? [])] \
             .map(o => o.text).join(',') === 'Pick a scene,WoW,Be right back,Transition,VIVE,Selective audio,\
             Work,Full cam,Multibox,Recording,FFXIV,Cooking,Other…'",
        )
        .await;
    assert_eq!(
        panel.eval::<String>("document.getElementById('node-2-plugin-scene').value").await,
        "Be right back"
    );

    // Fired: OBS is asked to switch.
    let mut control = app.control().await;
    control.send(&ControlRequest::Event { name: "brb".into(), data: Default::default() }).await;
    let request = obs.next().await;
    assert_eq!(request["requestType"], "SetCurrentProgramScene");
    assert_eq!(request["requestData"], serde_json::json!({ "sceneName": "Be right back" }));
    // Started when OBS took it, Ended once the scene was on air.
    assert_eq!(overlay.next_alert().await, "Started");
    assert_eq!(overlay.next_alert().await, "Ended");
    panel.close().await;

    // Its status, on its Plugins box's head (folded).
    let panel = Panel::open(&app, "plugins").await;
    panel
        .wait(
            "Connected",
            "document.getElementById('plugin-obs-status')?.innerText.trim() === 'Connected'              && document.querySelector('#plugin-obs-status .dot.ok')",
        )
        .await;

    // An overlay showing every effect is OBS's canvas size, dragged in too.
    panel.go("status").await;
    open_status_box(&panel, "overlays").await;
    panel
        .wait(
            "the canvas",
            "document.getElementById('overlay-default-summary')?.textContent === 'Every effect · 2560 × 1440'",
        )
        .await;
    let drag: String =
        panel.eval("document.getElementById('overlay-default-drag').getAttribute('href')").await;
    assert!(drag.contains("layer-width=2560&layer-height=1440"), "{drag}");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn obs_nodes_show_switch_mute_set_and_fire() {
    let mut obs = stand_in_obs().await;
    let mut config = test_config();
    e2e::obs_at(&mut config, &format!("127.0.0.1:{}", obs.port));
    let effect = |name: &str, node: &str, settings: serde_json::Value| {
        custom_into(name, name, plugin_node(2, "obs", node, settings))
    };
    let shown = serde_json::json!({ "scene": "WoW", "source": "Display", "action": "show", "seconds": 0.5 });
    config.cards.push(effect("show", "show_source", shown));
    let held = serde_json::json!({ "scene": "WoW", "source": "Display", "action": "show", "seconds": 60, "queue": true });
    config.cards.push(effect("hold", "show_source", held));
    let blur = serde_json::json!({ "source": "Webcam", "filter": "Blur", "action": "toggle" });
    config.cards.push(effect("blur", "filter", blur));
    config.cards.push(effect("mute", "mute", serde_json::json!({ "input": "Microphone", "action": "mute" })));
    config.cards.push(effect(
        "say",
        "set_text",
        serde_json::json!({ "source": "Status", "text": "Hello {who}" }),
    ));
    // The text from a wire instead: the event's name.
    let mut wired = effect("say_wired", "set_text", serde_json::json!({ "source": "Status" }));
    wired.wires.push(wire((1, "name"), (2, "text")));
    config.cards.push(wired);
    // Only for Be right back.
    let mut on_air =
        plugin_node(1, "obs", "scene_changed", serde_json::json!({ "scene": ["Be right back"] }));
    on_air.x = -50.0; // Left of the alert, for its × to be pressed.
    config.cards.push(alert_on("on_air", on_air, "On air: {scene}"));
    let recording = plugin_node(1, "obs", "recording_started", serde_json::json!({}));
    config.cards.push(alert_on("recording", recording, "Recording"));
    let saved = plugin_node(1, "obs", "recording_ended", serde_json::json!({}));
    config.cards.push(alert_on("saved", saved, "Saved {file}"));
    let app = App::start("obs_nodes_show_switch_mute_set_and_fire", config).await;
    app.wait_running(&["obs"]).await;
    let mut overlay = app.overlay(vec![]).await;

    // OBS was running as the app started: the Browser Sources showing the
    // app are refreshed, not the one showing another site.
    obs.app_port.send(app.port).unwrap();
    let mut refreshed = Vec::new();
    for _ in 0..2 {
        let request = obs.next().await;
        assert_eq!(request["requestType"], "PressInputPropertiesButton");
        assert_eq!(request["requestData"]["propertyName"], "refreshnocache");
        refreshed.push(request["requestData"]["inputName"].as_str().unwrap().to_string());
    }
    refreshed.sort();
    assert_eq!(refreshed, ["Chat", "LSK Default"]);

    // Scaled overlays: the default overlay is stretched from 1920 × 1080 to
    // 2560 × 1440 alike in every scene (the stand-in lists the same sources
    // for each). Warn (the default) says so under the setting...
    let scenes = 11;
    let panel = Panel::open(&app, "plugins/obs").await;
    panel.wait("the note", "document.querySelectorAll('#plugin-obs-scaled-note.warn p').length === 11").await;
    assert_eq!(
        panel.eval::<String>("document.querySelector('#plugin-obs-scaled-note p').innerText").await,
        "“LSK Default” in “WoW”: 1920 × 1080, shown at 2560 × 1440. Set Width and Height to 2560 × 1440, then \
         Reset Transform."
    );
    // ...and once switched to Resize (a row of two buttons), it's unscaled
    // in every scene at once and its page made 2560 × 1440 (the plugin is
    // told its settings changed).
    let choices: Vec<String> = panel
        .eval("[...document.querySelectorAll('#plugin-obs-setting-scaled button')].map(b => b.textContent)")
        .await;
    assert_eq!(choices, ["Warn", "Resize"]);
    panel.click("#plugin-obs-setting-scaled-fix").await;
    for _ in 0..scenes {
        let request = obs.next().await;
        assert_eq!(request["requestType"], "SetSceneItemTransform");
        assert_eq!(request["requestData"]["sceneItemId"], 23, "LSK Default");
        assert_eq!(
            request["requestData"]["sceneItemTransform"],
            serde_json::json!({ "scaleX": 1.0, "scaleY": 1.0 })
        );
    }
    let request = obs.next().await;
    assert_eq!(request["requestType"], "SetInputSettings");
    assert_eq!(
        request["requestData"],
        serde_json::json!({ "inputName": "LSK Default", "inputSettings": { "width": 2560, "height": 1440 },
            "overlay": true })
    );
    panel.wait("no note", "!document.getElementById('plugin-obs-scaled-note')").await;
    // A new password: it connects again at once with it.
    assert_eq!(obs.connections.load(std::sync::atomic::Ordering::SeqCst), 1);
    panel
        .run(
            "const s = document.getElementById('plugin-obs-setting-password'); s.value = 'sesame'; \
             s.dispatchEvent(new InputEvent('input', { bubbles: true }))",
        )
        .await;
    panel.click("#plugin-obs-setting-password-save").await;
    let again = async {
        while obs.connections.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), again).await.expect("connected again");
    // The stand-in doesn't keep changes: the overlay is found scaled again,
    // and fixed again.
    for _ in 0..scenes {
        assert_eq!(obs.next().await["requestType"], "SetSceneItemTransform");
    }
    assert_eq!(obs.next().await["requestType"], "SetInputSettings");
    panel
        .wait("connected", "document.getElementById('plugin-obs-status')?.innerText.trim() === 'Connected'")
        .await;
    panel.close().await;
    // The chat resized by hand in WoW (OBS tells each step of the drag):
    // once it stops, its page is the size it's shown at, unscaled there;
    // the other scenes keep it as it looked, scaled.
    let dragged = |scale_x: f64, scale_y: f64| {
        serde_json::json!({ "op": 5, "d": { "eventType": "SceneItemTransformChanged", "eventIntent": 524288,
            "eventData": { "sceneName": "WoW", "sceneUuid": "x", "sceneItemId": 22, "sceneItemTransform": {
                "alignment": 5, "boundsAlignment": 0, "boundsHeight": 0.0, "boundsType": "OBS_BOUNDS_NONE",
                "boundsWidth": 0.0, "cropBottom": 0, "cropLeft": 0, "cropRight": 0, "cropToBounds": false,
                "cropTop": 0, "height": 700.0 * scale_y, "positionX": 0.0, "positionY": 0.0, "rotation": 0.0,
                "scaleX": scale_x, "scaleY": scale_y, "sourceHeight": 700.0, "sourceWidth": 500.0,
                "width": 500.0 * scale_x } } } })
    };
    obs.events.send(dragged(1.1, 1.1)).unwrap();
    obs.events.send(dragged(0.9, 1.3)).unwrap();
    let request = obs.next().await;
    assert_eq!(request["requestType"], "SetSceneItemTransform");
    assert_eq!(
        request["requestData"],
        serde_json::json!({ "sceneName": "WoW", "sceneItemId": 22,
            "sceneItemTransform": { "scaleX": 1.0, "scaleY": 1.0 } })
    );
    for _ in 1..scenes {
        let request = obs.next().await;
        assert_eq!(request["requestType"], "SetSceneItemTransform");
        assert_ne!(request["requestData"]["sceneName"], "WoW");
        let kept = &request["requestData"]["sceneItemTransform"];
        assert!((kept["scaleX"].as_f64().unwrap() - 500.0 / 450.0).abs() < 1e-9, "{kept}");
        assert!((kept["scaleY"].as_f64().unwrap() - 700.0 / 910.0).abs() < 1e-9, "{kept}");
    }
    let request = obs.next().await;
    assert_eq!(request["requestType"], "SetInputSettings");
    assert_eq!(request["requestData"]["inputSettings"], serde_json::json!({ "width": 450, "height": 910 }));
    // Still scaled in the other scenes: the plugin's status says so, the
    // header's Plugins dot shows, and a note under Scaled overlays says
    // where and what to do, a line for each scene.
    let panel = Panel::open(&app, "plugins/obs").await;
    panel
        .wait(
            "the blurry chat",
            "document.getElementById('plugin-obs-status')?.innerText.trim() === 'Chat is blurry' \
             && document.querySelector('#plugin-obs-status .dot.warn') && document.getElementById('header-plugins')",
        )
        .await;
    panel.wait("the note", "document.querySelectorAll('#plugin-obs-scaled-note.warn p').length === 10").await;
    let line = panel.eval::<String>("document.querySelector('#plugin-obs-scaled-note p').innerText").await;
    assert_eq!(
        line,
        "“Chat” in “Be right back”: 450 × 910, shown at 500 × 700 (450 × 910 in “WoW”). Needs its own Browser Source."
    );
    panel.close().await;

    // The node shows OBS's logo, and the Source field follows the scene.
    let panel = open_card(&app, "show", 2).await;
    panel.wait("OBS's logo", "document.querySelector('[data-node=\"2\"] .platform-icon.obs')").await;
    panel
        .wait(
            "WoW's sources",
            "[...(document.querySelector('select#node-2-plugin-source')?.options ?? [])] \
             .map(o => o.text).includes('Display')",
        )
        .await;
    panel.close().await;

    let mut control = app.control().await;
    async fn fire(control: &mut e2e::Control, name: &str, data: serde_json::Value) {
        let data = data.as_object().unwrap().clone();
        control.send(&ControlRequest::Event { name: name.into(), data }).await;
    }
    let enabled = |request: &serde_json::Value, shown: bool| {
        assert_eq!(request["requestType"], "SetSceneItemEnabled");
        assert_eq!(
            request["requestData"],
            serde_json::json!({ "sceneName": "WoW", "sceneItemId": 1, "sceneItemEnabled": shown })
        );
    };
    // Shown for half a second, then hidden again (it was hidden).
    fire(&mut control, "show", serde_json::json!({})).await;
    enabled(&obs.next().await, true);
    enabled(&obs.next().await, false);
    // Waiting in the queue, for a minute: skipped, it's hidden at once.
    fire(&mut control, "hold", serde_json::json!({})).await;
    enabled(&obs.next().await, true);
    control.send(&ControlRequest::Queue { action: protocol::activity::QueueAction::Skip, kind: None }).await;
    enabled(&obs.next().await, false);

    fire(&mut control, "blur", serde_json::json!({})).await;
    let request = obs.next().await;
    assert_eq!(request["requestType"], "SetSourceFilterEnabled");
    assert_eq!(
        request["requestData"],
        serde_json::json!({ "sourceName": "Webcam", "filterName": "Blur", "filterEnabled": true })
    );
    fire(&mut control, "mute", serde_json::json!({})).await;
    let request = obs.next().await;
    assert_eq!(request["requestType"], "SetInputMute");
    assert_eq!(request["requestData"], serde_json::json!({ "inputName": "Microphone", "inputMuted": true }));
    fire(&mut control, "say", serde_json::json!({ "who": "Ann" })).await;
    let request = obs.next().await;
    assert_eq!(request["requestType"], "SetInputSettings");
    assert_eq!(
        request["requestData"],
        serde_json::json!({ "inputName": "Status", "inputSettings": { "text": "Hello Ann" }, "overlay": true })
    );
    fire(&mut control, "say_wired", serde_json::json!({})).await;
    let request = obs.next().await;
    assert_eq!(request["requestData"]["inputSettings"], serde_json::json!({ "text": "say_wired" }));

    // Its event nodes, from what OBS tells; Scene changed only for the
    // scenes picked.
    let event = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
    let changed = event(include_str!("../../obs/fixtures/current_program_scene_changed.json"));
    obs.events.send(changed.clone()).unwrap();
    assert_eq!(overlay.next_alert().await, "On air: Be right back");
    let mut elsewhere = changed.clone();
    elsewhere["d"]["eventData"]["sceneName"] = "WoW".into();
    obs.events.send(elsewhere).unwrap();
    obs.events.send(event(include_str!("../../obs/fixtures/record_started.json"))).unwrap();
    assert_eq!(overlay.next_alert().await, "Recording", "not WoW");
    obs.events.send(event(include_str!("../../obs/fixtures/record_stopped.json"))).unwrap();
    assert_eq!(overlay.next_alert().await, "Saved C:/Users/streamer/Videos/2026-10-13 10-22-53.mp4");

    // The scenes are picked one by one on the node, and taken out with ×.
    let panel = open_card(&app, "on_air", 2).await;
    panel
        .wait(
            "Be right back, picked",
            "document.getElementById('node-1-plugin-scene-0')?.value === 'Be right back'",
        )
        .await;
    let scenes = |config: &AppConfig| match &card(config, "on_air").nodes[0].kind {
        NodeKind::Plugin { settings, .. } => settings["scene"].clone(),
        _ => serde_json::Value::Null,
    };
    let add = "const s = document.getElementById('node-1-plugin-scene'); s.value = 'WoW'; \
               s.dispatchEvent(new Event('change', { bubbles: true }))";
    panel
        .wait(
            "WoW to add, not Be right back",
            "(o => o.includes('WoW') && !o.includes('Be right back'))\
             ([...document.getElementById('node-1-plugin-scene').options].map(o => o.value))",
        )
        .await;
    panel.run(add).await;
    app.wait_for_config("WoW added", |config| scenes(config) == serde_json::json!(["Be right back", "WoW"]))
        .await;
    panel.wait("WoW's row", "document.getElementById('node-1-plugin-scene-1')?.value === 'WoW'").await;
    panel.click("[aria-label='Take out Scenes 1']").await;
    app.wait_for_config("Be right back taken out", |config| scenes(config) == serde_json::json!(["WoW"]))
        .await;
    panel.close().await;
}

/// What a request to the app answers when sent with these headers (on a
/// plain socket, so `Host` is what the test says).
async fn status_with(port: u16, path: &str, headers: &[(&str, &str)]) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let mut request = format!("GET {path} HTTP/1.1\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    socket.write_all(request.as_bytes()).await.expect("send");
    let mut answer = Vec::new();
    let _ = socket.read_to_end(&mut answer).await;
    let answer = String::from_utf8_lossy(&answer);
    answer.split(' ').nth(1).and_then(|status| status.parse().ok()).unwrap_or_else(|| panic!("{answer}"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn only_this_pc_reaches_the_app() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let app = App::start("only_this_pc_reaches_the_app", test_config()).await;
    let port = app.port;

    // The overlay's WebSocket: OBS (Origin http://localhost:<port>) and
    // clients that aren't pages (no Origin) connect; another site, and a
    // plugin's page in its sandbox (Origin null), don't.
    let overlay_socket = async |origin: Option<&str>| {
        let mut request =
            format!("ws://127.0.0.1:{port}{}", protocol::WS_PATH).into_client_request().unwrap();
        if let Some(origin) = origin {
            request.headers_mut().insert("Origin", origin.parse().unwrap());
        }
        match tokio_tungstenite::connect_async(request).await {
            Ok(_) => 101,
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status().as_u16(),
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(overlay_socket(None).await, 101, "not a page");
    assert_eq!(overlay_socket(Some(&format!("http://localhost:{port}"))).await, 101, "OBS");
    assert_eq!(overlay_socket(Some(&format!("http://127.0.0.1:{port}"))).await, 101);
    assert_eq!(overlay_socket(Some("https://evil.example")).await, 403, "another site");
    assert_eq!(overlay_socket(Some("null")).await, 403, "a sandboxed plugin page");

    // DNS rebinding: a name other than localhost is refused, whatever the
    // path; localhost and IP addresses aren't.
    let path = protocol::panel::PANEL_PATH;
    for host in ["evil.example", &format!("evil.example:{port}"), &format!("localhost.evil.example:{port}")] {
        assert_eq!(status_with(port, path, &[("Host", host)]).await, 403, "{host}");
    }
    assert_eq!(status_with(port, "/control/state", &[("Host", "evil.example")]).await, 403);
    for host in [format!("localhost:{port}"), format!("127.0.0.1:{port}"), format!("[::1]:{port}")] {
        assert_eq!(status_with(port, path, &[("Host", &host)]).await, 200, "{host}");
    }

    // The overlay page itself, in a browser, still connects; a sandboxed
    // frame in it can't.
    let overlay = Panel::open_overlay(&app, "").await;
    let opened = |frame: &str| {
        format!(
            "new Promise(done => {{ const f = document.createElement('iframe'); {frame} \
             f.srcdoc = `<script>const s = new WebSocket('ws://127.0.0.1:{port}{}'); \
             s.onopen = () => parent.postMessage('open', '*'); \
             s.onerror = () => parent.postMessage('refused', '*');${{'<'}}/script>`; \
             addEventListener('message', e => ['open', 'refused'].includes(e.data) && done(e.data)); document.body.append(f); }})",
            protocol::WS_PATH
        )
    };
    let same_origin: String = overlay.eval(&opened("")).await;
    assert_eq!(same_origin, "open", "the overlay page's own origin");
    let sandboxed: String = overlay.eval(&opened("f.sandbox = 'allow-scripts';")).await;
    assert_eq!(sandboxed, "refused", "a sandboxed frame (Origin null)");
    overlay.close().await;
}

/// Unix ms now, for inputs that go in the chat log (one file a day).
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// Something from Twitch chat, as the chat provider sends it.
fn chat_input(at_ms: u64, kind: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "at_ms": at_ms,
        "origin": { "provider": "twitch_chat", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
        "kind": kind,
    })
}

/// A chat message from MossyKnight.
fn mossy_says(at_ms: u64, id: &str, text: &str) -> serde_json::Value {
    chat_input(
        at_ms,
        serde_json::json!({
            "input": "message",
            "id": id,
            "user": { "id": "1001", "name": "mossyknight", "display_name": "MossyKnight", "color": "#1E90FF" },
            "text": text,
            "emotes": [],
        }),
    )
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn viewer_cards_show_their_log_and_moderate() {
    use protocol::panel::PanelEvent;
    use protocol::viewers::{Access, Banned, ViewerCard, ViewerInfo};

    let app = App::start_debug("viewer_cards_show_their_log_and_moderate", test_config()).await;
    let panel = Panel::open(&app, "activity").await;
    let now = now_ms();
    let day = 86_400_000;
    panel.input(mossy_says(now - day, "m1", "hi chat! first time here")).await;
    panel.input(mossy_says(now - day + 1_000, "m2", "check this out")).await;
    panel
        .input(chat_input(
            now - day + 2_000,
            serde_json::json!({ "input": "moderation", "type": "message_deleted", "id": "m2" }),
        ))
        .await;
    panel
        .input(chat_input(
            now - day + 3_000,
            serde_json::json!({ "input": "moderation", "type": "user_cleared", "user_id": "1001", "timeout_secs": 600 }),
        ))
        .await;
    panel.input(mossy_says(now - 60_000, "m3", "gg! that was clean")).await;
    // A follow, from EventSub: in the History, with the viewer's id.
    panel
        .input(serde_json::json!({
            "at_ms": now,
            "origin": { "provider": "twitch_eventsub", "platform": "twitch", "channel": { "id": "1", "name": "chan" } },
            "kind": { "input": "event", "event": "follow", "user": { "id": "1001", "name": "mossyknight", "display_name": "MossyKnight" } },
        }))
        .await;

    // Find a viewer offers who has chatted; picking one opens the card.
    panel.click("#viewer-find").await;
    panel.type_text("moss").await;
    panel
        .wait(
            "MossyKnight offered",
            "document.querySelector('.viewer-found-item')?.dataset.viewer === 'mossyknight'",
        )
        .await;
    panel.click(".viewer-found-item").await;
    panel
        .wait(
            "the card",
            "document.querySelector('#viewer-card .viewer-title')?.textContent === 'MossyKnight'",
        )
        .await;
    let field: String = panel.eval("document.getElementById('viewer-find').value").await;
    assert_eq!(field, "", "the field empties");
    let messages: String = panel.eval("document.getElementById('viewer-messages').textContent").await;
    assert_eq!(messages, "Messages · 3");
    let lines = "[...document.querySelectorAll('#viewer-log .log-line .log-text')].map(t => t.textContent).join(' | ')";
    panel.wait("the follow, from the History", &format!("{lines}.includes('Followed')")).await;
    let read: String = panel.eval(lines).await;
    assert_eq!(
        read,
        "hi chat! first time here | check this out · deleted | Timed out for 10 min | gg! that was clean | Followed"
    );
    let days: String = panel
        .eval("[...document.querySelectorAll('#viewer-log .log-day')].map(d => d.textContent).join(' | ')")
        .await;
    assert_eq!(days, "Yesterday | Today");
    let note: String = panel.eval("document.querySelector('#viewer-twitch-note > span').textContent").await;
    assert_eq!(note, "Only chat seen while Local Stream Kit was running. Full log on Twitch");
    // Its ×: closed for good (in the config), its link by Messages.
    panel.click("#viewer-twitch-note-close").await;
    app.wait_for_config("the note closed", |config| config.closed_notes.chat_log).await;
    panel.wait("the note gone", "!document.getElementById('viewer-twitch-note')").await;
    let link: String =
        panel.eval("document.querySelector('.viewer-log-head #viewer-twitch-card').textContent").await;
    assert_eq!(link, "On Twitch");
    // Not logged in: the log, and what to do to moderate.
    let access: String = panel.eval("document.getElementById('viewer-access').textContent").await;
    assert_eq!(access, "Log in with Twitch on Status to time out, ban or warn.");
    assert_eq!(panel.count("#viewer-buttons").await, 0);

    // A new line shows while the card is open.
    panel.input(mossy_says(now_ms(), "m4", "one more thing")).await;
    panel.wait("the new line", &format!("{lines}.endsWith('one more thing')")).await;
    let messages: String = panel.eval("document.getElementById('viewer-messages').textContent").await;
    assert_eq!(messages, "Messages · 4");

    // Closed; a name in the History opens it again, without opening the entry.
    panel.click("#viewer-close").await;
    panel.wait("the card closed", "!document.getElementById('viewer-card')").await;
    panel.click(".history-item button.viewer-name").await;
    panel.wait("the card again", "!!document.getElementById('viewer-card')").await;
    let entry_open: bool = panel.eval("document.querySelector('.history-item details').open").await;
    assert!(!entry_open, "the History entry stays closed");

    // A name nobody has used in chat.
    panel.click("#viewer-find").await;
    panel.type_text("nobody_here").await;
    panel.key(Key::ENTER, 0).await;
    panel.wait("why there's no card", "document.getElementById('viewer-missing')?.textContent === 'Nobody called nobody_here has chatted yet.'").await;

    // Logged in (handed to the panel): the moderator's buttons.
    let card = |access| ViewerCard {
        id: "1001".into(),
        login: "mossyknight".into(),
        name: "MossyKnight".into(),
        color: Some("#1E90FF".into()),
        badges: Vec::new(),
        messages: 1,
        lines: vec![protocol::viewers::LogLine {
            at_ms: now_ms(),
            kind: protocol::viewers::LineKind::Message {
                id: "m9".into(),
                text: "hello".into(),
                parts: Vec::new(),
                deleted: false,
                action: false,
            },
        }],
        older: 0,
        access,
    };
    panel.run("document.getElementById('viewer-find').blur()").await;
    panel.receive(&PanelEvent::Viewer { card: card(Access::Ready { roles: true }) }).await;
    panel.wait("the buttons", "!!document.getElementById('viewer-buttons')").await;
    let info =
        |info: ViewerInfo| PanelEvent::ViewerInfo { id: "1001".into(), info: Some(info), problem: None };
    let year = 365 * day;
    panel
        .receive(&info(ViewerInfo {
            created_ms: now - 3 * year,
            following_since_ms: Some(now - year),
            ..ViewerInfo::default()
        }))
        .await;
    let buttons =
        "[...document.querySelectorAll('#viewer-buttons button')].map(b => b.textContent).join(' | ')";
    panel
        .wait(
            "the info line",
            "document.getElementById('viewer-info')?.textContent.startsWith('Following since')",
        )
        .await;
    let shown: String = panel.eval(buttons).await;
    assert_eq!(shown, "Timeout | Ban | Warn | Shoutout | VIP | Mod");
    let about: String = panel.eval("document.getElementById('viewer-info').textContent").await;
    assert!(about.ends_with(" · Account 3 years old"), "{about}");
    assert_eq!(panel.count("#viewer-log .log-bin").await, 1, "a recent message can be deleted");

    // Timeout opens its lengths.
    panel.click("#viewer-timeout").await;
    let lengths: String = panel
        .eval("[...document.querySelectorAll('#viewer-timeouts button')].map(b => b.textContent).join(' | ')")
        .await;
    assert_eq!(lengths, "1 min | 10 min | 1 hour | 1 day | 1 week");
    panel.click("#viewer-timeout").await;
    panel.wait("the lengths closed", "!document.getElementById('viewer-timeouts')").await;

    // Ban and Mod ask once more; the server says why it didn't happen.
    panel.click("#viewer-ban").await;
    let asking: String = panel.eval("document.getElementById('viewer-ban').textContent").await;
    assert_eq!(asking, "Ban MossyKnight?");
    panel.click("#viewer-ban").await;
    panel
        .wait(
            "the server's answer",
            "document.getElementById('viewer-problem')?.textContent === 'Couldn\\'t ban MossyKnight: Log in with Twitch on Status to moderate.'",
        )
        .await;
    panel.click("#viewer-mod").await;
    let asking: String = panel.eval("document.getElementById('viewer-mod').textContent").await;
    assert_eq!(asking, "Make MossyKnight a mod?");

    // Warn needs a reason.
    panel.click("#viewer-warn").await;
    panel.wait("the reason", "!!document.getElementById('viewer-warn-reason')").await;
    let send_off: bool = panel.eval("document.getElementById('viewer-warn-send').disabled").await;
    assert!(send_off, "no reason yet");

    // A VIP: Remove VIP, and no Mod; a mod: Remove mod, and no VIP.
    panel.receive(&info(ViewerInfo { vip: true, ..ViewerInfo::default() })).await;
    panel.wait("Remove VIP", &format!("{buttons}.includes('Remove VIP')")).await;
    let shown: String = panel.eval(buttons).await;
    assert_eq!(shown, "Timeout | Ban | Warn | Shoutout | Remove VIP");
    panel.receive(&info(ViewerInfo { moderator: true, ..ViewerInfo::default() })).await;
    panel.wait("Remove mod", &format!("{buttons}.includes('Remove mod')")).await;

    // Timed out: Remove timeout and Ban, and how long is left; banned: Unban.
    let banned = |until_ms| ViewerInfo { banned: Some(Banned { until_ms }), ..ViewerInfo::default() };
    panel.receive(&info(banned(Some(now_ms() + 600_000)))).await;
    panel
        .wait(
            "timed out",
            "document.getElementById('viewer-state')?.textContent === 'Timed out · 10 min left'",
        )
        .await;
    let shown: String = panel.eval(buttons).await;
    assert_eq!(shown, "Remove timeout | Ban");
    panel.receive(&info(banned(None))).await;
    panel.wait("banned", "document.getElementById('viewer-state')?.textContent === 'Banned'").await;
    let shown: String = panel.eval(buttons).await;
    assert_eq!(shown, "Unban");

    // VIPs and mods not ticked: no VIP or Mod.
    panel.receive(&PanelEvent::Viewer { card: card(Access::Ready { roles: false }) }).await;
    panel.receive(&info(ViewerInfo::default())).await;
    panel.wait("no VIP or Mod", &format!("{buttons} === 'Timeout | Ban | Warn | Shoutout'")).await;

    // Moderating not ticked, or ticked after logging in: what to do, with
    // a link to the Twitch box.
    panel.receive(&PanelEvent::Viewer { card: card(Access::Off) }).await;
    panel
        .wait(
            "moderating off",
            "document.getElementById('viewer-access')?.textContent === 'Moderating is off: Twitch'",
        )
        .await;
    let link: String =
        panel.eval("document.getElementById('viewer-access-ticks').getAttribute('href')").await;
    assert_eq!(link, "#/login", "Status, with the Twitch box open");
    panel.receive(&PanelEvent::Viewer { card: card(Access::NewLogin) }).await;
    panel
        .wait("a new login", "document.getElementById('viewer-access')?.textContent === 'Moderating needs a new login: Twitch'")
        .await;

    // Logged in as another channel; closed, it stays closed for that pair.
    let other = || PanelEvent::Viewer { card: card(Access::OtherChannel { login: "someone".into() }) };
    panel.receive(&other()).await;
    panel.wait("the other login", "document.getElementById('viewer-access')?.textContent.startsWith('The Twitch login is for someone')").await;
    panel.click("#viewer-access-close").await;
    app.wait_for_config("closed for the pair", |config| config.closed_notes.card_other_channel == "someone/")
        .await;
    panel.wait("the note gone", "!document.getElementById('viewer-access')").await;
    panel.receive(&other()).await;
    assert_eq!(panel.count("#viewer-access").await, 0, "still closed");
    panel.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser and built panel; run with `cargo e2e`"]
async fn the_twitch_box_ticks_what_the_login_may_do() {
    use protocol::config::TwitchPermissions;
    use protocol::panel::{LoginCode, LoginStatus, PanelEvent, Status};

    let app = App::start("the_twitch_box_ticks_what_the_login_may_do", test_config()).await;
    let panel = Panel::open(&app, "status").await;
    open_status_box(&panel, "status-twitch").await;
    panel.wait("the ticks", "!!document.getElementById('allow-roles')").await;
    let ticks = "['channel-points', 'follows', 'hype-train', 'moderating', 'roles']                  .map(key => document.getElementById('allow-' + key).checked).join(' ')";
    let ticked: String = panel.eval(ticks).await;
    assert_eq!(ticked, "true true true false false", "the events on, moderating off");
    assert_eq!(panel.count("#status-moderating").await, 0, "no state without a login");

    // A tick is saved with the config.
    panel.click("#allow-moderating").await;
    app.wait_for_config("moderating ticked", |config| config.connections.twitch_permissions.moderating).await;

    // Logged in (handed to the panel) with a login made for the events
    // only: moderating needs a new login, offered under Log out.
    let logged_in = |renewal: Option<LoginCode>| PanelEvent::Status {
        status: Status {
            login: LoginStatus::LoggedIn {
                login: "chan".into(),
                listening: true,
                points: true,
                follows: true,
                hype_train: false,
                granted: TwitchPermissions::default(),
            },
            renewal,
            ..Status::default()
        },
    };
    panel.receive(&logged_in(None)).await;
    let states = "['channel-points', 'follows', 'hype-train', 'moderating', 'roles']                   .map(key => document.getElementById('status-' + key)?.textContent).join(' | ')";
    panel.wait("the states", "!!document.getElementById('status-moderating')").await;
    let read: String = panel.eval(states).await;
    assert_eq!(
        read,
        "Listening · 0 rewards | Listening | Twitch refused; see the log | Needs a new login | Off"
    );
    assert_eq!(panel.count("#login-logout").await, 1);
    assert_eq!(panel.count("#login-start").await, 1, "a new login for moderating");

    // The second login's code, while the first goes on.
    panel
        .receive(&logged_in(Some(LoginCode {
            url: "https://www.twitch.tv/activate".into(),
            code: "ABCDEFGH".into(),
            expires_at_ms: u64::MAX / 2,
        })))
        .await;
    panel.wait("the code", "document.querySelector('#login-renewal #login-code')").await;
    assert_eq!(panel.count("#login-start").await, 0);
    assert_eq!(panel.count("#login-logout").await, 1);

    // Untick it: off, and no new login to offer.
    panel.receive(&logged_in(None)).await;
    panel.click("#allow-moderating").await;
    panel.wait("moderating off", "document.getElementById('status-moderating')?.textContent === 'Off'").await;
    assert_eq!(panel.count("#login-start").await, 0);
    app.wait_for_config("moderating unticked", |config| !config.connections.twitch_permissions.moderating)
        .await;
    panel.close().await;
}
