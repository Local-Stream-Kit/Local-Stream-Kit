//! Status: boxes that fold to their head like the plugins' (2026-10-14),
//! each head saying how it's going. Twitch (the channel, chat, the login,
//! what the login listens for), Overlays, the control API, Emotes (with
//! their sources and cache), System (the port and the log), and About (the
//! version and updates, the licence, support). A newer version shows as a
//! notice on top.

use gloo::timers::callback::Timeout;
use protocol::config::EMOTE_SOURCES;
use protocol::panel::{ChatStatus, LoginStatus, PanelRequest, Status};
use protocol::update::{self, AppLink, UpdateStatus};
use yew::prelude::*;

use super::boxes::{FoldBox, state_line, summary};
use super::control::ControlBox;
use super::icon;
use super::login::LoginControl;
use super::overlays::OverlaysBox;
use crate::Panel;
use crate::editor::fields::TextField;
use crate::util;

#[derive(Properties, PartialEq)]
pub struct StatusViewProps {
    /// The box a link opened it at: `twitch` (`#/login`) or `control`
    /// (`#/control`), the pages they were until 2026-10-14.
    #[prop_or_default]
    pub focus: Option<AttrValue>,
}

#[function_component]
pub fn StatusView(props: &StatusViewProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let status = &panel.model.status;
    let focus = props.focus.as_deref();
    html! {
        <section class="page">
            if !status.restart_needed.is_empty() {
                <div class="notice warn" role="status">
                    { icon("warn") }
                    <div>
                        <strong>{ "Restart needed" }</strong>
                        <div class="small">{ format!("Changed: {}. Restart Local Stream Kit for these to take effect.", status.restart_needed.join(", ")) }</div>
                    </div>
                </div>
            }
            <UpdateNotice />
            <TwitchBox focused={focus == Some("twitch")} />
            <OverlaysBox />
            <ControlBox focused={focus == Some("control")} />
            <EmotesBox />
            <SystemBox />
            <AboutBox />
        </section>
    }
}

/// A settings row: the label, then the control (`tall`: the label stays on
/// the control's first line).
fn row(id: &str, label: &str, tall: bool, control: Html) -> Html {
    html! {
        <div class={classes!("setting-row", tall.then_some("wraps"))}>
            <label class="setting-label" for={id.to_string()}>{ label.to_string() }</label>
            <div class="setting-control">{ control }</div>
        </div>
    }
}

/// What the Twitch box's head says.
fn twitch_summary(status: &Status) -> Html {
    let listening =
        matches!(status.login, LoginStatus::LoggedIn { listening: true, .. } | LoginStatus::TwitchCli { .. });
    match (&status.chat, &status.login) {
        (ChatStatus::Off, _) => summary("off", "No channel"),
        (ChatStatus::Connecting { .. }, _) => summary("warn", "Connecting…"),
        (_, LoginStatus::Waiting { .. }) => summary("accent", "Waiting for approval"),
        (_, LoginStatus::LoggedIn { listening: false, .. } | LoginStatus::Checking) => {
            summary("warn", "Logging in…")
        }
        _ if listening => summary("ok", "Connected"),
        _ => summary("off", "Chat only"),
    }
}

#[derive(Properties, PartialEq)]
struct TwitchBoxProps {
    focused: bool,
}

/// The channel, chat, the login, and what the login listens for.
#[function_component]
fn TwitchBox(props: &TwitchBoxProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let Some(config) = &panel.model.config else { return html! {} };
    let status = &panel.model.status;
    // Applied when the field is left or Enter is pressed, not per letter:
    // chat switches channel on every change.
    let on_channel = {
        let panel = panel.clone();
        Callback::from(move |typed: String| {
            let channel = util::channel_name(&typed);
            panel.edit(None, move |config| config.connections.twitch_channel = channel.clone());
        })
    };
    let chat = match &status.chat {
        ChatStatus::Off => state_line("status-chat", "off", "No channel set"),
        ChatStatus::Connecting { .. } => state_line("status-chat", "warn", "Connecting…"),
        ChatStatus::Connected { .. } => state_line("status-chat", "ok", "Connected"),
    };
    // What the login listens for (Twitch can refuse follows or hype trains
    // alone); nothing while there's no login.
    let events: Vec<(&str, &str, Html)> = match &status.login {
        LoginStatus::LoggedIn { listening: false, .. } => {
            let connecting = |id: &str| state_line(id, "warn", "Connecting…");
            vec![
                ("status-channel-points", "Channel points", connecting("status-channel-points")),
                ("status-follows", "Follows", connecting("status-follows")),
                ("status-hype-train", "Hype train", connecting("status-hype-train")),
            ]
        }
        LoginStatus::LoggedIn { listening: true, follows, hype_train, .. } => {
            let heard = |id: &str, on: bool| {
                if on {
                    state_line(id, "ok", "Listening")
                } else {
                    state_line(id, "warn", "Twitch refused; see the log")
                }
            };
            let rewards = status.rewards.len();
            vec![
                (
                    "status-channel-points",
                    "Channel points",
                    state_line(
                        "status-channel-points",
                        "ok",
                        format!("Listening · {rewards} reward{}", if rewards == 1 { "" } else { "s" }),
                    ),
                ),
                ("status-follows", "Follows", heard("status-follows", *follows)),
                ("status-hype-train", "Hype train", heard("status-hype-train", *hype_train)),
            ]
        }
        LoginStatus::TwitchCli { .. } => ["status-channel-points", "status-follows", "status-hype-train"]
            .into_iter()
            .zip(["Channel points", "Follows", "Hype train"])
            .map(|(id, label)| (id, label, state_line(id, "ok", "From the Twitch CLI")))
            .collect(),
        _ => Vec::new(),
    };
    // Open when it needs a look: no channel, or a login waiting for its
    // code to be approved.
    let needs_look =
        matches!(status.chat, ChatStatus::Off) || matches!(status.login, LoginStatus::Waiting { .. });
    html! {
        <FoldBox id="status-twitch" title="Twitch" summary={twitch_summary(status)}
                 open={props.focused || needs_look}>
            <div class="setting-rows">
                { row("conn-channel", "Channel", false, html! {
                    <TextField id="conn-channel" placeholder="Your Twitch name"
                               value={config.connections.twitch_channel.clone()} onchange={on_channel} />
                }) }
                { row("status-chat", "Chat", false, chat) }
                { row("status-login", "Login", true, html! { <LoginControl /> }) }
                { for events.into_iter().map(|(id, label, state)| row(id, label, false, state)) }
            </div>
        </FoldBox>
    }
}

/// The emotes: the global ones, Twitch's, and each third-party source's
/// (switched here, after a restart), the image cache, and Refresh.
#[function_component]
fn EmotesBox() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    // Shows "Loading…" for a moment after the button; the counts change
    // when the sets come in.
    let resyncing = use_state(|| None::<Timeout>);
    let Some(config) = &panel.model.config else { return html! {} };
    let status = &panel.model.status;
    let connections = &config.connections;
    let on_resync = {
        let (panel, resyncing) = (panel.clone(), resyncing.clone());
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::ResyncEmotes);
            let done = resyncing.clone();
            resyncing.set(Some(Timeout::new(3_000, move || done.set(None))));
        })
    };
    let on_cache = {
        let panel = panel.clone();
        Callback::from(move |event: InputEvent| {
            if let Ok(mb) = util::value_of(&event).trim().parse() {
                panel.edit(Some("image_cache_max_mb"), move |config| {
                    config.connections.image_cache_max_mb = mb
                });
            }
        })
    };
    let count_of =
        |source: &str| status.emotes.iter().find(|count| count.source == source).map(|count| count.count);
    let count = |n: Option<usize>| {
        html! {
            <span class="mono emote-count">{ n.map_or_else(|| "–".to_string(), |n| n.to_string()) }</span>
        }
    };
    let total = status.global_emotes + status.emotes.iter().map(|count| count.count).sum::<usize>();
    html! {
        <FoldBox id="status-emotes" title="Emotes" summary={summary("", total.to_string())}>
            <div class="setting-rows">
                { row("status-emotes-global", "Global", false, html! {
                    <div class="state-line emote-line" id="status-emotes-global">{ count(Some(status.global_emotes)) }</div>
                }) }
                { row("status-emotes-twitch", "Twitch", false, html! {
                    <div class="state-line emote-line" id="status-emotes-twitch">{ count(count_of("twitch")) }</div>
                }) }
                { for EMOTE_SOURCES.iter().map(|source| {
                    let on = connections.emote_sources.iter().any(|s| s == source);
                    let id = format!("conn-source-{source}");
                    let onchange = {
                        let panel = panel.clone();
                        Callback::from(move |event: Event| {
                            let on = util::checked_of(&event);
                            panel.edit(None, move |config| {
                                let sources = &mut config.connections.emote_sources;
                                sources.retain(|s| s != source);
                                if on {
                                    sources.push(source.to_string());
                                    // Keep the usual order.
                                    sources.sort_by_key(|s| EMOTE_SOURCES.iter().position(|known| known == s));
                                }
                            });
                        })
                    };
                    let label = match *source {
                        "7tv" => "7TV",
                        "bttv" => "BTTV",
                        "ffz" => "FFZ",
                        other => other,
                    };
                    row(&id, label, false, html! {
                        <div class="state-line">
                            <input id={id.clone()} type="checkbox" checked={on} {onchange}
                                   aria-label={format!("Load {label}'s emotes")} />
                            { count(count_of(source)) }
                        </div>
                    })
                }) }
                { row("conn-cache", "Image cache", false, html! {
                    <div class="number-row">
                        <input id="conn-cache" class="num" type="number" min="16"
                               value={connections.image_cache_max_mb.to_string()} oninput={on_cache} />
                        <span class="number-caption">{ "MB" }</span>
                    </div>
                }) }
                <div class="setting-row">
                    <span></span>
                    <div class="setting-control">
                        <div>
                            <button type="button" class="button ghost small" id="status-resync-emotes"
                                    disabled={resyncing.is_some()} onclick={on_resync}>
                                { icon("refresh") }{ if resyncing.is_some() { "Loading…" } else { "Refresh" } }
                            </button>
                        </div>
                    </div>
                </div>
            </div>
        </FoldBox>
    }
}

/// The app itself: its port, and the log (detailed or not, its files, and
/// the warnings and errors since it started).
#[function_component]
fn SystemBox() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let Some(config) = &panel.model.config else { return html! {} };
    let log = &panel.model.status.log;
    let on_port = {
        let panel = panel.clone();
        Callback::from(move |event: InputEvent| {
            if let Ok(port) = util::value_of(&event).trim().parse() {
                panel.edit(Some("overlay_port"), move |config| config.connections.overlay_port = port);
            }
        })
    };
    let on_detailed = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            panel.edit(None, move |config| config.log.detailed = on);
        })
    };
    let on_open_logs = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::OpenLogs);
        })
    };
    let on_clear_log = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::ClearLogLines);
        })
    };
    let errors = log.recent.iter().filter(|line| line.error).count();
    let warnings = log.recent.len() - errors;
    let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let head = if errors > 0 {
        summary("error", plural(errors, "error"))
    } else if warnings > 0 {
        summary("warn", plural(warnings, "warning"))
    } else {
        html! {}
    };
    html! {
        <FoldBox id="status-log" title="System" summary={head} open={errors > 0}>
            <div class="setting-rows">
                { row("conn-port", "Port", false, html! {
                    <div class="number-row">
                        <input id="conn-port" class="num" type="number" min="1" max="65535"
                               value={config.connections.overlay_port.to_string()} oninput={on_port} />
                    </div>
                }) }
                { row("log-detailed", "Detailed log", true, html! {
                    <>
                        <div class="check-field">
                            <input id="log-detailed" type="checkbox" checked={config.log.detailed}
                                   disabled={log.rust_log.is_some()} onchange={on_detailed}
                                   aria-label="Detailed log: every Twitch and OBS message and plugin debug line, for finding problems" />
                        </div>
                        if let Some(rust_log) = &log.rust_log {
                            <span class="muted small">{ format!("RUST_LOG is set ({rust_log}), so it decides what's logged.") }</span>
                        }
                    </>
                }) }
                { row("log-open", "Log files", false, html! {
                    <div class="state-line">
                        <button type="button" class="button ghost small" id="log-open" title={log.folder.clone()}
                                onclick={on_open_logs}>{ icon("folder") }{ "Open folder" }</button>
                        <span class="muted small log-folder">{ "A file a day, 7 kept" }</span>
                    </div>
                }) }
                { row("log-recent", "Warnings", true, html! {
                    if log.recent.is_empty() {
                        <span class="muted small state-line" id="log-recent-none">{ "None since the app started" }</span>
                    } else {
                        <>
                            <ul class="log-lines" id="log-recent">
                                { for log.recent.iter().map(|line| html! {
                                    <li class={classes!("log-line", line.error.then_some("error"))}>
                                        <span class={classes!("dot", if line.error { "error" } else { "warn" })}></span>
                                        <span class="mono small muted">{ util::time(line.at_ms) }</span>
                                        <span class="small">
                                            { &line.text }
                                            if line.count > 1 { <span class="muted">{ format!(" ×{}", line.count) }</span> }
                                        </span>
                                    </li>
                                }) }
                            </ul>
                            <div>
                                <button type="button" class="button ghost small" id="log-clear" onclick={on_clear_log}>{ "Clear" }</button>
                            </div>
                        </>
                    }
                }) }
            </div>
        </FoldBox>
    }
}

/// A link to one of the app's pages, opened in the browser of the PC the
/// app runs on (as viewers' links are): in an OBS dock it would open
/// inside OBS.
fn app_link(panel: &Panel, id: &str, link: AppLink, href: String, text: &str) -> Html {
    let onclick = {
        let panel = panel.clone();
        Callback::from(move |event: MouseEvent| {
            event.prevent_default();
            panel.send(PanelRequest::OpenAppLink { link });
        })
    };
    html! {
        <a class="app-link" id={id.to_string()} href={href.clone()} target="_blank" rel="noopener noreferrer"
           title={format!("{href} (opens in your browser)")} {onclick}>
            { text.to_string() }{ icon("external") }
        </a>
    }
}

/// `text` with a capital first letter: errors come as "couldn't …".
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// A newer version: "Version 0.2.0 is out", What's new and Download, then
/// how the download goes. Nothing while there's none.
#[function_component]
fn UpdateNotice() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let update = &panel.model.status.update;
    let Some(version) = update.version() else { return html! {} };
    let whats_new = app_link(
        &panel,
        "update-whats-new",
        AppLink::WhatsNew,
        update::release_url(&update::releases(), version),
        "What's new",
    );
    let download = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::DownloadUpdate);
        })
    };
    let button = |label: &str| {
        html! {
            <button type="button" class="button small primary" id="update-download" onclick={download.clone()}>
                { icon("download") }{ label.to_string() }
            </button>
        }
    };
    let (class, mark, text, action) = match update {
        UpdateStatus::Downloading { percent, .. } => (
            "info",
            "download",
            html! {
                <>
                    <strong>{ format!("Downloading version {version}… {percent}%") }</strong>
                    <div class="progress"><div class="progress-bar" style={format!("width: {percent}%")}></div></div>
                </>
            },
            html! {},
        ),
        UpdateStatus::Ready { .. } => (
            "info",
            "check",
            html! {
                <>
                    <strong>{ format!("Version {version} is ready") }</strong>
                    <div class="small">{ "It starts the next time you start the app. " }{ whats_new }</div>
                </>
            },
            html! {},
        ),
        UpdateStatus::Failed { error, .. } => (
            "warn",
            "warn",
            html! {
                <>
                    <strong>{ format!("The download of version {version} failed") }</strong>
                    <div class="small update-error">{ capitalised(error) }</div>
                    <div class="small">{ whats_new }</div>
                </>
            },
            button("Try again"),
        ),
        UpdateStatus::CantWrite { folder, .. } => (
            "warn",
            "warn",
            html! {
                <>
                    <strong>{ format!("Version {version} is out") }</strong>
                    <div class="small update-error">
                        { format!("The app can't replace itself in {folder}: download it from the release page.") }
                    </div>
                    <div class="small">{ whats_new }</div>
                </>
            },
            html! {},
        ),
        _ => (
            "info",
            "download",
            html! {
                <>
                    <strong>{ format!("Version {version} is out") }</strong>
                    <div class="small">{ whats_new }</div>
                </>
            },
            button("Download"),
        ),
    };
    html! {
        <div class={classes!("notice", "with-action", "update-notice", class)} id="update-notice" role="status">
            { icon(mark) }
            <div class="notice-text">{ text }</div>
            { action }
        </div>
    }
}

/// The version and the update check (its switch), the licence with its
/// `Required Notice:` line, and "Support the project".
#[function_component]
fn AboutBox() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let Some(config) = &panel.model.config else { return html! {} };
    let status = &panel.model.status;
    let on_check = {
        let panel = panel.clone();
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            panel.edit(None, move |config| config.updates.check = on);
        })
    };
    let running = &status.version;
    let (dot, state, error) = match &status.update {
        UpdateStatus::Off => ("off", "Not checked".to_string(), None),
        UpdateStatus::Development => ("off", "A development build, not checked".to_string(), None),
        UpdateStatus::Checking => ("warn", "Checking…".to_string(), None),
        UpdateStatus::UpToDate => ("ok", "Up to date".to_string(), None),
        UpdateStatus::CheckFailed { error } => ("warn", "Couldn't check".to_string(), Some(error.clone())),
        UpdateStatus::Ready { version } => ("accent", format!("{version} starts next time"), None),
        other => ("accent", format!("{} is out", other.version().unwrap_or_default()), None),
    };
    // At a glance: the version, or what's new about it.
    let head = match &status.update {
        UpdateStatus::Off | UpdateStatus::Development | UpdateStatus::Checking | UpdateStatus::UpToDate => {
            summary("", running.clone())
        }
        _ => summary(dot, state.clone()),
    };
    html! {
        <FoldBox id="status-about" title="About" summary={head}>
            <div class="setting-rows">
                { row("about-version", "Version", true, html! {
                    <div class="about-lines">
                        <div class="state-line" id="about-version">
                            <span class={classes!("dot", dot)}></span>
                            <span><span class="mono">{ running.clone() }</span>{ " · " }{ state }</span>
                        </div>
                        if let Some(error) = error {
                            <span class="muted small update-error" id="about-check-error">{ capitalised(&error) }</span>
                        }
                    </div>
                }) }
                { row("updates-check", "Updates", true, html! {
                    <div class="check-field">
                        <input id="updates-check" type="checkbox" checked={config.updates.check} onchange={on_check}
                               aria-label="Check for updates" />
                        <span class="muted small">{ "Check for updates on startup and every 12 hours" }</span>
                    </div>
                }) }
                { row("about-licence", "Licence", true, html! {
                    <div class="about-lines">
                        { app_link(&panel, "about-licence", AppLink::Licence, update::licence_url(), update::LICENCE) }
                        <span class="muted small" id="about-notice">{ update::REQUIRED_NOTICE }</span>
                    </div>
                }) }
                { row("about-support", "Support", false, html! {
                    <div class="state-line">
                        { app_link(&panel, "about-support", AppLink::Support, update::SUPPORT_URL.to_string(), "Support the project") }
                    </div>
                }) }
            </div>
        </FoldBox>
    }
}
