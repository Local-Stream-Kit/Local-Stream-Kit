//! Test: fake events through the real cards, onto the real overlays. In
//! the plugin boxes' rows since 2026-10-14 (designed on the canvas): an
//! event a row, each with Send, and a Message for those with one; the chat
//! message from a viewer named here; its texts kept (in the config) as
//! last typed.

use protocol::config::{AppConfig, TestTexts};
use protocol::panel::{PanelRequest, TestEvent, TestHypeTrain};
use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::icon;
use crate::Panel;
use crate::editor::fields::{PickField, TextField};
use crate::util;

/// A chat message with emotes most channels have (7TV globals).
pub const SAMPLE_CHAT: &str = protocol::config::TEST_MESSAGE;

/// What a test subscriber writes.
pub const SUB_MESSAGE: &str = protocol::config::TEST_EVENT_MESSAGE;

/// How many sent events are listed.
const SENT_KEPT: usize = 5;

/// A test event with one number field.
struct Quick {
    /// The row's label, and what's listed as sent.
    label: &'static str,
    /// Its id: `test-{id}` for the number, `test-{id}-send`.
    id: &'static str,
    /// The words after the number.
    caption: &'static str,
    initial: f64,
    /// The event, from the number and the Events' Message.
    event: fn(f64, String) -> TestEvent,
}

const QUICK: &[Quick] = &[
    Quick {
        label: "Sub",
        id: "sub",
        caption: "months",
        initial: 1.0,
        event: |n, message| TestEvent::Sub { months: n as u32, message },
    },
    Quick {
        label: "Gift bomb",
        id: "gift-bomb",
        caption: "subs",
        initial: 10.0,
        event: |n, _| TestEvent::GiftBomb { count: n as u32 },
    },
    Quick {
        label: "Bits",
        id: "bits",
        caption: "",
        initial: 500.0,
        event: |n, message| TestEvent::Bits {
            amount: n as u64,
            message: format!("Cheer{} {message}", n as u64).trim_end().to_string(),
        },
    },
    Quick {
        label: "Raid",
        id: "raid",
        caption: "viewers",
        initial: 250.0,
        event: |n, _| TestEvent::Raid { viewers: n as u32 },
    },
];

/// A settings row: the label, then the control.
fn row(id: &str, label: &str, control: Html) -> Html {
    html! {
        <div class="setting-row">
            <label class="setting-label" for={id.to_string()}>{ label.to_string() }</label>
            <div class="setting-control">{ control }</div>
        </div>
    }
}

/// "▶ Send" (or other words), its id given.
fn send_button(id: &str, words: &str, onclick: Callback<MouseEvent>) -> Html {
    html! {
        <button type="button" class="button ghost small" id={id.to_string()} {onclick}>
            { icon("play") }{ words.to_string() }
        </button>
    }
}

/// The text in a field.
fn text_of(field: &NodeRef) -> String {
    field.cast::<HtmlInputElement>().map(|field| field.value()).unwrap_or_default()
}

/// The text in a field, or `starting` (its placeholder) while it's empty.
fn typed(field: &NodeRef, starting: &str) -> String {
    let typed = text_of(field);
    if typed.trim().is_empty() { starting.to_string() } else { typed }
}

/// Keeps a text typed on the page once its field is left; emptied, it
/// stays empty (2026-10-15, asked).
fn keep(panel: &Panel, text: fn(&mut TestTexts) -> &mut Option<String>) -> Callback<String> {
    let panel = panel.clone();
    Callback::from(move |typed: String| {
        panel.edit(None, move |config: &mut AppConfig| *text(&mut config.test) = Some(typed.clone()));
    })
}

#[function_component]
pub fn Test() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let numbers = use_state(|| QUICK.iter().map(|quick| quick.initial).collect::<Vec<f64>>());
    let reward = use_state(String::new);
    let train_level = use_state(|| 2.0);
    // What was sent last, newest first, with when.
    let sent = use_state(Vec::<(f64, String)>::new);
    let (viewer_field, message_field, reward_field) = (use_node_ref(), use_node_ref(), use_node_ref());
    let event_message_field = use_node_ref();
    let Some(config) = &panel.model.config else { return html! {} };
    let texts = &config.test;

    let send = {
        let panel = panel.clone();
        let sent = sent.clone();
        move |event: TestEvent, label: String| {
            panel.send(PanelRequest::Test { event, card: None });
            let mut list = (*sent).clone();
            list.insert(0, (js_sys::Date::now(), label));
            list.truncate(SENT_KEPT);
            sent.set(list);
        }
    };
    let rewards = &panel.model.status.rewards;
    let reward_name = if reward.trim().is_empty() {
        rewards.first().cloned().unwrap_or_else(|| "Test reward".into())
    } else {
        (*reward).clone()
    };
    let chat = {
        let (send, viewer_field, message_field) = (send.clone(), viewer_field.clone(), message_field.clone());
        Callback::from(move |_: MouseEvent| {
            let viewer = typed(&viewer_field, protocol::config::TEST_VIEWER);
            let text = typed(&message_field, protocol::config::TEST_MESSAGE);
            send(
                TestEvent::Chat { viewer: viewer.clone(), text, viewers: 1, first: false },
                format!("Chat message from {viewer}"),
            )
        })
    };
    let redeem = {
        let (send, reward_field, name) = (send.clone(), reward_field.clone(), reward_name.clone());
        // Empty, no text (as a reward that asks for none).
        Callback::from(move |_: MouseEvent| {
            send(
                TestEvent::Reward { reward: name.clone(), text: text_of(&reward_field) },
                format!("Reward: {name}"),
            )
        })
    };

    html! {
        <section class="page">
            <section class="panel-box" id="test-events">
                <div class="box-head"><h2 class="box-title">{ "Events" }</h2></div>
                <div class="setting-rows">
                    { for QUICK.iter().enumerate().map(|(index, quick)| {
                        let id = format!("test-{}", quick.id);
                        let oninput = {
                            let numbers = numbers.clone();
                            Callback::from(move |event: InputEvent| {
                                if let Some(number) = util::number_of(&event) {
                                    let mut list = (*numbers).clone();
                                    list[index] = number.max(0.0);
                                    numbers.set(list);
                                }
                            })
                        };
                        let value = numbers[index];
                        let onclick = {
                            let (send, message_field) = (send.clone(), event_message_field.clone());
                            Callback::from(move |_| {
                                let event = (quick.event)(value, text_of(&message_field));
                                send(event, format!("{} {}", quick.label, util::show_number(value)))
                            })
                        };
                        row(&id, quick.label, html! {
                            <div class="test-line">
                                <input id={id.clone()} class="num" type="number" min="0" value={util::show_number(value)} {oninput} />
                                if !quick.caption.is_empty() {
                                    <span class="number-caption">{ quick.caption }</span>
                                }
                                { send_button(&format!("{id}-send"), "Send", onclick) }
                            </div>
                        })
                    }) }
                    { row("test-gifted-sub", "Gifted sub", html! {
                        <div class="test-line">
                            <span class="muted small">{ "From TestGifter" }</span>
                            { send_button("test-gifted-sub", "Send", {
                                let send = send.clone();
                                Callback::from(move |_| send(TestEvent::GiftedSub, "Gifted sub".into()))
                            }) }
                        </div>
                    }) }
                    { row("test-follow", "Follow", html! {
                        <div class="test-line">
                            <span class="muted small">{ "From TestViewer" }</span>
                            { send_button("test-follow", "Send", {
                                let send = send.clone();
                                Callback::from(move |_| send(TestEvent::Follow, "Follow".into()))
                            }) }
                        </div>
                    }) }
                    // One for the events with a message (2026-10-15, asked).
                    <div class="setting-row tall noted">
                        <label class="setting-label" for="test-event-message">{ "Message" }</label>
                        <div class="setting-control">
                            <TextField id="test-event-message" class="mono" field={event_message_field}
                                       placeholder="None" value={texts.event_message().to_string()}
                                       onchange={keep(&panel, |texts| &mut texts.event_message)} />
                            <span class="muted small">{ "Sent with Sub and Bits" }</span>
                        </div>
                    </div>
                </div>
            </section>

            <section class="panel-box" id="test-chat-box">
                <div class="box-head"><h2 class="box-title">{ "Chat message" }</h2></div>
                <div class="setting-rows">
                    { row("test-viewer", "Viewer", html! {
                        <TextField id="test-viewer" field={viewer_field}
                                   value={texts.viewer().to_string()} placeholder={protocol::config::TEST_VIEWER}
                                   onchange={keep(&panel, |texts| &mut texts.viewer)} />
                    }) }
                    { row("test-chat", "Message", html! {
                        <TextField id="test-chat" class="mono" field={message_field}
                                   value={texts.message().to_string()} placeholder={protocol::config::TEST_MESSAGE}
                                   onchange={keep(&panel, |texts| &mut texts.message)} />
                    }) }
                    <div class="setting-row">
                        <span></span>
                        <div class="setting-control"><div>{ send_button("test-chat-send", "Send", chat) }</div></div>
                    </div>
                </div>
            </section>

            <section class="panel-box" id="test-reward-box">
                <div class="box-head"><h2 class="box-title">{ "Channel point reward" }</h2></div>
                <div class="setting-rows">
                    { row("test-reward", "Reward", html! {
                        <PickField id="test-reward" options={rewards.clone()}
                                   value={if reward.is_empty() { rewards.first().cloned().unwrap_or_default() } else { (*reward).clone() }}
                                   placeholder="Test reward"
                                   onchange={let reward = reward.clone(); Callback::from(move |name: String| reward.set(name))} />
                    }) }
                    { row("test-reward-text", "Text", html! {
                        <TextField id="test-reward-text" field={reward_field} placeholder="If the reward asks for it"
                                   value={texts.reward_text().to_string()}
                                   onchange={keep(&panel, |texts| &mut texts.reward_text)} />
                    }) }
                    <div class="setting-row">
                        <span></span>
                        <div class="setting-control"><div>{ send_button("test-redeem", "Redeem", redeem) }</div></div>
                    </div>
                </div>
            </section>

            <section class="panel-box" id="test-hype-train">
                <div class="box-head"><h2 class="box-title">{ "Hype train" }</h2></div>
                <div class="setting-rows">
                    { row("test-hype-train-level", "Level", html! {
                        <div class="number-row">
                            <input id="test-hype-train-level" class="num" type="number" min="1" value={util::show_number(*train_level)}
                                   oninput={let level = train_level.clone(); Callback::from(move |event: InputEvent| {
                                       if let Some(number) = util::number_of(&event) {
                                           level.set(number.max(1.0));
                                       }
                                   })} />
                        </div>
                    }) }
                    <div class="setting-row wraps">
                        <span class="setting-label">{ "Send" }</span>
                        <div class="setting-control">
                            <div class="button-row">
                                { for [
                                    ("whole", "Whole train", TestHypeTrain::Whole),
                                    ("start", "Start", TestHypeTrain::Start),
                                    ("progress", "Progress", TestHypeTrain::Progress),
                                    ("level-up", "Level up", TestHypeTrain::LevelUp),
                                    ("end", "End", TestHypeTrain::End),
                                ].into_iter().map(|(id, label, change)| {
                                    let send = send.clone();
                                    let level = *train_level as u32;
                                    let onclick = Callback::from(move |_| {
                                        send(TestEvent::HypeTrain { change, level }, format!("Hype train: {label} (level {level})"))
                                    });
                                    html! { <button type="button" class="button ghost small" id={format!("test-hype-train-{id}")} {onclick}>{ label }</button> }
                                }) }
                            </div>
                        </div>
                    </div>
                </div>
            </section>

            if !sent.is_empty() {
                <section class="panel-box" id="test-sent" aria-label="Recently sent">
                    <div class="box-head"><h2 class="box-title">{ "Just sent" }</h2></div>
                    <div class="sent-lines">
                        { for sent.iter().map(|(at, label)| html! {
                            <div class="sent-line">
                                <span class="mono small muted">{ util::time(*at as u64) }</span>
                                <span>{ label.clone() }</span>
                            </div>
                        }) }
                    </div>
                </section>
            }
        </section>
    }
}
