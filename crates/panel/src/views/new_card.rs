//! "New card": the presets to start from. Empty card and Import (a card
//! exported to a file) on their own on top, then a box per group (Alerts,
//! Emote rain, More), a preset a row as on the Cards tab. A new card takes
//! its preset's name (renamed in the editor's bar, where it opens).

use gloo::file::callbacks::FileReader;
use protocol::cards::{self, Preset, PresetGroup};
use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::icon;
use crate::Panel;
use crate::card_file;
use crate::route::Route;
use crate::util;

#[function_component]
pub fn NewCard() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    // Import's file picker, and the file being read (dropping it stops
    // the reading).
    let import_input = use_node_ref();
    let reading = use_mut_ref(|| None::<FileReader>);
    card_file::use_own_problem(String::new());
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
    let choose_import = {
        let import_input = import_input.clone();
        Callback::from(move |_| {
            if let Some(input) = import_input.cast::<HtmlInputElement>() {
                input.click();
            }
        })
    };
    // An imported card goes at the end, as new cards do.
    let on_import = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            if let Some(file) = super::sounds::chosen_file(&event) {
                *reading.borrow_mut() = Some(card_file::import(&panel, file));
            }
        })
    };
    let close_problem = {
        let panel = panel.clone();
        Callback::from(move |_| panel.card_file_problem(None))
    };
    let import = html! {
        <li>
            <button type="button" class="preset" id="new-card-import" onclick={choose_import}>
                <span class="preset-plus">{ icon("upload") }</span>
                <span class="preset-text">
                    <span class="preset-title">{ "Import" }</span>
                    <span class="preset-description">{ "Start from a card exported to a file." }</span>
                </span>
                <span class="card-chevron">{ icon("chevron") }</span>
            </button>
            <input type="file" id="card-import" class="visually-hidden" accept=".json,application/json"
                   tabindex="-1" aria-hidden="true" ref={import_input} onchange={on_import} />
        </li>
    };
    html! {
        <section class="page">
            <div class="page-title">
                <a href={Route::Cards.href()} aria-label="Back to cards" class="back">{ icon("back") }</a>
                <h1>{ "New card" }</h1>
            </div>
            { for PresetGroup::ALL.into_iter().map(|group| html! {
                <>
                    <section class={classes!("panel-box", "preset-box", (group == PresetGroup::Empty).then_some("empty"))}>
                        if !group.title().is_empty() {
                            <h2 class="preset-group">{ group.title() }</h2>
                        }
                        <ul class="preset-list">
                            { for presets.iter().enumerate().filter(|(_, preset)| preset.group == group).map(|(index, preset)| row(index, preset)) }
                            if group == PresetGroup::Empty {
                                { import.clone() }
                            }
                        </ul>
                    </section>
                    if let (PresetGroup::Empty, Some(problem)) = (group, &panel.model.card_file_problem) {
                        <div class="notice warn" role="alert" id="card-file-problem">
                            { icon("warn") }
                            <span>{ problem.clone() }</span>
                            <button type="button" class="notice-close" title="Close" aria-label="Close" onclick={close_problem.clone()}>{ "×" }</button>
                        </div>
                    }
                </>
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
