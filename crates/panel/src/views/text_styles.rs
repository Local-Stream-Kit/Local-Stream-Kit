//! Text styles (a box at the top of the Plugins tab): the app's named
//! looks for parts of a text, which a node's text marks with
//! `[name]…[/]`. Each has a name, a font, a size (of the text around it),
//! bold, italic and a colour, and a sample. Laid out as the plugin boxes'
//! settings, a row each.

use std::collections::HashSet;

use protocol::config::AppConfig;
use protocol::plugin::Choice;
use protocol::text_style::{FONT_SUGGESTIONS, MAX_SIZE_PERCENT, MIN_SIZE_PERCENT, TextStyle};
use serde_json::Value;
use yew::prelude::*;

use super::icon;
use super::plugins::choice_buttons;
use crate::editor::fields::PickField;
use crate::{Panel, util};

/// Changes the style at `index`.
fn edit_style(panel: &Panel, index: usize, key: String, change: impl Fn(&mut TextStyle) + 'static) {
    panel.edit(Some(&key), move |config: &mut AppConfig| {
        if let Some(style) = config.text_styles.get_mut(index) {
            change(style);
        }
    });
}

/// Unchanged (None), On or Off, as a button's value.
fn from_choice(value: &str) -> Option<bool> {
    match value {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

fn to_choice(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "on",
        Some(false) => "off",
        None => "unchanged",
    }
}

/// Bold and italic: Unchanged, On or Off, as a row of buttons
/// (`{id}-unchanged`, `{id}-on`, `{id}-off`).
fn on_off_buttons(id: &str, label: &str, value: Option<bool>, set: Callback<Option<bool>>) -> Html {
    let options = [("unchanged", "Unchanged"), ("on", "On"), ("off", "Off")]
        .map(|(value, label)| Choice { value: value.into(), label: label.into() });
    let set = set.reform(|value: Value| from_choice(value.as_str().unwrap_or_default()));
    choice_buttons(id, label, &options, to_choice(value), set)
}

/// A name no style has yet: `style`, `style_2`, …
fn free_name(styles: &[TextStyle]) -> String {
    let taken = |name: &str| styles.iter().any(|style| style.name == name);
    std::iter::once("style".to_string())
        .chain((2..).map(|n| format!("style_{n}")))
        .find(|name| !taken(name))
        .expect("some number is free")
}

/// Loads the Google Fonts stylesheet for a font, for the samples (once).
pub(crate) fn load_font(style: &TextStyle) {
    let Some(sheet) = style.font_stylesheet() else { return };
    let document = gloo::utils::document();
    let selector = format!("link[data-font=\"{}\"]", style.font.replace(['"', '\\'], ""));
    if document.query_selector(&selector).ok().flatten().is_some() {
        return;
    }
    if let Ok(link) = document.create_element("link") {
        let _ = link.set_attribute("rel", "stylesheet");
        let _ = link.set_attribute("href", &sheet);
        let _ = link.set_attribute("data-font", &style.font);
        let _ = document.head().map(|head| head.append_child(&link));
    }
}

#[function_component]
pub fn TextStyles() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let open = use_state(|| false);
    // The styles that are open (their fields), by index; the rest show a line.
    let open_styles = use_state(HashSet::<usize>::new);
    let Some(config) = &panel.model.config else { return html! {} };
    let styles = config.text_styles.clone();
    let flip = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };
    // Their switch, as a plugin's: off, marks give no look.
    let on_toggle = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            panel.edit(None, move |config: &mut AppConfig| config.text_styles_enabled = on);
        })
    };
    let add = {
        let (panel, open_styles, new_index) = (panel.clone(), open_styles.clone(), styles.len());
        Callback::from(move |_: MouseEvent| {
            let mut open = (*open_styles).clone();
            open.insert(new_index);
            open_styles.set(open);
            panel.edit(None, |config: &mut AppConfig| {
                let name = free_name(&config.text_styles);
                config.text_styles.push(TextStyle { name, ..TextStyle::default() });
            });
        })
    };
    let mut seen = HashSet::new();
    let repeated: Vec<bool> = styles.iter().map(|style| !seen.insert(style.name.clone())).collect();

    html! {
        <section class="panel-box" id="text-styles">
            <div class="box-head">
                <input class="switch" type="checkbox" id="text-styles-on" checked={config.text_styles_enabled}
                       onchange={on_toggle} aria-label="Text styles on" />
                <button type="button" class={classes!("box-title", "box-fold", open.then_some("open"))}
                        id="text-styles-open" aria-expanded={open.to_string()} aria-controls="text-styles-body"
                        onclick={flip}>
                    { icon("chevron") }
                    { "Text styles" }
                </button>
                <span class="muted small">{ "built in" }</span>
            </div>
            if *open {
                <div class="box-body" id="text-styles-body">
                    <p class="muted small">
                        { "Give part of a node's text a different look by putting it between " }
                        <code>{ "[name]" }</code>{ " and " }<code>{ "[/]" }</code>
                    </p>
                    { for styles.iter().enumerate().map(|(index, style)| {
                        let is_open = open_styles.contains(&index);
                        let flip = {
                            let open_styles = open_styles.clone();
                            Callback::from(move |_: MouseEvent| {
                                let mut open = (*open_styles).clone();
                                if !open.remove(&index) {
                                    open.insert(index);
                                }
                                open_styles.set(open);
                            })
                        };
                        // The ones after it move up one.
                        let deleted = {
                            let open_styles = open_styles.clone();
                            Callback::from(move |()| {
                                let open = open_styles
                                    .iter()
                                    .filter(|&&other| other != index)
                                    .map(|&other| if other > index { other - 1 } else { other })
                                    .collect();
                                open_styles.set(open);
                            })
                        };
                        html! {
                            <StyleRow key={index} {index} style={style.clone()} repeated={repeated[index]}
                                      open={is_open} onflip={flip} ondeleted={deleted} />
                        }
                    }) }
                    <div class="button-row">
                        <button type="button" class="button small" id="text-styles-add" onclick={add}>
                            { icon("plus") }{ "Add a style" }
                        </button>
                    </div>
                </div>
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct StyleRowProps {
    index: usize,
    style: TextStyle,
    /// An earlier style has the same name (that one is used).
    repeated: bool,
    /// Its fields show; otherwise a line with its name and a sample.
    open: bool,
    onflip: Callback<MouseEvent>,
    ondeleted: Callback<()>,
}

#[function_component]
fn StyleRow(props: &StyleRowProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let (index, style) = (props.index, &props.style);
    let base = format!("text-style-{index}");
    load_font(style);
    let set = |field: &str, change: fn(&mut TextStyle, String)| {
        let (panel, key) = (panel.clone(), format!("{base}-{field}"));
        Callback::from(move |value: String| {
            edit_style(&panel, index, key.clone(), move |style| change(style, value.clone()));
        })
    };
    let rename =
        set("name", |style, name| style.name = name).reform(|event: InputEvent| util::value_of(&event));
    let font = set("font", |style, font| style.font = font);
    let size = {
        let (panel, key) = (panel.clone(), format!("{base}-size"));
        Callback::from(move |event: InputEvent| {
            let Some(size) = util::number_of(&event) else { return };
            let size = (size as f32).clamp(MIN_SIZE_PERCENT, MAX_SIZE_PERCENT);
            edit_style(&panel, index, key.clone(), move |style| style.size_percent = size);
        })
    };
    let on_off = |field: &str, change: fn(&mut TextStyle, Option<bool>)| {
        let (panel, key) = (panel.clone(), format!("{base}-{field}"));
        Callback::from(move |value: Option<bool>| {
            edit_style(&panel, index, key.clone(), move |style| change(style, value));
        })
    };
    let bold = on_off("bold", |style, bold| style.bold = bold);
    let italic = on_off("italic", |style, italic| style.italic = italic);
    let own_colour = {
        let (panel, key) = (panel.clone(), format!("{base}-own-colour"));
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            // Unticked, the colour is kept (not applied) for ticking again.
            edit_style(&panel, index, key.clone(), move |style| {
                if on {
                    let kept = std::mem::take(&mut style.kept_colour);
                    style.colour = if kept.is_empty() { "#ffcc00".into() } else { kept };
                } else if !style.colour.is_empty() {
                    style.kept_colour = std::mem::take(&mut style.colour);
                }
            });
        })
    };
    let colour = set("colour", |style, colour| style.colour = colour)
        .reform(|event: InputEvent| util::value_of(&event));
    let delete = {
        let (panel, ondeleted) = (panel.clone(), props.ondeleted.clone());
        Callback::from(move |_: MouseEvent| {
            panel.edit(None, move |config: &mut AppConfig| {
                if index < config.text_styles.len() {
                    config.text_styles.remove(index);
                }
            });
            ondeleted.emit(());
        })
    };
    let has_colour = !style.colour.is_empty();
    let problem = if !TextStyle::valid_name(&style.name) {
        Some("Use only letters, digits and _ in the name (no spaces).")
    } else if props.repeated {
        Some("Another style above has this name; that one is used.")
    } else {
        None
    };

    html! {
        <div class="text-style-row" id={base.clone()}>
            <div class="text-style-fold">
                <button type="button" class={classes!("box-fold", "text-style-title", props.open.then_some("open"))}
                        id={format!("{base}-open")} aria-expanded={props.open.to_string()}
                        aria-controls={format!("{base}-body")} onclick={props.onflip.clone()}>
                    { icon("chevron") }
                    if style.name.is_empty() {
                        <span class="muted">{ "No name" }</span>
                    } else {
                        { style.name.clone() }
                    }
                    if let (Some(problem), false) = (problem, props.open) {
                        <span class="dot warn" title={problem}></span>
                    }
                </button>
                if !props.open {
                    <span class="text-style-preview" id={format!("{base}-preview")}>
                        <span style={style.css()}>{ "Ann" }</span>{ " subscribed!" }
                    </span>
                }
            </div>
            if props.open {
                <div class="text-style-body" id={format!("{base}-body")}>
                    <div class="text-style-sample" id={format!("{base}-sample")}>
                        <span style={style.css()}>{ "Ann" }</span>
                        { " subscribed!" }
                    </div>
                    <div class="setting-row">
                        <label class="setting-label" for={format!("{base}-name")}>{ "Name" }</label>
                        <div class="setting-control">
                            <div class="text-style-name">
                                <input type="text" id={format!("{base}-name")} spellcheck="false"
                                       value={style.name.clone()} oninput={rename} />
                                <button type="button" class="icon-button flat" id={format!("{base}-delete")}
                                        aria-label="Delete this style"
                                        title="Delete (texts that mark parts with it show them plainly, with the mark written out)"
                                        onclick={delete}>{ icon("trash") }</button>
                            </div>
                        </div>
                    </div>
                    if let Some(problem) = problem {
                        <div class="notice warn" role="alert">{ icon("warn") }<div>{ problem }</div></div>
                    }
                    <div class="setting-rows">
                        <div class="setting-row">
                            <label class="setting-label" for={format!("{base}-font")}>{ "Font" }</label>
                            <div class="setting-control">
                                <PickField id={format!("{base}-font")} value={style.font.clone()}
                                           options={FONT_SUGGESTIONS.iter().map(|font| font.to_string()).collect::<Vec<_>>()}
                                           empty={AttrValue::from("Unchanged")} onchange={font} />
                            </div>
                        </div>
                        <div class="setting-row">
                            <span class="setting-label" id={format!("{base}-bold-label")}>{ "Bold" }</span>
                            <div class="setting-control">
                                { on_off_buttons(&format!("{base}-bold"), "Bold", style.bold, bold) }
                            </div>
                        </div>
                        <div class="setting-row">
                            <span class="setting-label" id={format!("{base}-italic-label")}>{ "Italic" }</span>
                            <div class="setting-control">
                                { on_off_buttons(&format!("{base}-italic"), "Italic", style.italic, italic) }
                            </div>
                        </div>
                        <div class="setting-row">
                            <label class="setting-label" for={format!("{base}-size")}>{ "Size" }</label>
                            <div class="setting-control">
                                <div class="slider-row">
                                    <input type="range" id={format!("{base}-size")} title="Of the text around it"
                                           min={MIN_SIZE_PERCENT.to_string()} max={MAX_SIZE_PERCENT.to_string()} step="5"
                                           value={style.size_percent.to_string()} oninput={size} />
                                    <span class="slider-value mono">{ format!("{:.0}%", style.size_percent) }</span>
                                </div>
                            </div>
                        </div>
                        <div class="setting-row">
                            <label class="setting-label" for={format!("{base}-own-colour")}>{ "Colour" }</label>
                            <div class="setting-control">
                                <div class="check-field">
                                    <input type="checkbox" id={format!("{base}-own-colour")} checked={has_colour}
                                           onchange={own_colour} />
                                    if has_colour {
                                        <input type="color" class="swatch" id={format!("{base}-colour")} aria-label="Colour"
                                               value={style.colour.clone()} oninput={colour} />
                                    } else {
                                        <label class="muted" for={format!("{base}-own-colour")}>{ "Unchanged" }</label>
                                    }
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
            }
        </div>
    }
}
