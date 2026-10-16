//! Activity: Find a viewer and the viewer's card on top (`viewers`), the
//! queue (what's on stage, what's up next; pause, skip what's on stage or
//! any one waiting, play any one waiting next, clear all or one kind; the
//! master volume and mute), and the history of events below it, each with
//! what it caused and a Replay, and a "Seen up to here" line when it's on.
//! One look answers "did that sub's alert play yet?" Every viewer's name
//! opens their card.

use std::collections::HashSet;
use std::rc::Rc;

use gloo::timers::callback::Timeout;
use protocol::AlertPart;
use protocol::activity::{
    EventKind, HistoryEntry, HistoryFilter, ItemOrigin, ItemState, NameColor, QueueAction, QueueGroup,
    QueueItem, QueueState,
};
use protocol::panel::{LinkFrom, PanelRequest};
use protocol::plugin::TextPart;
use protocol::text_style::TextStyle;
use yew::prelude::*;

use super::icon;
use crate::editor::fields::NumberField;
use crate::route::Route;
use crate::{Panel, util};

/// History entries listed at first, and added by "Show more".
const PAGE: usize = 100;

#[function_component]
pub fn Activity() -> Html {
    html! {
        <section class="page">
            <super::viewers::FindViewer />
            <super::viewers::ViewerBox />
            <Queue />
            <History />
        </section>
    }
}

/// Asks once more before something that can't be undone; the question
/// goes away by itself.
#[hook]
pub(crate) fn use_confirm() -> UseStateHandle<bool> {
    let confirm = use_state(|| false);
    {
        let reset = confirm.clone();
        use_effect_with(*confirm, move |asking| {
            let timeout = asking.then(|| Timeout::new(4_000, move || reset.set(false)));
            move || drop(timeout)
        });
    }
    confirm
}

#[function_component]
fn Queue() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let confirm = use_confirm();
    // The chip picked: the list shows only that kind, and Clear takes only it.
    let picked = use_state(|| None::<QueueGroup>);
    let queue = panel.model.queue.clone();
    // While the pointer is over the lists they hold still (the queue as it
    // was when it came), so a row doesn't move under it as the queue moves
    // on; it catches up when the pointer leaves. Rows that leave the queue
    // meanwhile (skipped here too) stay, faded, as taking them out would
    // move the rest; new items join at the end, where they move nothing.
    // Rows played next from here stay in place too, their ⤒ lit.
    let held = use_state(|| None::<Rc<QueueState>>);
    let skipped_here = use_state(HashSet::<u64>::new);
    let next_here = use_state(Vec::<u64>::new);
    // What preparing the waiting events again last changed, until closed;
    // meanwhile the rows it changed flash as they show.
    let note = panel.model.queue_note;
    let changed = if note.is_some() { panel.model.queue_changed.clone() } else { Rc::default() };
    let on_close_note = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| panel.close_queue_note())
    };
    let on_enter = {
        let held = held.clone();
        let queue = queue.clone();
        let next_here = next_here.clone();
        Callback::from(move |_: MouseEvent| {
            held.set(Some(queue.clone()));
            next_here.set(Vec::new());
        })
    };
    let on_leave = {
        let held = held.clone();
        let skipped_here = skipped_here.clone();
        let next_here = next_here.clone();
        Callback::from(move |_: MouseEvent| {
            held.set(None);
            skipped_here.set(HashSet::new());
            next_here.set(Vec::new());
        })
    };
    let shown = match &*held {
        Some(held) => {
            let known: HashSet<u64> = held.on_stage.iter().chain(&held.waiting).map(|item| item.id).collect();
            let mut shown = (**held).clone();
            let new: Vec<&QueueItem> = queue
                .on_stage
                .iter()
                .chain(&queue.waiting)
                .filter(|item| !known.contains(&item.id))
                .collect();
            shown.waiting_count += new.len();
            for item in new {
                *shown.by_group.entry(item.group).or_insert(0) += 1;
                shown.waiting.push(item.clone());
            }
            Rc::new(shown)
        }
        None => queue.clone(),
    };
    let muted = panel.model.config.as_ref().is_some_and(|config| config.effects.sound.muted);
    // A kind with nothing left waiting isn't picked any more.
    let only = picked.filter(|group| queue.by_group.contains_key(group));
    let action = |action: QueueAction| {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::Queue { action, only: None });
        })
    };
    // By id: a click that lands after the item has gone skips nothing.
    let skip = |id: u64| {
        let panel = panel.clone();
        let skipped_here = skipped_here.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::SkipItem { id });
            let mut skipped = (*skipped_here).clone();
            skipped.insert(id);
            skipped_here.set(skipped);
        })
    };
    let on_skip = queue.on_stage.as_ref().map(|item| skip(item.id));
    // By id too: one that has gone, or gone on stage, stays where it is.
    let play_next = |id: u64| {
        let panel = panel.clone();
        let next_here = next_here.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::PlayNext { id });
            let mut next = (*next_here).clone();
            next.retain(|other| *other != id);
            next.push(id);
            next_here.set(next);
        })
    };
    let on_clear = {
        let panel = panel.clone();
        let confirm = confirm.clone();
        Callback::from(move |_: MouseEvent| {
            if *confirm {
                panel.send(PanelRequest::Queue { action: QueueAction::Clear, only });
            }
            confirm.set(!*confirm);
        })
    };
    let waiting = queue.waiting_count;
    let clearing = only.map_or(waiting, |group| queue.by_group.get(&group).copied().unwrap_or(0));
    let what = match only {
        Some(group) => noun(group, clearing),
        None => "waiting",
    };
    let name_of = |plugin: &String| {
        let catalog = &panel.model.plugins;
        catalog.plugin(plugin).map_or(plugin.clone(), |info| info.manifest.name.clone())
    };
    let waiting_for = queue
        .waiting_for_page
        .as_ref()
        .map(|plugin| format!("Waiting for an overlay showing the {} plugin's page", name_of(plugin)))
        .or_else(|| {
            // Its server code plays it: switched off or stopped.
            let plugin = queue.waiting_for_plugin.as_ref()?;
            Some(format!("Waiting for the {} plugin to run", name_of(plugin)))
        });
    let (dot, state) = if queue.paused {
        ("warn", "Paused")
    } else if let Some(waiting_for) = &waiting_for {
        ("warn", waiting_for.as_str())
    } else if queue.on_stage.is_some() {
        ("ok", "Playing")
    } else if queue.players == 0 && waiting > 0 {
        ("warn", "Waiting for an overlay with alerts")
    } else {
        ("ok", "Ready")
    };
    let chip = |group: Option<QueueGroup>, label: String| {
        let pressed = only == group;
        let picked = picked.clone();
        let confirm = confirm.clone();
        let onclick = Callback::from(move |_: MouseEvent| {
            picked.set(group);
            confirm.set(false);
        });
        let id = format!("queue-only-{}", group.map_or("all", QueueGroup::name));
        html! {
            <button type="button" {id} class={classes!("filter", pressed.then_some("on"))} aria-pressed={pressed.to_string()} {onclick}>
                { label }
            </button>
        }
    };
    // Held: what's still in the queue, to tell the rows that have gone.
    let live: HashSet<u64> = queue.on_stage.iter().chain(&queue.waiting).map(|item| item.id).collect();
    // Links open in the PC's browser, not in OBS.
    let open = |id: u64| {
        let panel = panel.clone();
        Callback::from(move |url: String| {
            panel.send(PanelRequest::OpenLink { from: LinkFrom::Item(id), url });
        })
    };
    let first = queue.waiting.first().map(|item| item.id);
    let open_viewer = super::viewers::opener(&panel);
    let row = |item: &QueueItem, waiting: bool| {
        let gone = !live.contains(&item.id);
        let look = if gone {
            Look::Gone
        } else if changed.contains(&item.id) {
            Look::Changed
        } else {
            Look::Waiting
        };
        let next = if next_here.contains(&item.id) {
            NextButton::Moved
        } else if first == Some(item.id) {
            NextButton::First
        } else {
            NextButton::Ready
        };
        let buttons =
            (waiting && !gone).then(|| RowButtons { next: (play_next(item.id), next), skip: skip(item.id) });
        item_view(item, buttons, look, &open(item.id), &open_viewer)
    };
    let shown_total = shown.waiting_count;
    let listed: Vec<&QueueItem> =
        shown.waiting.iter().filter(|item| only.is_none_or(|group| item.group == group)).collect();
    let shown_count = only.map_or(shown_total, |group| shown.by_group.get(&group).copied().unwrap_or(0));
    // The queue moved on while held (not counting what was skipped or
    // played next here).
    let ids = |state: &QueueState| {
        let mut waiting: Vec<u64> =
            state.waiting.iter().map(|item| item.id).filter(|id| !skipped_here.contains(id)).collect();
        for id in next_here.iter() {
            if let Some(at) = waiting.iter().position(|other| other == id) {
                let id = waiting.remove(at);
                waiting.insert(0, id);
            }
        }
        (state.on_stage.as_ref().map(|item| item.id), waiting)
    };
    let moved = held.is_some() && ids(&shown) != ids(&queue);
    html! {
        <div class="panel-box queue">
            <div class="box-head">
                <span class="box-title">{ "Queue" }</span>
                if muted {
                    <span class="tag" id="queue-muted" title="Everything plays on, silently">{ "Muted" }</span>
                }
                <span class="queue-state" id="queue-state"><span class={classes!("dot", dot)}></span>{ state }</span>
            </div>
            if dot == "warn" && !queue.paused {
                <a class="small" id="queue-overlays" href={Route::Status.href()}>{ "Overlays are on Status" }</a>
            }
            <div class="button-row">
                if queue.paused {
                    <button type="button" id="queue-resume" class="button small primary" onclick={action(QueueAction::Resume)}>
                        { icon("play") }{ "Resume" }
                    </button>
                } else {
                    <button type="button" id="queue-pause" class="button small ghost" onclick={action(QueueAction::Pause)}
                            title="What's on stage finishes; the rest waits">
                        { icon("pause") }{ "Pause" }
                    </button>
                }
                <button type="button" id="queue-skip" class="button small ghost" onclick={on_skip}
                        disabled={queue.on_stage.is_none()} title="End what's on stage now">
                    { icon("skip") }{ "Skip" }
                </button>
                <button type="button" id="queue-clear" class={classes!("button", "small", if *confirm { "danger" } else { "ghost" })}
                        onclick={on_clear} disabled={clearing == 0}
                        title={if only.is_some() { "Drop what's waiting of this kind" } else { "Drop everything waiting" }}>
                    { match (*confirm, only) {
                        (true, _) => format!("Drop {clearing} {what}?"),
                        (false, Some(group)) => format!("Clear {}", group.plural().to_lowercase()),
                        (false, None) => "Clear".to_string(),
                    } }
                </button>
                <super::sounds::MasterVolume id="queue" compact=true />
            </div>
            if let Some(note) = note {
                <div class="notice queue-note" role="status" id="queue-note">
                    <p class="note-lines">{ note.note() }</p>
                    <button type="button" class="queue-skip-one" id="queue-note-close" title="Close"
                            aria-label="Close" onclick={on_close_note}>{ "×" }</button>
                </div>
            }
            <div class="queue-lists" id="queue-lists" onmouseenter={on_enter} onmouseleave={on_leave}>
                <div class="queue-section">
                    <span class="queue-heading">{ "On stage" }</span>
                    { match &shown.on_stage {
                        Some(item) => html! { <ul class="queue-list now">{ row(item, false) }</ul> },
                        None => html! { <p class="muted small">{ "Nothing." }</p> },
                    } }
                </div>
                if shown_total > 0 {
                    <div class="queue-section">
                        <span class="queue-heading">{ format!("Up next · {shown_total}") }</span>
                        if shown.by_group.len() > 1 {
                            <div class="filters" role="group" aria-label="Show">
                                { chip(None, "All".to_string()) }
                                { for shown.by_group.iter().map(|(group, count)| chip(Some(*group), format!("{} {count}", group.plural()))) }
                            </div>
                        }
                        <ol class="queue-list">
                            { for listed.iter().map(|item| row(item, true)) }
                        </ol>
                        if shown_count > listed.len() {
                            <p class="muted small">{ format!("and {} more", shown_count - listed.len()) }</p>
                        }
                    </div>
                }
                if moved {
                    <p class="muted small" id="queue-held">{ "The queue has moved on; this list catches up when the pointer leaves it." }</p>
                }
            </div>
        </div>
    }
}

/// "37 follows", "1 follow": what Clear asks about.
fn noun(group: QueueGroup, count: usize) -> &'static str {
    let (one, many) = match group {
        QueueGroup::Sub => ("sub", "subs"),
        QueueGroup::GiftBomb => ("gift bomb", "gift bombs"),
        QueueGroup::Bits => ("cheer", "cheers"),
        QueueGroup::Raid => ("raid", "raids"),
        QueueGroup::Reward => ("reward", "rewards"),
        QueueGroup::Follow => ("follow", "follows"),
        QueueGroup::HypeTrain => ("hype train item", "hype train items"),
        QueueGroup::Support => ("support item", "support items"),
        QueueGroup::Other => ("other item", "other items"),
    };
    if count == 1 { one } else { many }
}

/// How a queue item's row looks.
#[derive(Clone, Copy, PartialEq)]
enum Look {
    Waiting,
    /// Its event was prepared again and it changed: it flashes once.
    Changed,
    /// Held in view after it left the queue (played, cleared): faded.
    Gone,
}

/// A waiting row's buttons: ⤒ (play it next) and ×.
struct RowButtons {
    next: (Callback<MouseEvent>, NextButton),
    skip: Callback<MouseEvent>,
}

/// How a waiting row's ⤒ looks.
#[derive(Clone, Copy, PartialEq)]
enum NextButton {
    Ready,
    /// Moved to the front from this list while it held still: lit.
    Moved,
    /// The first plays next already: hidden, keeping its place, so the
    /// row's text doesn't move when another one becomes first.
    First,
}

/// A queue item: what it shows or says (with its emotes and text styles),
/// and under it what it is (in its
/// colour), whether it's a test or a replay, its event and its card.
/// Links in what it shows or says call `open`.
fn item_view(
    item: &QueueItem,
    buttons: Option<RowButtons>,
    look: Look,
    open: &Callback<String>,
    open_viewer: &Callback<NameColor>,
) -> Html {
    // The built-in plugins' effects get colours of their own.
    let kind = match item.label.as_str() {
        "Alert" => "kind-alert",
        "Speak" => "kind-speech",
        "Sound" => "kind-sound",
        _ => "kind-plugin",
    };
    let card = (!item.card.is_empty()).then_some(item.card.as_str());
    let origin = match item.origin {
        ItemOrigin::Live => None,
        ItemOrigin::Replay => Some("Replay"),
        ItemOrigin::Test => Some("Test"),
    };
    html! {
        <li class={classes!("queue-item", (look == Look::Gone).then_some("gone"), (look == Look::Changed).then_some("changed"))}
            key={item.id} data-item={item.id.to_string()}>
            <div class="queue-text">
                <span class="queue-main">{ looked(&item.text, &item.parts, &item.names, open, open_viewer) }</span>
                <span class="queue-from">
                    <span class={classes!("kind", kind)}>{ item.label.clone() }</span>
                    if let Some(origin) = origin {
                        { format!(" · {origin}") }
                    }
                    if let Some(event) = &item.event {
                        { " · " }{ util::with_names(event, &item.names, Some(open_viewer)) }
                    }
                    if let Some(card) = card {
                        { format!(" · {card}") }
                    }
                </span>
            </div>
            if let Some(RowButtons { next: (onclick, next), skip }) = buttons {
                <button type="button" title="Play this one next" aria-label={format!("Play {} next", item.text)}
                        class={classes!("queue-next", (next == NextButton::Moved).then_some("on"), (next == NextButton::First).then_some("first"))}
                        aria-pressed={(next == NextButton::Moved).to_string()} disabled={next == NextButton::First} {onclick}>
                    { icon("top") }
                </button>
                <button type="button" class="queue-skip-one" title="Skip this one"
                        aria-label={format!("Skip {}", item.text)} onclick={skip}>{ "×" }</button>
            }
        </li>
    }
}

#[function_component]
fn History() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let confirm = use_confirm();
    let filter = use_state(|| None::<EventKind>);
    let shown = use_state(|| PAGE);
    let editing = use_state(|| false);
    let Some(config) = &panel.model.config else { return html! {} };
    let settings = &config.history;
    let history = panel.model.history.clone();
    let visible: Vec<&HistoryEntry> = history.iter().filter(|entry| settings.shows(entry)).collect();
    let hidden = history.len() - visible.len();
    let kinds: Vec<EventKind> =
        EventKind::ALL.into_iter().filter(|kind| visible.iter().any(|entry| entry.kind == *kind)).collect();
    let entries: Vec<&HistoryEntry> =
        visible.iter().copied().filter(|entry| filter.is_none_or(|kind| entry.kind == kind)).collect();
    // The seen marker: new events above its line, which sits under the
    // oldest new one listed (none when that one is past "Show more").
    let seen = panel.model.history_seen;
    let new = if settings.seen_marker { settings.new_events(visible.iter().copied(), seen) } else { 0 };
    let line_under = if new > 0 {
        entries.iter().rposition(|entry| entry.is_new(seen)).filter(|at| *at < *shown)
    } else {
        None
    };
    let chip = |kind: Option<EventKind>, label: &str| {
        let pressed = *filter == kind;
        let filter = filter.clone();
        let shown = shown.clone();
        let onclick = Callback::from(move |_: MouseEvent| {
            filter.set(kind);
            shown.set(PAGE);
        });
        html! {
            <button type="button" class={classes!("filter", pressed.then_some("on"))} aria-pressed={pressed.to_string()} {onclick}>
                { label.to_string() }
            </button>
        }
    };
    let more = {
        let shown = shown.clone();
        Callback::from(move |_: MouseEvent| shown.set(*shown + PAGE))
    };
    let toggle = {
        let editing = editing.clone();
        Callback::from(move |_: MouseEvent| editing.set(!*editing))
    };
    let count = match history.len() {
        1 => "1 event".to_string(),
        count => format!("{count} events"),
    };
    let on_clear = {
        let panel = panel.clone();
        let confirm = confirm.clone();
        Callback::from(move |_: MouseEvent| {
            if *confirm {
                panel.send(PanelRequest::ClearHistory);
            }
            confirm.set(!*confirm);
        })
    };
    let all_seen = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::MarkSeen { up_to: None });
        })
    };
    let mut rows: Vec<Html> = Vec::new();
    for (at, entry) in entries.iter().take(*shown).enumerate() {
        let seeable = settings.seen_marker && entry.is_new(seen);
        rows.push(
            html! { <Entry key={entry.id} entry={(*entry).clone()} message={settings.messages} {seeable} /> },
        );
        if line_under == Some(at) {
            rows.push(html! {
                <li key="seen-line" class="seen-line" id="history-seen-line">
                    <span class="seen-rule"></span>
                    <span class="seen-words">{ "Seen up to here" }</span>
                    <span class="seen-rule"></span>
                    <button type="button" id="history-all-seen" class="seen-all" onclick={all_seen.clone()}
                            title="Everything in the history is seen">
                        { icon("check") }{ "All seen" }
                    </button>
                </li>
            });
        }
    }
    // Open, the Filter button looks pressed; closed, it's lit up while it
    // hides events.
    let filter_look = if *editing {
        "pressed"
    } else if settings.is_default() {
        "ghost"
    } else {
        "primary"
    };
    html! {
        <div class="panel-box history">
            <div class="box-head">
                <span class="box-title">{ "History" }</span>
                if new > 0 {
                    <span class="history-new" id="history-new">{ format!("{new} new") }</span>
                }
                <span class="muted small">{ count.clone() }</span>
                <button type="button" id="history-filter" onclick={toggle} aria-expanded={editing.to_string()}
                        class={classes!("button", "small", filter_look)} title="Which events to show">
                    { "Filter" }
                </button>
                <button type="button" id="history-clear" onclick={on_clear} disabled={history.is_empty()}
                        class={classes!("button", "small", if *confirm { "danger" } else { "ghost" })}
                        title="Forget every event in the history; the queue isn't touched">
                    { if *confirm { format!("Forget {count}?") } else { "Clear".to_string() } }
                </button>
            </div>
            if *editing {
                <HistorySettings />
            }
            if kinds.len() > 1 {
                <div class="filters" role="group" aria-label="Show">
                    { chip(None, "All") }
                    { for kinds.iter().map(|kind| chip(Some(*kind), kind.plural())) }
                </div>
            }
            if history.is_empty() {
                <p class="muted small">{ "Nothing yet. Subs, gift bombs, bits, raids and rewards show up here as they happen (test events don't), with what they set off." }</p>
            } else if hidden > 0 {
                <p class="muted small" id="history-hidden">{ match hidden {
                    1 => "1 event hidden by the filter".to_string(),
                    hidden => format!("{hidden} events hidden by the filter"),
                } }</p>
            }
            <ul class="history-list">
                { for rows }
            </ul>
            if entries.len() > *shown {
                <div>
                    <button type="button" class="button ghost small" onclick={more}>
                        { format!("Show more ({} left)", entries.len() - *shown) }
                    </button>
                </div>
            }
        </div>
    }
}

type Show = fn(&mut HistoryFilter, bool);
type Min = fn(&mut HistoryFilter, u64);

/// A settings row: the label, then the control.
fn row(id: &str, label: &str, control: Html) -> Html {
    html! {
        <div class="setting-row">
            <label class="setting-label" for={id.to_string()}>{ label.to_string() }</label>
            <div class="setting-control">{ control }</div>
        </div>
    }
}

/// A tick box that changes the History's settings.
fn tick(panel: &Panel, id: &str, on: bool, set: Show) -> Html {
    let onchange = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            panel.edit(None, move |config| set(&mut config.history, on));
        })
    };
    html! { <input type="checkbox" id={id.to_string()} checked={on} {onchange} /> }
}

/// One kind of event: shown or not, and the smallest shown.
fn kind_row(
    panel: &Panel,
    key: &'static str,
    label: &str,
    on: bool,
    show: Show,
    min: Option<(u64, &str, Min)>,
) -> Html {
    let id = format!("history-show-{key}");
    let min = min.map(|(value, unit, set)| {
        let panel = panel.clone();
        let onchange = Callback::from(move |number: f64| {
            let number = if number.is_finite() { number.clamp(0.0, 1e9) as u64 } else { 0 };
            panel.edit(Some(&format!("history-min-{key}")), move |config| set(&mut config.history, number));
        });
        html! {
            <>
                <span class="number-caption min-word">{ "at least" }</span>
                <NumberField id={format!("history-min-{key}")} value={value as f64} {onchange} />
                <span class="number-caption">{ unit.to_string() }</span>
            </>
        }
    });
    row(
        &id,
        label,
        html! {
            <div class={classes!("number-row", "history-kind", (!on).then_some("off"))}>
                { tick(panel, &id, on, show) }
                { min.unwrap_or_default() }
            </div>
        },
    )
}

/// The History's settings, in the box between two lines: messages in the
/// list, the seen marker, and which events show (kinds, and the smallest
/// of those with a size). Saved in the config, so every panel shows the
/// same.
#[function_component]
fn HistorySettings() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let Some(config) = &panel.model.config else { return html! {} };
    let settings = config.history.clone();
    let reset = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| panel.edit(None, |config| config.history.show_everything()))
    };
    // Switched on, the line starts above everything there is.
    let on_marker = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            if on {
                panel.send(PanelRequest::MarkSeen { up_to: None });
            }
            panel.edit(None, move |config| config.history.seen_marker = on);
        })
    };
    html! {
        <div class="history-settings" id="history-settings">
            <div class="setting-rows">
                { row("history-messages", "Messages", html! {
                    <div class="check-field">
                        <input type="checkbox" id="history-messages" checked={settings.messages}
                               aria-label="Show messages in the list" title="Show messages in the list"
                               onchange={let panel = panel.clone(); Callback::from(move |event: Event| {
                                   let on = util::checked_of(&event);
                                   panel.edit(None, move |config| config.history.messages = on);
                               })} />
                    </div>
                }) }
                <div class="setting-row wraps">
                    <label class="setting-label" for="history-seen-marker">{ "Seen marker" }</label>
                    <div class="setting-control">
                        <div class="check-field">
                            <input type="checkbox" id="history-seen-marker" checked={settings.seen_marker}
                                   title="A “Seen up to here” line: the events above it are new" onchange={on_marker} />
                        </div>
                        if settings.seen_marker {
                            <div class="check-field wraps">
                                { tick(&panel, "history-seen-at-start", settings.seen_at_stream_start,
                                       |f, on| f.seen_at_stream_start = on) }
                                <label for="history-seen-at-start">{ "Mark all seen at stream start" }</label>
                            </div>
                        }
                    </div>
                </div>
                <span class="history-settings-heading">{ "Events" }</span>
                { kind_row(&panel, "subs", "Subs", settings.subs, |f, on| f.subs = on, None) }
                { kind_row(&panel, "gift_bombs", "Gift bombs", settings.gift_bombs, |f, on| f.gift_bombs = on,
                           Some((settings.min_gift_bomb, "subs", |f, n| f.min_gift_bomb = n))) }
                { kind_row(&panel, "bits", "Bits", settings.bits, |f, on| f.bits = on,
                           Some((settings.min_bits, "bits", |f, n| f.min_bits = n))) }
                { kind_row(&panel, "raids", "Raids", settings.raids, |f, on| f.raids = on,
                           Some((settings.min_raid, "viewers", |f, n| f.min_raid = n))) }
                { kind_row(&panel, "rewards", "Rewards", settings.rewards, |f, on| f.rewards = on, None) }
                { kind_row(&panel, "follows", "Follows", settings.follows, |f, on| f.follows = on, None) }
                { kind_row(&panel, "hype_trains", "Hype trains", settings.hype_trains, |f, on| f.hype_trains = on, None) }
                if !settings.is_default() {
                    <div class="setting-row">
                        <span></span>
                        <div class="setting-control">
                            <div><button type="button" class="button ghost small" onclick={reset}>{ "Show everything" }</button></div>
                        </div>
                    </div>
                }
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct EntryProps {
    entry: HistoryEntry,
    /// Its message shows without opening it.
    message: bool,
    /// Above the seen marker: its ✓ moves the marker under it.
    seeable: bool,
}

#[function_component]
fn Entry(props: &EntryProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let entry = &props.entry;
    let replay = {
        let panel = panel.clone();
        let id = entry.id;
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::Replay { entry: id });
        })
    };
    // In the summary: the click marks it seen, not open.
    let seen_here = {
        let panel = panel.clone();
        let id = entry.id;
        Callback::from(move |event: MouseEvent| {
            event.prevent_default();
            event.stop_propagation();
            panel.send(PanelRequest::MarkSeen { up_to: Some(id) });
        })
    };
    // Links open in the PC's browser, not in OBS.
    let open = {
        let panel = panel.clone();
        let id = entry.id;
        Callback::from(move |url: String| {
            panel.send(PanelRequest::OpenLink { from: LinkFrom::Entry(id), url });
        })
    };
    let open_viewer = super::viewers::opener(&panel);
    let (dot, outcome) = outcome(entry);
    let inline_message = props.message && !entry.message.is_empty();
    // An alert that showed the message shows it, open, as it looked; the
    // quote above steps aside, so it reads once.
    let in_items = message_in_items(entry);
    let time = util::time(entry.at_ms);
    html! {
        <li class={classes!("history-item", props.seeable.then_some("seeable"), in_items.then_some("message-in-items"))}
            data-entry={entry.id.to_string()}>
            <details>
                <summary class={classes!(inline_message.then_some("with-message"))}>
                    <span class="history-time mono">{ time.clone() }</span>
                    <span class="history-summary">
                        if entry.replay_of.is_some() {
                            <span class="tag replay-tag">{ icon("replay") }{ "Replay" }</span>
                        }
                        { util::with_names(&entry.summary, &entry.names, Some(&open_viewer)) }
                    </span>
                    if !outcome.is_empty() {
                        <span class="history-outcome"><span class={classes!("dot", dot)}></span>{ outcome }</span>
                    }
                    // Shown without opening the entry, on the entry's one
                    // line; all of it when it's open.
                    if inline_message {
                        <span class="history-message"><span class="history-message-text">{ message(entry, &open) }</span></span>
                    }
                    if props.seeable {
                        <button type="button" class="seen-here" title="Seen up to here" aria-label="Seen up to here"
                                onclick={seen_here}>{ icon("check") }</button>
                    }
                </summary>
                // Under the summary's words, however wide the time is ("9
                // Oct 21:04" before today): an unseen copy of it holds the
                // place.
                <div class="history-body">
                    <span class="history-time mono history-gutter" aria-hidden="true">{ time }</span>
                    <div class="history-body-main">
                    if !props.message && !entry.message.is_empty() && !in_items {
                        <p class="history-message">{ message(entry, &open) }</p>
                    }
                    if entry.cards.is_empty() {
                        <p class="muted small">{ "No card reacted." }</p>
                    } else {
                        <p class="history-cards">{ cards_set_off(&panel, entry) }</p>
                    }
                    if !entry.items.is_empty() {
                        <ul class="history-items">
                            { for entry.items.iter().map(|item| html! {
                                <li key={item.id} data-state={item.state.label()}>
                                    { looked(&item.text, &item.parts, &entry.names, &open, &open_viewer) }
                                    if item.state != ItemState::Played {
                                        <span class="muted">{ format!(" · {}", item.state.label()) }</span>
                                    }
                                </li>
                            }) }
                        </ul>
                    }
                    <div class="button-row">
                        <button type="button" class="button ghost small replay" onclick={replay}
                                title="Send it through your cards again, as they are now">
                            { icon("replay") }{ "Replay" }
                        </button>
                    </div>
                    </div>
                </div>
            </details>
        </li>
    }
}

/// The cards the entry set off, each a link to its editor while the card
/// is still there. Entries saved before the history kept ids find it by
/// name.
fn cards_set_off(panel: &Panel, entry: &HistoryEntry) -> Html {
    let cards = panel.model.config.as_ref().map(|config| config.cards.as_slice()).unwrap_or_default();
    html! {
        { for entry.cards.iter().enumerate().map(|(n, name)| {
            let link = match entry.card_ids.get(n) {
                Some(id) => cards.iter().find(|card| card.id == *id),
                None => cards.iter().find(|card| card.name == *name),
            }
            .map(|card| Route::Card(card.id.clone()).href());
            html! {
                <>
                    if n > 0 { { ", " } }
                    if let Some(href) = link {
                        <a href={href}>{ name.clone() }</a>
                    } else {
                        { name.clone() }
                    }
                </>
            }
        }) }
    }
}

/// Whether one of the entry's queue items showed or said the viewer's
/// message (an alert's "Thank you … ! {message}").
fn message_in_items(entry: &HistoryEntry) -> bool {
    let message = entry.message.trim();
    !message.is_empty() && entry.items.iter().any(|item| item.text.contains(message))
}

/// A part's text style, as its `style` attribute (its font loaded).
fn style_of(style: Option<&TextStyle>) -> Option<String> {
    let style = style?;
    super::text_styles::load_font(style);
    Some(style.css()).filter(|css| !css.is_empty())
}

/// A queue item's text as it looked: its emotes, and its text styles.
/// Names keep their chat colour, but not in a part whose style has a
/// colour of its own (the alert showed that one). Links call `open`.
fn looked(
    text: &str,
    parts: &[TextPart],
    names: &[NameColor],
    open: &Callback<String>,
    open_viewer: &Callback<NameColor>,
) -> Html {
    if parts.is_empty() {
        return util::with_links(text, names, open, Some(open_viewer));
    }
    html! {
        { for parts.iter().map(|part| match part {
            TextPart::Text { text, style } => {
                let coloured = style.as_ref().is_some_and(|style| !style.colour.is_empty());
                html! {
                    <span style={style_of(style.as_ref())}>
                        { util::with_links(text, if coloured { &[] } else { names }, open, Some(open_viewer)) }
                    </span>
                }
            }
            TextPart::Emote { emote, style } => html! {
                <img class="history-emote" src={emote.url.clone()} alt={emote.code.clone()} title={emote.code.clone()}
                     style={style_of(style.as_ref())} />
            },
        }) }
    }
}

/// How the entry's queue items are doing, in a word or two, when it's
/// worth a look: nothing once they've all played.
fn outcome(entry: &HistoryEntry) -> (&'static str, String) {
    let states: Vec<ItemState> = entry.items.iter().map(|item| item.state).collect();
    if entry.cards.is_empty() {
        ("off", "no card".into())
    } else if states.contains(&ItemState::OnStage) {
        ("ok", "on stage".into())
    } else if states.contains(&ItemState::Waiting) {
        ("warn", "waiting".into())
    } else if let Some(other) = states.iter().find(|state| **state != ItemState::Played) {
        ("off", other.label().into())
    } else {
        ("", String::new())
    }
}

/// The viewer's message, with its emotes. Links call `open`.
fn message(entry: &HistoryEntry, open: &Callback<String>) -> Html {
    if entry.message_parts.is_empty() {
        return util::with_links(&entry.message, &[], open, None);
    }
    html! {
        { for entry.message_parts.iter().map(|part| match part {
            AlertPart::Text { text, style } => html! {
                <span style={style_of(style.as_ref())}>{ util::with_links(text, &[], open, None) }</span>
            },
            AlertPart::Emote { code, sprite, style } => html! {
                <img class="history-emote" src={sprite.image.path()} alt={code.clone()} title={code.clone()}
                     style={style_of(style.as_ref())} />
            },
        }) }
    }
}

#[cfg(test)]
mod tests {
    use protocol::activity::{EventKind, HistoryEntry, ItemRecord, ItemState};

    use super::message_in_items;

    #[test]
    fn an_alert_that_showed_the_message() {
        let item = |text: &str| ItemRecord {
            id: 1,
            label: "Alert".into(),
            text: text.into(),
            parts: Vec::new(),
            state: ItemState::Played,
        };
        let entry = |message: &str, items| HistoryEntry {
            id: 1,
            at_ms: 0,
            kind: EventKind::Sub,
            summary: String::new(),
            amount: 0,
            message: message.into(),
            message_parts: Vec::new(),
            replay_of: None,
            names: Vec::new(),
            cards: Vec::new(),
            card_ids: Vec::new(),
            items,
        };
        let message = "How is the road ? Not too bumpy ?";
        let alert =
            item("Thank you lukilurio for subscribing for 17 months! How is the road ? Not too bumpy ?");
        assert!(message_in_items(&entry(message, vec![alert.clone()])));
        assert!(!message_in_items(&entry(message, vec![item("Thanks!")])), "an alert without it");
        assert!(!message_in_items(&entry("", vec![alert])), "no message");
    }
}
