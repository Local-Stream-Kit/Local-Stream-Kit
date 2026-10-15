//! The overlay: runs inside an OBS Browser Source, receives commands from the
//! server over a WebSocket and hosts the effects: every plugin's page (the
//! alerts, the rain, sounds, speech, the chat display… are built-in
//! plugins) in an iframe.

mod plugins;
mod shell;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    shell::start(link()?)
}

/// What the page's link asks for. `?overlay=chat` names an overlay from
/// the panel's list, and the app says which pages it hosts; the plain
/// link is the default overlay. `?modules=rain,alert,confetti` (without
/// `?overlay=`) names the plugins whose pages it hosts, by id (`alert`,
/// `chat`, `rain` and `sound` are built-in ones that took over from the
/// overlay modules of those names).
fn link() -> Result<protocol::config::OverlayLink, JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let params = web_sys::UrlSearchParams::new_with_str(&window.location().search()?)?;
    let overlay = params
        .get(protocol::config::OVERLAY_PARAM)
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    let modules = params.get("modules").map(|list| {
        let mut plugins: Vec<String> = Vec::new();
        for name in list.split(',').map(str::trim).filter(|name| !name.is_empty()) {
            if !plugins.iter().any(|plugin| plugin == name) {
                plugins.push(name.to_string());
            }
        }
        plugins
    });
    Ok(protocol::config::OverlayLink { overlay, modules })
}

mod log {
    pub fn warn(message: &str) {
        web_sys::console::warn_1(&message.into());
    }

    pub fn info(message: &str) {
        web_sys::console::info_1(&message.into());
    }
}
