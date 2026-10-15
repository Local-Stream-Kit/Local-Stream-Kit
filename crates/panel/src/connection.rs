//! The WebSocket to the server: reconnects with backoff, hands every
//! [`PanelEvent`] to a callback, and sends [`PanelRequest`]s.

use std::cell::RefCell;
use std::rc::Rc;

use protocol::panel::{PANEL_WS_PATH, PanelEvent, PanelRequest};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CloseEvent, MessageEvent, WebSocket};
use yew::Callback;

const MAX_BACKOFF_MS: i32 = 10_000;

#[derive(Clone)]
pub struct Connection {
    inner: Rc<Inner>,
}

struct Inner {
    socket: RefCell<Option<WebSocket>>,
    on_event: Callback<PanelEvent>,
    on_connected: Callback<bool>,
    backoff_ms: RefCell<i32>,
    // Kept alive as long as the connection.
    closures: RefCell<Vec<Handler>>,
}

/// A socket event handler, as the browser calls it.
type Handler = Closure<dyn FnMut(JsValue)>;

impl PartialEq for Connection {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Connection {
    pub fn open(on_event: Callback<PanelEvent>, on_connected: Callback<bool>) -> Connection {
        let connection = Connection {
            inner: Rc::new(Inner {
                socket: RefCell::new(None),
                on_event,
                on_connected,
                backoff_ms: RefCell::new(500),
                closures: RefCell::new(Vec::new()),
            }),
        };
        connection.connect();
        connection
    }

    /// Sends a request; `false` if it couldn't go (not connected).
    pub fn send(&self, request: &PanelRequest) -> bool {
        let json = serde_json::to_string(request).expect("panel requests serialise");
        match self.inner.socket.borrow().as_ref() {
            Some(socket) if socket.ready_state() == WebSocket::OPEN => socket.send_with_str(&json).is_ok(),
            _ => false,
        }
    }

    fn connect(&self) {
        let Some(window) = web_sys::window() else { return };
        let location = window.location();
        let scheme = if location.protocol().ok().as_deref() == Some("https:") { "wss" } else { "ws" };
        let host = location.host().unwrap_or_default();
        let Ok(socket) = WebSocket::new(&format!("{scheme}://{host}{PANEL_WS_PATH}")) else {
            self.retry();
            return;
        };
        let mut closures = self.inner.closures.borrow_mut();
        closures.clear();

        let this = self.clone();
        let on_open = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            *this.inner.backoff_ms.borrow_mut() = 500;
            this.inner.on_connected.emit(true);
        });
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        closures.push(on_open);

        let this = self.clone();
        let on_message = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let Some(text) = event.dyn_into::<MessageEvent>().ok().and_then(|event| event.data().as_string())
            else {
                return;
            };
            match serde_json::from_str::<PanelEvent>(&text) {
                Ok(event) => this.inner.on_event.emit(event),
                Err(error) => web_sys::console::warn_1(&format!("unexpected message: {error}").into()),
            }
        });
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        closures.push(on_message);

        let this = self.clone();
        let on_close = Closure::<dyn FnMut(JsValue)>::new(move |_event: JsValue| {
            let _ = _event.dyn_into::<CloseEvent>();
            this.inner.on_connected.emit(false);
            this.retry();
        });
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        closures.push(on_close);

        drop(closures);
        *self.inner.socket.borrow_mut() = Some(socket);
    }

    fn retry(&self) {
        let delay = {
            let mut backoff = self.inner.backoff_ms.borrow_mut();
            let delay = *backoff;
            *backoff = (*backoff * 2).min(MAX_BACKOFF_MS);
            delay
        };
        let this = self.clone();
        gloo::timers::callback::Timeout::new(delay as u32, move || this.connect()).forget();
    }
}
