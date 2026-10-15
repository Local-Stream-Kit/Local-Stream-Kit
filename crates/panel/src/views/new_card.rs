//! "New card": the presets to start from. Empty card on its own on top,
//! then a box per group (Alerts, Emote rain, More), a preset a row as on
//! the Cards tab. A new card takes its preset's name (renamed in the
//! editor's bar, where it opens).

use protocol::cards::{self, Preset, PresetGroup};
use yew::prelude::*;

use super::icon;
use crate::Panel;
use crate::route::Route;
use crate::util;

#[function_component]
pub fn NewCard() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let Some(config) = &panel.model.config else { return html! {} };

    let presets = cards::presets();
    let row = |index: usize, preset: &Preset| {
        let create = {
            let panel = panel.clone();
            let existing = config.cards.clone();
            Callback::from(move |_| {
                let mut card = (cards::presets()[index].card)("new");
                card.attach_plugins(&panel.model.plugins);
                card.id = util::new_card_id(&card.name, &existing);
                card.enabled = true;
                let id = card.id.clone();
                panel.add_card(card);
                Route::Card(id).go();
            })
        };
        html! {
            <li>
                <button type="button" class="preset" onclick={create}>
                    if preset.group == PresetGroup::Empty {
                        <span class="preset-plus">{ icon("plus") }</span>
                    }
                    <span class="preset-text">
                        <span class="preset-title">{ preset.title }</span>
                        <span class="preset-description">{ preset.description }</span>
                    </span>
                    <span class="card-chevron">{ icon("chevron") }</span>
                </button>
            </li>
        }
    };
    html! {
        <section class="page">
            <div class="page-title">
                <a href={Route::Cards.href()} aria-label="Back to cards" class="back">{ icon("back") }</a>
                <h1>{ "New card" }</h1>
            </div>
            { for PresetGroup::ALL.into_iter().map(|group| html! {
                <section class={classes!("panel-box", "preset-box", (group == PresetGroup::Empty).then_some("empty"))}>
                    if !group.title().is_empty() {
                        <h2 class="preset-group">{ group.title() }</h2>
                    }
                    <ul class="preset-list">
                        { for presets.iter().enumerate().filter(|(_, preset)| preset.group == group).map(|(index, preset)| row(index, preset)) }
                    </ul>
                </section>
            }) }
        </section>
    }
}

pub fn category_class(category: cards::Category) -> &'static str {
    match category {
        cards::Category::Events => "events",
        cards::Category::Flow => "flow",
        cards::Category::Values => "values",
        cards::Category::Effects => "effects",
    }
}
