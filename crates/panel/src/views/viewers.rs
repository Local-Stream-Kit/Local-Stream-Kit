//! Viewers in Activity: Find a viewer (a field over the queue), and the
//! card of the viewer picked there or by their name anywhere in Activity:
//! what Twitch says about them, their log (their chat as the app saw it,
//! with their events from the History) and the moderator's buttons.

use std::cell::Cell;
use std::rc::Rc;

use gloo::timers::callback::Interval;
use protocol::AlertPart;
use protocol::activity::{HistoryEntry, NameColor};
use protocol::config::ClosedNotes;
use protocol::panel::PanelRequest;
use protocol::viewers::{
    Access, Banned, DELETABLE_FOR_MS, Done, FoundViewer, LineKind, LogLine, TIMEOUTS, ViewerAction,
    ViewerInfo, duration,
};
use yew::prelude::*;

use super::activity::use_confirm;
use super::icon;
use crate::model::ViewerState;
use crate::route::Route;
use crate::{Panel, util};

/// An account younger than this reads in amber: often a bot.
const NEW_ACCOUNT_MS: u64 = 7 * 86_400_000;

thread_local! {
    /// Where the page was scrolled to when a name opened a card: closing
    /// it goes back there.
    static RETURN_TO: Cell<Option<f64>> = const { Cell::new(None) };
}

/// What a name in Activity does: opens its card, at the top of the page.
pub fn opener(panel: &Panel) -> Callback<NameColor> {
    let panel = panel.clone();
    Callback::from(move |name: NameColor| {
        if let Some(window) = web_sys::window() {
            RETURN_TO.with(|at| at.set(window.scroll_y().ok()));
            window.scroll_to_with_x_and_y(0.0, 0.0);
        }
        panel.open_viewer(Some(name.id.clone()).filter(|id| !id.is_empty()), name.name.clone());
    })
}

/// Find a viewer: a field whose list offers the viewers the chat log has
/// seen; Enter on a name it doesn't offer looks it up on Twitch.
#[function_component]
pub fn FindViewer() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let text = use_state(String::new);
    let focused = use_state(|| false);
    let picked = use_state(|| 0usize);
    let (answered, found) = &*panel.model.found;
    let typed = text.trim().to_string();
    let offered: Vec<FoundViewer> =
        if !typed.is_empty() && answered.trim() == typed { found.clone() } else { Vec::new() };
    let listed = *focused && !offered.is_empty();
    let open = {
        let (panel, text) = (panel.clone(), text.clone());
        Callback::from(move |(viewer, typed): (Option<FoundViewer>, String)| {
            RETURN_TO.with(|at| at.set(None));
            match viewer {
                Some(viewer) => panel.open_viewer(Some(viewer.id), viewer.name),
                None if !typed.trim().is_empty() => panel.open_viewer(None, typed),
                None => return,
            }
            text.set(String::new());
        })
    };
    let oninput = {
        let (panel, text, picked) = (panel.clone(), text.clone(), picked.clone());
        Callback::from(move |event: InputEvent| {
            let value = util::value_of(&event);
            if !value.trim().is_empty() {
                panel.send(PanelRequest::FindViewers { text: value.trim().to_string() });
            }
            text.set(value);
            picked.set(0);
        })
    };
    let onkeydown = {
        let (open, offered, picked, text) = (open.clone(), offered.clone(), picked.clone(), text.clone());
        Callback::from(move |event: KeyboardEvent| match event.key().as_str() {
            "ArrowDown" if !offered.is_empty() => {
                event.prevent_default();
                picked.set((*picked + 1) % offered.len());
            }
            "ArrowUp" if !offered.is_empty() => {
                event.prevent_default();
                picked.set((*picked + offered.len() - 1) % offered.len());
            }
            _ if event.key() == "Enter" || event.key_code() == 13 => {
                event.prevent_default();
                open.emit((offered.get(*picked).cloned(), (*text).clone()));
            }
            "Escape" => text.set(String::new()),
            _ => {}
        })
    };
    let onfocus = {
        let focused = focused.clone();
        Callback::from(move |_: FocusEvent| focused.set(true))
    };
    let onblur = {
        let focused = focused.clone();
        Callback::from(move |_: FocusEvent| focused.set(false))
    };
    let missing = panel.model.viewer_missing.as_ref().map(|(_, problem)| problem.clone());
    html! {
        <div class="viewer-find">
            <div class="viewer-find-field">
                { icon("search") }
                <input type="search" id="viewer-find" placeholder="Find a viewer" aria-label="Find a viewer"
                       autocomplete="off" role="combobox" aria-controls="viewer-found" aria-expanded={listed.to_string()}
                       value={(*text).clone()} {oninput} {onkeydown} {onfocus} {onblur} />
            </div>
            if listed {
                <ul class="viewer-found" id="viewer-found" role="listbox" aria-label="Viewers">
                    { for offered.iter().enumerate().map(|(at, viewer)| {
                        let onclick = {
                            let (open, viewer) = (open.clone(), viewer.clone());
                            Callback::from(move |_: MouseEvent| open.emit((Some(viewer.clone()), String::new())))
                        };
                        // The field keeps the focus, so the list stays.
                        let onmousedown = Callback::from(|event: MouseEvent| event.prevent_default());
                        let color = viewer.color.as_deref().map(|color| format!("color: {}", util::readable_color(color)));
                        html! {
                            <li role="option" aria-selected={(at == *picked).to_string()}
                                class={classes!("viewer-found-item", (at == *picked).then_some("on"))}
                                data-viewer={viewer.login.clone()} {onclick} {onmousedown}>
                                <span class="viewer-found-name" style={color}>{ marked(&viewer.name, &typed) }</span>
                                <span class="viewer-found-seen mono">{ util::time(viewer.last_seen_ms) }</span>
                            </li>
                        }
                    }) }
                </ul>
            }
            if let Some(problem) = missing {
                <p class="danger-note" id="viewer-missing" role="alert">{ icon("error") }{ problem }</p>
            }
        </div>
    }
}

/// `name` with the part that matches `typed` in bold.
fn marked(name: &str, typed: &str) -> Html {
    let lower = name.to_lowercase();
    match lower.find(&typed.to_lowercase()).filter(|_| lower.len() == name.len()) {
        Some(at) => {
            let end = at + typed.len();
            html! { <>{ &name[..at] }<b>{ &name[at..end] }</b>{ &name[end..] }</> }
        }
        None => html! { { name } },
    }
}

/// The open viewer's card, if there is one.
#[function_component]
pub fn ViewerBox() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    match panel.model.viewer.clone() {
        Some(viewer) => {
            let key = viewer.card.id.clone();
            html! { <Card {key} {viewer} /> }
        }
        None => html! {},
    }
}

#[derive(Properties, PartialEq)]
struct CardProps {
    viewer: Rc<ViewerState>,
}

#[function_component]
fn Card(props: &CardProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let viewer = &props.viewer;
    let card = &viewer.card;
    let menu = use_state(|| false);
    let warning = use_state(|| None::<String>);
    let ban = use_confirm();
    let make_mod = use_confirm();
    let log_ref = use_node_ref();
    let scroll = use_mut_ref(|| None::<(f64, usize)>);
    // Drawn again now and then, so a timeout's time left counts down.
    let tick = use_state(|| 0u32);
    {
        let tick = tick.clone();
        use_effect_with((), move |()| {
            let ticker = Interval::new(15_000, move || tick.set(tick.wrapping_add(1)));
            move || drop(ticker)
        });
    }
    // Opened again after the connection came back: its lines go on.
    let was_connected = use_mut_ref(|| panel.model.connected);
    {
        let (panel, was) = (panel.clone(), was_connected.clone());
        let (connected, id, name) = (panel.model.connected, card.id.clone(), card.name.clone());
        use_effect_with(connected, move |connected| {
            if *connected && !*was.borrow() {
                panel.open_viewer(Some(id), name);
            }
            *was.borrow_mut() = *connected;
        });
    }
    let open_viewer = opener(&panel);
    let events = events(
        &panel.model.history,
        card.id.as_str(),
        &card.name,
        &card.login,
        card.lines.first(),
        card.older,
    );
    // The newest at the bottom, like chat; older ones loaded keep the view
    // where it was, and a new line is followed while the view is at the end.
    {
        let (log_ref, scroll) = (log_ref.clone(), scroll.clone());
        use_effect_with((card.lines.len() + events.len(), card.older), move |&(_, older)| {
            if let Some(log) = log_ref.cast::<web_sys::Element>() {
                let height = f64::from(log.scroll_height());
                let last = *scroll.borrow();
                match last {
                    Some((before, was_older)) if older < was_older => {
                        log.set_scroll_top(log.scroll_top() + height - before);
                    }
                    Some((before, _)) => {
                        if log.scroll_top() + f64::from(log.client_height()) >= before - 24.0 {
                            log.set_scroll_top(height);
                        }
                    }
                    None => log.set_scroll_top(height),
                }
                *scroll.borrow_mut() = Some((height, older));
            }
        });
    }
    let on_close = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.close_viewer();
            if let (Some(window), Some(y)) = (web_sys::window(), RETURN_TO.with(Cell::take)) {
                window.scroll_to_with_x_and_y(0.0, y);
            }
        })
    };
    let now_ms = js_sys::Date::now() as u64;
    let info = viewer.info.as_ref();
    // A timeout that has run out is over.
    let banned =
        info.and_then(|info| info.banned).filter(|ban| ban.until_ms.is_none_or(|until| until > now_ms));
    let state = banned.map(|ban| match ban.until_ms {
        Some(until) => {
            let left = until.saturating_sub(now_ms).div_ceil(60_000).max(1) * 60;
            ("warn", format!("Timed out · {} left", duration(left)))
        }
        None => ("error", "Banned".to_string()),
    });
    let color = card.color.as_deref().map(|color| format!("color: {}", util::readable_color(color)));
    let channel = panel
        .model
        .config
        .as_ref()
        .map(|config| config.connections.twitch_channel.clone())
        .unwrap_or_default();
    let open_twitch = {
        let (panel, login) = (panel.clone(), card.login.clone());
        Callback::from(move |event: MouseEvent| {
            event.prevent_default();
            panel.send(PanelRequest::OpenTwitchCard { login: login.clone() });
        })
    };
    let older = {
        let (panel, id, before) = (panel.clone(), card.id.clone(), card.older);
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::OlderViewerLines { id: id.clone(), before });
        })
    };
    // Status with the Twitch box open, where the ticks are.
    let closed = panel.model.config.as_ref().map(|config| config.closed_notes.clone()).unwrap_or_default();
    // A note's ×: closed for good, in every panel (saved in the config).
    let close = |id: &str, change: fn(&mut protocol::config::ClosedNotes, String), value: String| {
        let panel = panel.clone();
        let onclick = Callback::from(move |_: MouseEvent| {
            let value = value.clone();
            panel.edit(None, move |config| change(&mut config.closed_notes, value.clone()));
        });
        html! {
            <button type="button" class="notice-close" id={id.to_string()} title="Close" aria-label="Close" {onclick}>{ "×" }</button>
        }
    };
    let ticks = |text: &str| {
        html! {
            <div class="notice info viewer-access" id="viewer-access" role="note">
                { icon("help") }
                <span>{ text.to_string() }<a href={Route::Login.href()} id="viewer-access-ticks">{ "Twitch" }</a></span>
            </div>
        }
    };
    let buttons = match &card.access {
        Access::Ready { roles } => buttons(&panel, viewer, banned, *roles, &menu, &warning, &ban, &make_mod),
        Access::Off => ticks("Moderating is off: "),
        Access::NewLogin => ticks("Moderating needs a new login: "),
        Access::LoggedOut => html! {
            <div class="notice info viewer-access" id="viewer-access" role="note">
                { icon("help") }
                <span>{ "Log in with Twitch on " }<a href={Route::Status.href()}>{ "Status" }</a>{ " to time out, ban or warn." }</span>
            </div>
        },
        // Closed, it stays closed for this login and channel.
        Access::OtherChannel { login } if closed.card_other_channel == ClosedNotes::pair(login, &channel) => {
            html! {}
        }
        Access::OtherChannel { login } => html! {
            <div class="notice info viewer-access" id="viewer-access" role="note">
                { icon("help") }
                <span>
                    { format!("The Twitch login is for {login}, not {channel}: log in as {channel} on ") }
                    <a href={Route::Status.href()}>{ "Status" }</a>{ " to moderate." }
                </span>
                { close("viewer-access-close", |notes, pair| notes.card_other_channel = pair, ClosedNotes::pair(login, &channel)) }
            </div>
        },
    };
    let deletable = matches!(card.access, Access::Ready { .. });
    html! {
        <section class="panel-box viewer-card" id="viewer-card" aria-label={card.name.clone()} data-viewer={card.login.clone()}>
            <div class="box-head viewer-head">
                if !card.badges.is_empty() {
                    <span class="viewer-badges">
                        { for card.badges.iter().map(|badge| match &badge.image {
                            Some(image) => html! { <img class="viewer-badge" src={image.clone()} alt={badge.title.clone()} title={badge.title.clone()} /> },
                            None => html! {},
                        }) }
                    </span>
                }
                <span class="box-title viewer-title" style={color}>{ card.name.clone() }</span>
                if let Some((dot, text)) = state {
                    <span class="queue-state" id="viewer-state"><span class={classes!("dot", dot)}></span>{ text }</span>
                }
                <button type="button" class="queue-skip-one" id="viewer-close" title="Close" aria-label="Close" onclick={on_close}>{ "×" }</button>
            </div>
            if let Some(info) = info {
                <p class="viewer-info" id="viewer-info">{ about(info, now_ms) }</p>
            } else if let Some(problem) = &viewer.info_problem {
                <p class="muted small" id="viewer-info-problem">{ problem.clone() }</p>
            }
            { buttons }
            if let Some(problem) = &viewer.problem {
                <p class="danger-note" id="viewer-problem" role="alert">{ icon("error") }{ problem.clone() }</p>
            }
            <div class="viewer-log-section">
                <div class="viewer-log-head">
                    <span class="queue-heading" id="viewer-messages">{ format!("Messages · {}", card.messages) }</span>
                    // The note closed: its link here, so the full log stays
                    // a click away.
                    if closed.chat_log && !card.login.is_empty() {
                        <a href="#" id="viewer-twitch-card" onclick={open_twitch.clone()}>{ "On Twitch" }{ icon("external") }</a>
                    }
                </div>
                if !closed.chat_log {
                    <div class="notice warn viewer-twitch-note" id="viewer-twitch-note" role="note">
                        { icon("warn") }
                        <span>
                            { "Only chat seen while Local Stream Kit was running. " }
                            if !card.login.is_empty() {
                                <a href="#" id="viewer-twitch-card" onclick={open_twitch}>{ "Full log on Twitch" }{ icon("external") }</a>
                            }
                        </span>
                        { close("viewer-twitch-note-close", |notes, _| notes.chat_log = true, String::new()) }
                    </div>
                }
                <div class="viewer-log" id="viewer-log" ref={log_ref}>
                    if card.older > 0 {
                        <div class="viewer-older">
                            <button type="button" class="button ghost small" id="viewer-older" onclick={older}>
                                { format!("Show older ({})", card.older) }
                            </button>
                        </div>
                    }
                    if card.lines.is_empty() && events.is_empty() {
                        <p class="muted small viewer-none">{ "None seen yet." }</p>
                    }
                    { rows(&panel, card, &events, deletable, now_ms, &open_viewer) }
                </div>
            </div>
        </section>
    }
}

/// "Following since 14 Oct 2025 · Sub · Account 3 years old".
fn about(info: &ViewerInfo, now_ms: u64) -> Html {
    let following = match info.following_since_ms {
        Some(at) => format!("Following since {}", util::date(at)),
        None => "Not following".into(),
    };
    let sub = info.sub.map(|sub| match (sub.gifted, sub.tier) {
        (true, 1) => "Gifted sub".to_string(),
        (true, tier) => format!("Gifted sub (tier {tier})"),
        (false, 1) => "Sub".to_string(),
        (false, tier) => format!("Sub (tier {tier})"),
    });
    let age = now_ms.saturating_sub(info.created_ms);
    let young = age < NEW_ACCOUNT_MS;
    html! {
        <>
            { following }
            if let Some(sub) = sub {
                { format!(" · {sub}") }
            }
            if info.created_ms > 0 {
                { " · Account " }
                <span class={classes!(young.then_some("viewer-young"))}>{ format!("{} old", util::age(age)) }</span>
            }
        </>
    }
}

/// The moderator's buttons, as they apply: what undoes a ban or a timeout
/// while there is one, and, with `roles` (VIPs and mods ticked and given),
/// VIP or mod (Twitch allows one or the other).
#[allow(clippy::too_many_arguments)]
fn buttons(
    panel: &Panel,
    viewer: &ViewerState,
    banned: Option<Banned>,
    roles: bool,
    menu: &UseStateHandle<bool>,
    warning: &UseStateHandle<Option<String>>,
    ban: &UseStateHandle<bool>,
    make_mod: &UseStateHandle<bool>,
) -> Html {
    let busy = viewer.pending.is_some();
    let name = viewer.card.name.clone();
    let (vip, moderator) = viewer.info.as_ref().map_or((false, false), |info| (info.vip, info.moderator));
    let act = |action: ViewerAction| {
        let (panel, menu, warning) = (panel.clone(), menu.clone(), warning.clone());
        Callback::from(move |_: MouseEvent| {
            menu.set(false);
            warning.set(None);
            panel.moderate(action.clone());
        })
    };
    let button = |id: &str, label: &str, onclick: Callback<MouseEvent>| {
        html! {
            <button type="button" id={id.to_string()} class="button ghost small" disabled={busy} {onclick}>{ label.to_string() }</button>
        }
    };
    let asked = |id: &str,
                 confirm: &UseStateHandle<bool>,
                 label: &str,
                 asking: String,
                 look: &str,
                 action: ViewerAction| {
        let (confirm, act) = (confirm.clone(), act(action));
        let onclick = {
            let confirm = confirm.clone();
            Callback::from(move |event: MouseEvent| {
                if *confirm {
                    act.emit(event);
                }
                confirm.set(!*confirm);
            })
        };
        html! {
            <button type="button" id={id.to_string()} disabled={busy}
                    class={classes!("button", "small", if *confirm { look.to_string() } else { "ghost".to_string() })} {onclick}>
                { if *confirm { asking } else { label.to_string() } }
            </button>
        }
    };
    let toggle_menu = {
        let menu = menu.clone();
        Callback::from(move |_: MouseEvent| menu.set(!*menu))
    };
    let toggle_warning = {
        let (warning, menu) = (warning.clone(), menu.clone());
        Callback::from(move |_: MouseEvent| {
            menu.set(false);
            warning.set(if warning.is_some() { None } else { Some(String::new()) });
        })
    };
    let first = match banned {
        Some(Banned { until_ms: None }) => {
            html! { { button("viewer-unban", "Unban", act(ViewerAction::Unban)) } }
        }
        Some(_) => html! {
            <>
                { button("viewer-unban", "Remove timeout", act(ViewerAction::Unban)) }
                { asked("viewer-ban", ban, "Ban", format!("Ban {name}?"), "danger", ViewerAction::Ban) }
            </>
        },
        None => html! {
            <>
                <span class="viewer-menu-anchor">
                    <button type="button" id="viewer-timeout" aria-haspopup="menu" aria-expanded={menu.to_string()} disabled={busy}
                            class={classes!("button", "small", if **menu { "pressed" } else { "ghost" })} onclick={toggle_menu}>
                        { "Timeout" }{ icon("down") }
                    </button>
                    if **menu {
                        <div class="menu viewer-menu" id="viewer-timeouts" role="menu" aria-label="Timeout for">
                            { for TIMEOUTS.iter().map(|secs| html! {
                                <button type="button" role="menuitem" class="menu-item" onclick={act(ViewerAction::Timeout { secs: *secs })}>
                                    { duration(u64::from(*secs)) }
                                </button>
                            }) }
                        </div>
                    }
                </span>
                { asked("viewer-ban", ban, "Ban", format!("Ban {name}?"), "danger", ViewerAction::Ban) }
                <button type="button" id="viewer-warn" aria-expanded={warning.is_some().to_string()} disabled={busy}
                        class={classes!("button", "small", if warning.is_some() { "pressed" } else { "ghost" })} onclick={toggle_warning}>
                    { "Warn" }
                </button>
            </>
        },
    };
    let second = banned.is_none().then(|| {
        html! {
            <span class="viewer-buttons-group">
                { button("viewer-shoutout", "Shoutout", act(ViewerAction::Shoutout)) }
                if !roles {
                } else if moderator {
                    { button("viewer-unmod", "Remove mod", act(ViewerAction::RemoveMod)) }
                } else if vip {
                    { button("viewer-unvip", "Remove VIP", act(ViewerAction::RemoveVip)) }
                } else {
                    { button("viewer-vip", "VIP", act(ViewerAction::AddVip)) }
                    { asked("viewer-mod", make_mod, "Mod", format!("Make {name} a mod?"), "primary", ViewerAction::AddMod) }
                }
            </span>
        }
    });
    let warn_row = warning.as_ref().map(|reason| {
        let send = {
            let (panel, warning, reason) = (panel.clone(), warning.clone(), reason.clone());
            move || {
                if !reason.trim().is_empty() {
                    panel.moderate(ViewerAction::Warn { reason: reason.trim().to_string() });
                    warning.set(None);
                }
            }
        };
        let oninput = {
            let warning = warning.clone();
            Callback::from(move |event: InputEvent| warning.set(Some(util::value_of(&event))))
        };
        let onkeydown = {
            let send = send.clone();
            Callback::from(move |event: KeyboardEvent| {
                if event.key() == "Enter" {
                    event.prevent_default();
                    send();
                }
            })
        };
        let onclick = Callback::from(move |_: MouseEvent| send());
        html! {
            <div class="viewer-warn">
                <input type="text" id="viewer-warn-reason" placeholder="Reason" aria-label="Reason" autofocus=true
                       value={reason.clone()} {oninput} {onkeydown} />
                <button type="button" id="viewer-warn-send" class="button primary small"
                        disabled={busy || reason.trim().is_empty()} {onclick}>{ "Send" }</button>
            </div>
        }
    });
    html! {
        <>
            <div class="viewer-buttons" id="viewer-buttons">
                <span class="viewer-buttons-group">{ first }</span>
                { second.unwrap_or_default() }
            </div>
            { warn_row.unwrap_or_default() }
        </>
    }
}

/// The History's entries about this viewer that fall among the lines
/// shown (all of them while no older lines are left), oldest first.
fn events<'h>(
    history: &'h [HistoryEntry],
    id: &str,
    name: &str,
    login: &str,
    first: Option<&LogLine>,
    older: usize,
) -> Vec<&'h HistoryEntry> {
    let from = if older > 0 { first.map_or(u64::MAX, |line| line.at_ms) } else { 0 };
    let theirs = |known: &NameColor| {
        if known.id.is_empty() {
            known.name.eq_ignore_ascii_case(name) || known.name.eq_ignore_ascii_case(login)
        } else {
            known.id == id
        }
    };
    let mut events: Vec<&HistoryEntry> = history
        .iter()
        .filter(|entry| entry.replay_of.is_none() && entry.at_ms >= from && entry.names.iter().any(theirs))
        .collect();
    events.sort_by_key(|entry| entry.at_ms);
    events
}

/// The log's lines and the events, in time order, a line between days.
fn rows(
    panel: &Panel,
    card: &protocol::viewers::ViewerCard,
    events: &[&HistoryEntry],
    deletable: bool,
    now_ms: u64,
    open_viewer: &Callback<NameColor>,
) -> Html {
    enum Row<'r> {
        Line(&'r LogLine),
        Event(&'r HistoryEntry),
    }
    let mut all: Vec<(u64, Row)> = card.lines.iter().map(|line| (line.at_ms, Row::Line(line))).collect();
    all.extend(events.iter().map(|entry| (entry.at_ms, Row::Event(entry))));
    all.sort_by_key(|(at, _)| *at);
    let mut day = String::new();
    let mut out = Vec::with_capacity(all.len() * 2);
    for (at, row) in all {
        let this = util::day(at);
        if this != day {
            out.push(html! { <div class="log-day"><span>{ this.clone() }</span></div> });
            day = this;
        }
        let time = html! { <span class="log-time mono">{ util::clock(at) }</span> };
        out.push(match row {
            Row::Line(line) => line_row(panel, line, time, deletable && at + DELETABLE_FOR_MS > now_ms),
            Row::Event(entry) => html! {
                <div class="log-line log-event" data-entry={entry.id.to_string()}>
                    { time }
                    <span class="log-text">
                        { event_text(entry, card, open_viewer) }
                        if !entry.message.is_empty() {
                            <span class="log-quote">{ entry.message.clone() }</span>
                        }
                    </span>
                </div>
            },
        });
    }
    html! { { for out } }
}

fn line_row(panel: &Panel, line: &LogLine, time: Html, deletable: bool) -> Html {
    match &line.kind {
        LineKind::Message { id, text, parts: pieces, deleted, action } => {
            let bin = (deletable && !id.is_empty() && !*deleted).then(|| {
                let (panel, message) = (panel.clone(), id.clone());
                let onclick = Callback::from(move |_: MouseEvent| {
                    panel.moderate(ViewerAction::Delete { message: message.clone() });
                });
                html! {
                    <button type="button" class="log-bin" title="Delete this message" aria-label="Delete this message" {onclick}>
                        { icon("trash") }
                    </button>
                }
            });
            html! {
                <div class={classes!("log-line", "log-message", deleted.then_some("deleted"), action.then_some("action"))}
                     data-message={id.clone()}>
                    { time }
                    <span class="log-text">
                        <span class="log-said">{ if pieces.is_empty() { html! { { text.clone() } } } else { parts(pieces) } }</span>
                        if *deleted {
                            <span class="log-deleted-tag">{ " · deleted" }</span>
                        }
                    </span>
                    { bin.unwrap_or_default() }
                </div>
            }
        }
        LineKind::TimedOut { secs } => html! {
            <div class="log-line log-mod">{ time }<span class="log-text">{ format!("Timed out for {}", duration(*secs)) }</span></div>
        },
        LineKind::Banned => html! {
            <div class="log-line log-ban">{ time }<span class="log-text">{ "Banned" }</span></div>
        },
        LineKind::Done { done } => html! {
            <div class={classes!("log-line", if matches!(done, Done::Unbanned | Done::TimeoutRemoved | Done::Warned) { "log-mod" } else { "log-done" })}>
                { time }<span class="log-text">{ done.label() }</span>
            </div>
        },
    }
}

/// What a message said, with its emotes (links stay text: a spammer's
/// link isn't one to click by mistake).
fn parts(parts: &[AlertPart]) -> Html {
    html! {
        { for parts.iter().map(|part| match part {
            AlertPart::Text { text, .. } => html! { { text.clone() } },
            AlertPart::Emote { code, sprite, .. } => html! {
                <img class="history-emote" src={sprite.image.path()} alt={code.clone()} title={code.clone()} />
            },
        }) }
    }
}

/// An event's summary, without the viewer's own name at its start:
/// "Resubscribed for 2 months". Other names in it open their cards.
fn event_text(
    entry: &HistoryEntry,
    card: &protocol::viewers::ViewerCard,
    open_viewer: &Callback<NameColor>,
) -> Html {
    let summary = entry.summary.as_str();
    let rest = [card.name.as_str(), card.login.as_str()]
        .into_iter()
        .filter(|name| !name.is_empty())
        .find_map(|name| {
            summary.get(..name.len()).filter(|start| start.eq_ignore_ascii_case(name))?;
            summary[name.len()..].strip_prefix(' ')
        });
    match rest {
        Some(rest) => {
            let mut chars = rest.chars();
            let capital: String =
                chars.next().into_iter().flat_map(char::to_uppercase).chain(chars).collect();
            let others: Vec<NameColor> =
                entry.names.iter().filter(|name| name.id != card.id).cloned().collect();
            util::with_names(&capital, &others, Some(open_viewer))
        }
        None => util::with_names(summary, &entry.names, Some(open_viewer)),
    }
}
