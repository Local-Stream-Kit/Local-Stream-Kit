//! A position setting's control (2026-10-15): a small 16:9 screen with the
//! thing on it as a box and the nine dots of the old 3 × 3 grid. Drag the
//! box anywhere (near a dot it snaps to it), press anywhere to put it
//! there, or move it with the arrow keys (1 %, Shift: 10 %); the two
//! numbers say it exactly. A plugin can allow "Anywhere (random)". In a
//! node it doesn't fit, so [`PositionField`] is a field that opens it.

use std::rc::Rc;

use protocol::plugin::{Position, RANDOM_POSITION};
use serde_json::Value;
use web_sys::{Element, HtmlElement, KeyboardEvent, PointerEvent};
use yew::prelude::*;

use crate::editor::fields::NumberField;
use crate::util;

/// The room the box moves in, inside the screen: its margin in % of the
/// screen's width and height (the same pixels each way on 16:9).
const INSET_X: f64 = 3.5;
const INSET_Y: f64 = INSET_X * 16.0 / 9.0;
/// The box, in % of the screen.
const BOX_W: f64 = 30.0;
const BOX_H: f64 = 24.0;
/// A dot this close to the pointer catches it (px).
const SNAP_PX: f64 = 8.0;
/// The dots: 0, 50 and 100 % each way.
const GRID: [f64; 3] = [0.0, 50.0, 100.0];
/// The control in a node's popover, and its height (to fit it in the
/// window), in px.
const POPOVER_W: f64 = 262.0;
const POPOVER_H: f64 = 280.0;

/// What a stored value shows: a place, or anywhere (random).
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    At(Position),
    Random,
}

impl Shown {
    fn of(value: &Value, random: bool) -> Shown {
        if random && value.as_str() == Some(RANDOM_POSITION) {
            return Shown::Random;
        }
        Shown::At(Position::from_value(value).unwrap_or_default())
    }

    /// Where the box is drawn while random: the middle.
    fn position(self) -> Position {
        match self {
            Shown::At(position) => position,
            Shown::Random => Position { x: 50.0, y: 50.0 },
        }
    }

    fn label(self) -> String {
        match self {
            Shown::At(position) => position.label(),
            Shown::Random => "Anywhere".into(),
        }
    }
}

/// A point of the room (the box's matching point) in % of the screen.
fn point(position: Position) -> (f64, f64) {
    (
        INSET_X + position.x / 100.0 * (100.0 - 2.0 * INSET_X),
        INSET_Y + position.y / 100.0 * (100.0 - 2.0 * INSET_Y),
    )
}

/// The box's top left corner in % of the screen: 0 keeps it against the
/// room's left (top) edge, 100 against its right (bottom).
fn corner(position: Position) -> (f64, f64) {
    (
        INSET_X + position.x / 100.0 * (100.0 - 2.0 * INSET_X - BOX_W),
        INSET_Y + position.y / 100.0 * (100.0 - 2.0 * INSET_Y - BOX_H),
    )
}

/// A screen's place in the window: left, top, width, height.
type Rect = (f64, f64, f64, f64);

/// A point of the room in the window, on a screen at `rect`.
fn on_screen(rect: Rect, position: Position) -> (f64, f64) {
    let (x, y) = point(position);
    (rect.0 + rect.2 * x / 100.0, rect.1 + rect.3 * y / 100.0)
}

/// The dot within reach of the pointer, if one is.
fn near_dot(rect: Rect, pointer: (f64, f64)) -> Option<Position> {
    GRID.iter().flat_map(|y| GRID.iter().map(move |x| Position { x: *x, y: *y })).find(|dot| {
        let at = on_screen(rect, *dot);
        (at.0 - pointer.0).hypot(at.1 - pointer.1) <= SNAP_PX
    })
}

/// Where the pointer puts the box, on a screen at `rect`: the dot within
/// reach, else the point under it in whole %.
fn at_pointer(rect: Rect, pointer: (f64, f64)) -> Position {
    if let Some(dot) = near_dot(rect, pointer) {
        return dot;
    }
    let (left, top, width, height) = rect;
    let part = |at: f64, start: f64, size: f64, inset: f64| {
        if size <= 0.0 {
            return 0.0;
        }
        let percent = (at - start) / size * 100.0;
        ((percent - inset) / (100.0 - 2.0 * inset) * 100.0).clamp(0.0, 100.0).round()
    };
    Position { x: part(pointer.0, left, width, INSET_X), y: part(pointer.1, top, height, INSET_Y) }
}

/// Moved by an arrow key, 1 % (10 % with Shift), within 0–100.
fn nudged(position: Position, key: &str, shift: bool) -> Option<Position> {
    let step = if shift { 10.0 } else { 1.0 };
    let (dx, dy) = match key {
        "ArrowLeft" => (-step, 0.0),
        "ArrowRight" => (step, 0.0),
        "ArrowUp" => (0.0, -step),
        "ArrowDown" => (0.0, step),
        _ => return None,
    };
    let x = (position.x + dx).round().clamp(0.0, 100.0);
    let y = (position.y + dy).round().clamp(0.0, 100.0);
    Some(Position { x, y })
}

#[derive(Properties, PartialEq)]
pub struct PositionPickProps {
    /// The screen's id; the numbers' are `{id}-x` and `{id}-y`.
    pub id: AttrValue,
    pub label: AttrValue,
    pub value: Value,
    /// "Anywhere (random)" is a choice.
    #[prop_or_default]
    pub random: bool,
    /// The numbers under the screen, whatever the width (a node's popover).
    #[prop_or_default]
    pub stacked: bool,
    pub onchange: Callback<Value>,
}

/// The control: the screen, the two numbers beside it (under it in the
/// dock), and "Anywhere (random)" if the setting allows it.
#[function_component]
pub fn PositionPick(props: &PositionPickProps) -> Html {
    let screen = use_node_ref();
    // While dragging: the pointer's distance from the box's point, so the
    // box doesn't jump to where it was grabbed.
    let grab = use_mut_ref(|| None::<(f64, f64)>);
    let dragging = use_state(|| false);
    let shown = Shown::of(&props.value, props.random);
    let position = shown.position();
    // Sends a place, once per change (a drag moves through many).
    let last = use_mut_ref(|| None::<Value>);
    let send: Rc<dyn Fn(Position)> = {
        let (onchange, last, current) = (props.onchange.clone(), last.clone(), props.value.clone());
        Rc::new(move |position: Position| {
            let value = position.to_value();
            let mut last = last.borrow_mut();
            if last.as_ref() == Some(&value) || (last.is_none() && value == current) {
                return;
            }
            *last = Some(value.clone());
            onchange.emit(value);
        })
    };
    let place = {
        let (screen, grab, send) = (screen.clone(), grab.clone(), send.clone());
        move |event: &PointerEvent| {
            let Some(element) = screen.cast::<Element>() else { return };
            let rect = element.get_bounding_client_rect();
            let offset = grab.borrow().unwrap_or_default();
            let pointer = (event.client_x() - offset.0, event.client_y() - offset.1);
            send(at_pointer((rect.left(), rect.top(), rect.width(), rect.height()), pointer));
        }
    };
    let onpointerdown = {
        let (screen, grab, dragging, last, place) =
            (screen.clone(), grab.clone(), dragging.clone(), last.clone(), place.clone());
        Callback::from(move |event: PointerEvent| {
            if event.button() != 0 {
                return;
            }
            event.prevent_default();
            event.stop_propagation();
            let Some(element) = screen.cast::<HtmlElement>() else { return };
            let _ = element.focus();
            let _ = element.set_pointer_capture(event.pointer_id());
            let rect = element.get_bounding_client_rect();
            let rect = (rect.left(), rect.top(), rect.width(), rect.height());
            let pointer = (event.client_x(), event.client_y());
            // Grabbed by the box: it keeps where it was held. A dot (even
            // one under the box), or anywhere else: the box goes there.
            let on_box = near_dot(rect, pointer).is_none()
                && event
                    .target()
                    .and_then(|target| wasm_bindgen::JsCast::dyn_into::<Element>(target).ok())
                    .and_then(|target| target.closest(".position-box").ok().flatten())
                    .is_some();
            let offset = if on_box {
                let anchor = on_screen(rect, position);
                (pointer.0 - anchor.0, pointer.1 - anchor.1)
            } else {
                (0.0, 0.0)
            };
            *grab.borrow_mut() = Some(offset);
            *last.borrow_mut() = None;
            dragging.set(true);
            if !on_box {
                place(&event);
            }
        })
    };
    let onpointermove = {
        let (grab, place) = (grab.clone(), place.clone());
        Callback::from(move |event: PointerEvent| {
            if grab.borrow().is_some() {
                place(&event);
            }
        })
    };
    let stop = {
        let (grab, dragging) = (grab.clone(), dragging.clone());
        Callback::from(move |_: PointerEvent| {
            *grab.borrow_mut() = None;
            dragging.set(false);
        })
    };
    let onkeydown = {
        let (send, last) = (send.clone(), last.clone());
        Callback::from(move |event: KeyboardEvent| {
            if let Some(moved) = nudged(position, &event.key(), event.shift_key()) {
                event.prevent_default();
                *last.borrow_mut() = None;
                send(moved);
            }
        })
    };
    let number = |which: char| {
        let send = send.clone();
        let last = last.clone();
        Callback::from(move |number: f64| {
            let number = number.clamp(0.0, 100.0);
            let moved = if which == 'x' {
                Position { x: number, ..position }
            } else {
                Position { y: number, ..position }
            };
            *last.borrow_mut() = None;
            send(moved);
        })
    };
    let on_random = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: Event| {
            let value = if util::checked_of(&event) {
                Value::String(RANDOM_POSITION.into())
            } else {
                Shown::Random.position().to_value()
            };
            onchange.emit(value);
        })
    };

    let random = shown == Shown::Random;
    let id = props.id.clone();
    let style = |(left, top): (f64, f64)| format!("left: {left:.2}%; top: {top:.2}%");
    let dots = GRID.iter().flat_map(|y| GRID.iter().map(move |x| Position { x: *x, y: *y }));
    html! {
        <div class={classes!("position-pick", props.stacked.then_some("stacked"))}>
            <div ref={screen} id={id.clone()} role="slider" tabindex="0" aria-label={props.label.clone()}
                 aria-valuetext={shown.label()} title="Drag it, or press where it goes. Arrow keys: 1 % (Shift: 10 %)"
                 class={classes!("position-screen", random.then_some("random"), (*dragging).then_some("dragging"))}
                 {onpointerdown} {onpointermove} onpointerup={stop.clone()} onpointercancel={stop} {onkeydown}>
                { for dots.map(|dot| {
                    let on = !random && dot == position;
                    html! { <span class={classes!("position-dot", on.then_some("on"))} style={style(point(dot))}></span> }
                }) }
                if random {
                    // Somewhere else each time.
                    { for [(15.0, 20.0), (70.0, 35.0), (35.0, 80.0)].into_iter().map(|(x, y)| html! {
                        <span class="position-ghost" style={style(corner(Position { x, y }))}></span>
                    }) }
                } else {
                    <span class="position-box" style={style(corner(position))}></span>
                    if position.name().is_none() {
                        <span class="position-anchor" style={style(point(position))}></span>
                    }
                }
            </div>
            <div class="position-side">
                <fieldset class="position-numbers" disabled={random}>
                    <label for={format!("{id}-x")}>{ "From left" }</label>
                    <span class="position-number">
                        <NumberField id={format!("{id}-x")} value={position.x} onchange={number('x')} />{ "%" }
                    </span>
                    <label for={format!("{id}-y")}>{ "From top" }</label>
                    <span class="position-number">
                        <NumberField id={format!("{id}-y")} value={position.y} onchange={number('y')} />{ "%" }
                    </span>
                </fieldset>
                if props.random {
                    <label class="position-random">
                        <input type="checkbox" id={format!("{id}-random")} checked={random} onchange={on_random} />
                        { "Anywhere (random)" }
                    </label>
                }
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct PositionFieldProps {
    pub id: AttrValue,
    pub label: AttrValue,
    pub value: Value,
    #[prop_or_default]
    pub random: bool,
    pub onchange: Callback<Value>,
}

/// In a node: a field like the dropdowns (a little screen, then "Top" or
/// "12 %, 85 %") that opens the control under it, over everything (in
/// `<body>`, outside the zoomed canvas). A press outside, the wheel or
/// Escape closes it.
#[function_component]
pub fn PositionField(props: &PositionFieldProps) -> Html {
    // Where the control opens, in the window.
    let open = use_state(|| None::<(f64, f64)>);
    let button = use_node_ref();
    let shown = Shown::of(&props.value, props.random);
    {
        // The screen takes the focus, for the arrow keys.
        let id = format!("{}-pick", props.id);
        use_effect_with(open.is_some(), move |opened| {
            if *opened && let Some(screen) = gloo::utils::document().get_element_by_id(&id) {
                let _ = wasm_bindgen::JsCast::dyn_into::<HtmlElement>(screen).map(|screen| screen.focus());
            }
        });
    }
    let onclick = {
        let (open, button) = (open.clone(), button.clone());
        Callback::from(move |_: MouseEvent| {
            if open.is_some() {
                open.set(None);
                return;
            }
            let Some(element) = button.cast::<Element>() else { return };
            let rect = element.get_bounding_client_rect();
            let window = gloo::utils::window();
            let size = |value: Result<wasm_bindgen::JsValue, _>| {
                value.ok().and_then(|value| value.as_f64()).unwrap_or(f64::MAX)
            };
            let (width, height) = (size(window.inner_width()), size(window.inner_height()));
            let left = rect.left().min(width - POPOVER_W - 8.0).max(8.0);
            let below = rect.bottom() + 4.0;
            let top =
                if below + POPOVER_H > height { (rect.top() - 4.0 - POPOVER_H).max(8.0) } else { below };
            open.set(Some((left, top)));
        })
    };
    let close = {
        let (open, button) = (open.clone(), button.clone());
        Callback::from(move |()| {
            open.set(None);
            if let Some(button) = button.cast::<HtmlElement>() {
                let _ = button.focus();
            }
        })
    };
    // Events in the portal reach the node and the canvas through the
    // components (as Yew bubbles them): kept from them.
    let on_backdrop = close.reform(|event: PointerEvent| {
        event.prevent_default();
        event.stop_propagation();
    });
    let on_wheel = close.reform(|event: WheelEvent| event.stop_propagation());
    let no_menu = Callback::from(|event: MouseEvent| {
        event.prevent_default();
        event.stop_propagation();
    });
    let onkeydown = {
        let close = close.clone();
        Callback::from(move |event: KeyboardEvent| {
            event.stop_propagation();
            if event.key() == "Escape" {
                event.prevent_default();
                close.emit(());
            }
        })
    };
    let (mini_x, mini_y) = corner(shown.position());
    html! {
        <>
            <button type="button" ref={button} id={props.id.clone()} class="position-field" aria-haspopup="dialog"
                    aria-expanded={open.is_some().to_string()} {onclick}>
                <span class={classes!("position-mini", (shown == Shown::Random).then_some("random"))} aria-hidden="true">
                    <span style={format!("left: {mini_x:.1}%; top: {mini_y:.1}%")}></span>
                </span>
                <span class="position-field-text">{ shown.label() }</span>
            </button>
            if let Some((left, top)) = *open {
                { create_portal(
                    html! {
                        <div class="position-backdrop" onpointerdown={on_backdrop} onwheel={on_wheel} oncontextmenu={no_menu}>
                            <div class="position-popover" role="dialog" aria-label={props.label.clone()}
                                 style={format!("left: {left}px; top: {top}px")} {onkeydown}
                                 onpointerdown={Callback::from(|event: PointerEvent| event.stop_propagation())}
                                 onwheel={Callback::from(|event: WheelEvent| event.stop_propagation())}>
                                <PositionPick id={format!("{}-pick", props.id)} label={props.label.clone()}
                                              value={props.value.clone()} random={props.random} stacked=true
                                              onchange={props.onchange.clone()} />
                            </div>
                        </div>
                    },
                    gloo::utils::body().into(),
                ) }
            }
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 240 × 135 screen at (100, 50).
    const RECT: Rect = (100.0, 50.0, 240.0, 135.0);

    #[test]
    fn the_pointer_places_it_and_the_dots_catch_it() {
        // Near the bottom left dot: on it.
        let dot = on_screen(RECT, Position { x: 0.0, y: 100.0 });
        assert_eq!(at_pointer(RECT, (dot.0 + 5.0, dot.1 - 4.0)), Position { x: 0.0, y: 100.0 });
        assert_eq!(near_dot(RECT, (dot.0 + 9.0, dot.1)), None, "out of reach");
        // Between the dots: the point under the pointer, in whole %.
        let spot = on_screen(RECT, Position { x: 12.0, y: 85.0 });
        assert_eq!(at_pointer(RECT, spot), Position { x: 12.0, y: 85.0 });
        // Off the screen: against its edge.
        assert_eq!(at_pointer(RECT, (0.0, 300.0)), Position { x: 0.0, y: 100.0 });
    }

    #[test]
    fn the_box_stays_inside_and_its_point_is_on_the_place() {
        for position in
            [Position { x: 0.0, y: 0.0 }, Position { x: 100.0, y: 100.0 }, Position { x: 12.0, y: 85.0 }]
        {
            let (left, top) = corner(position);
            assert!(left >= INSET_X - 1e-9 && left + BOX_W <= 100.0 - INSET_X + 1e-9, "{position:?}");
            assert!(top >= INSET_Y - 1e-9 && top + BOX_H <= 100.0 - INSET_Y + 1e-9, "{position:?}");
            // The box's point that far across it is on the room's point.
            let (x, y) = point(position);
            assert!((left + BOX_W * position.x / 100.0 - x).abs() < 1e-9);
            assert!((top + BOX_H * position.y / 100.0 - y).abs() < 1e-9);
        }
    }

    #[test]
    fn the_arrow_keys_move_it() {
        let at = Position { x: 50.0, y: 0.0 };
        assert_eq!(nudged(at, "ArrowRight", false), Some(Position { x: 51.0, y: 0.0 }));
        assert_eq!(nudged(at, "ArrowDown", true), Some(Position { x: 50.0, y: 10.0 }));
        assert_eq!(nudged(at, "ArrowUp", false), Some(at), "not past the edge");
        assert_eq!(nudged(at, "Enter", false), None);
    }
}
