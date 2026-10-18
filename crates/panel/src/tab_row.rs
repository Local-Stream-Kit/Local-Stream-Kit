//! The header's row of tabs: the app's pages and plugins' tabs. What
//! doesn't fit goes under ⋯ (More), from the row's end. Room is worked out
//! from every tab at its widest (a two-digit count on the tabs that count,
//! in the current tab's weight), so a count coming or going, or moving to
//! another page, never moves a tab; only the row's width and which tabs
//! there are do.

use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use yew::prelude::*;

use crate::route::Route;
use crate::views::icon;

/// The space between two tabs (`.tabs`' gap), in px.
const GAP: f64 = 2.0;

/// Cards and Activity always stay in the row.
const KEEP: usize = 2;

/// One tab in the row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowTab {
    pub route: Route,
    /// As shown, with its count if it has one now: "Activity (3)".
    pub label: String,
    /// The widest it gets ("Activity (99)"), which the row keeps room for.
    pub widest: String,
    /// The page shown is this tab's.
    pub current: bool,
    /// It has a count now: under ⋯, ⋯ gets a dot.
    pub counted: bool,
    /// A plugin's tab (under ⋯, a line parts them from the app's).
    pub plugin: bool,
}

#[derive(Properties, PartialEq)]
pub struct TabRowProps {
    pub tabs: Rc<Vec<RowTab>>,
}

/// How many of the tabs (their widths, in order) the row shows: all of
/// them if they fit, else as many as fit with ⋯ (`more` wide) after them,
/// but never fewer than `keep`.
pub fn fitting(widths: &[f64], more: f64, room: f64, gap: f64, keep: usize) -> usize {
    // Half a pixel for rounding: the room is whole pixels.
    let room = room + 0.5;
    let width = |shown: usize| widths[..shown].iter().sum::<f64>() + gap * shown.saturating_sub(1) as f64;
    if width(widths.len()) <= room {
        return widths.len();
    }
    let mut shown = widths.len();
    while shown > keep && width(shown) + gap + more > room {
        shown -= 1;
    }
    shown
}

#[function_component]
pub fn TabRow(props: &TabRowProps) -> Html {
    let nav = use_node_ref();
    let measure = use_node_ref();
    let shown = use_state(|| usize::MAX);
    let menu = use_state(|| false);
    let widest: Vec<String> = props.tabs.iter().map(|tab| tab.widest.clone()).collect();
    {
        let (nav, measure, shown) = (nav.clone(), measure.clone(), shown.clone());
        use_effect_with(widest.clone(), move |_| {
            let fit = {
                let (nav, measure) = (nav.clone(), measure.clone());
                move || {
                    let (Some(nav), Some(measure)) =
                        (nav.cast::<web_sys::HtmlElement>(), measure.cast::<web_sys::HtmlElement>())
                    else {
                        return;
                    };
                    let children = measure.children();
                    let mut widths: Vec<f64> = (0..children.length())
                        .filter_map(|index| children.item(index))
                        .map(|child| child.get_bounding_client_rect().width())
                        .collect();
                    let Some(more) = widths.pop() else { return };
                    shown.set(fitting(&widths, more, f64::from(nav.client_width()), GAP, KEEP));
                }
            };
            fit();
            // Whenever the room changes (the dock resized), and when the
            // tabs' own widths do (the font loaded).
            let callback = Closure::<dyn FnMut()>::new(fit);
            let observer = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()).ok();
            for element in
                [nav.cast::<web_sys::Element>(), measure.cast::<web_sys::Element>()].into_iter().flatten()
            {
                if let Some(observer) = &observer {
                    observer.observe(&element);
                }
            }
            move || {
                if let Some(observer) = observer {
                    observer.disconnect();
                }
                drop(callback);
            }
        });
    }

    let count = (*shown).min(props.tabs.len());
    let (in_row, under_more) = props.tabs.split_at(count);
    let link = |tab: &RowTab| {
        html! {
            <a href={tab.route.href()} class={classes!("tab", tab.current.then_some("current"))}
               aria-current={tab.current.then_some("page")}>{ tab.label.clone() }</a>
        }
    };
    let toggle = {
        let menu = menu.clone();
        Callback::from(move |_: MouseEvent| menu.set(!*menu))
    };
    let close = {
        let menu = menu.clone();
        Callback::from(move |_: MouseEvent| menu.set(false))
    };
    let keys = {
        let menu = menu.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                menu.set(false);
            }
        })
    };
    let more = (!under_more.is_empty()).then(|| {
        let current = under_more.iter().any(|tab| tab.current);
        let counted = under_more.iter().any(|tab| tab.counted);
        let items = under_more.iter().enumerate().map(|(index, tab)| {
            // A line between the plugins' tabs and the app's.
            let parted = index > 0 && under_more[index - 1].plugin && !tab.plugin;
            html! {
                <>
                    if parted {
                        <div class="menu-sep" role="separator"></div>
                    }
                    <a href={tab.route.href()} role="menuitem" onclick={close.clone()}
                       class={classes!("menu-item", tab.current.then_some("current"))}
                       aria-current={tab.current.then_some("page")}>
                        <span class="grow">{ tab.label.clone() }</span>
                        if tab.current {
                            { icon("check") }
                        }
                    </a>
                </>
            }
        });
        let label = if counted { "More, one with a count" } else { "More" };
        html! {
            <div class="bar-menu tabs-more" onkeydown={keys}>
                <button type="button" id="tabs-more" aria-label={label} title="More" aria-haspopup="menu"
                        aria-expanded={menu.to_string()} onclick={toggle}
                        class={classes!("tab", "more", current.then_some("current"), (*menu).then_some("open"))}>
                    { icon("more") }
                    if counted {
                        <span class="more-dot"></span>
                    }
                </button>
                if *menu {
                    <div class="bar-menu-backdrop" onclick={close.clone()}></div>
                    <div class="bar-menu-list tabs-menu" role="menu">{ for items }</div>
                }
            </div>
        }
    });
    html! {
        <nav class="tabs" aria-label="Sections" ref={nav}>
            { for in_row.iter().map(link) }
            { more.unwrap_or_default() }
            // Every tab at its widest (in the current tab's weight), then
            // ⋯: what the room is worked out from. Never seen.
            // In a box of no size, so it never widens the page.
            <div class="tabs-measure-box" aria-hidden="true">
                <div class="tabs tabs-measure" ref={measure}>
                    { for widest.iter().map(|label| html! { <span class="tab">{ label.clone() }</span> }) }
                    <span class="tab more">{ icon("more") }</span>
                </div>
            </div>
        </nav>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The row's tabs at 13 px IBM Plex Sans (measured 2026-10-17): Cards,
    // Activity (99), Plugins, Test, Status; ⋯ is 36.
    const APP: [f64; 5] = [58.4, 94.8, 66.6, 48.5, 61.3];
    const MORE: f64 = 36.0;

    fn with_plugins(plugins: &[f64]) -> Vec<f64> {
        let mut widths = APP[..2].to_vec();
        widths.extend_from_slice(plugins);
        widths.extend_from_slice(&APP[2..]);
        widths
    }

    #[test]
    fn what_fits_in_the_dock() {
        // The dock: 380 px, 352 for the tabs. Today's five fit.
        assert_eq!(fitting(&APP, MORE, 352.0, GAP, KEEP), 5);
        // A tab that counts (Requests (99)): Plugins, Test and Status go.
        assert_eq!(fitting(&with_plugins(&[105.8]), MORE, 352.0, GAP, KEEP), 3);
        // One that doesn't (Counters): only Test and Status.
        assert_eq!(fitting(&with_plugins(&[76.9]), MORE, 352.0, GAP, KEEP), 4);
        // Three: only the first plugin's stays.
        assert_eq!(fitting(&with_plugins(&[76.9, 105.8, 52.2]), MORE, 352.0, GAP, KEEP), 3);
        // A 300 px dock without plugin tabs: Test and Status go.
        assert_eq!(fitting(&APP, MORE, 272.0, GAP, KEEP), 3);
        // Full width: everything.
        assert_eq!(fitting(&with_plugins(&[76.9, 105.8, 52.2]), MORE, 732.0, GAP, KEEP), 8);
    }

    #[test]
    fn cards_and_activity_always_stay() {
        assert_eq!(fitting(&APP, MORE, 100.0, GAP, KEEP), 2);
        assert_eq!(fitting(&[], MORE, 0.0, GAP, KEEP), 0);
    }
}
