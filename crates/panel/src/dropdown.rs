//! Dropdowns' open lists, drawn in the page.
//!
//! A `<select>`'s own list is a little window of its own, which the
//! browser draws outside the page. In an OBS dock on a screen with a
//! different scaling it's measured at one scale and drawn at another
//! (partly blank, or away from its dropdown); it doesn't follow the canvas
//! zoom; and the first one opened lays out before the web font has loaded
//! there. So a press on any dropdown (or Space, F4, Alt+↓ on it) opens this
//! list instead, next to the dropdown at its size on screen. Picking from
//! it sets the `<select>` and sends `input` and `change`, as the browser's
//! list would, so the dropdowns themselves stay plain `<select>`s.

use std::cell::RefCell;
use std::rc::Rc;

use gloo::events::{EventListener, EventListenerOptions, EventListenerPhase};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Element, EventInit, FocusOptions, HtmlElement, HtmlOptionElement, HtmlSelectElement, KeyboardEvent,
    MouseEvent, MutationObserver, MutationObserverInit, ScrollIntoViewOptions, ScrollLogicalPosition,
};
use yew::prelude::*;

/// Between the dropdown and its list, and kept clear of the window's edges
/// (px).
const GAP: f64 = 2.0;
const EDGE: f64 = 4.0;
/// A row's height, and the list's padding above and below, in em (as in
/// style.css).
const ROW_EM: f64 = 1.8;
const PAD_EM: f64 = 0.3;
/// Letters typed this close together find an option by what it starts
/// with (ms).
const TYPED_MS: f64 = 1000.0;
/// Options Page Up / Page Down move by.
const PAGE: usize = 8;

struct Open {
    select: HtmlSelectElement,
    /// The highlighted option's index.
    active: u32,
    /// Typed so far to find an option, and when.
    typed: String,
    typed_at: f64,
    /// Scroll the highlighted option into view after the next render.
    reveal: bool,
    /// Redraws when the options change (a folder's files arriving).
    observer: MutationObserver,
    _on_change: Closure<dyn FnMut()>,
}

impl Drop for Open {
    fn drop(&mut self) {
        self.observer.disconnect();
        let _ = self.select.remove_attribute("aria-expanded");
    }
}

/// The open list, if any, and a way to redraw it.
#[derive(Clone)]
struct Lists {
    open: Rc<RefCell<Option<Open>>>,
    redraw: UseForceUpdateHandle,
}

impl Lists {
    fn is_open(&self) -> bool {
        self.open.borrow().is_some()
    }

    fn showing(&self, select: &HtmlSelectElement) -> bool {
        self.open.borrow().as_ref().is_some_and(|open| &open.select == select)
    }

    fn open(&self, select: HtmlSelectElement) {
        let selected = u32::try_from(select.selected_index()).ok().filter(|&index| choosable(&select, index));
        let Some(active) = selected.or_else(|| end(&select, false)) else { return };
        let _ = select.set_attribute("aria-expanded", "true");
        let redraw = self.redraw.clone();
        let on_change = Closure::<dyn FnMut()>::new(move || redraw.force_update());
        let Ok(observer) = MutationObserver::new(on_change.as_ref().unchecked_ref()) else { return };
        let watch = MutationObserverInit::new();
        watch.set_child_list(true);
        watch.set_subtree(true);
        watch.set_attributes(true);
        watch.set_character_data(true);
        let _ = observer.observe_with_options(&select, &watch);
        let open = Open {
            select,
            active,
            typed: String::new(),
            typed_at: 0.0,
            reveal: true,
            observer,
            _on_change: on_change,
        };
        let replaced = self.open.borrow_mut().replace(open);
        drop(replaced);
        self.redraw.force_update();
    }

    fn close(&self) {
        let closed = self.open.borrow_mut().take();
        if closed.is_some() {
            drop(closed);
            self.redraw.force_update();
        }
    }

    /// Highlights another option, found from the select and the
    /// highlighted one; `reveal`: scrolls to it.
    fn highlight(&self, reveal: bool, to: impl FnOnce(&HtmlSelectElement, u32) -> Option<u32>) {
        let moved = {
            let mut open = self.open.borrow_mut();
            let Some(open) = open.as_mut() else { return };
            match to(&open.select, open.active) {
                Some(index) if index != open.active => {
                    open.active = index;
                    open.reveal = reveal;
                    true
                }
                _ => false,
            }
        };
        if moved {
            self.redraw.force_update();
        }
    }

    /// Finds an option by the letters typed.
    fn find(&self, letter: &str) {
        let now = js_sys::Date::now();
        let typed = {
            let mut open = self.open.borrow_mut();
            let Some(open) = open.as_mut() else { return };
            if now - open.typed_at > TYPED_MS {
                open.typed.clear();
            }
            open.typed.push_str(&letter.to_lowercase());
            open.typed_at = now;
            open.typed.clone()
        };
        self.highlight(true, |select, active| typed_option(select, active, &typed));
    }

    /// Closes the list and picks an option, as the browser's list would.
    fn pick(&self, index: u32) {
        let Some(open) = self.open.borrow_mut().take() else { return };
        let select = open.select.clone();
        drop(open);
        self.redraw.force_update();
        if select.selected_index() != index as i32 {
            select.set_selected_index(index as i32);
            for kind in ["input", "change"] {
                let init = EventInit::new();
                init.set_bubbles(true);
                if let Ok(event) = Event::new_with_event_init_dict(kind, &init) {
                    let _ = select.dispatch_event(&event);
                }
            }
        }
    }

    fn pick_active(&self) {
        let active = self.open.borrow().as_ref().map(|open| open.active);
        if let Some(active) = active {
            self.pick(active);
        }
    }
}

/// Opens, runs and closes the open list; put it once in the app.
#[function_component]
pub fn Dropdowns() -> Html {
    let open = use_mut_ref(|| None::<Open>);
    let lists = Lists { open: open.clone(), redraw: use_force_update() };
    let list = use_node_ref();

    {
        let lists = lists.clone();
        use_effect_with((), move |_| {
            let listeners = listen(&lists);
            move || drop(listeners)
        });
    }
    // After each render: in the window, and the highlighted option in view.
    {
        let (list, open) = (list.clone(), open.clone());
        use_effect(move || {
            if let Some(element) = list.cast::<HtmlElement>() {
                settle(&element, &open);
            }
        });
    }

    let open = open.borrow();
    let Some(open) = open.as_ref().filter(|open| open.select.is_connected()) else { return html! {} };
    let rows = rows(&open.select);
    let style = place(&open.select, rows.len());
    // Pressing in the list keeps the focus on the dropdown.
    let onmousedown = Callback::from(|event: MouseEvent| event.prevent_default());
    html! {
        <div class="dropdown-list" role="listbox" ref={list} {style} {onmousedown}>
            { for rows.into_iter().map(|row| match row {
                Row::Group(label) => html! { <div class="dropdown-group" role="presentation">{ label }</div> },
                Row::Choice { index, label, enabled, grouped } => {
                    let active = index == open.active;
                    let onmousemove = enabled.then(|| {
                        let lists = lists.clone();
                        Callback::from(move |_: MouseEvent| lists.highlight(false, |_, _| Some(index)))
                    });
                    // On release, so pressing on the dropdown and letting go
                    // on an option picks it too.
                    let onmouseup = enabled.then(|| {
                        let lists = lists.clone();
                        Callback::from(move |event: MouseEvent| {
                            if event.button() == 0 {
                                lists.pick(index);
                            }
                        })
                    });
                    html! {
                        <div role="option" aria-selected={active.to_string()} aria-disabled={(!enabled).then_some("true")}
                             class={classes!("dropdown-option", active.then_some("active"),
                                             (!enabled).then_some("disabled"), grouped.then_some("grouped"))}
                             {onmousemove} {onmouseup}>{ label }</div>
                    }
                }
            }) }
        </div>
    }
}

/// The page's events that open, steer and close the list.
fn listen(lists: &Lists) -> Vec<EventListener> {
    let document = gloo::utils::document();
    let window = gloo::utils::window();
    let watch = EventListenerOptions::run_in_capture_phase();
    let steer = EventListenerOptions { phase: EventListenerPhase::Capture, passive: false };
    let on = |target: &web_sys::EventTarget,
              kind: &'static str,
              options: EventListenerOptions,
              handle: fn(&Lists, &Event)| {
        let lists = lists.clone();
        EventListener::new_with_options(target, kind, options, move |event| handle(&lists, event))
    };
    vec![
        // A press anywhere else closes it.
        on(&document, "pointerdown", watch, |lists, event| {
            let on_it = dropdown_of(event).is_some_and(|select| lists.showing(&select));
            if !in_list(event) && !on_it {
                lists.close();
            }
        }),
        // A press on a dropdown opens it instead of the browser's list; a
        // second one closes it.
        on(&document, "mousedown", steer, |lists, event| {
            let Some(event) = event.dyn_ref::<MouseEvent>() else { return };
            if event.button() != 0 || in_list(event) {
                return;
            }
            let Some(select) = dropdown_of(event).filter(opens) else { return };
            event.prevent_default();
            let again = lists.showing(&select);
            lists.close();
            let focus = FocusOptions::new();
            focus.set_prevent_scroll(true);
            let _ = select.focus_with_options(&focus);
            if !again {
                lists.open(select);
            }
        }),
        on(&document, "keydown", steer, keydown),
        // The dropdown losing the focus (Tab, another window) closes it.
        on(&document, "focusout", watch, |lists, event| {
            if dropdown_of(event).is_some_and(|select| lists.showing(&select)) {
                lists.close();
            }
        }),
        // So does what moves the dropdown away: the wheel (the canvas
        // zooms), scrolling what it's in, a new window size.
        on(&document, "wheel", watch, |lists, event| {
            if !in_list(event) {
                lists.close();
            }
        }),
        on(&document, "scroll", watch, |lists, event| {
            let open = lists.open.borrow().as_ref().map(|open| open.select.clone());
            let Some(select) = open else { return };
            let scrolled = event.target().and_then(|target| target.dyn_into::<Element>().ok());
            if scrolled.is_none_or(|scrolled| scrolled.contains(Some(&select))) {
                lists.close();
            }
        }),
        on(&window, "resize", watch, |lists, _| lists.close()),
    ]
}

/// Keys: Space, F4 or Alt+↓ on a dropdown open it; while open, the arrows,
/// Page Up/Down, Home and End move the highlight, letters find an option,
/// Enter or Space picks and Escape closes. Other keys close it and do what
/// they do.
fn keydown(lists: &Lists, event: &Event) {
    let Some(event) = event.dyn_ref::<KeyboardEvent>() else { return };
    let key = event.key();
    let shortcut = event.ctrl_key() || event.meta_key();
    if !lists.is_open() {
        let Some(select) = dropdown_of(event).filter(opens) else { return };
        let plain = !shortcut && !event.alt_key();
        let opening = match key.as_str() {
            " " | "F4" => plain,
            "ArrowDown" | "ArrowUp" => event.alt_key() && !shortcut,
            _ => false,
        };
        if opening {
            event.prevent_default();
            lists.open(select);
        }
        return;
    }
    if shortcut || !dropdown_of(event).is_some_and(|select| lists.showing(&select)) {
        lists.close();
        return;
    }
    match key.as_str() {
        "ArrowDown" | "ArrowUp" if event.alt_key() => lists.pick_active(),
        "ArrowDown" => lists.highlight(true, |select, at| step(select, at, true, 1)),
        "ArrowUp" => lists.highlight(true, |select, at| step(select, at, false, 1)),
        "PageDown" => lists.highlight(true, |select, at| step(select, at, true, PAGE)),
        "PageUp" => lists.highlight(true, |select, at| step(select, at, false, PAGE)),
        "Home" => lists.highlight(true, |select, _| end(select, false)),
        "End" => lists.highlight(true, |select, _| end(select, true)),
        "Enter" | " " => lists.pick_active(),
        "Escape" | "F4" => lists.close(),
        "Shift" | "Control" | "Alt" | "AltGraph" | "Meta" | "CapsLock" => return,
        letter if letter.chars().count() == 1 && !event.alt_key() => lists.find(letter),
        _ => {
            lists.close();
            return;
        }
    }
    event.prevent_default();
    event.stop_propagation();
}

/// The dropdown an event happened on.
fn dropdown_of(event: &Event) -> Option<HtmlSelectElement> {
    let element = event.target()?.dyn_into::<Element>().ok()?;
    element.closest("select").ok().flatten()?.dyn_into().ok()
}

fn in_list(event: &Event) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .is_some_and(|element| element.closest(".dropdown-list").ok().flatten().is_some())
}

/// A dropdown that opens a list (not one switched off, nor a list box).
fn opens(select: &HtmlSelectElement) -> bool {
    !select.disabled() && !select.multiple() && select.size() <= 1
}

fn option(select: &HtmlSelectElement, index: u32) -> Option<HtmlOptionElement> {
    select.options().item(index)?.dyn_into().ok()
}

/// Shown, and not switched off (itself or its group).
fn choosable(select: &HtmlSelectElement, index: u32) -> bool {
    option(select, index)
        .is_some_and(|option| !option.hidden() && !option.matches(":disabled").unwrap_or(true))
}

/// The option `count` choosable ones on from `from`, or the last one
/// there is that way.
fn step(select: &HtmlSelectElement, from: u32, forward: bool, count: usize) -> Option<u32> {
    let len = select.options().length();
    let mut on: Vec<u32> = if forward { (from + 1..len).collect() } else { (0..from).rev().collect() };
    on.retain(|&index| choosable(select, index));
    on.get(count - 1).or(on.last()).copied()
}

/// The first (or, `last`, the last) choosable option.
fn end(select: &HtmlSelectElement, last: bool) -> Option<u32> {
    let len = select.options().length();
    if last {
        (0..len).rev().find(|&index| choosable(select, index))
    } else {
        (0..len).find(|&index| choosable(select, index))
    }
}

/// The option the letters typed find: one letter again and again steps
/// through those starting with it; more find the first starting with
/// them, from the highlighted one on.
fn typed_option(select: &HtmlSelectElement, active: u32, typed: &str) -> Option<u32> {
    let len = select.options().length();
    let first: String = typed.chars().take(1).collect();
    let repeated = typed.chars().all(|letter| first.starts_with(letter));
    let (prefix, from) = if repeated { (first.as_str(), active + 1) } else { (typed, active) };
    (0..len).map(|n| (from + n) % len).find(|&index| {
        choosable(select, index)
            && option(select, index)
                .is_some_and(|option| option.label().trim().to_lowercase().starts_with(prefix))
    })
}

enum Row {
    /// An `<optgroup>`'s label.
    Group(String),
    Choice {
        index: u32,
        label: String,
        enabled: bool,
        grouped: bool,
    },
}

fn rows(select: &HtmlSelectElement) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut group: Option<Element> = None;
    for index in 0..select.options().length() {
        let Some(option) = option(select, index) else { continue };
        if option.hidden() {
            continue;
        }
        let parent =
            option.parent_element().filter(|parent| parent.tag_name().eq_ignore_ascii_case("optgroup"));
        if parent != group {
            if let Some(parent) = &parent {
                rows.push(Row::Group(parent.get_attribute("label").unwrap_or_default()));
            }
            group = parent;
        }
        rows.push(Row::Choice {
            index,
            label: option.label(),
            enabled: choosable(select, index),
            grouped: group.is_some(),
        });
    }
    rows
}

/// Where the list goes: under its dropdown, or above it when there's more
/// room there and it doesn't fit under; at least as wide; its text the
/// dropdown's size on screen (on the canvas, zoomed).
fn place(select: &HtmlSelectElement, rows: usize) -> String {
    let window = gloo::utils::window();
    let size = |value: Result<JsValue, JsValue>| value.ok().and_then(|value| value.as_f64()).unwrap_or(0.0);
    let (width, height) = (size(window.inner_width()), size(window.inner_height()));
    let rect = select.get_bounding_client_rect();
    let zoom = match select.offset_height() {
        0 => 1.0,
        own => rect.height() / f64::from(own),
    };
    let font = window
        .get_computed_style(select)
        .ok()
        .flatten()
        .and_then(|style| style.get_property_value("font-size").ok())
        .and_then(|size| size.trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or(13.0)
        * zoom;
    let natural = (rows as f64 * ROW_EM + 2.0 * PAD_EM) * font + 2.0;
    let below = height - rect.bottom() - GAP - EDGE;
    let above = rect.top() - GAP - EDGE;
    let (vertical, room) = if natural <= below || below >= above {
        (format!("top: {}px", rect.bottom() + GAP), below)
    } else {
        (format!("bottom: {}px", height - rect.top() + GAP), above)
    };
    let min_width = rect.width().min(width - 2.0 * EDGE).max(0.0);
    let left = rect.left().min(width - EDGE - min_width).max(EDGE);
    format!(
        "left: {left}px; {vertical}; min-width: {min_width}px; max-width: {}px; max-height: {}px; font-size: {font}px",
        (width - 2.0 * EDGE).max(0.0),
        room.max((ROW_EM + 2.0 * PAD_EM) * font + 2.0),
    )
}

/// Moves a list wider than the room right of its dropdown left until it
/// fits, and scrolls the highlighted option into view once a key moved it.
fn settle(list: &HtmlElement, open: &RefCell<Option<Open>>) {
    let width = gloo::utils::window().inner_width().ok().and_then(|width| width.as_f64()).unwrap_or(0.0);
    let rect = list.get_bounding_client_rect();
    let over = rect.right() - (width - EDGE);
    if over > 0.5 {
        let _ = list.style().set_property("left", &format!("{}px", (rect.left() - over).max(EDGE)));
    }
    let reveal = open.borrow_mut().as_mut().is_some_and(|open| std::mem::take(&mut open.reveal));
    if reveal && let Ok(Some(active)) = list.query_selector(".dropdown-option.active") {
        let nearest = ScrollIntoViewOptions::new();
        nearest.set_block(ScrollLogicalPosition::Nearest);
        active.scroll_into_view_with_scroll_into_view_options(&nearest);
    }
}
