//! The Twitch login, in the Status page's Twitch box (its own page until
//! 2026-10-14): Twitch's device code flow, with the link and code, and
//! logging out. Logging in is what turns channel points, follows and hype
//! trains on (there was a switch for them until 2026-10-14).

use gloo::timers::callback::Interval;
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

    let body = match &panel.model.status.login {
        LoginStatus::Off => state_line("status-login-state", "off", "Off while a recording is replayed"),
        LoginStatus::LoggedOut => html! {
            <>
                <div class="state-spread">
                    { state_line("status-login-state", "off", "Not logged in") }
                    <button type="button" class="button primary small" id="login-start"
                            onclick={send(PanelRequest::StartLogin)}>{ "Log in with Twitch" }</button>
                </div>
                <span class="muted small">{ "With the broadcaster’s account" }</span>
            </>
        },
        LoginStatus::Checking => state_line("status-login-state", "warn", "Checking the saved login…"),
        LoginStatus::Waiting { url, code, expires_at_ms, reason } => {
            let minutes = ((*expires_at_ms as f64 - *now) / 60_000.0).ceil().max(0.0);
            html! {
                <>
                    { state_line("status-login-state", "accent", "Waiting for approval") }
                    if let Some(reason) = reason {
                        <span class="small warn-text">{ reason.clone() }</span>
                    }
                    <span class="muted small">{ "Log in as the broadcaster, and check Twitch shows this code:" }</span>
                    <CopyField id="login-code" value={code.clone()} what="the code" big=true />
                    <div class="button-row">
                        <button type="button" class="button primary small" id="open-login"
                                onclick={send(PanelRequest::OpenLogin)}>{ "Open in your browser" }{ icon("external") }</button>
                        <CopyLink url={url.clone()} />
                        <button type="button" class="button ghost small" id="login-cancel"
                                onclick={send(PanelRequest::CancelLogin)}>{ "Cancel" }</button>
                    </div>
                    <span class="muted small">{ format!("A new code in {minutes} minutes") }</span>
                </>
            }
        }
        LoginStatus::LoggedIn { login, listening, .. } => html! {
            <>
                <div class="state-spread">
                    { state_line("status-login-state", if *listening { "ok" } else { "warn" },
                                 if *listening { login.clone() } else { format!("{login}, connecting…") }) }
                    <button type="button" class="button ghost small" id="login-logout"
                            onclick={send(PanelRequest::Logout)}>{ "Log out" }</button>
                </div>
                if !channel.is_empty() && !channel.eq_ignore_ascii_case(login) {
                    <div class="notice warn small" role="status">
                        { icon("warn") }
                        <div>{ format!("Chat is set to {channel}, but channel points come from {login}’s channel.") }</div>
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
