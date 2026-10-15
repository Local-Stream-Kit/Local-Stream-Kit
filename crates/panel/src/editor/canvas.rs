//! The canvas: one card's nodes and wires on a surface that pans (drag the
//! background, or the middle button) and zooms (the wheel). Nodes move by
//! their header; wires are dragged from socket to socket.
//!
//! Gestures are only committed when they end: moving nodes is one edit (one
//! undo step) on release, and a wire picked up from an input only goes when
//! it's dropped somewhere else.

use std::collections::BTreeSet;
use std::rc::Rc;

use gloo::events::{EventListener, EventListenerOptions};
use protocol::cards::{Card, Node, NodeId, NodeKind, SocketRef, SocketType, Wire};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{ClipboardEvent, Element, HtmlElement, KeyboardEvent, PointerEvent, WheelEvent};
use yew::prelude::*;

use super::edit_card;
use super::layout;
use super::menu::{AddMenu, WireFilter};
use super::node::{self, NodeContext};
use crate::Panel;
use crate::util;

const MIN_ZOOM: f64 = 0.25;
const MAX_ZOOM: f64 = 2.0;
/// Background dot spacing at 100 %.
const GRID: f64 = 22.0;
/// Pointer travel (screen pixels) below which a press is a click.
const CLICK_SLOP: f64 = 4.0;
/// Two clicks this close (ms) are a double click.
const DOUBLE_CLICK_MS: f64 = 500.0;
/// Room left for the toolbar above the nodes when fitting.
const FIT_TOP: f64 = 70.0;
const FIT_PAD: f64 = 50.0;

/// Where the canvas looks: world point (0, 0) is at screen `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    x: f64,
    y: f64,
    zoom: f64,
}

impl Default for View {
    fn default() -> Self {
        View { x: 40.0, y: FIT_TOP, zoom: 1.0 }
    }
}

impl View {
    fn to_world(self, (x, y): (f64, f64)) -> (f64, f64) {
        ((x - self.x) / self.zoom, (y - self.y) / self.zoom)
    }

    /// Zooms keeping the world point under `at` (screen) in place.
    fn zoom_at(self, (x, y): (f64, f64), zoom: f64) -> View {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let scale = zoom / self.zoom;
        View { x: x - (x - self.x) * scale, y: y - (y - self.y) * scale, zoom }
    }

    /// Shows the whole card, at most at 100 %.
    fn fit(card: &Card, (width, height): (f64, f64)) -> View {
        let Some((left, top, right, bottom)) = layout::bounds(card) else { return View::default() };
        let (w, h) = ((right - left).max(1.0), (bottom - top).max(1.0));
        let zoom = ((width - 2.0 * FIT_PAD) / w).min((height - FIT_TOP - FIT_PAD) / h).clamp(MIN_ZOOM, 1.0);
        View {
            x: ((width - w * zoom) / 2.0 - left * zoom).round(),
            y: (FIT_TOP + ((height - FIT_TOP - FIT_PAD) - h * zoom).max(0.0) / 2.0 - top * zoom).round(),
            zoom,
        }
    }
}

pub enum ViewAction {
    Set(View),
    /// Multiply the zoom, around a screen point.
    ZoomBy((f64, f64), f64),
}

impl Reducible for View {
    type Action = ViewAction;

    fn reduce(self: Rc<Self>, action: ViewAction) -> Rc<Self> {
        Rc::new(match action {
            ViewAction::Set(view) => view,
            ViewAction::ZoomBy(at, factor) => self.zoom_at(at, self.zoom * factor),
        })
    }
}

/// What the pointer is doing.
#[derive(Debug, Clone, PartialEq)]
enum Drag {
    None,
    Pan {
        start: (f64, f64),
        view: View,
        /// Pressed with the left button (a click on empty canvas deselects).
        left: bool,
    },
    Nodes {
        /// World position where it started.
        start: (f64, f64),
        delta: (f64, f64),
        clicked: NodeId,
        /// The grabbed node's position when it started.
        origin: (f64, f64),
        moved: bool,
    },
    Wire {
        /// The end that stays put.
        anchor: SocketRef,
        anchor_is_input: bool,
        /// The anchor's type.
        ty: SocketType,
        /// The loose end, in world pixels.
        at: (f64, f64),
        /// A wire picked up from its input: it goes unless dropped back.
        detached: Option<Wire>,
    },
    Select {
        start: (f64, f64),
        at: (f64, f64),
    },
    /// A Text node's corner: its width and text box height.
    Resize {
        node: NodeId,
        /// World position where it started.
        start: (f64, f64),
        from: (f64, f64),
        size: (f64, f64),
        /// Past the click slop: a resize, not a click.
        moved: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct Menu {
    /// Screen position, in the canvas.
    screen: (f64, f64),
    /// Where the new node goes.
    world: (f64, f64),
    wire: Option<PendingWire>,
}

#[derive(Debug, Clone, PartialEq)]
struct PendingWire {
    anchor: SocketRef,
    anchor_is_input: bool,
    ty: SocketType,
}

/// Copied nodes, as they go on the clipboard.
#[derive(Serialize, Deserialize)]
struct Snippet {
    lsk: String,
    nodes: Vec<Node>,
    wires: Vec<Wire>,
}

const SNIPPET_TAG: &str = "nodes";

impl Snippet {
    fn of(card: &Card, selection: &BTreeSet<NodeId>) -> Option<Snippet> {
        let nodes: Vec<Node> =
            card.nodes.iter().filter(|node| selection.contains(&node.id)).cloned().collect();
        let wires = card
            .wires
            .iter()
            .filter(|wire| selection.contains(&wire.from.node) && selection.contains(&wire.to.node))
            .cloned()
            .collect();
        (!nodes.is_empty()).then(|| Snippet { lsk: SNIPPET_TAG.into(), nodes, wires })
    }

    fn parse(text: &str) -> Option<Snippet> {
        serde_json::from_str::<Snippet>(text).ok().filter(|snippet| snippet.lsk == SNIPPET_TAG)
    }

    /// The nodes with fresh ids for `card`, their top-left corner at `at`
    /// (or next to where they were).
    fn place(&self, card: &Card, at: Option<(f64, f64)>) -> (Vec<Node>, Vec<Wire>) {
        let left = self.nodes.iter().map(|node| node.x).fold(f64::INFINITY, f64::min);
        let top = self.nodes.iter().map(|node| node.y).fold(f64::INFINITY, f64::min);
        let (dx, dy) = match at {
            Some((x, y)) => ((x - left).round(), (y - top).round()),
            None => (30.0, 30.0),
        };
        let first = card.next_node_id();
        let new_id = |old: NodeId| {
            self.nodes.iter().position(|node| node.id == old).map(|index| first + index as NodeId)
        };
        let nodes = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| Node {
                id: first + index as NodeId,
                x: node.x + dx,
                y: node.y + dy,
                kind: node.kind.clone(),
            })
            .collect();
        let wires = self
            .wires
            .iter()
            .filter_map(|wire| {
                Some(Wire {
                    from: SocketRef::new(new_id(wire.from.node)?, &wire.from.socket),
                    to: SocketRef::new(new_id(wire.to.node)?, &wire.to.socket),
                })
            })
            .collect();
        (nodes, wires)
    }
}

/// A wire from `from` (an output) to `to` (an input), if they fit.
fn wire_between(card: &Card, from: SocketRef, to: SocketRef) -> Option<Wire> {
    let output = card.node(from.node)?.kind.output(&from.socket)?;
    let input = card.node(to.node)?.kind.input(&to.socket)?;
    (from.node != to.node && input.ty.accepts(output.ty)).then_some(Wire { from, to })
}

/// Adds a wire, replacing whatever went into its input.
fn connect(card: &mut Card, wire: &Wire) {
    card.wires.retain(|existing| existing.to != wire.to);
    card.wires.push(wire.clone());
}

/// For a wire dropped on a node rather than a socket: the socket of `node`
/// it fits, preferring a free one.
fn socket_on(card: &Card, node: &Node, ty: SocketType, anchor_is_input: bool) -> Option<SocketRef> {
    if anchor_is_input {
        let output = node.kind.outputs().into_iter().find(|output| ty.accepts(output.ty))?;
        Some(SocketRef::new(node.id, &output.name))
    } else {
        let fitting: Vec<_> = node.kind.inputs().into_iter().filter(|input| input.ty.accepts(ty)).collect();
        let free = fitting.iter().find(|input| card.wire_into(node.id, &input.name).is_none());
        free.or(fitting.first()).map(|input| SocketRef::new(node.id, &input.name))
    }
}

fn blur_focus() {
    if let Some(element) =
        gloo::utils::document().active_element().and_then(|e| e.dyn_into::<HtmlElement>().ok())
    {
        let _ = element.blur();
    }
}

/// Calls the latest `handler` for `event` on the document. (Listeners
/// added once would otherwise see the state of the first render.)
#[hook]
fn use_document_listener<F>(event: &'static str, handler: F)
where
    F: Fn(&Event) + 'static,
{
    let latest = use_mut_ref(|| None::<Rc<dyn Fn(&Event)>>);
    *latest.borrow_mut() = Some(Rc::new(handler));
    use_effect_with(event, move |event| {
        let listener = EventListener::new(&gloo::utils::document(), *event, move |event| {
            let handler = latest.borrow().clone();
            if let Some(handler) = handler {
                handler(event);
            }
        });
        move || drop(listener)
    });
}

#[derive(Properties, PartialEq)]
pub struct CanvasProps {
    pub card: Card,
}

#[function_component]
pub fn Canvas(props: &CanvasProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let surface = use_node_ref();
    let view = use_reducer(View::default);
    let drag = use_mut_ref(|| Drag::None);
    let redraw = use_force_update();
    let selection = use_state(BTreeSet::<NodeId>::new);
    let menu = use_state(|| None::<Menu>);
    // The pointer's last position on the canvas (screen pixels).
    let pointer = use_mut_ref(|| None::<(f64, f64)>);
    // The last click on a Text node's corner, and when: a second one soon
    // after is a double click. (The browser's own dblclick goes to the
    // canvas, which holds the pointer while the corner is pressed.)
    let corner_click = use_mut_ref(|| None::<(NodeId, f64)>);

    let card = &props.card;
    let card_id = card.id.clone();

    // A new card: show all of it, nothing selected.
    {
        let surface = surface.clone();
        let view = view.clone();
        let selection = selection.clone();
        let menu = menu.clone();
        let card = card.clone();
        let drag = drag.clone();
        use_effect_with(card_id.clone(), move |_| {
            view.dispatch(ViewAction::Set(View::fit(&card, size(&surface))));
            selection.set(BTreeSet::new());
            menu.set(None);
            *drag.borrow_mut() = Drag::None;
        });
    }

    // The wheel zooms, except over something in it that scrolls (a long
    // text); the listener has to be able to prevent scrolling.
    {
        let surface = surface.clone();
        let view = view.clone();
        use_effect_with((), move |_| {
            let element = surface.cast::<Element>().expect("the canvas is mounted");
            let target = element.clone();
            let listener = EventListener::new_with_options(
                &element,
                "wheel",
                EventListenerOptions::enable_prevent_default(),
                move |event| {
                    let Some(event) = event.dyn_ref::<WheelEvent>() else { return };
                    if scrolls_inside(event, &target) {
                        return;
                    }
                    event.prevent_default();
                    let mut delta = event.delta_y();
                    match event.delta_mode() {
                        WheelEvent::DOM_DELTA_LINE => delta *= 16.0,
                        WheelEvent::DOM_DELTA_PAGE => delta *= 400.0,
                        _ => {}
                    }
                    let rect = target.get_bounding_client_rect();
                    let at = (event.client_x() - rect.left(), event.client_y() - rect.top());
                    view.dispatch(ViewAction::ZoomBy(at, (-delta * 0.0015).exp()));
                },
            );
            move || drop(listener)
        });
    }

    let local = {
        let surface = surface.clone();
        move |event: &MouseEvent| -> (f64, f64) {
            let (x, y) = (event.client_x(), event.client_y());
            match surface.cast::<Element>() {
                Some(element) => {
                    let rect = element.get_bounding_client_rect();
                    (x - rect.left(), y - rect.top())
                }
                None => (x, y),
            }
        }
    };
    let capture = {
        let surface = surface.clone();
        move |event: &PointerEvent| {
            if let Some(element) = surface.cast::<Element>() {
                let _ = element.set_pointer_capture(event.pointer_id());
            }
        }
    };

    // Shared edits.
    let delete_selected = {
        let panel = panel.clone();
        let card_id = card_id.clone();
        let selection = selection.clone();
        move || {
            let gone = (*selection).clone();
            if gone.is_empty() {
                return;
            }
            edit_card(&panel, &card_id, None, move |card| {
                card.nodes.retain(|node| !gone.contains(&node.id));
                card.wires.retain(|wire| !gone.contains(&wire.from.node) && !gone.contains(&wire.to.node));
            });
            selection.set(BTreeSet::new());
        }
    };
    let paste = {
        let panel = panel.clone();
        let card = card.clone();
        let selection = selection.clone();
        move |snippet: &Snippet, at: Option<(f64, f64)>| {
            let (nodes, wires) = snippet.place(&card, at);
            selection.set(nodes.iter().map(|node| node.id).collect());
            edit_card(&panel, &card.id, None, move |card| {
                card.nodes.extend(nodes.iter().cloned());
                card.wires.extend(wires.iter().cloned());
            });
        }
    };
    let open_menu = {
        let menu = menu.clone();
        let surface = surface.clone();
        let view = *view;
        move |screen: (f64, f64), wire: Option<PendingWire>| {
            let (width, height) = size(&surface);
            // The Add node menu is 300 px wide and up to 490 px tall.
            let shown = ((screen.0).min(width - 308.0).max(8.0), (screen.1).min(height - 498.0).max(8.0));
            menu.set(Some(Menu { screen: shown, world: view.to_world(screen), wire }));
        }
    };
    let pointer_world = {
        let pointer = pointer.clone();
        let view = *view;
        move || pointer.borrow().map(|at| view.to_world(at))
    };

    // Keyboard shortcuts (Ctrl+Z / Ctrl+Y are the app's).
    {
        let card = card.clone();
        let selection = selection.clone();
        let menu = menu.clone();
        let drag = drag.clone();
        let redraw = redraw.clone();
        let view = view.clone();
        let surface = surface.clone();
        let pointer = pointer.clone();
        let delete_selected = delete_selected.clone();
        let paste = paste.clone();
        let open_menu = open_menu.clone();
        use_document_listener("keydown", move |event| {
            let Some(event) = event.dyn_ref::<KeyboardEvent>() else { return };
            if util::typing_in_field(event) {
                return;
            }
            let control = event.ctrl_key() || event.meta_key();
            match event.key().as_str() {
                "Escape" => {
                    if menu.is_some() {
                        menu.set(None);
                    } else if *drag.borrow() != Drag::None {
                        *drag.borrow_mut() = Drag::None;
                        redraw.force_update();
                    } else {
                        selection.set(BTreeSet::new());
                    }
                }
                // Only Delete: Backspace is too easy to hit by accident.
                "Delete" => delete_selected(),
                "A" | "a" if event.shift_key() && !control => {
                    let at = pointer.borrow().unwrap_or_else(|| {
                        let (width, height) = size(&surface);
                        (width / 2.0, height / 3.0)
                    });
                    open_menu(at, None);
                }
                "a" | "A" if control => selection.set(card.nodes.iter().map(|node| node.id).collect()),
                "d" | "D" if control => {
                    if let Some(snippet) = Snippet::of(&card, &selection) {
                        paste(&snippet, None);
                    }
                }
                "Home" => view.dispatch(ViewAction::Set(View::fit(&card, size(&surface)))),
                _ => return,
            }
            event.prevent_default();
        });
    }

    // Copy, cut and paste nodes, through the system clipboard so it works
    // between the dock and a browser tab too.
    {
        let (copy_card, copy_selection) = (card.clone(), selection.clone());
        use_document_listener("copy", move |event| copy(event, &copy_card, &copy_selection, || {}));
        let (cut_card, cut_selection) = (card.clone(), selection.clone());
        let delete_selected = delete_selected.clone();
        use_document_listener("cut", move |event| copy(event, &cut_card, &cut_selection, &delete_selected));
        let paste = paste.clone();
        let pointer_world = pointer_world.clone();
        use_document_listener("paste", move |event| {
            if util::typing_in_field(event) {
                return;
            }
            let Some(event) = event.dyn_ref::<ClipboardEvent>() else { return };
            let text = event.clipboard_data().and_then(|data| data.get_data("text/plain").ok());
            if let Some(snippet) = text.as_deref().and_then(Snippet::parse) {
                event.prevent_default();
                paste(&snippet, pointer_world());
            }
        });
    }

    // Pointer handling.
    let on_surface_down = {
        let drag = drag.clone();
        let redraw = redraw.clone();
        let menu = menu.clone();
        let local = local.clone();
        let capture = capture.clone();
        let view = *view;
        Callback::from(move |event: PointerEvent| {
            let button = event.button();
            if button != 0 && button != 1 {
                return;
            }
            blur_focus();
            menu.set(None);
            capture(&event);
            let at = local(&event);
            *drag.borrow_mut() = if button == 0 && (event.shift_key() || event.ctrl_key()) {
                let world = view.to_world(at);
                Drag::Select { start: world, at: world }
            } else {
                Drag::Pan { start: at, view, left: button == 0 }
            };
            event.prevent_default();
            redraw.force_update();
        })
    };
    let on_head = {
        let drag = drag.clone();
        let redraw = redraw.clone();
        let menu = menu.clone();
        let selection = selection.clone();
        let local = local.clone();
        let capture = capture.clone();
        let card = card.clone();
        let view = *view;
        Callback::from(move |(id, event): (NodeId, PointerEvent)| {
            if event.button() != 0 {
                return;
            }
            event.stop_propagation();
            event.prevent_default();
            blur_focus();
            menu.set(None);
            let mut selected = (*selection).clone();
            if event.shift_key() || event.ctrl_key() {
                if !selected.insert(id) {
                    selected.remove(&id);
                    selection.set(selected);
                    return;
                }
            } else if !selected.contains(&id) {
                selected = BTreeSet::from([id]);
            }
            selection.set(selected);
            capture(&event);
            let start = view.to_world(local(&event));
            let origin = card.node(id).map_or((0.0, 0.0), |node| (node.x, node.y));
            *drag.borrow_mut() = Drag::Nodes { start, delta: (0.0, 0.0), clicked: id, origin, moved: false };
            redraw.force_update();
        })
    };
    let on_body = {
        let menu = menu.clone();
        let selection = selection.clone();
        Callback::from(move |(id, event): (NodeId, PointerEvent)| {
            if event.button() != 0 {
                return;
            }
            event.stop_propagation();
            menu.set(None);
            if event.shift_key() || event.ctrl_key() {
                let mut selected = (*selection).clone();
                selected.insert(id);
                selection.set(selected);
            } else if !selection.contains(&id) {
                selection.set(BTreeSet::from([id]));
            }
        })
    };
    let on_socket = {
        let drag = drag.clone();
        let redraw = redraw.clone();
        let menu = menu.clone();
        let local = local.clone();
        let capture = capture.clone();
        let card = card.clone();
        let view = *view;
        Callback::from(move |(id, socket, input, event): (NodeId, String, bool, PointerEvent)| {
            if event.button() != 0 {
                return;
            }
            event.stop_propagation();
            event.prevent_default();
            blur_focus();
            menu.set(None);
            let Some(node) = card.node(id) else { return };
            let at = view.to_world(local(&event));
            let picked_up = input.then(|| card.wire_into(id, &socket).cloned()).flatten();
            let started = match picked_up {
                // Pick the wire up by its input end.
                Some(wire) => card
                    .node(wire.from.node)
                    .and_then(|from| from.kind.output(&wire.from.socket))
                    .map(|output| Drag::Wire {
                        anchor: wire.from.clone(),
                        anchor_is_input: false,
                        ty: output.ty,
                        at,
                        detached: Some(wire.clone()),
                    }),
                None => {
                    let found = if input { node.kind.input(&socket) } else { node.kind.output(&socket) };
                    found.map(|found| Drag::Wire {
                        anchor: SocketRef::new(id, &socket),
                        anchor_is_input: input,
                        ty: found.ty,
                        at,
                        detached: None,
                    })
                }
            };
            if let Some(started) = started {
                capture(&event);
                *drag.borrow_mut() = started;
                redraw.force_update();
            }
        })
    };
    let on_resize = {
        let drag = drag.clone();
        let redraw = redraw.clone();
        let menu = menu.clone();
        let local = local.clone();
        let capture = capture.clone();
        let card = card.clone();
        let view = *view;
        Callback::from(move |(id, event): (NodeId, PointerEvent)| {
            if event.button() != 0 {
                return;
            }
            let Some(NodeKind::Text { size, .. }) = card.node(id).map(|node| &node.kind) else { return };
            event.stop_propagation();
            event.prevent_default();
            blur_focus();
            menu.set(None);
            capture(&event);
            let from = layout::text_size(*size);
            let start = view.to_world(local(&event));
            *drag.borrow_mut() = Drag::Resize { node: id, start, from, size: from, moved: false };
            redraw.force_update();
        })
    };
    let on_move = {
        let drag = drag.clone();
        let redraw = redraw.clone();
        let pointer = pointer.clone();
        let local = local.clone();
        let view_handle = view.clone();
        let view = *view;
        Callback::from(move |event: PointerEvent| {
            let at = local(&event);
            *pointer.borrow_mut() = Some(at);
            let world = view.to_world(at);
            let mut current = drag.borrow_mut();
            match &mut *current {
                Drag::None => return,
                Drag::Pan { start, view: from, .. } => {
                    let moved =
                        View { x: from.x + at.0 - start.0, y: from.y + at.1 - start.1, zoom: from.zoom };
                    view_handle.dispatch(ViewAction::Set(moved));
                    return;
                }
                Drag::Nodes { start, delta, origin, moved, .. } => {
                    let pulled = (world.0 - start.0, world.1 - start.1);
                    *moved |= pulled.0.hypot(pulled.1) * view.zoom > CLICK_SLOP;
                    // Alt: the grabbed node lands on the grid, the others
                    // move with it.
                    *delta = if event.alt_key() { snapped(*origin, pulled) } else { pulled };
                }
                Drag::Wire { at, .. } | Drag::Select { at, .. } => *at = world,
                Drag::Resize { start, from, size, moved, .. } => {
                    let (dx, dy) = (world.0 - start.0, world.1 - start.1);
                    *moved |= dx.hypot(dy) * view.zoom > CLICK_SLOP;
                    if *moved {
                        *size = layout::text_size(Some([from.0 + dx, from.1 + dy]));
                    }
                }
            }
            drop(current);
            redraw.force_update();
        })
    };
    let on_up = {
        let panel = panel.clone();
        let corner_click = corner_click.clone();
        let card = card.clone();
        let drag = drag.clone();
        let redraw = redraw.clone();
        let selection = selection.clone();
        let local = local.clone();
        let open_menu = open_menu.clone();
        let surface = surface.clone();
        Callback::from(move |event: PointerEvent| {
            let ended = std::mem::replace(&mut *drag.borrow_mut(), Drag::None);
            if let Some(element) = surface.cast::<Element>() {
                let _ = element.release_pointer_capture(event.pointer_id());
            }
            match ended {
                Drag::None => return,
                Drag::Pan { start, left, .. } => {
                    let at = local(&event);
                    if left && (at.0 - start.0).hypot(at.1 - start.1) < CLICK_SLOP {
                        selection.set(BTreeSet::new());
                    }
                }
                Drag::Nodes { delta, clicked, moved, .. } => {
                    if moved {
                        let (dx, dy) = delta;
                        let moving = (*selection).clone();
                        edit_card(&panel, &card.id, None, move |card| {
                            for node in card.nodes.iter_mut().filter(|node| moving.contains(&node.id)) {
                                node.x = (node.x + dx).round();
                                node.y = (node.y + dy).round();
                            }
                        });
                    } else if !(event.shift_key() || event.ctrl_key()) {
                        selection.set(BTreeSet::from([clicked]));
                    }
                }
                Drag::Wire { anchor, anchor_is_input, ty, detached, .. } => {
                    let target = drop_target(&event, &card, ty, anchor_is_input);
                    let wire = target.and_then(|target| {
                        if anchor_is_input {
                            wire_between(&card, target, anchor.clone())
                        } else {
                            wire_between(&card, anchor.clone(), target)
                        }
                    });
                    match (wire, detached) {
                        (Some(wire), detached) if detached.as_ref() != Some(&wire) => {
                            edit_card(&panel, &card.id, None, move |card| {
                                if let Some(detached) = &detached {
                                    card.wires.retain(|existing| existing != detached);
                                }
                                connect(card, &wire);
                            });
                        }
                        (Some(_), _) => {} // dropped back where it was
                        (None, Some(detached)) => {
                            edit_card(&panel, &card.id, None, move |card| {
                                card.wires.retain(|existing| *existing != detached);
                            });
                        }
                        (None, None) if !over_node(&event) => {
                            open_menu(local(&event), Some(PendingWire { anchor, anchor_is_input, ty }));
                        }
                        (None, None) => {}
                    }
                }
                Drag::Resize { node, from, size, moved, .. } => {
                    // A resize sets the size; a double click puts it back.
                    let now = js_sys::Date::now();
                    let last = corner_click.borrow_mut().take();
                    let size = if moved {
                        (size != from).then_some(Some([size.0.round(), size.1.round()]))
                    } else if last.is_some_and(|(clicked, at)| clicked == node && now - at < DOUBLE_CLICK_MS)
                    {
                        Some(None)
                    } else {
                        *corner_click.borrow_mut() = Some((node, now));
                        None
                    };
                    if let Some(size) = size {
                        edit_card(&panel, &card.id, None, move |card| {
                            if let Some(NodeKind::Text { size: own, .. }) =
                                card.nodes.iter_mut().find(|each| each.id == node).map(|each| &mut each.kind)
                            {
                                *own = size;
                            }
                        });
                    }
                }
                Drag::Select { start, at } => {
                    let (left, right) = (start.0.min(at.0), start.0.max(at.0));
                    let (top, bottom) = (start.1.min(at.1), start.1.max(at.1));
                    let mut selected = (*selection).clone();
                    for node in &card.nodes {
                        let (w, h) = (layout::width(&node.kind), layout::height(&layout::rows(node, &card)));
                        if node.x < right && node.x + w > left && node.y < bottom && node.y + h > top {
                            selected.insert(node.id);
                        }
                    }
                    selection.set(selected);
                }
            }
            redraw.force_update();
        })
    };
    let on_cancel = {
        let drag = drag.clone();
        let redraw = redraw.clone();
        Callback::from(move |_: PointerEvent| {
            *drag.borrow_mut() = Drag::None;
            redraw.force_update();
        })
    };
    let on_context = {
        let local = local.clone();
        let open_menu = open_menu.clone();
        Callback::from(move |event: MouseEvent| {
            event.prevent_default();
            open_menu(local(&event), None);
        })
    };

    // Adding a node from the menu (and connecting the wire it came from).
    let on_pick = {
        let panel = panel.clone();
        let card = card.clone();
        let menu = menu.clone();
        let selection = selection.clone();
        let opened = (*menu).clone();
        Callback::from(move |kind: NodeKind| {
            let Some(opened) = &opened else { return };
            let id = card.next_node_id();
            let (mut x, mut y) = opened.world;
            if let Some(wire) = &opened.wire {
                // Line the new node's socket up with the pointer.
                y -= layout::HEADER + 18.0;
                if wire.anchor_is_input {
                    x -= layout::width(&kind);
                }
            }
            let node = Node { id, x: x.round(), y: y.round(), kind };
            let wire = opened.wire.as_ref().and_then(|pending| {
                let mut with_node = card.clone();
                with_node.nodes.push(node.clone());
                let target = socket_on(&with_node, &node, pending.ty, pending.anchor_is_input)?;
                if pending.anchor_is_input {
                    wire_between(&with_node, target, pending.anchor.clone())
                } else {
                    wire_between(&with_node, pending.anchor.clone(), target)
                }
            });
            edit_card(&panel, &card.id, None, move |card| {
                card.nodes.push(node.clone());
                if let Some(wire) = &wire {
                    connect(card, wire);
                }
            });
            selection.set(BTreeSet::from([id]));
            menu.set(None);
        })
    };
    let on_close_menu = {
        let menu = menu.clone();
        Callback::from(move |_| menu.set(None))
    };

    // What's drawn: the card, as the current gesture would leave it.
    let current = drag.borrow().clone();
    let mut shown = card.clone();
    match &current {
        Drag::Nodes { delta: (dx, dy), .. } => {
            for node in shown.nodes.iter_mut().filter(|node| selection.contains(&node.id)) {
                node.x += dx;
                node.y += dy;
            }
        }
        Drag::Wire { detached: Some(wire), .. } => shown.wires.retain(|existing| existing != wire),
        Drag::Resize { node, size, .. } => {
            if let Some(NodeKind::Text { size: own, .. }) =
                shown.nodes.iter_mut().find(|each| each.id == *node).map(|each| &mut each.kind)
            {
                *own = Some([size.0, size.1]);
            }
        }
        _ => {}
    }

    let problems = card.problems();
    let node_problems = |id: NodeId| -> Vec<String> {
        problems
            .iter()
            .filter(|problem| problem.node == Some(id))
            .map(|problem| problem.message.clone())
            .collect()
    };
    let context = NodeContext { panel: &panel, card: &shown, on_head, on_body, on_socket, on_resize };

    let mut wires: Vec<Html> = shown
        .wires
        .iter()
        .filter_map(|wire| {
            let from = shown.node(wire.from.node)?;
            let to = shown.node(wire.to.node)?;
            let start = layout::socket_position(from, &shown, &wire.from.socket, false)?;
            let end = layout::socket_position(to, &shown, &wire.to.socket, true)?;
            let ty = from.kind.output(&wire.from.socket)?.ty;
            Some(curve(start, end, node::type_class(ty), false))
        })
        .collect();
    if let Drag::Wire { anchor, anchor_is_input, ty, at, .. } = &current
        && let Some(position) = shown
            .node(anchor.node)
            .and_then(|node| layout::socket_position(node, &shown, &anchor.socket, *anchor_is_input))
    {
        let (start, end) = if *anchor_is_input { (*at, position) } else { (position, *at) };
        wires.push(curve(start, end, node::type_class(*ty), true));
    }

    let select_box = match current {
        Drag::Select { start, at } => html! {
            <div class="select-box" style={format!(
                "left: {}px; top: {}px; width: {}px; height: {}px",
                start.0.min(at.0), start.1.min(at.1), (at.0 - start.0).abs(), (at.1 - start.1).abs()
            )}></div>
        },
        _ => html! {},
    };

    let view_now = *view;
    let grid = GRID * view_now.zoom;
    let surface_style = format!(
        "background-size: {grid}px {grid}px; background-position: {}px {}px",
        view_now.x.rem_euclid(grid),
        view_now.y.rem_euclid(grid)
    );
    let world_style =
        format!("transform: translate({}px, {}px) scale({})", view_now.x, view_now.y, view_now.zoom);
    let panning = matches!(current, Drag::Pan { .. });

    let zoom_by = |factor: f64| {
        let view = view.clone();
        let surface = surface.clone();
        Callback::from(move |_| {
            let (width, height) = size(&surface);
            view.dispatch(ViewAction::ZoomBy((width / 2.0, height / 2.0), factor));
        })
    };
    let fit = {
        let view = view.clone();
        let surface = surface.clone();
        let card = card.clone();
        Callback::from(move |_| view.dispatch(ViewAction::Set(View::fit(&card, size(&surface)))))
    };
    let add_node = {
        let open_menu = open_menu.clone();
        let surface = surface.clone();
        Callback::from(move |_| {
            let (width, height) = size(&surface);
            open_menu((width / 2.0 - 145.0, height / 4.0), None);
        })
    };
    let delete_button = {
        let delete_selected = delete_selected.clone();
        Callback::from(move |_| delete_selected())
    };
    let card_problems: Vec<&str> = problems
        .iter()
        .filter(|problem| problem.node.is_none())
        .map(|problem| problem.message.as_str())
        .collect();

    html! {
        <div class="canvas">
            <div ref={surface.clone()} class={classes!("surface", panning.then_some("panning"))} style={surface_style}
                 onpointerdown={on_surface_down} onpointermove={on_move} onpointerup={on_up}
                 onpointercancel={on_cancel} oncontextmenu={on_context}>
                <div class="world" style={world_style}>
                    <svg class="wires" width="1" height="1">{ for wires }</svg>
                    // Keyed, and alone in their container, so adding or
                    // removing a node leaves the others' elements alone.
                    <div class="nodes">
                        { for shown.nodes.iter().map(|node| node::view(&context, node, selection.contains(&node.id), &node_problems(node.id))) }
                    </div>
                    { select_box }
                </div>
                if card.nodes.is_empty() {
                    <div class="canvas-empty">
                        <p>{ "Add an event (what happens on stream) and an effect (what shows on the overlay), then wire them together." }</p>
                    </div>
                }
            </div>

            <div class="canvas-tools">
                <button type="button" class="button raised small" onclick={add_node}>{ crate::views::icon("plus") }{ "Add node" }</button>
                if !selection.is_empty() {
                    <button type="button" class="button raised small" onclick={delete_button}>
                        { crate::views::icon("trash") }{ if selection.len() == 1 { "Delete node".to_string() } else { format!("Delete {} nodes", selection.len()) } }
                    </button>
                }
            </div>
            if !card_problems.is_empty() {
                <div class="canvas-problems" role="status">
                    { for card_problems.iter().map(|problem| html! { <div>{ problem.to_string() }</div> }) }
                </div>
            }
            <div class="zoom">
                <button type="button" aria-label="Zoom out" onclick={zoom_by(1.0 / 1.2)}>{ "−" }</button>
                <span>{ format!("{}%", (view_now.zoom * 100.0).round()) }</span>
                <button type="button" aria-label="Zoom in" onclick={zoom_by(1.2)}>{ "+" }</button>
                <button type="button" class="fit" onclick={fit} title="Show the whole card (Home)">{ "Fit" }</button>
            </div>
            <div class="legend" aria-hidden="true">
                <span><i class="event"></i>{ "Event" }</span>
                <span><i class="number"></i>{ "Number" }</span>
                <span><i class="text"></i>{ "Text" }</span>
                <span><i class="emotes"></i>{ "Emotes" }</span>
            </div>
            if let Some(opened) = &*menu {
                <AddMenu x={opened.screen.0} y={opened.screen.1}
                         filter={opened.wire.as_ref().map(|wire| WireFilter { ty: wire.ty, from_input: wire.anchor_is_input })}
                         plugins={panel.model.plugins.clone()}
                         on_pick={on_pick} on_close={on_close_menu} />
            }
        </div>
    }
}

/// How far to move a node at `origin` pulled by `pulled` so it lands on
/// the background's dots.
fn snapped(origin: (f64, f64), pulled: (f64, f64)) -> (f64, f64) {
    let to_grid = |value: f64| (value / GRID).round() * GRID;
    (to_grid(origin.0 + pulled.0) - origin.0, to_grid(origin.1 + pulled.1) - origin.1)
}

/// The canvas's size in screen pixels.
fn size(surface: &NodeRef) -> (f64, f64) {
    surface
        .cast::<Element>()
        .map(|element| (element.client_width() as f64, element.client_height() as f64))
        .unwrap_or((800.0, 600.0))
}

/// The socket under the pointer that a wire of type `ty` can go to, or the
/// fitting socket of the node under it.
fn drop_target(
    event: &PointerEvent,
    card: &Card,
    ty: SocketType,
    anchor_is_input: bool,
) -> Option<SocketRef> {
    let hit = gloo::utils::document().element_from_point(event.client_x() as f32, event.client_y() as f32)?;
    if let Some(socket) = hit.closest(".socket").ok().flatten() {
        let node: NodeId = socket.get_attribute("data-node")?.parse().ok()?;
        let name = socket.get_attribute("data-socket")?;
        let is_input = socket.get_attribute("data-input")? == "true";
        return (is_input != anchor_is_input).then(|| SocketRef::new(node, &name));
    }
    let node: NodeId = hit.closest(".node").ok().flatten()?.get_attribute("data-node")?.parse().ok()?;
    socket_on(card, card.node(node)?, ty, anchor_is_input)
}

fn over_node(event: &PointerEvent) -> bool {
    gloo::utils::document()
        .element_from_point(event.client_x() as f32, event.client_y() as f32)
        .and_then(|hit| hit.closest(".node, .socket").ok().flatten())
        .is_some()
}

fn copy(event: &Event, card: &Card, selection: &BTreeSet<NodeId>, then: impl Fn()) {
    if util::typing_in_field(event) {
        return;
    }
    let Some(event) = event.dyn_ref::<ClipboardEvent>() else { return };
    let Some(snippet) = Snippet::of(card, selection) else { return };
    let (Some(data), Ok(json)) = (event.clipboard_data(), serde_json::to_string(&snippet)) else { return };
    if data.set_data("text/plain", &json).is_ok() {
        event.prevent_default();
        then();
    }
}

/// A wire as an S-curve from an output to an input.
fn curve((x1, y1): (f64, f64), (x2, y2): (f64, f64), class: &'static str, dragging: bool) -> Html {
    let bend = ((x2 - x1).abs() / 2.0).max(40.0);
    let d = format!("M {x1} {y1} C {} {y1}, {} {y2}, {x2} {y2}", x1 + bend, x2 - bend);
    html! { <path class={classes!("wire", class, dragging.then_some("dragging"))} {d} /> }
}

/// Whether the wheel is over an element inside the canvas that scrolls
/// (a text box with more text than fits): it scrolls that instead of
/// zooming.
fn scrolls_inside(event: &WheelEvent, surface: &Element) -> bool {
    let window = gloo::utils::window();
    let mut at = event.target().and_then(|target| target.dyn_into::<Element>().ok());
    while let Some(element) = at {
        if element == *surface {
            return false;
        }
        if element.scroll_height() > element.client_height()
            && let Ok(Some(style)) = window.get_computed_style(&element)
            && matches!(style.get_property_value("overflow-y").as_deref(), Ok("auto" | "scroll"))
        {
            return true;
        }
        at = element.parent_element();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_lands_the_grabbed_node_on_the_grid() {
        // At (30, 50), pulled by (10, 3): (40, 53) is nearest (44, 44).
        assert_eq!(snapped((30.0, 50.0), (10.0, 3.0)), (14.0, -6.0));
        // Already on the grid, pulled a little: it stays.
        assert_eq!(snapped((44.0, 22.0), (5.0, -5.0)), (0.0, 0.0));
    }

    #[test]
    fn view_math() {
        let view = View { x: 100.0, y: 50.0, zoom: 2.0 };
        assert_eq!(view.to_world((100.0, 50.0)), (0.0, 0.0));
        assert_eq!(view.to_world((120.0, 70.0)), (10.0, 10.0));
        // Zooming keeps the point under the pointer still.
        let zoomed = view.zoom_at((300.0, 200.0), 1.0);
        assert_eq!(zoomed.to_world((300.0, 200.0)), view.to_world((300.0, 200.0)));
        assert_eq!(view.zoom_at((0.0, 0.0), 100.0).zoom, MAX_ZOOM);
    }

    #[test]
    fn paste_renumbers() {
        let card = protocol::cards::premade().into_iter().find(|card| card.id == "hydrate").unwrap();
        let snippet = Snippet::of(&card, &BTreeSet::from([1, 2])).unwrap();
        assert_eq!(snippet.wires.len(), 1, "only the wire between copied nodes");
        let json = serde_json::to_string(&snippet).unwrap();
        let back = Snippet::parse(&json).unwrap();
        let (nodes, wires) = back.place(&card, Some((0.0, 0.0)));
        assert_eq!(nodes.iter().map(|node| node.id).collect::<Vec<_>>(), [4, 5]);
        assert_eq!((nodes[0].x, nodes[0].y), (0.0, 0.0));
        assert_eq!(wires, [Wire { from: SocketRef::new(4, "event"), to: SocketRef::new(5, "event") }]);
        assert!(Snippet::parse("{\"nodes\": []}").is_none());
    }

    #[test]
    fn wires_check_types() {
        let card = crate::util::premade_cards().into_iter().find(|card| card.id == "rain-on-bits").unwrap();
        assert!(wire_between(&card, SocketRef::new(1, "amount"), SocketRef::new(2, "b")).is_some());
        assert!(wire_between(&card, SocketRef::new(1, "viewer"), SocketRef::new(2, "b")).is_none());
        assert!(
            wire_between(&card, SocketRef::new(2, "result"), SocketRef::new(2, "a")).is_none(),
            "not into itself"
        );
        let bits = card.node(1).unwrap();
        let fall = card.node(4).unwrap();
        // A number dropped on the rain goes to its only number input, though
        // something is wired into it already.
        assert_eq!(socket_on(&card, fall, SocketType::Number, false), Some(SocketRef::new(4, "count")));
        assert_eq!(socket_on(&card, bits, SocketType::Event, true), Some(SocketRef::new(1, "event")));
    }
}
