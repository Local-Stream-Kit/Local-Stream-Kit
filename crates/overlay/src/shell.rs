//! The shell: one WebSocket to the server (reconnecting with backoff), and
//! passing each plugin's commands on to its page.

use std::cell::RefCell;
use std::rc::Rc;

use protocol::config::OverlayLink;
use protocol::{ClientMessage, Command, Envelope, ShellCommand};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

use crate::log;
use crate::plugins::PluginFrames;

const MAX_BACKOFF_MS: i32 = 10_000;

pub struct Shell {
    /// What its link asks for (`?overlay=`, `?modules=`).
    link: OverlayLink,
    frames: PluginFrames,
    session: Option<u64>,
    /// Ids increase within a session, so the highest handled id is enough to
    /// skip anything delivered twice.
    last_id: u64,
    /// The overlay build this page runs: the first one the server announces.
    /// (A server restarted with a new build between this page loading and
    /// connecting, well under a second, would go unnoticed.)
    build: Option<String>,
}

impl Shell {
    fn handle(&mut self, envelope: Envelope) {
        if self.session != Some(envelope.session) {
            // The server restarted: ids start over.
            self.session = Some(envelope.session);
            self.last_id = 0;
        }
        if envelope.id <= self.last_id {
            return;
        }
        self.last_id = envelope.id;
        let issued_at_ms = envelope.issued_at_ms;
        match envelope.command {
            Command::Plugin(command) => self.frames.send(&command.plugin, &command.message, issued_at_ms),
            Command::Shell(ShellCommand::Plugins { plugins }) => self.frames.show(&plugins),
            Command::Shell(ShellCommand::Build { id }) => match &self.build {
                None => self.build = Some(id),
                Some(build) if *build != id => {
                    log::info(&format!("the overlay was rebuilt ({build} → {id}); reloading"));
                    if let Some(window) = web_sys::window() {
                        let _ = window.location().reload();
                    }
                }
                Some(_) => {}
            },
            Command::Shell(ShellCommand::Volume { volume, muted }) => self.frames.set_volume(volume, muted),
        }
    }
}

/// `link`: what the page's link asks for; the app answers with the pages
/// to host.
pub fn start(link: OverlayLink) -> Result<(), JsValue> {
    log::info(&match (&link.overlay, &link.modules) {
        (Some(overlay), _) => format!("overlay {overlay}"),
        (None, Some(modules)) => format!("overlay pages: {}", modules.join(", ")),
        (None, None) => "the default overlay".to_string(),
    });
    let shell = Shell { link, frames: PluginFrames::new(), session: None, last_id: 0, build: None };
    connect(Rc::new(RefCell::new(shell)), 0);
    Ok(())
}

fn ws_url() -> Result<String, JsValue> {
    let location = web_sys::window().ok_or("no window")?.location();
    let scheme = if location.protocol()? == "https:" { "wss:" } else { "ws:" };
    // Same host and port that served the page.
    Ok(format!("{scheme}//{}{}", location.host()?, protocol::WS_PATH))
}

/// The live socket and its callbacks. Replaced on every reconnect, which
/// drops the old callbacks (never from inside one of them: reconnects are
/// scheduled with a timer).
struct Connection {
    socket: WebSocket,
    _on_open: Closure<dyn FnMut()>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

thread_local! {
    static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

fn connect(shell: Rc<RefCell<Shell>>, attempt: u32) {
    let socket = match ws_url().and_then(|url| WebSocket::new(&url)) {
        Ok(socket) => socket,
        Err(error) => {
            log::warn(&format!("couldn't open the WebSocket: {error:?}"));
            schedule_reconnect(shell, attempt + 1);
            return;
        }
    };
    let opened = Rc::new(std::cell::Cell::new(false));

    let on_open = {
        let (socket, shell, opened) = (socket.clone(), Rc::clone(&shell), Rc::clone(&opened));
        Closure::<dyn FnMut()>::new(move || {
            opened.set(true);
            let hello = {
                let shell = shell.borrow();
                ClientMessage::Hello {
                    overlay: shell.link.overlay.clone(),
                    plugins: shell.link.modules.clone(),
                }
            };
            let json = serde_json::to_string(&hello).expect("hello serialises");
            if let Err(error) = socket.send_with_str(&json) {
                log::warn(&format!("couldn't send hello: {error:?}"));
            }
            log::info("connected to the server");
        })
    };
    let on_message = {
        let shell = Rc::clone(&shell);
        Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Some(text) = event.data().as_string() else { return };
            match serde_json::from_str::<Envelope>(&text) {
                Ok(envelope) => shell.borrow_mut().handle(envelope),
                Err(error) => log::warn(&format!("unexpected message from the server: {error}")),
            }
        })
    };
    let on_close = {
        let shell = Rc::clone(&shell);
        Closure::<dyn FnMut(CloseEvent)>::new(move |_: CloseEvent| {
            // Back off only while connecting keeps failing.
            let next = if opened.get() { 0 } else { attempt + 1 };
            schedule_reconnect(Rc::clone(&shell), next);
        })
    };
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    CONNECTION.with(|connection| {
        *connection.borrow_mut() =
            Some(Connection { socket, _on_open: on_open, _on_message: on_message, _on_close: on_close });
    });
}

/// Sends a message to the server, if it's connected. One sent while it
/// isn't is lost: the server doesn't depend on it (the queue moves on by
/// itself after an item's length).
pub fn send(message: &ClientMessage) {
    CONNECTION.with(|connection| {
        let connection = connection.borrow();
        let Some(connection) = connection.as_ref() else { return };
        if connection.socket.ready_state() != WebSocket::OPEN {
            return;
        }
        let json = serde_json::to_string(message).expect("client messages serialise");
        if let Err(error) = connection.socket.send_with_str(&json) {
            log::warn(&format!("couldn't send to the server: {error:?}"));
        }
    });
}

fn schedule_reconnect(shell: Rc<RefCell<Shell>>, attempt: u32) {
    let delay = (500_i32.saturating_mul(1 << attempt.min(5))).min(MAX_BACKOFF_MS);
    let callback = Closure::once_into_js(move || connect(shell, attempt));
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay);
    }
}
