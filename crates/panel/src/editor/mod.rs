//! The node editor for one card (the wide view): the card's name, switch
//! and actions on top, the other cards on the side, the canvas in the
//! middle.

mod canvas;
pub(crate) mod fields;
mod layout;
mod menu;
mod node;
pub(crate) mod template;

use gloo::timers::callback::Timeout;
use protocol::cards::{Card, Category, NodeId, NodeKind, SocketType};
use protocol::panel::{PanelRequest, TestEvent, TestHypeTrain};
use protocol::plugin::{Attached, NodeType};
use yew::prelude::*;

use crate::model::SaveState;
use crate::route::Route;
use crate::views::icon;
use crate::views::test::{SAMPLE_CHAT, SUB_MESSAGE};
use crate::{Banners, Panel, util};

/// Changes one card. `key` as for [`Panel::edit`]. Wires left without a
/// socket (a placeholder taken out of a text) go in the same step.
pub fn edit_card(panel: &Panel, card: &str, key: Option<String>, change: impl Fn(&mut Card) + 'static) {
    let id = card.to_string();
    panel.edit(key.as_deref(), move |config| {
        if let Some(card) = config.cards.iter_mut().find(|card| card.id == id) {
            change(card);
            card.drop_dangling_wires();
        }
    });
}

/// Changes one node's settings.
pub fn edit_node(
    panel: &Panel,
    card: &str,
    node: NodeId,
    key: Option<String>,
    change: impl Fn(&mut NodeKind) + 'static,
) {
    edit_card(panel, card, key, move |card| {
        if let Some(node) = card.nodes.iter_mut().find(|each| each.id == node) {
            change(&mut node.kind);
        }
    });
}

/// A fake event the card reacts to: what "Test this card" sends. A card
/// with a Timer: a tick of its timers.
fn test_event(card: &Card, rewards: &[String]) -> Option<TestEvent> {
    if card.nodes.iter().any(|node| matches!(node.kind, NodeKind::Timer { .. })) {
        return Some(TestEvent::Timer);
    }
    card.nodes.iter().find_map(|node| {
        Some(match &node.kind {
            NodeKind::EmotesInChat | NodeKind::ChatMessage => {
                TestEvent::Chat { viewer: String::new(), text: SAMPLE_CHAT.into(), viewers: 1, first: false }
            }
            NodeKind::FirstChatter => {
                TestEvent::Chat { viewer: String::new(), text: SAMPLE_CHAT.into(), viewers: 1, first: true }
            }
            NodeKind::ChatWords { words } => {
                let word = words.iter().map(|word| word.trim()).find(|word| !word.is_empty())?;
                TestEvent::Chat { viewer: String::new(), text: format!("{word}!"), viewers: 1, first: false }
            }
            NodeKind::ChatCommand { command } if !command.trim().is_empty() => TestEvent::Chat {
                viewer: String::new(),
                text: format!("{} {SAMPLE_CHAT}", command.trim()),
                viewers: 1,
                first: false,
            },
            // As many viewers as the crowd needs, posting one emote / word.
            NodeKind::EmoteCombo { viewers, .. } => {
                let emote = SAMPLE_CHAT.split_whitespace().next().unwrap_or("Kappa");
                TestEvent::Chat {
                    viewer: String::new(),
                    text: emote.into(),
                    viewers: crowd(*viewers),
                    first: false,
                }
            }
            NodeKind::Chant { words, viewers, .. } => {
                let word = words.iter().map(|word| word.trim()).find(|word| !word.is_empty())?;
                TestEvent::Chat {
                    viewer: String::new(),
                    text: format!("{word}!"),
                    viewers: crowd(*viewers),
                    first: false,
                }
            }
            NodeKind::Bits { min } => {
                let amount = min.max(100.0) as u64;
                TestEvent::Bits { amount, message: format!("Cheer{amount} {SAMPLE_CHAT}") }
            }
            NodeKind::Sub { new: true, .. } => TestEvent::Sub { months: 1, message: SUB_MESSAGE.into() },
            NodeKind::Sub { resub: true, .. } => TestEvent::Sub { months: 6, message: SUB_MESSAGE.into() },
            NodeKind::Sub { gifted: true, .. } => TestEvent::GiftedSub,
            NodeKind::GiftBomb => TestEvent::GiftBomb { count: 5 },
            NodeKind::Follow => TestEvent::Follow,
            // A whole short train; the node fires for what it's ticked for.
            NodeKind::HypeTrain { .. } => TestEvent::HypeTrain { change: TestHypeTrain::Whole, level: 1 },
            NodeKind::Raid { min_viewers } => TestEvent::Raid { viewers: min_viewers.max(25.0) as u32 },
            NodeKind::Reward { reward } => TestEvent::Reward {
                reward: if reward.trim().is_empty() {
                    rewards.first().cloned().unwrap_or_else(|| "Test reward".into())
                } else {
                    reward.trim().to_string()
                },
                text: "stay hydrated!".into(),
            },
            NodeKind::Custom { name } => TestEvent::Custom {
                name: if name.trim().is_empty() { "test".into() } else { name.trim().into() },
            },
            NodeKind::Plugin { plugin, node, ty: Attached(Some(ty)), .. }
                if ty.category == Category::Events =>
            {
                TestEvent::Plugin { plugin: plugin.clone(), node: node.clone(), values: sample_values(ty) }
            }
            _ => return None,
        })
    })
}

/// Something for each of a plugin event node's outputs, as its program
/// might send it.
fn sample_values(ty: &NodeType) -> serde_json::Map<String, serde_json::Value> {
    ty.outputs
        .iter()
        .filter_map(|output| {
            let value = match (output.ty, output.name.as_str()) {
                (SocketType::Number, _) => serde_json::Value::from(5),
                (SocketType::Text, "viewer") => "TestViewer".into(),
                (SocketType::Text, "message" | "text") => SAMPLE_CHAT.into(),
                (SocketType::Text, _) => format!("Test {}", output.label.to_lowercase()).into(),
                (SocketType::Emotes, _) => SAMPLE_CHAT.split_whitespace().collect::<Vec<_>>().into(),
                (SocketType::Event, _) => return None,
            };
            Some((output.name.clone(), value))
        })
        .collect()
}

/// How many test viewers make a crowd of `viewers`.
fn crowd(viewers: f64) -> u32 {
    if viewers.is_finite() { viewers.ceil().clamp(1.0, 100.0) as u32 } else { 1 }
}

#[derive(Properties, PartialEq)]
pub struct CardEditorProps {
    pub id: String,
}

#[function_component]
pub fn CardEditor(props: &CardEditorProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    // "Sent" shows on the test button for a moment.
    let sent = use_state(|| None::<Timeout>);
    // The narrow layout's menu (Duplicate, Delete), open or not.
    let menu_open = use_state(|| false);
    let Some(config) = &panel.model.config else { return html! {} };
    let Some(card) = config.cards.iter().find(|card| card.id == props.id) else {
        return html! {
            <main class="loading">
                <div class="missing">
                    <p>{ "This card doesn't exist (any more)." }</p>
                    <a href={Route::Cards.href()}>{ "Back to the cards" }</a>
                </div>
            </main>
        };
    };
    let id = card.id.clone();

    let on_name = {
        let panel = panel.clone();
        let id = id.clone();
        Callback::from(move |event: InputEvent| {
            let name = util::value_of(&event);
            edit_card(&panel, &id, Some(format!("{id}/name")), move |card| card.name = name.clone());
        })
    };
    let on_switch = {
        let panel = panel.clone();
        let id = id.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            edit_card(&panel, &id, None, move |card| card.enabled = on);
        })
    };
    let undo = {
        let panel = panel.clone();
        Callback::from(move |_| panel.undo())
    };
    let redo = {
        let panel = panel.clone();
        Callback::from(move |_| panel.redo())
    };
    let duplicate = {
        let panel = panel.clone();
        let card = card.clone();
        let existing = config.cards.clone();
        let menu_open = menu_open.clone();
        Callback::from(move |_| {
            menu_open.set(false);
            let mut copy = card.clone();
            copy.name = format!("{} copy", card.name);
            copy.id = util::new_card_id(&copy.name, &existing);
            let (original, new_id) = (card.id.clone(), copy.id.clone());
            panel.edit(None, move |config| {
                let at = config
                    .cards
                    .iter()
                    .position(|each| each.id == original)
                    .map_or(config.cards.len(), |i| i + 1);
                config.cards.insert(at, copy.clone());
            });
            Route::Card(new_id).go();
        })
    };
    let delete = {
        let panel = panel.clone();
        let id = id.clone();
        let name = card.name.clone();
        let menu_open = menu_open.clone();
        Callback::from(move |_| {
            menu_open.set(false);
            let question = format!("Delete the card “{name}”? (Undo brings it back.)");
            if gloo::dialogs::confirm(&question) {
                let id = id.clone();
                panel.edit(None, move |config| config.cards.retain(|each| each.id != id));
                Route::Cards.go();
            }
        })
    };
    let test = test_event(card, &panel.model.status.rewards);
    let on_test = {
        let panel = panel.clone();
        let sent = sent.clone();
        let test = test.clone();
        let card_id = id.clone();
        Callback::from(move |_| {
            let Some(event) = test.clone() else { return };
            // Unsaved edits go first, so the test runs the card as shown.
            if panel.model.save == SaveState::Pending {
                panel.save_now();
            }
            panel.send(PanelRequest::Test { event, card: Some(card_id.clone()) });
            let done = sent.clone();
            sent.set(Some(Timeout::new(1_500, move || done.set(None))));
        })
    };

    let toggle_menu = {
        let menu_open = menu_open.clone();
        Callback::from(move |_| menu_open.set(!*menu_open))
    };
    let close_menu = {
        let menu_open = menu_open.clone();
        Callback::from(move |_| menu_open.set(false))
    };
    let menu_keys = {
        let menu_open = menu_open.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                menu_open.set(false);
            }
        })
    };

    // Only news: nothing while it's saved.
    let save_state = match (&panel.model.save, panel.model.connected) {
        (_, false) => Some(("Offline, not saved yet", true)),
        (SaveState::Saved, _) => None,
        (SaveState::Pending | SaveState::Saving, _) => Some(("Saving…", false)),
        (SaveState::Failed(_), _) => Some(("Not saved", true)),
    };

    html! {
        <div class="editor">
            <header class="editor-bar">
                <a href={Route::Cards.href()} class="back" aria-label="Back to the cards">{ icon("back") }</a>
                <input class="name" type="text" aria-label="Card name" value={card.name.clone()} oninput={on_name} />
                <input class="switch" type="checkbox" checked={card.enabled} onchange={on_switch}
                       aria-label={if card.enabled { "Card is on" } else { "Card is off" }}
                       title={if card.enabled { "On: turn off" } else { "Off: turn on" }} />
                if let Some((text, error)) = save_state {
                    <span class={classes!("save-state", error.then_some("error"))} role="status">{ text }</span>
                }
                <span class="spacer"></span>
                <button type="button" class="icon-button flat" aria-label="Undo" title="Undo (Ctrl+Z)"
                        disabled={!panel.model.can_undo()} onclick={undo}>{ icon("undo") }</button>
                <button type="button" class="icon-button flat" aria-label="Redo" title="Redo (Ctrl+Y)"
                        disabled={!panel.model.can_redo()} onclick={redo}>{ icon("redo") }</button>
                <div class="bar-menu" onkeydown={menu_keys}>
                    <button type="button" class="icon-button flat" aria-label="More: duplicate, delete" title="Duplicate, delete"
                            aria-haspopup="menu" aria-expanded={menu_open.to_string()} onclick={toggle_menu}>{ icon("more") }</button>
                    if *menu_open {
                        <div class="bar-menu-backdrop" onclick={close_menu}></div>
                        <div class="bar-menu-list" role="menu">
                            <button type="button" class="menu-item" role="menuitem" onclick={duplicate}>{ icon("duplicate") }{ "Duplicate" }</button>
                            <button type="button" class="menu-item danger" role="menuitem" onclick={delete}>{ icon("trash") }{ "Delete" }</button>
                        </div>
                    }
                </div>
                <button type="button" id="card-test" class="button ghost small" disabled={test.is_none()} onclick={on_test}
                        title="Sends a fake event this card reacts to, even while it's off. Cards that are on and react to it fire too.">
                    { icon("play") }{ if sent.is_some() { "Sent" } else { "Test" } }
                </button>
            </header>
            <Banners />
            <div class="editor-body">
                <nav class="editor-side" aria-label="Cards">
                    <div class="side-head">
                        <span class="side-title">{ "Cards" }</span>
                        <a href={Route::NewCard.href()} class="icon-button flat" aria-label="New card" title="New card">{ icon("plus") }</a>
                    </div>
                    <div class="side-cards">
                    { for config.cards.iter().map(|each| {
                        let current = each.id == card.id;
                        html! {
                            <a href={Route::Card(each.id.clone()).href()} key={each.id.clone()}
                               class={classes!("side-card", current.then_some("current"), (!each.enabled).then_some("off"))}
                               aria-current={current.then_some("page")}>
                                <span class={classes!("dot", if each.enabled { "ok" } else { "off" })}></span>
                                <span class="label">{ each.name.clone() }</span>
                                if !each.problems().is_empty() {
                                    <span class="side-problem" aria-label="Has a problem">{ icon("error") }</span>
                                }
                            </a>
                        }
                    }) }
                    </div>
                </nav>
                <canvas::Canvas card={card.clone()} />
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_events_match_the_card() {
        let cards = protocol::cards::premade();
        let event = |id: &str| test_event(cards.iter().find(|card| card.id == id).unwrap(), &[]);
        assert_eq!(
            event("hydrate"),
            Some(TestEvent::Reward { reward: "Hydrate".into(), text: "stay hydrated!".into() })
        );
        assert_eq!(
            event("rain-on-bits"),
            Some(TestEvent::Bits { amount: 100, message: format!("Cheer100 {SAMPLE_CHAT}") })
        );
        assert_eq!(
            event("hype-words"),
            Some(TestEvent::Chat { viewer: String::new(), text: "hype!".into(), viewers: 1, first: false })
        );
        let combo = |kind| Card {
            id: "c".into(),
            name: "c".into(),
            enabled: true,
            nodes: vec![protocol::cards::Node { id: 1, x: 0.0, y: 0.0, kind }],
            wires: Vec::new(),
        };
        assert_eq!(
            test_event(&combo(NodeKind::EmoteCombo { viewers: 4.5, seconds: 10.0 }), &[]),
            Some(TestEvent::Chat {
                viewer: String::new(),
                text: "peepoHappy".into(),
                viewers: 5,
                first: false
            })
        );
        let chant = NodeKind::Chant { words: vec![" ".into(), "gg".into()], viewers: 3.0, seconds: 10.0 };
        assert_eq!(
            test_event(&combo(chant), &[]),
            Some(TestEvent::Chat { viewer: String::new(), text: "gg!".into(), viewers: 3, first: false })
        );
        assert_eq!(event("sub-alert"), Some(TestEvent::Sub { months: 1, message: SUB_MESSAGE.into() }));
        assert_eq!(
            event("hype-train-alert"),
            Some(TestEvent::HypeTrain { change: TestHypeTrain::Whole, level: 1 }),
            "a whole short train, whatever the node is ticked for"
        );
        assert_eq!(event("hype-train-bar"), event("hype-train-alert"));
        let empty =
            Card { id: "e".into(), name: "e".into(), enabled: true, nodes: Vec::new(), wires: Vec::new() };
        assert_eq!(test_event(&empty, &[]), None);
        assert_eq!(
            test_event(&combo(NodeKind::FirstChatter), &[]),
            Some(TestEvent::Chat {
                viewer: String::new(),
                text: SAMPLE_CHAT.into(),
                viewers: 1,
                first: true
            })
        );
        assert_eq!(
            test_event(&combo(NodeKind::Timer { minutes: 15.0, from_start: true }), &[]),
            Some(TestEvent::Timer),
            "a tick"
        );

        // A plugin's event node: something for each output.
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "billy", "name": "Billy", "version": "1", "api": 1,
            "nodes": [{ "id": "member", "title": "Member", "category": "events", "outputs": [
                { "name": "event", "label": "Event", "type": "event" },
                { "name": "viewer", "label": "Viewer", "type": "text" },
                { "name": "tier", "label": "Tier", "type": "text" },
                { "name": "months", "label": "Months", "type": "number" }
            ] }]
        }))
        .unwrap();
        let catalog = protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            program: true,
            connected: true,
            ..Default::default()
        }]);
        let Some(TestEvent::Plugin { plugin, node, values }) =
            test_event(&combo(catalog.node_kinds().remove(0)), &[])
        else {
            panic!("a plugin event")
        };
        assert_eq!((plugin.as_str(), node.as_str()), ("billy", "member"));
        assert_eq!(
            serde_json::Value::Object(values),
            serde_json::json!({ "viewer": "TestViewer", "tier": "Test tier", "months": 5 })
        );
    }
}
