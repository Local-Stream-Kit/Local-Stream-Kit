//! The Twitch login, in the Status page's Twitch box (its own page until
//! 2026-10-14): Twitch's device code flow, with the link and code, and
//! logging out. Logging in is what turns channel points, follows and hype
//! trains on (there was a switch for them until 2026-10-14), as far as the
//! box's ticks allow; logged in, a tick the login wasn't given offers a
//! second login, which waits for approval while the first goes on working.

use gloo::timers::callback::Interval;
use protocol::config::ClosedNotes;
use protocol::panel::{LoginStatus, PanelRequest};
use yew::prelude::*;

use super::boxes::{CopyField, state_line};
use super::icon;
use crate::Panel;

/// The Login row's control: the login and what can be done with it.
#[function_component]
pub fn LoginControl() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let now = use_state(js_sys::Date::now);
    {
        // Ticks the code's countdown.
        let now = now.clone();
        use_effect_with((), move |_| {
            let interval = Interval::new(1_000, move || now.set(js_sys::Date::now()));
            move || drop(interval)
        });
    }
    let send = |request: PanelRequest| {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(request.clone());
        })
    };
    let channel = panel
        .model
        .config
        .as_ref()
        .map(|config| config.connections.twitch_channel.clone())
        .unwrap_or_default();
    let ticked =
        panel.model.config.as_ref().map(|config| config.connections.twitch_permissions).unwrap_or_default();
    let closed_pair = panel
        .model
        .config
        .as_ref()
        .map(|config| config.closed_notes.login_other_channel.clone())
        .unwrap_or_default();
    let close_other = |pair: String| {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            let pair = pair.clone();
            panel.edit(None, move |config| config.closed_notes.login_other_channel = pair.clone());
        })
    };
    // A login's code, waiting for approval: the first login's, or a second.
    let waiting = |url: &str, code: &str, expires_at_ms: u64, reason: Option<&str>| {
        let minutes = ((expires_at_ms as f64 - *now) / 60_000.0).ceil().max(0.0);
        html! {
            <>
                { state_line("status-login-state", "accent", "Waiting for approval") }
                if let Some(reason) = reason {
                    <span class="small warn-text">{ reason.to_string() }</span>
                }
                <span class="muted small">{ "Log in as the broadcaster, and check Twitch shows this code:" }</span>
                <CopyField id="login-code" value={code.to_string()} what="the code" big=true />
                <div class="button-row">
                    <button type="button" class="button primary small" id="open-login"
                            onclick={send(PanelRequest::OpenLogin)}>{ "Open in your browser" }{ icon("external") }</button>
                    <CopyLink url={url.to_string()} />
                    <button type="button" class="button ghost small" id="login-cancel"
                            onclick={send(PanelRequest::CancelLogin)}>{ "Cancel" }</button>
                </div>
                <span class="muted small">{ format!("A new code in {minutes} minutes") }</span>
            </>
        }
    };
    let log_in = html! {
        <>
            <div>
                <button type="button" class="button primary small" id="login-start"
                        onclick={send(PanelRequest::StartLogin)}>{ "Log in with Twitch" }</button>
            </div>
            <span class="muted small">{ "With the broadcaster’s account" }</span>
        </>
    };

    let body = match &panel.model.status.login {
        LoginStatus::Off => state_line("status-login-state", "off", "Off while a recording is replayed"),
        // The button under the state, as when logged in with a tick to
        // ask for (the dock is too narrow for both on a line).
        LoginStatus::LoggedOut => html! {
            <>
                { state_line("status-login-state", "off", "Not logged in") }
                { log_in.clone() }
            </>
        },
        LoginStatus::Checking => state_line("status-login-state", "warn", "Checking the saved login…"),
        LoginStatus::Waiting { url, code, expires_at_ms, reason } => {
            waiting(url, code, *expires_at_ms, reason.as_deref())
        }
        LoginStatus::LoggedIn { login, listening, granted, .. } => html! {
            <>
                <div class="state-spread">
                    { state_line("status-login-state", if *listening { "ok" } else { "warn" },
                                 if *listening { login.clone() } else { format!("{login}, connecting…") }) }
                    <button type="button" class="button ghost small" id="login-logout"
                            onclick={send(PanelRequest::Logout)}>{ "Log out" }</button>
                </div>
                // A second login, for a tick this one wasn't given.
                if let Some(renewal) = &panel.model.status.renewal {
                    <div class="login-renewal" id="login-renewal">
                        { waiting(&renewal.url, &renewal.code, renewal.expires_at_ms, None) }
                    </div>
                } else if ticked.beyond(*granted) {
                    { log_in.clone() }
                }
                // Closed with its ×, it stays closed for this login and
                // channel (in every panel: it's in the config).
                if !channel.is_empty() && !channel.eq_ignore_ascii_case(login)
                    && closed_pair != ClosedNotes::pair(login, &channel)
                {
                    <div class="notice warn small" role="status" id="login-other-channel">
                        { icon("warn") }
                        <div>{ format!("Chat is set to {channel}, but channel points come from {login}’s channel.") }</div>
                        <button type="button" class="notice-close" id="login-other-channel-close" title="Close" aria-label="Close"
                                onclick={close_other(ClosedNotes::pair(login, &channel))}>{ "×" }</button>
                    </div>
                }
            </>
        },
        LoginStatus::TwitchCli { address } => {
            state_line("status-login-state", "ok", format!("The Twitch CLI at {address}, no login needed"))
        }
    };
    html! { <div class="login-control" id="status-login">{ body }</div> }
}

#[derive(Properties, PartialEq)]
struct CopyLinkProps {
    url: AttrValue,
}

/// Copies the login link, for a browser elsewhere ("Copied" for a moment).
#[function_component]
fn CopyLink(props: &CopyLinkProps) -> Html {
    let copied = use_state(|| None::<gloo::timers::callback::Timeout>);
    let onclick = {
        let (url, copied) = (props.url.clone(), copied.clone());
        Callback::from(move |_: MouseEvent| {
            let _ = gloo::utils::window().navigator().clipboard().write_text(&url);
            let done = copied.clone();
            copied.set(Some(gloo::timers::callback::Timeout::new(2_000, move || done.set(None))));
        })
    };
    html! {
        <button type="button" class="button ghost small" id="login-copy-link" title={props.url.clone()} {onclick}>
            { if copied.is_some() { "Copied" } else { "Copy link" } }
        </button>
    }
}
