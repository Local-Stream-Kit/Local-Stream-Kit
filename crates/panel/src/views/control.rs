//! The control API (for a Stream Deck, hotkey tools and scripts), a box on
//! Status (its own page until 2026-10-14): the key, the link the Stream
//! Deck plugin is given, and ready-to-copy URLs, folded under "Make a URL".

use protocol::activity::{QueueAction, QueueGroup};

use protocol::control::{self, EffectId, Switch};
use protocol::panel::PanelRequest;
use yew::prelude::*;

use super::boxes::{CopyField, FoldBox};
use super::icon;
use crate::editor::fields::PickField;
use crate::{Panel, util};

#[derive(Debug, Clone, Copy, PartialEq)]
enum What {
    Card,
    Effect,
    Queue,
    Mute,
    Event,
}

impl What {
    const ALL: [What; 5] = [What::Card, What::Effect, What::Queue, What::Mute, What::Event];

    fn name(self) -> &'static str {
        match self {
            What::Card => "card",
            What::Effect => "effect",
            What::Queue => "queue",
            What::Mute => "mute",
            What::Event => "event",
        }
    }

    fn label(self) -> &'static str {
        match self {
            What::Card => "Switch a card",
            What::Effect => "Switch an effect",
            What::Queue => "The queue",
            What::Mute => "Mute the sound",
            What::Event => "Fire a Custom event",
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct ControlBoxProps {
    /// Linked to (`#/control`): it opens.
    #[prop_or_default]
    pub focused: bool,
}

#[function_component]
pub fn ControlBox(props: &ControlBoxProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let shown = use_state(|| false);
    let making = use_state(|| false);
    let what = use_state(|| What::Card);
    // A card id, an effect id or an event name; empty = the first.
    let target = use_state(String::new);
    let action = use_state(|| "toggle".to_string());
    let Some(config) = &panel.model.config else { return html! {} };
    let key = panel.model.status.control_key.clone();
    let origin = gloo::utils::window().location().origin().unwrap_or_default();
    let events = control::custom_events(&config.cards);

    let switch = Switch::parse(&action).unwrap_or(Switch::Toggle);
    // What's picked, the first until something is.
    let card = config.cards.iter().find(|card| card.id == *target).or(config.cards.first());
    let picked_card = card.map(|card| card.id.clone()).unwrap_or_default();
    let picked_effect = EffectId::parse(&target).unwrap_or(EffectId::Rain);
    let path = match *what {
        What::Card => card.map(|card| control::card_path(&card.id, switch)),
        What::Effect => Some(control::effect_path(picked_effect, switch)),
        What::Queue => {
            let action = QueueAction::parse(&action).unwrap_or(QueueAction::Toggle);
            let only = QueueGroup::parse(&target).filter(|_| action == QueueAction::Clear);
            Some(control::queue_path(action, only))
        }
        What::Mute => Some(control::mute_path(switch)),
        What::Event => {
            let name = if target.trim().is_empty() {
                events.first().cloned()
            } else {
                Some(target.trim().to_string())
            };
            name.map(|name| control::event_path(&name))
        }
    };
    let url = path.map(|path| format!("{origin}{}", control::with_key(&path, &key)));

    let on_what = {
        let (what, target, action) = (what.clone(), target.clone(), action.clone());
        Callback::from(move |event: Event| {
            let picked = util::value_of(&event);
            what.set(What::ALL.into_iter().find(|w| w.name() == picked).unwrap_or(What::Card));
            target.set(String::new());
            action.set("toggle".into());
        })
    };
    let on_target = {
        let target = target.clone();
        Callback::from(move |event: Event| target.set(util::value_of(&event)))
    };
    let on_event_name = {
        let target = target.clone();
        Callback::from(move |name: String| target.set(name))
    };
    let on_action = {
        let action = action.clone();
        Callback::from(move |event: Event| action.set(util::value_of(&event)))
    };
    let new_key = {
        let panel = panel.clone();
        Callback::from(move |_| {
            let question = "Make a new key? Stream Deck keys and scripts using the old one stop working until they get the new one.";
            if gloo::dialogs::confirm(question) {
                panel.send(PanelRequest::NewControlKey);
            }
        })
    };
    let toggle_shown = {
        let shown = shown.clone();
        Callback::from(move |_| shown.set(!*shown))
    };
    let flip_making = {
        let making = making.clone();
        Callback::from(move |_: MouseEvent| making.set(!*making))
    };
    let hidden = |text: &str| if *shown { text.to_string() } else { text.replace(&key, "…") };

    let actions: Vec<(&str, &str)> = match *what {
        What::Card | What::Effect => vec![("on", "Turn on"), ("off", "Turn off"), ("toggle", "Toggle")],
        What::Queue => vec![
            ("pause", "Pause"),
            ("resume", "Resume"),
            ("toggle", "Pause / resume"),
            ("skip", "Skip what's playing"),
            ("clear", "Clear what's waiting"),
        ],
        What::Mute => vec![("toggle", "Mute / unmute"), ("on", "Mute"), ("off", "Unmute")],
        What::Event => Vec::new(),
    };
    let connection = control::connection_url(&origin, &key);

    // The rows of "Make a URL": what, then what it's for, what to do, and
    // the URL.
    let target_row = match *what {
        What::Card => Some(html! {
            <div class="setting-row">
                <label class="setting-label" for="control-target">{ "Card" }</label>
                <div class="setting-control">
                    <select id="control-target" onchange={on_target.clone()}>
                        { for config.cards.iter().map(|card| html! {
                            <option value={card.id.clone()} selected={card.id == picked_card}>{ card.name.clone() }</option>
                        }) }
                    </select>
                </div>
            </div>
        }),
        What::Effect => Some(html! {
            <div class="setting-row">
                <label class="setting-label" for="control-target">{ "Effect" }</label>
                <div class="setting-control">
                    <select id="control-target" onchange={on_target.clone()}>
                        { for EffectId::ALL.into_iter().map(|effect| html! {
                            <option value={effect.id()} selected={effect == picked_effect}>{ effect.label() }</option>
                        }) }
                    </select>
                </div>
            </div>
        }),
        What::Queue | What::Mute => None,
        What::Event => Some(html! {
            <div class="setting-row tall">
                <label class="setting-label" for="control-target">{ "Event name" }</label>
                <div class="setting-control">
                    <PickField id="control-target" options={events.clone()} placeholder="party"
                               value={if target.trim().is_empty() { events.first().cloned().unwrap_or_default() } else { (*target).clone() }}
                               onchange={on_event_name} />
                    if events.is_empty() {
                        <span class="muted small">{ "No card listens for one yet: start a card with a Custom event node, and give it this name." }</span>
                    }
                </div>
            </div>
        }),
    };

    html! {
        <FoldBox id="control" title="Control API" open={props.focused}>
            <div class="setting-rows">
                <div class="setting-row tall">
                    <label class="setting-label" for="control-key">{ "Key" }</label>
                    <div class="setting-control">
                        <div class="key-line">
                            <CopyField id="control-key" value={if *shown { key.clone() } else { "••••••••••••".to_string() }}
                                       copies={Some(AttrValue::from(key.clone()))} what="the key" />
                            <div class="key-buttons">
                                <button type="button" class="button ghost small" id="control-show"
                                        onclick={toggle_shown}>{ if *shown { "Hide" } else { "Show" } }</button>
                                <button type="button" class="button ghost small" id="control-new-key"
                                        onclick={new_key}>{ "New key" }</button>
                            </div>
                        </div>
                        <div class="danger-note">{ icon("warn") }{ "Do not show this to anyone" }</div>
                    </div>
                </div>
                <div class="setting-row">
                    <label class="setting-label" for="control-deck">{ "Stream Deck" }</label>
                    <div class="setting-control">
                        <CopyField id="control-deck" value={hidden(&connection)} copies={Some(AttrValue::from(connection.clone()))}
                                   what="the Stream Deck plugin's link" />
                    </div>
                </div>
            </div>
            <div class="setting-group">
                <button type="button" class={classes!("box-fold", "group-fold", making.then_some("open"))}
                        id="control-make-open" aria-expanded={making.to_string()} onclick={flip_making}>
                    { icon("chevron") }{ "Make a URL" }
                </button>
                if *making {
                    <div class="setting-rows">
                        <div class="setting-row">
                            <label class="setting-label" for="control-what">{ "What" }</label>
                            <div class="setting-control">
                                <select id="control-what" onchange={on_what}>
                                    { for What::ALL.into_iter().map(|w| html! {
                                        <option value={w.name()} selected={w == *what}>{ w.label() }</option>
                                    }) }
                                </select>
                            </div>
                        </div>
                        { for target_row }
                        if !actions.is_empty() {
                            <div class="setting-row">
                                <label class="setting-label" for="control-action">{ "Do" }</label>
                                <div class="setting-control">
                                    <select id="control-action" onchange={on_action}>
                                        { for actions.iter().map(|(value, label)| html! {
                                            <option value={*value} selected={*value == *action}>{ *label }</option>
                                        }) }
                                    </select>
                                </div>
                            </div>
                        }
                        if *what == What::Queue && *action == "clear" {
                            <div class="setting-row">
                                <label class="setting-label" for="control-target">{ "Only" }</label>
                                <div class="setting-control">
                                    <select id="control-target" onchange={on_target.clone()}>
                                        <option value="" selected={QueueGroup::parse(&target).is_none()}>{ "Everything" }</option>
                                        { for QueueGroup::ALL.into_iter().map(|group| html! {
                                            <option value={group.name()} selected={QueueGroup::parse(&target) == Some(group)}>{ group.plural() }</option>
                                        }) }
                                    </select>
                                </div>
                            </div>
                        }
                        <div class="setting-row tall">
                            <label class="setting-label" for="control-url">{ "URL" }</label>
                            <div class="setting-control">
                                { match url {
                                    Some(url) => html! {
                                        <CopyField id="control-url" value={hidden(&url)} copies={Some(AttrValue::from(url.clone()))}
                                                   what="the URL" />
                                    },
                                    None => html! { <span class="muted small state-line">{ "There's nothing to pick yet." }</span> },
                                } }
                                if *what == What::Event {
                                    <span class="muted small">{ "Anything else in the query (&colour=red), or the fields of a JSON body, fill the card's texts: {colour}." }</span>
                                }
                            </div>
                        </div>
                    </div>
                }
            </div>
        </FoldBox>
    }
}
