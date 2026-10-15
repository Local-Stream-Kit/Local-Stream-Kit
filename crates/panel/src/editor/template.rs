//! A node's text with `{placeholders}` (`TemplateField`): a text area, or a
//! one-line box. Right-clicking a selection in it opens a list of the text
//! styles (Plugins → Text styles); picking one wraps the selection in
//! `[name]…[/]`.

use std::collections::HashSet;

use protocol::text_style::TextStyle;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, HtmlInputElement, HtmlTextAreaElement, KeyboardEvent, MouseEvent, PointerEvent};
use yew::prelude::*;

use crate::{Panel, util};

/// The text with the part from `start` to `end` (in bytes) wrapped in
/// `name`'s marks, and where the wrapped part now starts and ends.
pub fn wrap(text: &str, start: usize, end: usize, name: &str) -> (String, usize, usize) {
    let (start, end) = (start.min(text.len()), end.min(text.len()));
    let (start, end) = (start.min(end), start.max(end));
    let wrapped = format!("{}[{name}]{}[/]{}", &text[..start], &text[start..end], &text[end..]);
    let new_end = end + name.len() + 5;
    (wrapped, start, new_end)
}

/// The styles a text can mark, once each, in the list's order.
fn markable(styles: &[TextStyle]) -> Vec<TextStyle> {
    let mut seen = HashSet::new();
    styles
        .iter()
        .filter(|style| TextStyle::valid_name(&style.name) && seen.insert(style.name.clone()))
        .cloned()
        .collect()
}

/// The field's element, a text area or a one-line box.
enum Field {
    Area(HtmlTextAreaElement),
    Line(HtmlInputElement),
}

impl Field {
    fn of(target: Option<web_sys::EventTarget>) -> Option<Self> {
        let target = target?;
        if let Some(area) = target.dyn_ref::<HtmlTextAreaElement>() {
            Some(Self::Area(area.clone()))
        } else {
            target.dyn_ref::<HtmlInputElement>().map(|line| Self::Line(line.clone()))
        }
    }

    fn value(&self) -> String {
        match self {
            Self::Area(area) => area.value(),
            Self::Line(line) => line.value(),
        }
    }

    /// The selection, in UTF-16 units.
    fn selection(&self) -> Option<(u32, u32)> {
        let (start, end) = match self {
            Self::Area(area) => (area.selection_start().ok()??, area.selection_end().ok()??),
            Self::Line(line) => (line.selection_start().ok()??, line.selection_end().ok()??),
        };
        Some((start, end))
    }

    fn set(&self, value: &str, start: u32, end: u32) {
        match self {
            Self::Area(area) => {
                area.set_value(value);
                let _ = area.focus();
                let _ = area.set_selection_range(start, end);
            }
            Self::Line(line) => {
                line.set_value(value);
                let _ = line.focus();
                let _ = line.set_selection_range(start, end);
            }
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct TemplateFieldProps {
    #[prop_or_default]
    pub id: Option<AttrValue>,
    pub value: AttrValue,
    pub onchange: Callback<String>,
    /// A text area (else a one-line box).
    #[prop_or(true)]
    pub multiline: bool,
    #[prop_or_default]
    pub class: Classes,
    #[prop_or_default]
    pub style: Option<AttrValue>,
    #[prop_or_default]
    pub title: Option<AttrValue>,
    #[prop_or_default]
    pub aria_label: Option<AttrValue>,
    #[prop_or_default]
    pub placeholder: Option<AttrValue>,
}

/// The open style list: where, and the selection it wraps (UTF-16 units).
#[derive(Clone, PartialEq)]
struct Opened {
    x: f64,
    y: f64,
    start: u32,
    end: u32,
}

#[function_component]
pub fn TemplateField(props: &TemplateFieldProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let opened = use_state(|| None::<Opened>);
    let field = use_node_ref();
    let styles = panel.model.config.as_ref().map(|config| markable(&config.text_styles)).unwrap_or_default();

    let oninput = props.onchange.reform(|event: InputEvent| util::value_of(&event));
    let oncontextmenu = {
        let (opened, any) = (opened.clone(), !styles.is_empty());
        Callback::from(move |event: MouseEvent| {
            let Some((start, end)) = Field::of(event.target()).and_then(|field| field.selection()) else {
                return;
            };
            if start == end || !any {
                // The canvas's own menu (add a node), as elsewhere.
                return;
            }
            event.prevent_default();
            event.stop_propagation();
            opened.set(Some(Opened { x: event.client_x(), y: event.client_y(), start, end }));
        })
    };
    let close = {
        let opened = opened.clone();
        Callback::from(move |()| opened.set(None))
    };
    let pick = {
        let (opened, field, onchange) = (opened.clone(), field.clone(), props.onchange.clone());
        Callback::from(move |name: String| {
            let Some(at) = &*opened else { return };
            if let Some(element) = Field::of(field.get().map(Into::into)) {
                let text = element.value();
                let (start, end) = (
                    super::fields::utf16_to_byte(&text, at.start as usize),
                    super::fields::utf16_to_byte(&text, at.end as usize),
                );
                let (wrapped, start, end) = wrap(&text, start, end, &name);
                element.set(
                    &wrapped,
                    super::fields::byte_to_utf16(&wrapped, start),
                    super::fields::byte_to_utf16(&wrapped, end),
                );
                onchange.emit(wrapped);
            }
            opened.set(None);
        })
    };

    let field_html = if props.multiline {
        html! {
            <textarea ref={field.clone()} id={props.id.clone()} class={props.class.clone()} style={props.style.clone()}
                      title={props.title.clone()} aria-label={props.aria_label.clone()} rows="2" spellcheck="false"
                      placeholder={props.placeholder.clone()} value={props.value.clone()} {oninput} {oncontextmenu} />
        }
    } else {
        html! {
            <input type="text" ref={field.clone()} id={props.id.clone()} class={props.class.clone()}
                   style={props.style.clone()} title={props.title.clone()} aria-label={props.aria_label.clone()}
                   spellcheck="false" placeholder={props.placeholder.clone()} value={props.value.clone()}
                   {oninput} {oncontextmenu} />
        }
    };
    html! {
        <>
            { field_html }
            if let Some(at) = &*opened {
                { create_portal(
                    html! { <StyleMenu x={at.x} y={at.y} {styles} on_pick={pick} on_close={close} /> },
                    gloo::utils::body().into(),
                ) }
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
struct StyleMenuProps {
    /// Where the right click was, in the window.
    x: f64,
    y: f64,
    styles: Vec<TextStyle>,
    on_pick: Callback<String>,
    on_close: Callback<()>,
}

/// One row's height in the list, and the list's width and padding, for
/// keeping it inside the window.
const ROW: f64 = 30.0;
const WIDTH: f64 = 200.0;
const PADDING: f64 = 40.0;

/// The style list, over everything (in `<body>`, outside the zoomed canvas).
#[function_component]
fn StyleMenu(props: &StyleMenuProps) -> Html {
    let active = use_state(|| 0_usize);
    let list = use_node_ref();
    {
        let list = list.clone();
        use_effect_with((), move |_| {
            if let Some(list) = list.cast::<HtmlElement>() {
                let _ = list.focus();
            }
        });
    }
    {
        let styles = props.styles.clone();
        use_effect_with(styles, |styles| styles.iter().for_each(crate::views::text_styles::load_font));
    }
    let count = props.styles.len();
    let index = (*active).min(count.saturating_sub(1));
    let onkeydown = {
        let (active, styles) = (active.clone(), props.styles.clone());
        let (on_pick, on_close) = (props.on_pick.clone(), props.on_close.clone());
        Callback::from(move |event: KeyboardEvent| {
            match event.key().as_str() {
                "ArrowDown" => active.set((index + 1).min(count.saturating_sub(1))),
                "ArrowUp" => active.set(index.saturating_sub(1)),
                "Enter" | " " => {
                    if let Some(style) = styles.get(index) {
                        on_pick.emit(style.name.clone());
                    }
                }
                "Escape" | "Tab" => on_close.emit(()),
                _ => return,
            }
            event.prevent_default();
            event.stop_propagation();
        })
    };
    let window = gloo::utils::window();
    let size = |value: Result<wasm_bindgen::JsValue, _>| {
        value.ok().and_then(|value| value.as_f64()).unwrap_or(f64::MAX)
    };
    let (width, height) = (size(window.inner_width()), size(window.inner_height()));
    let x = props.x.min(width - WIDTH - 8.0).max(8.0);
    let y = props.y.min(height - count as f64 * ROW - PADDING).max(8.0);
    let on_backdrop = props.on_close.reform(|event: PointerEvent| {
        event.prevent_default();
        event.stop_propagation();
    });
    let no_menu = Callback::from(|event: MouseEvent| event.prevent_default());

    html! {
        <div class="style-menu-backdrop" onpointerdown={on_backdrop} oncontextmenu={no_menu}>
            <div ref={list} class="menu style-menu" style={format!("left: {x}px; top: {y}px")} role="menu"
                 aria-label="Text style for the selection" tabindex="-1" {onkeydown}
                 onpointerdown={Callback::from(|event: PointerEvent| event.stop_propagation())}>
                <div class="menu-group">{ "Text style" }</div>
                <div class="menu-list">
                    { for props.styles.iter().enumerate().map(|(each, style)| {
                        let onclick = {
                            let (on_pick, name) = (props.on_pick.clone(), style.name.clone());
                            Callback::from(move |_: MouseEvent| on_pick.emit(name.clone()))
                        };
                        let onmouseenter = {
                            let active = active.clone();
                            Callback::from(move |_: MouseEvent| active.set(each))
                        };
                        html! {
                            <button type="button" role="menuitem" key={style.name.clone()} tabindex="-1"
                                    class={classes!("menu-item", (each == index).then_some("active"))}
                                    {onclick} {onmouseenter}>
                                <span class="style-menu-sample" style={style.css()}>{ style.name.clone() }</span>
                            </button>
                        }
                    }) }
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping() {
        assert_eq!(wrap("Ann subscribed", 0, 3, "name"), ("[name]Ann[/] subscribed".to_string(), 0, 12));
        let (text, start, end) = wrap("so hype", 3, 7, "big");
        assert_eq!(text, "so [big]hype[/]");
        assert_eq!(&text[start..end], "[big]hype[/]");
        // Backwards is the same selection.
        assert_eq!(wrap("ab", 2, 0, "x").0, "[x]ab[/]");
    }

    #[test]
    fn markable_styles() {
        let style = |name: &str| TextStyle { name: name.to_string(), ..TextStyle::default() };
        let names: Vec<String> =
            markable(&[style("a"), style(""), style("bad name"), style("a"), style("b")])
                .into_iter()
                .map(|s| s.name)
                .collect();
        assert_eq!(names, ["a", "b"]);
    }
}
