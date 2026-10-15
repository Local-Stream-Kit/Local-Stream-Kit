//! Node fields whose text isn't simply the stored value: numbers (where "1."
//! is on its way to "1.5") and word lists ("hype, " is on its way to a
//! second word). Each keeps what's typed and reports the parsed value. Lists
//! of emote codes also complete the code being typed. And a dropdown that
//! takes other values too (`PickField`), and a text saved when it's left
//! (`TextField`).

use std::rc::Rc;

use protocol::panel::KnownEmote;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::util;

fn parse_number(text: &str) -> Option<f64> {
    text.trim().replace(',', ".").parse::<f64>().ok().filter(|number| number.is_finite())
}

fn parse_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct NumberFieldProps {
    pub id: AttrValue,
    pub value: f64,
    pub onchange: Callback<f64>,
}

#[function_component]
pub fn NumberField(props: &NumberFieldProps) -> Html {
    let text = use_state(|| util::show_number(props.value));
    {
        // Follow changes from elsewhere (undo), but not our own.
        let text = text.clone();
        use_effect_with(props.value, move |value| {
            if parse_number(&text) != Some(*value) {
                text.set(util::show_number(*value));
            }
        });
    }
    let oninput = {
        let text = text.clone();
        let onchange = props.onchange.clone();
        Callback::from(move |event: InputEvent| {
            let raw = util::value_of(&event);
            if let Some(number) = parse_number(&raw) {
                onchange.emit(number);
            }
            text.set(raw);
        })
    };
    let onblur = {
        let text = text.clone();
        let value = props.value;
        Callback::from(move |_| text.set(util::show_number(value)))
    };
    html! {
        <input id={props.id.clone()} class="num" type="text" inputmode="decimal" autocomplete="off"
               value={(*text).clone()} {oninput} {onblur} />
    }
}

#[derive(Properties, PartialEq)]
pub struct TextFieldProps {
    pub id: AttrValue,
    pub value: AttrValue,
    /// What's typed, once the field is left (or on Enter).
    pub onchange: Callback<String>,
    #[prop_or_default]
    pub placeholder: Option<AttrValue>,
    #[prop_or_default]
    pub title: Option<AttrValue>,
    #[prop_or_default]
    pub aria_label: Option<AttrValue>,
    #[prop_or_default]
    pub class: Classes,
    /// The `<input>`, for reading what's in it before it's saved.
    #[prop_or_default]
    pub field: NodeRef,
}

/// A text box saved when it's left (or on Enter), keeping what's typed
/// until then: a redraw meanwhile (a status update, say) would otherwise
/// put the saved text back, since Yew sets an input's value whenever it
/// differs from the page's (2026-10-15: it brought back what was cleared).
#[function_component]
pub fn TextField(props: &TextFieldProps) -> Html {
    // What's typed, until it's saved (then the value as saved shows, tidied
    // up maybe) or the value changes elsewhere (undo).
    let typed = use_state(|| None::<String>);
    {
        let typed = typed.clone();
        use_effect_with(props.value.clone(), move |value| {
            if typed.as_deref().is_some_and(|typed| typed != value.as_str()) {
                typed.set(None);
            }
        });
    }
    let oninput = {
        let typed = typed.clone();
        Callback::from(move |event: InputEvent| typed.set(Some(util::value_of(&event))))
    };
    let onchange = {
        let (typed, onchange) = (typed.clone(), props.onchange.clone());
        Callback::from(move |event: Event| {
            typed.set(None);
            onchange.emit(util::value_of(&event));
        })
    };
    let shown = typed.as_ref().map_or_else(|| props.value.clone(), |typed| AttrValue::from(typed.clone()));
    html! {
        <input type="text" id={props.id.clone()} class={props.class.clone()} ref={props.field.clone()}
               spellcheck="false" value={shown} placeholder={props.placeholder.clone()}
               title={props.title.clone()} aria-label={props.aria_label.clone()} {oninput} {onchange} />
    }
}

fn is_separator(c: char) -> bool {
    c == ',' || c.is_whitespace()
}

/// The word being typed: from the separator before the caret to the one
/// after it, as byte offsets. `caret` is a byte offset.
fn word_at(text: &str, caret: usize) -> (usize, usize) {
    let caret = caret.min(text.len());
    let start = text[..caret].rfind(is_separator).map_or(0, |at| at + 1);
    let end = text[caret..].find(is_separator).map_or(text.len(), |at| caret + at);
    (start, end)
}

/// Codes completing `word`: those starting with it first, then those
/// containing it, ignoring case.
fn suggestions<'a>(emotes: &'a [KnownEmote], word: &str, limit: usize) -> Vec<&'a KnownEmote> {
    let word = word.to_lowercase();
    if word.is_empty() {
        return Vec::new();
    }
    let lower = |emote: &KnownEmote| emote.code.to_lowercase();
    let starting = emotes.iter().filter(|emote| lower(emote).starts_with(&word));
    let containing = emotes.iter().filter(|emote| {
        let code = lower(emote);
        !code.starts_with(&word) && code.contains(&word)
    });
    starting.chain(containing).take(limit).collect()
}

/// `text` with the word at `start..end` replaced by `code`, and where the
/// caret goes (a byte offset). At the end of the text, a separator follows,
/// ready for the next code.
fn complete(text: &str, (start, end): (usize, usize), code: &str) -> (String, usize) {
    let rest = &text[end..];
    let mut completed = format!("{}{code}", &text[..start]);
    if rest.trim().is_empty() {
        completed.push_str(", ");
        (completed.clone(), completed.len())
    } else {
        let caret = completed.len();
        completed.push_str(rest);
        (completed, caret)
    }
}

/// The browser counts the caret in UTF-16 units; Rust strings in bytes.
pub(crate) fn utf16_to_byte(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

pub(crate) fn byte_to_utf16(text: &str, byte: usize) -> u32 {
    text[..byte.min(text.len())].encode_utf16().count() as u32
}

/// How many completions show at once.
const SUGGESTIONS: usize = 8;

#[derive(Properties, PartialEq)]
pub struct ListFieldProps {
    pub id: AttrValue,
    pub value: Vec<String>,
    pub onchange: Callback<Vec<String>>,
    #[prop_or_default]
    pub placeholder: AttrValue,
    /// Emote codes to complete from, with their images.
    #[prop_or_default]
    pub emotes: Option<Rc<Vec<KnownEmote>>>,
    #[prop_or_default]
    pub onfocus: Callback<()>,
}

/// Words or emote codes, separated by commas or spaces. With `emotes`, the
/// code being typed is completed from them.
#[function_component]
pub fn ListField(props: &ListFieldProps) -> Html {
    let text = use_state(|| props.value.join(", "));
    // The word being completed (byte offsets), while completions show.
    let word = use_state(|| None::<(usize, usize)>);
    let active = use_state(|| 0_usize);
    let input = use_node_ref();
    {
        let text = text.clone();
        use_effect_with(props.value.clone(), move |value| {
            if &parse_list(&text) != value {
                text.set(value.join(", "));
            }
        });
    }

    let shown: Vec<KnownEmote> = match (&props.emotes, *word) {
        (Some(emotes), Some((start, end))) => {
            suggestions(emotes, text.get(start..end).unwrap_or_default(), SUGGESTIONS)
                .into_iter()
                .cloned()
                .collect()
        }
        _ => Vec::new(),
    };
    let active_index = (*active).min(shown.len().saturating_sub(1));

    let accept = {
        let text = text.clone();
        let word = word.clone();
        let input = input.clone();
        let onchange = props.onchange.clone();
        move |code: &str| {
            let Some(range) = *word else { return };
            let (completed, caret) = complete(&text, range, code);
            if let Some(field) = input.cast::<HtmlInputElement>() {
                field.set_value(&completed);
                let caret = byte_to_utf16(&completed, caret);
                let _ = field.set_selection_range(caret, caret);
            }
            onchange.emit(parse_list(&completed));
            text.set(completed);
            word.set(None);
        }
    };
    let oninput = {
        let text = text.clone();
        let word = word.clone();
        let active = active.clone();
        let onchange = props.onchange.clone();
        let completing = props.emotes.is_some();
        Callback::from(move |event: InputEvent| {
            let raw = util::value_of(&event);
            if completing {
                let caret = event
                    .target_dyn_into::<HtmlInputElement>()
                    .and_then(|field| field.selection_start().ok().flatten())
                    .map_or(raw.len(), |caret| utf16_to_byte(&raw, caret as usize));
                word.set(Some(word_at(&raw, caret)));
                active.set(0);
            }
            onchange.emit(parse_list(&raw));
            text.set(raw);
        })
    };
    let onkeydown = {
        let word = word.clone();
        let active = active.clone();
        let shown = shown.clone();
        let accept = accept.clone();
        Callback::from(move |event: KeyboardEvent| {
            if shown.is_empty() {
                return;
            }
            match event.key().as_str() {
                "ArrowDown" => active.set((active_index + 1) % shown.len()),
                "ArrowUp" => active.set((active_index + shown.len() - 1) % shown.len()),
                "Enter" | "Tab" => accept(&shown[active_index].code),
                "Escape" => word.set(None),
                _ => return,
            }
            event.prevent_default();
            event.stop_propagation();
        })
    };
    let onblur = {
        let text = text.clone();
        let word = word.clone();
        let value = props.value.clone();
        Callback::from(move |_| {
            text.set(value.join(", "));
            word.set(None);
        })
    };
    let onfocus = props.onfocus.reform(|_: FocusEvent| ());
    let list_id = format!("{}-completions", props.id);
    let open = !shown.is_empty();

    html! {
        <div class="list-field">
            <input ref={input} id={props.id.clone()} type="text" autocomplete="off" spellcheck="false"
                   placeholder={props.placeholder.clone()} value={(*text).clone()}
                   role={props.emotes.is_some().then_some("combobox")}
                   aria-autocomplete={props.emotes.is_some().then_some("list")}
                   aria-expanded={props.emotes.is_some().then(|| open.to_string())}
                   aria-controls={open.then(|| list_id.clone())}
                   {oninput} {onkeydown} {onblur} {onfocus} />
            if open {
                <div class="completions" id={list_id} role="listbox">
                    { for shown.iter().enumerate().map(|(index, emote)| {
                        let onmousedown = {
                            let accept = accept.clone();
                            let code = emote.code.clone();
                            Callback::from(move |event: MouseEvent| {
                                // Keep the focus in the field.
                                event.prevent_default();
                                accept(&code);
                            })
                        };
                        let selected = index == active_index;
                        html! {
                            <div key={format!("{}-{}", emote.source, emote.id)} role="option"
                                 aria-selected={selected.to_string()}
                                 class={classes!("completion", selected.then_some("active"))} {onmousedown}>
                                <img src={protocol::emote_path(&emote.source, &emote.id)} alt="" loading="lazy" />
                                <span class="code">{ emote.code.clone() }</span>
                                <span class="source">{ emote.source.to_uppercase() }</span>
                            </div>
                        }
                    }) }
                </div>
            }
        </div>
    }
}

/// `PickField`'s "Other…" option.
const OTHER: &str = "\u{1}other";

#[derive(Properties, PartialEq)]
pub struct PickFieldProps {
    pub id: AttrValue,
    pub value: AttrValue,
    pub options: Vec<String>,
    /// What options show where it isn't the value itself, by value (a
    /// plugin's `set-choices`).
    #[prop_or_default]
    pub labels: std::collections::BTreeMap<String, String>,
    /// The first option's label, standing for an empty value; without it
    /// there's no such option.
    #[prop_or_default]
    pub empty: Option<AttrValue>,
    #[prop_or_default]
    pub placeholder: Option<AttrValue>,
    #[prop_or_default]
    pub title: Option<AttrValue>,
    pub onchange: Callback<String>,
}

/// A dropdown of choices whose last one, "Other…", shows a text box beside
/// it (`{id}-other`) for any other value; a value that isn't one of the
/// choices shows there; with nothing to pick, only the box (`{id}`). While typing, the box keeps what's typed (so
/// emptying it isn't undone by a page that falls back to a default). Not a
/// `<datalist>`: OBS docks don't open those (2026-10-12).
#[function_component]
pub fn PickField(props: &PickFieldProps) -> Html {
    let value = props.value.as_str();
    let listed =
        (props.empty.is_some() && value.is_empty()) || props.options.iter().any(|each| each == value);
    // Some(typed) once "Other…" is picked or the box typed in.
    let other = use_state(|| None::<String>);
    let typing = other.is_some() || !listed;
    let input = use_node_ref();
    {
        // Into the box once "Other…" is picked.
        let input = input.clone();
        use_effect_with(other.is_some(), move |other| {
            if *other && let Some(input) = input.cast::<HtmlInputElement>() {
                let _ = input.focus();
            }
        });
    }
    let on_pick = {
        let (other, onchange) = (other.clone(), props.onchange.clone());
        let kept = if listed { String::new() } else { value.to_string() };
        Callback::from(move |event: Event| {
            let picked = util::value_of(&event);
            if picked == OTHER {
                other.set(Some(kept.clone()));
            } else {
                other.set(None);
                onchange.emit(picked);
            }
        })
    };
    let oninput = {
        let (other, onchange) = (other.clone(), props.onchange.clone());
        Callback::from(move |event: InputEvent| {
            let typed = util::value_of(&event);
            other.set(Some(typed.clone()));
            onchange.emit(typed);
        })
    };
    let shown = other.as_ref().map_or_else(|| props.value.clone(), |typed| AttrValue::from(typed.clone()));
    if props.options.is_empty() && props.empty.is_none() {
        // Nothing to pick: only the box.
        return html! {
            <input type="text" id={props.id.clone()} value={shown} placeholder={props.placeholder.clone()}
                   title={props.title.clone()} spellcheck="false" {oninput} />
        };
    }
    html! {
        <div class="pick-field">
            <select id={props.id.clone()} title={props.title.clone()} onchange={on_pick}>
                if let Some(empty) = &props.empty {
                    <option value="" selected={!typing && value.is_empty()}>{ empty.clone() }</option>
                }
                { for props.options.iter().map(|each| html! {
                    <option value={each.clone()} selected={!typing && each == value}>
                        { props.labels.get(each).unwrap_or(each).clone() }
                    </option>
                }) }
                <option value={OTHER} selected={typing}>{ "Other…" }</option>
            </select>
            if typing {
                <input type="text" id={format!("{}-other", props.id)} ref={input} value={shown}
                       placeholder={props.placeholder.clone()} title={props.title.clone()} spellcheck="false"
                       {oninput} />
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(parse_number(" 1,5 "), Some(1.5));
        assert_eq!(parse_number("1."), Some(1.0));
        assert_eq!(parse_number("-"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_list("hype, letsgo  PogU,,"), ["hype", "letsgo", "PogU"]);
    }

    fn emote(code: &str) -> KnownEmote {
        KnownEmote { code: code.into(), source: "7tv".into(), id: code.to_lowercase() }
    }

    #[test]
    fn completing_codes() {
        // The word around the caret.
        assert_eq!(word_at("Kappa, Po", 9), (7, 9));
        assert_eq!(word_at("Kappa, Po", 3), (0, 5));
        assert_eq!(word_at("", 0), (0, 0));

        let emotes = [emote("PogU"), emote("Pog"), emote("KEKW"), emote("catPog")];
        let codes =
            |word| suggestions(&emotes, word, 8).into_iter().map(|e| e.code.as_str()).collect::<Vec<_>>();
        assert_eq!(codes("pog"), ["PogU", "Pog", "catPog"], "starting with it first, then containing it");
        assert_eq!(codes(""), Vec::<&str>::new());
        assert_eq!(suggestions(&emotes, "pog", 1).len(), 1);

        // At the end, a separator follows for the next code.
        assert_eq!(complete("Kappa, Po", (7, 9), "PogU"), ("Kappa, PogU, ".to_string(), 13));
        // In the middle, the rest stays and the caret goes after the code.
        assert_eq!(complete("Ka, PogU", (0, 2), "Kappa"), ("Kappa, PogU".to_string(), 5));

        assert_eq!(utf16_to_byte("é Kappa", 2), 3);
        assert_eq!(byte_to_utf16("é Kappa", 3), 2);
    }
}
