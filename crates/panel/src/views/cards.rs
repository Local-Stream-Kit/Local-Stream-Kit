//! The card list: one box, a card a row with its on/off switch, its name
//! and summary (which open its node editor) and its problem if it has
//! one, then "+ New card" (new cards go at the end). Cards are put in
//! order by dragging their handle, or with the arrow keys on it.

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};
use yew::prelude::*;

use super::icon;
use crate::Panel;
use crate::route::Route;
use crate::util;

/// A card being dragged. The list keeps its order on the page until it's
/// dropped (moving the element would end the drag); the card follows the
/// pointer and the others slide aside.
#[derive(Clone, PartialEq)]
struct Drag {
    id: String,
    from: usize,
    to: usize,
    start_y: f64,
    y: f64,
    /// A card's height and the gap after it: how far the others slide.
    step: f64,
    /// The other cards' middles, in order, when the drag began.
    middles: Vec<f64>,
}

impl Drag {
    /// How far the card at `index` is moved on the page.
    fn shift(&self, index: usize) -> f64 {
        if index == self.from {
            return self.y - self.start_y;
        }
        let among_others = if index < self.from { index } else { index - 1 };
        let lands = if among_others < self.to { among_others } else { among_others + 1 };
        (lands as f64 - index as f64) * self.step
    }
}

/// Moves a card to `to` in the list (one undo step).
fn move_card(panel: &Panel, id: &str, to: usize) {
    let id = id.to_string();
    panel.edit(None, move |config| {
        if let Some(from) = config.cards.iter().position(|card| card.id == id) {
            let card = config.cards.remove(from);
            let to = to.min(config.cards.len());
            config.cards.insert(to, card);
        }
    });
}

fn handle_id(card: &str) -> String {
    format!("card-handle-{card}")
}

#[function_component]
pub fn Cards() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let list = use_node_ref();
    let drag = use_state(|| None::<Drag>);
    // A card moved with the keyboard: its handle gets the focus back.
    let refocus = use_state(|| None::<String>);
    {
        let refocus = refocus.clone();
        use_effect_with((*refocus).clone(), move |card| {
            if let Some(card) = card
                && let Some(handle) =
                    page_document().and_then(|document| document.get_element_by_id(&handle_id(card)))
                && let Ok(handle) = handle.dyn_into::<HtmlElement>()
            {
                let _ = handle.focus();
                refocus.set(None);
            }
        });
    }
    let Some(config) = &panel.model.config else { return html! {} };
    let count = config.cards.len();
    html! {
        <section class="page">
            <div class="panel-box card-box">
            if config.cards.is_empty() {
                <p class="card-empty">
                    { "A card turns something that happens on stream (a sub, a raid, a chat command) into effects on the overlay. Start one from a preset with New card." }
                </p>
            }
            <ul class={classes!("card-list", drag.is_some().then_some("sorting"))} ref={list.clone()}>
                { for config.cards.iter().enumerate().map(|(index, card)| {
                    let problems = card.problems();
                    let id = card.id.clone();
                    let toggle = {
                        let panel = panel.clone();
                        let id = id.clone();
                        Callback::from(move |event: Event| {
                            let on = util::checked_of(&event);
                            let id = id.clone();
                            panel.edit(None, move |config| {
                                if let Some(card) = config.cards.iter_mut().find(|card| card.id == id) {
                                    card.enabled = on;
                                }
                            });
                        })
                    };
                    let on_down = {
                        let drag = drag.clone();
                        let list = list.clone();
                        let id = id.clone();
                        Callback::from(move |event: PointerEvent| {
                            if event.button() != 0 {
                                return;
                            }
                            // No text selection or scrolling while dragging; the
                            // handle takes the focus itself (for the arrow keys).
                            event.prevent_default();
                            let handle = event
                                .target()
                                .and_then(|target| target.dyn_into::<Element>().ok())
                                .and_then(|target| target.closest(".drag-handle").ok().flatten());
                            if let Some(handle) = handle {
                                let _ = handle.set_pointer_capture(event.pointer_id());
                                if let Ok(handle) = handle.dyn_into::<HtmlElement>() {
                                    let _ = handle.focus();
                                }
                            }
                            let Some(list) = list.cast::<Element>() else { return };
                            let items: Vec<Element> = (0..list.children().length())
                                .filter_map(|child| list.children().item(child))
                                .collect();
                            let rects: Vec<_> = items.iter().map(Element::get_bounding_client_rect).collect();
                            let step = match (rects.first(), rects.get(1)) {
                                (Some(first), Some(second)) => second.top() - first.top(),
                                (Some(first), None) => first.height(),
                                _ => return,
                            };
                            let middles = rects
                                .iter()
                                .enumerate()
                                .filter(|(other, _)| *other != index)
                                .map(|(_, rect)| rect.top() + rect.height() / 2.0)
                                .collect();
                            let y = f64::from(event.client_y());
                            drag.set(Some(Drag { id: id.clone(), from: index, to: index, start_y: y, y, step, middles }));
                        })
                    };
                    let on_move = {
                        let drag = drag.clone();
                        Callback::from(move |event: PointerEvent| {
                            let Some(mut moving) = (*drag).clone() else { return };
                            moving.y = f64::from(event.client_y());
                            moving.to = moving.middles.iter().filter(|middle| moving.y > **middle).count();
                            drag.set(Some(moving));
                        })
                    };
                    let on_up = {
                        let drag = drag.clone();
                        let panel = panel.clone();
                        Callback::from(move |_: PointerEvent| {
                            if let Some(done) = &*drag
                                && done.to != done.from
                            {
                                move_card(&panel, &done.id, done.to);
                            }
                            drag.set(None);
                        })
                    };
                    let on_cancel = {
                        let drag = drag.clone();
                        Callback::from(move |_: PointerEvent| drag.set(None))
                    };
                    let on_key = {
                        let panel = panel.clone();
                        let refocus = refocus.clone();
                        let id = id.clone();
                        Callback::from(move |event: KeyboardEvent| {
                            let to = match event.key().as_str() {
                                "ArrowUp" if index > 0 => index - 1,
                                "ArrowDown" if index + 1 < count => index + 1,
                                _ => return,
                            };
                            event.prevent_default();
                            move_card(&panel, &id, to);
                            refocus.set(Some(id.clone()));
                        })
                    };
                    let moving = drag.as_ref().filter(|moving| moving.id == id).is_some();
                    let style = drag.as_ref().map(|moving| format!("transform: translateY({}px)", moving.shift(index)));
                    let switch_id = format!("card-switch-{id}");
                    html! {
                        <li class={classes!("card-item", (!card.enabled).then_some("off"), moving.then_some("dragging"))}
                            key={id.clone()} data-card={id.clone()} {style}>
                            <button type="button" class="drag-handle" id={handle_id(&id)}
                                    aria-label={format!("Move {}: drag, or use the arrow keys", card.name)}
                                    title="Drag to move (or ↑ / ↓)"
                                    onpointerdown={on_down} onpointermove={on_move} onpointerup={on_up}
                                    onpointercancel={on_cancel} onkeydown={on_key}>
                                { icon("grip") }
                            </button>
                            <input class="switch" type="checkbox" id={switch_id} checked={card.enabled}
                                   onchange={toggle}
                                   aria-label={format!("{} {}", if card.enabled { "Turn off" } else { "Turn on" }, card.name)} />
                            // Its name (and the rest of the row) opens it; only
                            // the switch turns it on or off.
                            <a class="card-open" href={Route::Card(id.clone()).href()}>
                                <span class="card-text">
                                    <span class="card-name">{ card.name.clone() }</span>
                                    <span class="card-summary">{ util::summary(card) }</span>
                                    if let Some(problem) = problems.first() {
                                        <span class="card-problem">{ icon("error") }{ problem.message.clone() }</span>
                                    }
                                </span>
                                <span class="card-chevron">{ icon("chevron") }</span>
                            </a>
                        </li>
                    }
                }) }
            </ul>
            <a class="card-new" id="card-new" href={Route::NewCard.href()}>
                <span class="card-new-plus">{ icon("plus") }</span>{ "New card" }
            </a>
            </div>
        </section>
    }
}

fn page_document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(from: usize, to: usize) -> Drag {
        Drag { id: "c".into(), from, to, start_y: 0.0, y: 10.0, step: 50.0, middles: Vec::new() }
    }

    #[test]
    fn the_others_slide_aside() {
        // Card 1 of 0..4 dragged to the end: 2 and 3 move up one place.
        let down = drag(1, 3);
        assert_eq!([0, 2, 3].map(|index| down.shift(index)), [0.0, -50.0, -50.0]);
        assert_eq!(down.shift(1), 10.0, "the dragged card follows the pointer");
        // Card 3 dragged to the top: 0, 1 and 2 move down.
        let up = drag(3, 0);
        assert_eq!([0, 1, 2].map(|index| up.shift(index)), [50.0, 50.0, 50.0]);
    }
}
