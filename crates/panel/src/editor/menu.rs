//! The add-node menu: every node kind by category, with search. Opened from
//! the toolbar, Shift+A, a right click, or by dropping a wire on empty
//! canvas (then it only offers nodes that wire can connect to).

use std::rc::Rc;

use protocol::cards::{Category, NodeKind, SocketType};
use protocol::plugin::Catalog;
use web_sys::{HtmlInputElement, KeyboardEvent, PointerEvent};
use yew::prelude::*;

use crate::util;
use crate::views::new_card::category_class;
use crate::views::node_icon;

/// The loose end of a wire dropped on empty canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireFilter {
    pub ty: SocketType,
    /// The wire's loose end came from an input (so the new node needs a
    /// matching output).
    pub from_input: bool,
}

impl WireFilter {
    pub fn fits(&self, kind: &NodeKind) -> bool {
        if self.from_input {
            kind.outputs().iter().any(|output| self.ty.accepts(output.ty))
        } else {
            kind.inputs().iter().any(|input| input.ty.accepts(self.ty))
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct AddMenuProps {
    /// Position in the canvas, in screen pixels.
    pub x: f64,
    pub y: f64,
    pub filter: Option<WireFilter>,
    /// The installed plugins, whose node types are listed too.
    pub plugins: Rc<Catalog>,
    pub on_pick: Callback<NodeKind>,
    pub on_close: Callback<()>,
}

/// Every node kind, by category in menu order: the built-in ones, then
/// plugins' (in their manifests' order), and Custom event last of the
/// events (it catches anything sent).
fn menu_kinds(plugins: &protocol::plugin::Catalog) -> Vec<NodeKind> {
    let (custom, built_in): (Vec<NodeKind>, Vec<NodeKind>) =
        NodeKind::catalog().into_iter().partition(|kind| matches!(kind, NodeKind::Custom { .. }));
    let all: Vec<NodeKind> = built_in.into_iter().chain(plugins.node_kinds()).chain(custom).collect();
    Category::ALL
        .into_iter()
        .flat_map(|category| all.iter().filter(move |kind| kind.category() == category).cloned())
        .collect()
}

fn matches(kind: &NodeKind, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || kind.title().to_lowercase().contains(&query)
        || kind.category().title().to_lowercase().contains(&query)
        || kind.platform().is_some_and(|platform| platform.contains(&query))
}

/// Where the search matches a node's title, ignoring case (none when it
/// only matches its group or platform, or lowercasing changed lengths).
fn match_in(title: &str, query: &str) -> Option<(usize, usize)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return None;
    }
    let at = title.to_lowercase().find(&query)?;
    let end = at + query.len();
    (title.is_char_boundary(at) && title.is_char_boundary(end)).then_some((at, end))
}

/// A node's title with the search's match in bold, in one span: the item
/// is a flex row, which would space its pieces apart.
fn marked(title: &str, query: &str) -> Html {
    match match_in(title, query) {
        Some((at, end)) => html! { <span>{ &title[..at] }<b>{ &title[at..end] }</b>{ &title[end..] }</span> },
        None => html! { <span>{ title.to_string() }</span> },
    }
}

/// Unique among the menu's items: two plugins may use the same title.
fn menu_key(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Plugin { plugin, node, .. } => format!("plugin/{plugin}/{node}"),
        kind => kind.title().to_string(),
    }
}

#[function_component]
pub fn AddMenu(props: &AddMenuProps) -> Html {
    let query = use_state(String::new);
    let active = use_state(|| 0_usize);
    let search = use_node_ref();
    {
        let search = search.clone();
        use_effect_with((), move |_| {
            if let Some(input) = search.cast::<HtmlInputElement>() {
                let _ = input.focus();
            }
        });
    }

    let kinds: Vec<NodeKind> = menu_kinds(&props.plugins)
        .into_iter()
        .filter(|kind| props.filter.is_none_or(|filter| filter.fits(kind)))
        .filter(|kind| matches(kind, &query))
        .collect();
    let active_index = (*active).min(kinds.len().saturating_sub(1));

    let oninput = {
        let query = query.clone();
        let active = active.clone();
        Callback::from(move |event: InputEvent| {
            query.set(util::value_of(&event));
            active.set(0);
        })
    };
    let onkeydown = {
        let active = active.clone();
        let kinds = kinds.clone();
        let on_pick = props.on_pick.clone();
        let on_close = props.on_close.clone();
        Callback::from(move |event: KeyboardEvent| {
            match event.key().as_str() {
                "ArrowDown" => active.set((active_index + 1).min(kinds.len().saturating_sub(1))),
                "ArrowUp" => active.set(active_index.saturating_sub(1)),
                "Enter" => {
                    if let Some(kind) = kinds.get(active_index) {
                        on_pick.emit(kind.clone());
                    }
                }
                "Escape" => on_close.emit(()),
                _ => return,
            }
            event.prevent_default();
            event.stop_propagation();
        })
    };

    let mut items = Vec::new();
    let mut group = None;
    for (index, kind) in kinds.iter().enumerate() {
        let category = kind.category();
        if group != Some(category) {
            group = Some(category);
            // Its group's colour by its name; the heading stays at the
            // top while its nodes scroll under it.
            items.push(html! {
                <div class="menu-group" key={category.title()}><i class={category_class(category)}></i>{ category.title() }</div>
            });
        }
        let onclick = {
            let on_pick = props.on_pick.clone();
            let kind = kind.clone();
            Callback::from(move |_| on_pick.emit(kind.clone()))
        };
        let onmouseenter = {
            let active = active.clone();
            Callback::from(move |_| active.set(index))
        };
        items.push(html! {
            <button type="button" key={menu_key(kind)} class={classes!("menu-item", (index == active_index).then_some("active"))}
                    {onclick} {onmouseenter}>
                <span class="menu-icon">{ node_icon(&props.plugins, kind) }</span>
                { marked(kind.title(), &query) }
            </button>
        });
    }

    html! {
        <div class="menu add-menu" style={format!("left: {}px; top: {}px", props.x, props.y)}
             role="dialog" aria-label="Add a node"
             onpointerdown={Callback::from(|event: PointerEvent| event.stop_propagation())}>
            <div class="menu-search">
                <input ref={search} type="search" placeholder="Search nodes" aria-label="Search nodes"
                       value={(*query).clone()} {oninput} {onkeydown} />
            </div>
            <div class="menu-list">
                if items.is_empty() {
                    <div class="menu-empty">{ "Nothing matches." }</div>
                } else {
                    { for items }
                }
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_filter() {
        let from_amount = WireFilter { ty: SocketType::Number, from_input: false };
        assert!(from_amount.fits(&NodeKind::Math { op: Default::default(), a: 0.0, b: 1.0 }));
        assert!(from_amount.fits(&NodeKind::Random { min: 1.0, max: 2.0 }));
        assert!(!from_amount.fits(&NodeKind::Chance { percent: 1.0 }));
        let into_trigger = WireFilter { ty: SocketType::Event, from_input: true };
        assert!(into_trigger.fits(&NodeKind::GiftBomb));
        assert!(!into_trigger.fits(&NodeKind::Number { value: 1.0 }));
        assert!(matches(&NodeKind::GiftBomb, "gift"));
        assert!(matches(&NodeKind::GiftBomb, "EVENTS"));
        assert!(!matches(&NodeKind::GiftBomb, "rain"));
        assert!(matches(&NodeKind::GiftBomb, "Twitch"), "by its platform");
    }

    #[test]
    fn the_match_is_marked() {
        assert_eq!(match_in("Emote rain", "RAI"), Some((6, 9)), "ignoring case");
        assert_eq!(match_in("Raid", " "), None);
        assert_eq!(match_in("Bits", "twitch"), None, "found by its platform, not its title");
    }

    #[test]
    fn custom_event_is_the_last_event() {
        let kinds = menu_kinds(&protocol::plugin::Catalog::default());
        let events: Vec<&NodeKind> = kinds.iter().filter(|kind| kind.is_event()).collect();
        assert!(matches!(events.last(), Some(NodeKind::Custom { .. })));
        assert_eq!(kinds.len(), NodeKind::catalog().len());
    }
}
